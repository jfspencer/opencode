//! `<textarea>` (`Textarea.ts` over `EditBufferRenderable.ts`): `opencode-tui-text`'s EditBuffer and EditorView
//! (rope edits, undo, word and visual motions, wrapping, the placeholder, the viewport following the cursor,
//! measuring for Yoga, `drawEditorView`), with the terminal cursor, the editor traits, the content-change,
//! cursor-change and submit callbacks, and EditBufferRenderable's edit methods as [`TextareaAction`]s.
//!
//! Keys: while its traits say `suspend` (the keymap's managed textarea layer sets it on the focused textarea, see
//! [`crate::edit_buffer_bindings`]) the textarea only inserts printable keys; otherwise it also runs
//! `defaultTextareaKeyBindings`.
//!
//! The textarea's state lives in a registry keyed by its element, so its getters and setters work while the tree
//! has the kind out (from its own callbacks, which run during a key press).
//!
//! TODO(port-tui): the `keyBindings`/`keyAliasMap` props, mouse selection, extmarks and the syntax style prop. The
//! keyboard selection (`select-*`) keeps its anchor here instead of in the renderer's selection
//! (`startSelection`/`updateSelection`), so it does not reach other renderables' selections.
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use opencode_tui_text::{EditBuffer, EditEvent, EditorView, SelectionBehavior, WrapMode};

use crate::buffer::OptimizedBuffer;
use crate::color::RGBA;
use crate::input::{KeyEvent, PasteEvent};
use crate::layout::MeasureMode;
use crate::renderables::text::text_context;
use crate::tree::{self, Element, Kind};

/// `EditorTraits`: what the focused editor tells the keymap and plugins.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorTraits {
    /// `EditorCapture[]`: `"escape"`, `"navigate"`, `"submit"`, `"tab"`.
    pub capture: Option<Vec<&'static str>>,
    /// The textarea's own key bindings are off (the keymap runs them).
    pub suspend: bool,
    pub status: Option<String>,
}

/// `TextareaAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextareaAction {
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    LineHome,
    LineEnd,
    SelectLineHome,
    SelectLineEnd,
    VisualLineHome,
    VisualLineEnd,
    SelectVisualLineHome,
    SelectVisualLineEnd,
    BufferHome,
    BufferEnd,
    SelectBufferHome,
    SelectBufferEnd,
    DeleteLine,
    DeleteToLineEnd,
    DeleteToLineStart,
    Backspace,
    Delete,
    Newline,
    Undo,
    Redo,
    WordForward,
    WordBackward,
    SelectWordForward,
    SelectWordBackward,
    DeleteWordForward,
    DeleteWordBackward,
    SelectAll,
    Submit,
}

impl TextareaAction {
    /// Every action, in `editBufferActions` order.
    pub const ALL: [TextareaAction; 36] = [
        TextareaAction::MoveLeft,
        TextareaAction::MoveRight,
        TextareaAction::MoveUp,
        TextareaAction::MoveDown,
        TextareaAction::SelectLeft,
        TextareaAction::SelectRight,
        TextareaAction::SelectUp,
        TextareaAction::SelectDown,
        TextareaAction::LineHome,
        TextareaAction::LineEnd,
        TextareaAction::SelectLineHome,
        TextareaAction::SelectLineEnd,
        TextareaAction::VisualLineHome,
        TextareaAction::VisualLineEnd,
        TextareaAction::SelectVisualLineHome,
        TextareaAction::SelectVisualLineEnd,
        TextareaAction::BufferHome,
        TextareaAction::BufferEnd,
        TextareaAction::SelectBufferHome,
        TextareaAction::SelectBufferEnd,
        TextareaAction::DeleteLine,
        TextareaAction::DeleteToLineEnd,
        TextareaAction::DeleteToLineStart,
        TextareaAction::Backspace,
        TextareaAction::Delete,
        TextareaAction::Newline,
        TextareaAction::Undo,
        TextareaAction::Redo,
        TextareaAction::WordForward,
        TextareaAction::WordBackward,
        TextareaAction::SelectWordForward,
        TextareaAction::SelectWordBackward,
        TextareaAction::DeleteWordForward,
        TextareaAction::DeleteWordBackward,
        TextareaAction::SelectAll,
        TextareaAction::Submit,
    ];

