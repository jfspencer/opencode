//! The matcher: the IR compiled to instructions for a backtracking machine with an explicit backtrack stack (an
//! undo log for captures and loop registers), so neither long inputs nor deep loops grow the native stack.

use super::charset::CharSet;
use super::parser::{Node, deep};
use super::unicode;

const UNSET: usize = usize::MAX;
const INFINITE: u32 = u32::MAX;

struct Set {
    ascii: [u64; 2],
    set: CharSet,
}

impl Set {
    fn new(set: CharSet) -> Set {
        let mut ascii = [0u64; 2];
        for &(a, b) in set.ranges() {
            if a >= 128 {
                break;
            }
            for c in a..=b.min(127) {
                ascii[(c / 64) as usize] |= 1 << (c % 64);
            }
        }
        Set { ascii, set }
    }

    #[inline]
    fn contains(&self, c: u32) -> bool {
        if c < 128 {
            return self.ascii[(c / 64) as usize] & (1 << (c % 64)) != 0;
        }
        self.set.contains(c)
    }
}

#[derive(Clone, Copy)]
enum One {
    Char(u32),
    Set(u32),
}

enum Inst {
    One { one: One, back: bool },
    /// A loop over one character: `min..=max` of them, greedy or lazy.
    Single { one: One, min: u32, max: u32, greedy: bool, back: bool },
    /// Try `a`, then `b`; `b` only when the next character is in the `guard` set, if there is one.
    Split { a: u32, b: u32, guard: Option<u32> },
    Jmp(u32),
    /// `undo`: the group's body has choice points that may be resumed after a later pass changed the register.
    GroupStart { reg: u32, undo: bool },
    GroupEnd { index: u32, reg: u32, back: bool },
    ClearCaptures { from: u32, to: u32 },
    RepInit { reg: u32 },
    RepLoop { reg: u32, min: u32, max: u32, greedy: bool, body: u32, exit: u32 },
    RepIter { reg: u32 },
    RepEnd { reg: u32, min: u32, head: u32 },
    LineStart { multiline: bool },
    LineEnd { multiline: bool },
    WordBoundary { negate: bool, unicode_icase: bool },
    BackRef { groups: u32, icase: bool, back: bool },
    Look { negate: bool, body: u32, next: u32 },
    Succeed,
}

pub struct Program {
    /// The characters a match can start with, when that is known (a search skips other positions).
    first: Option<Set>,
    insts: Vec<Inst>,
    sets: Vec<Set>,
    backref_groups: Vec<Vec<u32>>,
    regs: usize,
    capture_count: u32,
    unicode: bool,
}

struct Compiler {
    insts: Vec<Inst>,
    sets: Vec<Set>,
    backref_groups: Vec<Vec<u32>>,
    regs: u32,
    group_regs: Vec<u32>,
}

impl Compiler {
    fn pc(&self) -> u32 {
        self.insts.len() as u32
    }

    /// The node as a one-character matcher, when it matches exactly one character and captures nothing (an
    /// alternation of single characters is a class: whichever alternative matches, the outcome is the same).
    fn one(&mut self, node: &Node) -> Option<One> {
        let set = single_chars(node)?;
        if let [(a, b)] = set.ranges()
            && a == b
        {
            return Some(One::Char(*a));
        }
        self.sets.push(Set::new(set));
        Some(One::Set(self.sets.len() as u32 - 1))
    }

    fn emit(&mut self, node: &Node, back: bool) {
        deep(|| self.emit_node(node, back))
    }

    /// One iteration of a loop: its captures are cleared first (RepeatMatcher step 4).
    fn emit_iteration(&mut self, body: &Node, captures: (u32, u32), back: bool) {
        self.emit_clear(body, captures);
        self.emit(body, back);
    }

    /// Clears a loop body's groups, except those every iteration sets and the body never refers back to: their
    /// clearing could not be observed (the iteration overwrites them, and undoing it restores them).
    fn emit_clear(&mut self, body: &Node, captures: (u32, u32)) {
        let mut referenced = Vec::new();
        backref_groups(body, &mut referenced);
        let mut start = None;
        for g in captures.0..=captures.1 {
            let clear = g < captures.1 && (referenced.contains(&g) || !always_sets(body, g));
            match (clear, start) {
                (true, None) => start = Some(g),
                (false, Some(from)) => {
                    self.insts.push(Inst::ClearCaptures { from, to: g });
                    start = None;
                }
                _ => {}
            }
        }
    }

