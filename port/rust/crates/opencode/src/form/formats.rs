//! The JS built-ins form answers are checked with: `new RegExp(pattern)` (no flags), the email pattern,
//! `URL.canParse`, and `new Date(...)` validity (`date.rs`). Each was compared with the reference's engine on
//! generated inputs (tens of thousands of patterns and strings, dates, URLs): they agree except where noted.
//!
//! TODO(port): what the engine does that Rust's does not. Patterns are translated to Rust `regex` syntax (Annex B
//! included: literal braces, identity and legacy octal escapes, "nothing to repeat"); lookaround and
//! backreferences have no Rust counterpart and are reported as invalid patterns, and matching is by code point where
//! JS matches UTF-16 units. `URL.canParse` follows the WHATWG parser's checks (scheme, authority, host, IPv4, IPv6,
//! port) without IDNA, so hosts IDNA would reject or empty (`xn--a`, a lone soft hyphen) are accepted.

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

/// Capturing groups of a pattern (numbered and named), for telling backreferences from legacy octal escapes.
fn groups(chars: &[char]) -> (usize, bool) {
    let (mut count, mut named, mut in_class, mut i) = (0, false, false, 0);
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => match (chars.get(i + 1), chars.get(i + 2), chars.get(i + 3)) {
                (Some('?'), Some('<'), Some(c)) if !matches!(c, '=' | '!') => {
                    count += 1;
                    named = true;
                }
                (Some('?'), _, _) => {}
                _ => count += 1,
            },
            _ => {}
        }
        i += 1;
    }
    (count, named)
}

/// What a quantifier would apply to.
#[derive(Clone, Copy, PartialEq)]
enum Last {
    /// The start of the pattern, a group or an alternative: nothing to repeat.
    Start,
    Atom,
    /// `^`, `$`, `\b`, `\B`: not quantifiable.
    Assertion,
    Quantifier,
    /// A quantifier made lazy (`*?`).
    Lazy,
}

