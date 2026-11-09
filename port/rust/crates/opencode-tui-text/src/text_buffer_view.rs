//! Port of `text-buffer-view.zig` (`UnifiedTextBufferView`): the layout of a text buffer into virtual (visual)
//! lines under a wrap mode and width, the viewport, truncation, selection and measuring.
//!
//! The view does not hold its buffer: every method takes the buffer it lays out (an editor view passes its
//! placeholder buffer while that shows). Divergences: a layout is always rebuilt whole when the text or a setting
//! changed (the Zig view re-lays out only the changed lines, and keeps a per-line measure ledger; both give the
//! same lines). Word wrapping scans chunks for wrap opportunities each time instead of caching the lists.

use crate::iterators::{self, Coords};
use crate::rope::{Node, NodeRef, Position, Tree};
use crate::segment::{Segment, StyleSpan, TextChunk, ASCII_ONLY, STATIC_ELLIPSIS};
use crate::text_buffer::TextBufferCore;
use crate::utf8::{self, LayoutVisitor, LayoutWrapBreak, LayoutWrapBreakKind, WordClass};
use crate::Rgba;
use std::rc::{Rc, Weak};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum WrapMode {
    #[default]
    None,
    Char,
    Word,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// How a selection occupies cells between its anchor and focus (`SelectionOccupancy`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SelectionOccupancy {
    /// The grapheme under the max endpoint is included (block cursor).
    #[default]
    Cell,
    /// The half-open range `[min, max)` (thin caret).
    Boundary,
}

/// How local coordinates expand into a selection (`SelectionBehavior`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SelectionBehavior {
    #[default]
    Cell,
    Word,
    Line,
}

/// A selection: an exclusive offset range and its colors (`TextSelection`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextSelection {
    pub start: u32,
    pub end: u32,
    pub bg_color: Option<Rgba>,
    pub fg_color: Option<Rgba>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Viewport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Rows and widest row of a measured layout (`MeasureResult`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MeasureResult {
    pub line_count: u32,
    pub width_cols_max: u32,
}

/// The visual lines of a layout (`LineInfo`), owned.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LineInfo {
    pub line_start_cols: Vec<u32>,
    pub line_width_cols: Vec<u32>,
    pub line_sources: Vec<u32>,
    pub line_wraps: Vec<u32>,
    pub line_width_cols_max: u32,
}

/// A whole-grapheme slice of a chunk on a visual line (`VirtualChunk`).
#[derive(Clone)]
pub struct VirtualChunk {
    leaf: NodeRef,
    pub byte_start_in_chunk: u32,
    pub byte_len: u32,
    pub col_start_in_chunk: u32,
    pub width_cols: u32,
}

impl VirtualChunk {
    /// The text chunk this slice is of.
    pub fn chunk(&self) -> &TextChunk {
        match self.leaf.segment() {
            Some(Segment::Text(chunk)) => chunk,
            _ => unreachable!("virtual chunks are text"),
        }
    }

    fn same_chunk(&self, leaf: &NodeRef) -> bool {
        Rc::ptr_eq(&self.leaf, leaf)
    }
}

/// A visual line (`VirtualLine`).
#[derive(Clone)]
pub struct VirtualLine {
    pub chunks: Vec<VirtualChunk>,
    pub width_cols: u32,
    pub document_cell_offset: u32,
    pub source_line: u32,
    pub source_col_start: u32,
    pub is_truncated: bool,
    pub ellipsis_col: u32,
    pub truncation_suffix_col_start: u32,
}

impl VirtualLine {
    fn new(document_cell_offset: u32) -> Self {
        VirtualLine {
            chunks: Vec::new(),
            width_cols: 0,
            document_cell_offset,
            source_line: 0,
            source_col_start: 0,
            is_truncated: false,
            ellipsis_col: 0,
            truncation_suffix_col_start: 0,
        }
    }

    fn push_chunk(&mut self, chunk: VirtualChunk) {
        if self.chunks.capacity() == 0 {
            self.chunks.reserve_exact(1);
        }
        self.chunks.push(chunk);
    }
}

/// The spans of a visual line's source line (`VirtualLineSpanInfo`).
pub struct VirtualLineSpanInfo<'a> {
    pub spans: &'a [StyleSpan],
    pub source_line: u32,
    pub source_col_start: u32,
}

/// `alignmentPadCols`: left padding to align a line of `line_width` within `viewport_width`.
pub fn alignment_pad_cols(alignment: TextAlign, viewport_width: u32, line_width: u32) -> u32 {
    if line_width >= viewport_width {
        return 0;
    }
    match alignment {
        TextAlign::Left => 0,
        TextAlign::Center => (viewport_width - line_width) / 2,
        TextAlign::Right => viewport_width - line_width,
    }
}

