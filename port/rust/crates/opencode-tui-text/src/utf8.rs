//! Port of OpenTUI's `utf8.zig`: display widths, grapheme segmentation, wrap opportunities and byte/column
//! positions, under the four width methods.
//!
//! The Zig code processes 16-byte SIMD windows; for valid UTF-8 (every input here is a `&str` or a slice of one at
//! code point boundaries) a scalar loop visits the same code points in the same order, so the loops are scalar.
//! Where a fast path changes which code paths run (`walk_chunk_layout_info`'s ASCII runs), it is ported as is.

use crate::unicode::{self, BreakState, GraphemeBreak};

/// The method used to calculate the width of a grapheme (`WidthMethod`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
#[repr(u8)]
pub enum WidthMethod {
    /// Sum of code point widths inside each grapheme (tmux-style).
    Wcwidth = 0,
    /// Grapheme-aware widths.
    #[default]
    Unicode = 1,
    /// Grapheme-aware, but ZWJ never joins.
    NoZwj = 2,
    /// Grapheme-aware, widening every multi-code-point grapheme (Ghostty).
    UnicodeWide = 3,
}

impl WidthMethod {
    /// The TS name (`WidthMethod` in `types.ts`).
    pub fn parse(name: &str) -> Option<WidthMethod> {
        match name {
            "wcwidth" => Some(WidthMethod::Wcwidth),
            "unicode" => Some(WidthMethod::Unicode),
            "no-zwj" | "no_zwj" => Some(WidthMethod::NoZwj),
            "unicode-wide" | "unicode_wide" => Some(WidthMethod::UnicodeWide),
            _ => None,
        }
    }
}

/// How layout measures a control character (`ControlDisplay`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlDisplay {
    /// Zero width. Read-only text hides controls.
    Hidden,
    /// Vim notation: `^@`..`^_` and `^?` in two cells, C1 controls as `<80>`..`<9f>` in four.
    Notation,
}

/// Length of the run of printable ASCII (32..126) at the start of `text`.
pub fn printable_ascii_prefix_len(text: &[u8]) -> usize {
    text.iter().position(|&b| !(32..=126).contains(&b)).unwrap_or(text.len())
}

/// Whether `text` is not empty and holds only printable ASCII (32..126).
pub fn is_ascii_only(text: &[u8]) -> bool {
    !text.is_empty() && printable_ascii_prefix_len(text) == text.len()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineBreakKind {
    Lf,
    Cr,
    CrLf,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineBreak {
    /// The position of the LF (or the lone CR).
    pub pos: usize,
    pub kind: LineBreakKind,
}

/// `findLineBreaks`: LF, CR and CRLF, a CRLF reported at its LF.
pub fn find_line_breaks(text: &[u8], out: &mut Vec<LineBreak>) {
    out.clear();
    let mut pos = 0;
    while pos < text.len() {
        match text[pos] {
            b'\n' => out.push(LineBreak { pos, kind: LineBreakKind::Lf }),
            b'\r' => {
                if pos + 1 < text.len() && text[pos + 1] == b'\n' {
                    out.push(LineBreak { pos: pos + 1, kind: LineBreakKind::CrLf });
                    pos += 1;
                } else {
                    out.push(LineBreak { pos, kind: LineBreakKind::Cr });
                }
            }
            _ => {}
        }
        pos += 1;
    }
}

/// `LayoutWrapBreakKind`: every kind is a line-break opportunity; only word-ending kinds are word boundaries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutWrapBreakKind {
    None,
    Whitespace,
    PreservedWhitespace,
    Punctuation,
    ScriptTransition,
    CjkIntercharacter,
}

impl LayoutWrapBreakKind {
    pub fn is_word_boundary(self) -> bool {
        matches!(
            self,
            LayoutWrapBreakKind::Whitespace
                | LayoutWrapBreakKind::PreservedWhitespace
                | LayoutWrapBreakKind::Punctuation
                | LayoutWrapBreakKind::ScriptTransition
        )
    }
}

/// A wrap opportunity: the grapheme that creates the break, by bytes and display columns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LayoutWrapBreak {
    pub byte_start: u32,
    pub col_start: u32,
    pub byte_len: u32,
    pub width_cols: u32,
    pub kind: LayoutWrapBreakKind,
}

impl LayoutWrapBreak {
    pub fn col_end(&self) -> u32 {
        self.col_start + self.width_cols
    }
    pub fn byte_end(&self) -> u32 {
        self.byte_start + self.byte_len
    }
}

#[inline]
fn is_ascii_wrap_break(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t' | b'-' | b'/' | b'\\' | b'.' | b',' | b';' | b':' | b'!' | b'?' | b'(' | b')' | b'[' | b']' | b'{' | b'}'
    )
}

#[inline]
fn ascii_layout_wrap_break_kind(b: u8) -> LayoutWrapBreakKind {
    match b {
        b' ' | b'\t' => LayoutWrapBreakKind::Whitespace,
        b'-' | b'/' | b'\\' | b'.' | b',' | b';' | b':' | b'!' | b'?' | b'(' | b')' | b'[' | b']' | b'{' | b'}' => {
            LayoutWrapBreakKind::Punctuation
        }
        _ => LayoutWrapBreakKind::None,
    }
}

