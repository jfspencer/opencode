//! Grapheme clusters and their cell widths: `opencode-tui-text`'s port of `utf8.zig` (with uucode's tables), the
//! one set of widths that text buffers, frame buffers and components share. The buffer's own text drawing calls
//! `utf8` directly as Zig does (`findRenderClusterInfo`, `getWidthAt`, `calculateTextWidth`); this module adds the
//! whole-text cluster list that components need (`Intl.Segmenter` graphemes with widths).
pub use opencode_tui_text::WidthMethod;
use opencode_tui_text::unicode::BreakState;
use opencode_tui_text::utf8::{self, ControlDisplay, GraphemeWidthState};

/// One grapheme cluster of a text: its bytes and the cells it takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cluster {
    pub byte_start: usize,
    pub byte_len: usize,
    pub width: u32,
}

/// `isAsciiOnly`: non-empty and every byte printable ASCII (32..126).
pub fn is_ascii_only(text: &[u8]) -> bool {
    utf8::is_ascii_only(text)
}

/// The text's grapheme clusters with their widths (tab = `tab_width` cells, controls zero), every cluster
/// listed, zero-width ones included: `findRenderClusterInfo`'s walk without its sparseness.
pub fn clusters(text: &str, tab_width: u32, method: WidthMethod) -> Vec<Cluster> {
    let bytes = text.as_bytes();
    let tab_width = tab_width.min(u8::MAX as u32) as u8;
    let mut out = Vec::new();
    let mut pos = 0;
    let mut prev: Option<u32> = None;
    let mut break_state = BreakState::Default;
    let mut start = 0;
    let mut state = GraphemeWidthState::new(0, 0, method);
    while pos < bytes.len() {
        let (cp, len) = utf8::decode(bytes, pos);
        let width = utf8::char_width(bytes[pos], cp, tab_width, ControlDisplay::Hidden);
        if utf8::is_grapheme_break(prev, cp, &mut break_state, method) {
            if prev.is_some() {
                out.push(Cluster { byte_start: start, byte_len: pos - start, width: state.width });
            }
            start = pos;
            state = GraphemeWidthState::new(cp, width, method);
        } else {
            state.add(cp, width);
        }
        prev = Some(cp);
        pos += len.max(1);
    }
    if prev.is_some() {
        out.push(Cluster { byte_start: start, byte_len: bytes.len() - start, width: state.width });
    }
    out
}

/// The cells of the cursor unit at `offset` (`getWidthAt` with controls hidden).
pub fn width_at(text: &str, offset: usize, tab_width: u32, method: WidthMethod) -> u32 {
    utf8::get_width_at(text.as_bytes(), offset, tab_width.min(u8::MAX as u32) as u8, ControlDisplay::Hidden, method)
}

/// `calculateTextWidth` with controls hidden.
pub fn text_width(text: &str, tab_width: u32, method: WidthMethod) -> u32 {
    let bytes = text.as_bytes();
    utf8::calculate_text_width(bytes, tab_width.min(u8::MAX as u32) as u8, ControlDisplay::Hidden, is_ascii_only(bytes), method)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths() {
        assert_eq!(text_width("abc", 2, WidthMethod::Unicode), 3);
        assert_eq!(text_width("中文", 2, WidthMethod::Unicode), 4);
        assert_eq!(text_width("e\u{301}", 2, WidthMethod::Unicode), 1);
        assert_eq!(clusters("e\u{301}x", 2, WidthMethod::Unicode).len(), 2);
        assert_eq!(text_width("\t", 2, WidthMethod::Unicode), 2);
        assert_eq!(text_width("😀", 2, WidthMethod::Unicode), 2);
        assert_eq!(text_width("🇫🇷", 2, WidthMethod::Unicode), 2);
        assert_eq!(clusters("🇫🇷🇩🇪", 2, WidthMethod::Unicode).len(), 2);
        assert_eq!(text_width("┃▀█", 2, WidthMethod::Unicode), 3);
        assert_eq!(clusters("👩‍💻!", 2, WidthMethod::Unicode).iter().map(|c| c.width).collect::<Vec<_>>(), vec![2, 1]);
        let total: u32 = clusters("a\u{301}中\t😀", 2, WidthMethod::Unicode).iter().map(|c| c.width).sum();
        assert_eq!(total, text_width("a\u{301}中\t😀", 2, WidthMethod::Unicode));
    }
}
