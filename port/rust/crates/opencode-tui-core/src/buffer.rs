//! The cell buffer: `buffer.zig`'s `OptimizedBuffer` behind `buffer.ts`'s API (and the checks
//! `context.zig`'s `drawBufferOn` applies to each TS draw call).
//!
//! A cell is a `char` (a code point, or a cluster char naming pooled bytes, see [`crate::pool`]), a foreground
//! and background [`RGBA`] and attributes (base bits plus a link id). Drawing goes through the scissor stack
//! (clip rects intersect their parent) and the opacity stack (opacities multiply); colors with alpha blend
//! over the cell below them exactly as `blendCells` does in 8-bit integer arithmetic, and a translucent space
//! keeps the glyph below it.
//!
//! Text drawing is the checked path TS uses (`drawTextFitted`): printable ASCII is one cell per byte; other
//! text is segmented into clusters, a cluster wider than one cell spans continuation cells, a cluster a cell
//! cannot hold (tabs, controls, over 128 bytes or 16 cells) draws as spaces, and a glyph only draws when all of
//! its cells are inside the scissor and the buffer.
//!
//! Divergences:
//! - Images (placements, image cells, sixel/kitty fallbacks) are not ported: the TUI draws none. The image
//!   branches of the Zig paths are the no-image branches here.
//! - Invalid input that the native API rejects with an error (non-base attribute bits, malformed colors) is
//!   ignored instead: the caller is Rust code that only builds valid values.
//! - `drawTextBuffer` and `drawEditorView` (the text crate's views) are not here yet: they belong next to
//!   [`OptimizedBuffer::draw_text`] once `opencode-tui-text` lands (TODO(port-tui): opencode-tui-text).
//! - Cluster segmentation and widths come from [`crate::unicode`], a stand-in for the text crate's port of
//!   utf8.zig.
use crate::border::{BorderSides, BorderStyle, TitleAlignment, border_char_index as bci};
use crate::color::{ATTRIBUTE_BASE_MASK, RGBA, base_attributes, get_link_id, attributes_with_link};
use crate::pool::{self, Tracker};
use crate::unicode::{self, WidthMethod};

/// The space every cleared cell holds (`DEFAULT_SPACE_CHAR`).
pub const DEFAULT_SPACE_CHAR: u32 = 32;
/// Bytes one checked text draw accepts (`OT_BUFFER_TEXT_BYTES_MAX`).
pub const TEXT_BYTES_MAX: usize = 65536;
const TAB_WIDTH: u32 = 2;
const ELLIPSIS: &str = "\u{2026}";
const TRANSPARENT: RGBA = RGBA::rgb(0, 0, 0, 0);
const WHITE: RGBA = RGBA::rgb(255, 255, 255, 255);

/// One cell (`Cell`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub char: u32,
    pub fg: RGBA,
    pub bg: RGBA,
    pub attributes: u32,
}

/// A scissor rectangle (`ClipRect`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClipRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The selection of [`OptimizedBuffer::draw_text`]: UTF-16 offsets into the text, as in TS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextSelection {
    pub start: usize,
    pub end: usize,
    pub bg_color: Option<RGBA>,
    pub fg_color: Option<RGBA>,
}

/// The options of [`OptimizedBuffer::draw_box`] (`buffer.ts` `drawBox`).
#[derive(Clone, Debug, PartialEq)]
pub struct DrawBoxOptions<'a> {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub border_style: BorderStyle,
    pub custom_border_chars: Option<[u32; 11]>,
    pub border: BorderSides,
    pub border_color: RGBA,
    pub background_color: RGBA,
    pub should_fill: bool,
    pub title: Option<&'a str>,
    /// Defaults to the border color.
    pub title_color: Option<RGBA>,
    pub title_alignment: TitleAlignment,
    pub bottom_title: Option<&'a str>,
    pub bottom_title_alignment: TitleAlignment,
}

/// `isPrintableGlyph`: whether one cell may hold a cluster this wide.
pub fn is_printable_glyph(bytes: &[u8], width: u32) -> bool {
    if bytes.len() > pool::GRAPHEME_BYTES_MAX || width > pool::GLYPH_WIDTH_MAX {
        return false;
    }
    for (index, &byte) in bytes.iter().enumerate() {
        if byte < 0x20 || byte == 0x7f {
            return false;
        }
        if byte == 0xc2 && index + 1 < bytes.len() && bytes[index + 1] < 0xa0 {
            return false;
        }
    }
    true
}

/// `isControlCodepoint`.
pub fn is_control_codepoint(codepoint: u32) -> bool {
    codepoint < 0x20 || (0x7f..=0x9f).contains(&codepoint)
}

fn has_alpha(color: RGBA) -> bool {
    color.alpha() < 255
}

fn is_fully_opaque(opacity: f32, fg: RGBA, bg: RGBA) -> bool {
    opacity == 1.0 && !has_alpha(fg) && !has_alpha(bg)
}

fn is_fully_transparent(opacity: f32, fg: RGBA, bg: RGBA) -> bool {
    opacity == 0.0 || (fg.alpha() == 0 && bg.alpha() == 0)
}

fn mul_div_255(a: u32, b: u32) -> u32 {
    (a * b + 127) / 255
}

fn round_div(n: u32, d: u32) -> u16 {
    ((n + d / 2) / d) as u16
}

/// `blendColors`: `src` over `dst0` (or over the backdrop where `dst0` is transparent).
fn blend_colors(src: RGBA, dst0: RGBA, backdrop: Option<RGBA>) -> RGBA {
    let sa = src.alpha() as u32;
    if sa == 0 {
        return dst0;
    }
    let dst = if dst0.alpha() == 0 { backdrop.unwrap_or(dst0) } else { dst0 };
    if sa == 255 {
        return RGBA::rgb(src.red(), src.green(), src.blue(), 255);
    }
    let da = dst.alpha() as u32;
    let inv = 255 - sa;
    let out_a = sa + mul_div_255(da, inv);
    if out_a == 0 {
        return RGBA::rgb(0, 0, 0, 0);
    }
    if da == 255 {
        let channel = |s: u8, d: u8| ((s as u32 * sa + d as u32 * inv + 127) / 255) as u8;
        return RGBA::rgb(channel(src.red(), dst.red()), channel(src.green(), dst.green()), channel(src.blue(), dst.blue()), 255);
    }
    // TODO(port): a TS (native) bug kept as is. Over a nearly transparent destination a channel can round to 256
    // (`roundDiv` casts it to u8); the reference's release build keeps the 256 in the color's u16 lane, so the
    // channel reads 0 and its metadata byte 1. The lanes are built from the unclamped values to match.
    let channel = |s: u8, d: u8| round_div(s as u32 * sa + mul_div_255(d as u32 * da, inv), out_a);
    RGBA([channel(src.red(), dst.red()), channel(src.green(), dst.green()), channel(src.blue(), dst.blue()), out_a as u16])
}

/// `rgbaComponentToU8` on an f32 (round half away from zero).
fn opacity_to_u8(opacity: f32) -> u8 {
    if !opacity.is_finite() {
        return 0;
    }
    (opacity.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// `applyOpacity`: the alpha scaled, the metadata kept.
fn apply_opacity(color: RGBA, opacity: u8) -> RGBA {
    RGBA::pack(color.red(), color.green(), color.blue(), mul_div_255(color.alpha() as u32, opacity as u32) as u8, color.meta())
}

pub(crate) fn make_cell(char: u32, fg: RGBA, bg: RGBA, attributes: u32) -> Cell {
    Cell { char, fg, bg, attributes }
}

/// The cell buffer (`OptimizedBuffer`).
pub struct OptimizedBuffer {
    width: u32,
    height: u32,
    chars: Vec<u32>,
    fg: Vec<RGBA>,
    bg: Vec<RGBA>,
    attributes: Vec<u32>,
    respect_alpha: bool,
    blend_backdrop_color: Option<RGBA>,
    width_method: WidthMethod,
    graphemes: Tracker,
    links: Tracker,
    scissor_stack: Vec<ClipRect>,
    opacity_stack: Vec<f32>,
    /// The buffer's id (`fb_<n>` or a caller's).
    pub id: String,
}

impl std::fmt::Debug for OptimizedBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OptimizedBuffer({} {}x{})", self.id, self.width, self.height)
    }
}

