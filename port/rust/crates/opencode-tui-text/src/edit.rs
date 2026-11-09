//! The editing API the renderables call: `EditBuffer` (`edit-buffer.ts`) and `EditorView` (`editor-view.ts`), with
//! the steps of the native Context calls behind them (`context.zig`: `editCommand`, `editorSelect`, ...;
//! `context-editor-abi.zig`: positions, lines, measuring).
//!
//! Edit events reach TS listeners in microtasks after the call; here they queue on the buffer until
//! [`EditBuffer::take_events`] (`"cursor-changed"`, `"content-changed"`, `"cursorChanged"`).

use crate::edit_buffer::{measure_edit_text, Cursor, EditBufferCore, EditEvent, EditListener};
use crate::editor_view::{EditorViewCore, VisualCursor};
use crate::iterators;
use crate::syntax_style::{StyleDefinition, SyntaxStyle};
use crate::text::{
    check_local_coords, decode_text, highlight_buffer, set_buffer_defaults, validate_tab_indicator, validate_text_bytes,
    validate_viewport, yoga_measure_with, DefaultsMask, DrawSource, HighlightOp, HighlightRange, TextContext,
};
use crate::text_buffer::{effective_tab_width, OwnedStyledChunk};
use crate::text_buffer_view::{LineInfo, MeasureResult, SelectionBehavior, SelectionOccupancy, Viewport, WrapMode};
use crate::utf8::{ControlDisplay, WidthMethod};
use crate::{Rgba, TextError};
use std::cell::RefCell;
use std::rc::Rc;

/// What `_replaceSelectedText` did (`NativeEditorReplacement`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Replacement {
    pub deleted: bool,
    pub inserted: bool,
}

/// `LogicalCursor`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LogicalCursor {
    pub row: u32,
    pub col: u32,
    pub offset: u32,
}

impl From<Cursor> for LogicalCursor {
    fn from(c: Cursor) -> Self {
        LogicalCursor { row: c.row, col: c.col, offset: c.offset }
    }
}

pub(crate) struct EditShared {
    pub(crate) eb: EditBufferCore,
    input_mem_id: Option<u8>,
    views: Vec<Option<Box<EditorViewCore>>>,
    style: Option<SyntaxStyle>,
}

/// Every editor view of a buffer as its cursor listeners.
struct Views<'a>(&'a mut [Option<Box<EditorViewCore>>]);

impl EditListener for Views<'_> {
    fn cursor_changed(&mut self, eb: &EditBufferCore) {
        for view in self.0.iter_mut().flatten() {
            view.on_cursor_changed(eb);
        }
    }
}

/// `EditBuffer` (`edit-buffer.ts`). Clones are handles to the same buffer.
#[derive(Clone)]
pub struct EditBuffer {
    shared: Rc<RefCell<EditShared>>,
}

/// `measureEditText`: the editor offset at the end of `text` under these settings, without a buffer.
pub fn measure_edit_text_with(text: &str, width_method: WidthMethod, tab_width: u32) -> Result<u32, TextError> {
    if tab_width > 255 {
        return Err(TextError::InvalidOptions);
    }
    let tab = effective_tab_width(tab_width as u8);
    validate_text_bytes(tab, text.len(), 0)?;
    Ok(measure_edit_text(text.as_bytes(), tab, ControlDisplay::Notation, width_method))
}

impl EditBuffer {
    /// `EditBuffer.create(widthMethod, owner)`.
    pub fn create(width_method: WidthMethod, ctx: &TextContext) -> EditBuffer {
        let eb = EditBufferCore::new(width_method, ctx.links.clone());
        EditBuffer { shared: Rc::new(RefCell::new(EditShared { eb, input_mem_id: None, views: Vec::new(), style: None })) }
    }

    pub fn ptr_eq(&self, other: &EditBuffer) -> bool {
        Rc::ptr_eq(&self.shared, &other.shared)
    }

