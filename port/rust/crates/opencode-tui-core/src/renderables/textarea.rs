//! `<textarea>` (`Textarea.ts` over `EditBufferRenderable.ts`): `opencode-tui-text`'s EditBuffer and EditorView
//! (rope edits, undo, word and visual motions, wrapping, the placeholder, the viewport following the cursor,
//! measuring for Yoga, `drawEditorView`), with the terminal cursor and the content-change, cursor-change and submit
//! callbacks.
//!
//! [`Textarea`] is the renderable's API, with the TS members' names (`insert_text`, `delete_range`,
//! `set_selection`, `cursor_offset`, `logical_cursor`, `visual_cursor`, `scroll_y`, `extmarks`, the motions with
//! their `select` option, ...); `Element::textarea()` returns it, and works while the node's kind is borrowed (from
//! inside its own callbacks). Keys follow Textarea.ts's default binding table, the `keyBindings` prop merged over it
//! ([`Textarea::set_key_bindings`]), unless `traits.suspend` is set (the keymap's managed textarea layer then owns
//! the bindings; printable keys still insert).
//!
//! Edit events reach the listeners as TS's do: in a microtask ([`crate::renderer::next_tick`]) after the edit, through
//! [`EditBuffer::dispatch_events`] (which also runs the extmarks controller's highlight update).
//!
//! Keyboard selections (the select-* actions) stand in for the renderer's global Selection the TS starts and
//! updates (`_ctx.startSelection`/`updateSelection`): the textarea keeps the anchor and focus it would hold and
//! applies them to its editor view as `onSelectionChanged` does.
//!
//! TODO(port-tui): selection by mouse (the renderer's global selection), its auto-scroll, `keyAliasMap`, cursor
//! styles, tab indicators.
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use opencode_tui_text::{
    EditBuffer, EditEvent, EditorView, Extmarks, LogicalCursor, SelectionBehavior, SyntaxStyle, Viewport, VisualCursor,
    WidthMethod, WrapMode,
};

use crate::buffer::OptimizedBuffer;
use crate::color::RGBA;
use crate::input::{KeyEvent, PasteEvent};
use crate::layout::MeasureMode;
use crate::renderables::text::text_context;
use crate::tree::{self, Element, Kind, MouseEvent, MouseEventType, ScrollDirection};

/// A binding of the textarea's key table (`KeyBinding<TextareaAction>`).
#[derive(Clone, Debug, PartialEq)]
pub struct KeyBinding {
    pub name: String,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_: bool,
    pub action: &'static str,
}

/// `EditorTraits`: what the focused editor tells the keymap (`suspend` hands its bindings over) and status lines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorTraits {
    pub suspend: bool,
    pub status: Option<String>,
    pub capture: Option<Vec<&'static str>>,
}

/// `defaultTextareaKeyBindings` (Textarea.ts): (name, ctrl, shift, meta, super, action).
const DEFAULT_BINDINGS: &[(&str, bool, bool, bool, bool, &str)] = &[
    ("left", false, false, false, false, "move-left"),
    ("right", false, false, false, false, "move-right"),
    ("up", false, false, false, false, "move-up"),
    ("down", false, false, false, false, "move-down"),
    ("left", false, true, false, false, "select-left"),
    ("right", false, true, false, false, "select-right"),
    ("up", false, true, false, false, "select-up"),
    ("down", false, true, false, false, "select-down"),
    ("home", false, false, false, false, "buffer-home"),
    ("end", false, false, false, false, "buffer-end"),
    ("home", false, true, false, false, "select-buffer-home"),
    ("end", false, true, false, false, "select-buffer-end"),
    ("a", true, false, false, false, "line-home"),
    ("e", true, false, false, false, "line-end"),
    ("a", true, true, false, false, "select-line-home"),
    ("e", true, true, false, false, "select-line-end"),
    ("a", false, false, true, false, "visual-line-home"),
    ("e", false, false, true, false, "visual-line-end"),
    ("a", false, true, true, false, "select-visual-line-home"),
    ("e", false, true, true, false, "select-visual-line-end"),
    ("f", true, false, false, false, "move-right"),
    ("b", true, false, false, false, "move-left"),
    ("w", true, false, false, false, "delete-word-backward"),
    ("backspace", true, false, false, false, "delete-word-backward"),
    ("d", false, false, true, false, "delete-word-forward"),
    ("delete", false, false, true, false, "delete-word-forward"),
    ("delete", true, false, false, false, "delete-word-forward"),
    ("d", true, true, false, false, "delete-line"),
    ("k", true, false, false, false, "delete-to-line-end"),
    ("u", true, false, false, false, "delete-to-line-start"),
    ("backspace", false, false, false, false, "backspace"),
    ("backspace", false, true, false, false, "backspace"),
    ("d", true, false, false, false, "delete"),
    ("delete", false, false, false, false, "delete"),
    ("delete", false, true, false, false, "delete"),
    ("return", false, false, false, false, "newline"),
    ("kpenter", false, false, false, false, "newline"),
    ("linefeed", false, false, false, false, "newline"),
    ("return", false, false, true, false, "submit"),
    ("kpenter", false, false, true, false, "submit"),
    ("-", true, false, false, false, "undo"),
    (".", true, false, false, false, "redo"),
    ("z", false, false, false, true, "undo"),
    ("z", false, true, false, true, "redo"),
    ("f", false, false, true, false, "word-forward"),
    ("b", false, false, true, false, "word-backward"),
    ("right", false, false, true, false, "word-forward"),
    ("left", false, false, true, false, "word-backward"),
    ("right", true, false, false, false, "word-forward"),
    ("left", true, false, false, false, "word-backward"),
    ("f", false, true, true, false, "select-word-forward"),
    ("b", false, true, true, false, "select-word-backward"),
    ("right", false, true, true, false, "select-word-forward"),
    ("left", false, true, true, false, "select-word-backward"),
    ("backspace", false, false, true, false, "delete-word-backward"),
    ("left", false, false, false, true, "visual-line-home"),
    ("right", false, false, false, true, "visual-line-end"),
    ("up", false, false, false, true, "buffer-home"),
    ("down", false, false, false, true, "buffer-end"),
    ("left", false, true, false, true, "select-visual-line-home"),
    ("right", false, true, false, true, "select-visual-line-end"),
    ("up", false, true, false, true, "select-buffer-home"),
    ("down", false, true, false, true, "select-buffer-end"),
    ("a", false, false, false, true, "select-all"),
];

