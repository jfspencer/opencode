//! `isStaticBashWord` and `hasPowerShellDotWildcard` of parse.ts: whether a raw word means the same text however
//! the shell expands it.

use crate::js::{self, at, len, set_has, starts_with_ascii};

/// The segment bookkeeping both checks share: whether a path segment starts with a dot and holds an unquoted glob
/// (`.*` would match hidden files), and whether a glob shares the word with an escape.
#[derive(Default)]
struct Segments {
    started: bool,
    leading_dot: bool,
    glob: bool,
    escape: bool,
    unquoted_glob: bool,
}

impl Segments {
    /// `recordChar` of `isStaticBashWord`: false when the word is not static. `c` is `None` where TS reads one
    /// past the end (`raw[index + 1] ?? ""`).
    fn record_bash(&mut self, c: Option<u16>, unquoted_glob: bool) -> bool {
        if unquoted_glob {
            self.unquoted_glob = true;
        }
        if self.escape && self.unquoted_glob {
            return false;
        }
        if c == Some(b'/' as u16) {
            self.started = false;
            self.leading_dot = false;
            self.glob = false;
            return true;
        }
        if !self.started {
            self.started = true;
            self.leading_dot = c == Some(b'.' as u16);
        }
        if unquoted_glob {
            self.glob = true;
        }
        !(self.leading_dot && self.glob)
    }

    /// `recordChar` of `hasPowerShellDotWildcard`: true when the word has a dot wildcard.
    fn record_powershell(&mut self, c: Option<u16>, unquoted_glob: bool) -> bool {
        if unquoted_glob {
            self.unquoted_glob = true;
        }
        if self.escape && self.unquoted_glob {
            return true;
        }
        if c == Some(b'/' as u16) || c == Some(b'\\' as u16) {
            self.started = false;
            self.leading_dot = false;
            self.glob = false;
            return false;
        }
        if !self.started {
            self.started = true;
            self.leading_dot = c == Some(b'.' as u16);
        }
        if unquoted_glob {
            self.glob = true;
        }
        self.leading_dot && self.glob
    }
}

pub fn is_static_bash_word(raw: &[u16]) -> bool {
    #[derive(PartialEq, Clone, Copy)]
    enum Quote {
        Single,
        Double,
        Ansi,
    }
    let mut quote: Option<Quote> = None;
    let mut segments = Segments::default();
    let mut braces: Vec<bool> = Vec::new();
    let dynamic_dollar = |next: Option<u16>, index: isize| {
        next.is_some_and(|n| js::is_word(n) || set_has("@*#?$!({[-", n)) || starts_with_ascii(raw, "\\\n", index + 1)
    };
    let n = len(raw);
    let mut index: isize = -1;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        let c = raw[index as usize];
        let next = at(raw, index + 1);
        match quote {
            Some(Quote::Single) => {
                if c == b'\'' as u16 {
                    quote = None;
                } else if !segments.record_bash(Some(c), false) {
                    return false;
                }
                continue;
            }
            Some(Quote::Ansi) => {
                if c == b'\\' as u16 {
                    segments.escape = true;
                    if segments.unquoted_glob {
                        return false;
                    }
                    index += 1;
                } else if c == b'\'' as u16 {
                    quote = None;
                } else if !segments.record_bash(Some(c), false) {
                    return false;
                }
                continue;
            }
            Some(Quote::Double) => {
                if c == b'\\' as u16 && js::set_has_or_empty("$`\"\\\n", next) {
                    index += 1;
                    if next != Some(b'\n' as u16) && !segments.record_bash(next, false) {
                        return false;
                    }
                    continue;
                }
                if c == b'"' as u16 {
                    quote = None;
                    continue;
                }
                if c == b'`' as u16 || (c == b'$' as u16 && dynamic_dollar(next, index)) {
                    return false;
                }
                if !segments.record_bash(Some(c), false) {
                    return false;
                }
                continue;
            }
            None => {}
        }
        if c == b'\\' as u16 {
            index += 1;
            if next != Some(b'\n' as u16) && !segments.record_bash(next, false) {
                return false;
            }
            continue;
        }
        if c == b'\'' as u16 {
            quote = Some(Quote::Single);
            continue;
        }
        if c == b'"' as u16 {
            quote = Some(Quote::Double);
            continue;
        }
        if c == b'$' as u16 && next == Some(b'\'' as u16) {
            quote = Some(Quote::Ansi);
            index += 1;
            continue;
        }
        if c == b'$' as u16 && next == Some(b'"' as u16) {
            quote = Some(Quote::Double);
            index += 1;
            continue;
        }
        if c == b'`' as u16
            || ((c == b'<' as u16 || c == b'>' as u16 || c == b'=' as u16) && next == Some(b'(' as u16))
            || (c == b'$' as u16 && dynamic_dollar(next, index))
        {
            return false;
        }
        if c == b'{' as u16 {
            braces.push(false);
            if !segments.record_bash(Some(c), false) {
                return false;
            }
            continue;
        }
        if !braces.is_empty() && (c == b',' as u16 || (c == b'.' as u16 && next == Some(b'.' as u16))) {
            *braces.last_mut().unwrap() = true;
        }
        if c == b'}' as u16 && !braces.is_empty() {
            if braces.pop().unwrap() {
                return false;
            }
            if !segments.record_bash(Some(c), false) {
                return false;
            }
            continue;
        }
        let glob = c == b'*' as u16 || c == b'?' as u16 || c == b'[' as u16;
        if !segments.record_bash(Some(c), glob) {
            return false;
        }
    }
    quote.is_none()
}

pub fn has_powershell_dot_wildcard(raw: &[u16]) -> bool {
    let mut quote: Option<u16> = None;
    let mut segments = Segments::default();
    let n = len(raw);
    let mut index: isize = -1;
    loop {
        index += 1;
        if index >= n {
            break;
        }
        let c = raw[index as usize];
        let next = at(raw, index + 1);
        if quote == Some(b'\'' as u16) {
            if c == b'\'' as u16 && next == Some(b'\'' as u16) {
                index += 1;
                if segments.record_powershell(Some(b'\'' as u16), false) {
                    return true;
                }
            } else if c == b'\'' as u16 {
                quote = None;
            } else if segments.record_powershell(Some(c), false) {
                return true;
            }
            continue;
        }
        if quote == Some(b'"' as u16) {
            if c == b'`' as u16 {
                segments.escape = true;
                if segments.unquoted_glob {
                    return true;
                }
                index += 1;
            } else if c == b'"' as u16 && next == Some(b'"' as u16) {
                index += 1;
                if segments.record_powershell(Some(b'"' as u16), false) {
                    return true;
                }
            } else if c == b'"' as u16 {
                quote = None;
            } else if segments.record_powershell(Some(c), false) {
                return true;
            }
            continue;
        }
        if c == b'`' as u16 {
            index += 1;
            if segments.record_powershell(next, false) {
                return true;
            }
            continue;
        }
        if c == b'\'' as u16 || c == b'"' as u16 {
            quote = Some(c);
            continue;
        }
        let glob = c == b'*' as u16 || c == b'?' as u16 || c == b'[' as u16;
        if segments.record_powershell(Some(c), glob) {
            return true;
        }
    }
    false
}
