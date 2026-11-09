//! JavaScript RegExp objects and the String.prototype regex algorithms, matching JavaScriptCore (as bun runs it):
//! JSC's pattern grammar (Annex B included) and error messages, its case-insensitive equivalences and Unicode
//! property data (read out of JSC, see harness/jsregex/gen-tables.ts), and every lastIndex side effect.
//!
//! Strings are UTF-16 code units; lone surrogates work everywhere. The engine is a backtracking matcher with an
//! explicit stack (src/jsregex/vm.rs) over IR built by a Yarr-compatible parser (src/jsregex/parser.rs).
//! Verified differentially against bun by harness/jsregex/check.sh.

mod charset;
mod parser;
mod tables;
mod unicode;
mod vm;

use std::cell::Cell;
use std::fmt;
use std::rc::Rc;

/// Why `new RegExp(pattern, flags)` failed, with JSC's message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegExpError {
    Flags(String),
    Pattern(String),
}

impl RegExpError {
    pub fn message(&self) -> &str {
        match self {
            RegExpError::Flags(m) | RegExpError::Pattern(m) => m,
        }
    }
}

impl fmt::Display for RegExpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Flags {
    has_indices: bool,
    global: bool,
    ignore_case: bool,
    multiline: bool,
    dot_all: bool,
    unicode: bool,
    unicode_sets: bool,
    sticky: bool,
}

impl Flags {
    fn parse(flags: &[u16]) -> Option<Flags> {
        let mut out = Flags::default();
        for &f in flags {
            let slot = match f {
                0x64 => &mut out.has_indices,
                0x67 => &mut out.global,
                0x69 => &mut out.ignore_case,
                0x6D => &mut out.multiline,
                0x73 => &mut out.dot_all,
                0x75 => &mut out.unicode,
                0x76 => &mut out.unicode_sets,
                0x79 => &mut out.sticky,
                _ => return None,
            };
            if *slot {
                return None;
            }
            *slot = true;
        }
        if out.unicode && out.unicode_sets {
            return None;
        }
        Some(out)
    }

    fn full_unicode(&self) -> bool {
        self.unicode || self.unicode_sets
    }
}

struct Compiled {
    program: vm::Program,
    /// Distinct group names in pattern order, with the indices of the groups carrying each.
    names: Vec<(Vec<u16>, Vec<u32>)>,
}

/// A RegExp object's state: the compiled pattern, its source and flags, and lastIndex.
pub struct JsRegExp {
    pattern: Rc<[u16]>,
    flags: Flags,
    compiled: Rc<Compiled>,
    last_index: Cell<f64>,
}

/// One successful match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Code-unit ranges: [0] is the whole match; None for a group that did not participate.
    pub captures: Vec<Option<(usize, usize)>>,
    /// Named groups in pattern order with the range of the group that participated (duplicate names across
    /// alternatives: the participating one), or None when the pattern has no named groups.
    pub groups: Option<Vec<(Vec<u16>, Option<(usize, usize)>)>>,
}

impl Match {
    pub fn index(&self) -> usize {
        self.captures[0].map_or(0, |r| r.0)
    }

    pub fn end(&self) -> usize {
        self.captures[0].map_or(0, |r| r.1)
    }
}

const FLAGS_MESSAGE: &str = "Invalid flags supplied to RegExp constructor.";

/// Recently compiled patterns: a regex literal evaluated in a loop makes a new RegExp object each time.
const COMPILE_CACHE_SIZE: usize = 32;

type CacheEntry = (Rc<[u16]>, Flags, Rc<Compiled>);

