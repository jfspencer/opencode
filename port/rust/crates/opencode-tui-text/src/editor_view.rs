//! Port of `editor-view.zig` (`EditorView`): a text buffer view over an edit buffer that follows the cursor
//! (viewport scrolling with margins), moves it by visual lines, maps selections to the cursor, and shows a
//! placeholder buffer while the text is empty.
//!
//! Methods take the edit buffer and the buffer's other views (as an [`EditListener`]) instead of holding them:
//! an edit made through this view notifies every view of the buffer, this one included.

use crate::edit_buffer::{Cursor, EditBufferCore, EditListener};
use crate::iterators::{self};
use crate::text_buffer::{OwnedStyledChunk, TextBufferCore};
use crate::text_buffer_view::{LineInfo, SelectionBehavior, SelectionOccupancy, TextBufferViewCore, VirtualLine, Viewport, WrapMode};
use crate::{Rgba, TextError};

/// A cursor with visual (viewport-relative for the API) and logical coordinates (`VisualCursor`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VisualCursor {
    pub visual_row: u32,
    pub visual_col: u32,
    pub logical_row: u32,
    pub logical_col: u32,
    pub offset: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Affinity {
    offset: u32,
    visual_row: u32,
    visual_col: u32,
}

pub(crate) struct EditorViewCore {
    pub tbv: TextBufferViewCore,
    scroll_margin: f32,
    desired_visual_col: Option<u32>,
    affinity: Option<Affinity>,
    selection_updates_cursor: bool,
    selection_follow_cursor: bool,
    pub follow_pending: bool,
    pub placeholder: Option<TextBufferCore>,
    placeholder_mem_id: Option<u8>,
    pub placeholder_active: bool,
}

/// The view's buffer: the placeholder while it shows.
macro_rules! active {
    ($self:ident, $eb:expr) => {
        if $self.placeholder_active { $self.placeholder.as_ref().expect("placeholder") } else { &$eb.tb }
    };
}

/// This view and the buffer's other views as the edit buffer's listeners.
struct Both<'a> {
    me: &'a mut EditorViewCore,
    others: &'a mut dyn EditListener,
    finish_deletion: bool,
}

impl EditListener for Both<'_> {
    fn cursor_changed(&mut self, eb: &EditBufferCore) {
        self.me.on_cursor_changed(eb);
        self.others.cursor_changed(eb);
    }
    fn after_delete(&mut self, _eb: &EditBufferCore) {
        if self.finish_deletion {
            self.me.reset_local_selection();
            self.me.follow_pending = true;
        }
    }
}

fn clamp_visual_col(vlines: &[VirtualLine], row: u32, col: u32) -> u32 {
    let Some(vline) = vlines.get(row as usize) else { return col };
    let mut target = col.min(vline.width_cols);
    if target == vline.width_cols
        && vline.width_cols > 0
        && (row as usize + 1) < vlines.len()
        && vlines[row as usize + 1].source_line == vline.source_line
    {
        target = vline.width_cols - 1;
    }
    target
}

impl EditorViewCore {
    pub fn new(width: u32, height: u32) -> Self {
        let mut tbv = TextBufferViewCore::new();
        tbv.set_viewport(Some(Viewport { x: 0, y: 0, width, height }));
        EditorViewCore {
            tbv,
            scroll_margin: 0.15,
            desired_visual_col: None,
            affinity: None,
            selection_updates_cursor: false,
            selection_follow_cursor: false,
            follow_pending: false,
            placeholder: None,
            placeholder_mem_id: None,
            placeholder_active: false,
        }
    }

    /// The cursor-changed listener (`onCursorChanged`).
    pub fn on_cursor_changed(&mut self, eb: &EditBufferCore) {
        self.desired_visual_col = None;
        self.update_placeholder_visibility(eb);
        if self.affinity.is_some_and(|a| a.offset != eb.cursor.offset) {
            self.affinity = None;
        }
        if self.follows_cursor() {
            self.follow_pending = true;
        }
    }

