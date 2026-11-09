//! `<textarea>` (`Textarea.ts` over `EditBufferRenderable.ts`): `opencode-tui-text`'s EditBuffer and EditorView
//! (rope edits, undo, word and visual motions, wrapping, the placeholder, the viewport following the cursor,
//! measuring for Yoga, `drawEditorView`), with the terminal cursor and the content-change, cursor-change and submit
//! callbacks.
//!
//! TODO(port-tui): Textarea.ts's full key binding table (word motions, line deletes, undo/redo bindings,
//! `keyBindings`/`keyAliasMap` props), selection by mouse, extmarks and the syntax style prop. Bound here: printable
//! input, backspace/delete, arrows (visual up/down), home/end, return (submit), shift+return and linefeed
//! (newline), super+a (select all), ctrl+z/ctrl+shift+z (undo/redo), pastes.
use std::any::Any;
use std::rc::Rc;

use opencode_tui_text::{EditBuffer, EditEvent, EditorView, WidthMethod, WrapMode};

use crate::buffer::OptimizedBuffer;
use crate::color::RGBA;
use crate::input::{KeyEvent, PasteEvent};
use crate::layout::MeasureMode;
use crate::renderables::text::text_context;
use crate::tree::{self, Element, Kind};

pub struct TextareaKind {
    pub buffer: EditBuffer,
    pub view: Rc<EditorView>,
    pub placeholder: String,
    pub placeholder_color: RGBA,
    pub text_color: RGBA,
    pub focused_text_color: RGBA,
    pub background: RGBA,
    pub focused_background: RGBA,
    pub cursor_color: RGBA,
    pub on_content_change: Option<Rc<dyn Fn()>>,
    pub on_submit: Option<Rc<dyn Fn()>>,
    pub on_cursor_change: Option<Rc<dyn Fn()>>,
}

impl TextareaKind {
    fn sync_colors(&self, focused: bool) {
        let fg = if focused { self.focused_text_color } else { self.text_color };
        let _ = self.buffer.set_default_fg(Some(fg.0));
        let _ = self.view.set_placeholder_styled_text(&[(self.placeholder.as_str(), Some(self.placeholder_color.0), None, 0)]);
    }

    /// The edit events of the last call, as the renderable's callbacks.
    fn emit(&self, el: Element) {
        let events = self.buffer.take_events();
        if events.is_empty() {
            return;
        }
        el.mark_layout_dirty();
        el.request_render();
        let content = events.iter().any(|e| matches!(e, EditEvent::ContentChanged));
        let cursor = events.iter().any(|e| !matches!(e, EditEvent::ContentChanged));
        if content {
            if let Some(change) = self.on_content_change.clone() {
                change();
            }
        }
        if cursor || content {
            if let Some(change) = self.on_cursor_change.clone() {
                change();
            }
        }
    }

    fn select_all(&self) {
        let length = self.view.get_text().chars().count() as u32;
        if length > 0 {
            self.view.set_selection(0, length, None, None);
        }
    }
}