    /// The action's name (`"move-left"`, ...).
    pub fn name(self) -> &'static str {
        match self {
            TextareaAction::MoveLeft => "move-left",
            TextareaAction::MoveRight => "move-right",
            TextareaAction::MoveUp => "move-up",
            TextareaAction::MoveDown => "move-down",
            TextareaAction::SelectLeft => "select-left",
            TextareaAction::SelectRight => "select-right",
            TextareaAction::SelectUp => "select-up",
            TextareaAction::SelectDown => "select-down",
            TextareaAction::LineHome => "line-home",
            TextareaAction::LineEnd => "line-end",
            TextareaAction::SelectLineHome => "select-line-home",
            TextareaAction::SelectLineEnd => "select-line-end",
            TextareaAction::VisualLineHome => "visual-line-home",
            TextareaAction::VisualLineEnd => "visual-line-end",
            TextareaAction::SelectVisualLineHome => "select-visual-line-home",
            TextareaAction::SelectVisualLineEnd => "select-visual-line-end",
            TextareaAction::BufferHome => "buffer-home",
            TextareaAction::BufferEnd => "buffer-end",
            TextareaAction::SelectBufferHome => "select-buffer-home",
            TextareaAction::SelectBufferEnd => "select-buffer-end",
            TextareaAction::DeleteLine => "delete-line",
            TextareaAction::DeleteToLineEnd => "delete-to-line-end",
            TextareaAction::DeleteToLineStart => "delete-to-line-start",
            TextareaAction::Backspace => "backspace",
            TextareaAction::Delete => "delete",
            TextareaAction::Newline => "newline",
            TextareaAction::Undo => "undo",
            TextareaAction::Redo => "redo",
            TextareaAction::WordForward => "word-forward",
            TextareaAction::WordBackward => "word-backward",
            TextareaAction::SelectWordForward => "select-word-forward",
            TextareaAction::SelectWordBackward => "select-word-backward",
            TextareaAction::DeleteWordForward => "delete-word-forward",
            TextareaAction::DeleteWordBackward => "delete-word-backward",
            TextareaAction::SelectAll => "select-all",
            TextareaAction::Submit => "submit",
        }
    }
}

/// A `KeyBinding` of the textarea's own table: a key name, its modifiers and the action.
#[derive(Clone, Copy, Debug)]
pub struct TextareaKeyBinding {
    pub name: &'static str,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_: bool,
    pub action: TextareaAction,
}

const fn bind(name: &'static str, mods: &str, action: TextareaAction) -> TextareaKeyBinding {
    let bytes = mods.as_bytes();
    let mut binding = TextareaKeyBinding { name, ctrl: false, shift: false, meta: false, super_: false, action };
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'c' => binding.ctrl = true,
            b's' => binding.shift = true,
            b'm' => binding.meta = true,
            b'S' => binding.super_ = true,
            _ => {}
        }
        i += 1;
    }
    binding
}

use TextareaAction as A;

