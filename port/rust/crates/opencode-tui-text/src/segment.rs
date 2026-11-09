//! Port of `text-buffer-segment.zig`: the leaves of a text buffer's rope (text chunks, line breaks and line-start
//! markers), their metrics, and the boundary rules the rope applies at every join.
//!
//! Divergence: a chunk caches only its render clusters (what drawing reads every frame). The Zig chunk also caches
//! wrap-break lists for word wrapping and word motion; those are recomputed here, with the same results.

use crate::mem_registry::MemRegistry;
use crate::unicode::BreakState;
use crate::utf8::{self, ControlDisplay, RenderClusterInfo, WidthMethod};
use std::cell::{Cell, OnceCell, RefCell};

pub(crate) const ASCII_ONLY: u8 = 0b001;
pub(crate) const HAS_TAB: u8 = 0b010;
pub(crate) const STATIC_ELLIPSIS: u8 = 0b100;

#[derive(Default, Debug)]
struct ClusterCache {
    key: Option<(u8, WidthMethod)>,
    clusters: Vec<RenderClusterInfo>,
}

/// A contiguous range of UTF-8 bytes in one memory slot (`TextChunk`).
#[derive(Debug)]
pub struct TextChunk {
    pub(crate) mem_id: u8,
    pub(crate) flags: u8,
    pub(crate) byte_start: u32,
    pub(crate) byte_end: u32,
    width_cols: Cell<u32>,
    clusters: OnceCell<Box<RefCell<ClusterCache>>>,
}

impl Clone for TextChunk {
    fn clone(&self) -> Self {
        TextChunk::new(self.mem_id, self.flags, self.byte_start, self.byte_end, self.width_cols.get())
    }
}

impl TextChunk {
    pub(crate) fn new(mem_id: u8, flags: u8, byte_start: u32, byte_end: u32, width_cols: u32) -> Self {
        TextChunk { mem_id, flags, byte_start, byte_end, width_cols: Cell::new(width_cols), clusters: OnceCell::new() }
    }

    pub(crate) fn empty() -> Self {
        TextChunk::new(0, 0, 0, 0, 0)
    }

    /// The display width of the chunk.
    pub fn width_cols(&self) -> u32 {
        self.width_cols.get()
    }

    pub(crate) fn set_width_cols(&self, width: u32) {
        self.width_cols.set(width)
    }

    /// Printable ASCII only (32..126).
    pub fn is_ascii_only(&self) -> bool {
        self.flags & ASCII_ONLY != 0
    }

    pub fn has_tab(&self) -> bool {
        self.flags & HAS_TAB != 0
    }

    /// The truncation ellipsis of a view (`...`), which lives in no slot.
    pub fn is_ellipsis(&self) -> bool {
        self.flags & STATIC_ELLIPSIS != 0
    }

    pub fn byte_len(&self) -> u32 {
        self.byte_end - self.byte_start
    }

    pub(crate) fn bytes<'a>(&self, registry: &'a MemRegistry) -> &'a [u8] {
        if self.is_ellipsis() {
            return &b"..."[self.byte_start as usize..self.byte_end as usize];
        }
        match registry.get(self.mem_id) {
            Some(data) => &data[self.byte_start as usize..self.byte_end as usize],
            None => &[],
        }
    }

    /// `getRenderClusters`: the chunk's multibyte clusters and tabs, cached per tab width and width method.
    pub(crate) fn render_clusters<R>(
        &self,
        registry: &MemRegistry,
        tab_width: u8,
        control_display: ControlDisplay,
        method: WidthMethod,
        f: impl FnOnce(&[RenderClusterInfo]) -> R,
    ) -> R {
        if self.is_ascii_only() {
            return f(&[]);
        }
        let cache = self.clusters.get_or_init(Default::default);
        let mut cache = cache.borrow_mut();
        if cache.key != Some((tab_width, method)) {
            let mut clusters = std::mem::take(&mut cache.clusters);
            clusters.clear();
            utf8::find_render_cluster_info(self.bytes(registry), tab_width, control_display, false, method, &mut clusters);
            clusters.shrink_to_fit();
            cache.clusters = clusters;
            cache.key = Some((tab_width, method));
        }
        f(&cache.clusters)
    }
}

