//! The JavaScript semantics the ported TS relies on: string lengths in UTF-16 units, `parseInt`, `ToInt32`,
//! `String.fromCodePoint`/`fromCharCode`, and the two decoders the parser uses (`TextDecoder` and latin1).
//!
//! A JS string can hold a lone surrogate (`String.fromCodePoint(0xd800)`); a Rust `String` cannot, so one becomes
//! U+FFFD, as `TextEncoder` would write it. Only Kitty and modifyOtherKeys sequences naming a surrogate code point
//! make one.

/// ECMAScript `StrWhiteSpaceChar`: WhiteSpace and LineTerminator.
pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// A string's JS `length`.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// `parseInt(text, 10)`: optional white space and sign, then the longest run of ASCII digits; NaN without one.
pub fn parse_int(text: &str) -> f64 {
    let text = text.trim_start_matches(is_js_space);
    let (sign, rest) = match text.as_bytes().first() {
        Some(b'-') => (-1.0, &text[1..]),
        Some(b'+') => (1.0, &text[1..]),
        _ => (1.0, text),
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return f64::NAN;
    }
    // JSC rounds a long radix-10 run as a decimal literal: correctly.
    sign * rest[..digits].parse::<f64>().unwrap_or(f64::NAN)
}

/// `ToInt32`, what the bitwise operators apply to a number.
pub fn to_int32(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    let truncated = value.trunc();
    let modulo = truncated.rem_euclid(4294967296.0);
    modulo as u32 as i32
}

/// `String.fromCodePoint(codePoint)` for 0..=0x10FFFF (a surrogate becomes U+FFFD, see the module docs).
pub fn from_code_point(code_point: u32) -> char {
    char::from_u32(code_point).unwrap_or('\u{FFFD}')
}

/// `String.fromCharCode(value)`: the UTF-16 unit `ToUint16(value)`.
pub fn from_char_code(value: f64) -> char {
    let unit = to_int32(value) as u32 & 0xffff;
    char::from_u32(unit).unwrap_or('\u{FFFD}')
}

/// `new TextDecoder().decode(bytes)`: UTF-8 with replacement characters, and a leading BOM dropped.
pub fn decode_utf8(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    match text.strip_prefix('\u{FEFF}') {
        Some(rest) => rest.to_owned(),
        None => text.into_owned(),
    }
}

/// `Buffer.toString("latin1")`: one code point per byte.
pub fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| byte as char).collect()
}

/// `text.toLocaleUpperCase()` (the full Unicode mapping; the reference runs in a locale without special casing).
pub fn to_upper(text: &str) -> String {
    text.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_matches_js() {
        assert_eq!(parse_int("97"), 97.0);
        assert_eq!(parse_int(" +12x"), 12.0);
        assert!(parse_int("-0").is_sign_negative());
        assert!(parse_int("").is_nan());
        assert!(parse_int("x1").is_nan());
        assert_eq!(parse_int("0x10"), 0.0);
        assert_eq!(parse_int("99999999999999999999999"), 1e23);
    }

    #[test]
    fn to_int32_wraps() {
        assert_eq!(to_int32(4294967297.0), 1);
        assert_eq!(to_int32(-1.0), -1);
        assert_eq!(to_int32(2147483648.0), i32::MIN);
        assert_eq!(to_int32(f64::NAN), 0);
        assert_eq!(to_int32(1e23), to_int32(1e23 % 4294967296.0));
    }

    #[test]
    fn decoders() {
        assert_eq!(decode_utf8(b"\xef\xbb\xbfa"), "a");
        assert_eq!(decode_utf8(b"a\xef\xbb\xbf"), "a\u{feff}");
        assert_eq!(decode_utf8(b"\xed\xa0\x80"), "\u{fffd}\u{fffd}\u{fffd}");
        assert_eq!(decode_latin1(b"\x1b\xff"), "\u{1b}\u{ff}");
    }
}