/// `defaultTextareaKeyBindings` (Textarea.ts). Modifiers: `c` ctrl, `s` shift, `m` meta, `S` super.
pub const DEFAULT_TEXTAREA_KEY_BINDINGS: &[TextareaKeyBinding] = &[
    bind("left", "", A::MoveLeft),
    bind("right", "", A::MoveRight),
    bind("up", "", A::MoveUp),
    bind("down", "", A::MoveDown),
    bind("left", "s", A::SelectLeft),
    bind("right", "s", A::SelectRight),
    bind("up", "s", A::SelectUp),
    bind("down", "s", A::SelectDown),
    bind("home", "", A::BufferHome),
    bind("end", "", A::BufferEnd),
    bind("home", "s", A::SelectBufferHome),
    bind("end", "s", A::SelectBufferEnd),
    bind("a", "c", A::LineHome),
    bind("e", "c", A::LineEnd),
    bind("a", "cs", A::SelectLineHome),
    bind("e", "cs", A::SelectLineEnd),
    bind("a", "m", A::VisualLineHome),
    bind("e", "m", A::VisualLineEnd),
    bind("a", "ms", A::SelectVisualLineHome),
    bind("e", "ms", A::SelectVisualLineEnd),
    bind("f", "c", A::MoveRight),
    bind("b", "c", A::MoveLeft),
    bind("w", "c", A::DeleteWordBackward),
    bind("backspace", "c", A::DeleteWordBackward),
    bind("d", "m", A::DeleteWordForward),
    bind("delete", "m", A::DeleteWordForward),
    bind("delete", "c", A::DeleteWordForward),
    bind("d", "cs", A::DeleteLine),
    bind("k", "c", A::DeleteToLineEnd),
    bind("u", "c", A::DeleteToLineStart),
    bind("backspace", "", A::Backspace),
    bind("backspace", "s", A::Backspace),
    bind("d", "c", A::Delete),
    bind("delete", "", A::Delete),
    bind("delete", "s", A::Delete),
    bind("return", "", A::Newline),
    bind("kpenter", "", A::Newline),
    bind("linefeed", "", A::Newline),
    bind("return", "m", A::Submit),
    bind("kpenter", "m", A::Submit),
    bind("-", "c", A::Undo),
    bind(".", "c", A::Redo),
    bind("z", "S", A::Undo),
    bind("z", "Ss", A::Redo),
    bind("f", "m", A::WordForward),
    bind("b", "m", A::WordBackward),
    bind("right", "m", A::WordForward),
    bind("left", "m", A::WordBackward),
    bind("right", "c", A::WordForward),
    bind("left", "c", A::WordBackward),
    bind("f", "ms", A::SelectWordForward),
    bind("b", "ms", A::SelectWordBackward),
    bind("right", "ms", A::SelectWordForward),
    bind("left", "ms", A::SelectWordBackward),
    bind("backspace", "m", A::DeleteWordBackward),
    bind("left", "S", A::VisualLineHome),
    bind("right", "S", A::VisualLineEnd),
    bind("up", "S", A::BufferHome),
    bind("down", "S", A::BufferEnd),
    bind("left", "Ss", A::SelectVisualLineHome),
    bind("right", "Ss", A::SelectVisualLineEnd),
    bind("up", "Ss", A::SelectBufferHome),
    bind("down", "Ss", A::SelectBufferEnd),
    bind("a", "S", A::SelectAll),
];

/// `getKeyBindingAction(map, key)`: the key's name, then its base-layout name, with all four modifiers equal.
fn key_binding_action(key: &KeyEvent) -> Option<TextareaAction> {
    let find = |name: &str| {
        DEFAULT_TEXTAREA_KEY_BINDINGS
            .iter()
            .rev()
            .find(|b| b.name == name && b.ctrl == key.ctrl && b.shift == key.shift && b.meta == key.meta && b.super_ == key.super_)
            .map(|b| b.action)
    };
    find(&key.name).or_else(|| {
        opencode_tui_input::keymap::addons::base_layout_key_name(&key.name, key.base_code).and_then(|name| find(&name))
    })
}

struct Props {
    placeholder: String,
    placeholder_color: RGBA,
    text_color: RGBA,
    focused_text_color: RGBA,
    background: RGBA,
    focused_background: RGBA,
    cursor_color: RGBA,
    selection_bg: Option<RGBA>,
    selection_fg: Option<RGBA>,
    traits: EditorTraits,
    on_content_change: Option<Rc<dyn Fn()>>,
    on_submit: Option<Rc<dyn Fn()>>,
    on_cursor_change: Option<Rc<dyn Fn()>>,
}

/// A textarea's state.
pub struct Textarea {
    pub buffer: EditBuffer,
    pub view: Rc<EditorView>,
    props: RefCell<Props>,
    /// The keyboard selection's anchor cell, local to the textarea (the renderer selection's anchor).
    anchor: Cell<Option<(i32, i32)>>,
    keyboard_selection: Cell<bool>,
    /// The focus state the text color was last set for.
    synced_focus: Cell<Option<bool>>,
}