    fn follows_cursor(&self) -> bool {
        self.tbv.selection.is_none() || self.selection_follow_cursor
    }

    pub fn settle(&mut self, eb: &mut EditBufferCore) {
        if self.follow_pending {
            self.follow_cursor(eb);
        }
    }

    fn follow_cursor(&mut self, eb: &mut EditBufferCore) -> VisualCursor {
        let vcursor = self.primary_visual_cursor(eb);
        if self.tbv.virtual_lines_dirty {
            return vcursor;
        }
        self.follow_pending = false;
        self.ensure_cursor_visible(eb, vcursor.visual_row);
        vcursor
    }

    pub fn set_viewport(&mut self, eb: &mut EditBufferCore, vp: Option<Viewport>, move_cursor: bool) {
        self.settle(eb);
        let old = self.tbv.viewport;
        self.tbv.set_viewport(vp);
        if old.is_none() || vp.is_none() || old.unwrap().width != vp.unwrap().width {
            self.affinity = None;
        }
        if move_cursor {
            self.make_cursor_visible(eb);
        }
    }

    pub fn get_viewport(&mut self, eb: &mut EditBufferCore) -> Option<Viewport> {
        self.settle(eb);
        self.tbv.viewport
    }

    fn margin(height: u32, margin: f32) -> u32 {
        ((height as f32 * margin) as u32).max(1)
    }

    /// `makeCursorVisible`: moves the cursor (not the viewport) into the viewport's margins.
    pub fn make_cursor_visible(&mut self, eb: &mut EditBufferCore) {
        let Some(vp) = self.tbv.viewport else { return };
        if vp.height == 0 || vp.width == 0 {
            return;
        }
        let vcursor = self.primary_visual_cursor(eb);
        let margin = Self::margin(vp.height, self.scroll_margin).min(vp.height - 1);
        let above = vcursor.visual_row < vp.y;
        let below = vcursor.visual_row >= vp.y + vp.height;
        let near_top = vcursor.visual_row < vp.y + margin;
        let near_bottom = vcursor.visual_row >= vp.y + vp.height - margin;
        if !(above || below || near_top || near_bottom) {
            return;
        }
        let target = if above || near_top { vp.y + margin } else { vp.y + vp.height - margin - 1 };
        let tb = active!(self, eb);
        self.tbv.update_virtual_lines(tb);
        let vlines = self.tbv.virtual_lines_all();
        let Some(vline) = vlines.get(target as usize) else { return };
        let row = vline.source_line;
        let line_width = iterators::line_width_at(eb.tree(), row);
        let desired = eb.unsnapped_column().min(line_width);
        let col = eb.snap_column(row, desired);
        let Some(offset) = iterators::coords_to_offset(eb.tree(), row, col) else { return };
        eb.cursor = Cursor { row, col, desired_col: desired, offset };
    }

    pub fn set_scroll_margin(&mut self, eb: &mut EditBufferCore, margin: f32) {
        self.settle(eb);
        self.scroll_margin = margin.clamp(0.0, 0.5);
    }

    pub fn set_selection_follow_cursor(&mut self, eb: &mut EditBufferCore, enabled: bool) {
        self.settle(eb);
        self.selection_follow_cursor = enabled;
    }

