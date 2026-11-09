//! String helpers with JavaScript's semantics, for code ported from TS that depends on them.

/// `WhiteSpace` and `LineTerminator` of ECMAScript (what `String.prototype.trim` and `\s` match).
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}'
            | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// `String.prototype.trim`.
pub fn trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean)`.
pub fn lines(text: &str) -> Vec<String> {
    text.split('\n').map(|line| trim(line.strip_suffix('\r').unwrap_or(line))).filter(|l| !l.is_empty()).map(str::to_owned).collect()
}

/// `text.split("\0").filter(Boolean)`.
pub fn nuls(text: &str) -> Vec<&str> {
    text.split('\0').filter(|item| !item.is_empty()).collect()
}

/// `Number.parseInt(value, 10)` for the decimal numbers git prints; `NaN` is `None`.
pub fn parse_int(value: &str) -> Option<i64> {
    let value = trim(value);
    let (sign, digits) = match value.as_bytes().first() {
        Some(b'-') => (-1, &value[1..]),
        Some(b'+') => (1, &value[1..]),
        _ => (1, value),
    };
    let end = digits.bytes().position(|b| !b.is_ascii_digit()).unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    digits[..end].parse::<i64>().ok().map(|n| sign * n)
}

/// `path.relative(from, to)` for normalized absolute POSIX paths.
pub fn relative(from: &str, to: &str) -> String {
    let split = |p: &str| p.split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect::<Vec<_>>();
    let (from, to) = (split(from), split(to));
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_owned(), from.len() - common).collect();
    parts.extend(to[common..].iter().cloned());
    parts.join("/")
}

/// `path.extname`: the extension of the last segment, with its dot (`""` for none or a leading dot only).
pub fn extname(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rfind('.') {
        Some(0) | None => "",
        Some(index) => &base[index..],
    }
}

/// `path.basename`.
pub fn basename(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

/// `encodeURIComponent`.
pub fn encode_uri_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `decodeURIComponent`, failing (`URIError`) on malformed escapes or UTF-8.
pub fn decode_uri_component(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok().filter(|_| hex.bytes().all(|b| b.is_ascii_hexdigit()))?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(relative("/a/b", "/a/b/c/d"), "c/d");
        assert_eq!(relative("/a/b", "/a"), "..");
        assert_eq!(relative("/a/b", "/a/b"), "");
        assert_eq!(relative("/a/b/sub", "/a/b/x"), "../x");
        assert_eq!(extname("dir.x/file.tar.gz"), ".gz");
        assert_eq!(extname(".env"), "");
    }

    #[test]
    fn uri() {
        assert_eq!(encode_uri_component("a b/ü(x)"), "a%20b%2F%C3%BC(x)");
        assert_eq!(decode_uri_component("..%2Fetc").as_deref(), Some("../etc"));
        assert_eq!(decode_uri_component("%E0%A4%A"), None);
        assert_eq!(decode_uri_component("%zz"), None);
        assert_eq!(parse_int("12abc"), Some(12));
        assert_eq!(parse_int("-"), None);
    }
}