/// `decodeUtf8Unchecked`: the code point at `pos` and its length; a sequence cut by the end of `text` decodes as
/// U+FFFD of length 1.
#[inline]
pub fn decode(text: &[u8], pos: usize) -> (u32, usize) {
    let b0 = text[pos];
    if b0 < 0x80 {
        return (b0 as u32, 1);
    }
    if pos + 1 >= text.len() {
        return (0xFFFD, 1);
    }
    let b1 = text[pos + 1] as u32;
    if b0 & 0xE0 == 0xC0 {
        return (((b0 as u32 & 0x1F) << 6) | (b1 & 0x3F), 2);
    }
    if pos + 2 >= text.len() {
        return (0xFFFD, 1);
    }
    let b2 = text[pos + 2] as u32;
    if b0 & 0xF0 == 0xE0 {
        return (((b0 as u32 & 0x0F) << 12) | ((b1 & 0x3F) << 6) | (b2 & 0x3F), 3);
    }
    if pos + 3 >= text.len() {
        return (0xFFFD, 1);
    }
    let b3 = text[pos + 3] as u32;
    // u21 truncation, as in Zig.
    ((((b0 as u32 & 0x07) << 18) | ((b1 & 0x3F) << 12) | ((b2 & 0x3F) << 6) | (b3 & 0x3F)) & 0x1F_FFFF, 4)
}

/// The decode the measuring loops use: a sequence cut by the end of the text is U+FFFD.
#[inline]
fn decode_checked(text: &[u8], pos: usize) -> (u32, usize) {
    let b0 = text[pos];
    if b0 < 0x80 {
        return (b0 as u32, 1);
    }
    let (cp, len) = decode(text, pos);
    if pos + len > text.len() { (0xFFFD, len) } else { (cp, len) }
}

#[inline]
fn unicode_layout_wrap_break_kind(cp: u32) -> LayoutWrapBreakKind {
    match cp {
        0x00A0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000 | 0x200B => LayoutWrapBreakKind::Whitespace,
        0x00AD | 0x2010 | 0x3001 | 0x3002 | 0xFF01 | 0xFF0C | 0xFF1A | 0xFF1F => LayoutWrapBreakKind::Punctuation,
        _ => LayoutWrapBreakKind::None,
    }
}

/// `WordClass`: word boundaries split ASCII word runs from CJK runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WordClass {
    AsciiWord,
    CjkWord,
    Other,
}

#[inline]
fn is_ascii_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[inline]
fn is_cjk_word_codepoint(cp: u32) -> bool {
    matches!(cp,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2A6DF | 0x2A700..=0x2B73F
        | 0x2B740..=0x2B81F | 0x2B820..=0x2CEAF | 0x2CEB0..=0x2EBEF | 0x2EBF0..=0x2EE5D | 0x2F800..=0x2FA1F
        | 0x3040..=0x309F | 0x30A0..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9D
        | 0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7AF | 0xD7B0..=0xD7FF)
}

#[inline]
pub fn classify_word_class(cp: u32) -> WordClass {
    if cp <= 0x7F {
        return if is_ascii_word_byte(cp as u8) { WordClass::AsciiWord } else { WordClass::Other };
    }
    if is_cjk_word_codepoint(cp) {
        return WordClass::CjkWord;
    }
    WordClass::Other
}

#[inline]
pub fn is_word_codepoint(cp: u32) -> bool {
    classify_word_class(cp) != WordClass::Other
}

/// Adjacent CJK characters form a line-break opportunity (kana marks excepted).
#[inline]
pub fn is_cjk_intercharacter_break(prev: WordClass, curr: WordClass, curr_cp: u32) -> bool {
    prev == WordClass::CjkWord
        && curr == WordClass::CjkWord
        && curr_cp != 0x3099
        && curr_cp != 0x309A
        && curr_cp != 0x30A0
        && curr_cp != 0x30FB
}

#[inline]
pub fn is_cjk_ascii_transition(prev: WordClass, curr: WordClass) -> bool {
    (prev == WordClass::CjkWord && curr == WordClass::AsciiWord) || (prev == WordClass::AsciiWord && curr == WordClass::CjkWord)
}

/// `eastAsianWidth`: the cell width of a code point alone, 0 for controls.
pub fn east_asian_width(cp: u32) -> u32 {
    if cp > 0x10FFFF {
        return 0;
    }
    let width = eaw_to_width(cp);
    if width > 0 { width as u32 } else { 0 }
}

/// `isCombiningMark`: the first code point of `bytes` is a mark (Mn, Mc or Me).
pub fn is_combining_mark(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    unicode::is_mark(decode(bytes, 0).0)
}

/// `eawToWidth`: -1 for control characters.
#[inline]
fn eaw_to_width(cp: u32) -> i16 {
    if cp == 0 {
        return 0;
    }
    if cp < 32 || (0x7F..0xA0).contains(&cp) {
        return -1;
    }
    if unicode::is_mark(cp) {
        return 0;
    }
    if cp != 0x00AD && cp != 0x115F && cp != 0x1160 && unicode::is_default_ignorable(cp) {
        return 0;
    }
    if unicode::is_wide(cp) { 2 } else { 1 }
}

/// Cell count of the notation for `cp`, 0 when it has none (tab, line feed, carriage return, printable).
pub fn control_notation_width(cp: u32) -> u32 {
    if cp == '\t' as u32 || cp == '\n' as u32 || cp == '\r' as u32 {
        return 0;
    }
    if cp < 0x20 || cp == 0x7f {
        return 2;
    }
    if (0x80..0xa0).contains(&cp) {
        return 4;
    }
    0
}

/// Printable ASCII byte of notation cell `index` for control `cp`.
pub fn control_notation_glyph(cp: u32, index: u32) -> u8 {
    debug_assert!(index < control_notation_width(cp));
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    match index {
        0 => {
            if cp < 0x80 {
                b'^'
            } else {
                b'<'
            }
        }
        1 => {
            if cp < 0x80 {
                (cp ^ 0x40) as u8
            } else {
                DIGITS[(cp >> 4) as usize & 0xf]
            }
        }
        2 => DIGITS[(cp & 0xf) as usize],
        _ => b'>',
    }
}

#[inline]
fn control_width(cp: u32, control_display: ControlDisplay) -> u32 {
    match control_display {
        ControlDisplay::Hidden => 0,
        ControlDisplay::Notation => control_notation_width(cp),
    }
}

