//! The pattern parser: JavaScriptCore's (Yarr's) grammar, Annex B included, producing the matcher's IR with every
//! character set resolved (case folding, negation, Unicode properties).

use super::charset::CharSet;
use super::unicode::{self, Folding, MAX_CODE_POINT, MAX_CODE_UNIT, Property};

pub const QUANTIFIER_OUT_OF_ORDER: &str = "numbers out of order in {} quantifier";
pub const NOTHING_TO_REPEAT: &str = "nothing to repeat";
pub const QUANTIFIER_INCOMPLETE: &str = "incomplete {} quantifier for Unicode pattern";
pub const CANT_QUANTIFY: &str = "invalid quantifier";
pub const MISSING_PARENTHESES: &str = "missing )";
pub const BRACKET_UNMATCHED: &str = "unmatched ] or } bracket for Unicode pattern";
pub const PARENTHESES_UNMATCHED: &str = "unmatched parentheses";
pub const PARENTHESES_TYPE_INVALID: &str = "unrecognized character after (?";
pub const INVALID_GROUP_NAME: &str = "invalid group specifier name";
pub const DUPLICATE_GROUP_NAME: &str = "duplicate group specifier name";
pub const CLASS_UNMATCHED: &str = "missing terminating ] for character class";
pub const CLASS_RANGE_OUT_OF_ORDER: &str = "range out of order in character class";
pub const CLASS_RANGE_INVALID: &str = "invalid range in character class for Unicode pattern";
pub const ESCAPE_UNTERMINATED: &str = "\\ at end of pattern";
pub const INVALID_UNICODE_ESCAPE: &str = "invalid Unicode \\u escape";
pub const INVALID_CODE_POINT_ESCAPE: &str = "invalid Unicode code point \\u{} escape";
pub const INVALID_BACKREFERENCE: &str = "invalid backreference for Unicode pattern";
pub const INVALID_NAMED_BACKREFERENCE: &str = "invalid \\k<> named backreference";
pub const INVALID_IDENTITY_ESCAPE: &str = "invalid escaped character for Unicode pattern";
pub const INVALID_OCTAL_ESCAPE: &str = "invalid octal escape for Unicode pattern";
pub const INVALID_CONTROL_ESCAPE: &str = "invalid \\c escape for Unicode pattern";
pub const INVALID_PROPERTY: &str = "invalid property expression";
pub const INVALID_CLASS_SET_OPERATION: &str = "invalid operation in class set";
pub const NEGATED_MAY_CONTAIN_STRINGS: &str = "negated class set may contain strings";
pub const INVALID_CLASS_SET_CHARACTER: &str = "invalid class set character";
pub const INVALID_MODIFIER: &str = "invalid regular expression modifier";
pub const PATTERN_TOO_LONG: &str = "pattern exceeds string length limits";

type Fail = &'static str;

/// The matcher's IR.
#[derive(Debug, Clone)]
pub enum Node {
    Empty,
    /// One code unit (non-unicode) or code point (unicode).
    Char(u32),
    /// One character of the set.
    Class(CharSet),
    Seq(Vec<Node>),
    Alt(Vec<Node>),
    Capture {
        index: u32,
        body: Box<Node>,
    },
    /// A non-capturing or modifier group (kept for Yarr's pattern size rule; matches as its body).
    Group(Box<Node>),
    Look {
        behind: bool,
        negate: bool,
        body: Box<Node>,
    },
    /// `max == u32::MAX` is unbounded; `captures` is the range of group indices inside `body`.
    Repeat {
        body: Box<Node>,
        min: u32,
        max: u32,
        greedy: bool,
        captures: (u32, u32),
    },
    BackRef {
        groups: Vec<u32>,
        icase: bool,
    },
    /// `\k<name>` before every name is known (an index into the parser's references); never in parse output.
    NamedBackRef {
        reference: usize,
        icase: bool,
    },
    LineStart {
        multiline: bool,
    },
    LineEnd {
        multiline: bool,
    },
    WordBoundary {
        negate: bool,
        /// Under i with u or v the word characters include the characters that fold to them (ſ, K).
        unicode_icase: bool,
    },
}

pub struct Parsed {
    pub node: Node,
    pub capture_count: u32,
    /// Distinct group names in order of first appearance, with every group index carrying the name.
    pub names: Vec<(Vec<u16>, Vec<u32>)>,
}

#[derive(Clone, Copy)]
pub struct ParseFlags {
    pub icase: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub unicode: bool,
    pub unicode_sets: bool,
}

pub fn parse(pattern: &[u16], flags: ParseFlags) -> Result<Parsed, Fail> {
    let mut parser = Parser::new(pattern, flags, u32::MAX, true);
    let node = parser.parse_tokens()?;
    // Yarr's handleIllegalReferences: numeric references beyond the group count, `\k` identity escapes in a
    // pattern with named groups, and named references to missing groups are decided after a whole pass.
    let mut reparse = false;
    let mut backref_limit = u32::MAX;
    let mut forward_allowed = true;
    if parser.max_seen_backref > parser.num_subpatterns {
        if parser.unicode {
            return Err(INVALID_BACKREFERENCE);
        }
        backref_limit = parser.num_subpatterns;
        reparse = true;
    }
    if parser.k_identity_seen && !parser.names.is_empty() {
        return Err(INVALID_NAMED_BACKREFERENCE);
    }
    let illegal_forward = parser.forward_refs.iter().any(|name| !parser.names.iter().any(|(n, _, _)| n == name));
    if illegal_forward {
        if parser.unicode || !parser.names.is_empty() {
            return Err(INVALID_NAMED_BACKREFERENCE);
        }
        forward_allowed = false;
        reparse = true;
    }
    let (node, parser) = if reparse {
        let mut second = Parser::new(pattern, flags, backref_limit, forward_allowed);
        let node = second.parse_tokens()?;
        (node, second)
    } else {
        (node, parser)
    };
    let node = parser.resolve_named_refs(node);
    if check_size(&node, 0, parser.unicode).is_none() {
        return Err(PATTERN_TOO_LONG);
    }
    let mut names: Vec<(Vec<u16>, Vec<u32>)> = Vec::new();
    for (name, index, _) in &parser.names {
        match names.iter_mut().find(|(n, _)| n == name) {
            Some((_, indices)) => indices.push(*index),
            None => names.push((name.clone(), vec![*index])),
        }
    }
    Ok(Parsed { node, capture_count: parser.num_subpatterns, names })
}

#[derive(Clone, Copy, PartialEq)]
enum TermKind {
    Atom,
    LookAhead,
    LookBehind,
}

struct Term {
    node: Node,
    kind: TermKind,
    groups_before: u32,
}

enum FrameKind {
    Root,
    Capture(u32),
    Group,
    Look { behind: bool, negate: bool },
}

struct Frame {
    kind: FrameKind,
    alternatives: Vec<Node>,
    terms: Vec<Term>,
    saved: (bool, bool, bool),
    disjunction: u32,
    alternative: u32,
    groups_before: u32,
}