/// Ghostty-style double-click word boundaries.
const WORD_BOUNDARIES: [u32; 19] = [
    ' ' as u32, '\t' as u32, '\'' as u32, '"' as u32, '│' as u32, '`' as u32, '|' as u32, ':' as u32, ';' as u32,
    ',' as u32, '(' as u32, ')' as u32, '[' as u32, ']' as u32, '{' as u32, '}' as u32, '<' as u32, '>' as u32,
    '$' as u32,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SelectionRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SelectionEndpoints {
    pub anchor: u32,
    pub focus: u32,
}

struct SelectionGrapheme {
    start: u32,
    end: u32,
    first_cp: u32,
    is_break: bool,
}

#[derive(Clone)]
struct Baseline {
    root: Weak<Node>,
    tab_generation: u64,
    buffer_id: u64,
}

#[derive(Default)]
struct LayoutArrays {
    virtual_lines: Vec<VirtualLine>,
    starts: Vec<u32>,
    widths: Vec<u32>,
    sources: Vec<u32>,
    wraps: Vec<u32>,
    first_vline: Vec<u32>,
    vline_counts: Vec<u32>,
}

/// The layout state of one view (`UnifiedTextBufferView`).
pub(crate) struct TextBufferViewCore {
    pub selection: Option<TextSelection>,
    pub selection_endpoints: Option<SelectionEndpoints>,
    pub selection_occupancy: SelectionOccupancy,
    selection_behavior: SelectionBehavior,
    pub viewport: Option<Viewport>,
    pub wrap_width: Option<u32>,
    pub wrap_mode: WrapMode,
    pub text_align: TextAlign,
    pub first_line_offset: u32,
    draw_y: i32,
    layout: LayoutArrays,
    pub virtual_lines_dirty: bool,
    baseline: Option<Baseline>,
    pub tab_indicator: Option<u32>,
    pub tab_indicator_color: Option<Rgba>,
    pub truncate: bool,
    pub(crate) truncation_applied: bool,
    ellipsis: NodeRef,
    measure_cache: [(u32, MeasureResult); 3],
    measure_count: u8,
    measure_wrap_mode: WrapMode,
    measure_first_line_offset: u32,
    measure_epoch: u64,
    measure_buffer: Option<u64>,
}

impl TextBufferViewCore {
    pub fn new() -> Self {
        TextBufferViewCore {
            selection: None,
            selection_endpoints: None,
            selection_occupancy: SelectionOccupancy::Cell,
            selection_behavior: SelectionBehavior::Cell,
            viewport: None,
            wrap_width: None,
            wrap_mode: WrapMode::None,
            text_align: TextAlign::Left,
            first_line_offset: 0,
            draw_y: 0,
            layout: LayoutArrays::default(),
            virtual_lines_dirty: true,
            baseline: None,
            tab_indicator: None,
            tab_indicator_color: None,
            truncate: false,
            truncation_applied: false,
            ellipsis: Tree::detached_leaf(Segment::Text(TextChunk::new(0, ASCII_ONLY | STATIC_ELLIPSIS, 0, 3, 3))),
            measure_cache: [(0, MeasureResult::default()); 3],
            measure_count: 0,
            measure_wrap_mode: WrapMode::None,
            measure_first_line_offset: 0,
            measure_epoch: 0,
            measure_buffer: None,
        }
    }

    fn current_baseline(tb: &TextBufferCore) -> Baseline {
        Baseline { root: Rc::downgrade(tb.rope.root()), tab_generation: tb.tab_metrics_generation, buffer_id: tb.id }
    }

    fn baseline_matches(&self, tb: &TextBufferCore) -> bool {
        match &self.baseline {
            Some(b) => {
                b.tab_generation == tb.tab_metrics_generation
                    && b.buffer_id == tb.id
                    && std::ptr::eq(b.root.as_ptr(), Rc::as_ptr(tb.rope.root()))
            }
            None => false,
        }
    }

    pub fn set_viewport(&mut self, vp: Option<Viewport>) {
        self.viewport = vp;
        if let Some(vp) = vp {
            if self.wrap_width != Some(vp.width) {
                self.wrap_width = Some(vp.width);
                self.virtual_lines_dirty = true;
                self.truncation_applied = false;
            }
        } else {
            self.truncation_applied = false;
        }
    }

    pub fn set_viewport_size(&mut self, width: u32, height: u32) {
        let (x, y) = self.viewport.map_or((0, 0), |vp| (vp.x, vp.y));
        self.set_viewport(Some(Viewport { x, y, width, height }));
    }

    pub fn set_wrap_width(&mut self, width: Option<u32>) {
        if self.wrap_width != width {
            self.wrap_width = width;
            self.virtual_lines_dirty = true;
            self.truncation_applied = false;
        }
    }

    pub fn set_wrap_mode(&mut self, mode: WrapMode) {
        if self.wrap_mode != mode {
            self.wrap_mode = mode;
            self.virtual_lines_dirty = true;
            self.truncation_applied = false;
        }
    }

    pub fn set_draw_y(&mut self, y: i32) {
        self.draw_y = y;
    }

    pub fn line_alignment_pad(&self, vline_idx: usize, line_width: u32) -> u32 {
        let Some(vp) = self.viewport else { return 0 };
        if vline_idx == 0 && vp.y == 0 && self.draw_y == 0 && self.first_line_offset > 0 {
            return 0;
        }
        alignment_pad_cols(self.text_align, vp.width, line_width)
    }

    pub fn set_first_line_offset(&mut self, offset: u32) {
        if self.first_line_offset != offset {
            self.first_line_offset = offset;
            self.virtual_lines_dirty = true;
            self.truncation_applied = false;
        }
    }

    pub fn set_truncate(&mut self, truncate: bool) {
        if self.truncate != truncate {
            self.truncate = truncate;
            self.virtual_lines_dirty = true;
            self.truncation_applied = false;
        }
    }

    /// `switchToBuffer`: the next layout reads another buffer.
    pub fn switch_buffer(&mut self, from: u64, to: u64) {
        if from != to {
            self.measure_count = 0;
        }
        self.virtual_lines_dirty = true;
        self.baseline = None;
    }

    pub fn needs_prepare(&self, tb: &TextBufferCore) -> bool {
        self.virtual_lines_dirty || !self.baseline_matches(tb) || (self.truncate && self.viewport.is_some() && !self.truncation_applied)
    }

    pub fn update_virtual_lines(&mut self, tb: &TextBufferCore) {
        if !self.virtual_lines_dirty && self.baseline_matches(tb) {
            return;
        }
        self.layout = LayoutArrays::default();
        self.truncation_applied = false;
        self.lay_out(tb);
        self.baseline = Some(Self::current_baseline(tb));
        self.virtual_lines_dirty = false;
    }

    fn lay_out(&mut self, tb: &TextBufferCore) {
        let range = LineRange::full();
        match (self.wrap_mode, self.wrap_width) {
            (WrapMode::None, _) | (_, None) => unwrapped_layout(tb, range, &mut self.layout),
            (WrapMode::Char, Some(width)) => {
                Wrap::<true, false>::run(tb, range, width, self.first_line_offset, Output::Render(&mut self.layout))
            }
            (WrapMode::Word, Some(width)) => {
                Wrap::<true, true>::run(tb, range, width, self.first_line_offset, Output::Render(&mut self.layout))
            }
        }
    }

    pub fn virtual_lines_all(&self) -> &[VirtualLine] {
        &self.layout.virtual_lines
    }

    /// The start offsets of every visual line (parallel to `virtual_lines_all`).
    pub fn line_starts(&self) -> &[u32] {
        &self.layout.starts
    }

    pub fn virtual_line_count(&mut self, tb: &TextBufferCore) -> u32 {
        self.update_virtual_lines(tb);
        self.layout.virtual_lines.len() as u32
    }

    /// `getVirtualLines`: the lines in the viewport, truncated when enabled.
    pub fn virtual_lines(&mut self, tb: &TextBufferCore) -> &[VirtualLine] {
        self.update_virtual_lines(tb);
        if self.truncate && self.viewport.is_some() {
            self.ensure_truncation(tb);
        }
        let all = &self.layout.virtual_lines;
        match self.viewport {
            Some(vp) => {
                let start = (vp.y as usize).min(all.len());
                let end = (start + vp.height as usize).min(all.len());
                &all[start..end]
            }
            None => all,
        }
    }

    /// `getCachedLineInfo`: the viewport's lines (all lines without a viewport).
    pub fn cached_line_info(&mut self, tb: &TextBufferCore) -> LineInfo {
        self.update_virtual_lines(tb);
        let l = &self.layout;
        if let Some(vp) = self.viewport {
            let start = (vp.y as usize).min(l.starts.len());
            let end = (start + vp.height as usize).min(l.starts.len());
            let widths = l.widths[start..end].to_vec();
            let max = widths.iter().copied().max().unwrap_or(0);
            return LineInfo {
                line_start_cols: l.starts[start..end].to_vec(),
                line_width_cols: widths,
                line_sources: l.sources[start..end].to_vec(),
                line_wraps: l.wraps[start..end].to_vec(),
                line_width_cols_max: max,
            };
        }
        self.logical_line_info_window(tb, 0, usize::MAX)
    }

    /// `getLogicalLineInfo`: every visual line.
    pub fn logical_line_info(&mut self, tb: &TextBufferCore) -> LineInfo {
        self.update_virtual_lines(tb);
        self.logical_line_info_window(tb, 0, usize::MAX)
    }

    pub fn logical_line_info_window(&mut self, tb: &TextBufferCore, first: usize, count: usize) -> LineInfo {
        self.update_virtual_lines(tb);
        let l = &self.layout;
        let start = first.min(l.starts.len());
        let end = start.saturating_add(count).min(l.starts.len());
        LineInfo {
            line_start_cols: l.starts[start..end].to_vec(),
            line_width_cols: l.widths[start..end].to_vec(),
            line_sources: l.sources[start..end].to_vec(),
            line_wraps: l.wraps[start..end].to_vec(),
            line_width_cols_max: tb.line_width_cols_max(),
        }
    }

    pub fn find_visual_line_index(&mut self, tb: &TextBufferCore, logical_row: u32, logical_col: u32) -> u32 {
        self.update_virtual_lines(tb);
        let l = &self.layout;
        if l.virtual_lines.is_empty() {
            return 0;
        }
        let rows = l.first_vline.len();
        let row = if logical_row as usize >= rows { rows.saturating_sub(1) } else { logical_row as usize };
        if row >= rows {
            return 0;
        }
        let first = l.first_vline[row];
        let count = l.vline_counts[row];
        if count == 0 {
            return first;
        }
        for i in 1..count {
            let idx = first + i;
            if idx as usize >= l.virtual_lines.len() {
                break;
            }
            if logical_col < l.virtual_lines[idx as usize].source_col_start {
                return idx - 1;
            }
        }
        let last = first + count - 1;
        if (last as usize) < l.virtual_lines.len() { last } else { first }
    }

    fn clear_selection_endpoints(&mut self) {
        self.selection_endpoints = None;
    }

    fn offset_selection_range(tb: &TextBufferCore, start: u32, end: u32) -> SelectionRange {
        let text_end = tb.text_end_offset();
        let mut range_start = start.min(end).min(text_end);
        let mut range_end = start.max(end).min(text_end);
        if range_start == range_end {
            if let Some(b) = tb.grapheme_bounds_at_offset(range_start) {
                range_start = b.start;
            }
            return SelectionRange { start: range_start, end: range_start };
        }
        if let Some(b) = tb.grapheme_bounds_at_offset(range_start) {
            range_start = b.start;
        }
        if let Some(b) = tb.grapheme_bounds_at_offset(range_end)
            && b.start < range_end
        {
            range_end = b.end;
        }
        SelectionRange { start: range_start, end: range_end.min(text_end) }
    }

    pub fn set_selection(&mut self, tb: &TextBufferCore, start: u32, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        self.clear_selection_endpoints();
        let range = Self::offset_selection_range(tb, start, end);
        self.selection = Some(TextSelection { start: range.start, end: range.end, bg_color: bg, fg_color: fg });
    }

    pub fn update_selection(&mut self, tb: &TextBufferCore, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        if let Some(sel) = self.selection {
            self.clear_selection_endpoints();
            let range = Self::offset_selection_range(tb, sel.start, end);
            self.selection = Some(TextSelection { start: range.start, end: range.end, bg_color: bg, fg_color: fg });
        }
    }

    pub fn set_selection_colors(&mut self, bg: Option<Rgba>, fg: Option<Rgba>) {
        if let Some(sel) = self.selection.as_mut() {
            sel.bg_color = bg;
            sel.fg_color = fg;
        }
    }

    pub fn reset_selection(&mut self) {
        self.selection = None;
        self.clear_selection_endpoints();
        self.selection_behavior = SelectionBehavior::Cell;
    }

    pub fn set_selection_occupancy(&mut self, tb: &TextBufferCore, occupancy: SelectionOccupancy) {
        self.selection_occupancy = occupancy;
        let (Some(mut endpoints), Some(_)) = (self.selection_endpoints, self.selection) else { return };
        let text_end = self.text_end_offset(tb);
        endpoints.anchor = endpoints.anchor.min(text_end);
        endpoints.focus = endpoints.focus.min(text_end);
        self.selection_endpoints = Some(endpoints);
        let range = self
            .expand_selection_range(tb, endpoints.anchor, endpoints.focus, text_end, self.selection_behavior)
            .unwrap_or_else(|| self.selection_range(tb, endpoints.anchor, endpoints.focus, text_end));
        if let Some(sel) = self.selection.as_mut() {
            sel.start = range.start;
            sel.end = range.end;
        }
    }

    /// `setSelectionInclusiveStyle`: under cell occupancy, `end` includes the grapheme at it.
    pub fn set_selection_inclusive(&mut self, tb: &TextBufferCore, start: u32, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        let lo = start.min(end);
        let hi = start.max(end);
        let exclusive_end = if self.selection_occupancy == SelectionOccupancy::Cell {
            tb.grapheme_bounds_at_offset(hi).map_or(hi, |b| b.end)
        } else {
            hi
        };
        self.set_selection(tb, lo, exclusive_end, bg, fg);
    }

    pub fn set_local_selection(
        &mut self,
        tb: &TextBufferCore,
        anchor_x: i32,
        anchor_y: i32,
        focus_x: i32,
        focus_y: i32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        behavior: SelectionBehavior,
    ) -> bool {
        self.update_virtual_lines(tb);
        if self.truncate && self.viewport.is_some() {
            self.ensure_truncation(tb);
        }
        let anchor_above = anchor_y < 0;
        let focus_above = focus_y < 0;
        let max_y = self.layout.virtual_lines.len() as i32 - 1;
        let anchor_below = anchor_y > max_y;
        let focus_below = focus_y > max_y;
        if (anchor_above && focus_above) || (anchor_below && focus_below) {
            let had = self.selection.is_some();
            self.reset_selection();
            return had;
        }
        let text_end = self.text_end_offset(tb);
        let anchor = if anchor_below {
            text_end
        } else if anchor_above || anchor_x < 0 {
            0
        } else {
            match self.coords_to_char_offset(tb, anchor_x, anchor_y) {
                Some(o) => o,
                None => {
                    let had = self.selection.is_some();
                    self.reset_selection();
                    return had;
                }
            }
        };
        let focus = if focus_below {
            text_end
        } else if focus_above || focus_x < 0 {
            0
        } else {
            match self.coords_to_char_offset(tb, focus_x, focus_y) {
                Some(o) => o,
                None => {
                    let had = self.selection.is_some();
                    self.reset_selection();
                    return had;
                }
            }
        };
        self.selection_endpoints = Some(SelectionEndpoints { anchor, focus });
        self.selection_behavior = behavior;
        let range = self
            .expand_selection_range(tb, anchor, focus, text_end, behavior)
            .unwrap_or_else(|| self.selection_range(tb, anchor, focus, text_end));
        let new_selection = TextSelection { start: range.start, end: range.end, bg_color: bg, fg_color: fg };
        let changed = match self.selection {
            Some(old) => old.start != new_selection.start || old.end != new_selection.end,
            None => true,
        };
        self.selection = Some(new_selection);
        changed
    }

    pub fn update_local_selection(
        &mut self,
        tb: &TextBufferCore,
        anchor_x: i32,
        anchor_y: i32,
        focus_x: i32,
        focus_y: i32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        behavior: SelectionBehavior,
    ) -> bool {
        if self.selection_endpoints.is_some() {
            self.update_local_selection_focus_only(tb, focus_x, focus_y, bg, fg, behavior)
        } else {
            self.set_local_selection(tb, anchor_x, anchor_y, focus_x, focus_y, bg, fg, behavior)
        }
    }

    fn update_local_selection_focus_only(
        &mut self,
        tb: &TextBufferCore,
        focus_x: i32,
        focus_y: i32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        behavior: SelectionBehavior,
    ) -> bool {
        let Some(endpoints) = self.selection_endpoints else { return false };
        let anchor = endpoints.anchor;
        self.update_virtual_lines(tb);
        if self.truncate && self.viewport.is_some() {
            self.ensure_truncation(tb);
        }
        let focus_above = focus_y < 0;
        let max_y = self.layout.virtual_lines.len() as i32 - 1;
        let focus_below = focus_y > max_y;
        let text_end = self.text_end_offset(tb);
        let focus = if focus_below {
            text_end
        } else if focus_above || focus_x < 0 {
            0
        } else {
            match self.coords_to_char_offset(tb, focus_x, focus_y) {
                Some(o) => o,
                None => return false,
            }
        };
        if let Some(e) = self.selection_endpoints.as_mut() {
            e.focus = focus;
        }
        self.selection_behavior = behavior;
        let Some(range) = self.expand_selection_range(tb, anchor, focus, text_end, behavior) else { return false };
        self.selection = Some(TextSelection { start: range.start, end: range.end, bg_color: bg, fg_color: fg });
        true
    }

    /// `convertSelectionToCell`.
    pub fn convert_selection_to_cell(&mut self, tb: &TextBufferCore) -> bool {
        let Some(sel) = self.selection else { return false };
        if sel.start == sel.end {
            return false;
        }
        let focus = if self.selection_occupancy == SelectionOccupancy::Boundary {
            sel.end
        } else {
            tb.grapheme_bounds_at_offset(sel.end - 1).map_or(sel.end - 1, |b| b.start)
        };
        self.selection_endpoints = Some(SelectionEndpoints { anchor: sel.start, focus });
        self.selection_behavior = SelectionBehavior::Cell;
        true
    }

    fn expand_selection_range(
        &mut self,
        tb: &TextBufferCore,
        anchor: u32,
        focus: u32,
        text_end: u32,
        behavior: SelectionBehavior,
    ) -> Option<SelectionRange> {
        match behavior {
            SelectionBehavior::Cell => Some(self.selection_range(tb, anchor, focus, text_end)),
            SelectionBehavior::Word => {
                let a = select_word_between(tb, anchor, focus);
                let b = select_word_between(tb, focus, anchor);
                union_ranges(a, b)
            }
            SelectionBehavior::Line => {
                let a = select_line(tb, anchor);
                let b = select_line(tb, focus);
                union_ranges(a, b)
            }
        }
    }

    fn selection_range(&self, tb: &TextBufferCore, anchor: u32, focus: u32, text_end: u32) -> SelectionRange {
        let mut start = anchor.min(focus);
        let mut end = anchor.max(focus);
        if anchor == focus {
            let offset = start.min(text_end);
            return SelectionRange { start: offset, end: offset };
        }
        if let Some(b) = tb.grapheme_bounds_at_offset(start) {
            start = b.start;
        }
        if let Some(b) = tb.grapheme_bounds_at_offset(end)
            && (self.selection_occupancy == SelectionOccupancy::Cell || b.start < end)
        {
            end = b.end;
        }
        SelectionRange { start: start.min(text_end), end: end.min(text_end) }
    }

    pub fn reset_local_selection(&mut self) {
        self.reset_selection();
    }

    pub(crate) fn text_end_offset(&mut self, tb: &TextBufferCore) -> u32 {
        self.update_virtual_lines(tb);
        if self.truncate && self.viewport.is_some() {
            self.ensure_truncation(tb);
        }
        let Some(last) = self.layout.virtual_lines.last() else { return 0 };
        if last.is_truncated {
            return last.document_cell_offset
                + last.truncation_suffix_col_start
                + last.width_cols.saturating_sub(last.ellipsis_col).saturating_sub(3);
        }
        tb.text_end_offset()
    }

    fn max_local_x(&self, idx: usize) -> u32 {
        let vlines = &self.layout.virtual_lines;
        let vline = &vlines[idx];
        if vline.width_cols == 0 {
            return 0;
        }
        if self.selection_occupancy == SelectionOccupancy::Cell
            && idx + 1 < vlines.len()
            && vlines[idx + 1].source_line == vline.source_line
        {
            return vline.width_cols - 1;
        }
        vline.width_cols
    }

    fn coords_to_char_offset(&mut self, tb: &TextBufferCore, x: i32, y: i32) -> Option<u32> {
        self.update_virtual_lines(tb);
        if self.truncate && self.viewport.is_some() {
            self.ensure_truncation(tb);
        }
        let y_offset = self.viewport.map_or(0, |vp| vp.y as i32);
        let x_offset = match self.viewport {
            Some(vp) if self.wrap_mode == WrapMode::None => vp.x as i32,
            _ => 0,
        };
        let count = self.layout.virtual_lines.len();
        if count == 0 {
            return Some(0);
        }
        let abs_y = y + y_offset;
        let abs_x = x + x_offset;
        let clamped_y = abs_y.min(count as i32 - 1).max(0);
        let idx = clamped_y as usize;
        let max_local_x = self.max_local_x(idx);
        let vline = &self.layout.virtual_lines[idx];
        let line_start = vline.document_cell_offset;
        let pad = self.line_alignment_pad(idx, vline.width_cols) as i32;
        let aligned = abs_x - pad;
        let mut local_x = aligned.min(max_local_x as i32).max(0);
        if vline.is_truncated {
            let lx = local_x as u32;
            if lx >= vline.ellipsis_col && lx < vline.ellipsis_col + 3 {
                local_x = vline.ellipsis_col as i32;
            } else if lx >= vline.ellipsis_col + 3 {
                local_x = (vline.truncation_suffix_col_start + (lx - vline.ellipsis_col - 3)) as i32;
            }
        }
        if !vline.is_truncated && local_x == vline.width_cols as i32 {
            let rendered_end = vline.source_col_start + vline.width_cols;
            let next = idx + 1;
            let has_next_same = next < count && self.layout.virtual_lines[next].source_line == vline.source_line;
            if has_next_same && self.layout.virtual_lines[next].source_col_start > rendered_end {
                return Some(self.layout.virtual_lines[next].document_cell_offset);
            }
            if !has_next_same {
                let logical_width = tb.line_width_at(vline.source_line);
                if logical_width > rendered_end {
                    return Some(line_start - vline.source_col_start + logical_width);
                }
            }
        }
        Some(line_start + local_x as u32)
    }

    /// `packSelectionInfo` as an option: `None` for no or an empty selection.
    pub fn selection_info(&self) -> Option<(u32, u32)> {
        let sel = self.selection?;
        if sel.start == sel.end {
            return None;
        }
        Some((sel.start, sel.end))
    }

    pub fn selected_text(&self, tb: &TextBufferCore) -> Vec<u8> {
        match self.selection {
            Some(sel) => tb.copy_text_range(sel.start, sel.end),
            None => Vec::new(),
        }
    }

    pub fn virtual_line_spans<'a>(&self, tb: &'a TextBufferCore, vline_idx: usize) -> VirtualLineSpanInfo<'a> {
        let Some(vline) = self.layout.virtual_lines.get(vline_idx) else {
            return VirtualLineSpanInfo { spans: &[], source_line: 0, source_col_start: 0 };
        };
        VirtualLineSpanInfo {
            spans: tb.line_spans(vline.source_line as usize),
            source_line: vline.source_line,
            source_col_start: vline.source_col_start,
        }
    }

    fn ensure_truncation(&mut self, tb: &TextBufferCore) {
        if !self.truncate || self.viewport.is_none() || self.truncation_applied {
            return;
        }
        self.apply_truncation(tb);
        self.truncation_applied = true;
    }

    fn apply_truncation(&mut self, tb: &TextBufferCore) {
        let Some(vp) = self.viewport else { return };
        if vp.width == 0 {
            return;
        }
        const ELLIPSIS: u32 = 3;
        let s = tb.settings();
        let keep_prefix = |chunk: &VirtualChunk, keep: u32| -> Option<VirtualChunk> {
            if keep == 0 {
                return None;
            }
            if keep >= chunk.width_cols {
                return Some(chunk.clone());
            }
            let bytes = chunk.chunk().bytes(&tb.mem_registry);
            let window = &bytes[chunk.byte_start_in_chunk as usize..(chunk.byte_start_in_chunk + chunk.byte_len) as usize];
            let fit = utf8::find_wrap_pos_by_width_grapheme_safe(window, keep, s.tab_width, s.control_display, chunk.chunk().is_ascii_only(), s.method);
            if fit.byte_offset == 0 || fit.columns_used == 0 {
                return None;
            }
            let mut partial = chunk.clone();
            partial.byte_len = fit.byte_offset;
            partial.width_cols = fit.columns_used;
            Some(partial)
        };
        let drop_prefix = |chunk: &VirtualChunk, drop: u32| -> Option<VirtualChunk> {
            if drop == 0 {
                return Some(chunk.clone());
            }
            if drop >= chunk.width_cols {
                return None;
            }
            let bytes = chunk.chunk().bytes(&tb.mem_registry);
            let window = &bytes[chunk.byte_start_in_chunk as usize..(chunk.byte_start_in_chunk + chunk.byte_len) as usize];
            let dropped =
                utf8::find_grapheme_pos_by_width(window, drop, s.tab_width, s.control_display, chunk.chunk().is_ascii_only(), true, s.method);
            if dropped.byte_offset >= chunk.byte_len || dropped.columns_used >= chunk.width_cols {
                return None;
            }
            let mut partial = chunk.clone();
            partial.byte_start_in_chunk += dropped.byte_offset;
            partial.byte_len -= dropped.byte_offset;
            partial.col_start_in_chunk += dropped.columns_used;
            partial.width_cols -= dropped.columns_used;
            Some(partial)
        };
        let ellipsis = self.ellipsis.clone();
        for vline in self.layout.virtual_lines.iter_mut() {
            if vline.width_cols <= vp.width {
                continue;
            }
            let mut chunks: Vec<VirtualChunk> = Vec::new();
            let mut suffix_col_start = vline.width_cols;
            if vp.width <= ELLIPSIS {
                vline.chunks = chunks;
                vline.is_truncated = true;
                vline.width_cols = 0;
                vline.ellipsis_col = 0;
                vline.truncation_suffix_col_start = suffix_col_start;
                continue;
            }
            let available = vp.width - ELLIPSIS;
            let prefix_width = available / 2;
            let suffix_width = available - prefix_width;
            let mut prefix_acc = 0u32;
            for chunk in &vline.chunks {
                if prefix_acc >= prefix_width && chunk.width_cols != 0 {
                    break;
                }
                let space = prefix_width - prefix_acc;
                if chunk.width_cols <= space {
                    chunks.push(chunk.clone());
                    prefix_acc += chunk.width_cols;
                } else {
                    if let Some(partial) = keep_prefix(chunk, space) {
                        prefix_acc += partial.width_cols;
                        chunks.push(partial);
                    }
                    break;
                }
            }
            chunks.push(VirtualChunk { leaf: ellipsis.clone(), byte_start_in_chunk: 0, byte_len: 3, col_start_in_chunk: 0, width_cols: ELLIPSIS });
            let suffix_start = vline.width_cols - suffix_width;
            let mut pos_acc = 0u32;
            let mut suffix_acc = 0u32;
            let mut actual_suffix_start: Option<u32> = None;
            for chunk in &vline.chunks {
                let chunk_end = pos_acc + chunk.width_cols;
                if chunk_end <= suffix_start {
                    pos_acc += chunk.width_cols;
                    continue;
                }
                if pos_acc >= suffix_start {
                    chunks.push(chunk.clone());
                    if actual_suffix_start.is_none() {
                        actual_suffix_start = Some(pos_acc);
                    }
                    suffix_acc += chunk.width_cols;
                } else if let Some(partial) = drop_prefix(chunk, suffix_start - pos_acc) {
                    if actual_suffix_start.is_none() {
                        actual_suffix_start = Some(pos_acc + partial.col_start_in_chunk - chunk.col_start_in_chunk);
                    }
                    suffix_acc += partial.width_cols;
                    chunks.push(partial);
                }
                pos_acc += chunk.width_cols;
            }
            suffix_col_start = actual_suffix_start.unwrap_or(vline.width_cols);
            vline.chunks = chunks;
            vline.width_cols = prefix_acc + ELLIPSIS + suffix_acc;
            vline.is_truncated = true;
            vline.ellipsis_col = prefix_acc;
            vline.truncation_suffix_col_start = suffix_col_start;
        }
    }

    /// `measureForDimensions`: rows and widest row at `width`, cached for three widths.
    pub fn measure_for_dimensions(&mut self, tb: &TextBufferCore, width: u32) -> MeasureResult {
        let epoch = tb.content_epoch();
        let matches = self.measure_buffer == Some(tb.id)
            && self.measure_epoch == epoch
            && self.measure_wrap_mode == self.wrap_mode
            && self.measure_first_line_offset == self.first_line_offset;
        if matches {
            let count = self.measure_count as usize;
            if let Some(index) = self.measure_cache[..count].iter().position(|e| e.0 == width) {
                let entry = self.measure_cache[index];
                self.measure_cache.copy_within(0..index, 1);
                self.measure_cache[0] = entry;
                return entry.1;
            }
        }
        let result = self.measure_uncached(tb, width);
        if !matches {
            self.measure_count = 0;
        }
        self.measure_count = (self.measure_count + 1).min(3);
        let count = self.measure_count as usize;
        self.measure_cache.copy_within(0..count - 1, 1);
        self.measure_cache[0] = (width, result);
        self.measure_wrap_mode = self.wrap_mode;
        self.measure_first_line_offset = self.first_line_offset;
        self.measure_epoch = epoch;
        self.measure_buffer = Some(tb.id);
        result
    }

    fn measure_uncached(&mut self, tb: &TextBufferCore, width: u32) -> MeasureResult {
        if width == 0 || self.wrap_mode == WrapMode::None {
            return MeasureResult { line_count: tb.line_count(), width_cols_max: tb.line_width_cols_max() };
        }
        if self.wrap_width == Some(width) {
            self.update_virtual_lines(tb);
            return MeasureResult {
                line_count: self.layout.virtual_lines.len() as u32,
                width_cols_max: self.layout.widths.iter().copied().max().unwrap_or(0),
            };
        }
        let mut result = MeasureResult::default();
        let range = LineRange::full();
        match self.wrap_mode {
            WrapMode::None => unreachable!(),
            WrapMode::Char => Wrap::<false, false>::run(tb, range, width, self.first_line_offset, Output::Measure(&mut result)),
            WrapMode::Word => Wrap::<false, true>::run(tb, range, width, self.first_line_offset, Output::Measure(&mut result)),
        }
        result
    }

    pub fn layout_bytes(&self) -> usize {
        let l = &self.layout;
        let mut bytes = l.virtual_lines.capacity() * std::mem::size_of::<VirtualLine>();
        for line in &l.virtual_lines {
            bytes += line.chunks.capacity() * std::mem::size_of::<VirtualChunk>();
        }
        bytes + (l.starts.capacity() + l.widths.capacity() + l.sources.capacity() + l.wraps.capacity()) * 4
            + (l.first_vline.capacity() + l.vline_counts.capacity()) * 4
    }

}