impl Kind for TextareaKind {
    fn name(&self) -> &'static str {
        "textarea"
    }
    fn render(&mut self, el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: u32, height: u32) {
        let focused = el.focused();
        let bg = if focused { self.focused_background } else { self.background };
        if bg.a() > 0.0 {
            buffer.fill_rect(x, y, width as i32, height as i32, bg);
        }
        self.view.apply_layout(width, height);
        self.view.draw(y, |src| buffer.draw_text_buffer(src, x, y));
        if focused {
            let cursor = self.view.get_visual_cursor();
            let viewport = self.view.get_viewport();
            let row = cursor.visual_row.saturating_sub(viewport.y);
            let col = cursor.visual_col.saturating_sub(viewport.x);
            crate::renderer::set_cursor_color(self.cursor_color);
            crate::renderer::set_cursor_position(
                (x + col as i32).max(0) as u32,
                (y + row.min(height.saturating_sub(1)) as i32).max(0) as u32,
                true,
            );
        }
    }
    fn on_resize(&mut self, el: Element, width: f32, height: f32) {
        self.view.apply_layout(width.max(0.0) as u32, height.max(0.0) as u32);
        el.request_render();
    }
    fn handles_keys(&self) -> bool {
        true
    }
    fn handle_key_press(&mut self, el: Element, key: &mut KeyEvent) -> bool {
        let modified = key.ctrl || key.meta || key.super_ || key.hyper;
        let has_selection = self.view.has_selection();
        let handled = match key.name.as_str() {
            "return" if !key.shift && !modified => {
                if let Some(submit) = self.on_submit.clone() {
                    submit();
                }
                return true;
            }
            "return" | "linefeed" if !modified || key.shift => {
                if has_selection {
                    let _ = self.view.delete_selected_text();
                }
                let _ = self.buffer.new_line();
                true
            }
            "backspace" if !modified || key.shift => {
                if has_selection {
                    let _ = self.view.delete_selected_text();
                } else {
                    let _ = self.buffer.delete_char_backward();
                }
                true
            }
            "delete" if !modified => {
                if has_selection {
                    let _ = self.view.delete_selected_text();
                } else {
                    let _ = self.buffer.delete_char();
                }
                true
            }
            "a" if key.super_ && !key.ctrl => {
                self.select_all();
                el.request_render();
                return true;
            }
            "z" if key.ctrl && !key.shift => {
                self.buffer.undo();
                true
            }
            "z" if key.ctrl && key.shift => {
                self.buffer.redo();
                true
            }
            "left" if !modified => {
                self.view.reset_selection();
                self.buffer.move_cursor_left();
                true
            }
            "right" if !modified => {
                self.view.reset_selection();
                self.buffer.move_cursor_right();
                true
            }
            "up" if !modified => {
                self.view.reset_selection();
                self.view.move_up_visual();
                true
            }
            "down" if !modified => {
                self.view.reset_selection();
                self.view.move_down_visual();
                true
            }
            "home" if !modified => {
                self.view.reset_selection();
                let cursor = self.buffer.get_cursor_position();
                self.buffer.set_cursor(cursor.row, 0);
                true
            }
            "end" if !modified => {
                self.view.reset_selection();
                let eol = self.buffer.get_eol();
                self.buffer.set_cursor(eol.row, eol.col);
                true
            }
            _ => {
                let printable = !modified && !key.sequence.is_empty() && key.sequence.chars().all(|c| !c.is_control());
                if !printable {
                    return false;
                }
                if has_selection {
                    let _ = self.view.delete_selected_text();
                }
                let _ = self.buffer.insert_text(&key.sequence.clone());
                true
            }
        };
        self.emit(el);
        handled
    }
    fn handles_paste(&self) -> bool {
        true
    }
    fn handle_paste(&mut self, el: Element, event: &mut PasteEvent) {
        let text = event.text();
        if self.view.has_selection() {
            let _ = self.view.delete_selected_text();
        }
        let _ = self.buffer.insert_text(&text);
        self.emit(el);
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
    let buffer = EditBuffer::create(WidthMethod::Unicode, &text_context());
    let view = Rc::new(EditorView::create(&buffer, 0, 0).expect("editor view"));
    view.set_wrap_mode(WrapMode::Word);
    let white = RGBA::from_values(1.0, 1.0, 1.0, 1.0);
    let measured = view.clone();
    let kind = TextareaKind {
        buffer,
        view,
        placeholder: String::new(),
        placeholder_color: RGBA::from_hex("#666666"),
        text_color: white,
        focused_text_color: white,
        background: RGBA::from_values(0.0, 0.0, 0.0, 0.0),
        focused_background: RGBA::from_values(0.0, 0.0, 0.0, 0.0),
        cursor_color: white,
        on_content_change: None,
        on_submit: None,
        on_cursor_change: None,
    };
    kind.sync_colors(false);
    let el = tree::create(Box::new(kind), "textarea", false);
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
    fn with_textarea<R>(self, f: impl FnOnce(&mut TextareaKind) -> R) -> Option<R> {
        let focused = self.focused();
        let r = self.with_kind::<TextareaKind, _>(|k| {
            let r = f(k);
            k.sync_colors(focused);
            r
        });
        self.mark_layout_dirty();
        r
    }

    pub fn set_placeholder(self, placeholder: &str) {
        self.with_textarea(|k| k.placeholder = placeholder.to_string());
    }
    pub fn set_placeholder_color(self, color: RGBA) {
        self.with_textarea(|k| k.placeholder_color = color);
    }
    pub fn set_text_color(self, color: RGBA) {
        self.with_textarea(|k| k.text_color = color);
    }
    pub fn set_focused_text_color(self, color: RGBA) {
        self.with_textarea(|k| k.focused_text_color = color);
    }
    pub fn set_textarea_background(self, color: RGBA) {
        self.with_textarea(|k| k.background = color);
    }
    pub fn set_focused_background_color(self, color: RGBA) {
        self.with_textarea(|k| k.focused_background = color);
    }
    pub fn set_cursor_color_prop(self, color: RGBA) {
        self.with_textarea(|k| k.cursor_color = color);
    }
    pub fn set_on_content_change(self, f: Option<Rc<dyn Fn()>>) {
        self.with_kind::<TextareaKind, _>(|k| k.on_content_change = f);
    }
    pub fn set_on_submit(self, f: Option<Rc<dyn Fn()>>) {
        self.with_kind::<TextareaKind, _>(|k| k.on_submit = f);
    }
    pub fn set_on_cursor_change(self, f: Option<Rc<dyn Fn()>>) {
        self.with_kind::<TextareaKind, _>(|k| k.on_cursor_change = f);
    }

    /// `plainText`.
    pub fn textarea_text(self) -> String {
        self.with_kind::<TextareaKind, _>(|k| k.view.get_text()).unwrap_or_default()
    }

    /// `setText(value)`: the content replaced, the cursor at the end.
    pub fn set_textarea_text(self, value: &str) {
        self.with_textarea(|k| {
            let _ = k.buffer.set_text(value);
            let eof = k.buffer.get_eof();
            k.buffer.set_cursor(eof.row, eof.col);
            let _ = k.buffer.take_events();
        });
        self.request_render();
    }

    /// The editor view (`editorView`), for components that need its API.
    pub fn editor_view(self) -> Option<Rc<EditorView>> {
        self.with_kind::<TextareaKind, _>(|k| k.view.clone())
    }

    /// The edit buffer (`editBuffer`).
    pub fn edit_buffer(self) -> Option<EditBuffer> {
        self.with_kind::<TextareaKind, _>(|k| k.buffer.clone())
    }
}
