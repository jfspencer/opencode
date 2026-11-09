//! Grapheme clusters and their cell widths, as the buffer's text drawing needs them (`utf8.zig`:
//! `findRenderClusterInfo`, `getWidthAt`, `calculateTextWidth`, `isAsciiOnly`).
//!
//! TODO(port-tui): opencode-tui-text ports utf8.zig and grapheme.zig with uucode's tables; this stand-in
//! segments with a short list of extending code points and measures with a short list of wide and zero-width
//! ranges. Every caller goes through [`clusters`] and [`text_width`], so the swap is in this file only.
//! It agrees with the reference on ASCII, Latin, box drawing and block elements, CJK, single emoji and
//! combining marks; ZWJ sequences, flags and rarer scripts may differ until the text crate lands.

/// `WidthMethod` (utf8.zig).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum WidthMethod {
    Wcwidth,
    #[default]
    Unicode,
    NoZwj,
    UnicodeWide,
}

impl WidthMethod {
    /// The name TS uses (`"wcwidth" | "unicode" | ...`).
    pub fn name(self) -> &'static str {
        match self {
            WidthMethod::Wcwidth => "wcwidth",
            WidthMethod::Unicode => "unicode",
            WidthMethod::NoZwj => "no_zwj",
            WidthMethod::UnicodeWide => "unicode_wide",
        }
    }
}

/// One grapheme cluster of a text: its bytes and the cells it takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cluster {
    pub byte_start: usize,
    pub byte_len: usize,
    pub width: u32,
}

/// `isAsciiOnly`: non-empty and every byte printable ASCII (32..126).
pub fn is_ascii_only(text: &[u8]) -> bool {
    !text.is_empty() && text.iter().all(|b| (0x20..0x7f).contains(b))
}

fn in_ranges(cp: u32, ranges: &[(u32, u32)]) -> bool {
    ranges.binary_search_by(|&(lo, hi)| {
        if hi < cp {
            std::cmp::Ordering::Less
        } else if lo > cp {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }).is_ok()
}

const ZERO_WIDTH: &[(u32, u32)] = &[
    (0x0300, 0x036F),
    (0x0483, 0x0489),
    (0x0591, 0x05BD),
    (0x05BF, 0x05BF),
    (0x05C1, 0x05C2),
    (0x05C4, 0x05C5),
    (0x05C7, 0x05C7),
    (0x0610, 0x061A),
    (0x064B, 0x065F),
    (0x0670, 0x0670),
    (0x06D6, 0x06DC),
    (0x06DF, 0x06E4),
    (0x06E7, 0x06E8),
    (0x06EA, 0x06ED),
    (0x0E31, 0x0E31),
    (0x0E34, 0x0E3A),
    (0x0E47, 0x0E4E),
    (0x1AB0, 0x1AFF),
    (0x1DC0, 0x1DFF),
    (0x200B, 0x200F),
    (0x2028, 0x202E),
    (0x2060, 0x2064),
    (0x20D0, 0x20FF),
    (0xFE00, 0xFE0F),
    (0xFE20, 0xFE2F),
    (0xFEFF, 0xFEFF),
    (0xE0000, 0xE0FFF),
];

const WIDE: &[(u32, u32)] = &[
    (0x1100, 0x115F),
    (0x231A, 0x231B),
    (0x2329, 0x232A),
    (0x23E9, 0x23EC),
    (0x23F0, 0x23F0),
    (0x23F3, 0x23F3),
    (0x25FD, 0x25FE),
    (0x2614, 0x2615),
    (0x2648, 0x2653),
    (0x267F, 0x267F),
    (0x2693, 0x2693),
    (0x26A1, 0x26A1),
    (0x26AA, 0x26AB),
    (0x26BD, 0x26BE),
    (0x26C4, 0x26C5),
    (0x26CE, 0x26CE),
    (0x26D4, 0x26D4),
    (0x26EA, 0x26EA),
    (0x26F2, 0x26F3),
    (0x26F5, 0x26F5),
    (0x26FA, 0x26FA),
    (0x26FD, 0x26FD),
    (0x2705, 0x2705),
    (0x270A, 0x270B),
    (0x2728, 0x2728),
    (0x274C, 0x274C),
    (0x274E, 0x274E),
    (0x2753, 0x2755),
    (0x2757, 0x2757),
    (0x2795, 0x2797),
    (0x27B0, 0x27B0),
    (0x27BF, 0x27BF),
    (0x2B1B, 0x2B1C),
    (0x2B50, 0x2B50),
    (0x2B55, 0x2B55),
    (0x2E80, 0x303E),
    (0x3041, 0x33FF),
    (0x3400, 0x4DBF),
    (0x4E00, 0x9FFF),
    (0xA000, 0xA4CF),
    (0xA960, 0xA97F),
    (0xAC00, 0xD7A3),
    (0xF900, 0xFAFF),
    (0xFE10, 0xFE19),
    (0xFE30, 0xFE6F),
    (0xFF00, 0xFF60),
    (0xFFE0, 0xFFE6),
    (0x16FE0, 0x16FE4),
    (0x17000, 0x18AFF),
    (0x1B000, 0x1B2FF),
    (0x1F004, 0x1F004),
    (0x1F0CF, 0x1F0CF),
    (0x1F18E, 0x1F18E),
    (0x1F191, 0x1F19A),
    (0x1F200, 0x1F251),
    (0x1F300, 0x1F320),
    (0x1F32D, 0x1F335),
    (0x1F337, 0x1F37C),
    (0x1F37E, 0x1F393),
    (0x1F3A0, 0x1F3CA),
    (0x1F3CF, 0x1F3D3),
    (0x1F3E0, 0x1F3F0),
    (0x1F3F4, 0x1F3F4),
    (0x1F3F8, 0x1F43E),
    (0x1F440, 0x1F440),
    (0x1F442, 0x1F4FC),
    (0x1F4FF, 0x1F53D),
    (0x1F54B, 0x1F54E),
    (0x1F550, 0x1F567),
    (0x1F57A, 0x1F57A),
    (0x1F595, 0x1F596),
    (0x1F5A4, 0x1F5A4),
    (0x1F5FB, 0x1F64F),
    (0x1F680, 0x1F6C5),
    (0x1F6CC, 0x1F6CC),
    (0x1F6D0, 0x1F6D2),
    (0x1F6D5, 0x1F6D7),
    (0x1F6DC, 0x1F6DF),
    (0x1F6EB, 0x1F6EC),
    (0x1F6F4, 0x1F6FC),
    (0x1F7E0, 0x1F7EB),
    (0x1F7F0, 0x1F7F0),
    (0x1F90C, 0x1F93A),
    (0x1F93C, 0x1F945),
    (0x1F947, 0x1F9FF),
    (0x1FA70, 0x1FAFF),
    (0x20000, 0x2FFFD),
    (0x30000, 0x3FFFD),
];

/// Code points that extend the cluster before them.
fn extends(cp: u32) -> bool {
    cp == 0x200D
        || (0x1F3FB..=0x1F3FF).contains(&cp)
        || (in_ranges(cp, ZERO_WIDTH) && cp != 0x200B && !(0x2028..=0x202E).contains(&cp) && cp != 0xFEFF)
}

fn is_control(cp: u32) -> bool {
    cp < 0x20 || (0x7f..=0x9f).contains(&cp)
}

/// The cells one code point takes (`charWidth` with controls hidden and the given tab width).
fn code_point_width(cp: u32, tab_width: u32) -> u32 {
    if cp == '\t' as u32 {
        return tab_width;
    }
    if is_control(cp) || in_ranges(cp, ZERO_WIDTH) {
        return 0;
    }
    if in_ranges(cp, WIDE) {
        return 2;
    }
    1
}

fn is_regional_indicator(cp: u32) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&cp)
}