fn union_ranges(a: Option<SelectionRange>, b: Option<SelectionRange>) -> Option<SelectionRange> {
    match (a, b) {
        (None, b) => b,
        (a, None) => a,
        (Some(l), Some(r)) => Some(SelectionRange { start: l.start.min(r.start), end: l.end.max(r.end) }),
    }
}

fn is_word_boundary(cp: u32) -> bool {
    WORD_BOUNDARIES.contains(&cp)
}

fn selection_grapheme_at(tb: &TextBufferCore, offset: u32) -> Option<SelectionGrapheme> {
    let text_end = tb.text_end_offset();
    if offset >= text_end {
        return None;
    }
    let tree = tb.tree();
    let coords = iterators::offset_to_coords(tree, offset)?;
    let line_width = iterators::line_width_at(tree, coords.row);
    let line_start = offset - coords.col;
    if coords.col >= line_width {
        return Some(SelectionGrapheme {
            start: line_start + line_width,
            end: line_start + line_width + 1,
            first_cp: '\n' as u32,
            is_break: true,
        });
    }
    let mut seg_idx = iterators::line_start(tree, coords.row)?.leaf_index + 1;
    let mut cols_before = 0;
    let s = tb.settings();
    let count = tree.count();
    while seg_idx < count {
        let segment = tree.get(seg_idx)?;
        if segment.is_break() || segment.is_line_start() {
            break;
        }
        if let Some(chunk) = segment.as_text() {
            let next_cols = cols_before + chunk.width_cols();
            if coords.col < next_cols {
                let bytes = chunk.bytes(&tb.mem_registry);
                let pos = utf8::find_grapheme_pos_by_width(
                    bytes,
                    coords.col - cols_before,
                    s.tab_width,
                    s.control_display,
                    chunk.is_ascii_only(),
                    false,
                    s.method,
                );
                if pos.byte_offset as usize >= bytes.len() {
                    return None;
                }
                let width = utf8::get_grapheme_width_at(bytes, pos.byte_offset as usize, s.tab_width, s.control_display, s.method);
                let first_cp = utf8::decode(bytes, pos.byte_offset as usize).0;
                let start = line_start + cols_before + pos.columns_used;
                return Some(SelectionGrapheme {
                    start,
                    end: start + width,
                    first_cp,
                    is_break: first_cp == '\n' as u32 || first_cp == '\r' as u32,
                });
            }
            cols_before = next_cols;
        }
        seg_idx += 1;
    }
    None
}