/// `new RegExp(source)` (no flags): the pattern translated to Rust syntax, `None` when JS rejects it or Rust cannot
/// run it (lookaround, backreferences).
pub fn js_regex(source: &str) -> Option<Regex> {
    let chars: Vec<char> = source.chars().collect();
    let (group_count, named) = groups(&chars);
    let mut out = String::new();
    let mut in_class = false;
    let mut last = Last::Start;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            i += 1;
            let n = *chars.get(i)?;
            if !in_class {
                last = if matches!(n, 'b' | 'B') { Last::Assertion } else { Last::Atom };
            }
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
                '0'..='9' => {
                    let decimal: usize =
                        chars[i..].iter().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(usize::MAX);
                    if n != '0' && decimal <= group_count && !in_class {
                        return None;
                    }
                    if matches!(n, '8' | '9') {
                        out.push(n);
                    } else {
                        // Annex B legacy octal escape: up to three octal digits, at most 0o377.
                        let mut octal: String = chars[i..].iter().take(3).take_while(|c| ('0'..='7').contains(*c)).collect();
                        if octal.len() == 3 && octal.as_bytes()[0] > b'3' {
                            octal.pop();
                        }
                        out.push_str(&format!("\\x{{{:X}}}", u32::from_str_radix(&octal, 8).unwrap_or(0)));
                        i += octal.len() - 1;
                    }
                }
                'k' if named => return None,
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
                    last = Last::Atom;
                }
                '[' | '&' | '~' => out.push_str(&format!("\\{c}")),
                c => out.push(c),
            }
            i += 1;
            continue;
        }
        if matches!(c, '*' | '+' | '?') || (c == '{' && quantifier_at(&chars, i)) {
            last = match last {
                Last::Atom => Last::Quantifier,
                Last::Quantifier if c == '?' => Last::Lazy,
                _ => return None,
            };
            if c == '{' {
                while chars[i] != '}' {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            out.push(chars[i]);
            i += 1;
            continue;
        }
        last = match c {
            '(' | '|' => Last::Start,
            '^' | '$' => Last::Assertion,
            _ => Last::Atom,
        };
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
                Some(':') => {
                    out.push_str("(?:");
                    i += 3;
                    continue;
                }
                Some('<') if !matches!(chars.get(i + 3), Some('=' | '!')) => {
                    let close = chars[i..].iter().position(|c| *c == '>')?;
                    out.extend(&chars[i..=i + close]);
                    i += close + 1;
                    continue;
                }
                _ => return None,
            },
            '.' => out.push_str(r"[^\n\r\x{2028}\x{2029}]"),
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

/// The WHATWG IPv6 parser's verdict on a bracketed host.
fn valid_ipv6(text: &str) -> bool {
    let c: Vec<char> = text[1..text.len() - 1].chars().collect();
    let at = |i: usize| c.get(i).copied();
    let (mut piece, mut compress, mut i) = (0usize, false, 0usize);
    if at(0) == Some(':') {
        if at(1) != Some(':') {
            return false;
        }
        i = 2;
        piece = 1;
        compress = true;
    }
    while at(i).is_some() {
        if piece == 8 {
            return false;
        }
        if at(i) == Some(':') {
            if compress {
                return false;
            }
            i += 1;
            piece += 1;
            compress = true;
            continue;
        }
        let mut length = 0;
        while length < 4 && at(i).is_some_and(|c| c.is_ascii_hexdigit()) {
            i += 1;
            length += 1;
        }
        if at(i) == Some('.') {
            if length == 0 || piece > 6 {
                return false;
            }
            i -= length;
            let mut seen = 0;
            while at(i).is_some() {
                if seen > 0 {
                    if at(i) != Some('.') || seen >= 4 {
                        return false;
                    }
                    i += 1;
                }
                if !at(i).is_some_and(|c| c.is_ascii_digit()) {
                    return false;
                }
                let mut value: Option<u32> = None;
                while let Some(d) = at(i).and_then(|c| c.to_digit(10)) {
                    value = match value {
                        None => Some(d),
                        Some(0) => return false,
                        Some(v) => Some(v * 10 + d),
                    };
                    if value > Some(255) {
                        return false;
                    }
                    i += 1;
                }
                seen += 1;
                if seen == 2 || seen == 4 {
                    piece += 1;
                }
            }
            return seen == 4 && (compress || piece == 8);
        }
        if at(i) == Some(':') {
            i += 1;
            if at(i).is_none() {
                return false;
            }
        } else if at(i).is_some() {
            return false;
        }
        piece += 1;
    }
    compress || piece == 8
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
        let bracketed = host.starts_with('[') && host.ends_with(']') && valid_ipv6(host);
        return host.is_empty() || drive || bracketed || valid_domain(&percent_decode(host).to_lowercase());
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
        // Recorded from the reference's engine: nothing to repeat, stacked quantifiers, legacy octal escapes.
        assert!(js_regex("^+").is_none());
        assert!(js_regex(r"\b*").is_none());
        assert!(js_regex("a{1,}*").is_none());
        assert!(js_regex(r"\u{41}+").is_none());
        assert!(js_regex("a*?").unwrap().is_match("b"));
        assert!(js_regex(r"\1").unwrap().is_match("\u{1}"));
        assert!(js_regex(r"(a)\1").is_none());
        assert!(js_regex(r"\8").unwrap().is_match("8"));
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
        // Edge cases, recorded the same way. (IDNA is not applied: `http://\u{ad}` and `http://xn--a` differ.)
        let valid = ["foo://a:", "foo://a@b@c", "http://a@b@c", "http://1.2.3.4.", "file://[::1]/x", "file:////x", "http://%41", "http://a..b", "http://.", "http://..", "foo:/", "foo:", "a:b:c", "foo://a%20b", "foo://%", "ws:x", "ftp://a", "https://example.com/a b", "https://example.com/?q=a b", "http://ab%2Ecom", "http://0", "http://00000000000000000000001", "http://0x", "http://0xg", "http://1.0x", "http://example.com:65535", "http://[::ffff:1.2.3.4]", "\u{0}http://x", "ht\ttp://x", "http://x\n.com", "HtTpS://A.B", "a.b-c+d:e", "http://a/%zz", "http://\u{c4}.com", "http://fa\u{df}.de", "http://\u{200b}.com"];
        let invalid = ["foo://@", "http://[::1", "http://[::1]x", "http://a.0x", "http://0x100000000", "http://08", "http://09.1", "file://a b/", "http://%", "http://a%zz", "http://a:b@c:d", "http://a:-1", "http://a:1:2", "foo://a:1:2", "foo://a\\b", "wss://", "http://a b.com/x", "http://a%00b", "http://example.com:65536", "http://[1:2:3:4:5:6:7:8:9]", "http://[1::2::3]", "http://[:1]", "http://\u{0}x", "http:// x", "+a:b", "1:2", "-a:b", ":a", "", " ", "a"];
        for text in valid {
            assert!(url_can_parse(text), "{text:?} parses");
        }
        for text in invalid {
            assert!(!url_can_parse(text), "{text:?} does not parse");
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
