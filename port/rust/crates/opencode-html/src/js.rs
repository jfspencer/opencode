//! JavaScript string semantics over UTF-8 byte strings.
//!
//! The TS converters work on JS strings (UTF-16). The patched reference never makes a lone surrogate from a valid
//! string: htmlparser2 emits an astral entity as one code point and html-markdown.ts writes its input in chunks
//! that end between characters (D113, D114). A JS string is therefore kept as its UTF-8 bytes, whose length is what
//! `TextEncoder#encode(s).byteLength` gives in JS, so byte budgets are counted on the bytes directly.

/// A JS string, as UTF-8 bytes.
pub(crate) type JsString = Vec<u8>;

/// JS `WhiteSpace` and `LineTerminator`: what `String#trim`, `parseInt` and the regular expression `\s` match.
pub(crate) fn is_js_whitespace(cp: u32) -> bool {
    matches!(
        cp,
        0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

fn is_continuation(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

/// The code point that starts at `i`, and its length in bytes.
pub(crate) fn decode_at(s: &[u8], i: usize) -> (u32, usize) {
    let b = s[i];
    if b < 0x80 {
        return (b as u32, 1);
    }
    if b < 0xe0 {
        return ((((b & 0x1f) as u32) << 6) | (s[i + 1] & 0x3f) as u32, 2);
    }
    if b < 0xf0 {
        let cp = (((b & 0x0f) as u32) << 12) | (((s[i + 1] & 0x3f) as u32) << 6) | (s[i + 2] & 0x3f) as u32;
        return (cp, 3);
    }
    let cp = (((b & 0x07) as u32) << 18)
        | (((s[i + 1] & 0x3f) as u32) << 12)
        | (((s[i + 2] & 0x3f) as u32) << 6)
        | (s[i + 3] & 0x3f) as u32;
    (cp, 4)
}

/// `dst += src`.
pub(crate) fn push(dst: &mut JsString, src: &[u8]) {
    dst.extend_from_slice(src);
}

/// `dst += String.fromCodePoint(cp)`, for a code point that is not a surrogate (the tokenizer's entities never
/// are: `replaceCodePoint` turns a numeric one into U+FFFD).
pub(crate) fn push_code_point(dst: &mut JsString, cp: u32) {
    let c = char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER);
    dst.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
}

/// The string as a Rust `String`.
pub(crate) fn into_string(s: JsString) -> String {
    match String::from_utf8(s) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }
}

/// The longest prefix of whole code points that encodes in at most `bytes` bytes:
/// html-markdown.ts `sliceBytes`.
pub(crate) fn slice_bytes(s: &[u8], bytes: i64) -> &[u8] {
    if (s.len() as i64) <= bytes {
        return s;
    }
    if bytes <= 0 {
        return &s[..0];
    }
    let mut end = bytes as usize;
    while end > 0 && is_continuation(s[end]) {
        end -= 1;
    }
    &s[..end]
}

/// `String#trim`.
pub(crate) fn trim(s: &[u8]) -> &[u8] {
    trim_end(trim_start(s))
}

/// `String#trimStart`.
pub(crate) fn trim_start(s: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < s.len() {
        let (cp, len) = decode_at(s, i);
        if !is_js_whitespace(cp) {
            break;
        }
        i += len;
    }
    &s[i..]
}

/// `String#trimEnd`.
pub(crate) fn trim_end(s: &[u8]) -> &[u8] {
    let mut end = s.len();
    while end > 0 {
        let mut start = end - 1;
        while start > 0 && is_continuation(s[start]) {
            start -= 1;
        }
        let (cp, _) = decode_at(s, start);
        if !is_js_whitespace(cp) {
            break;
        }
        end = start;
    }
    &s[..end]
}

/// `String#toLowerCase` for a string without lone surrogates.
pub(crate) fn to_lower_case(s: &str) -> String {
    if s.is_ascii() { s.to_ascii_lowercase() } else { s.to_lowercase() }
}

const MANTISSA_OVERFLOW_LOWER_BOUND: f64 = 9_007_199_254_740_992.0; // 2^53

fn digit_value(c: u8, radix: u32) -> Option<u32> {
    let d = match c {
        b'0'..=b'9' => (c - b'0') as u32,
        b'a'..=b'z' => (c - b'a') as u32 + 10,
        b'A'..=b'Z' => (c - b'A') as u32 + 10,
        _ => return None,
    };
    (d < radix).then_some(d)
}

/// `Number.parseInt(s)` (no radix) as JavaScriptCore computes it: leading JS whitespace, a sign, a `0x` prefix, then
/// digits accumulated in a double; from 2^53 up, decimal digits are parsed again with correct rounding and hex
/// digits summed from the least significant one (JSC `parseIntOverflow`).
pub(crate) fn parse_int(s: &str) -> f64 {
    let s = s.trim_start_matches(|c: char| is_js_whitespace(c as u32)).as_bytes();
    let mut p = 0;
    let mut sign = 1.0;
    if p < s.len() {
        if s[p] == b'+' {
            p += 1;
        } else if s[p] == b'-' {
            sign = -1.0;
            p += 1;
        }
    }
    let mut radix = 10;
    if s.len() - p >= 2 && s[p] == b'0' && (s[p + 1] == b'x' || s[p + 1] == b'X') {
        radix = 16;
        p += 2;
    }
    let first = p;
    let mut number = 0.0f64;
    while p < s.len() {
        let Some(d) = digit_value(s[p], radix) else { break };
        number = number * radix as f64 + d as f64;
        p += 1;
    }
    if p == first {
        return f64::NAN;
    }
    if number >= MANTISSA_OVERFLOW_LOWER_BOUND {
        let digits = &s[first..p];
        number = if radix == 10 {
            std::str::from_utf8(digits).expect("ASCII digits").parse::<f64>().expect("decimal digits")
        } else {
            let mut number = 0.0f64;
            let mut multiplier = 1.0f64;
            for &c in digits.iter().rev() {
                if multiplier == f64::INFINITY {
                    if c != b'0' {
                        number = f64::INFINITY;
                        break;
                    }
                } else {
                    number += digit_value(c, 16).expect("hex digit") as f64 * multiplier;
                }
                multiplier *= 16.0;
            }
            number
        };
    }
    sign * number
}

/// `String(n)`: the shortest digits that round-trip, in JS layout (`1`, `0.000001`, `1e-7`, `1e+21`). `-0` prints as
/// `0`; non-finite values print as `NaN`, `Infinity`, `-Infinity`.
pub(crate) fn number_to_string(n: f64) -> String {
    let mut out = String::new();
    if n.is_nan() {
        return "NaN".into();
    }
    if n == 0.0 {
        return "0".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if n < 0.0 {
        out.push('-');
    }
    let (digits, n) = shortest_digits(n.abs());
    let k = digits.len() as i32;
    if k <= n && n <= 21 {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-n) as usize));
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if n > 0 { '+' } else { '-' });
        out.push_str(&(n - 1).abs().to_string());
    }
    out
}