fn prev_selection_grapheme(tb: &TextBufferCore, offset: u32) -> Option<SelectionGrapheme> {
    if offset == 0 {
        return None;
    }
    selection_grapheme_at(tb, offset - 1)
}

/// `selectWord`: the word at `offset`; `None` on a line break or the end.
pub(crate) fn select_word(tb: &TextBufferCore, offset: u32) -> Option<SelectionRange> {
    let g = selection_grapheme_at(tb, offset)?;
    if g.is_break {
        return None;
    }
    let expect = is_word_boundary(g.first_cp);
    let mut start = g.start;
    let mut end = g.end;
    while let Some(prev) = prev_selection_grapheme(tb, start) {
        if prev.is_break || is_word_boundary(prev.first_cp) != expect || prev.start >= start {
            break;
        }
        start = prev.start;
    }
    while let Some(next) = selection_grapheme_at(tb, end) {
        if next.is_break || is_word_boundary(next.first_cp) != expect || next.end <= end {
            break;
        }
        end = next.end;
    }
    Some(SelectionRange { start, end })
}

fn select_word_between(tb: &TextBufferCore, from: u32, toward: u32) -> Option<SelectionRange> {
    let text_end = tb.text_end_offset();
    let mut offset = from.min(text_end);
    let limit = toward.min(text_end);
    if offset <= limit {
        while offset <= limit {
            if let Some(range) = select_word(tb, offset) {
                return Some(range);
            }
            let next = selection_grapheme_at(tb, offset)?;
            if next.end <= offset {
                return None;
            }
            offset = next.end;
        }
        return None;
    }
    loop {
        if let Some(range) = select_word(tb, offset) {
            return Some(range);
        }
        if offset <= limit {
            return None;
        }
        let prev = prev_selection_grapheme(tb, offset)?;
        if prev.start >= offset {
            return None;
        }
        offset = prev.start;
    }
}

