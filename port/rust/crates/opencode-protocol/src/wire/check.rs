//! The Effect Schema checks the TS schemas use, with JS semantics: string lengths count UTF-16 code units,
//! `isTrimmed` uses JS whitespace, `isInt` means a safe integer.
//!
//! Each check takes the message Effect reports when it fails: the filter's `message` annotation, or `Expected `
//! followed by its `expected` annotation (`Expected a value greater than 0`). The generator reads both from the TS
//! AST, so a check with a custom message reports that message.

use super::Issue;
use super::number::{is_safe_integer, js_trim};
use regex::Regex;

fn ok_or(ok: bool, msg: &'static str) -> Result<(), Issue> {
    if ok { Ok(()) } else { Err(Issue::new(msg)) }
}

/// JS `string.length`.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

pub fn min_length(s: &str, n: usize, msg: &'static str) -> Result<(), Issue> {
    ok_or(utf16_len(s) >= n, msg)
}

pub fn max_length(s: &str, n: usize, msg: &'static str) -> Result<(), Issue> {
    ok_or(utf16_len(s) <= n, msg)
}

pub fn min_items<T>(items: &[T], n: usize, msg: &'static str) -> Result<(), Issue> {
    ok_or(items.len() >= n, msg)
}

pub fn max_items<T>(items: &[T], n: usize, msg: &'static str) -> Result<(), Issue> {
    ok_or(items.len() <= n, msg)
}

pub fn unique<T: PartialEq>(items: &[T], msg: &'static str) -> Result<(), Issue> {
    ok_or(items.iter().enumerate().all(|(i, a)| !items[..i].contains(a)), msg)
}

pub fn pattern(s: &str, re: &Regex, msg: &'static str) -> Result<(), Issue> {
    ok_or(re.is_match(s), msg)
}

pub fn starts_with(s: &str, prefix: &str, msg: &'static str) -> Result<(), Issue> {
    ok_or(s.starts_with(prefix), msg)
}

pub fn ends_with(s: &str, suffix: &str, msg: &'static str) -> Result<(), Issue> {
    ok_or(s.ends_with(suffix), msg)
}

pub fn includes(s: &str, part: &str, msg: &'static str) -> Result<(), Issue> {
    ok_or(s.contains(part), msg)
}

pub fn trimmed(s: &str, msg: &'static str) -> Result<(), Issue> {
    ok_or(js_trim(s) == s, msg)
}

pub fn int(n: f64, msg: &'static str) -> Result<(), Issue> {
    ok_or(is_safe_integer(n), msg)
}

pub fn finite(n: f64, msg: &'static str) -> Result<(), Issue> {
    ok_or(n.is_finite(), msg)
}

pub fn gt(n: f64, min: f64, msg: &'static str) -> Result<(), Issue> {
    ok_or(n > min, msg)
}

pub fn gte(n: f64, min: f64, msg: &'static str) -> Result<(), Issue> {
    ok_or(n >= min, msg)
}

pub fn lt(n: f64, max: f64, msg: &'static str) -> Result<(), Issue> {
    ok_or(n < max, msg)
}

pub fn lte(n: f64, max: f64, msg: &'static str) -> Result<(), Issue> {
    ok_or(n <= max, msg)
}