enum Escape {
    Char(u32),
    /// A class escape: its positive set and whether it is negated (\D, \S, \W, \P).
    Set(CharSet, bool),
    Strings(unicode::StringProperty),
    WordBoundary(bool),
    BackRef(u32),
    NamedBackRef(Vec<u16>),
}

#[derive(Clone, Copy, PartialEq)]
enum OperandKind {
    Char(u32),
    Escape,
    Nested,
    Strings,
}

#[derive(Clone, Copy, PartialEq)]
enum Context {
    Atom,
    Class,
    ClassSet,
}

struct Parser<'a> {
    p: &'a [u16],
    i: usize,
    unicode: bool,
    sets: bool,
    icase: bool,
    multiline: bool,
    dot_all: bool,
    backref_limit: u32,
    forward_allowed: bool,
    num_subpatterns: u32,
    max_seen_backref: u32,
    /// Group names with their group index and alternative path.
    names: Vec<(Vec<u16>, u32, Vec<(u32, u32)>)>,
    forward_refs: Vec<Vec<u16>>,
    k_identity_seen: bool,
    frames: Vec<Frame>,
    next_disjunction: u32,
    /// Named references, resolved to group indices once every name is known.
    named_refs: Vec<Vec<u16>>,
}

fn is_digit(c: u16) -> bool {
    (b'0' as u16..=b'9' as u16).contains(&c)
}

fn hex_value(c: u16) -> Option<u32> {
    match c {
        0x30..=0x39 => Some((c - 0x30) as u32),
        0x41..=0x46 => Some((c - 0x41 + 10) as u32),
        0x61..=0x66 => Some((c - 0x61 + 10) as u32),
        _ => None,
    }
}

fn is_lead(c: u32) -> bool {
    (0xD800..0xDC00).contains(&c)
}

fn is_trail(c: u32) -> bool {
    (0xDC00..0xE000).contains(&c)
}

fn combine(lead: u32, trail: u32) -> u32 {
    0x10000 + ((lead - 0xD800) << 10) + (trail - 0xDC00)
}

const SYNTAX_CHARACTERS: &[u8] = b"^$\\.*+?()[]{}|/";
const CLASS_SET_RESERVED_PUNCTUATORS: &[u8] = b"&-!#%,:;<=>@`~";
const CLASS_SET_DOUBLE_PUNCTUATORS: &[u8] = b"&!#$%*+,.:;<=>?@^`~";
const CLASS_SET_SYNTAX_CHARACTERS: &[u8] = b"()[]{}/-\\|";

fn in_ascii(set: &[u8], c: u32) -> bool {
    c < 0x80 && set.contains(&(c as u8))
}

/// The value of a v-flag class (or class operand): single characters and strings.
#[derive(Clone, Default)]
struct ClassValue {
    set: CharSet,
    strings: Vec<Vec<u32>>,
    may_contain_strings: bool,
}

impl ClassValue {
    fn union(&mut self, other: ClassValue) {
        self.set.add_set(&other.set);
        for s in other.strings {
            if !self.strings.contains(&s) {
                self.strings.push(s);
            }
        }
        self.may_contain_strings |= other.may_contain_strings;
    }

    fn intersect(&mut self, other: ClassValue) {
        self.set = self.set.intersect(&other.set);
        self.strings.retain(|s| other.strings.contains(s));
        self.may_contain_strings &= other.may_contain_strings;
    }

    fn subtract(&mut self, other: ClassValue) {
        self.set = self.set.subtract(&other.set);
        self.strings.retain(|s| !other.strings.contains(s));
    }
}