fn select_line(tb: &TextBufferCore, offset: u32) -> Option<SelectionRange> {
    let tree = tb.tree();
    if offset > tb.text_end_offset() {
        return None;
    }
    let Coords { row, col } = iterators::offset_to_coords(tree, offset)?;
    let start = offset - col;
    let range = SelectionRange { start, end: start + iterators::line_width_at(tree, row) };
    // trimLineWhitespace
    let mut trimmed_start = range.start;
    let mut trimmed_end = range.end;
    let mut saw = false;
    let mut at = range.start;
    while at < range.end {
        let Some(g) = selection_grapheme_at(tb, at) else { break };
        if g.is_break {
            break;
        }
        if g.first_cp != ' ' as u32 && g.first_cp != '\t' as u32 {
            if !saw {
                trimmed_start = g.start;
            }
            trimmed_end = g.end;
            saw = true;
        }
        if g.end <= at {
            break;
        }
        at = g.end;
    }
    if !saw {
        return Some(range);
    }
    Some(SelectionRange { start: trimmed_start, end: trimmed_end })
}

#[derive(Clone, Copy)]
struct LineRange {
    first: u32,
    start: Position,
    count: u32,
}

impl LineRange {
    fn full() -> Self {
        LineRange { first: 0, start: Position { leaf_index: 0, weight: 0 }, count: u32::MAX }
    }
}