/// A styled region on a line (`Highlight`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Highlight {
    pub col_start: u32,
    pub col_end: u32,
    pub style_id: u32,
    pub priority: u8,
    pub hl_ref: u16,
    pub internal: bool,
}

/// A contiguous region with one style, precomputed for drawing (`StyleSpan`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StyleSpan {
    pub col: u32,
    pub style_id: u32,
    pub next_col: u32,
}

/// A rope leaf (`Segment`).
#[derive(Clone, Debug)]
pub enum Segment {
    Text(TextChunk),
    Brk,
    LineStart,
}

/// Aggregated leaf metrics (`Segment.Metrics`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SegMetrics {
    pub total_width_cols: u32,
    pub total_bytes: u32,
    pub linestart_count: u32,
    pub newline_count: u32,
    pub line_width_first: u32,
    pub line_width_last: u32,
    pub line_width_inner_max: u32,
    pub ascii_only: bool,
    pub has_tabs: bool,
}

impl Default for SegMetrics {
    fn default() -> Self {
        SegMetrics {
            total_width_cols: 0,
            total_bytes: 0,
            linestart_count: 0,
            newline_count: 0,
            line_width_first: 0,
            line_width_last: 0,
            line_width_inner_max: 0,
            ascii_only: true,
            has_tabs: false,
        }
    }
}

impl SegMetrics {
    pub fn add(&mut self, other: &SegMetrics) {
        if other.newline_count == 0 {
            self.line_width_last += other.total_width_cols;
            if self.newline_count == 0 {
                self.line_width_first += other.total_width_cols;
            }
        } else if self.newline_count == 0 {
            self.line_width_first += other.line_width_first;
            self.line_width_last = other.line_width_last;
            self.line_width_inner_max = other.line_width_inner_max;
        } else {
            self.line_width_inner_max = self
                .line_width_inner_max
                .max(other.line_width_inner_max)
                .max(self.line_width_last + other.line_width_first);
            self.line_width_last = other.line_width_last;
        }
        self.total_width_cols += other.total_width_cols;
        self.total_bytes += other.total_bytes;
        self.linestart_count += other.linestart_count;
        self.newline_count += other.newline_count;
        self.ascii_only = self.ascii_only && other.ascii_only;
        self.has_tabs = self.has_tabs || other.has_tabs;
    }

    pub fn line_width_max(&self) -> u32 {
        self.line_width_first.max(self.line_width_last).max(self.line_width_inner_max)
    }

    /// The balancing weight: cells plus one per line break.
    pub fn weight(&self) -> u32 {
        self.total_width_cols + self.newline_count
    }
}

impl Segment {
    pub fn measure(&self) -> SegMetrics {
        match self {
            Segment::Text(chunk) => SegMetrics {
                total_width_cols: chunk.width_cols(),
                total_bytes: chunk.byte_len(),
                line_width_first: chunk.width_cols(),
                line_width_last: chunk.width_cols(),
                ascii_only: chunk.is_ascii_only(),
                has_tabs: chunk.has_tab(),
                ..SegMetrics::default()
            },
            Segment::Brk => SegMetrics { newline_count: 1, ..SegMetrics::default() },
            Segment::LineStart => SegMetrics { linestart_count: 1, ..SegMetrics::default() },
        }
    }

    pub(crate) fn empty() -> Segment {
        Segment::Text(TextChunk::empty())
    }

    /// Text without bytes, as a split can leave on one side (`isVoid`).
    pub(crate) fn is_void(&self) -> bool {
        matches!(self, Segment::Text(chunk) if chunk.byte_start == chunk.byte_end)
    }

    pub fn is_break(&self) -> bool {
        matches!(self, Segment::Brk)
    }

    pub fn is_line_start(&self) -> bool {
        matches!(self, Segment::LineStart)
    }

    pub fn is_text(&self) -> bool {
        matches!(self, Segment::Text(_))
    }

