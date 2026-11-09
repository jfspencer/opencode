//! JavaScript string and RegExp semantics, which the ported code depends on exactly.
//!
//! TS strings are UTF-16: lengths, slices, `charAt` and regex match offsets count code units, and marked mixes
//! those with code point counts. [`JsString`] keeps text as UTF-16 code units so every offset matches TS. [`JsRegex`]
//! runs ECMAScript regexes with `regress` (the same syntax, `\s`, `.`, case folding, lookbehind and `u`-flag
//! semantics as JavaScriptCore), matching non-`u` regexes on code units and `u` regexes on code points.

use std::fmt;

/// A JavaScript string: UTF-16 code units, possibly with lone surrogates.
#[derive(Clone, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct JsString(pub Vec<u16>);

impl fmt::Debug for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.to_string(), f)
    }
}

impl fmt::Display for JsString {
    /// Lone surrogates become U+FFFD, as when TS hands the string to native code as UTF-8.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf16_lossy(&self.0))
    }
}

impl From<&str> for JsString {
    fn from(s: &str) -> Self {
        JsString(s.encode_utf16().collect())
    }
}

impl From<String> for JsString {
    fn from(s: String) -> Self {
        JsString::from(s.as_str())
    }
}

impl From<&String> for JsString {
    fn from(s: &String) -> Self {
        JsString::from(s.as_str())
    }
}

impl From<&[u16]> for JsString {
    fn from(s: &[u16]) -> Self {
        JsString(s.to_vec())
    }
}

impl From<Vec<u16>> for JsString {
    fn from(s: Vec<u16>) -> Self {
        JsString(s)
    }
}

impl PartialEq<str> for JsString {
    fn eq(&self, other: &str) -> bool {
        self.0.iter().copied().eq(other.encode_utf16())
    }
}