thread_local! {
    static TEXTAREAS: RefCell<HashMap<Element, Rc<Textarea>>> = RefCell::new(HashMap::new());
}

fn state(el: Element) -> Option<Rc<Textarea>> {
    TEXTAREAS.with(|t| t.borrow().get(&el).cloned())
}

impl Textarea {
    fn sync_colors(&self, focused: bool) {
        self.synced_focus.set(Some(focused));
        let props = self.props.borrow();
        let fg = if focused { props.focused_text_color } else { props.text_color };
        let _ = self.buffer.set_default_fg(Some(fg.0));
        let _ = self.view.set_placeholder_styled_text(&[(props.placeholder.as_str(), Some(props.placeholder_color.0), None, 0)]);
    }

    fn selection_colors(&self) -> (Option<opencode_tui_text::Rgba>, Option<opencode_tui_text::Rgba>) {
        let props = self.props.borrow();
        (props.selection_bg.map(|c| c.0), props.selection_fg.map(|c| c.0))
    }

    /// The edit events since the last call, as the renderable's callbacks.
    fn emit(&self, el: Element) {
        let events = self.buffer.take_events();
        if events.is_empty() {
            return;
        }
        el.mark_layout_dirty();
        el.request_render();
        let content = events.iter().any(|e| matches!(e, EditEvent::ContentChanged));
        let (on_content, on_cursor) = {
            let props = self.props.borrow();
            (props.on_content_change.clone(), props.on_cursor_change.clone())
        };
        if content {
            if let Some(change) = on_content {
                change();
            }
        }
        if let Some(change) = on_cursor {
            change();
        }
    }

    // `_ctx.clearSelection()`: the renderer's selection, here only the keyboard one, cleared.
    fn clear_renderer_selection(&self) {
        if self.anchor.take().is_some() {
            self.keyboard_selection.set(false);
            self.view.reset_local_selection();
        }
    }

    /// `clearSelection()`.
    fn clear_selection(&self, el: Element) -> bool {
        let had = self.view.has_selection();
        self.view.reset_local_selection();
        self.clear_renderer_selection();
        if had {
            el.request_render();
        }
        had
    }

    fn delete_selected_text(&self, el: Element) {
        let _ = self.view.delete_selected_text();
        self.clear_renderer_selection();
        el.request_render();
    }

    // Horizontal movement collapses to an edge without taking another step.
    fn collapse_selection_to_edge(&self, el: Element, start: bool) -> bool {
        let Some((from, to)) = self.view.get_selection() else { return false };
        self.buffer.set_cursor_by_offset(if start { from } else { to });
        self.clear_selection(el);
        true
    }

    /// `updateSelectionForMovement(shiftPressed, isBeforeMovement)`.
    fn update_selection_for_movement(&self, el: Element, select: bool, before: bool) {
        if !select {
            self.keyboard_selection.set(false);
            self.clear_selection(el);
            return;
        }
        if !el.selectable() {
            return;
        }
        self.keyboard_selection.set(true);
        let cursor = self.view.get_visual_cursor();
        let (x, y) = (cursor.visual_col as i32, cursor.visual_row as i32);
        let (bg, fg) = self.selection_colors();
        if before {
            if self.anchor.get().is_none() || !self.view.has_selection() {
                // `startSelection`: the previous selection goes, a new one starts at the cursor.
                self.clear_renderer_selection();
                self.anchor.set(Some((x, y)));
                let follow = self.keyboard_selection.get();
                let _ = self.view.set_local_selection(x, y, x, y, bg, fg, true, follow, SelectionBehavior::Cell);
            }
            return;
        }
        let Some((ax, ay)) = self.anchor.get() else { return };
        let follow = self.keyboard_selection.get();
        let _ = self.view.update_local_selection(ax, ay, x, y, bg, fg, true, follow, SelectionBehavior::Cell);
    }

