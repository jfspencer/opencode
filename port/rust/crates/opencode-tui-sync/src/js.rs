//! The few JS semantics the client depends on: `String(value)`, `encodeURIComponent`, `trimStart`, UTF-16
//! string length and `TextDecoder`'s streaming UTF-8 decoding.

use opencode_protocol::Json;
use opencode_protocol::wire::Out;

/// `String(value)` of a JSON value (`undefined` is the caller's business).
pub fn string(value: &Json) -> String {
    match value {
        Json::Null => "null".into(),
        Json::Bool(b) => b.to_string(),
        Json::Number(n) => number(*n),
        Json::String(s) => s.clone(),
        Json::Array(items) => items
            .iter()
            .map(|item| match item {
                // `[null].join()` and `[undefined].join()` are empty.
                Json::Null => String::new(),
                other => string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Json::Object(_) => "[object Object]".into(),
    }
}

/// `String(number)`.
pub fn number(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let mut out = Out::new();
    out.number(n);
    out.finish()
}

/// `encodeURIComponent(text)`.
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

/// JS `WhiteSpace` and `LineTerminator`, which `trim` removes.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\u{0009}' | '\u{000A}' | '\u{000B}' | '\u{000C}' | '\u{000D}' | '\u{0020}' | '\u{00A0}' | '\u{1680}'
            | '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `text.trimStart()`.
pub fn trim_start(text: &str) -> &str {
    text.trim_start_matches(is_space)
}

/// `text.trim()`.
pub fn trim(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `text.length`: UTF-16 code units.
pub fn length(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// `new TextDecoder()` (UTF-8, replacement on errors) with `decode(chunk, { stream })`: a sequence split across
/// chunks decodes once its bytes are complete.
#[derive(Default)]
pub struct TextDecoder {
    pending: Vec<u8>,
}

impl TextDecoder {
    pub fn decode(&mut self, chunk: &[u8], stream: bool) -> String {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(chunk);
        if stream {
            let keep = incomplete_tail(&bytes);
            self.pending = bytes.split_off(bytes.len() - keep);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// How many bytes at the end are the start of a UTF-8 sequence that more bytes could complete.
fn incomplete_tail(bytes: &[u8]) -> usize {
    for back in 1..=3.min(bytes.len()) {
        let byte = bytes[bytes.len() - back];
        if byte & 0xC0 == 0x80 {
            continue;
        }
        let need = match byte {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return 0,
        };
        if need <= back {
            return 0;
        }
        // The continuation bytes so far must be valid for the lead byte (the decoder would reject them at once).
        let tail = &bytes[bytes.len() - back..];
        let second_ok = tail.get(1).is_none_or(|second| match byte {
            0xE0 => (0xA0..=0xBF).contains(second),
            0xED => (0x80..=0x9F).contains(second),
            0xF0 => (0x90..=0xBF).contains(second),
            0xF4 => (0x80..=0x8F).contains(second),
            _ => (0x80..=0xBF).contains(second),
        });
        return if second_ok { back } else { 0 };
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_streams_split_sequences() {
        let mut decoder = TextDecoder::default();
        let text = "a€b😀";
        let bytes = text.as_bytes();
        let mut out = String::new();
        for byte in bytes {
            out.push_str(&decoder.decode(&[*byte], true));
        }
        out.push_str(&decoder.decode(&[], false));
        assert_eq!(out, text);
        let mut decoder = TextDecoder::default();
        assert_eq!(decoder.decode(&[0xE2, 0x82], true), "");
        assert_eq!(decoder.decode(&[], false), "\u{FFFD}");
    }

    #[test]
    fn strings() {
        assert_eq!(number(1e21), "1e+21");
        assert_eq!(number(0.1), "0.1");
        assert_eq!(number(-0.0), "0");
        assert_eq!(encode_uri_component("a b/ü"), "a%20b%2F%C3%BC");
        assert_eq!(trim_start("\u{FEFF} x"), "x");
        assert_eq!(trim_start("\u{0085}x"), "\u{0085}x");
    }
}
