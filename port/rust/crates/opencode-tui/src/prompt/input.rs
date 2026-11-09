//! The prompt's handle on its `<textarea>` (`TextareaRenderable`), with the members index.tsx and
//! autocomplete.tsx use, over the core textarea's element API, its edit buffer and its editor view.
use opencode_tui_core::renderables::textarea::{EditorTraits, TextareaAction};
use opencode_tui_core::tree::Element;
use opencode_tui_text::{EditBuffer, EditorView, Extmarks, LogicalCursor, SyntaxStyle, VisualCursor, WidthMethod};
use std::rc::Rc;

pub use opencode_tui_input::paste::strip_ansi_sequences;

#[derive(Clone)]
pub struct Textarea {
    pub el: Element,
    buffer: EditBuffer,
    view: Rc<EditorView>,
}

impl Textarea {
    pub fn of(el: Element) -> Option<Textarea> {
        Some(Textarea { el, buffer: el.edit_buffer()?, view: el.editor_view()? })
    }

    /// `plainText`.
    pub fn plain_text(&self) -> String {
        self.el.textarea_text()
    }
    /// `cursorOffset`.
    pub fn cursor_offset(&self) -> u32 {
        self.buffer.get_cursor_position().offset
    }
    /// `cursorOffset = offset`.
    pub fn set_cursor_offset(&self, offset: u32) {
        self.el.set_textarea_cursor_offset(offset);
    }
    /// `logicalCursor`.
    pub fn logical_cursor(&self) -> LogicalCursor {
        self.buffer.get_cursor_position()
    }
    /// `visualCursor`.
    pub fn visual_cursor(&self) -> VisualCursor {
        self.view.get_visual_cursor()
    }
    /// `scrollY`.
    pub fn scroll_y(&self) -> u32 {
        self.view.get_viewport().y
    }
    /// `editorView.getTotalVirtualLineCount()`.
    pub fn total_virtual_line_count(&self) -> u32 {
        self.view.get_total_virtual_line_count()
    }
    /// `extmarks`.
    pub fn extmarks(&self) -> Extmarks {
        self.view.extmarks()
    }
    pub fn width_method(&self) -> WidthMethod {
        self.buffer.width_method()
    }
    pub fn traits(&self) -> EditorTraits {
        self.el.textarea_traits()
    }
    pub fn set_traits(&self, traits: EditorTraits) {
        self.el.set_textarea_traits(traits);
    }
    /// `syntaxStyle = style`.
    pub fn set_syntax_style(&self, style: Option<&SyntaxStyle>) {
        self.buffer.set_syntax_style(style);
        self.el.request_render();
    }
    /// `insertText(text)`.
    pub fn insert_text(&self, text: &str) {
        self.el.textarea_insert_text(text);
    }
    /// `deleteRange(startLine, startCol, endLine, endCol)`.
    pub fn delete_range(&self, start_line: u32, start_col: u32, end_line: u32, end_col: u32) {
        self.el.textarea_delete_range(start_line, start_col, end_line, end_col);
    }
    /// `setSelection(start, end)`.
    pub fn set_selection(&self, start: u32, end: u32) {
        self.el.set_textarea_selection(start, end);
    }
    /// `getTextRange(start, end)`.
    pub fn get_text_range(&self, start: u32, end: u32) -> String {
        self.buffer.get_text_range(start, end)
    }
    /// `gotoBufferEnd()`.
    pub fn goto_buffer_end(&self) {
        self.el.textarea_action(TextareaAction::BufferEnd);
    }
    /// `moveCursorUp()`.
    pub fn move_cursor_up(&self) {
        self.el.textarea_action(TextareaAction::MoveUp);
    }
    /// `moveCursorDown()`.
    pub fn move_cursor_down(&self) {
        self.el.textarea_action(TextareaAction::MoveDown);
    }
    /// `setText(text)`.
    pub fn set_text(&self, text: &str) {
        self.el.textarea_set_text(text);
    }
    /// `clear()`.
    pub fn clear(&self) {
        self.el.textarea_clear();
    }
    /// `invalidateIntrinsicSize()`.
    pub fn invalidate_intrinsic_size(&self) {
        self.el.mark_layout_dirty();
    }
}