#[inline]
fn ascii_char_width(byte: u8, tab_width: u8, control_display: ControlDisplay) -> u32 {
    if byte == b'\t' {
        tab_width as u32
    } else if (32..=126).contains(&byte) {
        1
    } else {
        control_width(byte as u32, control_display)
    }
}

/// `charWidth`: the display width of one code point.
#[inline]
pub fn char_width(byte: u8, cp: u32, tab_width: u8, control_display: ControlDisplay) -> u32 {
    if byte < 0x80 {
        return ascii_char_width(byte, tab_width, control_display);
    }
    if cp > 0x10FFFF {
        return 0;
    }
    let w = eaw_to_width(cp);
    if w < 0 {
        return control_width(cp, control_display);
    }
    w as u32
}

#[inline]
fn is_valid_codepoint(cp: u32) -> bool {
    cp != 0xFFFD && cp <= 0x10FFFF
}

/// `isGraphemeBreak`: whether a grapheme starts at `curr` after `prev` (always at the start).
#[inline]
pub fn is_grapheme_break(prev: Option<u32>, curr: u32, state: &mut BreakState, method: WidthMethod) -> bool {
    let Some(p) = prev else { return true };
    let forced = !is_valid_codepoint(curr) || !is_valid_codepoint(p) || (method == WidthMethod::NoZwj && p == 0x200D);
    if forced {
        *state = BreakState::Default;
        return true;
    }
    unicode::is_break(p, curr, state)
}

/// `GraphemeWidthState`: the width of a grapheme as code points join it.
#[derive(Clone, Copy, Debug)]
pub struct GraphemeWidthState {
    pub width: u32,
    has_width: bool,
    is_regional_indicator_pair: bool,
    is_emoji_modifier_base: bool,
    has_indic_virama: bool,
    method: WidthMethod,
}

impl GraphemeWidthState {
    #[inline]
    pub fn new(first_cp: u32, first_width: u32, method: WidthMethod) -> Self {
        GraphemeWidthState {
            width: first_width,
            has_width: first_width > 0,
            is_regional_indicator_pair: (0x1F1E6..=0x1F1FF).contains(&first_cp),
            is_emoji_modifier_base: unicode::grapheme_break(first_cp) == GraphemeBreak::EmojiModifierBase,
            has_indic_virama: false,
            method,
        }
    }

    #[inline]
    pub fn add(&mut self, cp: u32, cp_width: u32) {
        if self.method == WidthMethod::Wcwidth {
            let w = eaw_to_width(cp);
            if w > 0 {
                self.width += w as u32;
                self.has_width = true;
            }
            return;
        }
        let is_ri = (0x1F1E6..=0x1F1FF).contains(&cp);
        let is_vs16 = cp == 0xFE0F;
        let is_virama = unicode::is_nonspacing_mark(cp);
        let gb = unicode::grapheme_break(cp);
        let is_spacing_mark = gb == GraphemeBreak::SpacingMark;
        let is_devanagari_ra = cp == 0x0930;
        let is_devanagari_base = (0x0915..=0x0939).contains(&cp) || (0x0958..=0x095F).contains(&cp);

        if is_vs16 {
            if self.has_width && self.width == 1 {
                self.width = 2;
            }
            return;
        }
        if is_virama {
            self.has_indic_virama = true;
            return;
        }
        if self.is_regional_indicator_pair && is_ri {
            self.width += cp_width;
            self.has_width = true;
        } else if !self.has_width && cp_width > 0 {
            self.width = cp_width;
            self.has_width = true;
        } else if self.is_emoji_modifier_base && self.method != WidthMethod::UnicodeWide && gb == GraphemeBreak::EmojiModifier {
            self.width = 2;
        } else if self.method == WidthMethod::UnicodeWide && self.has_width && !unicode::wcwidth_zero_in_grapheme(cp) {
            self.width = self.width.max(2);
        } else if self.has_width && is_spacing_mark && cp_width > 0 {
            self.width = self.width.max(2);
        } else if self.has_width && self.has_indic_virama && is_devanagari_base && cp_width > 0 {
            if !is_devanagari_ra {
                self.width += cp_width;
            }
            self.has_indic_virama = false;
        }
    }
}

/// `WrapByWidthResult` / `PosByWidthResult`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PosResult {
    pub byte_offset: u32,
    pub grapheme_count: u32,
    pub columns_used: u32,
}

struct ClusterState {
    columns_used: u32,
    grapheme_count: u32,
    cluster_width: u32,
    cluster_start: usize,
    prev_cp: Option<u32>,
    break_state: BreakState,
    width_state: GraphemeWidthState,
    cluster_started: bool,
}

impl ClusterState {
    fn new(method: WidthMethod) -> Self {
        ClusterState {
            columns_used: 0,
            grapheme_count: 0,
            cluster_width: 0,
            cluster_start: 0,
            prev_cp: None,
            break_state: BreakState::Default,
            width_state: GraphemeWidthState::new(0, 0, method),
            cluster_started: false,
        }
    }

    fn result(&self) -> PosResult {
        PosResult { byte_offset: self.cluster_start as u32, grapheme_count: self.grapheme_count, columns_used: self.columns_used }
    }

    #[inline]
    fn add_code_point(&mut self, cp: u32, cp_width: u32, method: WidthMethod) {
        if !self.cluster_started {
            self.width_state = GraphemeWidthState::new(cp, cp_width, method);
            self.cluster_width = cp_width;
            self.cluster_started = true;
        } else {
            self.width_state.add(cp, cp_width);
            self.cluster_width = self.width_state.width;
        }
        self.prev_cp = Some(cp);
    }
}