thread_local! {
    static COMPILE_CACHE: std::cell::RefCell<Vec<CacheEntry>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl JsRegExp {
    /// `new RegExp(pattern, flags)` with string pattern and flags.
    pub fn new(pattern: &[u16], flags: &[u16]) -> Result<JsRegExp, RegExpError> {
        let parsed_flags = Flags::parse(flags).ok_or_else(|| RegExpError::Flags(FLAGS_MESSAGE.to_string()))?;
        // The flags that matter to the compiled program (g, y and d only affect exec's bookkeeping).
        let key_flags = Flags { global: false, sticky: false, has_indices: false, ..parsed_flags };
        let cached = COMPILE_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let index = cache.iter().position(|(p, f, _)| *f == key_flags && **p == *pattern)?;
            let entry = cache.remove(index);
            let compiled = entry.2.clone();
            cache.insert(0, entry);
            Some(compiled)
        });
        let compiled = match cached {
            Some(compiled) => compiled,
            None => {
                let compiled = Rc::new(Self::compile(pattern, parsed_flags)?);
                COMPILE_CACHE.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    cache.insert(0, (Rc::from(pattern), key_flags, compiled.clone()));
                    cache.truncate(COMPILE_CACHE_SIZE);
                });
                compiled
            }
        };
        Ok(JsRegExp { pattern: Rc::from(pattern), flags: parsed_flags, compiled, last_index: Cell::new(0.0) })
    }

    fn compile(pattern: &[u16], parsed_flags: Flags) -> Result<Compiled, RegExpError> {
        let parse_flags = parser::ParseFlags {
            icase: parsed_flags.ignore_case,
            multiline: parsed_flags.multiline,
            dot_all: parsed_flags.dot_all,
            unicode: parsed_flags.unicode,
            unicode_sets: parsed_flags.unicode_sets,
        };
        let parsed = parser::parse(pattern, parse_flags)
            .map_err(|message| RegExpError::Pattern(format!("Invalid regular expression: {message}")))?;
        let program = vm::compile(&parsed.node, parsed.capture_count, parsed_flags.full_unicode());
        Ok(Compiled { program, names: parsed.names })
    }

    /// RegExp.prototype.source, escaped as JSC does it.
    pub fn source(&self) -> Vec<u16> {
        escape_pattern(&self.pattern)
    }

    /// The pattern text as given to the constructor.
    pub fn pattern(&self) -> &[u16] {
        &self.pattern
    }

    /// RegExp.prototype.flags, in canonical order.
    pub fn flags(&self) -> String {
        let f = &self.flags;
        let mut out = String::new();
        for (on, c) in [
            (f.has_indices, 'd'),
            (f.global, 'g'),
            (f.ignore_case, 'i'),
            (f.multiline, 'm'),
            (f.dot_all, 's'),
            (f.unicode, 'u'),
            (f.unicode_sets, 'v'),
            (f.sticky, 'y'),
        ] {
            if on {
                out.push(c);
            }
        }
        out
    }

    pub fn global(&self) -> bool {
        self.flags.global
    }
    pub fn ignore_case(&self) -> bool {
        self.flags.ignore_case
    }
    pub fn multiline(&self) -> bool {
        self.flags.multiline
    }
    pub fn dot_all(&self) -> bool {
        self.flags.dot_all
    }
    pub fn unicode(&self) -> bool {
        self.flags.unicode
    }
    pub fn unicode_sets(&self) -> bool {
        self.flags.unicode_sets
    }
    pub fn sticky(&self) -> bool {
        self.flags.sticky
    }
    pub fn has_indices(&self) -> bool {
        self.flags.has_indices
    }

    /// The number of capturing groups.
    pub fn capture_count(&self) -> usize {
        self.compiled.program.capture_count() as usize
    }

    /// Distinct group names in pattern order.
    pub fn group_names(&self) -> Vec<Vec<u16>> {
        self.compiled.names.iter().map(|(n, _)| n.clone()).collect()
    }

    pub fn last_index(&self) -> f64 {
        self.last_index.get()
    }

    pub fn set_last_index(&self, value: f64) {
        self.last_index.set(value);
    }

    /// RegExpBuiltinExec: global and sticky regexes start at ToLength(lastIndex) and update it (0 on failure).
    pub fn exec(&self, text: &[u16]) -> Option<Match> {
        let global_or_sticky = self.flags.global || self.flags.sticky;
        let last_index = if global_or_sticky { to_length(self.last_index.get()) } else { 0.0 };
        if last_index > text.len() as f64 {
            if global_or_sticky {
                self.last_index.set(0.0);
            }
            return None;
        }
        let mut start = last_index as usize;
        // JSC starts a unicode-mode match that would begin between the halves of a surrogate pair at the pair.
        if self.flags.full_unicode()
            && start > 0
            && start < text.len()
            && is_trail(text[start])
            && is_lead(text[start - 1])
        {
            start -= 1;
        }
        match self.search(text, start, self.flags.sticky) {
            Some(m) => {
                if global_or_sticky {
                    self.last_index.set(m.end() as f64);
                }
                Some(m)
            }
            None => {
                if global_or_sticky {
                    self.last_index.set(0.0);
                }
                None
            }
        }
    }

    /// RegExp.prototype.test.
    pub fn test(&self, text: &[u16]) -> bool {
        self.exec(text).is_some()
    }

    /// A copy with the same pattern and flags and lastIndex 0.
    pub fn clone_regexp(&self) -> JsRegExp {
        JsRegExp {
            pattern: self.pattern.clone(),
            flags: self.flags,
            compiled: self.compiled.clone(),
            last_index: Cell::new(0.0),
        }
    }

    /// The first match at or after `from` (only at `from` when sticky), ignoring lastIndex.
    fn search(&self, text: &[u16], from: usize, sticky: bool) -> Option<Match> {
        let slots = self.compiled.program.search(text, from, sticky)?;
        Some(self.build_match(&slots))
    }

    fn build_match(&self, slots: &[usize]) -> Match {
        let captures: Vec<Option<(usize, usize)>> = slots
            .chunks(2)
            .map(|pair| if pair[0] == usize::MAX || pair[1] == usize::MAX { None } else { Some((pair[0], pair[1])) })
            .collect();
        let groups = if self.compiled.names.is_empty() {
            None
        } else {
            Some(
                self.compiled
                    .names
                    .iter()
                    .map(|(name, indices)| {
                        let range = indices.iter().find_map(|&i| captures[i as usize]);
                        (name.clone(), range)
                    })
                    .collect(),
            )
        };
        Match { captures, groups }
    }
}

