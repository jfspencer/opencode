//! `<textarea>` (`Textarea.ts` over `EditBufferRenderable.ts`): an editable, wrapping text with a placeholder, the
//! terminal cursor, and content-change and submit callbacks.
//!
//! TODO(port-tui): opencode-tui-text. The edit buffer and editor view (its milestone 2) are not in dev yet: this
//! keeps the text in a `String`, edits it at a char cursor (insert, backspace, delete, left/right/home/end,
//! select-all then delete, return to submit, shift+return or a linefeed for a newline) and lays it out and draws it
//! with the scene Text node, so wrapping, widths and measuring are the text crate's. Undo, word motions, vertical
//! motion, extmarks, mouse selection and syntax highlighting arrive with the editor view.
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use opencode_tui_text::{SceneText, SceneTextOptions, StyledChunk, TextAlign, WidthMethod, WrapMode};

use crate::buffer::OptimizedBuffer;
use crate::color::RGBA;
use crate::input::{KeyEvent, PasteEvent};
use crate::layout::MeasureMode;
use crate::renderables::text::text_context;
use crate::tree::{self, Element, Kind};

pub struct TextareaKind {
    pub scene: Rc<RefCell<SceneText>>,
    pub text: String,
    /// The cursor, in chars.
    pub cursor: usize,
    /// Everything is selected (`selectAll`): the next edit replaces it.
    pub all_selected: bool,
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

fn options(fg: RGBA) -> SceneTextOptions {
    SceneTextOptions {
        fg: fg.0,
        bg: RGBA::from_values(0.0, 0.0, 0.0, 0.0).0,
        attributes: 0,
        wrap_mode: WrapMode::Word,
        text_align: TextAlign::Left,
        truncate: false,
        scroll_x: 0.0,
        scroll_y: 0.0,
        first_line_offset: 0,
        tab_indicator: 0,
        tab_indicator_color: None,
    }
}

impl TextareaKind {
    /// Lays the shown text (the content, or the placeholder while empty) into the scene node.
    fn sync(&self, focused: bool) {
        let mut scene = self.scene.borrow_mut();
        if self.text.is_empty() {
            let _ = scene.set_styled_text(&[StyledChunk {
                text: &self.placeholder,
                fg: Some(self.placeholder_color.0),
                bg: None,
                attributes: 0,
                link: None,
            }]);
        } else {
            let _ = scene.set_text(&self.text);
        }
        let fg = if focused { self.focused_text_color } else { self.text_color };
        let _ = scene.set_options(options(fg), || [None, None]);
    }