/// `findWrapPosByWidth`: the longest grapheme prefix that fits `max_columns` (stops before exceeding it).
pub fn find_wrap_pos_by_width(
    text: &[u8],
    max_columns: u32,
    tab_width: u8,
    control_display: ControlDisplay,
    ascii_only: bool,
    method: WidthMethod,
) -> PosResult {
    match method {
        WidthMethod::Wcwidth => find_wrap_pos_by_width_wcwidth(text, max_columns, tab_width, control_display, ascii_only),
        _ => find_wrap_pos_by_width_unicode(text, max_columns, tab_width, control_display, ascii_only, method),
    }
}

/// `findWrapPosByWidthGraphemeSafe`: never splits a grapheme; wcwidth still sums code point widths per cluster.
pub fn find_wrap_pos_by_width_grapheme_safe(
    text: &[u8],
    max_columns: u32,
    tab_width: u8,
    control_display: ControlDisplay,
    ascii_only: bool,
    method: WidthMethod,
) -> PosResult {
    if method == WidthMethod::Wcwidth {
        return find_wrap_pos_by_width_unicode(text, max_columns, tab_width, control_display, ascii_only, WidthMethod::Wcwidth);
    }
    find_wrap_pos_by_width(text, max_columns, tab_width, control_display, ascii_only, method)
}

fn find_wrap_pos_by_width_unicode(
    text: &[u8],
    max_columns: u32,
    tab_width: u8,
    control_display: ControlDisplay,
    ascii_only: bool,
    method: WidthMethod,
) -> PosResult {
    if text.is_empty() || max_columns == 0 {
        return PosResult::default();
    }
    if ascii_only {
        let n = (max_columns as usize).min(text.len()) as u32;
        return PosResult { byte_offset: n, grapheme_count: n, columns_used: n };
    }
    let mut state = ClusterState::new(method);
    let mut pos = 0;
    while pos < text.len() {
        let b0 = text[pos];
        let (cp, len) = decode(text, pos);
        let is_break = is_grapheme_break(state.prev_cp, cp, &mut state.break_state, method);
        if is_break {
            if state.prev_cp.is_some() {
                if state.columns_used + state.cluster_width > max_columns {
                    return state.result();
                }
                state.columns_used += state.cluster_width;
                state.grapheme_count += 1;
            }
            state.cluster_width = 0;
            state.cluster_start = pos;
            state.cluster_started = false;
        }
        let cp_width = char_width(b0, cp, tab_width, control_display);
        state.add_code_point(cp, cp_width, method);
        pos += len;
    }
    if state.prev_cp.is_some() && state.cluster_width > 0 {
        if state.columns_used + state.cluster_width > max_columns {
            return state.result();
        }
        state.columns_used += state.cluster_width;
        state.grapheme_count += 1;
    }
    PosResult { byte_offset: text.len() as u32, grapheme_count: state.grapheme_count, columns_used: state.columns_used }
}

fn find_wrap_pos_by_width_wcwidth(
    text: &[u8],
    max_columns: u32,
    tab_width: u8,
    control_display: ControlDisplay,
    ascii_only: bool,
) -> PosResult {
    if text.is_empty() || max_columns == 0 {
        return PosResult::default();
    }
    if ascii_only {
        let n = (max_columns as usize).min(text.len()) as u32;
        return PosResult { byte_offset: n, grapheme_count: n, columns_used: n };
    }
    let mut pos = 0;
    let mut columns_used = 0u32;
    let mut count = 0u32;
    while pos < text.len() {
        let b0 = text[pos];
        let (cp, len) = decode_checked(text, pos);
        if pos + len > text.len() {
            break;
        }
        let cp_width = char_width(b0, cp, tab_width, control_display);
        if columns_used >= max_columns || columns_used + cp_width > max_columns {
            return PosResult { byte_offset: pos as u32, grapheme_count: count, columns_used };
        }
        columns_used += cp_width;
        count += 1;
        pos += len;
    }
    PosResult { byte_offset: text.len() as u32, grapheme_count: count, columns_used }
}

/// `findPosByWidth`: snaps `max_columns` to a boundary. With `include_start_before` a grapheme that starts before
/// `max_columns` is included (selection end); without, one that would cross it is excluded (selection start).
pub fn find_pos_by_width(
    text: &[u8],
    max_columns: u32,
    tab_width: u8,
    control_display: ControlDisplay,
    ascii_only: bool,
    include_start_before: bool,
    method: WidthMethod,
) -> PosResult {
    match method {
        WidthMethod::Wcwidth => {
            find_pos_by_width_wcwidth(text, max_columns, tab_width, control_display, ascii_only, include_start_before)
        }
        _ => find_pos_by_width_unicode(text, max_columns, tab_width, control_display, ascii_only, include_start_before, method),
    }
}

/// `findGraphemePosByWidth`: as `find_pos_by_width`, never splitting a Unicode grapheme.
pub fn find_grapheme_pos_by_width(
    text: &[u8],
    max_columns: u32,
    tab_width: u8,
    control_display: ControlDisplay,
    ascii_only: bool,
    include_start_before: bool,
    method: WidthMethod,
) -> PosResult {
    find_pos_by_width_unicode(text, max_columns, tab_width, control_display, ascii_only, include_start_before, method)
}