impl Clone for JsRegExp {
    fn clone(&self) -> JsRegExp {
        JsRegExp {
            pattern: self.pattern.clone(),
            flags: self.flags,
            compiled: self.compiled.clone(),
            last_index: Cell::new(self.last_index.get()),
        }
    }
}

impl fmt::Debug for JsRegExp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}/{}", String::from_utf16_lossy(&self.source()), self.flags())
    }
}

fn is_lead(u: u16) -> bool {
    (0xD800..0xDC00).contains(&u)
}

fn is_trail(u: u16) -> bool {
    (0xDC00..0xE000).contains(&u)
}

/// ToLength of an already-numeric value.
fn to_length(value: f64) -> f64 {
    if value.is_nan() || value <= 0.0 {
        return 0.0;
    }
    value.trunc().min(9007199254740991.0)
}

fn advance(text: &[u16], index: usize, unicode: bool) -> usize {
    if unicode && index + 1 < text.len() && is_lead(text[index]) && is_trail(text[index + 1]) {
        index + 2
    } else {
        index + 1
    }
}

/// AdvanceStringIndex on a numeric index (which may lie beyond the string).
fn advance_string_index(text: &[u16], index: f64, unicode: bool) -> f64 {
    if !unicode || index + 1.0 >= text.len() as f64 {
        return index + 1.0;
    }
    advance(text, index as usize, unicode) as f64
}

