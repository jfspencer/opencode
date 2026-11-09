//! Port of `lib/extmarks.ts` and `lib/extmarks-history.ts`: ranges of an edit buffer that move with its edits,
//! optionally styled (as highlights) and virtual (the cursor skips them, deletion removes them whole), with their
//! own undo history.
//!
//! The TS controller replaces methods of its `EditBuffer` and `EditorView` (`moveCursorLeft`, `insertText`,
//! `undo`, ...). Here [`EditorView::extmarks`] installs it on the buffer, and from then on those methods of
//! [`EditBuffer`] and [`EditorView`] run through it, until [`Extmarks::destroy`] (or the view's drop). Its
//! content-changed listener runs when the host delivers that event ([`EditBuffer::dispatch_events`]).
//!
//! Offsets are cells plus one per line break, as JS numbers: `i64` here.

use crate::edit::{EditBuffer, EditorView};
use crate::text::HighlightRange;
use crate::TextError;
use indexmap::{IndexMap, IndexSet};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

/// An extmark (`Extmark`).
#[derive(Clone)]
pub struct Extmark {
    pub id: u32,
    pub start: i64,
    pub end: i64,
    pub virtual_: bool,
    pub style_id: Option<u32>,
    pub priority: Option<u32>,
    pub data: Option<Rc<dyn Any>>,
    pub type_id: u32,
}

impl std::fmt::Debug for Extmark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extmark")
            .field("id", &self.id)
            .field("start", &self.start)
            .field("end", &self.end)
            .field("virtual", &self.virtual_)
            .field("style_id", &self.style_id)
            .field("priority", &self.priority)
            .field("type_id", &self.type_id)
            .finish()
    }
}

/// `ExtmarkOptions`.
#[derive(Clone, Default)]
pub struct ExtmarkOptions {
    pub start: i64,
    pub end: i64,
    pub virtual_: bool,
    pub style_id: Option<u32>,
    pub priority: Option<u32>,
    pub data: Option<Rc<dyn Any>>,
    pub type_id: Option<u32>,
    pub metadata: Option<Rc<dyn Any>>,
}

#[derive(Clone)]
struct Saved {
    extmark: Extmark,
    metadata: Option<Rc<dyn Any>>,
}

#[derive(Clone)]
struct Snapshot {
    extmarks: IndexMap<u32, Saved>,
    next_id: u32,
}

pub(crate) struct ExtmarksState {
    /// The editor view the controller was created for (its selection decides the wrapped motions).
    pub view: usize,
    extmarks: IndexMap<u32, Extmark>,
    by_type: IndexMap<u32, IndexSet<u32>>,
    metadata: IndexMap<u32, Rc<dyn Any>>,
    next_id: u32,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    type_name_to_id: IndexMap<String, u32>,
    type_id_to_name: IndexMap<u32, String>,
    next_type_id: u32,
}

impl ExtmarksState {
    pub fn new(view: usize) -> Self {
        ExtmarksState {
            view,
            extmarks: IndexMap::new(),
            by_type: IndexMap::new(),
            metadata: IndexMap::new(),
            next_id: 1,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            type_name_to_id: IndexMap::new(),
            type_id_to_name: IndexMap::new(),
            next_type_id: 1,
        }
    }

    fn delete_by_id(&mut self, id: u32) {
        if let Some(extmark) = self.extmarks.shift_remove(&id) {
            if let Some(set) = self.by_type.get_mut(&extmark.type_id) {
                set.shift_remove(&id);
            }
            self.metadata.shift_remove(&id);
        }
    }

    fn virtual_containing(&self, offset: i64) -> Option<Extmark> {
        self.extmarks.values().find(|e| e.virtual_ && offset >= e.start && offset < e.end).cloned()
    }

    fn adjust_after_insertion(&mut self, offset: i64, length: i64) {
        for e in self.extmarks.values_mut() {
            if e.start >= offset {
                e.start += length;
                e.end += length;
            } else if e.end > offset {
                e.end += length;
            }
        }
    }