    /// `ensureCursorVisible`: scrolls the viewport to keep `cursor_line` within the margins.
    pub fn ensure_cursor_visible(&mut self, eb: &EditBufferCore, cursor_line: u32) {
        let Some(vp) = self.tbv.viewport else { return };
        if vp.height == 0 || vp.width == 0 {
            return;
        }
        let margin_lines = Self::margin(vp.height, self.scroll_margin).min(if vp.height > 1 { (vp.height - 1) / 2 } else { 0 });
        let margin_cols = Self::margin(vp.width, self.scroll_margin).min(if vp.width > 1 { (vp.width - 1) / 2 } else { 0 });
        let tb = active!(self, eb);
        let total = self.tbv.virtual_line_count(tb);
        let max_offset_y = total.saturating_sub(vp.height);
        let mut y = vp.y;
        let mut x = vp.x;
        if cursor_line < vp.y + margin_lines {
            y = cursor_line.saturating_sub(margin_lines);
        } else if cursor_line >= vp.y + vp.height - margin_lines {
            y = (cursor_line + margin_lines + 1 - vp.height).min(max_offset_y);
        }
        if self.tbv.wrap_mode == WrapMode::None {
            let col = eb.cursor.col;
            if col < vp.x + margin_cols {
                x = col.saturating_sub(margin_cols);
            } else if col >= vp.x + vp.width - margin_cols {
                x = col + margin_cols + 1 - vp.width;
            }
        }
        if y != vp.y || x != vp.x {
            self.tbv.set_viewport(Some(Viewport { x, y, width: vp.width, height: vp.height }));
        }
    }

    pub fn update_cursor_before_render(&mut self, eb: &mut EditBufferCore) -> Option<VisualCursor> {
        self.update_placeholder_visibility(eb);
        if self.follows_cursor() {
            self.follow_pending = true;
        }
        if self.follow_pending { Some(self.follow_cursor(eb)) } else { None }
    }

    /// `getVirtualLines` (follows the cursor first).
    pub fn prepare_lines(&mut self, eb: &mut EditBufferCore) {
        self.update_cursor_before_render(eb);
        let tb = active!(self, eb);
        self.tbv.virtual_lines(tb);
    }

    pub fn cached_line_info(&mut self, eb: &mut EditBufferCore) -> LineInfo {
        self.update_cursor_before_render(eb);
        let tb = active!(self, eb);
        self.tbv.cached_line_info(tb)
    }

    pub fn logical_line_info(&mut self, eb: &EditBufferCore) -> LineInfo {
        self.update_placeholder_visibility(eb);
        let tb = active!(self, eb);
        self.tbv.logical_line_info(tb)
    }

    pub fn total_virtual_line_count(&mut self, eb: &EditBufferCore) -> u32 {
        let tb = active!(self, eb);
        self.tbv.virtual_line_count(tb)
    }

    /// `getVirtualLines().len`: the lines in the viewport.
    pub fn viewport_virtual_line_count(&mut self, eb: &EditBufferCore) -> u32 {
        let tb = active!(self, eb);
        self.tbv.virtual_lines(tb).len() as u32
    }

    pub fn set_selection(&mut self, eb: &mut EditBufferCore, start: u32, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        self.settle(eb);
        self.selection_updates_cursor = false;
        let tb = active!(self, eb);
        self.tbv.set_selection(tb, start, end, bg, fg);
    }

    pub fn update_selection(&mut self, eb: &EditBufferCore, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        self.selection_updates_cursor = false;
        let tb = active!(self, eb);
        self.tbv.update_selection(tb, end, bg, fg);
    }

    pub fn set_selection_inclusive(&mut self, eb: &mut EditBufferCore, start: u32, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        self.settle(eb);
        self.selection_updates_cursor = false;
        let tb = active!(self, eb);
        self.tbv.set_selection_inclusive(tb, start, end, bg, fg);
    }