/// `defaultKeyAliases` (keybinding.internal.ts).
const KEY_ALIASES: &[(&str, &str)] = &[
    ("enter", "return"),
    ("esc", "escape"),
    ("kp0", "0"),
    ("kp1", "1"),
    ("kp2", "2"),
    ("kp3", "3"),
    ("kp4", "4"),
    ("kp5", "5"),
    ("kp6", "6"),
    ("kp7", "7"),
    ("kp8", "8"),
    ("kp9", "9"),
    ("kpdecimal", "."),
    ("kpdivide", "/"),
    ("kpmultiply", "*"),
    ("kpminus", "-"),
    ("kpplus", "+"),
    ("kpenter", "enter"),
    ("kpequal", "="),
    ("kpseparator", ","),
    ("kpleft", "left"),
    ("kpright", "right"),
    ("kpup", "up"),
    ("kpdown", "down"),
    ("kppageup", "pageup"),
    ("kppagedown", "pagedown"),
    ("kphome", "home"),
    ("kpend", "end"),
    ("kpinsert", "insert"),
    ("kpdelete", "delete"),
];

/// `getKeyBindingKey`.
fn binding_key(name: &str, ctrl: bool, shift: bool, meta: bool, super_: bool) -> String {
    format!("{name}:{}:{}:{}:{}", ctrl as u8, shift as u8, meta as u8, super_ as u8)
}

/// `buildKeyBindingsMap(mergeKeyBindings(defaultTextareaKeyBindings, custom), defaultKeyAliases)`.
fn build_bindings(custom: &[KeyBinding]) -> HashMap<String, &'static str> {
    let mut merged: Vec<(String, KeyBinding)> = Vec::new();
    let defaults = DEFAULT_BINDINGS.iter().map(|(name, ctrl, shift, meta, super_, action)| KeyBinding {
        name: name.to_string(),
        ctrl: *ctrl,
        shift: *shift,
        meta: *meta,
        super_: *super_,
        action,
    });
    for binding in defaults.chain(custom.iter().cloned()) {
        let key = binding_key(&binding.name, binding.ctrl, binding.shift, binding.meta, binding.super_);
        match merged.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = binding,
            None => merged.push((key, binding)),
        }
    }
    let mut map = HashMap::new();
    for (key, binding) in &merged {
        map.insert(key.clone(), binding.action);
    }
    for (_, binding) in &merged {
        if let Some((_, alias)) = KEY_ALIASES.iter().find(|(name, _)| *name == binding.name) {
            map.insert(binding_key(alias, binding.ctrl, binding.shift, binding.meta, binding.super_), binding.action);
        }
    }
    map
}

