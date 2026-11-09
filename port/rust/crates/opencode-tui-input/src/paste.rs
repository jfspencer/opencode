//! `packages/core/src/lib/paste.ts`: paste metadata, the paste text decoder, and the ANSI stripping pasted text
//! goes through.

/// `PasteKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PasteKind {
    Text,
    Binary,
    Unknown,
}

/// `PasteMetadata`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PasteMetadata {
    pub mime_type: Option<String>,
    pub kind: Option<PasteKind>,
}

/// `decodePasteBytes`: `TextDecoder` UTF-8 (replacement characters, a leading BOM dropped).
pub fn decode_paste_bytes(bytes: &[u8]) -> String {
    crate::js::decode_utf8(bytes)
}

/// `stripAnsiSequences`: removes `ANSI_SEQUENCES` matches, ECMA-48 sequences in ESC and C1 form:
///
/// 1. `(?:\x1b\[|\x9b)[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]?` (a control sequence),
/// 2. `(?:\x1b[P\]X^_]|[\x90\x98\x9d-\x9f])[^\x07\x1b\x9c]*(?:\x07|\x1b\\|\x9c)?` (a control string),
/// 3. `\x1b[\x20-\x2f]*[\x30-\x7e]?` (any other escape),
///
/// tried in that order at each position.
pub fn strip_ansi_sequences(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    let run = |from: usize, accept: &dyn Fn(char) -> bool| {
        let mut end = from;
        while end < chars.len() && accept(chars[end]) {
            end += 1;
        }
        end
    };
    let within = |c: char, low: u32, high: u32| (low..=high).contains(&(c as u32));
    while index < chars.len() {
        let c = chars[index];
        let next = chars.get(index + 1).copied();
        let end = if (c == '\x1b' && next == Some('[')) || c == '\u{9b}' {
            let mut end = index + if c == '\x1b' { 2 } else { 1 };
            end = run(end, &|c| within(c, 0x30, 0x3f));
            end = run(end, &|c| within(c, 0x20, 0x2f));
            if chars.get(end).is_some_and(|&c| within(c, 0x40, 0x7e)) {
                end += 1;
            }
            Some(end)
        } else if (c == '\x1b' && matches!(next, Some('P' | ']' | 'X' | '^' | '_')))
            || matches!(c, '\u{90}' | '\u{98}' | '\u{9d}'..='\u{9f}')
        {
            let mut end = index + if c == '\x1b' { 2 } else { 1 };
            end = run(end, &|c| !matches!(c, '\x07' | '\x1b' | '\u{9c}'));
            match chars.get(end) {
                Some('\x07' | '\u{9c}') => end += 1,
                Some('\x1b') if chars.get(end + 1) == Some(&'\\') => end += 2,
                _ => {}
            }
            Some(end)
        } else if c == '\x1b' {
            let mut end = run(index + 1, &|c| within(c, 0x20, 0x2f));
            if chars.get(end).is_some_and(|&c| within(c, 0x30, 0x7e)) {
                end += 1;
            }
            Some(end)
        } else {
            None
        };
        match end {
            Some(end) => index = end,
            None => {
                out.push(c);
                index += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sequences() {
        assert_eq!(strip_ansi_sequences("a\x1b[31mb\x1b[0m"), "ab");
        assert_eq!(strip_ansi_sequences("x\x1b]0;title\x07y"), "xy");
        assert_eq!(strip_ansi_sequences("x\x1b]0;t\x1b\\y"), "xy");
        assert_eq!(strip_ansi_sequences("x\x1b]0;t\x1bqy"), "xy");
        assert_eq!(strip_ansi_sequences("\x1b"), "");
        assert_eq!(strip_ansi_sequences("a\x1b(Bb"), "ab");
        assert_eq!(strip_ansi_sequences("\u{9b}1;2Hz"), "z");
    }
}