fn find_pos_by_width_unicode(
    text: &[u8],
    max_columns: u32,
    tab_width: u8,
    control_display: ControlDisplay,
    ascii_only: bool,
    include_start_before: bool,
    method: WidthMethod,
) -> PosResult {
    if ascii_only {
        let n = (max_columns as usize).min(text.len()) as u32;
        return PosResult { byte_offset: n, grapheme_count: n, columns_used: n };
    }
    let mut state = ClusterState::new(method);
    let mut pos = 0;
    while pos < text.len() {
        let b0 = text[pos];
        let (cp, len) = decode(text, pos);
        let is_break = is_grapheme_break(state.prev_cp, cp, &mut state.break_state, method);
        if is_break {
            if state.prev_cp.is_some() {
                let start_col = state.columns_used;
                let end_col = state.columns_used + state.cluster_width;
                if include_start_before {
                    if start_col >= max_columns {
                        return state.result();
                    }
                    state.columns_used = end_col;
                    state.grapheme_count += 1;
                } else {
                    if end_col > max_columns {
                        return state.result();
                    }
                    state.columns_used = end_col;
                }
            }
            state.cluster_width = 0;
            state.cluster_start = pos;
            state.cluster_started = false;
        }
        let cp_width = char_width(b0, cp, tab_width, control_display);
        state.add_code_point(cp, cp_width, method);
        pos += len;
    }
    if state.prev_cp.is_some() && state.cluster_width > 0 {
        if state.columns_used >= max_columns {
            return state.result();
        }
        if !include_start_before && state.columns_used + state.cluster_width > max_columns {
            return state.result();
        }
        state.columns_used += state.cluster_width;
        if include_start_before {
            state.grapheme_count += 1;
        }
    }
    PosResult { byte_offset: text.len() as u32, grapheme_count: state.grapheme_count, columns_used: state.columns_used }
}

fn find_pos_by_width_wcwidth(
    text: &[u8],
    max_columns: u32,
    tab_width: u8,
    control_display: ControlDisplay,
    ascii_only: bool,
    include_start_before: bool,
) -> PosResult {
    if ascii_only {
        let n = (max_columns as usize).min(text.len()) as u32;
        return PosResult { byte_offset: n, grapheme_count: n, columns_used: n };
    }
    let mut pos = 0;
    let mut columns_used = 0u32;
    let mut count = 0u32;
    while pos < text.len() {
        let b0 = text[pos];
        let (cp, len) = decode_checked(text, pos);
        if pos + len > text.len() {
            break;
        }
        let cp_width = char_width(b0, cp, tab_width, control_display);
        let start_col = columns_used;
        let end_col = columns_used + cp_width;
        if include_start_before {
            if start_col >= max_columns {
                return PosResult { byte_offset: pos as u32, grapheme_count: count, columns_used };
            }
        } else if end_col > max_columns {
            return PosResult { byte_offset: pos as u32, grapheme_count: count, columns_used };
        }
        columns_used = end_col;
        count += 1;
        pos += len;
    }
    PosResult { byte_offset: text.len() as u32, grapheme_count: count, columns_used }
}

/// `getWidthAt`: the width of the cursor unit at `byte_offset` (a grapheme, or a code point under wcwidth).
pub fn get_width_at(text: &[u8], byte_offset: usize, tab_width: u8, control_display: ControlDisplay, method: WidthMethod) -> u32 {
    match method {
        WidthMethod::Wcwidth => get_width_at_wcwidth(text, byte_offset, tab_width, control_display),
        _ => get_width_at_unicode(text, byte_offset, tab_width, control_display, method),
    }
}

/// `getGraphemeWidthAt`: the width of the Unicode grapheme at `byte_offset`.
pub fn get_grapheme_width_at(
    text: &[u8],
    byte_offset: usize,
    tab_width: u8,
    control_display: ControlDisplay,
    method: WidthMethod,
) -> u32 {
    get_width_at_unicode(text, byte_offset, tab_width, control_display, method)
}

fn get_width_at_unicode(text: &[u8], byte_offset: usize, tab_width: u8, control_display: ControlDisplay, method: WidthMethod) -> u32 {
    if byte_offset >= text.len() {
        return 0;
    }
    let b0 = text[byte_offset];
    let (first_cp, first_len) = decode(text, byte_offset);
    if b0 >= 0x80 && byte_offset + first_len > text.len() {
        return 1;
    }
    let mut break_state = BreakState::Default;
    let mut prev = first_cp;
    let mut state = GraphemeWidthState::new(first_cp, char_width(b0, first_cp, tab_width, control_display), method);
    let mut pos = byte_offset + first_len;
    while pos < text.len() {
        let b = text[pos];
        let (cp, len) = decode(text, pos);
        if pos + len > text.len() {
            break;
        }
        if is_grapheme_break(Some(prev), cp, &mut break_state, method) {
            break;
        }
        state.add(cp, char_width(b, cp, tab_width, control_display));
        prev = cp;
        pos += len;
    }
    state.width
}

