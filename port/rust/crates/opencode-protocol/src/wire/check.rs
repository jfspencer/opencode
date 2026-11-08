//! The Effect Schema checks the TS schemas use, with JS semantics: string lengths count UTF-16 code units,
//! `isTrimmed` uses JS whitespace, `isInt` means a safe integer.

use super::Issue;
use super::number::{is_safe_integer, js_trim};
use regex::Regex;

fn fail(message: String) -> Result<(), Issue> {
    Err(Issue::new(message))
}

/// JS `string.length`.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

pub fn min_length(s: &str, n: usize) -> Result<(), Issue> {
    if utf16_len(s) >= n { Ok(()) } else { fail(format!("expected a value with a length of at least {n}")) }
}

pub fn max_length(s: &str, n: usize) -> Result<(), Issue> {
    if utf16_len(s) <= n { Ok(()) } else { fail(format!("expected a value with a length of at most {n}")) }
}

pub fn min_items<T>(items: &[T], n: usize) -> Result<(), Issue> {
    if items.len() >= n { Ok(()) } else { fail(format!("expected a value with a length of at least {n}")) }
}

pub fn max_items<T>(items: &[T], n: usize) -> Result<(), Issue> {
    if items.len() <= n { Ok(()) } else { fail(format!("expected a value with a length of at most {n}")) }
}

pub fn unique<T: PartialEq>(items: &[T]) -> Result<(), Issue> {
    for (i, a) in items.iter().enumerate() {
        if items[..i].contains(a) {
            return fail("expected an array with unique items".into());
        }
    }
    Ok(())
}

pub fn pattern(s: &str, re: &Regex) -> Result<(), Issue> {
    if re.is_match(s) { Ok(()) } else { fail(format!("expected a string matching the RegExp {}", re.as_str())) }
}

pub fn starts_with(s: &str, prefix: &str) -> Result<(), Issue> {
    if s.starts_with(prefix) { Ok(()) } else { fail(format!("expected a string starting with {prefix:?}")) }
}

pub fn ends_with(s: &str, suffix: &str) -> Result<(), Issue> {
    if s.ends_with(suffix) { Ok(()) } else { fail(format!("expected a string ending with {suffix:?}")) }
}

pub fn includes(s: &str, part: &str) -> Result<(), Issue> {
    if s.contains(part) { Ok(()) } else { fail(format!("expected a string including {part:?}")) }
}

pub fn trimmed(s: &str) -> Result<(), Issue> {
    if js_trim(s) == s { Ok(()) } else { fail("expected a string with no leading or trailing whitespace".into()) }
}

pub fn int(n: f64) -> Result<(), Issue> {
    if is_safe_integer(n) { Ok(()) } else { fail("expected an integer".into()) }
}

pub fn finite(n: f64) -> Result<(), Issue> {
    if n.is_finite() { Ok(()) } else { fail("expected a finite number".into()) }
}

pub fn gt(n: f64, min: f64) -> Result<(), Issue> {
    if n > min { Ok(()) } else { fail(format!("expected a value greater than {min}")) }
}

pub fn gte(n: f64, min: f64) -> Result<(), Issue> {
    if n >= min { Ok(()) } else { fail(format!("expected a value greater than or equal to {min}")) }
}

pub fn lt(n: f64, max: f64) -> Result<(), Issue> {
    if n < max { Ok(()) } else { fail(format!("expected a value less than {max}")) }
}

pub fn lte(n: f64, max: f64) -> Result<(), Issue> {
    if n <= max { Ok(()) } else { fail(format!("expected a value less than or equal to {max}")) }
}