enum Output<'a> {
    Render(&'a mut LayoutArrays),
    Measure(&'a mut MeasureResult),
}

/// `calculateUnwrappedVirtualLines`: one visual line per logical line.
fn unwrapped_layout(tb: &TextBufferCore, range: LineRange, out: &mut LayoutArrays) {
    let mut current = VirtualLine::new(0);
    let tree = tb.tree();
    walk_lines_with_leaves(tree, range, &mut |event| match event {
        WalkEvent::Chunk(leaf, chunk) => current.push_chunk(VirtualChunk {
            leaf: leaf.clone(),
            byte_start_in_chunk: 0,
            byte_len: chunk.byte_len(),
            col_start_in_chunk: 0,
            width_cols: chunk.width_cols(),
        }),
        WalkEvent::LineEnd { line_idx, col_offset, width_cols } => {
            out.first_vline.push(out.virtual_lines.len() as u32);
            out.vline_counts.push(1);
            let mut vline = std::mem::replace(&mut current, VirtualLine::new(0));
            vline.width_cols = width_cols;
            vline.document_cell_offset = col_offset;
            vline.source_line = line_idx;
            vline.source_col_start = 0;
            out.starts.push(col_offset);
            out.widths.push(width_cols);
            out.sources.push(line_idx);
            out.wraps.push(0);
            out.virtual_lines.push(vline);
        }
    });
}

enum WalkEvent<'a> {
    Chunk(&'a NodeRef, &'a TextChunk),
    LineEnd { line_idx: u32, col_offset: u32, width_cols: u32 },
}

/// `walkLineRange`, passing leaf nodes so visual lines can keep their chunks.
fn walk_lines_with_leaves(tree: &Tree, range: LineRange, f: &mut dyn FnMut(WalkEvent)) {
    if tree.count() == 0 || range.count == 0 {
        return;
    }
    let mut line_idx = range.first;
    let mut col_offset = range.start.weight;
    let mut line_start_seg = range.start.leaf_index;
    let mut current_seg = range.start.leaf_index;
    let mut line_width = 0u32;
    let mut lines_left = range.count;
    tree.walk_leaves_from(range.start.leaf_index, &mut |leaf, relative| {
        let idx = range.start.leaf_index + relative;
        match leaf.segment() {
            Some(Segment::Text(chunk)) => {
                f(WalkEvent::Chunk(leaf, chunk));
                line_width += chunk.width_cols();
            }
            Some(Segment::Brk) => {
                f(WalkEvent::LineEnd { line_idx, col_offset, width_cols: line_width });
                line_idx += 1;
                col_offset += line_width + 1;
                line_start_seg = idx + 1;
                line_width = 0;
                lines_left -= 1;
            }
            _ => {}
        }
        current_seg = idx + 1;
        lines_left > 0
    });
    if lines_left == 0 {
        return;
    }
    if line_start_seg < current_seg || line_idx > 0 {
        f(WalkEvent::LineEnd { line_idx, col_offset, width_cols: line_width });
    }
}

#[derive(Clone)]
struct PendingPiece {
    col_start: u32,
    width_cols: u32,
    byte_start: u32,
    byte_end: u32,
    leaf: NodeRef,
}

struct Fit {
    width_cols: u32,
    bytes_used: u32,
}

/// `calculateVirtualLinesGeneric`'s wrap context, for render or measure and char or word wrapping.
struct Wrap<'a, 'o, const RENDER: bool, const WORD: bool> {
    tb: &'a TextBufferCore,
    out: Output<'o>,
    wrap_w: u32,
    current_wrap_width: u32,
    document_cell_offset: u32,
    line_idx: u32,
    source_line_col_offset: u32,
    vline_width: u32,
    current_vline: VirtualLine,
    line_first_vline: u32,
    line_vline_count: u32,
    pending: Vec<PendingPiece>,
    pending_width: u32,
    pending_last_class: WordClass,
    has_non_whitespace: bool,
    word_chunk: Option<NodeRef>,
    word_col_start: u32,
    word_byte_start: u32,
    line_rows: u32,
    line_width: u32,
    failed: bool,
}

impl<'a, 'o, const RENDER: bool, const WORD: bool> Wrap<'a, 'o, RENDER, WORD> {
    fn run(tb: &'a TextBufferCore, range: LineRange, wrap_w: u32, first_line_offset: u32, out: Output<'o>) {
        let current_wrap_width =
            if range.first == 0 && first_line_offset > 0 && first_line_offset < wrap_w { wrap_w - first_line_offset } else { wrap_w };
        let mut w = Wrap::<RENDER, WORD> {
            tb,
            out,
            wrap_w,
            current_wrap_width,
            document_cell_offset: range.start.weight,
            line_idx: range.first,
            source_line_col_offset: 0,
            vline_width: 0,
            current_vline: VirtualLine::new(range.start.weight),
            line_first_vline: 0,
            line_vline_count: 0,
            pending: Vec::new(),
            pending_width: 0,
            pending_last_class: WordClass::Other,
            has_non_whitespace: false,
            word_chunk: None,
            word_col_start: 0,
            word_byte_start: 0,
            line_rows: 0,
            line_width: 0,
            failed: false,
        };
        walk_lines_with_leaves(tb.tree(), range, &mut |event| match event {
            WalkEvent::Chunk(leaf, chunk) => {
                if w.failed {
                    return;
                }
                if WORD {
                    w.process_word_chunk(leaf, chunk);
                } else {
                    w.process_char_chunk(leaf, chunk);
                }
            }
            WalkEvent::LineEnd { width_cols, .. } => w.line_end(width_cols),
        });
    }

    fn word_wrap_width(&self) -> u32 {
        self.current_wrap_width.max(1)
    }