/// `getKeyBindingKeys`: the parsed name, then Kitty's base-layout key.
fn lookup_keys(key: &KeyEvent) -> Vec<String> {
    let mut names = vec![key.name.clone()];
    if let Some(code) = key.base_code.filter(|c| *c > 32 && *c != 127) {
        let single_non_ascii = key.name.chars().count() == 1 && !key.name.is_ascii();
        if (single_non_ascii || (48..=57).contains(&code))
            && let Some(c) = char::from_u32(code)
        {
            let name = if c.is_ascii_uppercase() { c.to_ascii_lowercase().to_string() } else { c.to_string() };
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names.into_iter().map(|name| binding_key(&name, key.ctrl, key.shift, key.meta, key.super_)).collect()
}

/// The renderer's selection while it touches this textarea (keyboard selections only).
#[derive(Clone, Copy)]
struct GlobalSelection {
    anchor: (i32, i32),
    focus: (i32, i32),
    behavior: SelectionBehavior,
}

struct State {
    placeholder: Option<String>,
    placeholder_color: RGBA,
    text_color: RGBA,
    focused_text_color: RGBA,
    background: RGBA,
    focused_background: RGBA,
    cursor_color: RGBA,
    show_cursor: bool,
    selection_bg: Option<RGBA>,
    selection_fg: Option<RGBA>,
    wrap_mode: WrapMode,
    key_bindings: Vec<KeyBinding>,
    bindings: HashMap<String, &'static str>,
    traits: EditorTraits,
    keyboard_selection_active: bool,
    selection: Option<GlobalSelection>,
    /// The focus state the colors were last applied for.
    colors_focused: bool,
    on_content_change: Option<Rc<dyn Fn()>>,
    on_submit: Option<Rc<dyn Fn()>>,
    on_cursor_change: Option<Rc<dyn Fn()>>,
    on_traits_change: Option<Rc<dyn Fn()>>,
}

/// The textarea's API (`TextareaRenderable`), a cheap handle.
#[derive(Clone)]
pub struct Textarea {
    pub el: Element,
    pub buffer: EditBuffer,
    pub view: Rc<EditorView>,
    state: Rc<RefCell<State>>,
    events_scheduled: Rc<Cell<bool>>,
}

thread_local! {
    static TEXTAREAS: RefCell<HashMap<Element, Textarea>> = RefCell::new(HashMap::new());
}

pub struct TextareaKind {
    ta: Textarea,
}

impl Textarea {
    fn state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.state.borrow_mut())
    }

    /// `updateColors` and the placeholder's color: the effective colors for the focus state.
    fn update_colors(&self) {
        let focused = self.el.focused();
        let (fg, bg, placeholder, placeholder_color) = self.state(|s| {
            s.colors_focused = focused;
            let fg = if focused { s.focused_text_color } else { s.text_color };
            let bg = if focused { s.focused_background } else { s.background };
            (fg, bg, s.placeholder.clone(), s.placeholder_color)
        });
        let _ = self.buffer.set_default_fg(Some(fg.0));
        let _ = self.buffer.set_default_bg(Some(bg.0));
        match placeholder {
            Some(text) => {
                let _ = self.view.set_placeholder_styled_text(&[(text.as_str(), Some(placeholder_color.0), None, 0)]);
            }
            None => {
                let _ = self.view.set_placeholder_styled_text(&[]);
            }
        }
        self.el.request_render();
    }

    /// Queues delivery of the buffer's edit events (TS delivers them in a microtask).
    fn schedule_events(&self) {
        if self.events_scheduled.replace(true) {
            return;
        }
        let ta = self.clone();
        crate::renderer::next_tick(move || ta.deliver_events());
    }

    /// The `cursor-changed` and `content-changed` listeners of `setupEventListeners`.
    fn deliver_events(&self) {
        self.events_scheduled.set(false);
        if self.el.is_destroyed() {
            let _ = self.buffer.take_events();
            return;
        }
        let ta = self.clone();
        self.buffer.dispatch_events(move |event| match event {
            EditEvent::CursorChanged => {
                let listener = ta.state(|s| s.on_cursor_change.clone());
                if let Some(listener) = listener {
                    listener();
                }
            }
            EditEvent::ContentChanged => {
                ta.el.mark_layout_dirty();
                ta.el.request_render();
                let listener = ta.state(|s| s.on_content_change.clone());
                if let Some(listener) = listener {
                    listener();
                }
            }
            EditEvent::HistoryCursorChanged => {}
        });
    }

    /// Requests a frame and queues the edit events an operation produced.
    fn changed(&self) {
        self.el.request_render();
        self.schedule_events();
    }

    // -- properties ------------------------------------------------------------------------------------------

    /// `plainText`.
    pub fn plain_text(&self) -> String {
        self.buffer.get_text()
    }
    /// `logicalCursor`.
    pub fn logical_cursor(&self) -> LogicalCursor {
        self.buffer.get_cursor_position()
    }
    /// `visualCursor`: the cursor in the viewport's rows and columns.
    pub fn visual_cursor(&self) -> VisualCursor {
        self.view.get_visual_cursor()
    }
    /// `cursorOffset`.
    pub fn cursor_offset(&self) -> u32 {
        self.buffer.get_cursor_position().offset
    }
    /// `cursorOffset = offset`.
    pub fn set_cursor_offset(&self, offset: u32) {
        self.clear_selection();
        self.view.set_cursor_by_offset(offset);
        self.changed();
    }
    /// `scrollY`: the viewport's first row.
    pub fn scroll_y(&self) -> u32 {
        self.view.get_viewport().y
    }
    pub fn viewport(&self) -> Viewport {
        self.view.get_viewport()
    }
    pub fn line_count(&self) -> u32 {
        self.buffer.get_line_count()
    }
    pub fn virtual_line_count(&self) -> u32 {
        self.view.get_virtual_line_count()
    }
    pub fn total_virtual_line_count(&self) -> u32 {
        self.view.get_total_virtual_line_count()
    }
    /// `extmarks` (the editor view's controller, installed on first use).
    pub fn extmarks(&self) -> Extmarks {
        self.view.extmarks()
    }
    pub fn width_method(&self) -> WidthMethod {
        self.buffer.width_method()
    }
    pub fn traits(&self) -> EditorTraits {
        self.state(|s| s.traits.clone())
    }
    /// `traits = value` (unchanged traits notify nobody).
    pub fn set_traits(&self, traits: EditorTraits) {
        let listener = self.state(|s| {
            if s.traits == traits {
                return None;
            }
            s.traits = traits;
            Some(s.on_traits_change.clone())
        });
        if let Some(Some(listener)) = listener {
            listener();
        }
    }
    pub fn set_on_traits_change(&self, f: Option<Rc<dyn Fn()>>) {
        self.state(|s| s.on_traits_change = f);
    }
    pub fn set_syntax_style(&self, style: Option<&SyntaxStyle>) {
        self.buffer.set_syntax_style(style);
        self.el.request_render();
    }
    pub fn set_wrap_mode(&self, mode: WrapMode) {
        self.state(|s| s.wrap_mode = mode);
        self.view.set_wrap_mode(mode);
        self.el.mark_layout_dirty();
        self.el.request_render();
    }
    pub fn set_show_cursor(&self, show: bool) {
        self.state(|s| s.show_cursor = show);
        self.el.request_render();
    }
    pub fn set_selection_colors(&self, bg: Option<RGBA>, fg: Option<RGBA>) {
        self.state(|s| {
            s.selection_bg = bg;
            s.selection_fg = fg;
        });
        self.view.set_selection_colors(bg.map(|c| c.0), fg.map(|c| c.0));
        self.el.request_render();
    }
    /// The `keyBindings` prop: merged over the default table.
    pub fn set_key_bindings(&self, bindings: Vec<KeyBinding>) {
        self.state(|s| {
            s.bindings = build_bindings(&bindings);
            s.key_bindings = bindings;
        });
    }

    // -- selection -------------------------------------------------------------------------------------------

    pub fn has_selection(&self) -> bool {
        self.view.has_selection()
    }
    pub fn get_selection(&self) -> Option<(u32, u32)> {
        self.view.get_selection()
    }
    pub fn get_selected_text(&self) -> String {
        self.view.get_selected_text()
    }

    /// The renderer's `clearSelection()`: the textarea it touched hears `onSelectionChanged(null)`.
    fn clear_global_selection(&self) {
        if self.state(|s| s.selection.take()).is_some() {
            self.state(|s| s.keyboard_selection_active = false);
            self.view.reset_local_selection();
            self.el.request_render();
        }
    }

    /// `onSelectionChanged(selection)` for the renderer's (keyboard) selection.
    fn apply_global_selection(&self, start: bool) {
        let Some((selection, bg, fg, follow)) =
            self.state(|s| s.selection.map(|sel| (sel, s.selection_bg, s.selection_fg, s.keyboard_selection_active)))
        else {
            return;
        };
        let (ax, ay) = selection.anchor;
        let (fx, fy) = selection.focus;
        let bg = bg.map(|c| c.0);
        let fg = fg.map(|c| c.0);
        let changed = if start {
            self.view.set_local_selection(ax, ay, fx, fy, bg, fg, true, follow, selection.behavior)
        } else {
            self.view.update_local_selection(ax, ay, fx, fy, bg, fg, true, follow, selection.behavior)
        };
        // Not dragging: the auto-scroll state resets.
        self.state(|s| s.keyboard_selection_active = false);
        if changed.unwrap_or(false) {
            self.el.request_render();
        }
    }

    /// `clearSelection()`; true when there was one.
    pub fn clear_selection(&self) -> bool {
        let had = self.has_selection();
        self.view.reset_local_selection();
        self.clear_global_selection();
        if had {
            self.el.request_render();
        }
        had
    }

    /// `setSelection(start, end)`.
    pub fn set_selection(&self, start: u32, end: u32) {
        self.view.reset_local_selection();
        self.clear_global_selection();
        let (bg, fg) = self.state(|s| (s.selection_bg, s.selection_fg));
        self.view.set_selection(start, end, bg.map(|c| c.0), fg.map(|c| c.0));
        self.el.request_render();
    }

    /// `setSelectionInclusive(start, end)`.
    pub fn set_selection_inclusive(&self, start: u32, end: u32) {
        self.view.reset_local_selection();
        self.clear_global_selection();
        let (bg, fg) = self.state(|s| (s.selection_bg, s.selection_fg));
        self.view.set_selection_inclusive(start, end, bg.map(|c| c.0), fg.map(|c| c.0));
        self.el.request_render();
    }

    /// `deleteSelection()`.
    pub fn delete_selection(&self) -> bool {
        if !self.has_selection() {
            return false;
        }
        self.delete_selected_text();
        true
    }

    fn delete_selected_text(&self) {
        let _ = self.view.delete_selected_text();
        self.clear_global_selection();
        self.changed();
    }

    /// `updateSelectionForMovement(shiftPressed, isBeforeMovement)`.
    fn update_selection_for_movement(&self, shift: bool, before: bool) {
        if !shift {
            self.state(|s| s.keyboard_selection_active = false);
            self.clear_selection();
            return;
        }
        if !self.el.with_base(|b| b.selectable).unwrap_or(false) {
            return;
        }
        self.state(|s| s.keyboard_selection_active = true);
        let cursor = self.view.get_visual_cursor();
        let point = (cursor.visual_col as i32, cursor.visual_row as i32);
        if before {
            let active = self.state(|s| s.selection.is_some());
            if !active || !self.has_selection() {
                self.state(|s| s.selection = Some(GlobalSelection { anchor: point, focus: point, behavior: SelectionBehavior::Cell }));
                self.apply_global_selection(true);
            } else if self.state(|s| s.selection.is_some_and(|sel| sel.behavior != SelectionBehavior::Cell))
                && self.view.convert_selection_to_cell()
            {
                self.state(|s| {
                    if let Some(sel) = s.selection.as_mut() {
                        sel.behavior = SelectionBehavior::Cell;
                    }
                });
            }
            return;
        }
        self.state(|s| {
            if let Some(sel) = s.selection.as_mut() {
                sel.focus = point;
            }
        });
        // The update runs with the keyboard selection still active (it is reset after the editor view saw it).
        self.state(|s| s.keyboard_selection_active = true);
        self.apply_global_selection(false);
    }

    /// Horizontal movement collapses a selection to an edge without taking another step.
    fn collapse_selection_to_edge(&self, start: bool) -> bool {
        let Some((from, to)) = self.get_selection() else { return false };
        self.buffer.set_cursor_by_offset(if start { from } else { to });
        self.clear_selection();
        self.changed();
        true
    }

    fn motion(&self, select: bool, collapse: Option<bool>, f: impl FnOnce()) -> bool {
        if !select && let Some(start) = collapse && self.collapse_selection_to_edge(start) {
            return true;
        }
        self.update_selection_for_movement(select, true);
        f();
        self.update_selection_for_movement(select, false);
        self.changed();
        true
    }

    // -- editing ---------------------------------------------------------------------------------------------

    /// `insertText(text)`: replaces the selection, if any.
    pub fn insert_text(&self, text: &str) {
        if self.has_selection() {
            let _ = self.view.replace_selected_text(text);
            self.clear_global_selection();
        } else {
            let _ = self.buffer.insert_text(text);
        }
        self.changed();
    }
    /// `insertChar(char)`.
    pub fn insert_char(&self, text: &str) {
        if self.has_selection() {
            let _ = self.view.replace_selected_text(text);
            self.clear_global_selection();
        } else {
            let _ = self.buffer.insert_char(text);
        }
        self.changed();
    }
    pub fn delete_char(&self) -> bool {
        if self.has_selection() {
            self.delete_selected_text();
            return true;
        }
        self.clear_global_selection();
        let _ = self.buffer.delete_char();
        self.changed();
        true
    }
    pub fn delete_char_backward(&self) -> bool {
        if self.has_selection() {
            self.delete_selected_text();
            return true;
        }
        self.clear_global_selection();
        let _ = self.buffer.delete_char_backward();
        self.changed();
        true
    }
    pub fn new_line(&self) -> bool {
        self.clear_global_selection();
        let _ = self.buffer.new_line();
        self.changed();
        true
    }
    pub fn delete_line(&self) -> bool {
        self.clear_global_selection();
        let _ = self.buffer.delete_line();
        self.changed();
        true
    }
    pub fn move_cursor_left(&self, select: bool) -> bool {
        self.motion(select, Some(true), || self.buffer.move_cursor_left())
    }
    pub fn move_cursor_right(&self, select: bool) -> bool {
        self.motion(select, Some(false), || self.buffer.move_cursor_right())
    }
    pub fn move_cursor_up(&self, select: bool) -> bool {
        self.motion(select, None, || self.view.move_up_visual())
    }
    pub fn move_cursor_down(&self, select: bool) -> bool {
        self.motion(select, None, || self.view.move_down_visual())
    }
    pub fn goto_line(&self, line: u32) {
        self.clear_selection();
        self.buffer.goto_line(line);
        self.changed();
    }
    /// `setCursor(row, col)`.
    pub fn set_cursor(&self, row: u32, col: u32) {
        self.clear_selection();
        self.buffer.set_cursor(row, col);
        self.changed();
    }
    pub fn goto_line_home(&self, select: bool) -> bool {
        self.motion(select, Some(true), || {
            let (row, col) = self.view.get_cursor();
            if col == 0 && row > 0 {
                self.buffer.set_cursor(row - 1, 0);
                let eol = self.buffer.get_eol();
                self.buffer.set_cursor(eol.row, eol.col);
            } else {
                self.buffer.set_cursor(row, 0);
            }
        })
    }
    pub fn goto_line_end(&self, select: bool) -> bool {
        self.motion(select, Some(false), || {
            let (row, col) = self.view.get_cursor();
            let eol = self.buffer.get_eol();
            if col == eol.col && row + 1 < self.buffer.get_line_count() {
                self.buffer.set_cursor(row + 1, 0);
            } else {
                self.buffer.set_cursor(eol.row, eol.col);
            }
        })
    }
    pub fn goto_visual_line_home(&self, select: bool) -> bool {
        self.motion(select, Some(true), || {
            let sol = self.view.get_visual_sol();
            self.buffer.set_cursor(sol.logical_row, sol.logical_col);
        })
    }
    pub fn goto_visual_line_end(&self, select: bool) -> bool {
        self.motion(select, Some(false), || self.view.goto_visual_line_end())
    }
    pub fn goto_buffer_home(&self, select: bool) -> bool {
        self.motion(select, Some(true), || self.buffer.set_cursor(0, 0))
    }
    pub fn goto_buffer_end(&self, select: bool) -> bool {
        self.motion(select, Some(false), || self.buffer.goto_line(999_999))
    }
    pub fn select_all(&self) -> bool {
        self.update_selection_for_movement(false, true);
        self.buffer.set_cursor(0, 0);
        self.goto_buffer_end(true)
    }
    pub fn delete_to_line_end(&self) -> bool {
        let (row, col) = self.view.get_cursor();
        let eol = self.buffer.get_eol();
        if eol.col > col {
            let _ = self.buffer.delete_range(row, col, eol.row, eol.col);
        }
        self.changed();
        true
    }
    pub fn delete_to_line_start(&self) -> bool {
        let (row, col) = self.view.get_cursor();
        if col > 0 {
            let _ = self.buffer.delete_range(row, 0, row, col);
        } else if row > 0 {
            let _ = self.buffer.delete_char_backward();
        }
        self.changed();
        true
    }
    pub fn undo(&self) -> bool {
        self.clear_global_selection();
        self.buffer.undo();
        self.changed();
        true
    }
    pub fn redo(&self) -> bool {
        self.clear_global_selection();
        self.buffer.redo();
        self.changed();
        true
    }
    pub fn move_word_forward(&self, select: bool) -> bool {
        self.motion(select, Some(false), || {
            let next = self.buffer.get_next_word_boundary();
            self.buffer.set_cursor_by_offset(next.offset);
        })
    }
    pub fn move_word_backward(&self, select: bool) -> bool {
        self.motion(select, Some(true), || {
            let prev = self.buffer.get_prev_word_boundary();
            self.buffer.set_cursor_by_offset(prev.offset);
        })
    }
    pub fn delete_word_forward(&self) -> bool {
        if self.has_selection() {
            self.delete_selected_text();
            return true;
        }
        let cursor = self.buffer.get_cursor_position();
        let next = self.buffer.get_next_word_boundary();
        if next.offset > cursor.offset {
            let _ = self.buffer.delete_range(cursor.row, cursor.col, next.row, next.col);
        }
        self.clear_global_selection();
        self.changed();
        true
    }
    pub fn delete_word_backward(&self) -> bool {
        if self.has_selection() {
            self.delete_selected_text();
            return true;
        }
        let cursor = self.buffer.get_cursor_position();
        let prev = self.buffer.get_prev_word_boundary();
        if prev.offset < cursor.offset {
            let _ = self.buffer.delete_range(prev.row, prev.col, cursor.row, cursor.col);
        }
        self.clear_global_selection();
        self.changed();
        true
    }
    /// `setText(text)`: replaces the text and resets the history.
    pub fn set_text(&self, text: &str) {
        let _ = self.buffer.set_text(text);
        self.changed();
    }
    /// `replaceText(text)`: replaces the text as an undo step.
    pub fn replace_text(&self, text: &str) {
        let _ = self.buffer.replace_text(text);
        self.changed();
    }
    /// `clear()`: empties the text and its highlights.
    pub fn clear(&self) {
        self.buffer.clear();
        self.buffer.clear_all_highlights();
        self.changed();
    }
    pub fn delete_range(&self, start_line: u32, start_col: u32, end_line: u32, end_col: u32) {
        let _ = self.buffer.delete_range(start_line, start_col, end_line, end_col);
        self.changed();
    }
    pub fn get_text_range(&self, start: u32, end: u32) -> String {
        self.buffer.get_text_range(start, end)
    }
    pub fn get_text_range_by_coords(&self, start_row: u32, start_col: u32, end_row: u32, end_col: u32) -> String {
        self.buffer.get_text_range_by_coords(start_row, start_col, end_row, end_col)
    }
    /// `submit()`.
    pub fn submit(&self) -> bool {
        let listener = self.state(|s| s.on_submit.clone());
        if let Some(listener) = listener {
            listener();
        }
        true
    }
    /// `invalidateIntrinsicSize()`.
    pub fn invalidate_intrinsic_size(&self) {
        self.el.mark_layout_dirty();
    }

    /// Runs a textarea action by name (the action handlers of Textarea.ts).
    pub fn run_action(&self, action: &str) -> bool {
        match action {
            "move-left" => self.move_cursor_left(false),
            "move-right" => self.move_cursor_right(false),
            "move-up" => self.move_cursor_up(false),
            "move-down" => self.move_cursor_down(false),
            "select-left" => self.move_cursor_left(true),
            "select-right" => self.move_cursor_right(true),
            "select-up" => self.move_cursor_up(true),
            "select-down" => self.move_cursor_down(true),
            "line-home" => self.goto_line_home(false),
            "line-end" => self.goto_line_end(false),
            "select-line-home" => self.goto_line_home(true),
            "select-line-end" => self.goto_line_end(true),
            "visual-line-home" => self.goto_visual_line_home(false),
            "visual-line-end" => self.goto_visual_line_end(false),
            "select-visual-line-home" => self.goto_visual_line_home(true),
            "select-visual-line-end" => self.goto_visual_line_end(true),
            "select-buffer-home" => self.goto_buffer_home(true),
            "select-buffer-end" => self.goto_buffer_end(true),
            "buffer-home" => self.goto_buffer_home(false),
            "buffer-end" => self.goto_buffer_end(false),
            "delete-line" => self.delete_line(),
            "delete-to-line-end" => self.delete_to_line_end(),
            "delete-to-line-start" => self.delete_to_line_start(),
            "backspace" => self.delete_char_backward(),
            "delete" => self.delete_char(),
            "newline" => self.new_line(),
            "undo" => self.undo(),
            "redo" => self.redo(),
            "word-forward" => self.move_word_forward(false),
            "word-backward" => self.move_word_backward(false),
            "select-word-forward" => self.move_word_forward(true),
            "select-word-backward" => self.move_word_backward(true),
            "delete-word-forward" => self.delete_word_forward(),
            "delete-word-backward" => self.delete_word_backward(),
            "select-all" => self.select_all(),
            "submit" => self.submit(),
            _ => false,
        }
    }

    /// `handleKeyPress(key)`.
    pub fn handle_key_press(&self, key: &KeyEvent) -> bool {
        let action = self.state(|s| {
            if s.traits.suspend {
                return None;
            }
            lookup_keys(key).iter().find_map(|k| s.bindings.get(k).copied())
        });
        if let Some(action) = action {
            return self.run_action(action);
        }
        if !key.ctrl && !key.meta && !key.super_ && !key.hyper {
            if key.name == "space" {
                self.insert_text(" ");
                return true;
            }
            // A key sequence with a control character is a key that has no binding, not text.
            let control = key.sequence.chars().any(|c| (c as u32) < 0x20 || (0x7f..=0x9f).contains(&(c as u32)));
            if !key.sequence.is_empty() && !control {
                self.insert_text(&key.sequence.clone());
                return true;
            }
        }
        false
    }

    /// `handlePaste(event)`.
    pub fn handle_paste(&self, event: &PasteEvent) {
        let text = String::from_utf8_lossy(&event.bytes).into_owned();
        self.insert_text(&strip_ansi_sequences(&text));
    }

    /// `handleScroll(event)`.
    fn handle_scroll(&self, event: &MouseEvent) -> bool {
        let Some(scroll) = event.scroll else { return false };
        let delta = scroll.delta.max(0.0) as u32;
        let viewport = self.view.get_viewport();
        let before = (viewport.x, viewport.y);
        match scroll.direction {
            ScrollDirection::Up => {
                let y = viewport.y.saturating_sub(delta);
                let _ = self.view.set_viewport(viewport.x, y, viewport.width, viewport.height, true);
            }
            ScrollDirection::Down => {
                let total = self.view.get_total_virtual_line_count();
                let max = total.saturating_sub(viewport.height);
                let y = (viewport.y + delta).min(max);
                let _ = self.view.set_viewport(viewport.x, y, viewport.width, viewport.height, true);
            }
            _ => {}
        }
        if self.state(|s| s.wrap_mode == WrapMode::None) {
            match scroll.direction {
                ScrollDirection::Left => {
                    let x = viewport.x.saturating_sub(delta);
                    let _ = self.view.set_viewport(x, viewport.y, viewport.width, viewport.height, true);
                }
                ScrollDirection::Right => {
                    let widest = self.view.get_logical_line_info().line_width_cols_max;
                    let max = widest.saturating_sub(viewport.width);
                    let x = (viewport.x + delta).min(viewport.x.max(max));
                    let _ = self.view.set_viewport(x, viewport.y, viewport.width, viewport.height, true);
                }
                _ => {}
            }
        }
        self.el.request_render();
        let after = self.view.get_viewport();
        (after.x, after.y) != before
    }
}