/// One glyph of a text row with at least one cell to draw (`TextGlyph`).
struct TextGlyph {
    start: usize,
    end: usize,
    x: u32,
    width: u32,
    blank: bool,
    count: u32,
}

/// Places the glyphs of one text row (`TextRow`).
struct TextRow<'a> {
    target: &'a OptimizedBuffer,
    text: &'a str,
    clusters: Vec<unicode::Cluster>,
    x: i64,
    y: i32,
    end: i64,
    index: usize,
    advance: u64,
}

impl<'a> TextRow<'a> {
    fn new(target: &'a OptimizedBuffer, text: &'a str, x: i32, y: i32, end: i64) -> Self {
        let clusters = unicode::clusters(text, TAB_WIDTH, target.width_method);
        TextRow { target, text, clusters, x: x as i64, y, end, index: 0, advance: 0 }
    }

    /// The next glyph with at least one cell: `(start, end, x, width)`.
    fn next_glyph(&mut self) -> Option<(usize, usize, i64, u32)> {
        while self.index < self.clusters.len() {
            let cluster = self.clusters[self.index];
            self.index += 1;
            if cluster.width == 0 {
                continue;
            }
            let x = self.x + self.advance as i64;
            self.advance += cluster.width as u64;
            return Some((cluster.byte_start, cluster.byte_start + cluster.byte_len, x, cluster.width));
        }
        None
    }

    fn next(&mut self) -> Option<TextGlyph> {
        while self.x + (self.advance as i64) < self.end {
            let (start, end_byte, x, width) = self.next_glyph()?;
            let blank = !is_printable_glyph(&self.text.as_bytes()[start..end_byte], width);
            if !blank && x < 0 {
                continue;
            }
            let first = x.max(0) as u32;
            let end = (x + width as i64).min(self.end);
            let count = if !blank {
                1
            } else if end > first as i64 {
                (end - first as i64) as u32
            } else {
                0
            };
            if !self.is_visible(first, if blank { count } else { width }, blank) {
                continue;
            }
            return Some(TextGlyph { start, end: end_byte, x: first, width, blank, count });
        }
        None
    }

    /// The byte length of the glyphs that end at most `room` cells right of x, or `None` when the whole row is
    /// at most `max_width` cells wide (`fitBytes`).
    fn fit_bytes(&mut self, room: u64, max_width: u64) -> Option<usize> {
        let mut kept = None;
        while self.advance <= max_width {
            let (start, ..) = self.next_glyph()?;
            if kept.is_none() && self.advance > room {
                kept = Some(start);
            }
        }
        kept
    }

    fn is_visible(&self, first: u32, cells: u32, blank: bool) -> bool {
        if first as i64 + cells as i64 > self.end {
            return false;
        }
        for offset in 0..cells {
            let inside = self.target.is_point_in_scissor((first + offset) as i32, self.y);
            if blank && inside {
                return true;
            }
            if !blank && !inside {
                return false;
            }
        }
        !blank
    }
}

/// `BoxTitleLayout`.
#[derive(Default)]
struct TitleLayout {
    draw: bool,
    start_x: i32,
    end_x: i32,
}