    fn adjust_after_deletion(&mut self, offset: i64, length: i64) {
        let mut doomed = Vec::new();
        for e in self.extmarks.values_mut() {
            if e.end <= offset {
                continue;
            }
            if e.start >= offset + length {
                e.start -= length;
                e.end -= length;
            } else if e.start >= offset && e.end <= offset + length {
                doomed.push(e.id);
            } else if e.start < offset && e.end > offset + length {
                e.end -= length;
            } else if e.start < offset && e.end > offset {
                e.end -= e.end.min(offset + length) - offset;
            } else if e.start < offset + length && e.end > offset + length {
                e.start = offset;
                e.end -= length;
            }
        }
        for id in doomed {
            self.delete_by_id(id);
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            extmarks: self
                .extmarks
                .iter()
                .map(|(&id, e)| (id, Saved { extmark: e.clone(), metadata: self.metadata.get(&id).cloned() }))
                .collect(),
            next_id: self.next_id,
        }
    }

    fn save_snapshot(&mut self) {
        let snapshot = self.snapshot();
        self.undo_stack.push(snapshot);
        self.redo_stack.clear();
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.extmarks.clear();
        self.by_type.clear();
        self.metadata.clear();
        for (id, saved) in snapshot.extmarks {
            self.by_type.entry(saved.extmark.type_id).or_default().insert(id);
            self.extmarks.insert(id, saved.extmark);
            if let Some(m) = saved.metadata {
                self.metadata.insert(id, m);
            }
        }
        self.next_id = snapshot.next_id;
    }

    fn clear_marks(&mut self) {
        self.extmarks.clear();
        self.by_type.clear();
        self.metadata.clear();
    }

    /// What `destroy` empties (the id counters stay).
    pub(crate) fn clear_destroyed(&mut self) {
        self.clear_marks();
        self.type_name_to_id.clear();
        self.type_id_to_name.clear();
        self.undo_stack.clear();
        self.redo_stack.clear();
    }
}

pub(crate) type SharedExtmarks = Rc<RefCell<ExtmarksState>>;

/// `ExtmarksController`: a handle to the buffer's installed controller.
#[derive(Clone)]
pub struct Extmarks {
    pub(crate) state: SharedExtmarks,
    pub(crate) buffer: EditBuffer,
}

/// `updateHighlights`: the styled extmarks as the buffer's highlights.
pub(crate) fn update_highlights(state: &SharedExtmarks, buffer: &EditBuffer) -> Result<(), TextError> {
    buffer.clear_all_highlights();
    let marks: Vec<Extmark> = state.borrow().extmarks.values().cloned().collect();
    for e in marks {
        if let Some(style_id) = e.style_id {
            let start = offset_excluding_newlines(buffer, e.start);
            let end = offset_excluding_newlines(buffer, e.end);
            buffer.add_highlight_by_char_range(HighlightRange {
                start: start.clamp(0, u32::MAX as i64) as u32,
                end: end.clamp(0, u32::MAX as i64) as u32,
                style_id,
                priority: e.priority.unwrap_or(0),
                hl_ref: e.id & 0xffff,
            })?;
        }
    }
    Ok(())
}

fn offset_excluding_newlines(buffer: &EditBuffer, offset: i64) -> i64 {
    let row = buffer.offset_to_position(offset.clamp(0, u32::MAX as i64) as u32).map_or(0, |(row, _)| row);
    offset - row as i64
}

fn to_u32(v: i64) -> u32 {
    v.clamp(0, u32::MAX as i64) as u32
}

/// The wrapped operations (`wrapCursorMovement`, `wrapDeletion`, `wrapInsertion`, ...).
pub(crate) mod wrapped {
    use super::*;

    fn has_selection(state: &SharedExtmarks, buffer: &EditBuffer) -> bool {
        buffer.view_has_selection(state.borrow().view)
    }

    /// `measureChange`: the cells an edit adds (negative when it removes them), from the end offset.
    fn measure_change(buffer: &EditBuffer, edit: impl FnOnce()) -> i64 {
        let before = buffer.get_eof().offset as i64;
        edit();
        buffer.get_eof().offset as i64 - before
    }