/// `stripAnsiSequences` (lib/paste.ts).
pub fn strip_ansi_sequences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' && c != '\u{9b}' {
            out.push(c);
            continue;
        }
        // CSI: ESC [ params intermediates final; OSC: ESC ] ... BEL | ESC \; other: ESC and one char.
        let csi = c == '\u{9b}' || chars.peek() == Some(&'[');
        if csi {
            if c == '\x1b' {
                chars.next();
            }
            for next in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&next) {
                    break;
                }
            }
            continue;
        }
        match chars.peek() {
            Some(']') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\x07' {
                        break;
                    }
                    if next == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

impl Kind for TextareaKind {
    fn name(&self) -> &'static str {
        "textarea"
    }
    fn render(&mut self, el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: u32, height: u32) {
        let ta = self.ta.clone();
        let focused = el.focused();
        if ta.state(|s| s.colors_focused != focused) {
            ta.update_colors();
        }
        let (bg, show_cursor, cursor_color) = ta.state(|s| {
            (if focused { s.focused_background } else { s.background }, s.show_cursor, s.cursor_color)
        });
        if bg.a() > 0.0 {
            buffer.fill_rect(x, y, width as i32, height as i32, bg);
        }
        ta.view.apply_layout(width, height);
        ta.view.draw(y, |src| buffer.draw_text_buffer(src, x, y));
        if focused && show_cursor {
            let cursor = ta.view.get_visual_cursor();
            crate::renderer::set_cursor_color(cursor_color);
            crate::renderer::set_cursor_position(
                (x + cursor.visual_col as i32).max(0) as u32,
                (y + cursor.visual_row.min(height.saturating_sub(1)) as i32).max(0) as u32,
                true,
            );
        }
    }
    fn on_resize(&mut self, el: Element, width: f32, height: f32) {
        self.ta.view.apply_layout(width.max(0.0) as u32, height.max(0.0) as u32);
        el.request_render();
    }
    fn handles_keys(&self) -> bool {
        true
    }
    fn handle_key_press(&mut self, _el: Element, key: &mut KeyEvent) -> bool {
        self.ta.clone().handle_key_press(key)
    }
    fn handles_paste(&self) -> bool {
        true
    }
    fn handle_paste(&mut self, _el: Element, event: &mut PasteEvent) {
        self.ta.clone().handle_paste(event);
    }
    fn on_mouse_event(&mut self, _el: Element, event: &mut MouseEvent) {
        if event.kind == MouseEventType::Scroll && self.ta.clone().handle_scroll(event) {
            event.stop_propagation();
        }
    }
    fn destroy(&mut self, el: Element) {
        TEXTAREAS.with(|t| t.borrow_mut().remove(&el));
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Creates a `<textarea>` (focusable, selectable, word wrapping, a measured height).
pub fn create_textarea() -> Element {
    let buffer = EditBuffer::create(WidthMethod::Unicode, &text_context());
    let view = Rc::new(EditorView::create(&buffer, 80, 24).expect("editor view"));
    view.set_wrap_mode(WrapMode::Word);
    let _ = view.set_scroll_margin(0.2);
    let white = RGBA::from_values(1.0, 1.0, 1.0, 1.0);
    let transparent = RGBA::from_values(0.0, 0.0, 0.0, 0.0);
    let state = State {
        placeholder: None,
        placeholder_color: RGBA::from_hex("#666666"),
        text_color: white,
        focused_text_color: white,
        background: transparent,
        focused_background: transparent,
        cursor_color: white,
        show_cursor: true,
        selection_bg: None,
        selection_fg: None,
        wrap_mode: WrapMode::Word,
        key_bindings: Vec::new(),
        bindings: build_bindings(&[]),
        traits: EditorTraits::default(),
        keyboard_selection_active: false,
        selection: None,
        colors_focused: false,
        on_content_change: None,
        on_submit: None,
        on_cursor_change: None,
        on_traits_change: None,
    };
    let _ = buffer.set_default_attributes(Some(0));
    let measured = view.clone();
    let placeholder_ta = Textarea {
        el: Element { index: u32::MAX, generation: 0 },
        buffer: buffer.clone(),
        view: view.clone(),
        state: Rc::new(RefCell::new(state)),
        events_scheduled: Rc::new(Cell::new(false)),
    };
    let el = tree::create(Box::new(TextareaKind { ta: placeholder_ta.clone() }), "textarea", false);
    let ta = Textarea { el, ..placeholder_ta };
    el.with_kind::<TextareaKind, _>(|k| k.ta = ta.clone());
    TEXTAREAS.with(|t| t.borrow_mut().insert(el, ta.clone()));
    ta.update_colors();
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
    /// The textarea's API, for a `<textarea>` (also from inside its own callbacks).
    pub fn textarea(self) -> Option<Textarea> {
        TEXTAREAS.with(|t| t.borrow().get(&self).cloned())
    }

    fn with_textarea<R>(self, f: impl FnOnce(&Textarea) -> R) -> Option<R> {
        self.textarea().map(|ta| f(&ta))
    }

    fn with_textarea_colors(self, f: impl FnOnce(&mut State)) {
        self.with_textarea(|ta| {
            ta.state(f);
            ta.update_colors();
        });
    }

    /// `placeholder = text` (`None`: no placeholder).
    pub fn set_placeholder_text(self, placeholder: Option<&str>) {
        self.with_textarea_colors(|s| s.placeholder = placeholder.map(str::to_string));
    }
    pub fn set_placeholder(self, placeholder: &str) {
        self.set_placeholder_text(Some(placeholder));
    }
    pub fn set_placeholder_color(self, color: RGBA) {
        self.with_textarea_colors(|s| s.placeholder_color = color);
    }
    pub fn set_text_color(self, color: RGBA) {
        self.with_textarea_colors(|s| s.text_color = color);
    }
    pub fn set_focused_text_color(self, color: RGBA) {
        self.with_textarea_colors(|s| s.focused_text_color = color);
    }
    pub fn set_textarea_background(self, color: RGBA) {
        self.with_textarea_colors(|s| s.background = color);
    }
    pub fn set_focused_background_color(self, color: RGBA) {
        self.with_textarea_colors(|s| s.focused_background = color);
    }
    pub fn set_cursor_color_prop(self, color: RGBA) {
        self.with_textarea(|ta| {
            ta.state(|s| s.cursor_color = color);
            self.request_render();
        });
    }
    pub fn set_on_content_change(self, f: Option<Rc<dyn Fn()>>) {
        self.with_textarea(|ta| ta.state(|s| s.on_content_change = f));
    }
    pub fn set_on_submit(self, f: Option<Rc<dyn Fn()>>) {
        self.with_textarea(|ta| ta.state(|s| s.on_submit = f));
    }
    pub fn set_on_cursor_change(self, f: Option<Rc<dyn Fn()>>) {
        self.with_textarea(|ta| ta.state(|s| s.on_cursor_change = f));
    }

    /// `plainText`.
    pub fn textarea_text(self) -> String {
        self.with_textarea(|ta| ta.plain_text()).unwrap_or_default()
    }

    /// `setText(value)` then `gotoBufferEnd()`.
    pub fn set_textarea_text(self, value: &str) {
        self.with_textarea(|ta| {
            ta.set_text(value);
            ta.goto_buffer_end(false);
        });
    }

    /// The editor view (`editorView`), for components that need its API.
    pub fn editor_view(self) -> Option<Rc<EditorView>> {
        self.with_textarea(|ta| ta.view.clone())
    }

    /// The edit buffer (`editBuffer`).
    pub fn edit_buffer(self) -> Option<EditBuffer> {
        self.with_textarea(|ta| ta.buffer.clone())
    }
}
