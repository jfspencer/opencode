//! Character classes as acorn tests them. Character codes are `i64` so that `-1` can stand for the `NaN` that
//! `charCodeAt` returns past the end of the input: every predicate here is false for it, as acorn's are for `NaN`.

use super::ident_tables::{ID_CHAR, ID_START};

fn in_table(table: &[(u32, u32)], code: i64) -> bool {
    let code = code as u32;
    table
        .binary_search_by(|&(lo, hi)| {
            if hi < code {
                std::cmp::Ordering::Less
            } else if lo > code {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// acorn's `isIdentifierStart(code, true)`.
pub(super) fn is_identifier_start(code: i64) -> bool {
    if code < 0 {
        return false;
    }
    if code < 65 {
        return code == 36;
    }
    if code < 91 {
        return true;
    }
    if code < 97 {
        return code == 95;
    }
    if code < 123 {
        return true;
    }
    if code < 0x80 {
        return false;
    }
    in_table(&ID_START, code)
}

/// acorn's `isIdentifierChar(code, true)`.
pub(super) fn is_identifier_char(code: i64) -> bool {
    if code < 0 {
        return false;
    }
    if code < 48 {
        return code == 36;
    }
    if code < 58 {
        return true;
    }
    if code < 65 {
        return false;
    }
    if code < 91 {
        return true;
    }
    if code < 97 {
        return code == 95;
    }
    if code < 123 {
        return true;
    }
    if code < 0x80 {
        return false;
    }
    in_table(&ID_CHAR, code)
}

/// acorn's `isNewLine`.
pub(super) fn is_new_line(code: i64) -> bool {
    code == 10 || code == 13 || code == 0x2028 || code == 0x2029
}

/// acorn's `nonASCIIwhitespace`.
pub(super) fn is_non_ascii_whitespace(code: i64) -> bool {
    code == 0x1680 || (0x2000..=0x200a).contains(&code) || code == 0x202f || code == 0x205f || code == 0x3000 || code == 0xfeff
}

/// JavaScript's `\s`.
pub(super) fn is_js_space(code: i64) -> bool {
    matches!(code, 9..=13 | 32 | 0xa0 | 0x2028 | 0x2029) || is_non_ascii_whitespace(code)
}

/// acorn's `codePointToString`, as Rust text (a lone surrogate becomes U+FFFD). `-1` (`NaN`) gives U+10000, as
/// `String.fromCharCode(NaN >> 10 + 0xD800, ...)` does.
pub(super) fn code_point_to_string(code: i64) -> String {
    if code < 0 {
        return "\u{10000}".to_string();
    }
    char::from_u32(code as u32).map(String::from).unwrap_or_else(|| "\u{FFFD}".to_string())
}

/// acorn's `codePointToString`, as UTF-16 code units appended to `out`.
pub(super) fn push_code_point(out: &mut Vec<u16>, code: u32) {
    if code <= 0xffff {
        out.push(code as u16);
    } else {
        let code = code - 0x10000;
        out.push(((code >> 10) + 0xd800) as u16);
        out.push(((code & 1023) + 0xdc00) as u16);
    }
}