    /// The characters any of `nodes` can start with, as a set index, when that is known for all of them.
    fn first_set(&mut self, nodes: &[Node]) -> Option<u32> {
        let mut set = CharSet::new();
        for n in nodes {
            set.add_set(&first_chars(n)?);
        }
        self.sets.push(Set::new(set));
        Some(self.sets.len() as u32 - 1)
    }

    fn emit_node(&mut self, node: &Node, back: bool) {
        match node {
            Node::Empty => {}
            Node::Char(_) | Node::Class(_) => {
                let one = self.one(node).expect("one character");
                self.insts.push(Inst::One { one, back });
            }
            Node::Seq(nodes) => {
                if back {
                    for n in nodes.iter().rev() {
                        self.emit(n, back);
                    }
                } else {
                    for n in nodes {
                        self.emit(n, back);
                    }
                }
            }
            Node::Alt(_) if single_chars(node).is_some() => {
                let one = self.one(node).expect("one character");
                self.insts.push(Inst::One { one, back });
            }
            Node::Alt(alternatives) => {
                // Guards (the characters the remaining alternatives can start with) for hand-sized alternations;
                // huge ones (properties of strings) are not worth a set per alternative.
                let mut guards: Vec<Option<u32>> = vec![None; alternatives.len() + 1];
                if !back && alternatives.len() <= 32 {
                    let mut suffix = Some(CharSet::new());
                    for k in (1..alternatives.len()).rev() {
                        suffix = match (suffix, first_chars(&alternatives[k])) {
                            (Some(mut set), Some(first)) => {
                                set.add_set(&first);
                                Some(set)
                            }
                            _ => None,
                        };
                        if let Some(set) = &suffix {
                            self.sets.push(Set::new(set.clone()));
                            guards[k] = Some(self.sets.len() as u32 - 1);
                        }
                    }
                }
                let mut jumps = Vec::new();
                for (k, alternative) in alternatives.iter().enumerate() {
                    if k + 1 < alternatives.len() {
                        let split = self.pc();
                        let guard = guards[k + 1];
                        self.insts.push(Inst::Split { a: split + 1, b: 0, guard });
                        self.emit(alternative, back);
                        jumps.push(self.pc());
                        self.insts.push(Inst::Jmp(0));
                        let next = self.pc();
                        if let Inst::Split { b, .. } = &mut self.insts[split as usize] {
                            *b = next;
                        }
                    } else {
                        self.emit(alternative, back);
                    }
                }
                let end = self.pc();
                for j in jumps {
                    self.insts[j as usize] = Inst::Jmp(end);
                }
            }
            Node::Group(body) => self.emit(body, back),
            Node::Capture { index, body } => {
                let reg = self.group_regs[*index as usize];
                self.insts.push(Inst::GroupStart { reg, undo: !deterministic(body) });
                self.emit(body, back);
                self.insts.push(Inst::GroupEnd { index: *index, reg, back });
            }
            Node::Look { behind, negate, body } => {
                let look = self.pc();
                self.insts.push(Inst::Look { negate: *negate, body: look + 1, next: 0 });
                self.emit(body, *behind);
                self.insts.push(Inst::Succeed);
                let next = self.pc();
                if let Inst::Look { next: n, .. } = &mut self.insts[look as usize] {
                    *n = next;
                }
            }
            Node::Repeat { body, min, max, greedy, captures } => {
                if captures.0 >= captures.1
                    && let Some(one) = self.one(body)
                {
                    self.insts.push(Inst::Single { one, min: *min, max: *max, greedy: *greedy, back });
                    return;
                }
                if *min == 1 && *max == 1 {
                    self.emit_iteration(body, *captures, back);
                    return;
                }
                if *min <= 1 && (*max == INFINITE || *min == 0 && *max == 1) && min_length(body) > 0 {
                    // No iteration can be empty, so neither the empty check nor (for these bounds) a counter is
                    // needed: a choice point per iteration is all the loop keeps.
                    if *min == 1 {
                        self.emit_iteration(body, *captures, back);
                    }
                    let head = self.pc();
                    let guard = if back || *greedy { None } else { self.first_set(std::slice::from_ref(&**body)) };
                    self.insts.push(Inst::Split { a: 0, b: 0, guard });
                    let body_pc = self.pc();
                    self.emit_iteration(body, *captures, back);
                    if *max == INFINITE {
                        self.insts.push(Inst::Jmp(head));
                    }
                    let exit = self.pc();
                    let (a, b) = if *greedy { (body_pc, exit) } else { (exit, body_pc) };
                    self.insts[head as usize] = Inst::Split { a, b, guard };
                    return;
                }
                let reg = self.regs;
                self.regs += 2;
                self.insts.push(Inst::RepInit { reg });
                let head = self.pc();
                self.insts.push(Inst::RepLoop { reg, min: *min, max: *max, greedy: *greedy, body: head + 1, exit: 0 });
                self.insts.push(Inst::RepIter { reg });
                self.emit_clear(body, *captures);
                self.emit(body, back);
                self.insts.push(Inst::RepEnd { reg, min: *min, head });
                let exit = self.pc();
                if let Inst::RepLoop { exit: e, .. } = &mut self.insts[head as usize] {
                    *e = exit;
                }
            }
            Node::BackRef { groups, icase } => {
                self.backref_groups.push(groups.clone());
                let groups = self.backref_groups.len() as u32 - 1;
                self.insts.push(Inst::BackRef { groups, icase: *icase, back });
            }
            Node::NamedBackRef { .. } => unreachable!("resolved by the parser"),
            Node::LineStart { multiline } => self.insts.push(Inst::LineStart { multiline: *multiline }),
            Node::LineEnd { multiline } => self.insts.push(Inst::LineEnd { multiline: *multiline }),
            Node::WordBoundary { negate, unicode_icase } => {
                self.insts.push(Inst::WordBoundary { negate: *negate, unicode_icase: *unicode_icase })
            }
        }
    }
}