    fn shift(state: &SharedExtmarks, buffer: &EditBuffer, offset: i64, change: i64, update: bool) {
        if change > 0 {
            state.borrow_mut().adjust_after_insertion(offset, change);
        } else if change < 0 {
            state.borrow_mut().adjust_after_deletion(offset, -change);
        } else {
            return;
        }
        if update {
            let _ = update_highlights(state, buffer);
        }
    }

    pub fn move_cursor_left(state: &SharedExtmarks, buffer: &EditBuffer) {
        let current = buffer.get_cursor_position().offset as i64;
        if has_selection(state, buffer) {
            return buffer.raw_move_cursor_left();
        }
        let target = current - 1;
        if target < 0 {
            return buffer.raw_move_cursor_left();
        }
        let mark = state.borrow().virtual_containing(target);
        if let Some(mark) = mark
            && current >= mark.end
        {
            return set_cursor_by_offset(state, buffer, to_u32((mark.start - 1).max(0)));
        }
        buffer.raw_move_cursor_left();
    }

    pub fn move_cursor_right(state: &SharedExtmarks, buffer: &EditBuffer) {
        let current = buffer.get_cursor_position().offset as i64;
        if has_selection(state, buffer) {
            return buffer.raw_move_cursor_right();
        }
        let target = current + 1;
        if buffer.offset_to_position(to_u32(target)).is_none() {
            return buffer.raw_move_cursor_right();
        }
        let mark = state.borrow().virtual_containing(target);
        if let Some(mark) = mark
            && current <= mark.start
        {
            return set_cursor_by_offset(state, buffer, to_u32(mark.end));
        }
        buffer.raw_move_cursor_right();
    }

    pub fn move_vertical(state: &SharedExtmarks, buffer: &EditBuffer, view: &EditorView, down: bool) {
        let mv = |v: &EditorView| if down { v.raw_move_down_visual() } else { v.raw_move_up_visual() };
        if has_selection(state, buffer) {
            return mv(view);
        }
        let current = buffer.get_cursor_position().offset as i64;
        mv(view);
        let new = buffer.get_cursor_position().offset as i64;
        let mark = state.borrow().virtual_containing(new);
        if let Some(mark) = mark {
            let to_start = new - mark.start;
            let to_end = mark.end - new;
            if to_start < to_end {
                let target = if down {
                    let adjusted = mark.start - 1;
                    if adjusted <= current { mark.end } else { adjusted }
                } else {
                    (mark.start - 1).max(0)
                };
                view.raw_set_cursor_by_offset(to_u32(target));
            } else {
                view.raw_set_cursor_by_offset(to_u32(mark.end));
            }
        }
    }

    pub fn set_cursor_by_offset(state: &SharedExtmarks, buffer: &EditBuffer, offset: u32) {
        let current = buffer.get_cursor_position().offset as i64;
        if has_selection(state, buffer) {
            return buffer.raw_set_cursor_by_offset(offset);
        }
        let offset_i = offset as i64;
        if offset_i > current {
            let mark = state.borrow().virtual_containing(offset_i);
            if let Some(mark) = mark
                && current <= mark.start
            {
                return buffer.raw_set_cursor_by_offset(to_u32(mark.end));
            }
        } else {
            let found = state
                .borrow()
                .extmarks
                .values()
                .find(|e| e.virtual_ && current >= e.end && offset_i < e.end && offset_i >= e.start)
                .map(|e| e.start);
            if let Some(start) = found {
                return buffer.raw_set_cursor_by_offset(to_u32((start - 1).max(0)));
            }
        }
        buffer.raw_set_cursor_by_offset(offset);
    }