    fn byte_at(&self, chars: usize) -> usize {
        self.text.char_indices().nth(chars).map_or(self.text.len(), |(i, _)| i)
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn insert(&mut self, value: &str) {
        if self.all_selected {
            self.text.clear();
            self.cursor = 0;
            self.all_selected = false;
        }
        let at = self.byte_at(self.cursor);
        self.text.insert_str(at, value);
        self.cursor += value.chars().count();
    }

    fn changed(&self, el: Element) {
        let focused = el.focused();
        self.sync(focused);
        el.mark_layout_dirty();
        el.request_render();
    }

    /// The cursor's cell (column, row) in the laid-out text.
    fn cursor_cell(&self, width: u32) -> (u32, u32) {
        let before: String = self.text.chars().take(self.cursor).collect();
        let mut probe = SceneText::new(WidthMethod::Unicode, &text_context());
        let _ = probe.set_text(&before);
        let _ = probe.set_options(options(self.text_color), || [None, None]);
        probe.apply_layout(width.max(1), u32::MAX / 2);
        let info = probe.line_info(0, u32::MAX);
        let row = info.line_width_cols.len().saturating_sub(1) as u32;
        let col = info.line_width_cols.last().copied().unwrap_or(0);
        if before.ends_with('\n') { (0, row + 1) } else { (col, row) }
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
        {
            let mut scene = self.scene.borrow_mut();
            if scene.needs_prepare(width, height) {
                scene.apply_layout(width, height);
            }
            scene.draw(y, |src| buffer.draw_text_buffer(src, x, y));
        }
        if focused {
            let (col, row) = if self.text.is_empty() { (0, 0) } else { self.cursor_cell(width) };
            crate::renderer::set_cursor_color(self.cursor_color);
            crate::renderer::set_cursor_position((x + col as i32).max(0) as u32, (y + row.min(height.saturating_sub(1)) as i32).max(0) as u32, true);
        }
    }
    fn on_resize(&mut self, el: Element, width: f32, height: f32) {
        self.scene.borrow_mut().apply_layout(width.max(0.0) as u32, height.max(0.0) as u32);
        el.request_render();
    }
    fn handles_keys(&self) -> bool {
        true
    }
    fn handle_key_press(&mut self, el: Element, key: &mut KeyEvent) -> bool {
        let name = key.name.clone();
        let modified = key.ctrl || key.meta || key.super_ || key.hyper;
        let mut content = false;
        match name.as_str() {
            "return" if !key.shift && !modified => {
                if let Some(submit) = self.on_submit.clone() {
                    submit();
                }
                return true;
            }
            "return" | "linefeed" if !modified || key.shift => {
                self.insert("\n");
                content = true;
            }
            "backspace" if !modified || key.shift => {
                if self.all_selected {
                    self.text.clear();
                    self.cursor = 0;
                    self.all_selected = false;
                    content = true;
                } else if self.cursor > 0 {
                    let end = self.byte_at(self.cursor);
                    let start = self.byte_at(self.cursor - 1);
                    self.text.replace_range(start..end, "");
                    self.cursor -= 1;
                    content = true;
                }
            }
            "delete" if !modified => {
                if self.all_selected {
                    self.text.clear();
                    self.cursor = 0;
                    self.all_selected = false;
                    content = true;
                } else if self.cursor < self.len() {
                    let start = self.byte_at(self.cursor);
                    let end = self.byte_at(self.cursor + 1);
                    self.text.replace_range(start..end, "");
                    content = true;
                }
            }
            "a" if key.super_ && !key.ctrl => {
                self.all_selected = !self.text.is_empty();
                return true;
            }
            "left" if !modified => {
                self.all_selected = false;
                self.cursor = self.cursor.saturating_sub(1);
            }
            "right" if !modified => {
                self.all_selected = false;
                self.cursor = (self.cursor + 1).min(self.len());
            }
            "home" if !modified => {
                self.all_selected = false;
                self.cursor = 0;
            }
            "end" if !modified => {
                self.all_selected = false;
                self.cursor = self.len();
            }
            _ => {
                let printable = !modified
                    && !key.sequence.is_empty()
                    && key.sequence.chars().all(|c| !c.is_control());
                if !printable {
                    return false;
                }
                let sequence = key.sequence.clone();
                self.insert(&sequence);
                content = true;
            }
        }
        self.changed(el);
        if content {
            if let Some(change) = self.on_content_change.clone() {
                change();
            }
        }
        if let Some(cursor) = self.on_cursor_change.clone() {
            cursor();
        }
        true
    }
    fn handles_paste(&self) -> bool {
        true
    }
    fn handle_paste(&mut self, el: Element, event: &mut PasteEvent) {
        let text = event.text();
        self.insert(&text);
        self.changed(el);
        if let Some(change) = self.on_content_change.clone() {
            change();
        }
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
    let scene = Rc::new(RefCell::new(SceneText::new(WidthMethod::Unicode, &text_context())));
    let measured = scene.clone();
    let white = RGBA::from_values(1.0, 1.0, 1.0, 1.0);
    let kind = TextareaKind {
        scene,
        text: String::new(),
        cursor: 0,
        all_selected: false,
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
    kind.sync(false);
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
        measured.borrow_mut().yoga_measure(width, mode, height, absolute)
    })));
    el
}

impl Element {
    fn with_textarea<R>(self, f: impl FnOnce(&mut TextareaKind) -> R) -> Option<R> {
        let focused = self.focused();
        let r = self.with_kind::<TextareaKind, _>(|k| {
            let r = f(k);
            k.sync(focused);
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
        self.with_kind::<TextareaKind, _>(|k| k.text.clone()).unwrap_or_default()
    }

    /// `setText(value)` (the cursor goes to the end).
    pub fn set_textarea_text(self, value: &str) {
        self.with_textarea(|k| {
            k.text = value.to_string();
            k.cursor = k.len();
            k.all_selected = false;
        });
        self.request_render();
    }
}