fn get_width_at_wcwidth(text: &[u8], byte_offset: usize, tab_width: u8, control_display: ControlDisplay) -> u32 {
    if byte_offset >= text.len() {
        return 0;
    }
    let b0 = text[byte_offset];
    let (cp, len) = decode(text, byte_offset);
    if b0 >= 0x80 && byte_offset + len > text.len() {
        return 1;
    }
    char_width(b0, cp, tab_width, control_display)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PrevGrapheme {
    pub start_offset: usize,
    pub width: u32,
}

/// `getPrevGraphemeStart`: the last unit before `byte_offset` that takes cells.
pub fn get_prev_grapheme_start(
    text: &[u8],
    byte_offset: usize,
    tab_width: u8,
    control_display: ControlDisplay,
    method: WidthMethod,
) -> Option<PrevGrapheme> {
    match method {
        WidthMethod::Wcwidth => get_prev_grapheme_start_wcwidth(text, byte_offset, tab_width, control_display),
        _ => get_prev_grapheme_start_unicode(text, byte_offset, tab_width, control_display, method),
    }
}

fn get_prev_grapheme_start_wcwidth(text: &[u8], byte_offset: usize, tab_width: u8, control_display: ControlDisplay) -> Option<PrevGrapheme> {
    if byte_offset == 0 || text.is_empty() || byte_offset > text.len() {
        return None;
    }
    let mut pos = 0;
    let mut last = None;
    while pos < byte_offset {
        let b = text[pos];
        let (cp, len) = decode_checked(text, pos);
        let w = char_width(b, cp, tab_width, control_display);
        if w > 0 {
            last = Some(PrevGrapheme { start_offset: pos, width: w });
        }
        pos += len;
    }
    last
}

fn get_prev_grapheme_start_unicode(
    text: &[u8],
    byte_offset: usize,
    tab_width: u8,
    control_display: ControlDisplay,
    method: WidthMethod,
) -> Option<PrevGrapheme> {
    if byte_offset == 0 || text.is_empty() || byte_offset > text.len() {
        return None;
    }
    let last = text[byte_offset - 1];
    let before = if byte_offset > 1 { text[byte_offset - 2] } else { 0 };
    if last < 0x80 && before < 0x80 {
        let width = ascii_char_width(last, tab_width, control_display);
        if width > 0 {
            return Some(PrevGrapheme { start_offset: byte_offset - 1, width });
        }
    }
    let last_start = last_grapheme_start(text, byte_offset, method);
    let last_width = get_width_at(text, last_start, tab_width, control_display, method);
    if last_width > 0 {
        return Some(PrevGrapheme { start_offset: last_start, width: last_width });
    }
    let mut break_state = BreakState::Default;
    let mut pos = 0;
    let mut prev = None;
    let mut grapheme_start = 0;
    let mut width_state = GraphemeWidthState::new(0, 0, method);
    let mut result = None;
    while pos < byte_offset {
        let b = text[pos];
        let (cp, len) = decode_checked(text, pos);
        let cp_width = char_width(b, cp, tab_width, control_display);
        if is_grapheme_break(prev, cp, &mut break_state, method) {
            if width_state.width > 0 {
                result = Some(PrevGrapheme { start_offset: grapheme_start, width: width_state.width });
            }
            grapheme_start = pos;
            width_state = GraphemeWidthState::new(cp, cp_width, method);
        } else {
            width_state.add(cp, cp_width);
        }
        prev = Some(cp);
        pos += len;
    }
    result
}

fn last_grapheme_start(text: &[u8], byte_offset: usize, method: WidthMethod) -> usize {
    let mut break_state = BreakState::Default;
    let mut pos = 0;
    let mut prev = None;
    let mut start = 0;
    while pos < byte_offset {
        let (cp, len) = decode(text, pos);
        if is_grapheme_break(prev, cp, &mut break_state, method) {
            start = pos;
        }
        prev = Some(cp);
        pos += len;
    }
    start
}

/// `calculateTextWidth`: the display width of `text`.
pub fn calculate_text_width(text: &[u8], tab_width: u8, control_display: ControlDisplay, ascii_only: bool, method: WidthMethod) -> u32 {
    if text.is_empty() {
        return 0;
    }
    if ascii_only {
        return text.len() as u32;
    }
    if method == WidthMethod::Wcwidth {
        let mut total = 0;
        let mut pos = 0;
        while pos < text.len() {
            let b0 = text[pos];
            let (cp, len) = decode_checked(text, pos);
            total += char_width(b0, cp, tab_width, control_display);
            pos += len;
        }
        return total;
    }
    let mut total = 0;
    let mut pos = 0;
    let mut prev = None;
    let mut break_state = BreakState::Default;
    let mut state = GraphemeWidthState::new(0, 0, method);
    while pos < text.len() {
        let b0 = text[pos];
        let (cp, len) = decode_checked(text, pos);
        let cp_width = char_width(b0, cp, tab_width, control_display);
        if is_grapheme_break(prev, cp, &mut break_state, method) {
            if prev.is_some() {
                total += state.width;
            }
            state = GraphemeWidthState::new(cp, cp_width, method);
        } else {
            state.add(cp, cp_width);
        }
        prev = Some(cp);
        pos += len;
    }
    if prev.is_some() {
        total += state.width;
    }
    total
}

/// `TextWidthCursor`: measures increasing byte boundaries of one text in one pass; a boundary inside a grapheme
/// includes that whole grapheme.
pub struct TextWidthCursor<'a> {
    pub text: &'a [u8],
    pub tab_width: u8,
    pub control_display: ControlDisplay,
    pub method: WidthMethod,
    pub byte_offset: usize,
    pub columns: u32,
    break_state: BreakState,
}

impl<'a> TextWidthCursor<'a> {
    pub fn new(text: &'a [u8], tab_width: u8, control_display: ControlDisplay, method: WidthMethod) -> Self {
        TextWidthCursor { text, tab_width, control_display, method, byte_offset: 0, columns: 0, break_state: BreakState::Default }
    }

    pub fn advance_to(&mut self, byte_end: usize) -> u32 {
        let text = self.text;
        let end = byte_end.min(text.len());
        if end <= self.byte_offset {
            return self.columns;
        }
        if (end == text.len() || text[end] < 0x80) && is_ascii_only(&text[self.byte_offset..end]) {
            self.columns += (end - self.byte_offset) as u32;
            self.byte_offset = end;
            self.break_state = BreakState::Default;
            return self.columns;
        }
        while self.byte_offset < end {
            let b0 = text[self.byte_offset];
            if b0 < 0x80 && (self.byte_offset + 1 == text.len() || text[self.byte_offset + 1] < 0x80) {
                self.columns += ascii_char_width(b0, self.tab_width, self.control_display);
                self.byte_offset += 1;
                self.break_state = BreakState::Default;
                continue;
            }
            let (first_cp, first_len) = decode(text, self.byte_offset);
            let mut previous = if self.byte_offset + first_len <= text.len() { first_cp } else { 0xFFFD };
            let mut state =
                GraphemeWidthState::new(previous, char_width(b0, previous, self.tab_width, self.control_display), self.method);
            self.byte_offset += first_len.min(text.len() - self.byte_offset);
            while self.byte_offset < text.len() {
                let byte = text[self.byte_offset];
                let (cp, len) = decode(text, self.byte_offset);
                let cp = if self.byte_offset + len <= text.len() { cp } else { 0xFFFD };
                if is_grapheme_break(Some(previous), cp, &mut self.break_state, self.method) {
                    break;
                }
                state.add(cp, char_width(byte, cp, self.tab_width, self.control_display));
                self.byte_offset += len.min(text.len() - self.byte_offset);
                previous = cp;
            }
            self.columns += state.width;
        }
        self.columns
    }
}

