//! The JS built-ins form answers are checked with: `new RegExp(pattern)` (no flags), the email pattern,
//! `URL.canParse`, and `new Date(...)` validity.
//!
//! TODO(port): approximations of the JS engine. Patterns are translated to Rust `regex` syntax; constructs it lacks
//! (lookaround, backreferences) are reported as invalid patterns, and matching is by code point where JS matches
//! UTF-16 units. `URL.canParse` follows the WHATWG parser's structure (scheme, authority, host and port checks)
//! without IDNA. Dates accept the ECMAScript date-time format and the common forms of JavaScriptCore's legacy
//! parser (month names, `m/d/y`, times, AM/PM, time zones), not all of them.

use regex::Regex;

/// JS `\s` (WhiteSpace and LineTerminator), as a class body.
const JS_SPACE: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// Whether a `{` at `i` starts a JS quantifier (`{n}`, `{n,}`, `{n,m}`); elsewhere it is a literal brace.
fn quantifier_at(chars: &[char], i: usize) -> bool {
    let mut j = i + 1;
    let digits = |j: &mut usize| {
        let start = *j;
        while *j < chars.len() && chars[*j].is_ascii_digit() {
            *j += 1;
        }
        *j > start
    };
    if !digits(&mut j) {
        return false;
    }
    if chars.get(j) == Some(&',') {
        j += 1;
        digits(&mut j);
    }
    chars.get(j) == Some(&'}')
}

/// `new RegExp(source)`: the pattern translated to Rust syntax, `None` when JS would reject it or Rust cannot run it.
pub fn js_regex(source: &str) -> Option<Regex> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::new();
    let mut in_class = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            i += 1;
            let n = *chars.get(i)?;
            match n {
                'd' => out.push_str(if in_class { "0-9" } else { "[0-9]" }),
                'w' => out.push_str(if in_class { "A-Za-z0-9_" } else { "[A-Za-z0-9_]" }),
                's' if in_class => out.push_str(JS_SPACE),
                's' => out.push_str(&format!("[{JS_SPACE}]")),
                'D' | 'W' | 'S' if in_class => return None,
                'D' => out.push_str("[^0-9]"),
                'W' => out.push_str("[^A-Za-z0-9_]"),
                'S' => out.push_str(&format!("[^{JS_SPACE}]")),
                'b' if in_class => out.push_str(r"\x08"),
                'b' | 'B' => out.push_str(&format!("(?-u:\\{n})")),
                '1'..='9' => return None,
                '0' => out.push_str(r"\x00"),
                't' | 'n' | 'r' | 'v' | 'f' => out.push_str(&format!("\\{n}")),
                'u' if chars.len() > i + 4 && chars[i + 1..i + 5].iter().all(char::is_ascii_hexdigit) => {
                    out.push_str(&format!("\\x{{{}}}", chars[i + 1..i + 5].iter().collect::<String>()));
                    i += 4;
                }
                'x' if chars.len() > i + 2 && chars[i + 1..i + 3].iter().all(char::is_ascii_hexdigit) => {
                    out.push_str(&format!("\\x{}", chars[i + 1..i + 3].iter().collect::<String>()));
                    i += 2;
                }
                'c' if chars.get(i + 1).is_some_and(char::is_ascii_alphabetic) => {
                    out.push_str(&format!("\\x{:02X}", chars[i + 1] as u32 % 32));
                    i += 1;
                }
                // Identity escapes: the character itself.
                other => out.push_str(&regex::escape(&other.to_string())),
            }
            i += 1;
            continue;
        }
        if in_class {
            match c {
                ']' => {
                    in_class = false;
                    out.push(']');
                }
                '[' | '&' | '~' => out.push_str(&format!("\\{c}")),
                c => out.push(c),
            }
            i += 1;
            continue;
        }
        match c {
            '[' => {
                if chars.get(i + 1) == Some(&'^') && chars.get(i + 2) == Some(&']') {
                    out.push_str("(?s:.)");
                    i += 3;
                    continue;
                }
                if chars.get(i + 1) == Some(&']') {
                    // `[]` matches nothing.
                    out.push_str(r"[^\x00-\x{10FFFF}]");
                    i += 2;
                    continue;
                }
                in_class = true;
                out.push('[');
                if chars.get(i + 1) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                // A leading `]` cannot follow; a leading `-` is literal in both.
            }
            '(' if chars.get(i + 1) == Some(&'?') => match chars.get(i + 2) {
                Some(':') => out.push('('),
                Some('<') if !matches!(chars.get(i + 3), Some('=' | '!')) => out.push('('),
                _ => return None,
            },
            '.' => out.push_str(r"[^\n\r\x{2028}\x{2029}]"),
            '{' if quantifier_at(&chars, i) => {
                while chars[i] != '}' {
                    out.push(chars[i]);
                    i += 1;
                }
                out.push('}');
            }
            '{' => out.push_str(r"\{"),
            '}' => out.push_str(r"\}"),
            c => out.push(c),
        }
        i += 1;
    }
    if in_class {
        return None;
    }
    Regex::new(&out).ok()
}