    fn with<R>(&self, f: impl FnOnce(&mut EditBufferCore, &mut dyn EditListener) -> R) -> R {
        let mut s = self.shared.borrow_mut();
        let s = &mut *s;
        f(&mut s.eb, &mut Views(&mut s.views))
    }

    fn set_native_text(&self, text: &str, preserve_history: bool) -> Result<(), TextError> {
        let mut s = self.shared.borrow_mut();
        let s = &mut *s;
        validate_text_bytes(s.eb.tb.tab_width, text.len(), 0)?;
        if preserve_history {
            s.eb.replace_text(text.as_bytes(), &mut Views(&mut s.views))
        } else {
            s.input_mem_id = Some(s.eb.set_text_owned(text.as_bytes(), s.input_mem_id, &mut Views(&mut s.views))?);
            Ok(())
        }
    }

    /// `setText`: replaces the text and clears the history.
    pub fn set_text(&self, text: &str) -> Result<(), TextError> {
        self.set_native_text(text, false)
    }

    pub fn set_text_owned(&self, text: &str) -> Result<(), TextError> {
        self.set_native_text(text, false)
    }

    /// `replaceText`: replaces the text as an undo step.
    pub fn replace_text(&self, text: &str) -> Result<(), TextError> {
        self.set_native_text(text, true)
    }

    pub fn replace_text_owned(&self, text: &str) -> Result<(), TextError> {
        self.set_native_text(text, true)
    }

    pub fn get_line_count(&self) -> u32 {
        self.shared.borrow().eb.tb.line_count()
    }

    pub fn set_tab_width(&self, width: u32) -> Result<(), TextError> {
        if width > 255 {
            return Err(TextError::InvalidOptions);
        }
        self.with(|eb, l| {
            eb.tb.check_tab_width(width as u8)?;
            eb.set_tab_width(width as u8, l);
            Ok(())
        })
    }

    pub fn get_tab_width(&self) -> u8 {
        self.shared.borrow().eb.tb.tab_width
    }

    pub fn get_text(&self) -> String {
        decode_text(self.shared.borrow().eb.tb.plain_text())
    }

    pub fn insert_char(&self, text: &str) -> Result<(), TextError> {
        self.insert_text(text)
    }

    pub fn insert_text(&self, text: &str) -> Result<(), TextError> {
        self.with(|eb, l| {
            validate_text_bytes(eb.tb.tab_width, text.len(), eb.tb.byte_size())?;
            eb.insert_text(text.as_bytes(), l)
        })
    }

    pub fn delete_char(&self) -> Result<(), TextError> {
        self.with(|eb, l| eb.delete_forward(l))
    }

    pub fn delete_char_backward(&self) -> Result<(), TextError> {
        self.with(|eb, l| eb.backspace(l))
    }

    pub fn delete_range(&self, start_line: u32, start_col: u32, end_line: u32, end_col: u32) -> Result<(), TextError> {
        self.with(|eb, l| {
            eb.delete_range(
                Cursor { row: start_line, col: start_col, ..Cursor::default() },
                Cursor { row: end_line, col: end_col, ..Cursor::default() },
                l,
            )
        })
    }

    pub fn new_line(&self) -> Result<(), TextError> {
        self.with(|eb, l| {
            validate_text_bytes(eb.tb.tab_width, 1, eb.tb.byte_size())?;
            eb.insert_text(b"\n", l)
        })
    }

    pub fn delete_line(&self) -> Result<(), TextError> {
        self.with(|eb, l| eb.delete_line(l))
    }

    pub fn move_cursor_left(&self) {
        self.with(|eb, l| eb.move_left(l))
    }

    pub fn move_cursor_right(&self) {
        self.with(|eb, l| eb.move_right(l))
    }

    pub fn move_cursor_up(&self) {
        self.with(|eb, l| eb.move_up(l))
    }

    pub fn move_cursor_down(&self) {
        self.with(|eb, l| eb.move_down(l))
    }