    fn delete_virtual(state: &SharedExtmarks, buffer: &EditBuffer, mark: &Extmark) -> Result<(), TextError> {
        let (sr, sc) = buffer.offset_to_position(to_u32(mark.start)).unwrap_or((0, 0));
        let (er, ec) = buffer.offset_to_position(to_u32(mark.end)).unwrap_or((0, 0));
        let mut result = Ok(());
        let change = measure_change(buffer, || result = buffer.raw_delete_range(sr, sc, er, ec));
        result?;
        state.borrow_mut().save_snapshot();
        state.borrow_mut().delete_by_id(mark.id);
        shift(state, buffer, mark.start, change, true);
        Ok(())
    }

    pub fn delete_char_backward(state: &SharedExtmarks, buffer: &EditBuffer) -> Result<(), TextError> {
        let current = buffer.get_cursor_position().offset as i64;
        // TODO(port): with a selection the buffer still deletes one grapheme, but TS (and so this) shifts no
        // extmark, which then lags the text; `deleteChar` below does the same.
        if current == 0 || has_selection(state, buffer) {
            buffer.raw_delete_char_backward()?;
            state.borrow_mut().save_snapshot();
            return Ok(());
        }
        let mark = state.borrow().virtual_containing(current - 1);
        if let Some(mark) = mark
            && current == mark.end
        {
            return delete_virtual(state, buffer, &mark);
        }
        let delete_offset = buffer.get_prev_char_boundary().offset as i64;
        let mut result = Ok(());
        let change = measure_change(buffer, || result = buffer.raw_delete_char_backward());
        result?;
        state.borrow_mut().save_snapshot();
        shift(state, buffer, delete_offset, change, true);
        Ok(())
    }

    pub fn delete_char(state: &SharedExtmarks, buffer: &EditBuffer) -> Result<(), TextError> {
        let current = buffer.get_cursor_position().offset as i64;
        if has_selection(state, buffer) {
            buffer.raw_delete_char()?;
            state.borrow_mut().save_snapshot();
            return Ok(());
        }
        let mark = state.borrow().virtual_containing(current);
        if let Some(mark) = mark
            && current == mark.start
        {
            return delete_virtual(state, buffer, &mark);
        }
        let mut result = Ok(());
        let change = measure_change(buffer, || result = buffer.raw_delete_char());
        result?;
        state.borrow_mut().save_snapshot();
        shift(state, buffer, current, change, true);
        Ok(())
    }

    pub fn delete_range(state: &SharedExtmarks, buffer: &EditBuffer, sl: u32, sc: u32, el: u32, ec: u32) -> Result<(), TextError> {
        let start = (buffer.position_to_offset(sl, sc) as i64).min(buffer.position_to_offset(el, ec) as i64);
        let mut result = Ok(());
        let change = measure_change(buffer, || result = buffer.raw_delete_range(sl, sc, el, ec));
        result?;
        state.borrow_mut().save_snapshot();
        shift(state, buffer, start, change, true);
        Ok(())
    }

    pub fn delete_line(state: &SharedExtmarks, buffer: &EditBuffer) -> Result<(), TextError> {
        let row = buffer.get_cursor_position().row;
        let line_start = buffer.get_line_start_offset(row) as i64;
        let (start, end) = if row + 1 < buffer.get_line_count() {
            (line_start, buffer.get_line_start_offset(row + 1) as i64)
        } else {
            ((line_start - 1).max(0), buffer.get_eol().offset as i64)
        };
        buffer.raw_delete_line()?;
        state.borrow_mut().save_snapshot();
        state.borrow_mut().adjust_after_deletion(start, end - start);
        let _ = update_highlights(state, buffer);
        Ok(())
    }

    pub fn insert_text(state: &SharedExtmarks, buffer: &EditBuffer, text: &str) -> Result<(), TextError> {
        let current = buffer.get_cursor_position().offset as i64;
        let mut result = Ok(());
        let change = measure_change(buffer, || result = buffer.raw_insert_text(text));
        result?;
        state.borrow_mut().save_snapshot();
        shift(state, buffer, current, change, true);
        Ok(())
    }

    pub fn set_text(state: &SharedExtmarks, buffer: &EditBuffer, text: &str, replace: bool) -> Result<(), TextError> {
        if replace {
            buffer.raw_replace_text(text)?;
            state.borrow_mut().save_snapshot();
        } else {
            buffer.raw_set_text(text)?;
        }
        state.borrow_mut().clear_marks();
        let _ = update_highlights(state, buffer);
        Ok(())
    }