/// The form's email check: `/^[^\s@]+@[^\s@]+\.[^\s@]+$/`.
pub fn email(text: &str) -> bool {
    static EMAIL: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        let part = format!("[^{JS_SPACE}@]+");
        Regex::new(&format!("^{part}@{part}\\.{part}$")).expect("email pattern")
    });
    EMAIL.is_match(text)
}

const SPECIAL: &[&str] = &["ftp", "file", "http", "https", "ws", "wss"];

/// WHATWG's forbidden host code points (plus `%` for domains, which must be valid after percent-decoding).
fn forbidden_host(c: char, domain: bool) -> bool {
    matches!(c, '\0' | '\t' | '\n' | '\r' | ' ' | '#' | '/' | ':' | '<' | '>' | '?' | '@' | '[' | '\\' | ']' | '^' | '|')
        || (domain && (c <= '\x1f' || c == '%' || c == '\x7f'))
}

fn valid_ipv6(text: &str) -> bool {
    let body = &text[1..text.len() - 1];
    !body.is_empty()
        && body.contains(':')
        && body.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
        && body.matches("::").count() <= 1
}

/// WHATWG IPv4 number parsing (decimal, `0x` hex, leading-zero octal).
fn ipv4_number(part: &str) -> Option<u64> {
    let (digits, radix) = if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
        (hex, 16)
    } else if part.len() > 1 && part.starts_with('0') {
        (&part[1..], 8)
    } else {
        (part, 10)
    };
    if digits.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(digits, radix).ok()
}