    /// `insertText(text)`: replaces the selection, if any.
    fn insert_text(&self, el: Element, text: &str) {
        if self.view.has_selection() {
            let _ = self.view.replace_selected_text(text);
            self.clear_renderer_selection();
        } else {
            let _ = self.buffer.insert_text(text);
        }
        el.request_render();
    }

    fn movement(&self, el: Element, select: bool, collapse: Option<bool>, step: impl FnOnce(&Textarea)) -> bool {
        if let Some(start) = collapse {
            if !select && self.collapse_selection_to_edge(el, start) {
                return true;
            }
        }
        self.update_selection_for_movement(el, select, true);
        step(self);
        self.update_selection_for_movement(el, select, false);
        el.request_render();
        true
    }

    fn goto_line_home(&self, el: Element, select: bool) -> bool {
        self.movement(el, select, Some(true), |t| {
            let (row, col) = t.view.get_cursor();
            if col == 0 && row > 0 {
                t.buffer.set_cursor(row - 1, 0);
                let eol = t.buffer.get_eol();
                t.buffer.set_cursor(eol.row, eol.col);
            } else {
                t.buffer.set_cursor(row, 0);
            }
        })
    }

    fn goto_line_end(&self, el: Element, select: bool) -> bool {
        self.movement(el, select, Some(false), |t| {
            let (row, col) = t.view.get_cursor();
            let eol = t.buffer.get_eol();
            if col == eol.col && row + 1 < t.buffer.get_line_count() {
                t.buffer.set_cursor(row + 1, 0);
            } else {
                t.buffer.set_cursor(eol.row, eol.col);
            }
        })
    }

    fn goto_buffer_end(&self, el: Element, select: bool) -> bool {
        self.movement(el, select, Some(false), |t| t.buffer.goto_line(999_999))
    }

    fn delete_word(&self, el: Element, forward: bool) -> bool {
        if self.view.has_selection() {
            self.delete_selected_text(el);
            return true;
        }
        let cursor = self.buffer.get_cursor_position();
        if forward {
            let next = self.buffer.get_next_word_boundary();
            if next.offset > cursor.offset {
                let _ = self.buffer.delete_range(cursor.row, cursor.col, next.row, next.col);
            }
        } else {
            let prev = self.buffer.get_prev_word_boundary();
            if prev.offset < cursor.offset {
                let _ = self.buffer.delete_range(prev.row, prev.col, cursor.row, cursor.col);
            }
        }
        self.clear_renderer_selection();
        el.request_render();
        true
    }

    fn delete_char(&self, el: Element, backward: bool) -> bool {
        if self.view.has_selection() {
            self.delete_selected_text(el);
            return true;
        }
        self.clear_renderer_selection();
        let _ = if backward { self.buffer.delete_char_backward() } else { self.buffer.delete_char() };
        el.request_render();
        true
    }

    fn simple_edit(&self, el: Element, edit: impl FnOnce(&EditBuffer)) -> bool {
        self.clear_renderer_selection();
        edit(&self.buffer);
        el.request_render();
        true
    }