/// Sparse render-cluster metadata: a multibyte grapheme or a tab, with its cell-column start.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RenderClusterInfo {
    pub byte_start: u32,
    pub byte_len: u32,
    pub width_cols: u32,
    pub col_start: u32,
}

/// `findRenderClusterInfo`: the multibyte clusters and tabs of `text`.
pub fn find_render_cluster_info(
    text: &[u8],
    tab_width: u8,
    control_display: ControlDisplay,
    ascii_only: bool,
    method: WidthMethod,
    out: &mut Vec<RenderClusterInfo>,
) {
    if ascii_only || text.is_empty() {
        return;
    }
    let wcwidth = method == WidthMethod::Wcwidth;
    let mut pos = 0;
    let mut col = 0u32;
    let mut prev: Option<u32> = None;
    let mut break_state = BreakState::Default;
    let mut cluster_start = 0usize;
    let mut cluster_start_col = 0u32;
    let mut state = GraphemeWidthState::new(0, 0, method);
    let mut multibyte = false;
    let mut tab = false;
    let emit = |out: &mut Vec<RenderClusterInfo>, start: usize, end: usize, start_col: u32, state: &GraphemeWidthState| {
        if state.width > 0 || wcwidth {
            out.push(RenderClusterInfo {
                byte_start: start as u32,
                byte_len: (end - start) as u32,
                width_cols: state.width,
                col_start: start_col,
            });
        }
    };
    while pos < text.len() {
        let b0 = text[pos];
        let (cp, len) = if wcwidth { decode_checked(text, pos) } else { decode(text, pos) };
        if pos + len > text.len() {
            break;
        }
        if is_grapheme_break(prev, cp, &mut break_state, method) {
            if prev.is_some() {
                if multibyte || tab {
                    emit(out, cluster_start, pos, cluster_start_col, &state);
                }
                col += state.width;
            }
            cluster_start = pos;
            cluster_start_col = col;
            tab = b0 == b'\t';
            multibyte = len != 1;
            state = GraphemeWidthState::new(cp, char_width(b0, cp, tab_width, control_display), method);
        } else {
            multibyte = multibyte || len != 1;
            state.add(cp, char_width(b0, cp, tab_width, control_display));
        }
        prev = Some(cp);
        pos += len;
    }
    if prev.is_some() && (multibyte || tab) {
        emit(out, cluster_start, text.len(), cluster_start_col, &state);
    }
}

/// Word-class endpoints of a chunk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WordClassEdges {
    pub first: WordClass,
    pub last: WordClass,
    pub has_cjk_breaks: bool,
}

/// `chunkWordClassEdges`: the classes of the first and last code points.
pub fn chunk_word_class_edges(text: &[u8]) -> WordClassEdges {
    if text.is_empty() {
        return WordClassEdges { first: WordClass::Other, last: WordClass::Other, has_cjk_breaks: false };
    }
    let first = decode(text, 0).0;
    let mut last_start = text.len() - 1;
    let mut continuation = 0;
    while last_start > 0 && text[last_start] & 0xC0 == 0x80 && continuation < 3 {
        last_start -= 1;
        continuation += 1;
    }
    let last = decode(text, last_start).0;
    WordClassEdges { first: classify_word_class(first), last: classify_word_class(last), has_cjk_breaks: false }
}

/// Receives the wrap opportunities of `walk_chunk_layout_info`; returning false stops the walk.
pub trait LayoutVisitor {
    /// Word motion: line-only CJK opportunities are not emitted.
    const WORD_ONLY: bool;
    fn emit(&mut self, wrap_break: LayoutWrapBreak) -> bool;
}

/// Collects every wrap opportunity (`findChunkLayoutInfo`) or the word boundaries only (`findWordChunkLayoutInfo`).
pub struct CollectBreaks<'a, const WORD_ONLY: bool>(pub &'a mut Vec<LayoutWrapBreak>);

impl<const W: bool> LayoutVisitor for CollectBreaks<'_, W> {
    const WORD_ONLY: bool = W;
    fn emit(&mut self, wrap_break: LayoutWrapBreak) -> bool {
        self.0.push(wrap_break);
        true
    }
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn commit_layout_cluster<V: LayoutVisitor>(
    visitor: &mut V,
    cluster_start: usize,
    cluster_end: usize,
    col_offset: u32,
    width: u32,
    break_kind: LayoutWrapBreakKind,
    class: WordClass,
    next_class: Option<WordClass>,
    next_cp: u32,
    has_cjk_breaks: &mut bool,
) -> bool {
    let kind = if break_kind != LayoutWrapBreakKind::None {
        break_kind
    } else if let Some(next) = next_class {
        if is_cjk_ascii_transition(class, next) {
            LayoutWrapBreakKind::ScriptTransition
        } else if width > 0 && is_cjk_intercharacter_break(class, next, next_cp) {
            *has_cjk_breaks = true;
            if V::WORD_ONLY { LayoutWrapBreakKind::None } else { LayoutWrapBreakKind::CjkIntercharacter }
        } else {
            LayoutWrapBreakKind::None
        }
    } else {
        LayoutWrapBreakKind::None
    };
    if kind != LayoutWrapBreakKind::None {
        return visitor.emit(LayoutWrapBreak {
            byte_start: cluster_start as u32,
            col_start: col_offset,
            byte_len: (cluster_end - cluster_start) as u32,
            width_cols: width,
            kind,
        });
    }
    true
}