pub fn compile(node: &Node, capture_count: u32, unicode: bool) -> Program {
    let mut compiler = Compiler {
        insts: Vec::new(),
        sets: Vec::new(),
        backref_groups: Vec::new(),
        regs: capture_count + 1,
        group_regs: (0..=capture_count).collect(),
    };
    compiler.emit(node, false);
    compiler.insts.push(Inst::Succeed);
    Program {
        first: first_chars(node).map(Set::new),
        insts: compiler.insts,
        sets: compiler.sets,
        backref_groups: compiler.backref_groups,
        regs: compiler.regs as usize,
        capture_count,
        unicode,
    }
}

/// Backtrack records hold positions as u32 (JS strings are shorter than 2^32), so each is 16 bytes.
enum Bt {
    Branch { pc: u32, pos: u32 },
    Reg { reg: u32, old: u32 },
    /// A group's capture before it changed.
    Group { group: u32, start: u32, end: u32 },
    /// A greedy single-character loop that can give back characters down to `limit`.
    Greedy { inst: u32, pos: u32, limit: u32 },
    /// A lazy single-character loop that can take one more character.
    Lazy { inst: u32, pos: u32, count: u32 },
}

fn pack(value: usize) -> u32 {
    if value == UNSET { u32::MAX } else { value as u32 }
}

fn unpack(value: u32) -> usize {
    if value == u32::MAX { UNSET } else { value as usize }
}

/// The fewest code units a match of `node` takes (0 when it may be empty).
fn min_length(node: &Node) -> u64 {
    match node {
        Node::Char(_) | Node::Class(_) => 1,
        Node::Seq(nodes) => nodes.iter().map(|n| deep(|| min_length(n))).sum(),
        Node::Alt(alternatives) => alternatives.iter().map(|n| deep(|| min_length(n))).min().unwrap_or(0),
        Node::Capture { body, .. } | Node::Group(body) => deep(|| min_length(body)),
        Node::Repeat { body, min, .. } => *min as u64 * deep(|| min_length(body)),
        _ => 0,
    }
}

/// Whether every match of `node` sets group `g`.
fn always_sets(node: &Node, g: u32) -> bool {
    match node {
        Node::Capture { index, body } => *index == g || deep(|| always_sets(body, g)),
        Node::Seq(nodes) => nodes.iter().any(|n| deep(|| always_sets(n, g))),
        Node::Alt(alternatives) => alternatives.iter().all(|n| deep(|| always_sets(n, g))),
        Node::Group(body) => deep(|| always_sets(body, g)),
        Node::Repeat { body, min, .. } => *min >= 1 && deep(|| always_sets(body, g)),
        Node::Look { negate: false, body, .. } => deep(|| always_sets(body, g)),
        _ => false,
    }
}

