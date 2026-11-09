//! The JS built-ins form answers are checked with, as the reference's engine runs them: `new RegExp(pattern)` (no
//! flags) and the email pattern through `opencode_codemode::jsregex` (JavaScriptCore's grammar and matcher over
//! UTF-16), `URL.canParse` through the `url` crate (the WHATWG parser with IDNA), and `new Date(...)` through
//! `opencode_codemode::jsdate` (bun's port of V8's date parser). Each was compared with the engine on generated
//! inputs.

use opencode_codemode::jsdate;
use opencode_codemode::jsregex::JsRegExp;

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// `new RegExp(source)`: `None` when the engine rejects the pattern.
pub fn js_regex(source: &str) -> Option<JsRegExp> {
    JsRegExp::new(&utf16(source), &[]).ok()
}

/// `regex.test(text)`.
pub fn test(regex: &JsRegExp, text: &str) -> bool {
    regex.test(&utf16(text))
}

/// The form's email check: `/^[^\s@]+@[^\s@]+\.[^\s@]+$/`.
pub fn email(text: &str) -> bool {
    js_regex(r"^[^\s@]+@[^\s@]+\.[^\s@]+$").is_some_and(|regex| test(&regex, text))
}

/// `URL.canParse(text)` (no base).
pub fn url_can_parse(text: &str) -> bool {
    url::Url::parse(text).is_ok_and(|url| url.cannot_be_a_base() || url.is_special() || opaque_authority(text))
}

/// Two WHATWG rules the `url` crate does not apply to a non-special URL's authority, which ends at `/`, `?` or `#`
/// (a backslash is part of it): credentials need a host, and a port is digits only.
fn opaque_authority(text: &str) -> bool {
    let input: String =
        text.trim_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let Some(authority) = input.split_once(':').and_then(|(_, rest)| rest.strip_prefix("//")) else { return true };
    let authority = &authority[..authority.find(['/', '?', '#']).unwrap_or(authority.len())];
    let (credentials, host_port) = match authority.rfind('@') {
        Some(at) => (true, &authority[at + 1..]),
        None => (false, authority),
    };
    if credentials && host_port.is_empty() {
        return false;
    }
    let port = match host_port.rfind(']') {
        Some(end) => host_port[end + 1..].strip_prefix(':'),
        None => host_port.rfind(':').map(|colon| &host_port[colon + 1..]),
    };
    port.is_none_or(|port| port.chars().all(|c| c.is_ascii_digit()))
}

/// The form's date check: `YYYY-MM-DD` that round-trips through `new Date(value + "T00:00:00.000Z")`.
pub fn date(text: &str) -> bool {
    let bytes = text.as_bytes();
    let shape = bytes.len() == 10
        && bytes.iter().enumerate().all(|(i, b)| if i == 4 || i == 7 { *b == b'-' } else { b.is_ascii_digit() });
    shape
        && jsdate::to_iso_string(jsdate::parse(&utf16(&format!("{text}T00:00:00.000Z"))))
            .is_some_and(|iso| iso.get(..10) == Some(text))
}

/// The form's date-time check: `!Number.isNaN(new Date(value).getTime())`.
pub fn date_time(text: &str) -> bool {
    !jsdate::parse(&utf16(text)).is_nan()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        let matches = |pattern: &str, text: &str| js_regex(pattern).map(|regex| test(&regex, text));
        assert_eq!(matches("(", "x"), None);
        assert_eq!(matches("^[a-z]+$", "alice"), Some(true));
        assert_eq!(matches("^[a-z]+$", "Alice"), Some(false));
        assert_eq!(matches(r"^\d{2}$", "12"), Some(true));
        assert_eq!(matches("a{", "a{"), Some(true));
        assert_eq!(matches("^+", ""), None);
        // Lookaround and backreferences run as in the engine.
        assert_eq!(matches("^(?=.*[0-9])[a-z0-9]+$", "abc1"), Some(true));
        assert_eq!(matches("^(?=.*[0-9])[a-z0-9]+$", "abc"), Some(false));
        assert_eq!(matches(r"^(a)\1$", "aa"), Some(true));
        // `.` is one UTF-16 unit without the `u` flag.
        assert_eq!(matches("^.$", "\u{1F600}"), Some(false));
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
        let invalid = ["not a uri", "https://", "http://exa mple.com", "http://a:99999", "http://1.2.3.256", "1http://a", "https://host@", "http://a%20b", "http://a b", "http://1.2.3.4.5", "http://999999999999", "http://4294967296", "http://[zz]", "foo://a b", "foo://[x]", "http://a^b", "http://a|b", "http://a<b", "foo://x:abc", "http://#", "http://?", "http://\u{ad}", "http://xn--a", "foo://@", "foo://-:\\x", "foo://a\\b", "foo://a:1:2"];
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
        assert!(date("0000-01-01"));
        assert!(!date("2026-2-28"));
        // Recorded from the reference's engine (`!Number.isNaN(new Date(text).getTime())`).
        let valid = ["2026-02-28T10:00:00Z", "2026-02-30", "Feb 28 2026", "abc 2026", "10", "-000000-01-01", "1/1/2020 10:00 +596523:00"];
        let invalid = ["yesterday", "Mon", "10:00", "-000000", "2026-02-28T10:00:00 GMT", "1/1/2020 10:00 +596523:30"];
        for text in valid {
            assert!(date_time(text), "{text:?} is a date");
        }
        for text in invalid {
            assert!(!date_time(text), "{text:?} is not a date");
        }
    }
}