/// `walkChunkLayoutInfoGeneric`: the wrap opportunities of a chunk, in byte order.
pub fn walk_chunk_layout_info<V: LayoutVisitor>(
    text: &[u8],
    tab_width: u8,
    control_display: ControlDisplay,
    ascii_only: bool,
    method: WidthMethod,
    visitor: &mut V,
) -> WordClassEdges {
    if text.is_empty() {
        return WordClassEdges { first: WordClass::Other, last: WordClass::Other, has_cjk_breaks: false };
    }
    if ascii_only {
        for (pos, &b) in text.iter().enumerate() {
            if is_ascii_wrap_break(b) {
                let kind = ascii_layout_wrap_break_kind(b);
                if !visitor.emit(LayoutWrapBreak { byte_start: pos as u32, col_start: pos as u32, byte_len: 1, width_cols: 1, kind }) {
                    break;
                }
            }
        }
        return chunk_word_class_edges(text);
    }

    let first_word_class = classify_word_class(decode(text, 0).0);
    let mut has_cjk_breaks = false;
    const VECTOR_LEN: usize = 16;
    let mut pos = 0usize;
    let mut col = 0u32;
    let mut prev: Option<u32> = None;
    let mut break_state = BreakState::Default;
    let mut cluster_started = false;
    let mut cluster_start = 0usize;
    let mut cluster_col_offset = 0u32;
    let mut cluster_state = GraphemeWidthState::new(0, 0, method);
    let mut cluster_break_kind = LayoutWrapBreakKind::None;
    let mut cluster_class = WordClass::Other;

    while pos < text.len() {
        let ascii_len = if pos + VECTOR_LEN <= text.len() {
            text[pos..pos + VECTOR_LEN].iter().position(|&b| b >= 0x80).unwrap_or(VECTOR_LEN)
        } else {
            text[pos..].iter().position(|&b| b >= 0x80).unwrap_or(text.len() - pos)
        };
        if ascii_len > 1 {
            let mut fast = true;
            if cluster_started {
                let mut probe = break_state;
                fast = is_grapheme_break(prev, text[pos] as u32, &mut probe, method);
            }
            if fast {
                let first_class = classify_word_class(text[pos] as u32);
                if cluster_started {
                    if !commit_layout_cluster(
                        visitor,
                        cluster_start,
                        pos,
                        cluster_col_offset,
                        cluster_state.width,
                        cluster_break_kind,
                        cluster_class,
                        Some(first_class),
                        text[pos] as u32,
                        &mut has_cjk_breaks,
                    ) {
                        return chunk_word_class_edges(text);
                    }
                    col += cluster_state.width;
                }
                for i in 0..ascii_len - 1 {
                    let b = text[pos + i];
                    let width = ascii_char_width(b, tab_width, control_display);
                    if is_ascii_wrap_break(b)
                        && !visitor.emit(LayoutWrapBreak {
                            byte_start: (pos + i) as u32,
                            col_start: col,
                            byte_len: 1,
                            width_cols: width,
                            kind: ascii_layout_wrap_break_kind(b),
                        })
                    {
                        return chunk_word_class_edges(text);
                    }
                    col += width;
                }
                let last_b = text[pos + ascii_len - 1];
                cluster_started = true;
                cluster_start = pos + ascii_len - 1;
                cluster_col_offset = col;
                cluster_state = GraphemeWidthState::new(last_b as u32, ascii_char_width(last_b, tab_width, control_display), method);
                cluster_break_kind = ascii_layout_wrap_break_kind(last_b);
                cluster_class = classify_word_class(last_b as u32);
                prev = Some(last_b as u32);
                break_state = BreakState::Default;
                pos += ascii_len;
                continue;
            }
        }

        let b0 = text[pos];
        let (cp, len) = decode(text, pos);
        let curr_class = classify_word_class(cp);
        let is_break = is_grapheme_break(prev, cp, &mut break_state, method);
        let cp_width = char_width(b0, cp, tab_width, control_display);
        let kind = if b0 < 0x80 { ascii_layout_wrap_break_kind(b0) } else { unicode_layout_wrap_break_kind(cp) };
        if is_break {
            if cluster_started {
                if !commit_layout_cluster(
                    visitor,
                    cluster_start,
                    pos,
                    cluster_col_offset,
                    cluster_state.width,
                    cluster_break_kind,
                    cluster_class,
                    Some(curr_class),
                    cp,
                    &mut has_cjk_breaks,
                ) {
                    return chunk_word_class_edges(text);
                }
                col += cluster_state.width;
            }
            cluster_started = true;
            cluster_start = pos;
            cluster_col_offset = col;
            cluster_state = GraphemeWidthState::new(cp, cp_width, method);
            cluster_break_kind = kind;
            cluster_class = curr_class;
        } else {
            cluster_state.add(cp, cp_width);
            if cluster_break_kind == LayoutWrapBreakKind::Whitespace || kind == LayoutWrapBreakKind::Whitespace {
                cluster_break_kind = if cluster_break_kind == LayoutWrapBreakKind::Whitespace && kind == LayoutWrapBreakKind::Whitespace {
                    LayoutWrapBreakKind::Whitespace
                } else {
                    LayoutWrapBreakKind::PreservedWhitespace
                };
            } else if cluster_break_kind == LayoutWrapBreakKind::None {
                cluster_break_kind = kind;
            }
        }
        prev = Some(cp);
        pos += len;
    }
    if cluster_started {
        commit_layout_cluster(
            visitor,
            cluster_start,
            text.len(),
            cluster_col_offset,
            cluster_state.width,
            cluster_break_kind,
            cluster_class,
            None,
            0,
            &mut has_cjk_breaks,
        );
    }
    WordClassEdges {
        first: first_word_class,
        last: if cluster_started { cluster_class } else { WordClass::Other },
        has_cjk_breaks,
    }
}