    /// The action's EditBufferRenderable method (`buildActionHandlers`, `editBufferCommandHandlers`).
    fn run(&self, el: Element, action: TextareaAction) -> bool {
        match action {
            A::MoveLeft | A::SelectLeft => {
                self.movement(el, action == A::SelectLeft, Some(true), |t| t.buffer.move_cursor_left())
            }
            A::MoveRight | A::SelectRight => {
                self.movement(el, action == A::SelectRight, Some(false), |t| t.buffer.move_cursor_right())
            }
            A::MoveUp | A::SelectUp => self.movement(el, action == A::SelectUp, None, |t| t.view.move_up_visual()),
            A::MoveDown | A::SelectDown => self.movement(el, action == A::SelectDown, None, |t| t.view.move_down_visual()),
            A::LineHome | A::SelectLineHome => self.goto_line_home(el, action == A::SelectLineHome),
            A::LineEnd | A::SelectLineEnd => self.goto_line_end(el, action == A::SelectLineEnd),
            A::VisualLineHome | A::SelectVisualLineHome => {
                self.movement(el, action == A::SelectVisualLineHome, Some(true), |t| {
                    let sol = t.view.get_visual_sol();
                    t.buffer.set_cursor(sol.logical_row, sol.logical_col);
                })
            }
            A::VisualLineEnd | A::SelectVisualLineEnd => {
                self.movement(el, action == A::SelectVisualLineEnd, Some(false), |t| t.view.goto_visual_line_end())
            }
            A::BufferHome | A::SelectBufferHome => {
                self.movement(el, action == A::SelectBufferHome, Some(true), |t| t.buffer.set_cursor(0, 0))
            }
            A::BufferEnd | A::SelectBufferEnd => self.goto_buffer_end(el, action == A::SelectBufferEnd),
            A::DeleteLine => self.simple_edit(el, |b| {
                let _ = b.delete_line();
            }),
            A::DeleteToLineEnd => {
                let (row, col) = self.view.get_cursor();
                let eol = self.buffer.get_eol();
                if eol.col > col {
                    let _ = self.buffer.delete_range(row, col, eol.row, eol.col);
                }
                el.request_render();
                true
            }
            A::DeleteToLineStart => {
                let (row, col) = self.view.get_cursor();
                if col > 0 {
                    let _ = self.buffer.delete_range(row, 0, row, col);
                } else if row > 0 {
                    let _ = self.buffer.delete_char_backward();
                }
                el.request_render();
                true
            }
            A::Backspace => self.delete_char(el, true),
            A::Delete => self.delete_char(el, false),
            A::Newline => self.simple_edit(el, |b| {
                let _ = b.new_line();
            }),
            A::Undo => self.simple_edit(el, |b| {
                b.undo();
            }),
            A::Redo => self.simple_edit(el, |b| {
                b.redo();
            }),
            A::WordForward | A::SelectWordForward => {
                self.movement(el, action == A::SelectWordForward, Some(false), |t| {
                    let next = t.buffer.get_next_word_boundary();
                    t.buffer.set_cursor_by_offset(next.offset);
                })
            }
            A::WordBackward | A::SelectWordBackward => {
                self.movement(el, action == A::SelectWordBackward, Some(true), |t| {
                    let prev = t.buffer.get_prev_word_boundary();
                    t.buffer.set_cursor_by_offset(prev.offset);
                })
            }
            A::DeleteWordForward => self.delete_word(el, true),
            A::DeleteWordBackward => self.delete_word(el, false),
            A::SelectAll => {
                self.update_selection_for_movement(el, false, true);
                self.buffer.set_cursor(0, 0);
                self.goto_buffer_end(el, true)
            }
            A::Submit => {
                let submit = self.props.borrow().on_submit.clone();
                if let Some(submit) = submit {
                    submit();
                }
                true
            }
        }
    }
}

pub struct TextareaKind {
    state: Rc<Textarea>,
}

impl Drop for TextareaKind {
    fn drop(&mut self) {
        let _ = TEXTAREAS.try_with(|t| {
            if let Ok(mut t) = t.try_borrow_mut() {
                t.retain(|_, state| !Rc::ptr_eq(state, &self.state));
            }
        });
    }
}