    fn bytes_of(&self, leaf: &NodeRef) -> &'a [u8] {
        match leaf.segment() {
            Some(Segment::Text(chunk)) => chunk.bytes(&self.tb.mem_registry),
            _ => &[],
        }
    }

    fn record_virtual_line(&mut self) {
        match &mut self.out {
            Output::Measure(_) => {
                self.line_rows += 1;
                self.line_width = self.line_width.max(self.vline_width);
            }
            Output::Render(out) => {
                let vline = std::mem::replace(&mut self.current_vline, VirtualLine::new(0));
                out.starts.push(vline.document_cell_offset);
                out.widths.push(vline.width_cols);
                out.sources.push(self.line_idx);
                out.wraps.push(self.line_vline_count);
                out.virtual_lines.push(vline);
            }
        }
    }

    fn commit(&mut self) {
        if RENDER {
            self.current_vline.width_cols = self.vline_width;
            self.current_vline.source_line = self.line_idx;
            self.current_vline.source_col_start = self.source_line_col_offset;
        }
        self.record_virtual_line();
        if RENDER {
            self.line_vline_count += 1;
        }
        self.source_line_col_offset += self.vline_width;
        if RENDER {
            self.current_vline = VirtualLine::new(self.document_cell_offset);
        }
        self.vline_width = 0;
        self.current_wrap_width = self.wrap_w;
    }

    fn add_chunk(&mut self, leaf: &NodeRef, byte_start: u32, byte_len: u32, col_start: u32, width: u32) {
        if byte_len == 0 {
            return;
        }
        if RENDER && WORD
            && let Some(last) = self.current_vline.chunks.last_mut()
            && last.same_chunk(leaf)
            && last.byte_start_in_chunk + last.byte_len == byte_start
            && last.col_start_in_chunk + last.width_cols == col_start
        {
            last.byte_len += byte_len;
            last.width_cols += width;
            self.document_cell_offset += width;
            self.vline_width += width;
            return;
        }
        if RENDER {
            self.current_vline.push_chunk(VirtualChunk {
                leaf: leaf.clone(),
                byte_start_in_chunk: byte_start,
                byte_len,
                col_start_in_chunk: col_start,
                width_cols: width,
            });
        }
        self.document_cell_offset += width;
        self.vline_width += width;
    }

    fn consume_dropped_whitespace(&mut self, width: u32) {
        self.document_cell_offset += width;
        self.source_line_col_offset += width;
        if RENDER {
            self.current_vline.document_cell_offset = self.document_cell_offset;
        }
    }

    fn queue_pending(&mut self, leaf: &NodeRef, col_start: u32, width: u32, byte_start: u32, byte_end: u32) {
        if self.failed || (width == 0 && byte_end <= byte_start) {
            return;
        }
        if let Some(last) = self.pending.last_mut()
            && Rc::ptr_eq(&last.leaf, leaf)
            && last.col_start + last.width_cols == col_start
            && last.byte_end == byte_start
        {
            last.width_cols += width;
            last.byte_end = byte_end;
            self.pending_width += width;
            return;
        }
        self.pending.push(PendingPiece { col_start, width_cols: width, byte_start, byte_end, leaf: leaf.clone() });
        self.pending_width += width;
    }

    fn clear_pending(&mut self) {
        self.pending.clear();
        self.pending_width = 0;
        self.pending_last_class = WordClass::Other;
    }

    fn fit_piece(&mut self, piece: &PendingPiece, max: u32, allow_forced: bool) -> Fit {
        if piece.width_cols <= max {
            return Fit { width_cols: piece.width_cols, bytes_used: piece.byte_end - piece.byte_start };
        }
        let bytes = self.bytes_of(&piece.leaf);
        if piece.byte_start > piece.byte_end || piece.byte_end as usize > bytes.len() {
            self.failed = true;
            return Fit { width_cols: 0, bytes_used: 0 };
        }
        let slice = &bytes[piece.byte_start as usize..piece.byte_end as usize];
        let ascii = match piece.leaf.segment() {
            Some(Segment::Text(c)) => c.is_ascii_only(),
            _ => false,
        };
        let s = self.tb.settings();
        let fit = utf8::find_wrap_pos_by_width_grapheme_safe(slice, max, s.tab_width, s.control_display, ascii, s.method);
        let piece_bytes = piece.byte_end - piece.byte_start;
        if fit.columns_used > 0 && fit.byte_offset > 0 {
            return Fit { width_cols: fit.columns_used.min(piece.width_cols), bytes_used: fit.byte_offset.min(piece_bytes) };
        }
        if !allow_forced || max == 0 {
            let prefix = utf8::find_grapheme_pos_by_width(slice, 0, s.tab_width, s.control_display, ascii, false, s.method);
            return Fit { width_cols: 0, bytes_used: prefix.byte_offset };
        }
        let forced = utf8::find_grapheme_pos_by_width(slice, max, s.tab_width, s.control_display, ascii, true, s.method);
        if forced.columns_used == 0 || forced.byte_offset == 0 {
            self.failed = true;
            return Fit { width_cols: 0, bytes_used: 0 };
        }
        let kept = utf8::find_wrap_pos_by_width_grapheme_safe(slice, forced.columns_used, s.tab_width, s.control_display, ascii, s.method);
        Fit {
            width_cols: forced.columns_used.min(piece.width_cols),
            bytes_used: forced.byte_offset.max(kept.byte_offset).min(piece_bytes),
        }
    }

    fn consume_pending_prefix(&mut self, max: u32) -> bool {
        if max == 0 || self.pending_width == 0 || self.failed {
            return false;
        }
        let before = self.vline_width;
        let mut remaining = max;
        let mut consumed = 0usize;
        while consumed < self.pending.len() {
            let piece = self.pending[consumed].clone();
            if piece.width_cols <= remaining {
                self.add_chunk(&piece.leaf, piece.byte_start, piece.byte_end - piece.byte_start, piece.col_start, piece.width_cols);
                remaining -= piece.width_cols;
                consumed += 1;
                continue;
            }
            let fit = self.fit_piece(&piece, remaining, self.vline_width == before);
            if fit.bytes_used == 0 {
                break;
            }
            self.add_chunk(&piece.leaf, piece.byte_start, fit.bytes_used, piece.col_start, fit.width_cols);
            let p = &mut self.pending[consumed];
            p.col_start += fit.width_cols;
            p.width_cols -= fit.width_cols;
            p.byte_start += fit.bytes_used;
            if p.width_cols > 0 {
                break;
            }
            remaining = 0;
        }
        self.pending.drain(..consumed);
        let consumed_width = self.vline_width - before;
        self.pending_width -= consumed_width;
        consumed_width > 0
    }

    fn append_pending_to_line(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for piece in &pending {
            self.add_chunk(&piece.leaf, piece.byte_start, piece.byte_end - piece.byte_start, piece.col_start, piece.width_cols);
        }
        self.pending = pending;
        self.clear_pending();
    }

    fn finalize_pending(&mut self) {
        while self.pending_width > 0 && !self.failed {
            let limit = self.word_wrap_width();
            if self.vline_width > 0 && self.vline_width + self.pending_width > limit {
                self.commit();
                continue;
            }
            if self.vline_width == 0 && self.pending_width > limit {
                if !self.consume_pending_prefix(limit) {
                    return;
                }
                if self.pending_width > 0 {
                    self.commit();
                }
                continue;
            }
            self.append_pending_to_line();
        }
        if !self.pending.is_empty() && !self.failed {
            self.append_pending_to_line();
        }
    }

    fn place_complete_piece(&mut self, leaf: &NodeRef, col_start: u32, width: u32, byte_start: u32, byte_end: u32) {
        if width == 0 || self.failed {
            return;
        }
        let mut piece = PendingPiece { col_start, width_cols: width, byte_start, byte_end, leaf: leaf.clone() };
        while piece.width_cols > 0 && !self.failed {
            let limit = self.word_wrap_width();
            if piece.width_cols <= limit {
                if self.vline_width > 0 && self.vline_width + piece.width_cols > limit {
                    self.commit();
                }
                self.add_chunk(leaf, piece.byte_start, piece.byte_end - piece.byte_start, piece.col_start, piece.width_cols);
                return;
            }
            if self.vline_width > 0 {
                self.commit();
            }
            let fit = self.fit_piece(&piece, self.word_wrap_width(), true);
            if fit.width_cols == 0 {
                return;
            }
            self.add_chunk(leaf, piece.byte_start, fit.bytes_used, piece.col_start, fit.width_cols);
            piece.col_start += fit.width_cols;
            piece.width_cols -= fit.width_cols;
            piece.byte_start += fit.bytes_used;
            if piece.width_cols > 0 {
                self.commit();
            }
        }
    }

    fn flush_complete_piece(&mut self, leaf: &NodeRef, col_start: u32, width: u32, byte_start: u32, byte_end: u32) {
        if width == 0 || self.failed {
            return;
        }
        if !self.pending.is_empty() {
            self.queue_pending(leaf, col_start, width, byte_start, byte_end);
            self.finalize_pending();
        } else {
            self.place_complete_piece(leaf, col_start, width, byte_start, byte_end);
        }
    }

    fn process_whitespace_break(&mut self, leaf: &NodeRef, col_start: u32, byte_start: u32, b: LayoutWrapBreak) {
        if self.pending.is_empty()
            && b.width_cols > 0
            && b.col_start > col_start
            && self.vline_width + (b.col_end() - col_start) <= self.word_wrap_width()
        {
            self.add_chunk(leaf, byte_start, b.byte_end() - byte_start, col_start, b.col_end() - col_start);
            self.has_non_whitespace = true;
            return;
        }
        if b.col_start > col_start {
            self.flush_complete_piece(leaf, col_start, b.col_start - col_start, byte_start, b.byte_start);
            if self.failed {
                return;
            }
            self.has_non_whitespace = true;
        } else {
            self.queue_pending(leaf, col_start, 0, byte_start, b.byte_start);
            self.finalize_pending();
            if self.failed {
                return;
            }
        }
        let preserve_leading = !self.has_non_whitespace;
        let limit = self.word_wrap_width();
        if !preserve_leading && self.vline_width + b.width_cols > limit {
            if self.vline_width > 0 {
                self.commit();
            }
            self.consume_dropped_whitespace(b.width_cols);
            return;
        }
        if !preserve_leading && self.vline_width == 0 {
            self.consume_dropped_whitespace(b.width_cols);
            return;
        }
        self.place_complete_piece(leaf, b.col_start, b.width_cols, b.byte_start, b.byte_end());
    }

    fn process_word_break(&mut self, b: LayoutWrapBreak) {
        let Some(leaf) = self.word_chunk.clone() else { return };
        let Some(Segment::Text(chunk)) = leaf.segment() else { return };
        let bytes_len = self.bytes_of(&leaf).len() as u32;
        let col_end = b.col_end().min(chunk.width_cols());
        let byte_end = b.byte_end().min(bytes_len);
        if col_end < self.word_col_start || byte_end < self.word_byte_start {
            return;
        }
        if b.kind == LayoutWrapBreakKind::Whitespace {
            self.process_whitespace_break(&leaf, self.word_col_start, self.word_byte_start, b);
            if self.failed {
                return;
            }
            self.word_col_start = col_end;
            self.word_byte_start = byte_end;
            return;
        }
        if col_end > self.word_col_start {
            self.flush_complete_piece(&leaf, self.word_col_start, col_end - self.word_col_start, self.word_byte_start, byte_end);
            if self.failed {
                return;
            }
            self.has_non_whitespace = true;
        } else if byte_end > self.word_byte_start {
            self.queue_pending(&leaf, self.word_col_start, 0, self.word_byte_start, byte_end);
            if self.failed {
                return;
            }
            if b.kind.is_word_boundary() && self.pending_width > 0 {
                self.finalize_pending();
                if self.failed {
                    return;
                }
            }
        }
        self.word_col_start = col_end;
        self.word_byte_start = byte_end;
    }

    fn process_word_chunk(&mut self, leaf: &NodeRef, chunk: &TextChunk) {
        let bytes = chunk.bytes(&self.tb.mem_registry);
        let edges = utf8::chunk_word_class_edges(bytes);
        if self.pending_width > 0
            && (utf8::is_cjk_ascii_transition(self.pending_last_class, edges.first)
                || (!bytes.is_empty()
                    && utf8::is_cjk_intercharacter_break(self.pending_last_class, edges.first, utf8::decode(bytes, 0).0)))
        {
            self.finalize_pending();
            if self.failed {
                return;
            }
        }
        self.word_chunk = Some(leaf.clone());
        self.word_col_start = 0;
        self.word_byte_start = 0;
        struct Visitor<'w, 'a, 'o, const R: bool, const W: bool>(&'w mut Wrap<'a, 'o, R, W>);
        impl<const R: bool, const W: bool> LayoutVisitor for Visitor<'_, '_, '_, R, W> {
            const WORD_ONLY: bool = false;
            fn emit(&mut self, wrap_break: LayoutWrapBreak) -> bool {
                self.0.process_word_break(wrap_break);
                !self.0.failed
            }
        }
        let s = self.tb.settings();
        let streamed =
            utf8::walk_chunk_layout_info(bytes, s.tab_width, s.control_display, chunk.is_ascii_only(), s.method, &mut Visitor(self));
        if (self.word_byte_start as usize) < bytes.len() {
            self.queue_pending(leaf, self.word_col_start, chunk.width_cols() - self.word_col_start, self.word_byte_start, bytes.len() as u32);
            if self.word_col_start < chunk.width_cols() {
                if !self.failed {
                    self.pending_last_class = streamed.last;
                }
                self.has_non_whitespace = true;
            }
        }
    }

    fn force_char_grapheme(&self, bytes: &[u8], ascii: bool) -> utf8::PosResult {
        let s = self.tb.settings();
        let forced = utf8::find_grapheme_pos_by_width(bytes, 1, s.tab_width, s.control_display, ascii, true, s.method);
        if forced.grapheme_count == 0 {
            return forced;
        }
        let kept = utf8::find_wrap_pos_by_width_grapheme_safe(bytes, forced.columns_used, s.tab_width, s.control_display, ascii, s.method);
        utf8::PosResult {
            byte_offset: forced.byte_offset.max(kept.byte_offset),
            grapheme_count: forced.grapheme_count,
            columns_used: forced.columns_used,
        }
    }

    fn process_char_chunk(&mut self, leaf: &NodeRef, chunk: &TextChunk) {
        let bytes = chunk.bytes(&self.tb.mem_registry);
        let ascii = chunk.is_ascii_only();
        let s = self.tb.settings();
        let width = chunk.width_cols();
        let mut byte_off = 0usize;
        let mut col_off = 0u32;
        if !ascii {
            byte_off = utf8::find_grapheme_pos_by_width(bytes, 0, s.tab_width, s.control_display, false, false, s.method).byte_offset as usize;
            self.add_chunk(leaf, 0, byte_off as u32, 0, 0);
        }
        while col_off < width {
            let line_wrap = self.current_wrap_width;
            let remaining = line_wrap.saturating_sub(self.vline_width);
            if remaining == 0 {
                if self.vline_width > 0 {
                    self.commit();
                    continue;
                }
                let force = self.force_char_grapheme(&bytes[byte_off..], ascii);
                if force.grapheme_count > 0 {
                    self.add_chunk(leaf, byte_off as u32, force.byte_offset, col_off, force.columns_used);
                    col_off += force.columns_used;
                    byte_off += force.byte_offset as usize;
                } else {
                    break;
                }
                continue;
            }
            let rest = &bytes[byte_off..];
            let wrap = utf8::find_wrap_pos_by_width_grapheme_safe(rest, remaining, s.tab_width, s.control_display, ascii, s.method);
            if wrap.grapheme_count == 0 {
                if self.vline_width > 0 {
                    self.commit();
                    continue;
                }
                let force = self.force_char_grapheme(rest, ascii);
                if force.grapheme_count > 0 {
                    self.add_chunk(leaf, byte_off as u32, force.byte_offset, col_off, force.columns_used);
                    col_off += force.columns_used;
                    byte_off += force.byte_offset as usize;
                    if col_off < width {
                        self.commit();
                        continue;
                    }
                }
                break;
            }
            self.add_chunk(leaf, byte_off as u32, wrap.byte_offset, col_off, wrap.columns_used);
            col_off += wrap.columns_used;
            byte_off += wrap.byte_offset as usize;
            if self.vline_width >= line_wrap && col_off < width {
                self.commit();
            }
        }
    }

    fn line_end(&mut self, line_width_cols: u32) {
        if self.failed {
            return;
        }
        if WORD {
            self.finalize_pending();
            self.clear_pending();
        }
        let has_content = if RENDER { !self.current_vline.chunks.is_empty() } else { self.vline_width > 0 };
        if has_content || line_width_cols == 0 {
            if RENDER {
                self.current_vline.width_cols = self.vline_width;
                self.current_vline.source_line = self.line_idx;
                self.current_vline.source_col_start = self.source_line_col_offset;
            }
            self.record_virtual_line();
            if RENDER {
                self.line_vline_count += 1;
            }
        }
        match &mut self.out {
            Output::Measure(result) => {
                result.line_count += self.line_rows;
                result.width_cols_max = result.width_cols_max.max(self.line_width);
                self.line_rows = 0;
                self.line_width = 0;
            }
            Output::Render(out) => {
                out.first_vline.push(self.line_first_vline);
                out.vline_counts.push(self.line_vline_count);
            }
        }
        self.document_cell_offset += 1;
        self.line_idx += 1;
        self.source_line_col_offset = 0;
        self.vline_width = 0;
        self.current_wrap_width = self.wrap_w;
        if RENDER {
            self.current_vline = VirtualLine::new(self.document_cell_offset);
            if let Output::Render(out) = &self.out {
                self.line_first_vline = out.virtual_lines.len() as u32;
            }
            self.line_vline_count = 0;
        }
        if WORD {
            self.has_non_whitespace = false;
            self.word_chunk = None;
        }
    }
}