/// Every group a backreference in `node` refers to.
fn backref_groups(node: &Node, out: &mut Vec<u32>) {
    match node {
        Node::BackRef { groups, .. } => out.extend_from_slice(groups),
        Node::Seq(nodes) | Node::Alt(nodes) => nodes.iter().for_each(|n| deep(|| backref_groups(n, out))),
        Node::Capture { body, .. } | Node::Group(body) | Node::Look { body, .. } | Node::Repeat { body, .. } => {
            deep(|| backref_groups(body, out))
        }
        _ => {}
    }
}

/// Whether matching `node` leaves no choice point behind (a lookaround's are discarded when it completes).
fn deterministic(node: &Node) -> bool {
    match node {
        Node::Seq(nodes) => nodes.iter().all(|n| deep(|| deterministic(n))),
        Node::Alt(_) => single_chars(node).is_some(),
        Node::Capture { body, .. } | Node::Group(body) => deep(|| deterministic(body)),
        Node::Repeat { body, min, max, captures, .. } => {
            min == max && captures.0 >= captures.1 && single_chars(body).is_some()
        }
        _ => true,
    }
}

/// The characters of a node that matches exactly one of them.
fn single_chars(node: &Node) -> Option<CharSet> {
    match node {
        Node::Char(c) => Some(CharSet::single(*c)),
        Node::Class(set) => Some(set.clone()),
        Node::Group(body) => deep(|| single_chars(body)),
        Node::Alt(alternatives) => {
            let mut set = CharSet::new();
            for a in alternatives {
                set.add_set(&deep(|| single_chars(a))?);
            }
            Some(set)
        }
        _ => None,
    }
}

/// The characters a non-empty match of `node` must start with; None when it may match empty (or it is unclear).
fn first_chars(node: &Node) -> Option<CharSet> {
    match node {
        Node::Char(c) => Some(CharSet::single(*c)),
        Node::Class(set) => Some(set.clone()),
        Node::Seq(nodes) => {
            for n in nodes {
                match n {
                    Node::Empty
                    | Node::LineStart { .. }
                    | Node::LineEnd { .. }
                    | Node::WordBoundary { .. }
                    | Node::Look { .. } => continue,
                    _ => return deep(|| first_chars(n)),
                }
            }
            None
        }
        Node::Alt(alternatives) => {
            let mut set = CharSet::new();
            for a in alternatives {
                set.add_set(&deep(|| first_chars(a))?);
            }
            Some(set)
        }
        Node::Capture { body, .. } | Node::Group(body) => deep(|| first_chars(body)),
        Node::Repeat { body, min, .. } if *min >= 1 => deep(|| first_chars(body)),
        _ => None,
    }
}

fn is_lead(u: u16) -> bool {
    (0xD800..0xDC00).contains(&u)
}

fn is_trail(u: u16) -> bool {
    (0xDC00..0xE000).contains(&u)
}

struct Machine<'p, 't> {
    program: &'p Program,
    text: &'t [u16],
    caps: Vec<usize>,
    regs: Vec<usize>,
    stack: Vec<Bt>,
    /// Alternation and loop choice points this search may still resume (see BACKTRACK_BUDGET).
    budget: u64,
}

/// How many alternation and loop choice points one search (an exec) may resume before it gives up and reports no
/// match, as JSC's Yarr interpreter does when it exhausts its match limit, instead of backtracking exponentially
/// for ever. Single-character loops giving back characters are not counted, so plain scans of long texts never
/// run out.
const BACKTRACK_BUDGET: u64 = 50_000_000;

impl Program {
    pub fn capture_count(&self) -> u32 {
        self.capture_count
    }