/// EscapeRegExpPattern as JSC writes it: "(?:)" for the empty pattern, `/` outside classes and line terminators
/// escaped (a line terminator after a backslash becomes its escape letter).
fn escape_pattern(pattern: &[u16]) -> Vec<u16> {
    if pattern.is_empty() {
        return "(?:)".encode_utf16().collect();
    }
    let is_lt = |c: u16| c == 0x0A || c == 0x0D || c == 0x2028 || c == 0x2029;
    let mut previous_backslash = false;
    let mut in_brackets = false;
    let mut needs = false;
    for &c in pattern {
        if !previous_backslash {
            if in_brackets {
                if c == b']' as u16 {
                    in_brackets = false;
                }
            } else if c == b'/' as u16 {
                needs = true;
                break;
            } else if c == b'[' as u16 {
                in_brackets = true;
            }
        }
        if is_lt(c) {
            needs = true;
            break;
        }
        previous_backslash = if previous_backslash { false } else { c == b'\\' as u16 };
    }
    if !needs {
        return pattern.to_vec();
    }
    let mut out = Vec::with_capacity(pattern.len() + 8);
    previous_backslash = false;
    in_brackets = false;
    for &c in pattern {
        if !previous_backslash {
            if in_brackets {
                if c == b']' as u16 {
                    in_brackets = false;
                }
            } else if c == b'/' as u16 {
                out.push(b'\\' as u16);
            } else if c == b'[' as u16 {
                in_brackets = true;
            }
        }
        if is_lt(c) {
            if !previous_backslash {
                out.push(b'\\' as u16);
            }
            let escape = match c {
                0x0A => "n",
                0x0D => "r",
                0x2028 => "u2028",
                _ => "u2029",
            };
            out.extend(escape.encode_utf16());
        } else {
            out.push(c);
        }
        previous_backslash = if previous_backslash { false } else { c == b'\\' as u16 };
    }
    out
}

fn capture_strings(m: &Match, text: &[u16]) -> Vec<Option<Vec<u16>>> {
    m.captures[1..].iter().map(|c| c.map(|(a, b)| text[a..b].to_vec())).collect()
}

fn group_strings(m: &Match, text: &[u16]) -> Option<Vec<(Vec<u16>, Option<Vec<u16>>)>> {
    m.groups
        .as_ref()
        .map(|groups| groups.iter().map(|(name, range)| (name.clone(), range.map(|(a, b)| text[a..b].to_vec()))).collect())
}

/// `"s".replace(re, replacement)` (every match when global).
pub fn replace(re: &JsRegExp, text: &[u16], replacement: &[u16]) -> Vec<u16> {
    let matches = replace_matches(re, text);
    let mut out = Vec::with_capacity(text.len());
    let mut next_source_position = 0;
    for m in &matches {
        let (start, end) = m.captures[0].expect("match range");
        let position = start.min(text.len());
        let matched = &text[start..end];
        let captures = capture_strings(m, text);
        let groups = group_strings(m, text);
        let substitution = get_substitution(matched, text, position, &captures, groups.as_deref(), replacement);
        if position >= next_source_position {
            out.extend_from_slice(&text[next_source_position..position]);
            out.extend_from_slice(&substitution);
            next_source_position = position + matched.len();
        }
    }
    if next_source_position < text.len() {
        out.extend_from_slice(&text[next_source_position..]);
    }
    out
}

/// The matches `"s".replace(re, fn)` passes to `fn`, in order (with the lastIndex effects of collecting them).
pub fn replace_matches(re: &JsRegExp, text: &[u16]) -> Vec<Match> {
    if !re.flags.global {
        return re.exec(text).into_iter().collect();
    }
    let unicode = re.flags.full_unicode();
    re.set_last_index(0.0);
    let mut results = Vec::new();
    while let Some(m) = re.exec(text) {
        if m.captures[0].is_some_and(|(a, b)| a == b) {
            let this_index = to_length(re.last_index());
            re.set_last_index(advance_string_index(text, this_index, unicode));
        }
        results.push(m);
    }
    results
}