    pub fn goto_line(&self, line: u32) {
        self.with(|eb, l| eb.goto_line(line, l))
    }

    pub fn set_cursor(&self, line: u32, col: u32) {
        self.with(|eb, l| eb.set_cursor(line, col, l))
    }

    pub fn set_cursor_to_line_col(&self, line: u32, col: u32) {
        self.set_cursor(line, col)
    }

    pub fn set_cursor_by_offset(&self, offset: u32) {
        self.with(|eb, l| eb.set_cursor_by_offset(offset, l))
    }

    pub fn get_cursor_position(&self) -> LogicalCursor {
        self.shared.borrow().eb.cursor.into()
    }

    pub fn get_next_word_boundary(&self) -> LogicalCursor {
        self.shared.borrow().eb.next_word_boundary().into()
    }

    pub fn get_prev_word_boundary(&self) -> LogicalCursor {
        self.shared.borrow().eb.prev_word_boundary().into()
    }

    pub fn get_eol(&self) -> LogicalCursor {
        self.shared.borrow().eb.eol().into()
    }

    /// `_getEOF`: the end of the text.
    pub fn get_eof(&self) -> LogicalCursor {
        let s = self.shared.borrow();
        let tree = s.eb.tree();
        let at = tree.total_weight();
        let c = iterators::offset_to_coords(tree, at).expect("the end is a valid offset");
        LogicalCursor { row: c.row, col: c.col, offset: at }
    }

    /// `_getPrevCharBoundary`: where moving left would put the cursor.
    pub fn get_prev_char_boundary(&self) -> LogicalCursor {
        self.shared.borrow().eb.prev_char_boundary().into()
    }

    pub fn offset_to_position(&self, offset: u32) -> Option<(u32, u32)> {
        let s = self.shared.borrow();
        iterators::offset_to_coords(s.eb.tree(), offset).map(|c| (c.row, c.col))
    }

    pub fn position_to_offset(&self, row: u32, col: u32) -> u32 {
        iterators::coords_to_offset(self.shared.borrow().eb.tree(), row, col).unwrap_or(0)
    }

    pub fn get_line_start_offset(&self, row: u32) -> u32 {
        iterators::coords_to_offset(self.shared.borrow().eb.tree(), row, 0).unwrap_or(0)
    }

    /// `measureText`: the offset at the end of `text` laid out by this buffer, without storing it.
    pub fn measure_text(&self, text: &str) -> Result<u32, TextError> {
        let s = self.shared.borrow();
        let tb = &s.eb.tb;
        validate_text_bytes(tb.tab_width, text.len(), 0)?;
        Ok(measure_edit_text(text.as_bytes(), tb.tab_width, tb.control_display, tb.width_method))
    }

    pub fn get_text_range(&self, start: u32, end: u32) -> String {
        if start >= end {
            return String::new();
        }
        decode_text(self.shared.borrow().eb.tb.copy_text_range(start, end))
    }

    pub fn get_text_range_by_coords(&self, start_row: u32, start_col: u32, end_row: u32, end_col: u32) -> String {
        let s = self.shared.borrow();
        let tree = s.eb.tree();
        let (Some(start), Some(end)) =
            (iterators::coords_to_offset(tree, start_row, start_col), iterators::coords_to_offset(tree, end_row, end_col))
        else {
            return String::new();
        };
        decode_text(s.eb.tb.copy_text_range(start, end))
    }

    /// `undo`: the restored step's cursor metadata (`cursor:row:col:desired`), `None` with no history.
    pub fn undo(&self) -> Option<String> {
        self.with(|eb, l| if eb.can_undo() { eb.undo(l) } else { None }).filter(|m| !m.is_empty())
    }

    pub fn redo(&self) -> Option<String> {
        self.with(|eb, l| if eb.can_redo() { eb.redo(l) } else { None }).filter(|m| !m.is_empty())
    }