/// A domain whose last label is a number must be a valid IPv4 address.
fn valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.chars().any(|c| forbidden_host(c, true)) {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    let last = match labels.as_slice() {
        [.., last, ""] if labels.len() > 1 => *last,
        [.., last] => *last,
        [] => "",
    };
    let numeric = !last.is_empty()
        && (last.chars().all(|c| c.is_ascii_digit())
            || (last.len() >= 2 && last[..2].eq_ignore_ascii_case("0x") && last[2..].chars().all(|c| c.is_ascii_hexdigit())));
    if !numeric {
        return true;
    }
    let parts: Vec<&str> = if domain.ends_with('.') { labels[..labels.len() - 1].to_vec() } else { labels };
    if parts.len() > 4 || parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    let Some(numbers) = parts.iter().map(|p| ipv4_number(p)).collect::<Option<Vec<u64>>>() else { return false };
    let (last, rest) = numbers.split_last().expect("one part");
    rest.iter().all(|n| *n <= 255) && *last < 256u64.pow(5 - numbers.len() as u32)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(byte) = text.get(i + 1..i + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An authority (`[userinfo@]host[:port]`); `special` hosts are domains or IP addresses and must not be empty.
fn valid_authority(authority: &str, special: bool) -> bool {
    let host_port = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    let (host, port) = if host_port.starts_with('[') {
        let Some(end) = host_port.find(']') else { return false };
        match &host_port[end + 1..] {
            "" => (&host_port[..=end], None),
            after => match after.strip_prefix(':') {
                Some(port) => (&host_port[..=end], Some(port)),
                None => return false,
            },
        }
    } else {
        match host_port.rfind(':') {
            Some(colon) => (&host_port[..colon], Some(&host_port[colon + 1..])),
            None => (host_port, None),
        }
    };
    if let Some(port) = port {
        let significant = port.trim_start_matches('0');
        if !port.chars().all(|c| c.is_ascii_digit()) || significant.len() > 5 || significant.parse::<u32>().unwrap_or(0) > 65535 {
            return false;
        }
    }
    if host.starts_with('[') {
        return host.ends_with(']') && valid_ipv6(host);
    }
    if host.is_empty() {
        return !special && authority.rfind('@').is_none() && port.is_none();
    }
    if special {
        valid_domain(&percent_decode(host).to_lowercase())
    } else {
        !host.chars().any(|c| forbidden_host(c, false))
    }
}

/// `URL.canParse(text)` (no base).
pub fn url_can_parse(text: &str) -> bool {
    let trimmed = text.trim_matches(|c: char| c <= ' ');
    let input: String = trimmed.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let Some(colon) = input.find(':') else { return false };
    let scheme = &input[..colon];
    let mut scheme_chars = scheme.chars();
    if !scheme_chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !scheme_chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return false;
    }
    let scheme = scheme.to_ascii_lowercase();
    let rest = &input[colon + 1..];
    let end_of_authority = |s: &str, backslash: bool| {
        s.find(|c: char| c == '/' || c == '?' || c == '#' || (backslash && c == '\\')).unwrap_or(s.len())
    };
    if scheme == "file" {
        let slashes = rest.chars().take_while(|c| *c == '/' || *c == '\\').count();
        if slashes < 2 {
            return true;
        }
        let after = &rest[slashes.min(2)..];
        let host = &after[..end_of_authority(after, true)];
        // A Windows drive letter is a path; any other host must be a valid domain.
        let drive = host.len() == 2 && host.as_bytes()[0].is_ascii_alphabetic() && matches!(host.as_bytes()[1], b':' | b'|');
        return host.is_empty() || drive || valid_domain(&percent_decode(host).to_lowercase());
    }
    if SPECIAL.contains(&scheme.as_str()) {
        let after = rest.trim_start_matches(['/', '\\']);
        let authority = &after[..end_of_authority(after, true)];
        return valid_authority(authority, true);
    }
    match rest.strip_prefix("//") {
        Some(after) => valid_authority(&after[..end_of_authority(after, false)], false),
        None => true,
    }
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The form's date check: `YYYY-MM-DD` naming a real day (`new Date(value + "T00:00:00.000Z")` round-trips).
pub fn date(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        bytes[range.clone()].iter().all(u8::is_ascii_digit).then(|| text[range].parse().ok()).flatten()
    };
    let (Some(year), Some(month), Some(day)) = (digits(0..4), digits(5..7), digits(8..10)) else { return false };
    (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year as i64, month)
}

/// The form's date-time check: `!Number.isNaN(new Date(value).getTime())`.
pub fn date_time(text: &str) -> bool {
    super::date::parse(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(js_regex("(").is_none());
        assert!(js_regex("^[a-z]+$").unwrap().is_match("alice"));
        assert!(!js_regex("^[a-z]+$").unwrap().is_match("Alice"));
        assert!(js_regex(r"^\d{2}$").unwrap().is_match("12"));
        assert!(js_regex("a{").unwrap().is_match("a{"));
        assert!(js_regex("(?=x)").is_none());
    }

    #[test]
    fn emails() {
        assert!(email("alice@example.test"));
        assert!(!email("not-an-email"));
        assert!(!email("a b@c.d"));
    }

    #[test]
    fn urls() {
        // Recorded from the reference's engine (`URL.canParse`).
        let valid = ["https://example.test", "mailto:a@b.c", "file:///etc", "foo://", "http://[::1]:80/x", "http://a:", "http://a:0000080", "x:", "http:a", "http:/a", "https://user:pw@host/p", "https://@host", "ws://h", "a+b-c.d:x", "http://ex%41mple.com", "http://0x7f.1", "http://1.2.3", "http://4294967295", "javascript:alert(1)", " https://x ", "http://[1:2::3]", "file://host/path", "file://C:/x", "file:x", "data:text/plain,hi", "urn:isbn:123", "http://a.b.c.", "http://-a.b", "http://a_b", "http://a`b", "https://例え.jp", "HTTP://X", "http:\\\\x\\y", "foo://x:1234", "http://a?b", "http://a#b", "sc:\\x", "c:\\x\\y"];
        let invalid = ["not a uri", "https://", "http://exa mple.com", "http://a:99999", "http://1.2.3.256", "1http://a", "https://host@", "http://a%20b", "http://a b", "http://1.2.3.4.5", "http://999999999999", "http://4294967296", "http://[zz]", "foo://a b", "foo://[x]", "http://a^b", "http://a|b", "http://a<b", "foo://x:abc", "http://#", "http://?"];
        for text in valid {
            assert!(url_can_parse(text), "{text} parses");
        }
        for text in invalid {
            assert!(!url_can_parse(text), "{text} does not parse");
        }
    }

    #[test]
    fn dates() {
        assert!(date("2026-02-28"));
        assert!(!date("2026-02-30"));
        assert!(date("2024-02-29"));
        assert!(!date("2026-13-01"));
    }
}
