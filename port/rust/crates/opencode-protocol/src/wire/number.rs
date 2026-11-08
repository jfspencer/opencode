//! JavaScript number semantics: `Number.prototype.toString()` (which `JSON.stringify` and `String(n)` use) and
//! `Number(string)`.

/// The largest integer `n` such that `n` and `n + 1` are both exactly representable (`Number.MAX_SAFE_INTEGER`).
pub const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `Number.isSafeInteger(n)`.
pub fn is_safe_integer(n: f64) -> bool {
    n.is_finite() && n.trunc() == n && n.abs() <= MAX_SAFE_INTEGER
}

/// Appends `String(n)`: the shortest digits that round-trip, in JS layout (`1`, `0.000001`, `1e-7`, `1e+21`).
/// `-0` prints as `0`; non-finite values print as `NaN`, `Infinity`, `-Infinity`.
pub fn write_js_number(out: &mut String, n: f64) {
    if n.is_nan() {
        return out.push_str("NaN");
    }
    if n == 0.0 {
        return out.push('0');
    }
    if n.is_infinite() {
        return out.push_str(if n > 0.0 { "Infinity" } else { "-Infinity" });
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
        out.push(if n - 1 >= 0 { '+' } else { '-' });
        out.push_str(&(n - 1).abs().to_string());
    }
}

/// The shortest digits `d1..dk` that round-trip and the decimal point position `n`, such that the value is
/// `0.d1..dk * 10^n`. Among several shortest candidates JS takes the closest, and on an exact tie the even one
/// (Number::toString step 5); Ryu does the same, where Rust's `{:e}` rounds ties up.
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
    // point after `int` digits, shifted by the exponent, minus the zeros trimmed in front
    (digits, int.len() as i32 + exp - leading as i32)
}

pub fn js_number_string(n: f64) -> String {
    let mut s = String::new();
    write_js_number(&mut s, n);
    s
}

/// JS `StrWhiteSpaceChar`: WhiteSpace and LineTerminator, as stripped by `String.prototype.trim` and `Number()`.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// `String.prototype.trim`.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// `Number(s)` for a string `s` (ECMAScript StringToNumber).
pub fn js_string_to_number(s: &str) -> f64 {
    let s = js_trim(s);
    if s.is_empty() {
        return 0.0;
    }
    let b = s.as_bytes();
    if b.len() > 2 && b[0] == b'0' {
        let radix = match b[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if radix != 0 {
            return non_decimal(&s[2..], radix);
        }
    }
    let (sign, body) = match b[0] {
        b'+' => (1.0, &s[1..]),
        b'-' => (-1.0, &s[1..]),
        _ => (1.0, s),
    };
    if body == "Infinity" {
        return sign * f64::INFINITY;
    }
    if !is_decimal_literal(body) {
        return f64::NAN;
    }
    sign * body.parse::<f64>().unwrap_or(f64::NAN)
}

// StrUnsignedDecimalLiteral: digits [. digits?] | . digits, then an optional exponent.
fn is_decimal_literal(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    let int = count_digits(b, &mut i);
    let mut frac = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        frac = count_digits(b, &mut i);
    }
    if int == 0 && frac == 0 {
        return false;
    }
    if let Some(b'e' | b'E') = b.get(i) {
        i += 1;
        if let Some(b'+' | b'-') = b.get(i) {
            i += 1;
        }
        if count_digits(b, &mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

fn count_digits(b: &[u8], i: &mut usize) -> usize {
    let start = *i;
    while let Some(b'0'..=b'9') = b.get(*i) {
        *i += 1;
    }
    *i - start
}

// Hex/octal/binary integer literal. Exact while it fits in a u128; beyond that the value is accumulated in f64,
// which can differ from JS's correctly rounded result in the last place.
fn non_decimal(digits: &str, radix: u32) -> f64 {
    let mut exact: Option<u128> = Some(0);
    let mut approx = 0f64;
    for c in digits.chars() {
        let Some(d) = c.to_digit(radix) else { return f64::NAN };
        exact = exact.and_then(|v| v.checked_mul(radix as u128)).and_then(|v| v.checked_add(d as u128));
        approx = approx * radix as f64 + d as f64;
    }
    match exact {
        Some(v) => v as f64,
        None => approx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_js() {
        for (n, s) in [
            (1.0, "1"),
            (-0.0, "0"),
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (-2.5e300, "-2.5e+300"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (9007199254740993.0, "9007199254740992"),
            // exact ties between two shortest candidates go to the even digit
            (25640097834275.8125, "25640097834275.812"),
            (-1744036447266197.25, "-1744036447266197.2"),
            (0.000001234, "0.000001234"),
            (100.0, "100"),
            (1.5, "1.5"),
        ] {
            assert_eq!(js_number_string(n), s, "{n}");
        }
    }

    #[test]
    fn parses_like_js_number() {
        for (s, n) in [("", 0.0), ("  12 ", 12.0), ("0x10", 16.0), ("1e3", 1000.0), (".5", 0.5), ("5.", 5.0), ("-Infinity", f64::NEG_INFINITY)] {
            assert_eq!(js_string_to_number(s), n, "{s:?}");
        }
        for s in ["abc", "1_000", "0x", "-0x10", "inf", "NaN", "1e", "+-1", "."] {
            assert!(js_string_to_number(s).is_nan(), "{s:?}");
        }
    }
}