static NEXT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl OptimizedBuffer {
    /// `OptimizedBuffer.create`: every cell zero (char 0, transparent colors), as Zig's `init` leaves them.
    /// A zero dimension is clamped to one cell (the native constructor rejects it).
    pub fn new(width: u32, height: u32, respect_alpha: bool, width_method: WidthMethod) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let size = (width * height) as usize;
        OptimizedBuffer {
            width,
            height,
            chars: vec![0; size],
            fg: vec![TRANSPARENT; size],
            bg: vec![TRANSPARENT; size],
            attributes: vec![0; size],
            respect_alpha,
            blend_backdrop_color: None,
            width_method,
            graphemes: Tracker::graphemes(),
            links: Tracker::links(),
            scissor_stack: Vec::new(),
            opacity_stack: Vec::new(),
            id: format!("fb_{}", NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)),
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn width_method(&self) -> WidthMethod {
        self.width_method
    }

    pub fn set_width_method(&mut self, method: WidthMethod) {
        self.width_method = method;
    }

    pub fn respect_alpha(&self) -> bool {
        self.respect_alpha
    }

    /// `setRespectAlpha`: whether [`Self::draw_frame_buffer`] blends this buffer's cells when it is the source.
    pub fn set_respect_alpha(&mut self, respect_alpha: bool) {
        self.respect_alpha = respect_alpha;
    }

    pub fn blend_backdrop_color(&self) -> Option<RGBA> {
        self.blend_backdrop_color
    }

    /// `setBlendBackdropColor`: what a translucent color blends over where the cell below is transparent.
    pub fn set_blend_backdrop_color(&mut self, color: Option<RGBA>) {
        self.blend_backdrop_color = color;
    }

    /// The char plane (row-major).
    pub fn chars(&self) -> &[u32] {
        &self.chars
    }
    pub fn fg(&self) -> &[RGBA] {
        &self.fg
    }
    pub fn bg(&self) -> &[RGBA] {
        &self.bg
    }
    pub fn attributes(&self) -> &[u32] {
        &self.attributes
    }

    /// Whether any cell holds a cluster char (`grapheme_tracker.hasAny()`).
    pub fn has_graphemes(&self) -> bool {
        self.graphemes.has_any()
    }

    /// Whether any cell holds a link (`link_tracker.hasAny()`).
    pub fn has_links(&self) -> bool {
        self.links.has_any()
    }

    /// The cell at a position (`get`).
    pub fn get(&self, x: u32, y: u32) -> Option<Cell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = self.index(x, y);
        Some(Cell { char: self.chars[i], fg: self.fg[i], bg: self.bg[i], attributes: self.attributes[i] })
    }

    fn index(&self, x: u32, y: u32) -> usize {
        (y * self.width + x) as usize
    }

    // ---- scissor and opacity stacks ----

    pub fn current_scissor_rect(&self) -> Option<ClipRect> {
        self.scissor_stack.last().copied()
    }

    pub fn is_point_in_scissor(&self, x: i32, y: i32) -> bool {
        let Some(s) = self.current_scissor_rect() else { return true };
        x >= s.x && (x as i64) < s.x as i64 + s.width as i64 && y >= s.y && (y as i64) < s.y as i64 + s.height as i64
    }

    fn is_cell_in_buffer_and_scissor(&self, x: u32, y: u32) -> bool {
        x < self.width && y < self.height && self.is_point_in_scissor(x as i32, y as i32)
    }

    pub fn is_rect_in_scissor(&self, x: i32, y: i32, width: u32, height: u32) -> bool {
        let Some(s) = self.current_scissor_rect() else { return true };
        let rect_end_x = x as i64 + width as i64;
        let rect_end_y = y as i64 + height as i64;
        let scissor_end_x = s.x as i64 + s.width as i64;
        let scissor_end_y = s.y as i64 + s.height as i64;
        !(x as i64 >= scissor_end_x || rect_end_x <= s.x as i64 || y as i64 >= scissor_end_y || rect_end_y <= s.y as i64)
    }

    pub fn clip_rect_to_scissor(&self, x: i32, y: i32, width: u32, height: u32) -> Option<ClipRect> {
        let Some(s) = self.current_scissor_rect() else { return Some(ClipRect { x, y, width, height }) };
        let ix = (x as i64).max(s.x as i64);
        let iy = (y as i64).max(s.y as i64);
        let iex = (x as i64 + width as i64).min(s.x as i64 + s.width as i64);
        let iey = (y as i64 + height as i64).min(s.y as i64 + s.height as i64);
        if ix >= iex || iy >= iey {
            return None;
        }
        Some(ClipRect { x: ix as i32, y: iy as i32, width: (iex - ix) as u32, height: (iey - iy) as u32 })
    }

    /// `pushScissorRect`: intersected with the current one (a non-positive extent is an empty rect).
    pub fn push_scissor_rect(&mut self, x: i32, y: i32, width: i32, height: i32) {
        let mut rect = ClipRect { x, y, width: width.max(0) as u32, height: height.max(0) as u32 };
        if self.current_scissor_rect().is_some() {
            rect = self.clip_rect_to_scissor(rect.x, rect.y, rect.width, rect.height).unwrap_or(ClipRect { x: 0, y: 0, width: 0, height: 0 });
        }
        self.scissor_stack.push(rect);
    }

    pub fn pop_scissor_rect(&mut self) {
        self.scissor_stack.pop();
    }

    pub fn clear_scissor_rects(&mut self) {
        self.scissor_stack.clear();
    }

    /// `getCurrentOpacity`: the product of the stacked opacities.
    pub fn current_opacity(&self) -> f32 {
        self.opacity_stack.last().copied().unwrap_or(1.0)
    }

    /// `pushOpacity`: multiplied with the current opacity.
    pub fn push_opacity(&mut self, opacity: f32) {
        let effective = self.current_opacity() * opacity.clamp(0.0, 1.0);
        self.opacity_stack.push(effective);
    }

    pub fn pop_opacity(&mut self) {
        self.opacity_stack.pop();
    }

    pub fn clear_opacity(&mut self) {
        self.opacity_stack.clear();
    }

    // ---- whole-buffer operations ----

    /// `clear` (TS defaults `bg` to opaque black): every cell a space with white foreground.
    pub fn clear(&mut self, bg: RGBA) {
        self.clear_with(bg, None);
    }

    /// `clear` with a fill char (`clear(bg, char)` natively).
    pub fn clear_with(&mut self, bg: RGBA, char: Option<u32>) {
        self.links.clear();
        self.graphemes.clear();
        self.chars.fill(char.unwrap_or(DEFAULT_SPACE_CHAR));
        self.attributes.fill(0);
        self.fg.fill(WHITE);
        self.bg.fill(bg);
    }

    /// `clearRows`: rows `[top, bottom)` as [`Self::clear`] does.
    pub fn clear_rows(&mut self, top: u32, bottom: u32, bg: RGBA, char: Option<u32>) {
        let start = (top * self.width) as usize;
        let end = (bottom.min(self.height) * self.width) as usize;
        for i in start..end {
            let cell = self.chars[i];
            if pool::is_grapheme_char(cell) {
                self.graphemes.remove(pool::grapheme_id_from_char(cell));
            }
            let link = get_link_id(self.attributes[i]);
            if link != 0 {
                self.links.remove(link);
            }
        }
        self.chars[start..end].fill(char.unwrap_or(DEFAULT_SPACE_CHAR));
        self.attributes[start..end].fill(0);
        self.fg[start..end].fill(WHITE);
        self.bg[start..end].fill(bg);
    }

    /// `resize`: a new size clears every cell to opaque black spaces; the same size changes nothing.
    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.height {
            return;
        }
        let size = (width * height) as usize;
        self.width = width;
        self.height = height;
        self.chars.resize(size, 0);
        self.fg.resize(size, TRANSPARENT);
        self.bg.resize(size, TRANSPARENT);
        self.attributes.resize(size, 0);
        self.clear_with(RGBA::rgb(0, 0, 0, 255), None);
    }

    /// `copyFrom`: every cell and reference of a buffer of the same size, without blending.
    pub fn copy_from(&mut self, source: &OptimizedBuffer) {
        if self.width != source.width || self.height != source.height {
            return;
        }
        self.graphemes.copy_from(&source.graphemes);
        self.links.copy_from(&source.links);
        self.chars.copy_from_slice(&source.chars);
        self.fg.copy_from_slice(&source.fg);
        self.bg.copy_from_slice(&source.bg);
        self.attributes.copy_from_slice(&source.attributes);
    }

    // ---- cell writes ----

    /// `setRaw`: one cell and its link reference, no grapheme tracking.
    pub fn set_raw(&mut self, x: u32, y: u32, cell: Cell) {
        if !self.is_cell_in_buffer_and_scissor(x, y) {
            return;
        }
        let index = self.index(x, y);
        self.write_cell_and_links(index, cell);
    }

    /// `syncCell`: [`Self::set`] without span cleanup (the renderer's diff loop).
    pub fn sync_cell(&mut self, x: u32, y: u32, cell: Cell) {
        self.set_internal(false, x, y, cell);
    }

    /// `set`: one cell, keeping wide clusters' spans consistent.
    pub fn set(&mut self, x: u32, y: u32, cell: Cell) {
        self.set_internal(true, x, y, cell);
    }

    fn set_internal(&mut self, span_cleanup: bool, x: u32, y: u32, cell: Cell) {
        if !self.is_cell_in_buffer_and_scissor(x, y) {
            return;
        }
        let index = self.index(x, y);
        let prev_char = self.chars[index];
        if !pool::is_cluster_char(prev_char) && !pool::is_grapheme_char(cell.char) {
            self.write_cell_and_links(index, cell);
            return;
        }
        self.set_cluster_internal(span_cleanup, x, y, index, prev_char, cell);
    }

    fn set_cluster_internal(&mut self, span_cleanup: bool, x: u32, y: u32, index: usize, prev_char: u32, cell: Cell) {
        let new_link_id = get_link_id(cell.attributes);
        // Cleanup can remove the last old cell using the replacement's link.
        if new_link_id != 0 {
            self.links.add(new_link_id);
        }
        let mut tracker_replaced = false;
        let new_start_id = if pool::is_grapheme_char(cell.char) && x + pool::char_right_extent(cell.char) < self.width {
            Some(pool::grapheme_id_from_char(cell.char))
        } else {
            None
        };

        if !span_cleanup {
            let old_start_id = pool::is_grapheme_char(prev_char).then(|| pool::grapheme_id_from_char(prev_char));
            if old_start_id.is_some() || new_start_id.is_some() {
                self.graphemes.replace(old_start_id, new_start_id);
                tracker_replaced = true;
            }
        } else if pool::is_cluster_char(prev_char) && prev_char != cell.char {
            let row_start = (y * self.width) as usize;
            let id = pool::grapheme_id_from_char(prev_char);
            if let Some(new_id) = new_start_id {
                self.graphemes.add(new_id);
            }
            tracker_replaced = true;
            let span_start = index - (pool::char_left_extent(prev_char) as usize).min(index - row_start);
            let span_end = (span_start + pool::GLYPH_WIDTH_MAX as usize).min(row_start + self.width as usize);
            for span_i in span_start..span_end {
                let span_char = self.chars[span_i];
                if !pool::is_cluster_char(span_char) || pool::grapheme_id_from_char(span_char) != id {
                    continue;
                }
                if pool::char_left_extent(span_char) as usize != span_i - span_start {
                    continue;
                }
                if pool::is_grapheme_char(span_char) {
                    self.graphemes.remove(id);
                }
                let span_link = get_link_id(self.attributes[span_i]);
                if span_link != 0 {
                    self.links.remove(span_link);
                }
                self.chars[span_i] = DEFAULT_SPACE_CHAR;
                self.attributes[span_i] = 0;
            }
        }

        if pool::is_grapheme_char(cell.char) {
            let right = pool::char_right_extent(cell.char);
            let width = 1 + right;
            if x + width > self.width {
                let end_of_line = ((y + 1) * self.width) as usize;
                for eol_i in index..end_of_line {
                    let eol_char = self.chars[eol_i];
                    if eol_i != index && pool::is_grapheme_char(eol_char) {
                        self.graphemes.remove(pool::grapheme_id_from_char(eol_char));
                    }
                    let eol_link = get_link_id(self.attributes[eol_i]);
                    if eol_link != 0 {
                        self.links.remove(eol_link);
                    }
                }
                self.chars[index..end_of_line].fill(DEFAULT_SPACE_CHAR);
                self.attributes[index..end_of_line].fill(cell.attributes);
                self.fg[index..end_of_line].fill(cell.fg);
                self.bg[index..end_of_line].fill(cell.bg);
                if new_link_id != 0 {
                    for _ in index..end_of_line {
                        self.links.add(new_link_id);
                    }
                }
                if new_link_id != 0 {
                    self.links.remove(new_link_id);
                }
                return;
            }

            self.write_cell_and_links(index, cell);
            let id = pool::grapheme_id_from_char(cell.char);
            let same_start = pool::is_grapheme_char(prev_char) && prev_char == cell.char;
            if !tracker_replaced && !same_start {
                self.graphemes.add(id);
            }
            if right > 0 {
                let row_end_index = (y * self.width + self.width - 1) as usize;
                for cont_i in 1..=right as usize {
                    let cont_index = index + cont_i;
                    let cont_char = self.chars[cont_index];
                    if pool::is_grapheme_char(cont_char) {
                        let old_id = pool::grapheme_id_from_char(cont_char);
                        self.graphemes.remove(old_id);
                        if span_cleanup {
                            // Ordinary writes must not leave a tail outside the new span.
                            let tail_end = cont_index + (pool::char_right_extent(cont_char) as usize).min(row_end_index - cont_index);
                            let mut tail_i = index + width as usize;
                            while tail_i <= tail_end {
                                let tail_char = self.chars[tail_i];
                                if pool::is_continuation_char(tail_char)
                                    && pool::grapheme_id_from_char(tail_char) == old_id
                                    && pool::char_left_extent(tail_char) as usize == tail_i - cont_index
                                {
                                    let tail_link = get_link_id(self.attributes[tail_i]);
                                    if tail_link != 0 {
                                        self.links.remove(tail_link);
                                    }
                                    self.chars[tail_i] = DEFAULT_SPACE_CHAR;
                                    self.attributes[tail_i] = 0;
                                }
                                tail_i += 1;
                            }
                        }
                    }
                    let cont_link = get_link_id(self.attributes[cont_index]);
                    if cont_link != 0 {
                        self.links.remove(cont_link);
                    }
                }
                let span = index + 1..index + 1 + right as usize;
                self.fg[span.clone()].fill(cell.fg);
                self.bg[span.clone()].fill(cell.bg);
                self.attributes[span].fill(cell.attributes);
                for k in 1..=right {
                    self.chars[index + k as usize] = pool::pack_continuation(k, id);
                    if new_link_id != 0 {
                        self.links.add(new_link_id);
                    }
                }
            }
        } else {
            self.write_cell_and_links(index, cell);
        }
        if new_link_id != 0 {
            self.links.remove(new_link_id);
        }
    }

    fn write_cell_and_links(&mut self, index: usize, cell: Cell) {
        let prev_link = get_link_id(self.attributes[index]);
        let new_link = get_link_id(cell.attributes);
        self.chars[index] = cell.char;
        self.fg[index] = cell.fg;
        self.bg[index] = cell.bg;
        self.attributes[index] = cell.attributes;
        if prev_link != 0 && prev_link != new_link {
            self.links.remove(prev_link);
        }
        if new_link != 0 && new_link != prev_link {
            self.links.add(new_link);
        }
    }

    // ---- blending ----

    /// `blendCells`: `overlay` over `dest`.
    pub fn blend_cells(&self, overlay: Cell, dest: Cell) -> Cell {
        let bg_alpha = has_alpha(overlay.bg);
        let fg_alpha = has_alpha(overlay.fg);
        if !bg_alpha && !fg_alpha {
            return overlay;
        }
        let blended_bg = if bg_alpha { blend_colors(overlay.bg, dest.bg, self.blend_backdrop_color) } else { overlay.bg };
        let preserve = overlay.char == DEFAULT_SPACE_CHAR
            && dest.char != 0
            && dest.char != DEFAULT_SPACE_CHAR
            && pool::encoded_char_width(dest.char) == 1;
        let char = if preserve { dest.char } else { overlay.char };
        let fg = if preserve {
            blend_colors(overlay.bg, dest.fg, self.blend_backdrop_color)
        } else if fg_alpha {
            blend_colors(overlay.fg, blended_bg, self.blend_backdrop_color)
        } else {
            overlay.fg
        };
        // A preserved glyph keeps its base attributes, never its link: links always come from the overlay.
        let base = if preserve { base_attributes(dest.attributes) } else { base_attributes(overlay.attributes) };
        Cell { char, fg, bg: blended_bg, attributes: attributes_with_link(base, get_link_id(overlay.attributes)) }
    }

    fn blend_cell_with_opacity(&mut self, x: u32, y: u32, cell: Cell, opacity: f32, dest: Option<Cell>) {
        let opacity = opacity_to_u8(opacity);
        let effective = make_cell(cell.char, apply_opacity(cell.fg, opacity), apply_opacity(cell.bg, opacity), cell.attributes);
        match dest {
            Some(dest) => {
                let blended = self.blend_cells(effective, dest);
                if !self.graphemes.has_any() && !self.links.has_any() && !pool::is_cluster_char(blended.char) {
                    self.set_raw(x, y, blended);
                } else {
                    self.set(x, y, blended);
                }
            }
            None => self.set(x, y, effective),
        }
    }

    /// `setCellWithAlphaBlendingCellWithoutImages`.
    fn set_cell_blended_without_images(&mut self, x: u32, y: u32, cell: Cell) {
        if !self.is_cell_in_buffer_and_scissor(x, y) {
            return;
        }
        let opacity = self.current_opacity();
        if is_fully_transparent(opacity, cell.fg, cell.bg) {
            return;
        }
        if is_fully_opaque(opacity, cell.fg, cell.bg) {
            self.set(x, y, cell);
            return;
        }
        let dest = self.get(x, y);
        self.blend_cell_with_opacity(x, y, cell, opacity, dest);
    }

    /// `setCellWithAlphaBlendingCell` (no image placements: a fully transparent cell draws nothing).
    pub(crate) fn set_cell_blended(&mut self, x: u32, y: u32, cell: Cell) {
        let opacity = self.current_opacity();
        if is_fully_transparent(opacity, cell.fg, cell.bg) {
            return;
        }
        if !self.is_cell_in_buffer_and_scissor(x, y) {
            return;
        }
        if is_fully_opaque(opacity, cell.fg, cell.bg) {
            self.set(x, y, cell);
            return;
        }
        let dest = self.get(x, y);
        self.blend_cell_with_opacity(x, y, cell, opacity, dest);
    }

    /// `setCellWithAlphaBlendingRawCell`: for code point cells only.
    fn set_cell_blended_raw(&mut self, x: u32, y: u32, cell: Cell) {
        if !self.is_cell_in_buffer_and_scissor(x, y) {
            return;
        }
        let opacity = self.current_opacity();
        if opacity == 0.0 {
            return;
        }
        if is_fully_opaque(opacity, cell.fg, cell.bg) {
            self.set_raw(x, y, cell);
            return;
        }
        if is_fully_transparent(opacity, cell.fg, cell.bg) {
            return;
        }
        let opacity = opacity_to_u8(opacity);
        let effective = make_cell(cell.char, apply_opacity(cell.fg, opacity), apply_opacity(cell.bg, opacity), cell.attributes);
        match self.get(x, y) {
            Some(dest) => {
                let blended = self.blend_cells(effective, dest);
                self.set_raw(x, y, blended);
            }
            None => self.set_raw(x, y, effective),
        }
    }

    /// `trySetTransparentTextCellFast`: an opaque glyph over a transparent background keeps the destination
    /// background.
    fn try_set_transparent_text_cell_fast(&mut self, index: usize, char: u32, fg: RGBA, attributes: u32) -> bool {
        if fg.alpha() != 255 || get_link_id(attributes) != 0 || pool::is_cluster_char(char) {
            return false;
        }
        let dest_char = self.chars[index];
        if get_link_id(self.attributes[index]) != 0 || pool::is_cluster_char(dest_char) {
            return false;
        }
        if char == DEFAULT_SPACE_CHAR && dest_char != 0 && dest_char != DEFAULT_SPACE_CHAR && pool::encoded_char_width(dest_char) == 1 {
            return true;
        }
        self.chars[index] = char;
        self.fg[index] = fg;
        self.attributes[index] = attributes;
        true
    }

    // ---- the TS drawing API ----

    /// `setCell` (the `cell` draw): the cell as given, no blending. A control code point draws a space.
    pub fn set_cell(&mut self, x: i32, y: i32, ch: char, fg: RGBA, bg: RGBA, attributes: u32) {
        self.draw_cell_op(CellOp::Set, ch as u32, x, y, fg, bg, attributes);
    }

    /// `setCellWithAlphaBlending` (the `cellBlend` draw).
    pub fn set_cell_with_alpha_blending(&mut self, x: i32, y: i32, ch: char, fg: RGBA, bg: RGBA, attributes: u32) {
        self.draw_cell_op(CellOp::Blend, ch as u32, x, y, fg, bg, attributes);
    }

    /// `drawChar`: a code point, blended (the `char` draw).
    pub fn draw_char(&mut self, code_point: u32, x: i32, y: i32, fg: RGBA, bg: RGBA, attributes: u32) {
        self.draw_cell_op(CellOp::Char, code_point, x, y, fg, bg, attributes);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_cell_op(&mut self, op: CellOp, code_point: u32, x: i32, y: i32, fg: RGBA, bg: RGBA, attributes: u32) {
        if attributes & !ATTRIBUTE_BASE_MASK != 0 || code_point > 0x10ffff || (0xd800..=0xdfff).contains(&code_point) {
            return;
        }
        let char = if is_control_codepoint(code_point) { DEFAULT_SPACE_CHAR } else { code_point };
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let (x, y) = (x as u32, y as u32);
        let cell = make_cell(char, fg, bg, attributes);
        match op {
            CellOp::Set => self.set(x, y, cell),
            // Both are setCellWithAlphaBlendingCell without images.
            CellOp::Blend | CellOp::Char => self.set_cell_blended(x, y, cell),
        }
    }

    /// `drawText`: one row of text from (x, y); with `bg` `None` each cell keeps its background. A selection
    /// (UTF-16 offsets, as TS slices) draws its part in the selection colors: by default the foreground on the
    /// background (or black).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(&mut self, text: &str, x: i32, y: i32, fg: RGBA, bg: Option<RGBA>, attributes: u32, selection: Option<&TextSelection>) {
        let Some(selection) = selection else {
            self.draw_text_fitted(text, x, y, fg, bg, attributes, 0, false);
            return;
        };
        let (selection_bg, selection_fg) = match selection.bg_color {
            Some(bg_color) => (bg_color, selection.fg_color.unwrap_or(fg)),
            None => {
                let default_bg = bg.unwrap_or(TRANSPARENT);
                (fg, if default_bg.alpha() > 0 { default_bg } else { RGBA::rgb(0, 0, 0, 255) })
            }
        };
        let units: Vec<u16> = text.encode_utf16().collect();
        let slice = |from: usize, to: usize| String::from_utf16_lossy(&units[from.min(units.len())..to.min(units.len())]);
        let (start, end) = (selection.start, selection.end);
        if start > 0 {
            self.draw_text(&slice(0, start), x, y, fg, bg, attributes, None);
        }
        if end > start {
            self.draw_text(&slice(start, end), x + start as i32, y, selection_fg, Some(selection_bg), attributes, None);
        }
        if end < units.len() {
            self.draw_text(&slice(end, units.len()), x + end as i32, y, fg, bg, attributes, None);
        }
    }

    /// `drawTextFit`: text within `max_width` cells from x, cut on a glyph (with an ellipsis when asked);
    /// nothing when `max_width < 1`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_fit(&mut self, text: &str, x: i32, y: i32, max_width: i32, fg: RGBA, bg: Option<RGBA>, ellipsis: bool) {
        if max_width < 1 {
            return;
        }
        self.draw_text_fitted(text, x, y, fg, bg, 0, max_width as u32, ellipsis);
    }

    /// `drawTextFitted` (`max_width` 0 is no limit).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_fitted(&mut self, text: &str, x: i32, y: i32, fg: RGBA, bg: Option<RGBA>, attributes: u32, max_width: u32, ellipsis: bool) {
        if text.len() > TEXT_BYTES_MAX || attributes & !ATTRIBUTE_BASE_MASK != 0 {
            return;
        }
        if y < 0 || x as i64 >= self.width as i64 || y as u32 >= self.height || text.is_empty() {
            return;
        }
        let row = y as u32;
        let opacity = self.current_opacity();
        if is_fully_transparent(opacity, fg, bg.unwrap_or(TRANSPARENT)) {
            return;
        }
        let end: i64 = if max_width == 0 { self.width as i64 } else { (self.width as i64).min(x as i64 + max_width as i64) };
        let ascii = unicode::is_ascii_only(text.as_bytes());
        if ascii && !(ellipsis && text.len() > max_width as usize) {
            let clipped = if x < 0 { text.len().min(x.unsigned_abs() as usize) } else { 0 };
            for (column, &byte) in (x.max(0) as u32..).zip(&text.as_bytes()[clipped..]) {
                if column as i64 >= end {
                    break;
                }
                let background = bg.unwrap_or_else(|| self.bg[self.index(column, row)]);
                self.set_text_cell(column, row, make_cell(byte as u32, fg, background, attributes));
            }
            return;
        }

        let mut owned = String::new();
        let mut input = text;
        if ellipsis {
            let room = (max_width as u64).saturating_sub(unicode::width_at(ELLIPSIS, 0, TAB_WIDTH, self.width_method) as u64);
            let mut measure = TextRow::new(self, input, x, y, i64::MAX);
            if let Some(kept) = measure.fit_bytes(room, max_width as u64) {
                owned.push_str(&text[..kept]);
                owned.push_str(ELLIPSIS);
                input = &owned;
            }
        }

        struct Run {
            x: u32,
            char: u32,
            count: u32,
        }
        let mut runs: Vec<Run> = Vec::new();
        {
            let mut layout = TextRow::new(self, input, x, y, end);
            while let Some(glyph) = layout.next() {
                let bytes = &input.as_bytes()[glyph.start..glyph.end];
                let char = if glyph.blank {
                    DEFAULT_SPACE_CHAR
                } else if bytes.len() == 1 {
                    bytes[0] as u32
                } else {
                    let id = pool::with_graphemes(|pool| pool.acquire(bytes));
                    pool::pack_grapheme_start(id, glyph.width)
                };
                let _ = glyph.width;
                runs.push(Run { x: glyph.x, char, count: glyph.count });
            }
        }
        for run in &runs {
            let background = bg.unwrap_or_else(|| self.bg[self.index(run.x, row)]);
            let cell = make_cell(run.char, fg, background, attributes);
            for offset in 0..run.count {
                self.set_text_cell(run.x + offset, row, cell);
            }
        }
        // The runs' own references end once the cells hold theirs.
        for run in runs {
            if pool::is_grapheme_char(run.char) {
                pool::with_graphemes(|pool| pool.decref(pool::grapheme_id_from_char(run.char)));
            }
        }
    }

    /// `setTextCell`.
    fn set_text_cell(&mut self, x: u32, y: u32, cell: Cell) {
        if has_alpha(cell.bg) {
            self.set_cell_blended_without_images(x, y, cell);
            return;
        }
        self.set(x, y, cell);
    }

    /// `drawGraphemeChecked`: one already-segmented cluster with a given width.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_grapheme(&mut self, bytes: &str, cell_width: u8, x: u32, y: u32, fg: RGBA, bg: RGBA, attributes: u32) {
        let cell_width = cell_width as u32;
        if attributes & !ATTRIBUTE_BASE_MASK != 0 || bytes.is_empty() || cell_width == 0 || x >= self.width || y >= self.height {
            return;
        }
        if cell_width > self.width - x {
            return;
        }
        if (0..cell_width).any(|offset| !self.is_point_in_scissor((x + offset) as i32, y as i32)) {
            return;
        }
        if !is_printable_glyph(bytes.as_bytes(), cell_width) {
            for offset in 0..cell_width {
                self.set(x + offset, y, make_cell(DEFAULT_SPACE_CHAR, fg, bg, attributes));
            }
            return;
        }
        if bytes.len() == 1 && cell_width == 1 {
            self.set(x, y, make_cell(bytes.as_bytes()[0] as u32, fg, bg, attributes));
            return;
        }
        let id = pool::with_graphemes(|pool| pool.acquire(bytes.as_bytes()));
        self.set(x, y, make_cell(pool::pack_grapheme_start(id, cell_width), fg, bg, attributes));
        pool::with_graphemes(|pool| pool.decref(id));
    }

    /// `fillRect` (TS: nothing for a non-positive extent; native `fillRectClipped` for the part inside).
    pub fn fill_rect(&mut self, x: i32, y: i32, width: i32, height: i32, bg: RGBA) {
        if width <= 0 || height <= 0 {
            return;
        }
        self.fill_rect_clipped(x, y, width as u32, height as u32, bg);
    }

    /// `fillRectClipped`: the part of a rectangle at a signed position inside the buffer.
    pub fn fill_rect_clipped(&mut self, x: i32, y: i32, width: u32, height: u32, bg: RGBA) {
        let start_x = (x as i64).max(0);
        let start_y = (y as i64).max(0);
        let end_x = (self.width as i64).min(x as i64 + width as i64);
        let end_y = (self.height as i64).min(y as i64 + height as i64);
        if start_x >= end_x || start_y >= end_y {
            return;
        }
        self.fill_rect_native(start_x as u32, start_y as u32, (end_x - start_x) as u32, (end_y - start_y) as u32, bg);
    }

    /// `fillRect` (buffer.zig).
    fn fill_rect_native(&mut self, x: u32, y: u32, width: u32, height: u32, bg: RGBA) {
        if width == 0 || height == 0 || x >= self.width || y >= self.height {
            return;
        }
        if !self.is_rect_in_scissor(x as i32, y as i32, width, height) {
            return;
        }
        let opacity = self.current_opacity();
        if is_fully_transparent(opacity, TRANSPARENT, bg) {
            return;
        }
        let end_x = (self.width - 1).min(x + width - 1);
        let end_y = (self.height - 1).min(y + height - 1);
        let Some(clipped) = self.clip_rect_to_scissor(x as i32, y as i32, end_x - x + 1, end_y - y + 1) else { return };
        let sx = x.max(clipped.x as u32);
        let sy = y.max(clipped.y as u32);
        let ex = end_x.min((clipped.x + clipped.width as i32 - 1) as u32);
        let ey = end_y.min((clipped.y + clipped.height as i32 - 1) as u32);
        let blank = make_cell(DEFAULT_SPACE_CHAR, WHITE, bg, 0);
        if self.graphemes.has_any() || self.links.has_any() {
            for fy in sy..=ey {
                for fx in sx..=ex {
                    self.set_cell_blended(fx, fy, blank);
                }
            }
        } else if has_alpha(bg) || opacity < 1.0 {
            for fy in sy..=ey {
                for fx in sx..=ex {
                    self.set_cell_blended_raw(fx, fy, blank);
                }
            }
        } else {
            for fy in sy..=ey {
                let start = self.index(sx, fy);
                let end = start + (ex - sx + 1) as usize;
                self.chars[start..end].fill(DEFAULT_SPACE_CHAR);
                self.fg[start..end].fill(WHITE);
                self.bg[start..end].fill(bg);
                self.attributes[start..end].fill(0);
            }
        }
    }

    /// `drawBox` (TS: nothing for a non-positive extent).
    pub fn draw_box(&mut self, options: &DrawBoxOptions) {
        if options.width <= 0 || options.height <= 0 {
            return;
        }
        let chars = options.custom_border_chars.unwrap_or_else(|| options.border_style.char_array());
        if chars.iter().any(|&c| c > 0x10ffff || (0xd800..=0xdfff).contains(&c) || (c != 0 && (c < 32 || (127..=159).contains(&c)))) {
            return;
        }
        let title = options.title.filter(|t| !t.is_empty());
        let bottom_title = options.bottom_title.filter(|t| !t.is_empty());
        self.draw_box_checked(
            options.x,
            options.y,
            options.width as u32,
            options.height as u32,
            &chars,
            options.border,
            options.border_color,
            options.background_color,
            options.title_color.unwrap_or(options.border_color),
            options.should_fill,
            title,
            options.title_alignment.code(),
            bottom_title,
            options.bottom_title_alignment.code(),
        );
    }

    /// `drawBoxChecked`: the native box draw.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_box_checked(
        &mut self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        border_chars: &[u32; 11],
        sides: BorderSides,
        border_color: RGBA,
        background_color: RGBA,
        title_color: RGBA,
        should_fill: bool,
        title: Option<&str>,
        title_alignment: u8,
        bottom_title: Option<&str>,
        bottom_title_alignment: u8,
    ) {
        if width > i32::MAX as u32 || height > i32::MAX as u32 || x as i64 + width as i64 > i32::MAX as i64 || y as i64 + height as i64 > i32::MAX as i64 {
            return;
        }
        let opacity = self.current_opacity();
        let border_bg_transparent = is_fully_transparent(opacity, border_color, background_color);
        let has_title = title.is_some() || bottom_title.is_some();
        let title_visible = has_title && !is_fully_transparent(opacity, title_color, background_color);
        if border_bg_transparent && !title_visible {
            return;
        }

        let start_x = x.max(0);
        let start_y = y.max(0);
        let end_x = (self.width as i32 - 1).min(x + width as i32 - 1);
        let end_y = (self.height as i32 - 1).min(y + height as i32 - 1);
        if start_x > end_x || start_y > end_y {
            return;
        }
        let box_width = (end_x - start_x + 1) as u32;
        let box_height = (end_y - start_y + 1) as u32;
        if !self.is_rect_in_scissor(start_x, start_y, box_width, box_height) {
            return;
        }
        let at_left = start_x == x;
        let at_right = end_x == x + width as i32 - 1;
        let at_top = start_y == y;
        let at_bottom = end_y == y + height as i32 - 1;

        let title_layout = self.box_title_layout(title, sides.top, at_top, x, width, title_alignment);
        let bottom_layout = self.box_title_layout(bottom_title, sides.bottom, at_bottom, x, width, bottom_title_alignment);

        if should_fill {
            if !sides.any() {
                self.fill_rect_native(start_x as u32, start_y as u32, box_width, box_height, background_color);
            } else {
                let inner_sx = start_x + (sides.left && at_left) as i32;
                let inner_sy = start_y + (sides.top && at_top) as i32;
                let inner_ex = end_x - (sides.right && at_right) as i32;
                let inner_ey = end_y - (sides.bottom && at_bottom) as i32;
                if inner_ex >= inner_sx && inner_ey >= inner_sy {
                    self.fill_rect_native(inner_sx as u32, inner_sy as u32, (inner_ex - inner_sx + 1) as u32, (inner_ey - inner_sy + 1) as u32, background_color);
                }
            }
        }

        let left_only = sides.left && at_left && !sides.top && !sides.bottom;
        let right_only = sides.right && at_right && !sides.top && !sides.bottom;
        let bottom_with_verticals = sides.bottom && at_bottom && !sides.top && (sides.left || sides.right);
        let top_with_verticals = sides.top && at_top && !sides.bottom && (sides.left || sides.right);
        let extend_to_top = left_only || right_only || bottom_with_verticals;
        let extend_to_bottom = left_only || right_only || top_with_verticals;
        let transparent_fast = self.can_use_transparent_border_fast_path(border_chars, border_color, background_color, start_x, start_y, box_width, box_height);
        let opaque_fast = is_fully_opaque(opacity, border_color, background_color);

        let border_cell = |buffer: &mut OptimizedBuffer, cx: i32, cy: i32, char: u32| {
            if transparent_fast {
                buffer.set_transparent_border_cell(cx as u32, cy as u32, char, border_color);
            } else if opaque_fast {
                buffer.set(cx as u32, cy as u32, make_cell(char, border_color, background_color, 0));
            } else {
                buffer.set_cell_blended_without_images(cx as u32, cy as u32, make_cell(char, border_color, background_color, 0));
            }
        };

        if sides.top && at_top {
            for draw_x in start_x..=end_x {
                if title_layout.draw && draw_x >= title_layout.start_x && draw_x <= title_layout.end_x {
                    continue;
                }
                let mut char = border_chars[bci::HORIZONTAL];
                if draw_x == start_x && at_left {
                    char = if sides.left { border_chars[bci::TOP_LEFT] } else { border_chars[bci::HORIZONTAL] };
                } else if draw_x == end_x && at_right {
                    char = if sides.right { border_chars[bci::TOP_RIGHT] } else { border_chars[bci::HORIZONTAL] };
                }
                border_cell(self, draw_x, start_y, char);
            }
        }
        if sides.bottom && at_bottom {
            for draw_x in start_x..=end_x {
                if bottom_layout.draw && draw_x >= bottom_layout.start_x && draw_x <= bottom_layout.end_x {
                    continue;
                }
                let mut char = border_chars[bci::HORIZONTAL];
                if draw_x == start_x && at_left {
                    char = if sides.left { border_chars[bci::BOTTOM_LEFT] } else { border_chars[bci::HORIZONTAL] };
                } else if draw_x == end_x && at_right {
                    char = if sides.right { border_chars[bci::BOTTOM_RIGHT] } else { border_chars[bci::HORIZONTAL] };
                }
                border_cell(self, draw_x, end_y, char);
            }
        }

        let vertical_start = if extend_to_top { start_y } else { start_y + (sides.top && at_top) as i32 };
        let vertical_end = if extend_to_bottom { end_y } else { end_y - (sides.bottom && at_bottom) as i32 };
        if sides.left || sides.right {
            let mut draw_y = vertical_start;
            while draw_y <= vertical_end {
                if sides.left && at_left && start_x >= 0 && start_x < self.width as i32 {
                    border_cell(self, start_x, draw_y, border_chars[bci::VERTICAL]);
                }
                if sides.right && at_right && end_x >= 0 && end_x < self.width as i32 {
                    border_cell(self, end_x, draw_y, border_chars[bci::VERTICAL]);
                }
                draw_y += 1;
            }
        }

        if title_layout.draw
            && let Some(text) = title {
                self.draw_text_fitted(text, title_layout.start_x, start_y, title_color, Some(background_color), 0, 0, false);
            }
        if bottom_layout.draw
            && let Some(text) = bottom_title {
                self.draw_text_fitted(text, bottom_layout.start_x, end_y, title_color, Some(background_color), 0, 0, false);
            }
    }

    fn box_title_layout(&self, title: Option<&str>, side: bool, at_side: bool, box_x: i32, width: u32, alignment: u8) -> TitleLayout {
        let Some(text) = title else { return TitleLayout::default() };
        if text.is_empty() || !side || !at_side {
            return TitleLayout::default();
        }
        let length = unicode::text_width(text, TAB_WIDTH, self.width_method) as i32;
        if (width as i32) < length + 4 {
            return TitleLayout::default();
        }
        let padding = 2;
        let title_x = match alignment {
            1 => box_x + (width as i32 - length).div_euclid(2),
            2 => box_x + width as i32 - padding - length,
            _ => box_x + padding,
        };
        TitleLayout { draw: true, start_x: title_x, end_x: title_x + length - 1 }
    }

    #[allow(clippy::too_many_arguments)]
    fn can_use_transparent_border_fast_path(&self, chars: &[u32; 11], border: RGBA, background: RGBA, x: i32, y: i32, width: u32, height: u32) -> bool {
        self.current_opacity() == 1.0
            && border.alpha() == 255
            && background.alpha() == 0
            && self.is_point_in_scissor(x, y)
            && self.is_point_in_scissor(x + width as i32 - 1, y + height as i32 - 1)
            && !self.graphemes.has_any()
            && !self.links.has_any()
            && [bci::TOP_LEFT, bci::TOP_RIGHT, bci::BOTTOM_LEFT, bci::BOTTOM_RIGHT, bci::HORIZONTAL, bci::VERTICAL]
                .iter()
                .all(|&i| is_single_width_border_char(chars[i]))
    }

    fn set_transparent_border_cell(&mut self, x: u32, y: u32, char: u32, color: RGBA) {
        let index = self.index(x, y);
        if char != DEFAULT_SPACE_CHAR {
            self.chars[index] = char;
            self.fg[index] = color;
            self.attributes[index] = 0;
            return;
        }
        if self.try_set_transparent_text_cell_fast(index, char, color, 0) {
            return;
        }
        self.set_cell_blended(x, y, make_cell(char, color, TRANSPARENT, 0));
    }

    /// `drawFrameBuffer`: `source`'s cells (a part of them when given) at (dest_x, dest_y), copied, or blended
    /// when the source respects alpha or the opacity is below one.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_frame_buffer(
        &mut self,
        dest_x: i32,
        dest_y: i32,
        source: &OptimizedBuffer,
        source_x: Option<u32>,
        source_y: Option<u32>,
        source_width: Option<u32>,
        source_height: Option<u32>,
    ) {
        let opacity = self.current_opacity();
        if opacity == 0.0 {
            return;
        }
        let src_x = source_x.unwrap_or(0);
        let src_y = source_y.unwrap_or(0);
        let src_w = source_width.unwrap_or(source.width);
        let src_h = source_height.unwrap_or(source.height);
        if src_x >= source.width || src_y >= source.height || src_w == 0 || src_h == 0 {
            return;
        }
        let w = src_w.min(source.width - src_x);
        let h = src_h.min(source.height - src_y);
        let start_x = dest_x.max(0);
        let start_y = dest_y.max(0);
        let end_x = (self.width as i32 - 1).min(dest_x + w as i32 - 1);
        let end_y = (self.height as i32 - 1).min(dest_y + h as i32 - 1);
        if start_x > end_x || start_y > end_y {
            return;
        }
        let dest_w = (end_x - start_x + 1) as u32;
        let dest_h = (end_y - start_y + 1) as u32;
        if !self.is_rect_in_scissor(start_x, start_y, dest_w, dest_h) {
            return;
        }
        let grapheme_aware = self.graphemes.has_any() || source.graphemes.has_any();
        let link_aware = self.links.has_any() || source.links.has_any();
        let Some(clipped) = self.clip_rect_to_scissor(start_x, start_y, dest_w, dest_h) else { return };
        let cs_x = start_x.max(clipped.x);
        let cs_y = start_y.max(clipped.y);
        let ce_x = end_x.min(clipped.x + clipped.width as i32 - 1);
        let ce_y = end_y.min(clipped.y + clipped.height as i32 - 1);

        if opacity == 1.0 && !grapheme_aware && !source.respect_alpha && !link_aware {
            for d_y in cs_y..=ce_y {
                let s_y = src_y + (d_y - dest_y) as u32;
                if s_y >= source.height {
                    continue;
                }
                let s_x = src_x + (cs_x - dest_x) as u32;
                if s_x >= source.width {
                    continue;
                }
                let dest_start = self.index(cs_x as u32, d_y as u32);
                let src_start = source.index(s_x, s_y);
                let count = ((ce_x - cs_x + 1) as u32).min(source.width - s_x) as usize;
                self.chars[dest_start..dest_start + count].copy_from_slice(&source.chars[src_start..src_start + count]);
                self.fg[dest_start..dest_start + count].copy_from_slice(&source.fg[src_start..src_start + count]);
                self.bg[dest_start..dest_start + count].copy_from_slice(&source.bg[src_start..src_start + count]);
                self.attributes[dest_start..dest_start + count].copy_from_slice(&source.attributes[src_start..src_start + count]);
            }
            return;
        }

        let copies = opacity == 1.0 && !source.respect_alpha;
        for d_y in cs_y..=ce_y {
            let mut last_drawn: Option<u32> = None;
            for d_x in cs_x..=ce_x {
                let s_x = src_x + (d_x - dest_x) as u32;
                let s_y = src_y + (d_y - dest_y) as u32;
                if s_x >= source.width || s_y >= source.height {
                    continue;
                }
                let si = source.index(s_x, s_y);
                let mut src_char = source.chars[si];
                let (src_fg, src_bg, src_attr) = (source.fg[si], source.bg[si], source.attributes[si]);
                if !copies && src_bg.alpha() == 0 && src_fg.alpha() == 0 {
                    continue;
                }
                let (dx, dy) = (d_x as u32, d_y as u32);
                if grapheme_aware {
                    if pool::is_continuation_char(src_char) {
                        let id = src_char & pool::GRAPHEME_ID_MASK;
                        if Some(id) != last_drawn {
                            // The start is left of the drawn part: a styled space fills the cell.
                            self.copy_or_blend(copies, dx, dy, make_cell(DEFAULT_SPACE_CHAR, src_fg, src_bg, src_attr));
                        }
                        continue;
                    }
                    if pool::is_grapheme_char(src_char) {
                        if pool::char_right_extent(src_char) > (ce_x - d_x) as u32 {
                            src_char = DEFAULT_SPACE_CHAR;
                            last_drawn = None;
                        } else {
                            last_drawn = Some(src_char & pool::GRAPHEME_ID_MASK);
                        }
                    }
                    self.copy_or_blend(copies, dx, dy, make_cell(src_char, src_fg, src_bg, src_attr));
                    continue;
                }
                let cell = make_cell(src_char, src_fg, src_bg, src_attr);
                if copies {
                    self.set(dx, dy, cell);
                } else {
                    self.set_cell_blended_raw(dx, dy, cell);
                }
            }
        }
    }

    fn copy_or_blend(&mut self, copies: bool, x: u32, y: u32, cell: Cell) {
        if copies {
            self.set(x, y, cell);
        } else {
            self.set_cell_blended(x, y, cell);
        }
    }

    // ---- links ----

    /// Adds a URL to the link pool for cells of this buffer (`LinkTracker.trackUrl`): the id goes into a cell's
    /// attributes with [`attributes_with_link`]. `None` for a URL over 512 bytes.
    pub fn track_link(&mut self, url: &str) -> Option<u32> {
        self.links.track_url(url)
    }

    // ---- reading ----

    /// The text of a cell's char: a code point, a cluster's bytes, nothing for a continuation, a space for 0 or
    /// an invalid code point (`resolveChars`).
    pub fn char_text(char: u32) -> String {
        if pool::is_continuation_char(char) {
            return String::new();
        }
        if pool::is_grapheme_char(char) {
            return pool::grapheme_bytes(pool::grapheme_id_from_char(char))
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|| " ".to_owned());
        }
        match char::from_u32(char) {
            Some(c) if char != 0 => c.to_string(),
            _ => " ".to_owned(),
        }
    }

    /// `getRealCharBytes`: every cell's text, a newline after each row when asked.
    pub fn get_real_char_bytes(&self, add_line_breaks: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.chars.len());
        for (index, &char) in self.chars.iter().enumerate() {
            out.extend_from_slice(Self::char_text(char).as_bytes());
            if add_line_breaks && (index + 1) % self.width as usize == 0 {
                out.push(b'\n');
            }
        }
        out
    }
}