    pub fn clear(state: &SharedExtmarks, buffer: &EditBuffer) {
        buffer.raw_clear();
        state.borrow_mut().save_snapshot();
        state.borrow_mut().clear_marks();
        let _ = update_highlights(state, buffer);
    }

    pub fn new_line(state: &SharedExtmarks, buffer: &EditBuffer) -> Result<(), TextError> {
        let current = buffer.get_cursor_position().offset as i64;
        buffer.raw_new_line()?;
        state.borrow_mut().save_snapshot();
        state.borrow_mut().adjust_after_insertion(current, 1);
        let _ = update_highlights(state, buffer);
        Ok(())
    }

    pub fn replace_selected_text(
        state: &SharedExtmarks,
        buffer: &EditBuffer,
        view: &EditorView,
        text: &str,
    ) -> Result<crate::edit::Replacement, TextError> {
        let delete_offset = view.get_selection().map_or(0, |(s, e)| s.min(e)) as i64;
        let cursor_offset = buffer.get_cursor_position().offset as i64;
        let mut result = Err(TextError::InvalidOptions);
        let change = measure_change(buffer, || result = view.raw_replace_selected_text(text));
        let result = result?;
        let insert_offset = if result.deleted { delete_offset } else { cursor_offset };
        let insert_length = if result.inserted { buffer.get_cursor_position().offset as i64 - insert_offset } else { 0 };
        if result.deleted {
            state.borrow_mut().save_snapshot();
            shift(state, buffer, delete_offset, change - insert_length, false);
        }
        if result.inserted {
            state.borrow_mut().save_snapshot();
        }
        shift(state, buffer, insert_offset, insert_length, false);
        if change != 0 || insert_length != 0 {
            let _ = update_highlights(state, buffer);
        }
        Ok(result)
    }

    pub fn delete_selected_text(state: &SharedExtmarks, buffer: &EditBuffer, view: &EditorView) -> Result<(), TextError> {
        let Some((s, e)) = view.get_selection() else {
            view.raw_delete_selected_text()?;
            state.borrow_mut().save_snapshot();
            return Ok(());
        };
        let delete_offset = s.min(e) as i64;
        let mut result = Ok(());
        let change = measure_change(buffer, || result = view.raw_delete_selected_text());
        result?;
        state.borrow_mut().save_snapshot();
        shift(state, buffer, delete_offset, change, true);
        Ok(())
    }

    pub fn undo(state: &SharedExtmarks, buffer: &EditBuffer, redo: bool) -> Option<String> {
        let can = if redo { !state.borrow().redo_stack.is_empty() } else { !state.borrow().undo_stack.is_empty() };
        if !can {
            return if redo { buffer.raw_redo() } else { buffer.raw_undo() };
        }
        let result = if redo { buffer.raw_redo() } else { buffer.raw_undo() };
        let mut s = state.borrow_mut();
        let current = s.snapshot();
        let snapshot = if redo {
            s.undo_stack.push(current);
            s.redo_stack.pop().unwrap()
        } else {
            s.redo_stack.push(current);
            s.undo_stack.pop().unwrap()
        };
        s.restore(snapshot);
        drop(s);
        let _ = update_highlights(state, buffer);
        result
    }
}

impl Extmarks {
    fn live(&self) -> Result<(), TextError> {
        if self.buffer.extmarks_state().is_some_and(|s| Rc::ptr_eq(&s, &self.state)) {
            Ok(())
        } else {
            Err(TextError::Destroyed)
        }
    }

    fn alive(&self) -> bool {
        self.live().is_ok()
    }