    /// The first match starting at or after `from` (only at `from` when sticky), advancing by code points in
    /// unicode mode: the capture slots (2 per group, group 0 included).
    pub fn search(&self, text: &[u16], from: usize, sticky: bool) -> Option<Vec<usize>> {
        let mut machine = Machine {
            program: self,
            text,
            caps: vec![UNSET; 2 * (self.capture_count as usize + 1)],
            regs: vec![UNSET; self.regs],
            stack: Vec::new(),
            budget: BACKTRACK_BUDGET,
        };
        let mut pos = from;
        loop {
            let candidate = sticky
                || match &self.first {
                    None => true,
                    Some(first) => machine.next_char(pos, false).is_some_and(|(c, _)| first.contains(c)),
                };
            if candidate {
                machine.caps.fill(UNSET);
                machine.regs.fill(UNSET);
                machine.stack.clear();
                if let Some(end) = machine.run(0, pos, 0) {
                    machine.caps[0] = pos;
                    machine.caps[1] = end;
                    return Some(machine.caps);
                }
                if machine.budget == 0 {
                    return None;
                }
            }
            if sticky || pos >= text.len() {
                return None;
            }
            pos = match machine.next_char(pos, false) {
                Some((_, next)) => next,
                None => pos + 1,
            };
        }
    }
}

impl<'p, 't> Machine<'p, 't> {
    /// The character at `pos` going forward (or ending at `pos` going back), and the position past it.
    #[inline]
    fn next_char(&self, pos: usize, back: bool) -> Option<(u32, usize)> {
        let text = self.text;
        if back {
            if pos == 0 {
                return None;
            }
            let u = text[pos - 1];
            if self.program.unicode && is_trail(u) && pos >= 2 && is_lead(text[pos - 2]) {
                let c = 0x10000 + (((text[pos - 2] as u32) - 0xD800) << 10) + (u as u32 - 0xDC00);
                return Some((c, pos - 2));
            }
            Some((u as u32, pos - 1))
        } else {
            if pos >= text.len() {
                return None;
            }
            let u = text[pos];
            if self.program.unicode && is_lead(u) && pos + 1 < text.len() && is_trail(text[pos + 1]) {
                let c = 0x10000 + (((u as u32) - 0xD800) << 10) + (text[pos + 1] as u32 - 0xDC00);
                return Some((c, pos + 2));
            }
            Some((u as u32, pos + 1))
        }
    }

    #[inline]
    fn matches_one(&self, one: One, c: u32) -> bool {
        match one {
            One::Char(x) => x == c,
            One::Set(s) => self.program.sets[s as usize].contains(c),
        }
    }

    #[inline]
    fn step_one(&self, one: One, pos: usize, back: bool) -> Option<usize> {
        let (c, next) = self.next_char(pos, back)?;
        if self.matches_one(one, c) { Some(next) } else { None }
    }

    /// Undoes one character of a single-character loop: from `pos` one character back toward `limit`.
    fn give_back(&self, pos: usize, limit: usize, back: bool) -> usize {
        let text = self.text;
        if back {
            // The loop went left; give back to the right.
            if self.program.unicode && pos + 2 <= limit && is_lead(text[pos]) && is_trail(text[pos + 1]) {
                return pos + 2;
            }
            pos + 1
        } else {
            if self.program.unicode && pos >= limit + 2 && is_trail(text[pos - 1]) && is_lead(text[pos - 2]) {
                return pos - 2;
            }
            pos - 1
        }
    }

    fn set_group(&mut self, group: usize, start: usize, end: usize) {
        let (old_start, old_end) = (self.caps[2 * group], self.caps[2 * group + 1]);
        if (old_start, old_end) != (start, end) {
            self.stack.push(Bt::Group { group: group as u32, start: pack(old_start), end: pack(old_end) });
            self.caps[2 * group] = start;
            self.caps[2 * group + 1] = end;
        }
    }

    fn set_reg(&mut self, reg: usize, value: usize) {
        let old = self.regs[reg];
        if old != value {
            self.stack.push(Bt::Reg { reg: reg as u32, old: pack(old) });
            self.regs[reg] = value;
        }
    }

    fn is_word(c: u16, unicode_icase: bool) -> bool {
        let c = c as u32;
        matches!(c, 0x30..=0x39 | 0x41..=0x5A | 0x5F | 0x61..=0x7A) || (unicode_icase && (c == 0x17F || c == 0x212A))
    }

