//! JavaScript string semantics over UTF-16 code units.
//!
//! The TS scanner indexes strings by UTF-16 code unit, and every offset it reports (`wordEnds`, `redirectPrefixEnd`)
//! is one. The port therefore works on `[u16]` throughout and converts at the public boundary. Positions are `isize`
//! because the TS code computes `index - 1` before a loop increment and compares against `-1`.

use crate::unicode;

/// A JS string: UTF-16 code units, possibly with lone surrogates.
pub type JsString = Vec<u16>;

pub fn units(text: &str) -> JsString {
    text.encode_utf16().collect()
}

/// The text of a JS string, with lone surrogates replaced by U+FFFD.
pub fn text(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// `s[i]`: the code unit at `i`, or `None` (JS `undefined`) out of range.
#[inline]
pub fn at(s: &[u16], i: isize) -> Option<u16> {
    if i < 0 { None } else { s.get(i as usize).copied() }
}

#[inline]
pub fn len(s: &[u16]) -> isize {
    s.len() as isize
}

/// `s.slice(start, end)`: negative positions count from the end, then both clamp to the string.
pub fn slice(s: &[u16], start: isize, end: isize) -> &[u16] {
    let n = len(s);
    let clamp = |p: isize| if p < 0 { (n + p).max(0) } else { p.min(n) };
    let (a, b) = (clamp(start), clamp(end));
    if a >= b { &[] } else { &s[a as usize..b as usize] }
}

/// `s.slice(start)`.
pub fn slice_from(s: &[u16], start: isize) -> &[u16] {
    slice(s, start, len(s))
}

/// `s.startsWith(pattern, position)`.
pub fn starts_with_at(s: &[u16], pattern: &[u16], position: isize) -> bool {
    let p = position.clamp(0, len(s)) as usize;
    s[p..].starts_with(pattern)
}

/// `s.startsWith(pattern, position)` for an ASCII pattern.
pub fn starts_with_ascii(s: &[u16], pattern: &str, position: isize) -> bool {
    let p = position.clamp(0, len(s)) as usize;
    let rest = &s[p..];
    rest.len() >= pattern.len() && pattern.bytes().zip(rest).all(|(b, &c)| c == b as u16)
}

/// `s.indexOf(unit, from)`.
pub fn index_of(s: &[u16], unit: u16, from: isize) -> isize {
    let p = from.clamp(0, len(s)) as usize;
    s[p..].iter().position(|&c| c == unit).map_or(-1, |i| (i + p) as isize)
}

/// `s.indexOf(pattern, from)` for a pattern of several units.
pub fn index_of_seq(s: &[u16], pattern: &[u16], from: isize) -> isize {
    let p = from.clamp(0, len(s)) as usize;
    if pattern.is_empty() {
        return p as isize;
    }
    if pattern.len() > s.len() {
        return -1;
    }
    (p..=s.len() - pattern.len()).find(|&i| s[i..].starts_with(pattern)).map_or(-1, |i| i as isize)
}

pub fn includes(s: &[u16], unit: u16) -> bool {
    s.contains(&unit)
}

pub fn includes_seq(s: &[u16], pattern: &[u16]) -> bool {
    index_of_seq(s, pattern, 0) >= 0
}

pub fn includes_ascii(s: &[u16], pattern: &str) -> bool {
    let pattern = units(pattern);
    includes_seq(s, &pattern)
}

/// `chars.includes(c)` for a literal set of ASCII characters, where `c` may be JS `undefined` turned into `""` by a
/// `?? ""` fallback: `"abc".includes("")` is true.
#[inline]
pub fn set_has_or_empty(chars: &str, c: Option<u16>) -> bool {
    match c {
        None => true,
        Some(c) => set_has(chars, c),
    }
}

/// `chars.includes(c)` for one code unit and a literal set of ASCII characters.
#[inline]
pub fn set_has(chars: &str, c: u16) -> bool {
    c < 128 && chars.as_bytes().contains(&(c as u8))
}

/// `set.includes(c)` where `c` may be undefined (no `?? ""` fallback): `"abc".includes(undefined)` searches for
/// the text "undefined", which a short set never holds.
#[inline]
pub fn set_has_opt(chars: &str, c: Option<u16>) -> bool {
    c.is_some_and(|c| set_has(chars, c))
}

#[inline]
pub fn is(c: Option<u16>, ascii: u8) -> bool {
    c == Some(ascii as u16)
}

/// JS `\s` and `String.prototype.trim` whitespace.
#[inline]
pub fn is_space(c: u16) -> bool {
    matches!(c, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

#[inline]
pub fn is_space_opt(c: Option<u16>) -> bool {
    c.is_some_and(is_space)
}

/// A regex `.` without the `s` flag: anything but a line terminator.
#[inline]
pub fn is_dot(c: u16) -> bool {
    !matches!(c, 0x0a | 0x0d | 0x2028 | 0x2029)
}

#[inline]
pub fn is_digit(c: u16) -> bool {
    (b'0' as u16..=b'9' as u16).contains(&c)
}

#[inline]
pub fn is_digit_opt(c: Option<u16>) -> bool {
    c.is_some_and(is_digit)
}

#[inline]
pub fn is_alpha(c: u16) -> bool {
    (b'a' as u16..=b'z' as u16).contains(&c) || (b'A' as u16..=b'Z' as u16).contains(&c)
}

/// `[A-Za-z_]`
#[inline]
pub fn is_ident_start(c: u16) -> bool {
    is_alpha(c) || c == b'_' as u16
}

/// `[A-Za-z0-9_]`, regex `\w`.
#[inline]
pub fn is_word(c: u16) -> bool {
    is_alpha(c) || is_digit(c) || c == b'_' as u16
}

#[inline]
pub fn is_alnum(c: u16) -> bool {
    is_alpha(c) || is_digit(c)
}

#[inline]
pub fn is_hex(c: u16) -> bool {
    is_digit(c) || (b'a' as u16..=b'f' as u16).contains(&c) || (b'A' as u16..=b'F' as u16).contains(&c)
}

pub fn trim(s: &[u16]) -> &[u16] {
    trim_end(trim_start(s))
}

pub fn trim_start(s: &[u16]) -> &[u16] {
    let start = s.iter().position(|&c| !is_space(c)).unwrap_or(s.len());
    &s[start..]
}

pub fn trim_end(s: &[u16]) -> &[u16] {
    let end = s.iter().rposition(|&c| !is_space(c)).map_or(0, |i| i + 1);
    &s[..end]
}

/// `s.replace(/^[ \t\n]+|[ \t\n]+$/g, "")`
pub fn trim_blanks(s: &[u16]) -> &[u16] {
    let blank = |c: &u16| matches!(*c, 0x20 | 0x09 | 0x0a);
    let start = s.iter().position(|c| !blank(c)).unwrap_or(s.len());
    let end = s.iter().rposition(|c| !blank(c)).map_or(0, |i| i + 1);
    if start >= end { &[] } else { &s[start..end] }
}

pub fn eq_ascii(s: &[u16], ascii: &str) -> bool {
    s.len() == ascii.len() && ascii.bytes().zip(s).all(|(b, &c)| c == b as u16)
}

/// ASCII case-insensitive comparison, as a regex `i` flag without `u` reads ASCII letters.
pub fn eq_ignore_ascii_case(c: u16, ascii: u8) -> bool {
    c < 128 && (c as u8).eq_ignore_ascii_case(&ascii)
}

/// Case-insensitive comparison under the `iu` flags: simple case folding, where only U+017F folds to `s` and
/// U+212A to `k` among ASCII letters.
pub fn eq_fold(c: u16, ascii: u8) -> bool {
    eq_ignore_ascii_case(c, ascii)
        || (ascii.eq_ignore_ascii_case(&b's') && c == 0x017f)
        || (ascii.eq_ignore_ascii_case(&b'k') && c == 0x212a)
}

/// `s.startsWith(word)` compared case-insensitively (ASCII, the non-unicode `i` flag).
pub fn starts_with_ignore_case(s: &[u16], pos: isize, word: &str) -> bool {
    pos >= 0
        && (pos as usize) + word.len() <= s.len()
        && word.bytes().enumerate().all(|(i, b)| eq_ignore_ascii_case(s[pos as usize + i], b))
}

/// The same under the `iu` flags (simple case folding).
pub fn starts_with_fold(s: &[u16], pos: isize, word: &str) -> bool {
    pos >= 0
        && (pos as usize) + word.len() <= s.len()
        && word.bytes().enumerate().all(|(i, b)| eq_fold(s[pos as usize + i], b))
}

/// The code point starting at `i` (a regex with the `u` flag), and its length in units. A lone surrogate is its own
/// code point.
pub fn code_point(s: &[u16], i: isize) -> Option<(u32, isize)> {
    let c = at(s, i)? as u32;
    if (0xd800..0xdc00).contains(&c)
        && let Some(low) = at(s, i + 1).map(u32::from)
        && (0xdc00..0xe000).contains(&low)
    {
        return Some((0x10000 + ((c - 0xd800) << 10) + (low - 0xdc00), 2));
    }
    Some((c, 1))
}

/// Where a sticky `u`-flag regex starting at `i` begins matching: JavaScriptCore starts at the code point that holds
/// unit `i`, which is the one before it when `i` is the low half of a surrogate pair.
pub fn sticky_unicode_start(s: &[u16], i: isize) -> isize {
    if i > 0
        && at(s, i).is_some_and(|c| (0xdc00..0xe000).contains(&c))
        && at(s, i - 1).is_some_and(|c| (0xd800..0xdc00).contains(&c))
    {
        i - 1
    } else {
        i
    }
}

/// Whether a `u`-flag class test of a short string (`/[...]/u.test(text)`) finds a code point satisfying `class`.
pub fn any_code_point(s: &[u16], class: impl Fn(u32) -> bool) -> bool {
    let mut i = 0;
    while let Some((c, n)) = code_point(s, i) {
        if class(c) {
            return true;
        }
        i += n;
    }
    false
}

/// `[\p{L}\p{Nl}]` with the `u` flag.
#[inline]
pub fn is_letter(c: u32) -> bool {
    unicode::is_letter_or_letter_number(c)
}

/// `[\p{L}\p{Nl}]` with the `iu` flags: also U+0345, which folds to a Greek letter.
#[inline]
pub fn is_letter_fold(c: u32) -> bool {
    c == 0x345 || unicode::is_letter_or_letter_number(c)
}

/// `String.prototype.toLowerCase`, keeping lone surrogates.
pub fn to_lower(s: &[u16]) -> JsString {
    if s.iter().all(|&c| c < 128) {
        return s.iter().map(|&c| (c as u8).to_ascii_lowercase() as u16).collect();
    }
    if let Ok(text) = String::from_utf16(s) {
        return text.to_lowercase().encode_utf16().collect();
    }
    let mut out = JsString::new();
    for item in char::decode_utf16(s.iter().copied()) {
        match item {
            Ok(c) => {
                let mut buf = [0u16; 2];
                for l in c.to_lowercase() {
                    out.extend_from_slice(l.encode_utf16(&mut buf));
                }
            }
            Err(e) => out.push(e.unpaired_surrogate()),
        }
    }
    out
}

/// `String.fromCodePoint(point)`.
pub fn push_code_point(out: &mut JsString, point: u32) {
    if point >= 0x10000 {
        let p = point - 0x10000;
        out.push(0xd800 + (p >> 10) as u16);
        out.push(0xdc00 + (p & 0x3ff) as u16);
    } else {
        out.push(point as u16);
    }
}

pub fn concat(parts: &[&[u16]]) -> JsString {
    let mut out = JsString::with_capacity(parts.iter().map(|p| p.len()).sum());
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

/// `values.join(separator)`.
pub fn join(values: &[JsString], separator: &[u16]) -> JsString {
    let mut out = JsString::new();
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(separator);
        }
        out.extend_from_slice(value);
    }
    out
}

/// `s.replaceAll(from, to)` for single units.
pub fn replace_unit(s: &[u16], from: u16, to: &[u16]) -> JsString {
    let mut out = JsString::with_capacity(s.len());
    for &c in s {
        if c == from { out.extend_from_slice(to) } else { out.push(c) }
    }
    out
}

/// `s.replaceAll(from, to)` for a pattern of several units.
pub fn replace_seq(s: &[u16], from: &[u16], to: &[u16]) -> JsString {
    let mut out = JsString::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if !from.is_empty() && s[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// `JSON.stringify(s)` for a string: lone surrogates are escaped (well-formed `JSON.stringify`).
pub fn json_string(s: &[u16], out: &mut String) {
    out.push('"');
    for item in char::decode_utf16(s.iter().copied()) {
        match item {
            Ok(c) => match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            },
            Err(e) => out.push_str(&format!("\\u{:04x}", e.unpaired_surrogate())),
        }
    }
    out.push('"');
}
