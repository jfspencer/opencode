//! `packages/core/src/util/wildcard.ts`: the permission pattern language.
//!
//! TS compiles a pattern to a regex: backslashes become `/` in both strings, `*` is `.*`, `?` is `.` (one UTF-16
//! unit, dotAll), `[*]` and `[?]` are literal, everything else is literal, and a trailing ` *` also matches nothing
//! (`( .*)?`, so `ls *` matches `ls`). Matching is case-sensitive except on Windows, which the port does not target.

use crate::js::{self, JsString};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    Literal(u16),
    Any,
    Star,
}

fn normalize(text: &[u16]) -> JsString {
    js::replace_unit(text, b'\\' as u16, &[b'/' as u16])
}

fn tokens(pattern: &[u16]) -> Vec<Token> {
    let pattern = normalize(pattern);
    let mut tokens = Vec::with_capacity(pattern.len());
    let mut i = 0;
    while i < pattern.len() {
        let c = pattern[i];
        if c == b'[' as u16
            && pattern.get(i + 2) == Some(&(b']' as u16))
            && pattern.get(i + 1).is_some_and(|&m| m == b'*' as u16 || m == b'?' as u16)
        {
            tokens.push(Token::Literal(pattern[i + 1]));
            i += 3;
            continue;
        }
        tokens.push(match c {
            0x2a => Token::Star,
            0x3f => Token::Any,
            _ => Token::Literal(c),
        });
        i += 1;
    }
    tokens
}

fn glob(input: &[u16], tokens: &[Token]) -> bool {
    // The classic single-backtrack matcher: a star absorbs one more unit each time the rest fails.
    let (mut i, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while i < input.len() {
        match tokens.get(t) {
            Some(Token::Star) => {
                star = Some((t, i));
                t += 1;
                continue;
            }
            Some(Token::Any) => {
                i += 1;
                t += 1;
                continue;
            }
            Some(Token::Literal(c)) if *c == input[i] => {
                i += 1;
                t += 1;
                continue;
            }
            _ => {}
        }
        match star {
            Some((star_t, star_i)) => {
                star = Some((star_t, star_i + 1));
                t = star_t + 1;
                i = star_i + 1;
            }
            None => return false,
        }
    }
    tokens[t..].iter().all(|token| *token == Token::Star)
}

/// `Wildcard.match(input, pattern)` over UTF-16 units.
pub fn matches_units(input: &[u16], pattern: &[u16]) -> bool {
    let input = normalize(input);
    let tokens = tokens(pattern);
    if glob(&input, &tokens) {
        return true;
    }
    // `if (escaped.endsWith(" .*")) escaped = escaped.slice(0, -3) + "( .*)?"`
    let n = tokens.len();
    n >= 2 && tokens[n - 1] == Token::Star && tokens[n - 2] == Token::Literal(b' ' as u16) && glob(&input, &tokens[..n - 2])
}

/// `Wildcard.match(input, pattern)`.
pub fn matches(input: &str, pattern: &str) -> bool {
    matches_units(&js::units(input), &js::units(pattern))
}

/// `Wildcard.escape(text)`: a pattern that matches `text` itself.
pub fn escape_units(text: &[u16]) -> JsString {
    let mut out = JsString::with_capacity(text.len());
    for &c in text {
        if c == b'*' as u16 || c == b'?' as u16 {
            out.extend_from_slice(&[b'[' as u16, c, b']' as u16]);
        } else {
            out.push(c);
        }
    }
    out
}

/// `Wildcard.escape(text)`.
pub fn escape(text: &str) -> String {
    js::text(&escape_units(&js::units(text)))
}