    pub fn reset_selection(&mut self) {
        self.selection_updates_cursor = false;
        self.tbv.reset_selection();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_local_selection(
        &mut self,
        eb: &mut EditBufferCore,
        anchor_x: i32,
        anchor_y: i32,
        focus_x: i32,
        focus_y: i32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        update_cursor: bool,
        behavior: SelectionBehavior,
    ) -> bool {
        self.settle(eb);
        let tb = active!(self, eb);
        let changed = self.tbv.set_local_selection(tb, anchor_x, anchor_y, focus_x, focus_y, bg, fg, behavior);
        self.selection_updates_cursor = update_cursor;
        if update_cursor && self.tbv.selection_endpoints.is_some() {
            self.sync_cursor_to_selection_focus(eb);
            self.set_affinity_for_local_row(eb, focus_y);
        }
        changed
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_local_selection(
        &mut self,
        eb: &mut EditBufferCore,
        anchor_x: i32,
        anchor_y: i32,
        focus_x: i32,
        focus_y: i32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        update_cursor: bool,
        behavior: SelectionBehavior,
    ) -> bool {
        self.settle(eb);
        let had_endpoints = self.tbv.selection_endpoints.is_some();
        let tb = active!(self, eb);
        let changed = self.tbv.update_local_selection(tb, anchor_x, anchor_y, focus_x, focus_y, bg, fg, behavior);
        if !had_endpoints {
            self.selection_updates_cursor = update_cursor;
        } else if update_cursor {
            self.selection_updates_cursor = true;
        }
        if update_cursor && self.tbv.selection_endpoints.is_some() {
            self.sync_cursor_to_selection_focus(eb);
            self.set_affinity_for_local_row(eb, focus_y);
        }
        changed
    }

    pub fn convert_selection_to_cell(&mut self, eb: &mut EditBufferCore) -> bool {
        let tb = active!(self, eb);
        if !self.tbv.convert_selection_to_cell(tb) {
            return false;
        }
        self.selection_updates_cursor = true;
        self.sync_cursor_to_selection_focus(eb);
        true
    }

    pub fn reset_local_selection(&mut self) {
        self.selection_updates_cursor = false;
        self.tbv.reset_local_selection();
    }

    pub fn set_selection_occupancy(&mut self, eb: &mut EditBufferCore, occupancy: SelectionOccupancy, others: &mut dyn EditListener) {
        let visual_row = self.affinity.map(|a| a.visual_row);
        let selection = self.tbv.selection;
        let has_local = self.selection_updates_cursor
            && self.tbv.selection_endpoints.is_some()
            && selection.is_some_and(|s| s.start != s.end);
        let before = eb.cursor.offset;
        let tb = active!(self, eb);
        self.tbv.set_selection_occupancy(tb, occupancy);
        if has_local {
            self.sync_cursor_to_selection_focus(eb);
            let cursor = eb.cursor;
            if cursor.offset != before {
                eb.set_cursor(cursor.row, cursor.col, &mut Both { me: self, others, finish_deletion: false });
            }
        }
        if let Some(row) = visual_row {
            self.set_affinity_for_absolute_row(eb, row);
        }
    }

    /// `syncCursorToSelectionFocus`: the cursor goes to the selection's focus, without scrolling.
    pub fn sync_cursor_to_selection_focus(&mut self, eb: &mut EditBufferCore) {
        let Some(sel) = self.tbv.selection else { return };
        let mut focus = sel.end;
        if let Some(e) = self.tbv.selection_endpoints {
            focus = e.focus;
            let boundary = self.tbv.selection_occupancy == SelectionOccupancy::Boundary;
            if boundary && focus < e.anchor {
                focus = sel.start;
            } else if boundary && focus > e.anchor {
                focus = sel.end;
            } else if let Some(b) = eb.tb.cursor_unit_bounds_at_offset(focus) {
                focus = b.start;
            }
        }
        let tree = eb.tree();
        let Some(coords) = iterators::offset_to_coords(tree, focus) else { return };
        if coords.row >= iterators::line_count(tree) {
            return;
        }
        if coords.col > iterators::line_width_at(tree, coords.row) {
            return;
        }
        eb.cursor = Cursor { row: coords.row, col: coords.col, desired_col: coords.col, offset: focus };
    }

    pub fn set_viewport_size(&mut self, eb: &mut EditBufferCore, width: u32, height: u32) {
        self.settle(eb);
        let old_width = self.tbv.viewport.map_or(0, |vp| vp.width);
        if old_width != width {
            self.affinity = None;
        }
        self.tbv.set_viewport_size(width, height);
        let Some(vp) = self.tbv.viewport else { return };
        let tb = active!(self, eb);
        let total = self.tbv.virtual_line_count(tb);
        let max_offset_y = total.saturating_sub(vp.height);
        let mut x = vp.x;
        if self.tbv.wrap_mode == WrapMode::None {
            let max_x = iterators::max_line_width(eb.tree()).saturating_sub(vp.width);
            if vp.x > max_x {
                x = max_x;
            }
        }
        if vp.y > max_offset_y || x != vp.x {
            self.tbv.set_viewport(Some(Viewport { x, y: vp.y.min(max_offset_y), width: vp.width, height: vp.height }));
        }
        let vcursor = self.primary_visual_cursor(eb);
        self.ensure_cursor_visible(eb, vcursor.visual_row);
    }

    pub fn set_wrap_mode(&mut self, eb: &mut EditBufferCore, mode: WrapMode) {
        self.settle(eb);
        self.affinity = None;
        self.tbv.set_wrap_mode(mode);
    }

    /// `getPrimaryVisualCursorAbsolute`.
    fn primary_visual_cursor(&mut self, eb: &EditBufferCore) -> VisualCursor {
        let cursor = eb.cursor;
        if let Some(a) = self.affinity
            && a.offset == cursor.offset
        {
            self.set_affinity_for_absolute_row(eb, a.visual_row);
            if let Some(v) = self.affinity {
                return VisualCursor {
                    visual_row: v.visual_row,
                    visual_col: v.visual_col,
                    logical_row: cursor.row,
                    logical_col: cursor.col,
                    offset: cursor.offset,
                };
            }
        }
        self.logical_to_visual_cursor(eb, cursor.row, cursor.col)
    }

    fn set_affinity_for_absolute_row(&mut self, eb: &EditBufferCore, visual_row: u32) {
        let tb = active!(self, eb);
        self.tbv.update_virtual_lines(tb);
        let vlines = self.tbv.virtual_lines_all();
        let Some(vline) = vlines.get(visual_row as usize) else {
            self.affinity = None;
            return;
        };
        let cursor = eb.cursor;
        if vline.source_line != cursor.row || cursor.offset < vline.document_cell_offset {
            self.affinity = None;
            return;
        }
        let visual_col = cursor.offset - vline.document_cell_offset;
        let mut max = vline.width_cols;
        let viewport_width = self.tbv.viewport.map_or(vline.width_cols, |vp| vp.width);
        if self.tbv.selection_occupancy == SelectionOccupancy::Cell
            && vline.width_cols > 0
            && vline.width_cols <= viewport_width
            && (visual_row as usize + 1) < vlines.len()
            && vlines[visual_row as usize + 1].source_line == vline.source_line
        {
            max -= 1;
        }
        if visual_col > max {
            self.affinity = None;
            return;
        }
        self.affinity = Some(Affinity { offset: cursor.offset, visual_row, visual_col });
    }

    fn set_affinity_for_local_row(&mut self, eb: &EditBufferCore, visual_row: i32) {
        let tb = active!(self, eb);
        self.tbv.update_virtual_lines(tb);
        let count = self.tbv.virtual_lines_all().len() as i64;
        if count == 0 {
            return;
        }
        let vy = self.tbv.viewport.map_or(0, |vp| vp.y as i64);
        let row = (visual_row as i64 + vy).min(count - 1).max(0) as u32;
        self.set_affinity_for_absolute_row(eb, row);
    }

    /// `getVisualCursor`: viewport-relative.
    pub fn visual_cursor(&mut self, eb: &mut EditBufferCore) -> VisualCursor {
        let vcursor = match self.update_cursor_before_render(eb) {
            Some(v) => v,
            None => self.primary_visual_cursor(eb),
        };
        let Some(vp) = self.tbv.viewport else { return vcursor };
        let row = vcursor.visual_row.saturating_sub(vp.y);
        let col = if self.tbv.wrap_mode == WrapMode::None { vcursor.visual_col.saturating_sub(vp.x) } else { vcursor.visual_col };
        VisualCursor { visual_row: row, visual_col: col, ..vcursor }
    }

    pub fn logical_to_visual_cursor(&mut self, eb: &EditBufferCore, row: u32, col: u32) -> VisualCursor {
        let tree = eb.tree();
        let line_count = iterators::line_count(tree);
        let row = if line_count > 0 { row.min(line_count - 1) } else { 0 };
        let col = col.min(iterators::line_width_at(tree, row));
        let tb = active!(self, eb);
        let index = self.tbv.find_visual_line_index(tb, row, col);
        let offset = iterators::coords_to_offset(tree, row, col).unwrap_or(0);
        let vlines = self.tbv.virtual_lines_all();
        let Some(vline) = vlines.get(index as usize) else {
            return VisualCursor { visual_row: 0, visual_col: 0, logical_row: row, logical_col: col, offset };
        };
        let visual_col = if col >= vline.source_col_start { (col - vline.source_col_start).min(vline.width_cols) } else { 0 };
        VisualCursor { visual_row: index, visual_col, logical_row: row, logical_col: col, offset }
    }

    fn visual_to_logical_cursor(&mut self, eb: &EditBufferCore, visual_row: u32, visual_col: u32) -> Option<VisualCursor> {
        let tb = active!(self, eb);
        self.tbv.update_virtual_lines(tb);
        let vline = self.tbv.virtual_lines_all().get(visual_row as usize)?;
        let clamped = visual_col.min(vline.width_cols);
        let logical_col = vline.source_col_start + clamped;
        let logical_row = vline.source_line;
        let offset = iterators::coords_to_offset(eb.tree(), logical_row, logical_col).unwrap_or(0);
        Some(VisualCursor { visual_row, visual_col: clamped, logical_row, logical_col, offset })
    }

    pub fn move_up_visual(&mut self, eb: &mut EditBufferCore) {
        self.settle(eb);
        let vcursor = self.primary_visual_cursor(eb);
        if vcursor.visual_row == 0 {
            return;
        }
        self.move_to_visual_row(eb, vcursor, vcursor.visual_row - 1);
    }

    pub fn move_down_visual(&mut self, eb: &mut EditBufferCore) {
        self.settle(eb);
        let vcursor = self.primary_visual_cursor(eb);
        if self.placeholder_active || vcursor.visual_row as usize + 1 >= self.tbv.virtual_lines_all().len() {
            return;
        }
        self.move_to_visual_row(eb, vcursor, vcursor.visual_row + 1);
    }

    fn move_to_visual_row(&mut self, eb: &mut EditBufferCore, vcursor: VisualCursor, target_row: u32) {
        let desired = self
            .desired_visual_col
            .unwrap_or_else(|| vcursor.visual_col.wrapping_add(eb.unsnapped_column().wrapping_sub(vcursor.logical_col)));
        self.desired_visual_col = Some(desired);
        let boundary = self.tbv.selection_occupancy == SelectionOccupancy::Boundary;
        let vlines = self.tbv.virtual_lines_all();
        let target_col = if boundary {
            desired.min(vlines[target_row as usize].width_cols)
        } else {
            clamp_visual_col(vlines, target_row, desired)
        };
        let Some(new) = self.visual_to_logical_cursor(eb, target_row, target_col) else { return };
        let col = eb.snap_column(new.logical_row, new.logical_col);
        eb.cursor = Cursor {
            row: new.logical_row,
            col,
            desired_col: new.logical_col,
            offset: iterators::coords_to_offset(eb.tree(), new.logical_row, col).unwrap_or(0),
        };
        self.affinity = None;
        if boundary {
            self.set_affinity_for_absolute_row(eb, target_row);
        }
        self.ensure_cursor_visible(eb, new.visual_row);
    }

    pub fn delete_selected_text(&mut self, eb: &mut EditBufferCore, others: &mut dyn EditListener) -> Result<(), TextError> {
        let Some(sel) = self.tbv.selection else { return Ok(()) };
        let tree = eb.tree();
        let Some(start) = iterators::offset_to_coords(tree, sel.start) else { return Ok(()) };
        let Some(end) = iterators::offset_to_coords(tree, sel.end) else { return Ok(()) };
        eb.delete_range(
            Cursor { row: start.row, col: start.col, desired_col: start.col, offset: 0 },
            Cursor { row: end.row, col: end.col, desired_col: end.col, offset: 0 },
            &mut Both { me: self, others, finish_deletion: false },
        )?;
        self.reset_local_selection();
        self.follow_pending = true;
        Ok(())
    }

    /// `replaceSelectedText`: bit 1 when text was inserted, bit 0 when a selection was deleted.
    pub fn replace_selected_text(&mut self, eb: &mut EditBufferCore, bytes: &[u8], others: &mut dyn EditListener) -> Result<u32, TextError> {
        let inserted = if bytes.is_empty() { 0 } else { 2 };
        if let Some(sel) = self.tbv.selection
            && sel.end <= eb.tree().total_weight()
        {
            eb.replace_range(sel.start, sel.end, bytes, &mut Both { me: self, others, finish_deletion: true })?;
            return Ok(inserted | u32::from(sel.start != sel.end));
        }
        eb.insert_text(bytes, &mut Both { me: self, others, finish_deletion: false })?;
        Ok(inserted)
    }

    pub fn set_cursor_by_offset(&mut self, eb: &mut EditBufferCore, offset: u32, others: &mut dyn EditListener) {
        eb.set_cursor_by_offset(offset, &mut Both { me: self, others, finish_deletion: false });
    }

    pub fn next_word_boundary(&mut self, eb: &EditBufferCore) -> VisualCursor {
        let c = eb.next_word_boundary();
        self.logical_to_visual_cursor(eb, c.row, c.col)
    }

    pub fn prev_word_boundary(&mut self, eb: &EditBufferCore) -> VisualCursor {
        let c = eb.prev_word_boundary();
        self.logical_to_visual_cursor(eb, c.row, c.col)
    }

    pub fn eol(&mut self, eb: &EditBufferCore) -> VisualCursor {
        let c = eb.eol();
        self.logical_to_visual_cursor(eb, c.row, c.col)
    }

    /// `getVisualSOL`.
    pub fn visual_sol(&mut self, eb: &EditBufferCore) -> VisualCursor {
        let cursor = eb.cursor;
        let vcursor = self.primary_visual_cursor(eb);
        let tree = eb.tree();
        let Some(vline) = self.tbv.virtual_lines_all().get(vcursor.visual_row as usize) else {
            let offset = iterators::coords_to_offset(tree, cursor.row, 0).unwrap_or(0);
            return VisualCursor { visual_row: vcursor.visual_row, visual_col: 0, logical_row: cursor.row, logical_col: 0, offset };
        };
        let (row, col) = (vline.source_line, vline.source_col_start);
        let offset = iterators::coords_to_offset(tree, row, col).unwrap_or(0);
        VisualCursor { visual_row: vcursor.visual_row, visual_col: 0, logical_row: row, logical_col: col, offset }
    }

    /// `getVisualEOL`.
    pub fn visual_eol(&mut self, eb: &EditBufferCore) -> VisualCursor {
        let vcursor = self.primary_visual_cursor(eb);
        let vlines = self.tbv.virtual_lines_all();
        let Some(vline) = vlines.get(vcursor.visual_row as usize) else {
            let c = eb.eol();
            return self.logical_to_visual_cursor(eb, c.row, c.col);
        };
        let viewport_width = self.tbv.viewport.map_or(vline.width_cols, |vp| vp.width);
        let oversized = vline.width_cols > viewport_width;
        let cell = self.tbv.selection_occupancy == SelectionOccupancy::Cell;
        let mut target = if !cell {
            vline.width_cols
        } else if oversized {
            vcursor.visual_col.min(vline.width_cols)
        } else {
            clamp_visual_col(vlines, vcursor.visual_row, vline.width_cols)
        };
        if cell && !oversized {
            let target_offset = vline.document_cell_offset + target;
            if let Some(b) = eb.tb.cursor_unit_bounds_at_offset(target_offset)
                && b.start < target_offset
            {
                let t = if b.start >= vline.document_cell_offset { b.start } else { b.end };
                target = t.saturating_sub(vline.document_cell_offset).min(vline.width_cols);
            }
        }
        let row = vline.source_line;
        let col = vline.source_col_start + target;
        let offset = iterators::coords_to_offset(eb.tree(), row, col).unwrap_or(0);
        VisualCursor { visual_row: vcursor.visual_row, visual_col: target, logical_row: row, logical_col: col, offset }
    }

    pub fn goto_visual_line_end(&mut self, eb: &mut EditBufferCore, others: &mut dyn EditListener) {
        let eol = self.visual_eol(eb);
        self.affinity = Some(Affinity { offset: eol.offset, visual_row: eol.visual_row, visual_col: eol.visual_col });
        eb.set_cursor(eol.logical_row, eol.logical_col, &mut Both { me: self, others, finish_deletion: false });
    }

    pub fn clear_placeholder(&mut self, eb: &EditBufferCore) {
        if self.placeholder_active {
            let from = self.placeholder.as_ref().map_or(0, |p| p.id);
            self.tbv.switch_buffer(from, eb.tb.id);
            self.placeholder_active = false;
        }
        self.placeholder = None;
        self.placeholder_mem_id = None;
    }

    pub fn set_placeholder_owned_styled_text(&mut self, eb: &EditBufferCore, text: Vec<u8>, chunks: &[OwnedStyledChunk]) -> Result<(), TextError> {
        let mut placeholder = match self.placeholder.take() {
            Some(p) => p,
            None => TextBufferCore::new(eb.tb.width_method, eb.tb.control_display, eb.tb.link_pool().clone()),
        };
        match placeholder.replace_owned_styled_text(text, self.placeholder_mem_id, chunks, None) {
            Ok(id) => self.placeholder_mem_id = Some(id),
            Err(e) => {
                self.placeholder = Some(placeholder);
                return Err(e);
            }
        }
        self.placeholder = Some(placeholder);
        if self.placeholder_active {
            self.tbv.virtual_lines_dirty = true;
        }
        self.update_placeholder_visibility(eb);
        Ok(())
    }

    fn update_placeholder_visibility(&mut self, eb: &EditBufferCore) {
        let show = eb.tree().total_weight() == 0 && self.placeholder.is_some();
        if show && !self.placeholder_active {
            if let Some(p) = &self.placeholder {
                self.tbv.switch_buffer(eb.tb.id, p.id);
                self.placeholder_active = true;
            }
        } else if !show && self.placeholder_active {
            let from = self.placeholder.as_ref().map_or(0, |p| p.id);
            self.tbv.switch_buffer(from, eb.tb.id);
            self.placeholder_active = false;
        }
    }

    pub fn active_buffer<'a>(&'a self, eb: &'a EditBufferCore) -> &'a TextBufferCore {
        active!(self, eb)
    }

    /// The layout state and the buffer it lays out.
    pub fn split<'a>(&'a mut self, eb: &'a EditBufferCore) -> (&'a mut TextBufferViewCore, &'a TextBufferCore) {
        let tb = active!(self, eb);
        (&mut self.tbv, tb)
    }
}