    pub fn can_undo(&self) -> bool {
        self.shared.borrow().eb.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.shared.borrow().eb.can_redo()
    }

    pub fn clear_history(&self) {
        self.shared.borrow_mut().eb.clear_history()
    }

    fn set_defaults(&self, mask: DefaultsMask, fg: Option<Rgba>, bg: Option<Rgba>, attributes: Option<u32>) -> Result<(), TextError> {
        set_buffer_defaults(&mut self.shared.borrow_mut().eb.tb, mask, fg, bg, attributes)
    }

    pub fn set_default_fg(&self, fg: Option<Rgba>) -> Result<(), TextError> {
        self.set_defaults(DefaultsMask { fg: true, bg: false, attributes: false }, fg, None, None)
    }

    pub fn set_default_bg(&self, bg: Option<Rgba>) -> Result<(), TextError> {
        self.set_defaults(DefaultsMask { fg: false, bg: true, attributes: false }, None, bg, None)
    }

    pub fn set_default_attributes(&self, attributes: Option<u32>) -> Result<(), TextError> {
        self.set_defaults(DefaultsMask { fg: false, bg: false, attributes: true }, None, None, attributes)
    }

    pub fn reset_defaults(&self) {
        let _ = self.set_defaults(DefaultsMask { fg: true, bg: true, attributes: true }, None, None, None);
    }

    pub fn set_syntax_style(&self, style: Option<&SyntaxStyle>) {
        let mut s = self.shared.borrow_mut();
        s.eb.tb.set_syntax_style(style.cloned());
        s.style = style.cloned();
    }

    pub fn get_syntax_style(&self) -> Option<SyntaxStyle> {
        self.shared.borrow().style.clone()
    }

    fn highlight(&self, op: HighlightOp) -> Result<(), TextError> {
        highlight_buffer(&mut self.shared.borrow_mut().eb.tb, op)
    }

    pub fn add_highlight(&self, line: u32, h: HighlightRange) -> Result<(), TextError> {
        self.highlight(HighlightOp::AddLine(line, h))
    }

    pub fn add_highlight_by_char_range(&self, h: HighlightRange) -> Result<(), TextError> {
        self.highlight(HighlightOp::AddRange(h))
    }

    pub fn remove_highlights_by_ref(&self, hl_ref: u32) -> Result<(), TextError> {
        self.highlight(HighlightOp::RemoveRef(hl_ref))
    }

    pub fn clear_line_highlights(&self, line: u32) {
        let _ = self.highlight(HighlightOp::ClearLine(line));
    }

    pub fn clear_all_highlights(&self) {
        let _ = self.highlight(HighlightOp::ClearAll);
    }

    pub fn get_line_highlights(&self, line: u32) -> Vec<HighlightRange> {
        self.shared.borrow().eb.tb.line_highlights(line as usize).iter().map(HighlightRange::from).collect()
    }

    /// `clear`: empties the text (keeping history and highlights) and moves the cursor home.
    pub fn clear(&self) {
        self.with(|eb, l| eb.clear(l))
    }

    /// The events queued since the last call, in order.
    pub fn take_events(&self) -> Vec<EditEvent> {
        self.shared.borrow_mut().eb.take_events()
    }

    pub fn content_epoch(&self) -> u64 {
        self.shared.borrow().eb.tb.content_epoch()
    }

    pub fn width_method(&self) -> WidthMethod {
        self.shared.borrow().eb.tb.width_method
    }

    pub fn byte_size(&self) -> u32 {
        self.shared.borrow().eb.tb.byte_size()
    }
}

/// `EditorView` (`editor-view.ts`).
pub struct EditorView {
    shared: Rc<RefCell<EditShared>>,
    index: usize,
}

impl Drop for EditorView {
    fn drop(&mut self) {
        if let Ok(mut s) = self.shared.try_borrow_mut() {
            s.views[self.index] = None;
        }
    }
}