/// The text's grapheme clusters with their widths (tab = `tab_width` cells, controls zero), every cluster
/// listed, zero-width ones included.
pub fn clusters(text: &str, tab_width: u32, method: WidthMethod) -> Vec<Cluster> {
    let mut out: Vec<Cluster> = Vec::new();
    let mut previous: Option<u32> = None;
    // The current cluster is a lone regional indicator, which the next one pairs with into a flag.
    let mut lone_indicator = false;
    for (offset, ch) in text.char_indices() {
        let cp = ch as u32;
        let len = ch.len_utf8();
        let joins = match previous {
            None => false,
            // GB3: CR LF stays one cluster; otherwise controls break.
            Some(0x0D) => cp == 0x0A,
            Some(prev) if is_control(prev) || is_control(cp) => false,
            Some(0x200D) => method != WidthMethod::NoZwj,
            Some(_) if is_regional_indicator(cp) => lone_indicator,
            Some(_) => extends(cp),
        };
        if joins {
            let last = out.last_mut().expect("a joined code point follows a cluster");
            last.byte_len += len;
            if method == WidthMethod::Wcwidth {
                last.width += code_point_width(cp, tab_width);
            } else if (cp == 0xFE0F && last.width == 1) || is_regional_indicator(cp) {
                // VS16 makes a narrow symbol an emoji, and a pair of indicators is a flag; a ZWJ sequence keeps
                // its first glyph's width.
                last.width = 2;
            }
            lone_indicator = false;
        } else {
            let width = if is_regional_indicator(cp) { 1 } else { code_point_width(cp, tab_width) };
            out.push(Cluster { byte_start: offset, byte_len: len, width });
            lone_indicator = is_regional_indicator(cp);
        }
        previous = Some(cp);
    }
    out
}

/// The cells of the first cluster at `offset` (`getWidthAt` with controls hidden).
pub fn width_at(text: &str, offset: usize, tab_width: u32, method: WidthMethod) -> u32 {
    clusters(&text[offset..], tab_width, method).first().map_or(0, |cluster| cluster.width)
}

/// `calculateTextWidth` with controls hidden.
pub fn text_width(text: &str, tab_width: u32, method: WidthMethod) -> u32 {
    if is_ascii_only(text.as_bytes()) {
        return text.len() as u32;
    }
    clusters(text, tab_width, method).iter().map(|cluster| cluster.width).sum()
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
    }
}