impl PartialEq<&str> for JsString {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

impl std::ops::Deref for JsString {
    type Target = [u16];
    fn deref(&self) -> &[u16] {
        &self.0
    }
}

/// The code units of a string literal.
pub fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// JS `WhiteSpace` and `LineTerminator`: the set `trim()` removes and `\s` matches.
pub fn is_js_whitespace(c: u16) -> bool {
    matches!(
        c,
        0x09 | 0x0a | 0x0b | 0x0c | 0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

/// Resolves a JS relative index (negative counts from the end) the way `slice` does.
fn rel(index: isize, len: usize) -> usize {
    if index < 0 {
        len.saturating_sub(index.unsigned_abs())
    } else {
        (index as usize).min(len)
    }
}

impl JsString {
    pub fn new() -> Self {
        JsString(Vec::new())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_slice(&self) -> &[u16] {
        &self.0
    }

    pub fn push_str(&mut self, s: &[u16]) {
        self.0.extend_from_slice(s);
    }

    pub fn push_literal(&mut self, s: &str) {
        self.0.extend(s.encode_utf16());
    }

    /// `a + b`.
    pub fn concat(&self, other: &[u16]) -> JsString {
        let mut v = Vec::with_capacity(self.len() + other.len());
        v.extend_from_slice(&self.0);
        v.extend_from_slice(other);
        JsString(v)
    }
}

/// String operations on code-unit slices, named after their JS counterparts.
pub trait JsStr {
    fn units(&self) -> &[u16];

    /// `s.slice(start, end)` with JS index rules.
    fn js_slice(&self, start: isize, end: Option<isize>) -> JsString {
        let s = self.units();
        let len = s.len();
        let from = rel(start, len);
        let to = end.map_or(len, |e| rel(e, len));
        if from >= to { JsString::new() } else { JsString(s[from..to].to_vec()) }
    }

    /// `s.substring(start, end)`: clamps at 0 and swaps a reversed pair.
    fn js_substring(&self, start: isize, end: Option<isize>) -> JsString {
        let s = self.units();
        let len = s.len() as isize;
        let a = start.clamp(0, len) as usize;
        let b = end.unwrap_or(len).clamp(0, len) as usize;
        let (from, to) = if a <= b { (a, b) } else { (b, a) };
        JsString(s[from..to].to_vec())
    }

    /// `s.charAt(i)` (empty past the end).
    fn char_at(&self, i: usize) -> JsString {
        self.units().get(i).map_or_else(JsString::new, |&c| JsString(vec![c]))
    }

    /// `s[i]` as a code unit.
    fn unit_at(&self, i: isize) -> Option<u16> {
        if i < 0 { None } else { self.units().get(i as usize).copied() }
    }

    fn js_starts_with(&self, prefix: &[u16]) -> bool {
        self.units().starts_with(prefix)
    }

    /// `s.startsWith(prefix, position)`.
    fn starts_with_at(&self, prefix: &[u16], position: usize) -> bool {
        let s = self.units();
        position <= s.len() && s[position..].starts_with(prefix)
    }

    fn js_ends_with(&self, suffix: &[u16]) -> bool {
        self.units().ends_with(suffix)
    }

    /// `s.indexOf(needle, from)`, -1 when absent.
    fn index_of(&self, needle: &[u16], from: usize) -> isize {
        let s = self.units();
        let from = from.min(s.len());
        if needle.is_empty() {
            return from as isize;
        }
        if needle.len() > s.len() {
            return -1;
        }
        for i in from..=s.len() - needle.len() {
            if &s[i..i + needle.len()] == needle {
                return i as isize;
            }
        }
        -1
    }

    /// `s.lastIndexOf(needle)`.
    fn last_index_of(&self, needle: &[u16]) -> isize {
        let s = self.units();
        if needle.len() > s.len() {
            return -1;
        }
        for i in (0..=s.len() - needle.len()).rev() {
            if &s[i..i + needle.len()] == needle {
                return i as isize;
            }
        }
        -1
    }

    fn includes(&self, needle: &[u16]) -> bool {
        self.index_of(needle, 0) >= 0
    }

    fn js_trim(&self) -> JsString {
        let s = self.units();
        let start = s.iter().position(|&c| !is_js_whitespace(c)).unwrap_or(s.len());
        let end = s.iter().rposition(|&c| !is_js_whitespace(c)).map_or(start, |e| e + 1);
        JsString(s[start..end.max(start)].to_vec())
    }

    fn js_trim_start(&self) -> JsString {
        let s = self.units();
        let start = s.iter().position(|&c| !is_js_whitespace(c)).unwrap_or(s.len());
        JsString(s[start..].to_vec())
    }

    fn js_trim_end(&self) -> JsString {
        let s = self.units();
        let end = s.iter().rposition(|&c| !is_js_whitespace(c)).map_or(0, |e| e + 1);
        JsString(s[..end].to_vec())
    }

    /// `s.split(separator)` with a string separator.
    fn split_str(&self, sep: &[u16]) -> Vec<JsString> {
        let s = self.units();
        if sep.is_empty() {
            return s.iter().map(|&c| JsString(vec![c])).collect();
        }
        let mut parts = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i + sep.len() <= s.len() {
            if &s[i..i + sep.len()] == sep {
                parts.push(JsString(s[start..i].to_vec()));
                i += sep.len();
                start = i;
            } else {
                i += 1;
            }
        }
        parts.push(JsString(s[start..].to_vec()));
        parts
    }

    /// `s.split(separator, 1)[0]`: the text before the first separator.
    fn split_first_on(&self, sep: &[u16]) -> JsString {
        let i = self.index_of(sep, 0);
        if i < 0 { JsString(self.units().to_vec()) } else { JsString(self.units()[..i as usize].to_vec()) }
    }

    /// `s.toLowerCase()`.
    fn to_lower(&self) -> JsString {
        let s = self.units();
        if s.iter().all(|&c| c < 0x80) {
            return JsString(s.iter().map(|&c| (c as u8).to_ascii_lowercase() as u16).collect());
        }
        // Lone surrogates pass through unchanged, as in JS.
        let mut out = Vec::with_capacity(s.len());
        let mut run = Vec::new();
        for item in char::decode_utf16(s.iter().copied()) {
            match item {
                Ok(c) => run.push(c),
                Err(e) => {
                    let text: String = run.drain(..).collect();
                    out.extend(text.to_lowercase().encode_utf16());
                    out.push(e.unpaired_surrogate());
                }
            }
        }
        let text: String = run.drain(..).collect();
        out.extend(text.to_lowercase().encode_utf16());
        JsString(out)
    }

    /// `s.repeat(n)`.
    fn repeat(&self, n: usize) -> JsString {
        JsString(self.units().repeat(n))
    }

    /// The number of code points: `[...s].length`.
    fn code_point_len(&self) -> usize {
        let s = self.units();
        let mut n = 0;
        let mut i = 0;
        while i < s.len() {
            if is_high_surrogate(s[i]) && i + 1 < s.len() && is_low_surrogate(s[i + 1]) {
                i += 2;
            } else {
                i += 1;
            }
            n += 1;
        }
        n
    }

    /// The code units of the first code point: `[...s][0]`.
    fn first_code_point_len(&self) -> usize {
        let s = self.units();
        if s.is_empty() {
            0
        } else if is_high_surrogate(s[0]) && s.len() > 1 && is_low_surrogate(s[1]) {
            2
        } else {
            1
        }
    }

    fn to_js(&self) -> JsString {
        JsString(self.units().to_vec())
    }
}

impl JsStr for [u16] {
    fn units(&self) -> &[u16] {
        self
    }
}

impl JsStr for JsString {
    fn units(&self) -> &[u16] {
        &self.0
    }
}

impl JsStr for Vec<u16> {
    fn units(&self) -> &[u16] {
        self
    }
}

pub fn is_high_surrogate(c: u16) -> bool {
    (0xd800..=0xdbff).contains(&c)
}

pub fn is_low_surrogate(c: u16) -> bool {
    (0xdc00..=0xdfff).contains(&c)
}

/// A regex match: the matched range and the capture groups, as code-unit ranges.
#[derive(Clone, Debug)]
pub struct JsMatch {
    pub start: usize,
    pub end: usize,
    /// Index 0 is the whole match.
    pub groups: Vec<Option<(usize, usize)>>,
}

impl JsMatch {
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// `match[i]` (None when the group did not participate).
    pub fn get(&self, s: &[u16], i: usize) -> Option<JsString> {
        self.groups.get(i).copied().flatten().map(|(a, b)| JsString(s[a..b].to_vec()))
    }

    /// `match[i]` where an unmatched group reads as the empty string (the `match[i] || ""` idiom).
    pub fn get_or_empty(&self, s: &[u16], i: usize) -> JsString {
        self.get(s, i).unwrap_or_default()
    }

    /// Whether `match[i]` is truthy: it participated and is non-empty.
    pub fn truthy(&self, i: usize) -> bool {
        matches!(self.groups.get(i), Some(Some((a, b))) if b > a)
    }

    /// Whether group `i` participated (`match[i] !== undefined`).
    pub fn has(&self, i: usize) -> bool {
        matches!(self.groups.get(i), Some(Some(_)))
    }
}

/// A compiled ECMAScript regex with its flags.
pub struct JsRegex {
    re: regress::Regex,
    pub source: String,
    pub global: bool,
    pub unicode: bool,
    pub sticky: bool,
}

impl fmt::Debug for JsRegex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}/", self.source)
    }
}

/// A replacement for [`JsRegex::replace`]: a pattern string with `$` substitutions, or a closure.
pub enum Replacement<'a> {
    /// A replacement pattern: `$1`..`$99`, `$&`, `` $` ``, `$'`, `$$`, `$<name>` is not supported.
    Pattern(&'a str),
    Func(&'a mut dyn FnMut(&JsMatch, &[u16]) -> JsString),
}

impl JsRegex {
    /// `new RegExp(source, flags)`. Errors are SyntaxErrors in JS.
    pub fn new(source: &str, flags: &str) -> Result<JsRegex, String> {
        let mut engine_flags = String::new();
        for f in flags.chars() {
            match f {
                'i' | 'm' | 's' | 'u' | 'v' => engine_flags.push(f),
                'g' | 'y' | 'd' => {}
                other => return Err(format!("Invalid regular expression flags '{other}'")),
            }
        }
        // Validate the source as written, then compile the anchored form when every alternative starts with `^`:
        // the same matches, but regress only detects anchoring at the start of the whole pattern.
        let mut re = regress::Regex::with_flags(source, engine_flags.as_str()).map_err(|e| e.to_string())?;
        if !flags.contains('m') && all_alternatives_anchored(source) && !source.starts_with('^') {
            if let Ok(anchored) = regress::Regex::with_flags(&format!("^(?:{source})"), engine_flags.as_str()) {
                re = anchored;
            }
        }
        Ok(JsRegex {
            re,
            source: source.to_owned(),
            global: flags.contains('g'),
            unicode: flags.contains('u') || flags.contains('v'),
            sticky: flags.contains('y'),
        })
    }

    /// For regexes known to compile.
    pub fn compile(source: &str, flags: &str) -> JsRegex {
        JsRegex::new(source, flags).unwrap_or_else(|e| panic!("regex /{source}/{flags}: {e}"))
    }

    fn convert(m: regress::Match) -> JsMatch {
        let mut groups = Vec::with_capacity(m.captures.len() + 1);
        groups.push(Some((m.range.start, m.range.end)));
        for c in &m.captures {
            groups.push(c.as_ref().map(|r| (r.start, r.end)));
        }
        JsMatch { start: m.range.start, end: m.range.end, groups }
    }

    /// The first match starting at or after `from`.
    pub fn find_from(&self, s: &[u16], from: usize) -> Option<JsMatch> {
        if from > s.len() {
            return None;
        }
        let m = if self.unicode {
            self.re.find_from_utf16(s, from).next()
        } else {
            self.re.find_from_ucs2(s, from).next()
        };
        m.map(Self::convert)
    }

    /// A match starting exactly at `at` (sticky matching).
    pub fn match_at(&self, s: &[u16], at: usize) -> Option<JsMatch> {
        // regress has no sticky mode: search from `at` and keep only a match that starts there. A leftmost
        // search that finds a later match first cannot hide one at `at`, since the earliest start wins.
        self.find_from(s, at).filter(|m| m.start == at)
    }

    /// `re.exec(s)` for a regex without the g or y flag.
    pub fn exec(&self, s: &[u16]) -> Option<JsMatch> {
        self.find_from(s, 0)
    }

    /// `re.exec(s)` on a stateful (g or y) regex with its `lastIndex`.
    pub fn exec_stateful(&self, s: &[u16], last_index: &mut usize) -> Option<JsMatch> {
        let from = *last_index;
        let found = if from > s.len() {
            None
        } else if self.sticky {
            self.match_at(s, from)
        } else {
            self.find_from(s, from)
        };
        match found {
            Some(m) => {
                *last_index = m.end;
                Some(m)
            }
            None => {
                *last_index = 0;
                None
            }
        }
    }

    /// `re.test(s)` for a regex without the g or y flag.
    pub fn test(&self, s: &[u16]) -> bool {
        self.exec(s).is_some()
    }

    /// `s.search(re)`.
    pub fn search(&self, s: &[u16]) -> isize {
        self.exec(s).map_or(-1, |m| m.start as isize)
    }

    /// The index after `i` when stepping over an empty match: a code point with the u flag, else a unit.
    fn advance(&self, s: &[u16], i: usize) -> usize {
        if self.unicode && i + 1 < s.len() && is_high_surrogate(s[i]) && is_low_surrogate(s[i + 1]) {
            i + 2
        } else {
            i + 1
        }
    }

    /// Every match, as `s.matchAll(re)` or a global `replace` finds them.
    pub fn find_all(&self, s: &[u16]) -> Vec<JsMatch> {
        let mut out = Vec::new();
        let mut index = 0;
        while index <= s.len() {
            let Some(m) = self.find_from(s, index) else { break };
            index = if m.end == m.start { self.advance(s, m.end) } else { m.end };
            out.push(m);
        }
        out
    }

    /// `s.replace(re, replacement)`: every match with the g flag, else the first.
    pub fn replace(&self, s: &[u16], replacement: Replacement<'_>) -> JsString {
        let matches = if self.global { self.find_all(s) } else { self.exec(s).into_iter().collect() };
        if matches.is_empty() {
            return JsString(s.to_vec());
        }
        let mut out = Vec::with_capacity(s.len());
        let mut last = 0;
        let mut replacement = replacement;
        for m in &matches {
            out.extend_from_slice(&s[last..m.start]);
            match &mut replacement {
                Replacement::Pattern(p) => expand_pattern(&mut out, p, m, s),
                Replacement::Func(f) => out.extend_from_slice(&f(m, s)),
            }
            last = m.end;
        }
        out.extend_from_slice(&s[last..]);
        JsString(out)
    }

    /// `s.replace(re, "pattern")`.
    pub fn replace_str(&self, s: &[u16], pattern: &str) -> JsString {
        self.replace(s, Replacement::Pattern(pattern))
    }

    /// `s.split(re)` per `RegExp.prototype[@@split]`, captures included.
    pub fn split(&self, s: &[u16]) -> Vec<Option<JsString>> {
        let size = s.len();
        let mut out = Vec::new();
        if size == 0 {
            if self.match_at(s, 0).is_some() {
                return out;
            }
            out.push(Some(JsString::new()));
            return out;
        }
        let mut p = 0;
        let mut q = p;
        while q < size {
            match self.match_at(s, q) {
                None => q = self.advance(s, q),
                Some(m) => {
                    let e = m.end.min(size);
                    if e == p {
                        q = self.advance(s, q);
                    } else {
                        out.push(Some(JsString(s[p..q].to_vec())));
                        for g in &m.groups[1..] {
                            out.push(g.map(|(a, b)| JsString(s[a..b].to_vec())));
                        }
                        p = e;
                        q = p;
                    }
                }
            }
        }
        out.push(Some(JsString(s[p..size].to_vec())));
        out
    }
}

/// Whether each top-level alternative of a regex source begins with `^`.
fn all_alternatives_anchored(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut in_class = false;
    let mut at_start = true;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if at_start && depth == 0 {
            if c != b'^' {
                return false;
            }
            at_start = false;
        }
        match c {
            b'\\' => i += 1,
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            b'(' if !in_class => depth += 1,
            b')' if !in_class => depth = depth.saturating_sub(1),
            b'|' if !in_class && depth == 0 => at_start = true,
            _ => {}
        }
        i += 1;
    }
    !at_start
}

/// Appends a `String.prototype.replace` replacement pattern, expanded for one match.
fn expand_pattern(out: &mut Vec<u16>, pattern: &str, m: &JsMatch, s: &[u16]) {
    let p: Vec<u16> = pattern.encode_utf16().collect();
    let groups = m.groups.len() - 1;
    let mut i = 0;
    while i < p.len() {
        let c = p[i];
        if c != b'$' as u16 || i + 1 >= p.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let next = p[i + 1];
        match next {
            0x24 => {
                out.push(0x24);
                i += 2;
            }
            0x26 => {
                out.extend_from_slice(&s[m.start..m.end]);
                i += 2;
            }
            0x60 => {
                out.extend_from_slice(&s[..m.start]);
                i += 2;
            }
            0x27 => {
                out.extend_from_slice(&s[m.end..]);
                i += 2;
            }
            d if (0x30..=0x39).contains(&d) => {
                let first = (d - 0x30) as usize;
                let two = p.get(i + 2).filter(|c| (0x30..=0x39).contains(*c)).map(|c| first * 10 + (*c - 0x30) as usize);
                if let Some(n) = two.filter(|&n| n >= 1 && n <= groups) {
                    if let Some(Some((a, b))) = m.groups.get(n) {
                        out.extend_from_slice(&s[*a..*b]);
                    }
                    i += 3;
                } else if first >= 1 && first <= groups {
                    if let Some(Some((a, b))) = m.groups.get(first) {
                        out.extend_from_slice(&s[*a..*b]);
                    }
                    i += 2;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
}

/// `Number(s)` for the decimal digit strings the ported code converts.
pub fn parse_decimal(s: &[u16]) -> f64 {
    String::from_utf16_lossy(s).trim().parse::<f64>().unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn js(s: &str) -> JsString {
        JsString::from(s)
    }

    #[test]
    fn replace_and_split() {
        let re = JsRegex::compile(r"(\d)", "g");
        assert_eq!(re.replace_str(&js("a1b2"), "[$1]").to_string(), "a[1]b[2]");
        let re = JsRegex::compile(r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", "");
        let parts = re.split(&js("@@ -1 +2,3 @@ x"));
        let shown: Vec<Option<String>> = parts.into_iter().map(|p| p.map(|s| s.to_string())).collect();
        assert_eq!(
            shown,
            vec![
                Some("".into()),
                Some("1".into()),
                None,
                Some("2".into()),
                Some("3".into()),
                Some(" x".into())
            ]
        );
    }

    #[test]
    fn unicode_and_units() {
        let s = js("a😀b");
        assert_eq!(s.len(), 4);
        assert_eq!(s.code_point_len(), 3);
        let re = JsRegex::compile(r"^.", "");
        assert_eq!(re.exec(&s.js_slice(1, None)).unwrap().len(), 1);
        let re = JsRegex::compile(r"^.", "u");
        assert_eq!(re.exec(&s.js_slice(1, None)).unwrap().len(), 2);
        assert_eq!(js("  a\u{a0}").js_trim().to_string(), "a");
    }
}