/// `"s".split(re, limit)`; None stands for an undefined capture. The separator matches through a sticky copy, so
/// `re`'s lastIndex is untouched.
pub fn split(re: &JsRegExp, text: &[u16], limit: Option<u32>) -> Vec<Option<Vec<u16>>> {
    let lim = limit.unwrap_or(u32::MAX);
    let mut out: Vec<Option<Vec<u16>>> = Vec::new();
    if lim == 0 {
        return out;
    }
    let size = text.len();
    if size == 0 {
        if re.search(text, 0, true).is_some() {
            return out;
        }
        out.push(Some(text.to_vec()));
        return out;
    }
    let unicode = re.flags.full_unicode();
    let mut p = 0;
    let mut q = p;
    while q < size {
        // The next position from q at which the sticky splitter matches.
        let Some(m) = re.search(text, q, false) else { break };
        let (start, end) = m.captures[0].expect("match range");
        if start >= size {
            break;
        }
        q = start;
        let e = end.min(size);
        if e == p {
            q = advance(text, q, unicode);
            continue;
        }
        out.push(Some(text[p..q].to_vec()));
        if out.len() as u32 == lim {
            return out;
        }
        p = e;
        for c in &m.captures[1..] {
            out.push(c.map(|(a, b)| text[a..b].to_vec()));
            if out.len() as u32 == lim {
                return out;
            }
        }
        q = p;
    }
    out.push(Some(text[p..].to_vec()));
    out
}

/// `"s".match(re)` for a global re (None is null).
pub fn match_global(re: &JsRegExp, text: &[u16]) -> Option<Vec<Vec<u16>>> {
    let unicode = re.flags.full_unicode();
    re.set_last_index(0.0);
    let mut out = Vec::new();
    while let Some(m) = re.exec(text) {
        let (a, b) = m.captures[0].expect("match range");
        if a == b {
            let this_index = to_length(re.last_index());
            re.set_last_index(advance_string_index(text, this_index, unicode));
        }
        out.push(text[a..b].to_vec());
    }
    if out.is_empty() { None } else { Some(out) }
}

/// `[..."s".matchAll(re)]`: iterates a copy whose lastIndex starts at ToLength(re.lastIndex); re is untouched.
pub fn match_all(re: &JsRegExp, text: &[u16]) -> Vec<Match> {
    let matcher = re.clone_regexp();
    matcher.set_last_index(to_length(re.last_index()));
    let unicode = re.flags.full_unicode();
    let global = re.flags.global;
    let mut out = Vec::new();
    while let Some(m) = matcher.exec(text) {
        let empty = m.captures[0].is_some_and(|(a, b)| a == b);
        out.push(m);
        if !global {
            break;
        }
        if empty {
            let this_index = to_length(matcher.last_index());
            matcher.set_last_index(advance_string_index(text, this_index, unicode));
        }
    }
    out
}

/// `"s".search(re)`: the index of the first match or -1; lastIndex is restored.
pub fn search(re: &JsRegExp, text: &[u16]) -> i64 {
    let previous = re.last_index();
    let same = |a: f64, b: f64| a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan());
    if !same(previous, 0.0) {
        re.set_last_index(0.0);
    }
    let result = re.exec(text);
    if !same(re.last_index(), previous) {
        re.set_last_index(previous);
    }
    result.map_or(-1, |m| m.index() as i64)
}