impl<'a> Parser<'a> {
    fn new(p: &'a [u16], flags: ParseFlags, backref_limit: u32, forward_allowed: bool) -> Parser<'a> {
        Parser {
            p,
            i: 0,
            unicode: flags.unicode || flags.unicode_sets,
            sets: flags.unicode_sets,
            icase: flags.icase,
            multiline: flags.multiline,
            dot_all: flags.dot_all,
            backref_limit,
            forward_allowed,
            num_subpatterns: 0,
            max_seen_backref: 0,
            names: Vec::new(),
            forward_refs: Vec::new(),
            k_identity_seen: false,
            frames: Vec::new(),
            next_disjunction: 0,
            named_refs: Vec::new(),
        }
    }

    fn at_end(&self) -> bool {
        self.i >= self.p.len()
    }

    fn peek(&self) -> u16 {
        self.p[self.i]
    }

    fn peek_is(&self, c: u8) -> bool {
        !self.at_end() && self.p[self.i] == c as u16
    }

    fn peek_at(&self, offset: usize) -> Option<u16> {
        self.p.get(self.i + offset).copied()
    }

    fn try_consume(&mut self, c: u8) -> bool {
        if self.peek_is(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn consume(&mut self) -> u32 {
        let c = self.p[self.i] as u32;
        self.i += 1;
        c
    }

    fn consume_possible_surrogate_pair(&mut self) -> u32 {
        let c = self.consume();
        if self.unicode && is_lead(c) && !self.at_end() && is_trail(self.peek() as u32) {
            let t = self.consume();
            return combine(c, t);
        }
        c
    }

    /// Consumes decimal digits, saturating like Yarr's consumeNumber (overflow reads as "infinite").
    fn consume_number(&mut self) -> u32 {
        let mut n: u64 = 0;
        while !self.at_end() && is_digit(self.peek()) {
            n = n * 10 + (self.consume() - 0x30) as u64;
            if n > u32::MAX as u64 {
                n = u32::MAX as u64 + 1;
            }
        }
        if n > u32::MAX as u64 { u32::MAX } else { n as u32 }
    }

    fn folding(&self) -> &'static Folding {
        if self.unicode { unicode::unicode_folding() } else { unicode::ucs2_folding() }
    }

    fn max_char(&self) -> u32 {
        if self.unicode { MAX_CODE_POINT } else { MAX_CODE_UNIT }
    }

    fn current(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("frame")
    }

    fn push_term(&mut self, node: Node, kind: TermKind, groups_before: u32) {
        self.current().terms.push(Term { node, kind, groups_before });
    }

    fn push_atom(&mut self, node: Node) {
        let groups_before = self.num_subpatterns;
        self.push_term(node, TermKind::Atom, groups_before);
    }

    fn char_node(&self, c: u32) -> Node {
        if self.icase {
            let set = self.folding().close(&CharSet::single(c));
            if set.ranges() != [(c, c)] {
                return Node::Class(set);
            }
        }
        Node::Char(c)
    }

    fn open_frame(&mut self, kind: FrameKind) {
        let disjunction = self.next_disjunction;
        self.next_disjunction += 1;
        let groups_before = self.num_subpatterns;
        let saved = (self.icase, self.multiline, self.dot_all);
        self.frames.push(Frame {
            kind,
            alternatives: Vec::new(),
            terms: Vec::new(),
            saved,
            disjunction,
            alternative: 0,
            groups_before,
        });
    }

    fn finish_alternative(terms: Vec<Term>) -> Node {
        let mut nodes: Vec<Node> = terms.into_iter().map(|t| t.node).filter(|n| !matches!(n, Node::Empty)).collect();
        match nodes.len() {
            0 => Node::Empty,
            1 => nodes.pop().unwrap(),
            _ => Node::Seq(nodes),
        }
    }

    fn close_frame(&mut self) -> (Frame, Node) {
        let mut frame = self.frames.pop().expect("frame");
        let terms = std::mem::take(&mut frame.terms);
        frame.alternatives.push(Self::finish_alternative(terms));
        let body = if frame.alternatives.len() == 1 {
            frame.alternatives.pop().unwrap()
        } else {
            Node::Alt(std::mem::take(&mut frame.alternatives))
        };
        (frame, body)
    }

    fn parse_tokens(&mut self) -> Result<Node, Fail> {
        self.open_frame(FrameKind::Root);
        let mut last_was_atom = false;
        while !self.at_end() {
            let c = self.peek();
            match c {
                0x7C /* | */ => {
                    self.i += 1;
                    let frame = self.current();
                    let terms = std::mem::take(&mut frame.terms);
                    frame.alternatives.push(Self::finish_alternative(terms));
                    frame.alternative += 1;
                    last_was_atom = false;
                }
                0x28 /* ( */ => {
                    self.parse_parentheses_begin()?;
                    last_was_atom = false;
                }
                0x29 /* ) */ => {
                    last_was_atom = self.parse_parentheses_end()?;
                }
                0x5E /* ^ */ => {
                    self.i += 1;
                    let node = Node::LineStart { multiline: self.multiline };
                    let groups_before = self.num_subpatterns;
                    self.push_term(node, TermKind::Atom, groups_before);
                    last_was_atom = false;
                }
                0x24 /* $ */ => {
                    self.i += 1;
                    let node = Node::LineEnd { multiline: self.multiline };
                    let groups_before = self.num_subpatterns;
                    self.push_term(node, TermKind::Atom, groups_before);
                    last_was_atom = false;
                }
                0x2E /* . */ => {
                    self.i += 1;
                    let set = if self.dot_all {
                        CharSet::range(0, self.max_char())
                    } else {
                        unicode::line_terminators().complement(self.max_char())
                    };
                    self.push_atom(Node::Class(set));
                    last_was_atom = true;
                }
                0x5B /* [ */ => {
                    let node = if self.sets { self.parse_class_set()? } else { self.parse_class()? };
                    self.push_atom(node);
                    last_was_atom = true;
                }
                0x5D | 0x7D /* ] } */ => {
                    if self.unicode {
                        return Err(BRACKET_UNMATCHED);
                    }
                    self.i += 1;
                    let node = self.char_node(c as u32);
                    self.push_atom(node);
                    last_was_atom = true;
                }
                0x5C /* \ */ => {
                    last_was_atom = self.parse_atom_escape()?;
                }
                0x2A /* * */ => {
                    self.i += 1;
                    self.parse_quantifier(last_was_atom, 0, u32::MAX)?;
                    last_was_atom = false;
                }
                0x2B /* + */ => {
                    self.i += 1;
                    self.parse_quantifier(last_was_atom, 1, u32::MAX)?;
                    last_was_atom = false;
                }
                0x3F /* ? */ => {
                    self.i += 1;
                    self.parse_quantifier(last_was_atom, 0, 1)?;
                    last_was_atom = false;
                }
                0x7B /* { */ => {
                    let state = self.i;
                    self.i += 1;
                    if !self.at_end() && is_digit(self.peek()) {
                        let min = self.consume_number();
                        let mut max = min;
                        if self.try_consume(b',') {
                            max = if !self.at_end() && is_digit(self.peek()) { self.consume_number() } else { u32::MAX };
                        }
                        if self.try_consume(b'}') {
                            if min > max {
                                return Err(QUANTIFIER_OUT_OF_ORDER);
                            }
                            self.parse_quantifier(last_was_atom, min, max)?;
                            last_was_atom = false;
                            continue;
                        }
                    }
                    if self.unicode {
                        return Err(QUANTIFIER_INCOMPLETE);
                    }
                    self.i = state + 1;
                    let node = self.char_node(0x7B);
                    self.push_atom(node);
                    last_was_atom = true;
                }
                _ => {
                    let ch = self.consume_possible_surrogate_pair();
                    let node = self.char_node(ch);
                    self.push_atom(node);
                    last_was_atom = true;
                }
            }
        }
        if self.frames.len() > 1 {
            return Err(MISSING_PARENTHESES);
        }
        let (_, body) = self.close_frame();
        Ok(body)
    }

    fn parse_quantifier(&mut self, last_was_atom: bool, min: u32, max: u32) -> Result<(), Fail> {
        if !last_was_atom {
            return Err(NOTHING_TO_REPEAT);
        }
        let greedy = !self.try_consume(b'?');
        let num_subpatterns = self.num_subpatterns;
        let frame = self.current();
        let term = frame.terms.pop().expect("quantified term");
        match term.kind {
            TermKind::LookBehind => return Err(CANT_QUANTIFY),
            TermKind::LookAhead => {
                // Yarr runs a quantified assertion once, or drops it when it may match zero times.
                if min == 0 {
                    frame.terms.push(Term { node: Node::Empty, kind: TermKind::Atom, groups_before: term.groups_before });
                } else {
                    frame.terms.push(term);
                }
                return Ok(());
            }
            TermKind::Atom => {}
        }
        if max == 0 {
            frame.terms.push(Term { node: Node::Empty, kind: TermKind::Atom, groups_before: term.groups_before });
            return Ok(());
        }
        let node = Node::Repeat {
            body: Box::new(term.node),
            min,
            max,
            greedy,
            captures: (term.groups_before + 1, num_subpatterns + 1),
        };
        frame.terms.push(Term { node, kind: TermKind::Atom, groups_before: term.groups_before });
        Ok(())
    }

    fn parse_parentheses_begin(&mut self) -> Result<(), Fail> {
        self.i += 1;
        if self.try_consume(b'?') {
            if self.at_end() {
                return Err(PARENTHESES_TYPE_INVALID);
            }
            match self.peek() {
                0x3A /* : */ => {
                    self.i += 1;
                    self.open_frame(FrameKind::Group);
                }
                0x3D /* = */ => {
                    self.i += 1;
                    self.open_frame(FrameKind::Look { behind: false, negate: false });
                }
                0x21 /* ! */ => {
                    self.i += 1;
                    self.open_frame(FrameKind::Look { behind: false, negate: true });
                }
                0x3C /* < */ => {
                    self.i += 1;
                    if self.try_consume(b'=') {
                        self.open_frame(FrameKind::Look { behind: true, negate: false });
                    } else if self.try_consume(b'!') {
                        self.open_frame(FrameKind::Look { behind: true, negate: true });
                    } else {
                        let Some(name) = self.try_consume_group_name()? else {
                            return Err(INVALID_GROUP_NAME);
                        };
                        let path: Vec<(u32, u32)> = self.frames.iter().map(|f| (f.disjunction, f.alternative)).collect();
                        for (other, _, other_path) in &self.names {
                            if *other == name && might_both_participate(&path, other_path) {
                                return Err(DUPLICATE_GROUP_NAME);
                            }
                        }
                        self.num_subpatterns += 1;
                        let index = self.num_subpatterns;
                        self.names.push((name, index, path));
                        self.open_frame(FrameKind::Capture(index));
                        self.frames.last_mut().unwrap().groups_before = index - 1;
                    }
                }
                _ => self.parse_modifiers()?,
            }
        } else {
            self.num_subpatterns += 1;
            let index = self.num_subpatterns;
            self.open_frame(FrameKind::Capture(index));
            self.frames.last_mut().unwrap().groups_before = index - 1;
        }
        Ok(())
    }

    /// After `(?`, at a character that does not start another group kind: `(?ims-ims:`. Yarr scans to the `:` (or
    /// the end of the pattern), remembering the last problem, and reports it only then.
    fn parse_modifiers(&mut self) -> Result<(), Fail> {
        if !matches!(self.peek(), 0x69 | 0x6D | 0x73 | 0x2D) {
            return Err(PARENTHESES_TYPE_INVALID);
        }
        let mut add = 0u8;
        let mut remove = 0u8;
        let mut dash = false;
        let mut error = None;
        while !self.at_end() && self.peek() != 0x3A {
            let bit = match self.consume() {
                0x69 => 1,
                0x6D => 2,
                0x73 => 4,
                0x2D => {
                    if dash {
                        error = Some(INVALID_MODIFIER);
                    }
                    dash = true;
                    continue;
                }
                _ => {
                    error = Some(PARENTHESES_TYPE_INVALID);
                    continue;
                }
            };
            let set = if dash { &mut remove } else { &mut add };
            if *set & bit != 0 {
                error = Some(INVALID_MODIFIER);
            }
            *set |= bit;
        }
        if let Some(error) = error {
            return Err(error);
        }
        if (add == 0 && remove == 0) || add & remove != 0 {
            return Err(INVALID_MODIFIER);
        }
        self.try_consume(b':');
        self.open_frame(FrameKind::Group);
        if add & 1 != 0 {
            self.icase = true;
        }
        if remove & 1 != 0 {
            self.icase = false;
        }
        if add & 2 != 0 {
            self.multiline = true;
        }
        if remove & 2 != 0 {
            self.multiline = false;
        }
        if add & 4 != 0 {
            self.dot_all = true;
        }
        if remove & 4 != 0 {
            self.dot_all = false;
        }
        Ok(())
    }

    /// Returns whether the closed group can be quantified (is an atom, for Yarr's parser).
    fn parse_parentheses_end(&mut self) -> Result<bool, Fail> {
        self.i += 1;
        if self.frames.len() <= 1 {
            return Err(PARENTHESES_UNMATCHED);
        }
        let (frame, body) = self.close_frame();
        (self.icase, self.multiline, self.dot_all) = frame.saved;
        let (node, kind) = match frame.kind {
            FrameKind::Capture(index) => (Node::Capture { index, body: Box::new(body) }, TermKind::Atom),
            FrameKind::Group => (Node::Group(Box::new(body)), TermKind::Atom),
            FrameKind::Look { behind, negate } => (
                Node::Look { behind, negate, body: Box::new(body) },
                if behind { TermKind::LookBehind } else { TermKind::LookAhead },
            ),
            FrameKind::Root => unreachable!(),
        };
        let quantifiable = match kind {
            TermKind::LookAhead => !self.unicode,
            _ => true,
        };
        self.push_term(node, kind, frame.groups_before);
        Ok(quantifiable)
    }

    /// After `<`: a group name and its closing `>`, or None (input restored). A malformed `\u` escape inside is
    /// an error of its own.
    fn try_consume_group_name(&mut self) -> Result<Option<Vec<u16>>, Fail> {
        let state = self.i;
        let mut name = Vec::new();
        if !self.at_end() {
            let ch = self.consume_identifier_character()?;
            if unicode::group_name_start().contains(ch) {
                push_code_point(&mut name, ch);
                while !self.at_end() {
                    let ch = self.consume_identifier_character()?;
                    if ch == '>' as u32 {
                        return Ok(Some(name));
                    }
                    if !unicode::group_name_part().contains(ch) {
                        break;
                    }
                    push_code_point(&mut name, ch);
                }
            }
        }
        self.i = state;
        Ok(None)
    }

    fn consume_identifier_character(&mut self) -> Result<u32, Fail> {
        let c = self.consume();
        if c == '\\' as u32 {
            if !self.try_consume(b'u') {
                return Err(INVALID_UNICODE_ESCAPE);
            }
            return Ok(self.try_consume_unicode_escape(true)?.expect("group name escapes are strict"));
        }
        if is_lead(c) && !self.at_end() && is_trail(self.peek() as u32) {
            let t = self.consume();
            return Ok(combine(c, t));
        }
        Ok(c)
    }

    fn try_consume_hex4(&mut self) -> Option<u32> {
        if self.i + 4 > self.p.len() {
            return None;
        }
        let mut value = 0;
        for k in 0..4 {
            value = value * 16 + hex_value(self.p[self.i + k])?;
        }
        self.i += 4;
        Some(value)
    }

    /// After `\u`. Ok(None): not an escape (non-unicode: an identity escape of `u`).
    fn try_consume_unicode_escape(&mut self, group_name: bool) -> Result<Option<u32>, Fail> {
        let braces = self.unicode || group_name;
        if braces && self.peek_is(b'{') {
            let state = self.i;
            self.i += 1;
            let mut value: u32 = 0;
            let mut digits = 0;
            while !self.at_end() {
                let Some(d) = hex_value(self.peek()) else { break };
                self.i += 1;
                digits += 1;
                value = value.saturating_mul(16).saturating_add(d);
                if value > MAX_CODE_POINT {
                    value = MAX_CODE_POINT + 1;
                }
            }
            if digits > 0 && value <= MAX_CODE_POINT && self.try_consume(b'}') {
                return Ok(Some(value));
            }
            if self.unicode || group_name {
                return Err(INVALID_CODE_POINT_ESCAPE);
            }
            self.i = state;
            return Ok(None);
        }
        let state = self.i;
        let Some(value) = self.try_consume_hex4() else {
            if self.unicode || group_name {
                return Err(INVALID_UNICODE_ESCAPE);
            }
            self.i = state;
            return Ok(None);
        };
        if (self.unicode || group_name) && is_lead(value) && self.peek_is(b'\\') && self.peek_at(1) == Some(b'u' as u16) {
            let state = self.i;
            self.i += 2;
            match self.try_consume_hex4() {
                Some(trail) if is_trail(trail) => return Ok(Some(combine(value, trail))),
                _ => self.i = state,
            }
        }
        Ok(Some(value))
    }

    fn word_set(&self) -> CharSet {
        if self.icase && self.unicode {
            unicode::unicode_folding().close(&unicode::word())
        } else {
            unicode::word()
        }
    }

    /// After `\`: everything an escape can be, in the given context.
    fn parse_escape(&mut self, context: Context) -> Result<Escape, Fail> {
        self.i += 1;
        if self.at_end() {
            return Err(ESCAPE_UNTERMINATED);
        }
        let in_class = context != Context::Atom;
        let c = self.peek();
        match c as u8 {
            _ if c >= 0x80 => {}
            b'b' => {
                self.i += 1;
                return Ok(if in_class { Escape::Char(8) } else { Escape::WordBoundary(false) });
            }
            b'B' => {
                self.i += 1;
                if !in_class {
                    return Ok(Escape::WordBoundary(true));
                }
                if self.unicode {
                    return Err(INVALID_IDENTITY_ESCAPE);
                }
                return Ok(Escape::Char('B' as u32));
            }
            b'd' | b'D' | b's' | b'S' | b'w' | b'W' => {
                self.i += 1;
                let (base, negate) = match c as u8 {
                    b'd' => (unicode::digits(), false),
                    b'D' => (unicode::digits(), true),
                    b's' => (unicode::space().clone(), false),
                    b'S' => (unicode::space().clone(), true),
                    b'w' => (self.word_set(), false),
                    _ => (self.word_set(), true),
                };
                let base = if self.unicode { base } else { base.intersect(&CharSet::range(0, MAX_CODE_UNIT)) };
                return Ok(Escape::Set(base, negate));
            }
            b'1'..=b'9' if !in_class => {
                let state = self.i;
                let n = self.consume_number();
                if n <= self.backref_limit {
                    self.max_seen_backref = self.max_seen_backref.max(n);
                    return Ok(Escape::BackRef(n));
                }
                self.i = state;
                if self.unicode {
                    return Err(INVALID_BACKREFERENCE);
                }
                if self.peek() >= b'8' as u16 {
                    return Ok(Escape::Char(self.consume()));
                }
                return Ok(Escape::Char(self.consume_octal()));
            }
            b'0'..=b'9' => {
                if self.unicode {
                    if c == b'0' as u16 && !(self.peek_at(1).is_some_and(is_digit)) {
                        self.i += 1;
                        return Ok(Escape::Char(0));
                    }
                    return Err(INVALID_OCTAL_ESCAPE);
                }
                if c >= b'8' as u16 {
                    return Ok(Escape::Char(self.consume()));
                }
                return Ok(Escape::Char(self.consume_octal()));
            }
            b'f' => {
                self.i += 1;
                return Ok(Escape::Char(0x0C));
            }
            b'n' => {
                self.i += 1;
                return Ok(Escape::Char(0x0A));
            }
            b'r' => {
                self.i += 1;
                return Ok(Escape::Char(0x0D));
            }
            b't' => {
                self.i += 1;
                return Ok(Escape::Char(0x09));
            }
            b'v' => {
                self.i += 1;
                return Ok(Escape::Char(0x0B));
            }
            b'c' => {
                let state = self.i;
                self.i += 1;
                if !self.at_end() {
                    let control = self.consume();
                    let alpha = (0x41..=0x5A).contains(&control) || (0x61..=0x7A).contains(&control);
                    let class_extra = in_class && !self.unicode && ((0x30..=0x39).contains(&control) || control == 0x5F);
                    if alpha || class_extra {
                        return Ok(Escape::Char(control & 0x1F));
                    }
                    if self.unicode {
                        return Err(INVALID_CONTROL_ESCAPE);
                    }
                } else if self.unicode {
                    return Err(INVALID_IDENTITY_ESCAPE);
                }
                // `\` is a literal; the `c` is read again as a pattern character.
                self.i = state;
                return Ok(Escape::Char('\\' as u32));
            }
            b'x' => {
                self.i += 1;
                if let (Some(a), Some(b)) = (self.peek_at(0).and_then(hex_value), self.peek_at(1).and_then(hex_value)) {
                    self.i += 2;
                    return Ok(Escape::Char(a * 16 + b));
                }
                if self.unicode {
                    return Err(INVALID_IDENTITY_ESCAPE);
                }
                return Ok(Escape::Char('x' as u32));
            }
            b'u' => {
                self.i += 1;
                return Ok(Escape::Char(self.try_consume_unicode_escape(false)?.unwrap_or('u' as u32)));
            }
            b'k' => {
                self.i += 1;
                if !in_class {
                    let state = self.i;
                    if !self.at_end() && self.consume() == '<' as u32 {
                        if let Some(name) = self.try_consume_group_name()? {
                            if self.names.iter().any(|(n, _, _)| *n == name) {
                                return Ok(Escape::NamedBackRef(name));
                            }
                            if self.forward_allowed {
                                self.forward_refs.push(name.clone());
                                return Ok(Escape::NamedBackRef(name));
                            }
                        }
                    }
                    self.i = state;
                }
                if self.unicode {
                    return Err(INVALID_IDENTITY_ESCAPE);
                }
                self.k_identity_seen = true;
                return Ok(Escape::Char('k' as u32));
            }
            b'p' | b'P' => {
                self.i += 1;
                if !self.unicode {
                    return Ok(Escape::Char(c as u32));
                }
                let negate = c == b'P' as u16;
                if !self.try_consume(b'{') {
                    return Err(INVALID_PROPERTY);
                }
                return self.parse_property(negate);
            }
            _ => {}
        }
        // Identity escape. In unicode mode JSC allows any non-ASCII character, not only the syntax characters.
        if self.unicode {
            let c = c as u32;
            let allowed = c >= 0x80
                || in_ascii(SYNTAX_CHARACTERS, c)
                || (in_class && c == '-' as u32)
                || (context == Context::ClassSet && in_ascii(CLASS_SET_RESERVED_PUNCTUATORS, c));
            if !allowed {
                return Err(INVALID_IDENTITY_ESCAPE);
            }
            return Ok(Escape::Char(self.consume_possible_surrogate_pair()));
        }
        Ok(Escape::Char(self.consume()))
    }

    fn consume_octal(&mut self) -> u32 {
        let mut n = self.consume() - 0x30;
        while n < 32 && !self.at_end() && (0x30..=0x37).contains(&self.peek()) {
            n = n * 8 + (self.consume() - 0x30);
        }
        n
    }

    /// After `\p{` / `\P{`.
    fn parse_property(&mut self, negate: bool) -> Result<Escape, Fail> {
        let start = self.i;
        let mut equals = None;
        loop {
            if self.at_end() {
                return Err(INVALID_PROPERTY);
            }
            let c = self.consume();
            if c == '}' as u32 {
                break;
            }
            if c == '=' as u32 {
                if equals.is_some() {
                    return Err(INVALID_PROPERTY);
                }
                equals = Some(self.i - 1);
                continue;
            }
            let ok = c < 0x80 && ((c as u8).is_ascii_alphanumeric() || c == '_' as u32);
            if !ok {
                return Err(INVALID_PROPERTY);
            }
        }
        let end = self.i - 1;
        let text = |a: usize, b: usize| -> String { self.p[a..b].iter().map(|&u| u as u8 as char).collect() };
        let (name, value) = match equals {
            Some(eq) => (Some(text(start, eq)), text(eq + 1, end)),
            None => (None, text(start, end)),
        };
        if value.is_empty() || name.as_deref() == Some("") {
            return Err(INVALID_PROPERTY);
        }
        match unicode::property(name.as_deref(), &value, self.sets) {
            Some(Property::Set(set)) => Ok(Escape::Set(set, negate)),
            Some(Property::Strings(strings)) => {
                if negate {
                    return Err(NEGATED_MAY_CONTAIN_STRINGS);
                }
                Ok(Escape::Strings(strings))
            }
            None => Err(INVALID_PROPERTY),
        }
    }

    /// Returns whether the escape is an atom (quantifiable).
    fn parse_atom_escape(&mut self) -> Result<bool, Fail> {
        let groups_before = self.num_subpatterns;
        match self.parse_escape(Context::Atom)? {
            Escape::Char(c) => {
                let node = self.char_node(c);
                self.push_atom(node);
            }
            Escape::Set(set, negate) => {
                let node = if self.sets {
                    let set = self.fold_v(set);
                    let set = if negate { self.complement_v(&set) } else { set };
                    self.class_value_node(ClassValue { set, strings: Vec::new(), may_contain_strings: false })
                } else {
                    let set = if negate { set.complement(self.max_char()) } else { set };
                    self.class_node(set, false)
                };
                self.push_atom(node);
            }
            Escape::Strings(strings) => {
                let value = self.strings_value(strings);
                let node = self.class_value_node(value);
                self.push_atom(node);
            }
            Escape::WordBoundary(negate) => {
                let node = Node::WordBoundary { negate, unicode_icase: self.icase && self.unicode };
                self.push_term(node, TermKind::Atom, groups_before);
                return Ok(false);
            }
            Escape::BackRef(n) => {
                self.push_atom(Node::BackRef { groups: vec![n], icase: self.icase });
            }
            Escape::NamedBackRef(name) => {
                // Resolved once every name is known (forward references are allowed).
                let reference = self.named_refs.len();
                self.named_refs.push(name);
                self.push_atom(Node::NamedBackRef { reference, icase: self.icase });
            }
        }
        Ok(true)
    }

    fn resolve_named_refs(&self, node: Node) -> Node {
        if self.named_refs.is_empty() {
            return node;
        }
        let resolve = |reference: usize| -> Vec<u32> {
            let name = &self.named_refs[reference];
            self.names.iter().filter(|(n, _, _)| n == name).map(|(_, i, _)| *i).collect()
        };
        map_backrefs(node, &resolve)
    }

    /// A non-v class or class escape: the union of its atoms, closed under case folding, maybe negated.
    fn class_node(&self, set: CharSet, negate: bool) -> Node {
        let set = if self.icase { self.folding().close(&set) } else { set };
        let set = if negate { set.complement(self.max_char()) } else { set };
        Node::Class(set)
    }

    fn parse_class(&mut self) -> Result<Node, Fail> {
        self.i += 1;
        let negate = self.try_consume(b'^');
        let mut set = CharSet::new();
        #[derive(PartialEq)]
        enum State {
            Empty,
            Cached(u32),
            CachedHyphen(u32),
            AfterClass,
            AfterClassHyphen,
        }
        let mut state = State::Empty;
        let unicode = self.unicode;
        loop {
            if self.at_end() {
                return Err(CLASS_UNMATCHED);
            }
            let c = self.peek();
            let (atom, hyphen_is_range) = match c {
                0x5D /* ] */ => {
                    self.i += 1;
                    match state {
                        State::Cached(ch) => set.add(ch),
                        State::CachedHyphen(ch) => {
                            set.add(ch);
                            set.add('-' as u32);
                        }
                        _ => {}
                    }
                    return Ok(self.class_node(set, negate));
                }
                0x5C /* \ */ => match self.parse_escape(Context::Class)? {
                    Escape::Char(ch) => (Ok(ch), false),
                    Escape::Set(s, negate) => (Err(if negate { s.complement(self.max_char()) } else { s }), false),
                    _ => unreachable!("class escapes are characters or sets"),
                },
                _ => (Ok(self.consume_possible_surrogate_pair()), true),
            };
            match atom {
                Ok(ch) => {
                    state = match state {
                        State::AfterClass if hyphen_is_range && ch == '-' as u32 => {
                            set.add('-' as u32);
                            State::AfterClassHyphen
                        }
                        State::Empty | State::AfterClass => State::Cached(ch),
                        State::Cached(prev) => {
                            if hyphen_is_range && ch == '-' as u32 {
                                State::CachedHyphen(prev)
                            } else {
                                set.add(prev);
                                State::Cached(ch)
                            }
                        }
                        State::CachedHyphen(prev) => {
                            if ch < prev {
                                return Err(CLASS_RANGE_OUT_OF_ORDER);
                            }
                            set.add_range(prev, ch);
                            State::Empty
                        }
                        State::AfterClassHyphen => {
                            if unicode {
                                return Err(CLASS_RANGE_INVALID);
                            }
                            set.add(ch);
                            State::Empty
                        }
                    };
                }
                Err(class) => {
                    state = match state {
                        State::Cached(prev) => {
                            set.add(prev);
                            set.add_set(&class);
                            State::AfterClass
                        }
                        State::Empty | State::AfterClass => {
                            set.add_set(&class);
                            State::AfterClass
                        }
                        State::CachedHyphen(prev) => {
                            if unicode {
                                return Err(CLASS_RANGE_INVALID);
                            }
                            set.add(prev);
                            set.add('-' as u32);
                            set.add_set(&class);
                            State::Empty
                        }
                        State::AfterClassHyphen => {
                            if unicode {
                                return Err(CLASS_RANGE_INVALID);
                            }
                            set.add_set(&class);
                            State::Empty
                        }
                    };
                }
            }
        }
    }

    // ---- v flag ------------------------------------------------------------------------------------------------

    /// CharacterComplement for the v flag: under i the universe is the class representatives.
    fn complement_v(&self, set: &CharSet) -> CharSet {
        let all = if self.icase {
            unicode::unicode_folding().non_representatives().complement(MAX_CODE_POINT)
        } else {
            CharSet::range(0, MAX_CODE_POINT)
        };
        all.subtract(set)
    }

    fn fold_v(&self, set: CharSet) -> CharSet {
        if self.icase { unicode::unicode_folding().fold_set(&set) } else { set }
    }

    fn fold_string(&self, s: Vec<u32>) -> Vec<u32> {
        if self.icase {
            let folding = unicode::unicode_folding();
            s.into_iter().map(|c| folding.canon(c)).collect()
        } else {
            s
        }
    }

    fn strings_value(&self, property: unicode::StringProperty) -> ClassValue {
        let mut value = ClassValue {
            set: self.fold_v(property.singles),
            strings: Vec::new(),
            may_contain_strings: true,
        };
        for s in property.strings {
            let s = self.fold_string(s);
            if !value.strings.contains(&s) {
                value.strings.push(s);
            }
        }
        value
    }

    fn parse_class_set(&mut self) -> Result<Node, Fail> {
        self.i += 1;
        let negate = self.try_consume(b'^');
        let value = self.parse_class_set_contents()?;
        if negate {
            if value.may_contain_strings {
                return Err(NEGATED_MAY_CONTAIN_STRINGS);
            }
            let set = self.complement_v(&value.set);
            return Ok(self.class_value_node(ClassValue { set, strings: Vec::new(), may_contain_strings: false }));
        }
        Ok(self.class_value_node(value))
    }

    /// A v-flag class value as a matcher: the longest strings first, then single characters, then "".
    fn class_value_node(&self, value: ClassValue) -> Node {
        let close = |set: &CharSet| if self.icase { unicode::unicode_folding().close(set) } else { set.clone() };
        let mut strings = value.strings;
        let has_empty = strings.iter().any(|s| s.is_empty());
        strings.retain(|s| s.len() > 1);
        if strings.is_empty() && !has_empty {
            return Node::Class(close(&value.set));
        }
        strings.sort_by(|a, b| b.len().cmp(&a.len()));
        let mut alternatives: Vec<Node> = strings
            .into_iter()
            .map(|s| {
                Node::Seq(
                    s.into_iter()
                        .map(|c| {
                            let set = close(&CharSet::single(c));
                            if set.ranges() == [(c, c)] { Node::Char(c) } else { Node::Class(set) }
                        })
                        .collect(),
                )
            })
            .collect();
        if !value.set.is_empty() {
            alternatives.push(Node::Class(close(&value.set)));
        }
        if has_empty {
            alternatives.push(Node::Empty);
        }
        if alternatives.len() == 1 { alternatives.pop().unwrap() } else { Node::Alt(alternatives) }
    }

    fn at_double(&self, c: u8) -> bool {
        self.peek_is(c) && self.peek_at(1) == Some(c as u16)
    }

    /// After `[` (and `^`): a ClassSetExpression through its `]`, as JSC parses it: `&&` and `--` may follow a
    /// union whose characters and ranges are limited to a first character (nested classes and escapes are fine),
    /// and once an operator is in force a nested class may follow an operand without repeating it.
    fn parse_class_set_contents(&mut self) -> Result<ClassValue, Fail> {
        if self.at_end() {
            return Err(CLASS_UNMATCHED);
        }
        if self.try_consume(b']') {
            return Ok(ClassValue::default());
        }
        if self.at_double(b'&') || self.at_double(b'-') {
            return Err(INVALID_CLASS_SET_OPERATION);
        }
        let mut result = ClassValue::default();
        let mut committed = false;
        let mut first = true;
        loop {
            if self.at_end() {
                return Err(CLASS_UNMATCHED);
            }
            if self.try_consume(b']') {
                return Ok(result);
            }
            let intersect = self.at_double(b'&');
            if intersect || self.at_double(b'-') {
                if committed {
                    return Err(INVALID_CLASS_SET_OPERATION);
                }
                return self.parse_class_set_operation(result, intersect);
            }
            let (value, kind) = self.parse_class_set_operand(false)?;
            let single_dash = self.peek_is(b'-') && !self.at_double(b'-');
            match kind {
                OperandKind::Char(a) if single_dash => {
                    let range = self.parse_class_set_range(Some(a))?;
                    result.set.add_set(&self.fold_v(range));
                    committed = true;
                }
                OperandKind::Escape if single_dash => {
                    self.parse_class_set_range(None)?;
                }
                OperandKind::Nested | OperandKind::Strings if single_dash => {
                    return Err(INVALID_CLASS_SET_OPERATION);
                }
                kind => {
                    if matches!(kind, OperandKind::Char(_)) && !first {
                        committed = true;
                    }
                    result.union(value);
                }
            }
            first = false;
        }
    }

    /// At the first `&&` or `--` of an intersection or subtraction whose left side is `result`.
    fn parse_class_set_operation(&mut self, mut result: ClassValue, intersect: bool) -> Result<ClassValue, Fail> {
        let op = if intersect { b'&' } else { b'-' };
        let apply = |result: &mut ClassValue, value: ClassValue| {
            if intersect { result.intersect(value) } else { result.subtract(value) }
        };
        loop {
            self.i += 2;
            if intersect && self.peek_is(b'&') {
                return Err(INVALID_CLASS_SET_CHARACTER);
            }
            let (value, _) = self.parse_class_set_operand(true)?;
            apply(&mut result, value);
            loop {
                if self.at_end() {
                    return Err(CLASS_UNMATCHED);
                }
                if self.try_consume(b']') {
                    return Ok(result);
                }
                if self.at_double(op) {
                    break;
                }
                if self.at_double(b'&') || self.peek_is(b'-') {
                    return Err(INVALID_CLASS_SET_OPERATION);
                }
                let (value, kind) = self.parse_class_set_operand(false)?;
                if kind != OperandKind::Nested {
                    return Err(INVALID_CLASS_SET_OPERATION);
                }
                apply(&mut result, value);
            }
        }
    }

    /// At the `-` after a range's first operand (None: the first operand was not a character).
    fn parse_class_set_range(&mut self, first: Option<u32>) -> Result<CharSet, Fail> {
        self.i += 1;
        if self.peek_is(b'[') || (self.peek_is(b'\\') && self.peek_at(1) == Some(b'q' as u16)) {
            return Err(INVALID_CLASS_SET_CHARACTER);
        }
        let (_, kind) = self.parse_class_set_operand(true)?;
        let (Some(a), OperandKind::Char(b)) = (first, kind) else {
            return Err(CLASS_RANGE_INVALID);
        };
        if b < a {
            return Err(CLASS_RANGE_OUT_OF_ORDER);
        }
        Ok(CharSet::range(a, b))
    }

    /// A ClassSetOperand and its kind. `required`: an operand must come here (after an operator or a range's `-`).
    fn parse_class_set_operand(&mut self, required: bool) -> Result<(ClassValue, OperandKind), Fail> {
        if self.at_end() {
            return Err(CLASS_UNMATCHED);
        }
        let c = self.peek() as u32;
        if c == '[' as u32 {
            self.i += 1;
            let negate = self.try_consume(b'^');
            let inner = self.parse_class_set_contents()?;
            if negate {
                if inner.may_contain_strings {
                    return Err(NEGATED_MAY_CONTAIN_STRINGS);
                }
                let set = self.complement_v(&inner.set);
                return Ok((ClassValue { set, strings: Vec::new(), may_contain_strings: false }, OperandKind::Nested));
            }
            return Ok((inner, OperandKind::Nested));
        }
        if c == '\\' as u32 {
            match self.peek_at(1).map(|u| u as u32) {
                Some(0x71 /* q */) => {
                    self.i += 2;
                    if !self.try_consume(b'{') {
                        return Err(INVALID_IDENTITY_ESCAPE);
                    }
                    return Ok((self.parse_class_string_disjunction()?, OperandKind::Strings));
                }
                Some(0x64 | 0x44 | 0x73 | 0x53 | 0x77 | 0x57 | 0x70 | 0x50) => {
                    let value = match self.parse_escape(Context::ClassSet)? {
                        Escape::Set(set, negate) => {
                            let set = self.fold_v(set);
                            let set = if negate { self.complement_v(&set) } else { set };
                            ClassValue { set, strings: Vec::new(), may_contain_strings: false }
                        }
                        Escape::Strings(strings) => self.strings_value(strings),
                        _ => unreachable!("class set escapes are sets"),
                    };
                    return Ok((value, OperandKind::Escape));
                }
                _ => {}
            }
        }
        let ch = self.parse_class_set_character(required)?;
        let folded = if self.icase { unicode::unicode_folding().canon(ch) } else { ch };
        let value = ClassValue { set: CharSet::single(folded), strings: Vec::new(), may_contain_strings: false };
        Ok((value, OperandKind::Char(ch)))
    }

    fn parse_class_set_character(&mut self, required: bool) -> Result<u32, Fail> {
        if self.at_end() {
            return Err(CLASS_UNMATCHED);
        }
        let c = self.peek() as u32;
        if c == '\\' as u32 {
            if let Some(next) = self.peek_at(1) {
                let next = next as u32;
                if next == 'b' as u32 {
                    self.i += 2;
                    return Ok(8);
                }
                if in_ascii(CLASS_SET_RESERVED_PUNCTUATORS, next) {
                    self.i += 2;
                    return Ok(next);
                }
            }
            return match self.parse_escape(Context::ClassSet)? {
                Escape::Char(ch) => Ok(ch),
                _ => Err(INVALID_CLASS_SET_CHARACTER),
            };
        }
        if in_ascii(CLASS_SET_DOUBLE_PUNCTUATORS, c) && self.peek_at(1) == Some(c as u16) {
            return Err(if required { INVALID_CLASS_SET_CHARACTER } else { INVALID_CLASS_SET_OPERATION });
        }
        if in_ascii(CLASS_SET_SYNTAX_CHARACTERS, c) {
            return Err(INVALID_CLASS_SET_CHARACTER);
        }
        Ok(self.consume_possible_surrogate_pair())
    }

    /// After `\q{`.
    fn parse_class_string_disjunction(&mut self) -> Result<ClassValue, Fail> {
        let mut value = ClassValue::default();
        let mut current: Vec<u32> = Vec::new();
        loop {
            if self.at_end() {
                return Err(CLASS_UNMATCHED);
            }
            let c = self.peek();
            if c == '}' as u16 || c == '|' as u16 {
                self.i += 1;
                let s = self.fold_string(std::mem::take(&mut current));
                if s.len() == 1 {
                    value.set.add(s[0]);
                } else {
                    value.may_contain_strings = true;
                    if !value.strings.contains(&s) {
                        value.strings.push(s);
                    }
                }
                if c == '}' as u16 {
                    return Ok(value);
                }
                continue;
            }
            current.push(self.parse_class_set_character(true)?);
        }
    }
}

fn push_code_point(out: &mut Vec<u16>, c: u32) {
    if c > 0xFFFF {
        let c = c - 0x10000;
        out.push(0xD800 + (c >> 10) as u16);
        out.push(0xDC00 + (c & 0x3FF) as u16);
    } else {
        out.push(c as u16);
    }
}

/// Whether two groups at these alternative paths could both participate in a match: they do unless some
/// disjunction holds them in different alternatives.
fn might_both_participate(a: &[(u32, u32)], b: &[(u32, u32)]) -> bool {
    for (x, y) in a.iter().zip(b.iter()) {
        if x.0 != y.0 {
            return true;
        }
        if x.1 != y.1 {
            return false;
        }
    }
    true
}

/// Yarr's minimum-size bookkeeping (setupAlternativeOffsets): the input position after `node` when it starts at
/// `pos`, counting fixed-count characters and once-only groups; None when it passes u32::MAX, which JSC rejects.
fn check_size(node: &Node, pos: u64, unicode: bool) -> Option<u64> {
    let add = |pos: u64, n: u64| if pos + n > u32::MAX as u64 { None } else { Some(pos + n) };
    let width = |node: &Node| -> Option<u64> {
        match node {
            Node::Char(c) => Some(if unicode && *c > 0xFFFF { 2 } else { 1 }),
            Node::Class(set) => {
                let astral = !set.is_empty() && set.ranges()[0].0 > 0xFFFF;
                Some(if unicode && astral { 2 } else { 1 })
            }
            _ => None,
        }
    };
    match node {
        Node::Char(_) | Node::Class(_) => add(pos, width(node).unwrap()),
        Node::Seq(nodes) => nodes.iter().try_fold(pos, |pos, n| check_size(n, pos, unicode)),
        Node::Alt(alternatives) => {
            let mut min = u64::MAX;
            for a in alternatives {
                min = min.min(check_size(a, pos, unicode)? - pos);
            }
            add(pos, min)
        }
        Node::Capture { body, .. } | Node::Group(body) => check_size(body, pos, unicode),
        Node::Look { behind, body, .. } => {
            check_size(body, if *behind { 0 } else { pos }, unicode)?;
            Some(pos)
        }
        Node::Repeat { body, min, max, .. } => {
            if let Some(w) = width(body) {
                return add(pos, *min as u64 * w);
            }
            if *min == 1 && *max == 1 {
                return check_size(body, pos, unicode);
            }
            check_size(body, pos, unicode)?;
            Some(pos)
        }
        _ => Some(pos),
    }
}

fn map_backrefs(node: Node, f: &dyn Fn(usize) -> Vec<u32>) -> Node {
    match node {
        Node::NamedBackRef { reference, icase } => Node::BackRef { groups: f(reference), icase },
        Node::Seq(nodes) => Node::Seq(nodes.into_iter().map(|n| map_backrefs(n, f)).collect()),
        Node::Alt(nodes) => Node::Alt(nodes.into_iter().map(|n| map_backrefs(n, f)).collect()),
        Node::Capture { index, body } => Node::Capture { index, body: Box::new(map_backrefs(*body, f)) },
        Node::Group(body) => Node::Group(Box::new(map_backrefs(*body, f))),
        Node::Look { behind, negate, body } => Node::Look { behind, negate, body: Box::new(map_backrefs(*body, f)) },
        Node::Repeat { body, min, max, greedy, captures } => {
            Node::Repeat { body: Box::new(map_backrefs(*body, f)), min, max, greedy, captures }
        }
        other => other,
    }
}