/// The shortest round-trip digits `d1..dk` and the point position `n` (value = `0.d1..dk * 10^n`). Ryu picks the
/// closest candidate and the even one on a tie, as JS Number::toString does.
fn shortest_digits(n: f64) -> (String, i32) {
    let mut buf = ryu::Buffer::new();
    let text = buf.format_finite(n);
    let (mantissa, exp) = match text.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().expect("exponent digits")),
        None => (text, 0),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let all = format!("{int}{frac}");
    let leading = all.len() - all.trim_start_matches('0').len();
    let digits = all.trim_matches('0').to_owned();
    (digits, int.len() as i32 + exp - leading as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_code_points() {
        let mut s = Vec::new();
        push_code_point(&mut s, 0x1d504);
        push_code_point(&mut s, 0xe9);
        assert_eq!(into_string(s), "\u{1d504}é");
    }

    #[test]
    fn numbers() {
        assert_eq!(number_to_string(1e21), "1e+21");
        assert_eq!(number_to_string(-0.0), "0");
        assert_eq!(number_to_string(16.0), "16");
        assert_eq!(parse_int(" 0x10"), 16.0);
        assert!(parse_int("x").is_nan());
        assert_eq!(parse_int("99999999999999999999999"), 1e23);
    }

    #[test]
    fn slices_whole_code_points() {
        assert_eq!(slice_bytes("😀é".as_bytes(), 5), "😀".as_bytes());
        assert_eq!(slice_bytes("😀é".as_bytes(), 3), b"");
        assert_eq!(trim("\u{a0} x\u{feff}".as_bytes()), b"x");
    }
}