#[derive(Clone, Copy)]
enum CellOp {
    Set,
    Blend,
    Char,
}

/// `isSingleWidthBorderChar`.
fn is_single_width_border_char(char: u32) -> bool {
    if char == 0 || (32..=126).contains(&char) || (0x2500..=0x257F).contains(&char) {
        return true;
    }
    if char > 0x10FFFF {
        return false;
    }
    // TODO(port-tui): opencode-tui-text: utf8.eastAsianWidth.
    char::from_u32(char).is_some_and(|c| unicode::text_width(&c.to_string(), TAB_WIDTH, WidthMethod::Unicode) == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(buffer: &OptimizedBuffer, y: u32) -> String {
        (0..buffer.width()).map(|x| OptimizedBuffer::char_text(buffer.get(x, y).unwrap().char)).collect()
    }

    #[test]
    fn text_and_boxes() {
        let mut buffer = OptimizedBuffer::new(10, 3, false, WidthMethod::Unicode);
        buffer.clear(RGBA::rgb(0, 0, 0, 255));
        buffer.draw_text("a中b", 0, 0, WHITE, None, 0, None);
        assert_eq!(row(&buffer, 0), "a中b      ");
        assert!(pool::is_continuation_char(buffer.get(2, 0).unwrap().char));
        buffer.draw_text("x", 2, 0, WHITE, None, 0, None);
        // Writing over a continuation clears the whole wide glyph.
        assert_eq!(row(&buffer, 0), "a xb      ");
        let mut boxed = OptimizedBuffer::new(8, 3, false, WidthMethod::Unicode);
        boxed.clear(RGBA::rgb(0, 0, 0, 255));
        boxed.draw_box(&DrawBoxOptions {
            x: 0,
            y: 0,
            width: 8,
            height: 3,
            border_style: BorderStyle::Single,
            custom_border_chars: None,
            border: BorderSides::ALL,
            border_color: WHITE,
            background_color: RGBA::rgb(0, 0, 0, 255),
            should_fill: true,
            title: Some("t"),
            title_color: None,
            title_alignment: TitleAlignment::Left,
            bottom_title: None,
            bottom_title_alignment: TitleAlignment::Left,
        });
        assert_eq!(row(&boxed, 0), "┌─t────┐");
        assert_eq!(row(&boxed, 1), "│      │");
        assert_eq!(row(&boxed, 2), "└──────┘");
    }
}