impl Kind for TextareaKind {
    fn name(&self) -> &'static str {
        "textarea"
    }
    fn render(&mut self, el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: u32, height: u32) {
        let t = self.state.clone();
        let focused = el.focused();
        if t.synced_focus.get() != Some(focused) {
            t.sync_colors(focused);
        }
        let (bg, cursor_color) = {
            let props = t.props.borrow();
            (if focused { props.focused_background } else { props.background }, props.cursor_color)
        };
        if bg.a() > 0.0 {
            buffer.fill_rect(x, y, width as i32, height as i32, bg);
        }
        t.view.apply_layout(width, height);
        t.view.draw(y, |src| buffer.draw_text_buffer(src, x, y));
        if focused {
            let cursor = t.view.get_visual_cursor();
            crate::renderer::set_cursor_color(cursor_color);
            crate::renderer::set_cursor_position(
                (x + cursor.visual_col as i32).max(0) as u32,
                (y + cursor.visual_row.min(height.saturating_sub(1)) as i32).max(0) as u32,
                true,
            );
        }
    }
    fn on_resize(&mut self, el: Element, width: f32, height: f32) {
        self.state.view.apply_layout(width.max(0.0) as u32, height.max(0.0) as u32);
        el.request_render();
    }
    fn handles_keys(&self) -> bool {
        true
    }
    fn handle_key_press(&mut self, el: Element, key: &mut KeyEvent) -> bool {
        let t = self.state.clone();
        let suspended = t.props.borrow().traits.suspend;
        if !suspended {
            if let Some(action) = key_binding_action(key) {
                let handled = t.run(el, action);
                t.emit(el);
                return handled;
            }
        }
        if key.ctrl || key.meta || key.super_ || key.hyper {
            return false;
        }
        let text = if key.name == "space" {
            " ".to_string()
        } else if !key.sequence.is_empty() && !key.sequence.chars().any(|c| (c as u32) < 0x20 || (0x7f..=0x9f).contains(&(c as u32))) {
            key.sequence.clone()
        } else {
            return false;
        };
        t.insert_text(el, &text);
        t.emit(el);
        true
    }
    fn handles_paste(&self) -> bool {
        true
    }
    fn handle_paste(&mut self, el: Element, event: &mut PasteEvent) {
        let t = self.state.clone();
        t.insert_text(el, &event.text());
        t.emit(el);
    }
    fn destroy(&mut self, el: Element) {
        self.state.props.borrow_mut().traits = EditorTraits::default();
        TEXTAREAS.with(|t| t.borrow_mut().remove(&el));
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Creates a `<textarea>` (focusable, word wrapping, a measured height).
pub fn create_textarea() -> Element {
    let buffer = EditBuffer::create(crate::renderer::width_method(), &text_context());
    let view = Rc::new(EditorView::create(&buffer, 0, 0).expect("editor view"));
    view.set_wrap_mode(WrapMode::Word);
    let white = RGBA::from_values(1.0, 1.0, 1.0, 1.0);
    let measured = view.clone();
    let state = Rc::new(Textarea {
        buffer,
        view,
        props: RefCell::new(Props {
            placeholder: String::new(),
            placeholder_color: RGBA::from_hex("#666666"),
            text_color: white,
            focused_text_color: white,
            background: RGBA::from_values(0.0, 0.0, 0.0, 0.0),
            focused_background: RGBA::from_values(0.0, 0.0, 0.0, 0.0),
            cursor_color: white,
            selection_bg: None,
            selection_fg: None,
            traits: EditorTraits::default(),
            on_content_change: None,
            on_submit: None,
            on_cursor_change: None,
        }),
        anchor: Cell::new(None),
        keyboard_selection: Cell::new(false),
        synced_focus: Cell::new(None),
    });
    state.sync_colors(false);
    let el = tree::create(Box::new(TextareaKind { state: state.clone() }), "textarea", false);
    TEXTAREAS.with(|t| t.borrow_mut().insert(el, state));
    el.set_focusable(true);
    el.set_selectable(true);
    let measure_el = el;
    el.set_measure_func(Some(Box::new(move |width, width_mode, height, _| {
        let mode = match width_mode {
            MeasureMode::Undefined => 0,
            MeasureMode::Exactly => 1,
            MeasureMode::AtMost => 2,
        };
        let absolute = measure_el.with_base(|b| b.position_absolute).unwrap_or(false);
        measured.yoga_measure(width, mode, height, absolute)
    })));
    el
}

impl Element {
    fn with_textarea_props(self, f: impl FnOnce(&mut Props)) {
        let Some(t) = state(self) else { return };
        f(&mut t.props.borrow_mut());
        t.sync_colors(self.focused());
        self.mark_layout_dirty();
        self.request_render();
    }

    /// An edit-buffer renderable (`isEditBufferRenderable`).
    pub fn is_textarea(self) -> bool {
        state(self).is_some()
    }

    pub fn set_placeholder(self, placeholder: &str) {
        self.with_textarea_props(|p| p.placeholder = placeholder.to_string());
    }
    pub fn set_placeholder_color(self, color: RGBA) {
        self.with_textarea_props(|p| p.placeholder_color = color);
    }
    pub fn set_text_color(self, color: RGBA) {
        self.with_textarea_props(|p| p.text_color = color);
    }
    pub fn set_focused_text_color(self, color: RGBA) {
        self.with_textarea_props(|p| p.focused_text_color = color);
    }
    pub fn set_textarea_background(self, color: RGBA) {
        self.with_textarea_props(|p| p.background = color);
    }
    pub fn set_focused_background_color(self, color: RGBA) {
        self.with_textarea_props(|p| p.focused_background = color);
    }
    pub fn set_cursor_color_prop(self, color: RGBA) {
        self.with_textarea_props(|p| p.cursor_color = color);
    }
    pub fn set_selection_bg(self, color: Option<RGBA>) {
        self.with_textarea_props(|p| p.selection_bg = color);
    }
    pub fn set_selection_fg(self, color: Option<RGBA>) {
        self.with_textarea_props(|p| p.selection_fg = color);
    }
    pub fn set_on_content_change(self, f: Option<Rc<dyn Fn()>>) {
        if let Some(t) = state(self) {
            t.props.borrow_mut().on_content_change = f;
        }
    }
    pub fn set_on_submit(self, f: Option<Rc<dyn Fn()>>) {
        if let Some(t) = state(self) {
            t.props.borrow_mut().on_submit = f;
        }
    }
    pub fn set_on_cursor_change(self, f: Option<Rc<dyn Fn()>>) {
        if let Some(t) = state(self) {
            t.props.borrow_mut().on_cursor_change = f;
        }
    }

    /// `traits`.
    pub fn textarea_traits(self) -> EditorTraits {
        state(self).map(|t| t.props.borrow().traits.clone()).unwrap_or_default()
    }

    /// `traits = value`: emits `"traits-changed"` when they differ.
    pub fn set_textarea_traits(self, traits: EditorTraits) {
        let Some(t) = state(self) else { return };
        if t.props.borrow().traits == traits {
            return;
        }
        t.props.borrow_mut().traits = traits;
        self.emit("traits-changed");
    }

    /// Runs one of EditBufferRenderable's edit methods; false when it did not apply (`submit` without a listener
    /// still returns true, as TS's does).
    pub fn textarea_action(self, action: TextareaAction) -> bool {
        let Some(t) = state(self) else { return false };
        let handled = t.run(self, action);
        t.emit(self);
        handled
    }

    /// `insertText(text)`.
    pub fn textarea_insert_text(self, text: &str) {
        let Some(t) = state(self) else { return };
        t.insert_text(self, text);
        t.emit(self);
    }

    /// `clearSelection()`.
    pub fn textarea_clear_selection(self) -> bool {
        state(self).is_some_and(|t| t.clear_selection(self))
    }

    /// `getSelectedText()`.
    pub fn textarea_selected_text(self) -> String {
        state(self).map(|t| t.view.get_selected_text()).unwrap_or_default()
    }

    /// `plainText`.
    pub fn textarea_text(self) -> String {
        state(self).map(|t| t.view.get_text()).unwrap_or_default()
    }

    /// `setText(value)`: the content replaced, the cursor at the end.
    pub fn set_textarea_text(self, value: &str) {
        let Some(t) = state(self) else { return };
        let _ = t.buffer.set_text(value);
        let eof = t.buffer.get_eof();
        t.buffer.set_cursor(eof.row, eof.col);
        let _ = t.buffer.take_events();
        self.mark_layout_dirty();
        self.request_render();
    }

    /// The editor view (`editorView`), for components that need its API.
    pub fn editor_view(self) -> Option<Rc<EditorView>> {
        state(self).map(|t| t.view.clone())
    }

    /// The edit buffer (`editBuffer`).
    pub fn edit_buffer(self) -> Option<EditBuffer> {
        state(self).map(|t| t.buffer.clone())
    }
}