/// GetSubstitution: `$$`, `$&`, `` $` ``, `$'`, `$n`, `$nn`, `$<name>` in a replacement template.
pub fn get_substitution(
    matched: &[u16],
    text: &[u16],
    position: usize,
    captures: &[Option<Vec<u16>>],
    groups: Option<&[(Vec<u16>, Option<Vec<u16>>)]>,
    replacement: &[u16],
) -> Vec<u16> {
    let dollar = b'$' as u16;
    let is_digit = |u: u16| (0x30..=0x39).contains(&u);
    let tail_position = (position + matched.len()).min(text.len());
    let m = captures.len();
    let mut out = Vec::with_capacity(replacement.len());
    let mut i = 0;
    while i < replacement.len() {
        let c = replacement[i];
        if c != dollar || i + 1 >= replacement.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let n = replacement[i + 1];
        match n {
            0x24 /* $ */ => {
                out.push(dollar);
                i += 2;
            }
            0x26 /* & */ => {
                out.extend_from_slice(matched);
                i += 2;
            }
            0x60 /* ` */ => {
                out.extend_from_slice(&text[..position.min(text.len())]);
                i += 2;
            }
            0x27 /* ' */ => {
                out.extend_from_slice(&text[tail_position..]);
                i += 2;
            }
            0x30..=0x39 => {
                let mut digit_count = if i + 2 < replacement.len() && is_digit(replacement[i + 2]) { 2 } else { 1 };
                let mut index = (n - 0x30) as usize;
                if digit_count == 2 {
                    index = index * 10 + (replacement[i + 2] - 0x30) as usize;
                    if index > m {
                        digit_count = 1;
                        index = (n - 0x30) as usize;
                    }
                }
                if index >= 1 && index <= m {
                    if let Some(capture) = &captures[index - 1] {
                        out.extend_from_slice(capture);
                    }
                } else {
                    out.extend_from_slice(&replacement[i..i + 1 + digit_count]);
                }
                i += 1 + digit_count;
            }
            0x3C /* < */ => {
                let Some(groups) = groups else {
                    out.extend_from_slice(&replacement[i..i + 2]);
                    i += 2;
                    continue;
                };
                match replacement[i + 2..].iter().position(|&u| u == b'>' as u16) {
                    None => {
                        out.extend_from_slice(&replacement[i..i + 2]);
                        i += 2;
                    }
                    Some(offset) => {
                        let name = &replacement[i + 2..i + 2 + offset];
                        if let Some((_, Some(value))) = groups.iter().find(|(n, _)| n == name) {
                            out.extend_from_slice(value);
                        }
                        i += 2 + offset + 1;
                    }
                }
            }
            _ => {
                out.push(dollar);
                i += 1;
            }
        }
    }
    out
}

fn index_of(text: &[u16], search: &[u16], from: usize) -> Option<usize> {
    if search.is_empty() {
        return if from <= text.len() { Some(from) } else { None };
    }
    if search.len() > text.len() {
        return None;
    }
    (from..=text.len() - search.len()).find(|&k| &text[k..k + search.len()] == search)
}

/// The match offsets a string-pattern replace (or replaceAll) visits.
pub fn string_replace_positions(text: &[u16], search: &[u16], all: bool) -> Vec<usize> {
    if !all {
        return index_of(text, search, 0).into_iter().collect();
    }
    let advance_by = search.len().max(1);
    let mut out = Vec::new();
    let mut position = index_of(text, search, 0);
    while let Some(p) = position {
        out.push(p);
        position = index_of(text, search, p + advance_by);
    }
    out
}

/// `"s".replace("x", str)` / `"s".replaceAll("x", str)`.
pub fn string_replace(text: &[u16], search: &[u16], replacement: &[u16], all: bool) -> Vec<u16> {
    let positions = string_replace_positions(text, search, all);
    if positions.is_empty() {
        return text.to_vec();
    }
    let mut out = Vec::with_capacity(text.len());
    let mut end_of_last = 0;
    for p in positions {
        out.extend_from_slice(&text[end_of_last..p]);
        out.extend_from_slice(&get_substitution(search, text, p, &[], None, replacement));
        end_of_last = p + search.len();
    }
    if end_of_last < text.len() {
        out.extend_from_slice(&text[end_of_last..]);
    }
    out
}

