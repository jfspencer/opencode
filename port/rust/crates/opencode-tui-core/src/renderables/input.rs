//! `<input>`: `Input.ts`, a single-line `<textarea>`: height 1, no wrapping, newlines stripped from what it takes,
//! `maxLength` (1000) enforced, `return`/`kpenter`/`linefeed` submitting (the `enter` event), and the `input`
//! event (`onInput`) with the value after each change.
//!
//! Built on the textarea renderable: its own key and paste handlers run before the textarea's (as Input's
//! overridden `keyBindings`, `newLine` and `insertText` do), and content changes reach `onInput`.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::input::KeyEvent;
use crate::layout::Value;
use crate::tree::Element;

struct InputState {
    /// The editor view, which reads while the node's kind is borrowed (from inside its own callbacks).
    view: Option<Rc<opencode_tui_text::EditorView>>,
    max_length: usize,
    on_input: Option<Rc<dyn Fn(String)>>,
    on_enter: Option<Rc<dyn Fn(String)>>,
}

thread_local! {
    static INPUTS: RefCell<HashMap<Element, InputState>> = RefCell::new(HashMap::new());
}

/// `plainText`.
fn value_of(el: Element) -> String {
    let view = INPUTS.with(|i| i.borrow().get(&el).and_then(|s| s.view.clone()));
    match view {
        Some(view) => view.get_text(),
        None => el.textarea_text(),
    }
}

fn emit_input(el: Element) {
    let listener = INPUTS.with(|i| i.borrow().get(&el).and_then(|s| s.on_input.clone()));
    if let Some(listener) = listener {
        listener(value_of(el));
    }
}

/// Input's `keyBindings` over Textarea's: `return`, `kpenter` and `linefeed` submit; a newline is never inserted.
fn submits(key: &KeyEvent) -> bool {
    let plain = !key.ctrl && !key.shift && !key.meta && !key.super_;
    matches!(key.name.as_str(), "return" | "kpenter" | "linefeed") && (plain || (key.meta && !key.ctrl && !key.shift))
}

fn inserts_newline(key: &KeyEvent) -> bool {
    matches!(key.name.as_str(), "return" | "kpenter" | "linefeed")
}

/// Creates an `<input>`.
pub fn create_input() -> Element {
    let el = crate::renderables::textarea::create_textarea();
    el.set_height(Some(Value::Point(1.0)));
    let view = el.editor_view();
    if let Some(view) = &view {
        view.set_wrap_mode(opencode_tui_text::WrapMode::None);
    }
    INPUTS.with(|i| i.borrow_mut().insert(el, InputState { view, max_length: 1000, on_input: None, on_enter: None }));
    el.on(
        "destroyed",
        Rc::new(move || {
            INPUTS.with(|i| i.borrow_mut().remove(&el));
        }),
    );
    el.set_on_key_down(Some(Rc::new(move |key: &mut KeyEvent| {
        if submits(key) {
            key.prevent_default();
            let listener = INPUTS.with(|i| i.borrow().get(&el).and_then(|s| s.on_enter.clone()));
            if let Some(listener) = listener {
                listener(value_of(el));
            }
            return;
        }
        if inserts_newline(key) {
            key.prevent_default();
            return;
        }
        // insertText: nothing past maxLength.
        let modified = key.ctrl || key.meta || key.super_ || key.hyper;
        let printable = !modified && !key.sequence.is_empty() && key.sequence.chars().all(|c| !c.is_control());
        if printable {
            let max = INPUTS.with(|i| i.borrow().get(&el).map(|s| s.max_length)).unwrap_or(1000);
            if value_of(el).encode_utf16().count() >= max {
                key.prevent_default();
            }
        }
    })));
    el.set_on_paste(Some(Rc::new(move |event: &mut crate::input::PasteEvent| {
        let text: String = event.text().chars().filter(|c| *c != '\n' && *c != '\r').collect();
        let max = INPUTS.with(|i| i.borrow().get(&el).map(|s| s.max_length)).unwrap_or(1000);
        let remaining = max.saturating_sub(value_of(el).encode_utf16().count());
        let kept: String = text.chars().scan(0usize, |units, c| {
            *units += c.len_utf16();
            (*units <= remaining).then_some(c)
        }).collect();
        if kept.is_empty() {
            event.prevent_default();
            return;
        }
        event.bytes = kept.into_bytes();
    })));
    el.set_on_content_change(Some(Rc::new(move || emit_input(el))));
    el
}

impl Element {
    /// Whether this is an `<input>` (`instanceof InputRenderable`).
    pub fn is_input(self) -> bool {
        INPUTS.with(|i| i.borrow().contains_key(&self))
    }

    /// `value`.
    pub fn input_value(self) -> String {
        value_of(self)
    }

    /// `value = v`: newlines stripped, at most `maxLength`; the cursor at the end; `input` when it changed.
    pub fn set_input_value(self, value: &str) {
        let max = INPUTS.with(|i| i.borrow().get(&self).map(|s| s.max_length)).unwrap_or(1000);
        let units: Vec<u16> = value.encode_utf16().take(max).collect();
        let value: String = String::from_utf16_lossy(&units).chars().filter(|c| *c != '\n' && *c != '\r').collect();
        if self.textarea_text() == value {
            return;
        }
        self.set_textarea_text(&value);
        emit_input(self);
    }

    /// `onInput`.
    pub fn set_on_input(self, listener: Option<Rc<dyn Fn(String)>>) {
        INPUTS.with(|i| {
            if let Some(state) = i.borrow_mut().get_mut(&self) {
                state.on_input = listener;
            }
        });
    }

    /// `onSubmit` / the `enter` event.
    pub fn set_on_enter(self, listener: Option<Rc<dyn Fn(String)>>) {
        INPUTS.with(|i| {
            if let Some(state) = i.borrow_mut().get_mut(&self) {
                state.on_enter = listener;
            }
        });
    }

    /// `maxLength`.
    pub fn set_max_length(self, max: usize) {
        INPUTS.with(|i| {
            if let Some(state) = i.borrow_mut().get_mut(&self) {
                state.max_length = max;
            }
        });
        let current = self.textarea_text();
        if current.encode_utf16().count() > max {
            let units: Vec<u16> = current.encode_utf16().take(max).collect();
            self.set_textarea_text(&String::from_utf16_lossy(&units));
        }
    }
}