    fn backref(&self, groups: u32, icase: bool, back: bool, pos: usize) -> Option<usize> {
        let program = self.program;
        let mut range = None;
        for &g in &program.backref_groups[groups as usize] {
            let (s, e) = (self.caps[2 * g as usize], self.caps[2 * g as usize + 1]);
            if s != UNSET && e != UNSET {
                range = Some((s, e));
                break;
            }
        }
        let Some((s, e)) = range else {
            return Some(pos);
        };
        let len = e - s;
        let text = self.text;
        let (from, to) = if back {
            if pos < len {
                return None;
            }
            (pos - len, pos)
        } else {
            if pos + len > text.len() {
                return None;
            }
            (pos, pos + len)
        };
        let captured = &text[s..e];
        let candidate = &text[from..to];
        let equal = if !icase {
            captured == candidate
        } else if program.unicode {
            let folding = unicode::unicode_folding();
            let mut a = char_iter(captured);
            let mut b = char_iter(candidate);
            loop {
                match (a.next(), b.next()) {
                    (None, None) => break true,
                    (Some(x), Some(y)) if x == y || folding.canon(x) == folding.canon(y) => {}
                    _ => break false,
                }
            }
        } else {
            let folding = unicode::ucs2_folding();
            captured
                .iter()
                .zip(candidate)
                .all(|(&x, &y)| x == y || folding.canon(x as u32) == folding.canon(y as u32))
        };
        if !equal {
            return None;
        }
        Some(if back { from } else { to })
    }