/// RegExp.escape.
pub fn escape(text: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(text.len() * 2);
    let hex = |out: &mut Vec<u16>, prefix: &str, value: u32, width: usize| {
        out.extend(prefix.encode_utf16());
        out.extend(format!("{value:0width$x}").encode_utf16());
    };
    for (k, c) in char::decode_utf16(text.iter().copied()).enumerate() {
        let c = match c {
            Ok(c) => c as u32,
            Err(e) => e.unpaired_surrogate() as u32,
        };
        let ascii_alnum = c < 0x80 && (c as u8).is_ascii_alphanumeric();
        if k == 0 && ascii_alnum {
            hex(&mut out, "\\x", c, 2);
            continue;
        }
        if c < 0x80 && b"^$\\.*+?()[]{}|/".contains(&(c as u8)) {
            out.push(b'\\' as u16);
            out.push(c as u16);
            continue;
        }
        let control = match c {
            0x09 => Some('t'),
            0x0A => Some('n'),
            0x0B => Some('v'),
            0x0C => Some('f'),
            0x0D => Some('r'),
            _ => None,
        };
        if let Some(letter) = control {
            out.push(b'\\' as u16);
            out.push(letter as u16);
            continue;
        }
        let punctuator = c < 0x80 && b",-=<>#&!%:;@~'`\"".contains(&(c as u8));
        let space = unicode::space().contains(c) || unicode::is_line_terminator(c);
        let surrogate = (0xD800..0xE000).contains(&c);
        if punctuator || space || surrogate {
            if c <= 0xFF {
                hex(&mut out, "\\x", c, 2);
            } else {
                let mut units = [0u16; 2];
                let encoded: &[u16] = match char::from_u32(c) {
                    Some(ch) => ch.encode_utf16(&mut units),
                    None => {
                        units[0] = c as u16;
                        &units[..1]
                    }
                };
                for &u in encoded {
                    hex(&mut out, "\\u", u as u32, 4);
                }
            }
            continue;
        }
        match char::from_u32(c) {
            Some(ch) => {
                let mut units = [0u16; 2];
                out.extend_from_slice(ch.encode_utf16(&mut units));
            }
            None => out.push(c as u16),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    //! A few of the behaviors the differential check (harness/jsregex/check.sh) pins against JSC, kept here so they
    //! are checked without bun.
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn re(pattern: &str, flags: &str) -> JsRegExp {
        JsRegExp::new(&u(pattern), &u(flags)).expect("valid pattern")
    }

    fn error(pattern: &str, flags: &str) -> String {
        JsRegExp::new(&u(pattern), &u(flags)).err().expect("invalid pattern").message().to_string()
    }

    fn text(units: &[u16]) -> String {
        String::from_utf16_lossy(units)
    }

    #[test]
    fn constructor_errors_use_jsc_messages() {
        assert_eq!(error("a", "gg"), "Invalid flags supplied to RegExp constructor.");
        assert_eq!(error("a", "uv"), "Invalid flags supplied to RegExp constructor.");
        assert_eq!(error("(", ""), "Invalid regular expression: missing )");
        assert_eq!(error("a**", ""), "Invalid regular expression: nothing to repeat");
        assert_eq!(error("\\2(", "u"), "Invalid regular expression: missing )");
        assert_eq!(error("(?i)", ""), "Invalid regular expression: unrecognized character after (?");
        assert_eq!(error("(?ii", ""), "Invalid regular expression: invalid regular expression modifier");
        assert_eq!(error("[ab&&c]", "v"), "Invalid regular expression: invalid operation in class set");
        assert_eq!(error("a{4294967295}x", ""), "Invalid regular expression: pattern exceeds string length limits");
    }

    #[test]
    fn annex_b_syntax_is_accepted_outside_unicode_mode() {
        for pattern in ["]", "{", "a{", "\\c", "[\\c1]", "\\8", "[\\w-a]", "(?=a)*", "\\k<a", "\\q"] {
            assert!(JsRegExp::new(&u(pattern), &[]).is_ok(), "{pattern}");
        }
        assert!(JsRegExp::new(&u("\\ω"), &u("u")).is_ok(), "JSC allows non-ASCII identity escapes");
    }

    #[test]
    fn source_is_escaped_like_jsc() {
        assert_eq!(text(&re("", "").source()), "(?:)");
        assert_eq!(text(&re("a/b[/]\n", "").source()), "a\\/b[/]\\n");
        assert_eq!(text(&re("\\\n", "").source()), "\\n");
        assert_eq!(re("a", "yusmigd").flags(), "dgimsuy");
    }

    #[test]
    fn exec_follows_last_index() {
        let g = re("a", "g");
        g.set_last_index(2.5);
        assert_eq!(g.exec(&u("aaaa")).map(|m| m.index()), Some(2));
        assert_eq!(g.last_index(), 3.0);
        g.set_last_index(9.0);
        assert!(g.exec(&u("aaaa")).is_none());
        assert_eq!(g.last_index(), 0.0);
        // A unicode match never starts inside a surrogate pair.
        let pair = re(".", "gu");
        pair.set_last_index(1.0);
        let m = pair.exec(&u("😀")).unwrap();
        assert_eq!(m.captures[0], Some((0, 2)));
        let sticky = re("\\udc00", "y");
        sticky.set_last_index(1.0);
        assert_eq!(sticky.exec(&u("\u{10000}")).map(|m| m.index()), Some(1));
    }

    #[test]
    fn case_folding_uses_jsc_tables() {
        assert!(re("\\u1c89", "iu").test(&u("\u{1c8a}")));
        assert!(!re("\\u1c89", "i").test(&u("\u{1c8a}")), "the non-unicode table is older");
        assert!(re("\\w", "iu").test(&u("ſ")));
        assert!(!re("\\w", "i").test(&u("ſ")));
        assert!(re("[\\p{Lu}&&k]", "iv").test(&u("K")));
        assert!(!re("\\P{Ll}", "iv").test(&u("a")));
    }

    #[test]
    fn duplicate_named_groups_report_the_participating_group() {
        let m = re("(?<a>x)|(?<a>y)", "").exec(&u("y")).unwrap();
        assert_eq!(m.groups, Some(vec![(u("a"), Some((0, 1)))]));
        assert!(re("(?:(?<a>x)|(?<a>y))\\k<a>", "").test(&u("yy")));
    }

    #[test]
    fn string_algorithms() {
        assert_eq!(text(&replace(&re("(x)", ""), &u("x"), &u("$0|$01|$1|$10|$2|$11"))), "$0|x|x|x0|$2|x1");
        assert_eq!(text(&replace(&re("(?:)", "gu"), &u("😀"), &u("-"))), "-😀-");
        let parts: Vec<String> =
            split(&re("(a)|b", ""), &u("ab"), None).iter().map(|p| p.as_deref().map_or("~".into(), text)).collect();
        assert_eq!(parts, ["", "a", "", "~", ""]);
        let sticky = re("a", "y");
        sticky.set_last_index(1.0);
        assert_eq!(split(&sticky, &u("aaa"), None).len(), 4);
        assert_eq!(sticky.last_index(), 1.0);
        let g = re("a", "g");
        g.set_last_index(-0.0);
        assert_eq!(search(&g, &u("bab")), 1);
        assert!(g.last_index() == 0.0 && g.last_index().is_sign_negative());
        assert_eq!(match_global(&re("a|", "g"), &u("ab")).unwrap().len(), 3);
        assert_eq!(text(&escape(&u("1a b-c"))), "\\x31a\\x20b\\x2dc");
        assert_eq!(string_replace_positions(&u("aaa"), &u(""), true), [0, 1, 2, 3]);
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let depth = 20_000;
        let pattern = "(?:".repeat(depth) + "a" + &")".repeat(depth);
        let handle = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || re(&pattern, "").test(&u("a")))
            .unwrap();
        assert!(handle.join().unwrap());
    }
}