    /// `create`: the new extmark's id. A rejected highlight (priority above 255) leaves the controller as it was.
    pub fn create(&self, options: ExtmarkOptions) -> Result<u32, TextError> {
        self.live()?;
        let id = {
            let mut s = self.state.borrow_mut();
            let id = s.next_id;
            s.next_id += 1;
            let type_id = options.type_id.unwrap_or(0);
            s.extmarks.insert(
                id,
                Extmark {
                    id,
                    start: options.start,
                    end: options.end,
                    virtual_: options.virtual_,
                    style_id: options.style_id,
                    priority: options.priority,
                    data: options.data,
                    type_id,
                },
            );
            s.by_type.entry(type_id).or_default().insert(id);
            if let Some(m) = options.metadata {
                s.metadata.insert(id, m);
            }
            id
        };
        if let Err(e) = update_highlights(&self.state, &self.buffer) {
            self.state.borrow_mut().delete_by_id(id);
            let _ = update_highlights(&self.state, &self.buffer);
            return Err(e);
        }
        Ok(id)
    }

    pub fn delete(&self, id: u32) -> Result<bool, TextError> {
        self.live()?;
        if !self.state.borrow().extmarks.contains_key(&id) {
            return Ok(false);
        }
        self.state.borrow_mut().delete_by_id(id);
        let _ = update_highlights(&self.state, &self.buffer);
        Ok(true)
    }

    pub fn get(&self, id: u32) -> Option<Extmark> {
        if !self.alive() {
            return None;
        }
        self.state.borrow().extmarks.get(&id).cloned()
    }

    pub fn get_all(&self) -> Vec<Extmark> {
        if !self.alive() {
            return Vec::new();
        }
        self.state.borrow().extmarks.values().cloned().collect()
    }

    pub fn get_virtual(&self) -> Vec<Extmark> {
        self.get_all().into_iter().filter(|e| e.virtual_).collect()
    }

    pub fn get_at_offset(&self, offset: i64) -> Vec<Extmark> {
        self.get_all().into_iter().filter(|e| offset >= e.start && offset < e.end).collect()
    }

    pub fn get_all_for_type_id(&self, type_id: u32) -> Vec<Extmark> {
        if !self.alive() {
            return Vec::new();
        }
        let s = self.state.borrow();
        let Some(ids) = s.by_type.get(&type_id) else { return Vec::new() };
        ids.iter().filter_map(|id| s.extmarks.get(id).cloned()).collect()
    }

    pub fn clear(&self) {
        if !self.alive() {
            return;
        }
        self.state.borrow_mut().clear_marks();
        let _ = update_highlights(&self.state, &self.buffer);
    }

    /// `adjustExtmarksAfterDeletion`.
    pub fn adjust_extmarks_after_deletion(&self, offset: i64, length: i64, update_highlights_after: bool) {
        self.state.borrow_mut().adjust_after_deletion(offset, length);
        if update_highlights_after {
            let _ = update_highlights(&self.state, &self.buffer);
        }
    }

    pub fn register_type(&self, name: &str) -> Result<u32, TextError> {
        self.live()?;
        let mut s = self.state.borrow_mut();
        if let Some(&id) = s.type_name_to_id.get(name) {
            return Ok(id);
        }
        let id = s.next_type_id;
        s.next_type_id += 1;
        s.type_name_to_id.insert(name.to_owned(), id);
        s.type_id_to_name.insert(id, name.to_owned());
        Ok(id)
    }

    pub fn get_type_id(&self, name: &str) -> Option<u32> {
        if !self.alive() {
            return None;
        }
        self.state.borrow().type_name_to_id.get(name).copied()
    }

    pub fn get_type_name(&self, id: u32) -> Option<String> {
        if !self.alive() {
            return None;
        }
        self.state.borrow().type_id_to_name.get(&id).cloned()
    }

    pub fn get_metadata_for(&self, id: u32) -> Option<Rc<dyn Any>> {
        if !self.alive() {
            return None;
        }
        self.state.borrow().metadata.get(&id).cloned()
    }

    /// `destroy`: uninstalls the controller; the methods run unwrapped again.
    pub fn destroy(&self) {
        if self.alive() {
            self.buffer.uninstall_extmarks();
            self.state.borrow_mut().clear_destroyed();
        }
    }
}