    /// Runs from `pc` at `pos` until Succeed (the end position) or until backtracking would pop below `base`.
    fn run(&mut self, mut pc: u32, mut pos: usize, base: usize) -> Option<usize> {
        let program = self.program;
        loop {
            let ok = match &program.insts[pc as usize] {
                Inst::One { one, back } => match self.step_one(*one, pos, *back) {
                    Some(next) => {
                        pos = next;
                        pc += 1;
                        true
                    }
                    None => false,
                },
                Inst::Single { one, min, max, greedy, back } => {
                    let (one, min, max, back) = (*one, *min, *max, *back);
                    let mut count = 0u32;
                    let mut ok = true;
                    while count < min {
                        match self.step_one(one, pos, back) {
                            Some(next) => {
                                pos = next;
                                count += 1;
                            }
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        if *greedy {
                            let limit = pos;
                            while count < max {
                                match self.step_one(one, pos, back) {
                                    Some(next) => {
                                        pos = next;
                                        count += 1;
                                    }
                                    None => break,
                                }
                            }
                            if pos != limit {
                                self.stack.push(Bt::Greedy { inst: pc, pos: pos as u32, limit: limit as u32 });
                            }
                        } else if count < max {
                            self.stack.push(Bt::Lazy { inst: pc, pos: pos as u32, count });
                        }
                        pc += 1;
                    }
                    ok
                }
                Inst::Split { a, b, guard } => {
                    let viable = match guard {
                        None => true,
                        Some(set) => self.next_char(pos, false).is_some_and(|(c, _)| program.sets[*set as usize].contains(c)),
                    };
                    if viable {
                        self.stack.push(Bt::Branch { pc: *b, pos: pos as u32 });
                    }
                    pc = *a;
                    true
                }
                Inst::Jmp(target) => {
                    pc = *target;
                    true
                }
                Inst::GroupStart { reg, undo } => {
                    if *undo {
                        self.set_reg(*reg as usize, pos);
                    } else {
                        self.regs[*reg as usize] = pos;
                    }
                    pc += 1;
                    true
                }
                Inst::GroupEnd { index, reg, back } => {
                    let other = self.regs[*reg as usize];
                    let (s, e) = if *back { (pos, other) } else { (other, pos) };
                    self.set_group(*index as usize, s, e);
                    pc += 1;
                    true
                }
                Inst::ClearCaptures { from, to } => {
                    for g in *from..*to {
                        self.set_group(g as usize, UNSET, UNSET);
                    }
                    pc += 1;
                    true
                }
                Inst::RepInit { reg } => {
                    self.set_reg(*reg as usize, 0);
                    pc += 1;
                    true
                }
                Inst::RepLoop { reg, min, max, greedy, body, exit } => {
                    let count = self.regs[*reg as usize] as u32;
                    if count < *min {
                        pc = *body;
                    } else if *max != INFINITE && count >= *max {
                        pc = *exit;
                    } else if *greedy {
                        self.stack.push(Bt::Branch { pc: *exit, pos: pos as u32 });
                        pc = *body;
                    } else {
                        self.stack.push(Bt::Branch { pc: *body, pos: pos as u32 });
                        pc = *exit;
                    }
                    true
                }
                Inst::RepIter { reg } => {
                    self.set_reg(*reg as usize + 1, pos);
                    pc += 1;
                    true
                }
                Inst::RepEnd { reg, min, head } => {
                    let count = self.regs[*reg as usize];
                    if count as u64 >= *min as u64 && pos == self.regs[*reg as usize + 1] {
                        false
                    } else {
                        self.set_reg(*reg as usize, count + 1);
                        pc = *head;
                        true
                    }
                }
                Inst::LineStart { multiline } => {
                    if pos == 0 || (*multiline && unicode::is_line_terminator(self.text[pos - 1] as u32)) {
                        pc += 1;
                        true
                    } else {
                        false
                    }
                }
                Inst::LineEnd { multiline } => {
                    if pos == self.text.len() || (*multiline && unicode::is_line_terminator(self.text[pos] as u32)) {
                        pc += 1;
                        true
                    } else {
                        false
                    }
                }
                Inst::WordBoundary { negate, unicode_icase } => {
                    let a = pos > 0 && Self::is_word(self.text[pos - 1], *unicode_icase);
                    let b = pos < self.text.len() && Self::is_word(self.text[pos], *unicode_icase);
                    if (a != b) != *negate {
                        pc += 1;
                        true
                    } else {
                        false
                    }
                }
                Inst::BackRef { groups, icase, back } => match self.backref(*groups, *icase, *back, pos) {
                    Some(next) => {
                        pos = next;
                        pc += 1;
                        true
                    }
                    None => false,
                },
                Inst::Look { negate, body, next } => {
                    let height = self.stack.len();
                    let body = *body;
                    let matched = deep(|| self.run(body, pos, height)).is_some();
                    if self.budget == 0 {
                        return None;
                    }
                    if matched && !*negate {
                        // Atomic: drop the body's choice points, keep its capture undo records.
                        let mut keep = height;
                        for k in height..self.stack.len() {
                            if let Bt::Group { group, start, end } = self.stack[k] {
                                self.stack[keep] = Bt::Group { group, start, end };
                                keep += 1;
                            }
                        }
                        self.stack.truncate(keep);
                        pc = *next;
                        true
                    } else if matched {
                        self.unwind(height);
                        false
                    } else if *negate {
                        pc = *next;
                        true
                    } else {
                        false
                    }
                }
                Inst::Succeed => return Some(pos),
            };
            if ok {
                continue;
            }
            // Backtrack.
            loop {
                if self.stack.len() <= base {
                    return None;
                }
                match self.stack.pop().unwrap() {
                    Bt::Branch { pc: p, pos: q } => {
                        if self.budget == 0 {
                            return None;
                        }
                        self.budget -= 1;
                        pc = p;
                        pos = q as usize;
                        break;
                    }
                    Bt::Reg { reg, old } => self.regs[reg as usize] = unpack(old),
                    Bt::Group { group, start, end } => {
                        self.caps[2 * group as usize] = unpack(start);
                        self.caps[2 * group as usize + 1] = unpack(end);
                    }
                    Bt::Greedy { inst, pos: q, limit } => {
                        let Inst::Single { back, .. } = program.insts[inst as usize] else { unreachable!() };
                        let next = self.give_back(q as usize, limit as usize, back);
                        if next != limit as usize {
                            self.stack.push(Bt::Greedy { inst, pos: next as u32, limit });
                        }
                        pc = inst + 1;
                        pos = next;
                        break;
                    }
                    Bt::Lazy { inst, pos: q, count } => {
                        let Inst::Single { one, max, back, .. } = program.insts[inst as usize] else { unreachable!() };
                        if let Some(next) = self.step_one(one, q as usize, back) {
                            if count + 1 < max {
                                self.stack.push(Bt::Lazy { inst, pos: next as u32, count: count + 1 });
                            }
                            pc = inst + 1;
                            pos = next;
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Pops back to `height`, applying the undo records.
    fn unwind(&mut self, height: usize) {
        while self.stack.len() > height {
            match self.stack.pop().unwrap() {
                Bt::Reg { reg, old } => self.regs[reg as usize] = unpack(old),
                Bt::Group { group, start, end } => {
                    self.caps[2 * group as usize] = unpack(start);
                    self.caps[2 * group as usize + 1] = unpack(end);
                }
                _ => {}
            }
        }
    }
}

fn char_iter(units: &[u16]) -> impl Iterator<Item = u32> + '_ {
    char::decode_utf16(units.iter().copied()).map(|r| match r {
        Ok(c) => c as u32,
        Err(e) => e.unpaired_surrogate() as u32,
    })
}