/// The selection operations of `editorSelect`.
enum Select {
    Set(u32, u32, Option<Rgba>, Option<Rgba>),
    Update(u32, Option<Rgba>, Option<Rgba>),
    Reset,
    Local { update: bool, ax: i32, ay: i32, fx: i32, fy: i32, bg: Option<Rgba>, fg: Option<Rgba>, update_cursor: bool, follow_cursor: bool, behavior: SelectionBehavior },
    LocalReset,
    Cell,
    Occupancy(SelectionOccupancy),
    Inclusive(u32, u32, Option<Rgba>, Option<Rgba>),
    Colors(Option<Rgba>, Option<Rgba>),
}

impl EditorView {
    /// `EditorView.create(editBuffer, width, height)`.
    pub fn create(buffer: &EditBuffer, width: u32, height: u32) -> Result<EditorView, TextError> {
        if width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(TextError::InvalidOptions);
        }
        let mut s = buffer.shared.borrow_mut();
        let index = match s.views.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                s.views.push(None);
                s.views.len() - 1
            }
        };
        s.views[index] = Some(Box::new(EditorViewCore::new(width, height)));
        Ok(EditorView { shared: buffer.shared.clone(), index })
    }

    pub fn buffer(&self) -> EditBuffer {
        EditBuffer { shared: self.shared.clone() }
    }

    /// Runs `f` on this view, the buffer, and the other views as listeners.
    fn with<R>(&self, f: impl FnOnce(&mut EditorViewCore, &mut EditBufferCore, &mut dyn EditListener) -> R) -> R {
        let mut s = self.shared.borrow_mut();
        let s = &mut *s;
        let mut view = s.views[self.index].take().expect("live view");
        let result = f(&mut view, &mut s.eb, &mut Views(&mut s.views));
        s.views[self.index] = Some(view);
        result
    }

    /// `Editor.prepareView`: lays the view out and applies a due scroll.
    fn prepare(view: &mut EditorViewCore, eb: &mut EditBufferCore) {
        let (tbv, tb) = view.split(eb);
        tbv.virtual_lines(tb);
        view.settle(eb);
    }

    pub fn set_viewport_size(&self, width: u32, height: u32) -> Result<(), TextError> {
        validate_viewport(Viewport { x: 0, y: 0, width, height })?;
        self.with(|v, eb, _| {
            Self::prepare(v, eb);
            v.set_viewport_size(eb, width, height);
            Ok(())
        })
    }

    pub fn set_viewport(&self, x: u32, y: u32, width: u32, height: u32, move_cursor: bool) -> Result<(), TextError> {
        let vp = Viewport { x, y, width, height };
        validate_viewport(vp)?;
        self.with(|v, eb, _| {
            Self::prepare(v, eb);
            v.set_viewport(eb, Some(vp), move_cursor);
            Ok(())
        })
    }

    /// `getViewport` (offsets as `x`, `y`).
    pub fn get_viewport(&self) -> Viewport {
        self.with(|v, eb, _| {
            if v.follow_pending {
                Self::prepare(v, eb);
            }
            v.get_viewport(eb).expect("an editor has a viewport")
        })
    }

    pub fn set_scroll_margin(&self, margin: f32) -> Result<(), TextError> {
        if !margin.is_finite() {
            return Err(TextError::InvalidOptions);
        }
        self.with(|v, eb, _| {
            if v.follow_pending {
                Self::prepare(v, eb);
            }
            v.set_scroll_margin(eb, margin);
        });
        Ok(())
    }

    pub fn set_wrap_mode(&self, mode: WrapMode) {
        self.with(|v, eb, _| {
            if v.follow_pending {
                Self::prepare(v, eb);
            }
            v.set_wrap_mode(eb, mode);
        })
    }

    /// `getVirtualLineCount`: the visual lines in the viewport, after following the cursor.
    pub fn get_virtual_line_count(&self) -> u32 {
        self.with(|v, eb, _| {
            v.update_cursor_before_render(eb);
            Self::prepare(v, eb);
            v.viewport_virtual_line_count(eb)
        })
    }

    /// `getTotalVirtualLineCount`: every visual line.
    pub fn get_total_virtual_line_count(&self) -> u32 {
        self.with(|v, eb, _| {
            Self::prepare(v, eb);
            v.total_virtual_line_count(eb)
        })
    }

    fn select(&self, op: Select) -> Result<bool, TextError> {
        self.with(|v, eb, others| {
            let local = matches!(op, Select::Local { .. });
            if let Select::Local { ax, ay, fx, fy, .. } = op {
                if v.follow_pending {
                    Self::prepare(v, eb);
                }
                check_local_coords(&v.tbv, &eb.tb, ax, ay, fx, fy)?;
            }
            if !matches!(op, Select::Reset | Select::LocalReset) {
                Self::prepare(v, eb);
            }
            if let Select::Local { follow_cursor, .. } = op {
                v.set_selection_follow_cursor(eb, follow_cursor);
            }
            let _ = local;
            Ok(match op {
                Select::Set(s, e, bg, fg) => {
                    v.set_selection(eb, s, e, bg, fg);
                    false
                }
                Select::Update(e, bg, fg) => {
                    v.update_selection(eb, e, bg, fg);
                    false
                }
                Select::Reset => {
                    v.reset_selection();
                    false
                }
                Select::Local { update: false, ax, ay, fx, fy, bg, fg, update_cursor, behavior, .. } => {
                    v.set_local_selection(eb, ax, ay, fx, fy, bg, fg, update_cursor, behavior)
                }
                Select::Local { update: true, ax, ay, fx, fy, bg, fg, update_cursor, behavior, .. } => {
                    v.update_local_selection(eb, ax, ay, fx, fy, bg, fg, update_cursor, behavior)
                }
                Select::LocalReset => {
                    v.reset_local_selection();
                    false
                }
                Select::Cell => v.convert_selection_to_cell(eb),
                Select::Occupancy(o) => {
                    v.set_selection_occupancy(eb, o, others);
                    false
                }
                Select::Inclusive(s, e, bg, fg) => {
                    v.set_selection_inclusive(eb, s, e, bg, fg);
                    false
                }
                Select::Colors(bg, fg) => {
                    v.tbv.set_selection_colors(bg, fg);
                    false
                }
            })
        })
    }

    pub fn set_selection(&self, start: u32, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        let _ = self.select(Select::Set(start, end, bg, fg));
    }

    pub fn update_selection(&self, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        let _ = self.select(Select::Update(end, bg, fg));
    }

    pub fn reset_selection(&self) {
        let _ = self.select(Select::Reset);
    }

    pub fn get_selection(&self) -> Option<(u32, u32)> {
        self.with(|v, _, _| v.tbv.selection_info())
    }

    pub fn has_selection(&self) -> bool {
        self.get_selection().is_some()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_local_selection(
        &self,
        anchor_x: i32,
        anchor_y: i32,
        focus_x: i32,
        focus_y: i32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        update_cursor: bool,
        follow_cursor: bool,
        behavior: SelectionBehavior,
    ) -> Result<bool, TextError> {
        self.select(Select::Local { update: false, ax: anchor_x, ay: anchor_y, fx: focus_x, fy: focus_y, bg, fg, update_cursor, follow_cursor, behavior })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_local_selection(
        &self,
        anchor_x: i32,
        anchor_y: i32,
        focus_x: i32,
        focus_y: i32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        update_cursor: bool,
        follow_cursor: bool,
        behavior: SelectionBehavior,
    ) -> Result<bool, TextError> {
        self.select(Select::Local { update: true, ax: anchor_x, ay: anchor_y, fx: focus_x, fy: focus_y, bg, fg, update_cursor, follow_cursor, behavior })
    }

    pub fn reset_local_selection(&self) {
        let _ = self.select(Select::LocalReset);
    }

    pub fn convert_selection_to_cell(&self) -> bool {
        self.select(Select::Cell).unwrap_or(false)
    }

    pub fn set_selection_occupancy(&self, occupancy: SelectionOccupancy) {
        let _ = self.select(Select::Occupancy(occupancy));
    }

    pub fn get_selection_occupancy(&self) -> SelectionOccupancy {
        self.with(|v, _, _| v.tbv.selection_occupancy)
    }

    pub fn set_selection_inclusive(&self, start: u32, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        let _ = self.select(Select::Inclusive(start, end, bg, fg));
    }

    pub fn set_selection_colors(&self, bg: Option<Rgba>, fg: Option<Rgba>) {
        let _ = self.select(Select::Colors(bg, fg));
    }

    pub fn get_selected_text(&self) -> String {
        self.with(|v, eb, _| {
            let tb = v.active_buffer(eb);
            decode_text(v.tbv.selected_text(tb))
        })
    }

    pub fn get_cursor(&self) -> (u32, u32) {
        let s = self.shared.borrow();
        (s.eb.cursor.row, s.eb.cursor.col)
    }

    pub fn get_text(&self) -> String {
        self.buffer().get_text()
    }

    fn position(&self, f: impl FnOnce(&mut EditorViewCore, &mut EditBufferCore) -> VisualCursor) -> VisualCursor {
        self.with(|v, eb, _| {
            Self::prepare(v, eb);
            f(v, eb)
        })
    }

    /// `getVisualCursor`: visual coordinates relative to the viewport.
    pub fn get_visual_cursor(&self) -> VisualCursor {
        self.position(|v, eb| v.visual_cursor(eb))
    }

    pub fn move_up_visual(&self) {
        self.with(|v, eb, _| {
            Self::prepare(v, eb);
            v.move_up_visual(eb);
        })
    }

    pub fn move_down_visual(&self) {
        self.with(|v, eb, _| {
            Self::prepare(v, eb);
            v.move_down_visual(eb);
        })
    }

    pub fn delete_selected_text(&self) -> Result<(), TextError> {
        self.with(|v, eb, others| v.delete_selected_text(eb, others))
    }

    /// `_replaceSelectedText`: the selection (or the cursor position) replaced by `text`, as a deletion step and
    /// an insertion step.
    pub fn replace_selected_text(&self, text: &str) -> Result<Replacement, TextError> {
        self.with(|v, eb, others| {
            validate_text_bytes(eb.tb.tab_width, text.len(), eb.tb.byte_size())?;
            let steps = v.replace_selected_text(eb, text.as_bytes(), others)?;
            Ok(Replacement { deleted: steps & 1 != 0, inserted: steps & 2 != 0 })
        })
    }

    pub fn set_cursor_by_offset(&self, offset: u32) {
        self.with(|v, eb, others| v.set_cursor_by_offset(eb, offset, others))
    }

    pub fn get_next_word_boundary(&self) -> VisualCursor {
        self.position(|v, eb| v.next_word_boundary(eb))
    }

    pub fn get_prev_word_boundary(&self) -> VisualCursor {
        self.position(|v, eb| v.prev_word_boundary(eb))
    }

    pub fn get_eol(&self) -> VisualCursor {
        self.position(|v, eb| v.eol(eb))
    }

    pub fn get_visual_sol(&self) -> VisualCursor {
        self.position(|v, eb| v.visual_sol(eb))
    }

    pub fn get_visual_eol(&self) -> VisualCursor {
        self.position(|v, eb| v.visual_eol(eb))
    }

    pub fn goto_visual_line_end(&self) {
        self.with(|v, eb, others| {
            Self::prepare(v, eb);
            v.goto_visual_line_end(eb, others);
        })
    }

    /// `getLineInfo`: the viewport's visual lines, after following the cursor.
    pub fn get_line_info(&self) -> LineInfo {
        self.with(|v, eb, _| {
            Self::prepare(v, eb);
            v.cached_line_info(eb)
        })
    }

    pub fn get_logical_line_info(&self) -> LineInfo {
        self.with(|v, eb, _| {
            Self::prepare(v, eb);
            v.logical_line_info(eb)
        })
    }

    /// `setPlaceholderStyledText`: shown while the text is empty; an empty list removes it.
    pub fn set_placeholder_styled_text(&self, chunks: &[(&str, Option<Rgba>, Option<Rgba>, u32)]) -> Result<(), TextError> {
        self.with(|v, eb, _| {
            let chunks: Vec<_> = chunks.iter().filter(|c| !c.0.is_empty()).collect();
            let total: usize = chunks.iter().map(|c| c.0.len()).sum();
            validate_text_bytes(eb.tb.tab_width, total, 0)?;
            if chunks.iter().any(|c| c.3 & !crate::link::attributes::BASE_MASK != 0) {
                return Err(TextError::InvalidOptions);
            }
            if chunks.is_empty() {
                v.clear_placeholder(eb);
                return Ok(());
            }
            let mut bytes = Vec::with_capacity(total);
            let ranges: Vec<OwnedStyledChunk> = chunks
                .iter()
                .map(|c| {
                    bytes.extend_from_slice(c.0.as_bytes());
                    OwnedStyledChunk { byte_count: c.0.len() as u32, style: StyleDefinition { fg: c.1, bg: c.2, attributes: c.3 } }
                })
                .collect();
            v.set_placeholder_owned_styled_text(eb, bytes, &ranges)
        })
    }

    pub fn set_tab_indicator(&self, code_point: u32) -> Result<(), TextError> {
        let indicator = (code_point != 0).then_some(code_point);
        validate_tab_indicator(indicator)?;
        self.with(|v, _, _| v.tbv.tab_indicator = indicator);
        Ok(())
    }

    pub fn set_tab_indicator_color(&self, color: Option<Rgba>) {
        self.with(|v, _, _| v.tbv.tab_indicator_color = color)
    }

    pub fn measure_for_dimensions(&self, width: u32, height: u32) -> Result<MeasureResult, TextError> {
        if width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(TextError::InvalidOptions);
        }
        Ok(self.with(|v, eb, _| {
            let (tbv, tb) = v.split(eb);
            tbv.measure_for_dimensions(tb, width)
        }))
    }

    /// Yoga's measure function for an editor node (`NativeRenderable.measure`).
    pub fn yoga_measure(&self, width: f32, width_mode: u32, height: f32, absolute: bool) -> (f32, f32) {
        self.with(|v, eb, _| {
            let (tbv, tb) = v.split(eb);
            yoga_measure_with(width, width_mode, height, absolute, |w| tbv.measure_for_dimensions(tb, w))
        })
    }

    /// The scene's resize hook: the laid-out size becomes the viewport size.
    pub fn apply_layout(&self, width: u32, height: u32) {
        self.with(|v, eb, _| {
            if v.tbv.viewport.is_none_or(|vp| vp.width != width || vp.height != height) {
                v.set_viewport_size(eb, width, height);
            }
            v.prepare_lines(eb);
        })
    }

    /// Prepares the layout (following the cursor) and passes what drawing reads, as `drawEditorView` does.
    /// `drawEditorViewOn` calls this only when the origin is inside the target (`x < width && y < height`); the
    /// draw itself follows the cursor, so a skipped draw does not.
    pub fn draw<R>(&self, draw_y: i32, f: impl FnOnce(&DrawSource) -> R) -> R {
        self.with(|v, eb, _| {
            v.tbv.set_draw_y(draw_y);
            v.prepare_lines(eb);
            v.update_cursor_before_render(eb);
            let fill = eb.tb.default_bg.filter(|bg| bg[3] & 0xff != 0);
            let (tbv, tb) = v.split(eb);
            tbv.virtual_lines(tb);
            f(&DrawSource { tb, view: tbv, default_bg_fill: fill })
        })
    }
}