    pub fn as_text(&self) -> Option<&TextChunk> {
        match self {
            Segment::Text(chunk) => Some(chunk),
            _ => None,
        }
    }

    /// `canMerge`: contiguous text in the same slot with the same flags (tab presence aside).
    pub(crate) fn can_merge(left: &Segment, right: &Segment) -> bool {
        let (Some(l), Some(r)) = (left.as_text(), right.as_text()) else { return false };
        l.mem_id == r.mem_id && l.byte_end == r.byte_start && (l.flags & !HAS_TAB) == (r.flags & !HAS_TAB)
    }
}

/// `findJunction`: where a grapheme crosses the end of `left` into `right` (`left[tail..] ++ right[..head]`), or
/// `None` when the junction is a grapheme break.
pub(crate) fn find_junction(left: &[u8], right: &[u8], method: WidthMethod) -> Option<(u32, u32)> {
    debug_assert!(!right.is_empty());
    if left.is_empty() {
        return None;
    }
    let mut pos = codepoint_start_before(left, left.len());
    let mut cp = utf8::decode(left, pos).0;
    if breaks_in_every_state(cp, utf8::decode(right, 0).0, method) {
        return None;
    }
    while pos > 0 {
        let prev_pos = codepoint_start_before(left, pos);
        let prev_cp = utf8::decode(left, prev_pos).0;
        if breaks_in_every_state(prev_cp, cp, method) {
            break;
        }
        pos = prev_pos;
        cp = prev_cp;
    }
    let mut state = BreakState::Default;
    let mut prev: Option<u32> = None;
    let mut tail = pos;
    while pos < left.len() {
        cp = utf8::decode(left, pos).0;
        if utf8::is_grapheme_break(prev, cp, &mut state, method) {
            tail = pos;
        }
        pos += utf8::decode(left, pos).1;
        prev = Some(cp);
    }
    let mut head = 0;
    while head < right.len() {
        cp = utf8::decode(right, head).0;
        if utf8::is_grapheme_break(prev, cp, &mut state, method) {
            break;
        }
        prev = Some(cp);
        head += utf8::decode(right, head).1;
    }
    if head == 0 {
        return None;
    }
    Some((tail as u32, head as u32))
}

fn breaks_in_every_state(prev: u32, cp: u32, method: WidthMethod) -> bool {
    BreakState::ALL.iter().all(|&initial| {
        let mut state = initial;
        utf8::is_grapheme_break(Some(prev), cp, &mut state, method)
    })
}

fn codepoint_start_before(bytes: &[u8], end: usize) -> usize {
    let mut pos = end - 1;
    while pos > 0 && bytes[pos] & 0xC0 == 0x80 {
        pos -= 1;
    }
    pos
}

/// The rewrite of a join boundary (`BoundaryAction`); the only insertion is a line-start marker.
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct BoundaryAction {
    pub delete_left: bool,
    pub delete_right: bool,
    pub insert_linestart: bool,
}

/// `rewriteBoundary`: keeps every line starting with a line-start marker.
pub(crate) fn rewrite_boundary(left: Option<&Segment>, right: Option<&Segment>) -> BoundaryAction {
    if right.is_none() && left.is_some_and(|l| l.is_break()) {
        return BoundaryAction { insert_linestart: true, ..Default::default() };
    }
    let (Some(l), Some(r)) = (left, right) else { return BoundaryAction::default() };
    if l.is_line_start() && r.is_line_start() {
        return BoundaryAction { delete_right: true, ..Default::default() };
    }
    if l.is_break() && (r.is_break() || r.is_text()) {
        return BoundaryAction { insert_linestart: true, ..Default::default() };
    }
    if l.is_text() && r.is_line_start() {
        return BoundaryAction { delete_right: true, ..Default::default() };
    }
    BoundaryAction::default()
}

/// `rewriteEnds`: a rope starts with a line-start marker, even when empty.
pub(crate) fn rewrite_ends(first: Option<&Segment>) -> bool {
    match first {
        Some(segment) => !segment.is_line_start(),
        None => true,
    }
}
