//! `registerManagedTextareaLayer` (`@opentui/keymap/addons/opentui`, edit-buffer-bindings.ts) as the Keymap
//! Provider registers it (context/keymap.tsx): the edit-buffer commands (`input.move.left`, ...) acting on the
//! focused textarea, the focused textarea's own key table suspended (`traits.suspend`; printable keys still
//! insert), and the bindings: the config's `input.*` keybinds, then Textarea.ts's default table.
//!
//! The two binding lists are two layers here, the defaults the older one: a newer layer of the same priority
//! answers first, as the config bindings come first in TS's single layer. Both are the keymap's oldest layers, so
//! every other layer takes precedence, as in TS.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use opencode_tui_core::renderables::textarea::{EditorTraits, Textarea, default_key_bindings};
use opencode_tui_core::renderer;
use opencode_tui_core::tree::{self, Element};

use crate::keymap::{Command, Enabled, Layer, create_layer};

/// The edit-buffer actions and their command names (`defaultEditBufferCommandNames`).
const COMMANDS: [(&str, &str); 36] = [
    ("move-left", "input.move.left"),
    ("move-right", "input.move.right"),
    ("move-up", "input.move.up"),
    ("move-down", "input.move.down"),
    ("select-left", "input.select.left"),
    ("select-right", "input.select.right"),
    ("select-up", "input.select.up"),
    ("select-down", "input.select.down"),
    ("line-home", "input.line.home"),
    ("line-end", "input.line.end"),
    ("select-line-home", "input.select.line.home"),
    ("select-line-end", "input.select.line.end"),
    ("visual-line-home", "input.visual.line.home"),
    ("visual-line-end", "input.visual.line.end"),
    ("select-visual-line-home", "input.select.visual.line.home"),
    ("select-visual-line-end", "input.select.visual.line.end"),
    ("buffer-home", "input.buffer.home"),
    ("buffer-end", "input.buffer.end"),
    ("select-buffer-home", "input.select.buffer.home"),
    ("select-buffer-end", "input.select.buffer.end"),
    ("delete-line", "input.delete.line"),
    ("delete-to-line-end", "input.delete.to.line.end"),
    ("delete-to-line-start", "input.delete.to.line.start"),
    ("backspace", "input.backspace"),
    ("delete", "input.delete"),
    ("newline", "input.newline"),
    ("undo", "input.undo"),
    ("redo", "input.redo"),
    ("word-forward", "input.word.forward"),
    ("word-backward", "input.word.backward"),
    ("select-word-forward", "input.select.word.forward"),
    ("select-word-backward", "input.select.word.backward"),
    ("delete-word-forward", "input.delete.word.forward"),
    ("delete-word-backward", "input.delete.word.backward"),
    ("select-all", "input.select.all"),
    ("submit", "input.submit"),
];

/// The focused managed textarea (`renderer.currentFocusedEditor`, a textarea that is not an input).
fn focused_editor() -> Option<Textarea> {
    let focused = tree::focused()?;
    if focused.kind_name() != "textarea" || focused.is_destroyed() {
        return None;
    }
    focused.textarea()
}

/// Runs an edit-buffer action on the focused editor (`withFocusedEditor`).
fn run(action: &'static str) -> bool {
    focused_editor().is_some_and(|editor| editor.run_action(action))
}

fn set_suspend(editor: &Textarea, suspend: bool) {
    let traits = editor.traits();
    if traits.suspend != suspend {
        editor.set_traits(EditorTraits { suspend, ..traits });
    }
}

/// `registerManagedTextareaLayer(keymap, renderer, { enabled, bindings })`, in the Provider's owner.
pub fn install() {
    // `registerTextareaMappingSuspension`: the focused editor's own bindings are suspended, the previous state
    // restored when it loses focus.
    let previous: Rc<RefCell<HashMap<Element, bool>>> = Rc::new(RefCell::new(HashMap::new()));
    let suspended: Rc<RefCell<Option<Textarea>>> = Rc::new(RefCell::new(None));
    let suspend_focused = {
        let previous = previous.clone();
        let suspended = suspended.clone();
        move || {
            let current = focused_editor();
            let last = suspended.borrow_mut().take();
            if let Some(last) = last.filter(|l| Some(l.el) != current.as_ref().map(|c| c.el)) {
                if let Some(state) = previous.borrow_mut().remove(&last.el)
                    && !last.el.is_destroyed()
                {
                    set_suspend(&last, state);
                }
            }
            let Some(editor) = current else { return };
            previous.borrow_mut().entry(editor.el).or_insert_with(|| editor.traits().suspend);
            set_suspend(&editor, true);
            *suspended.borrow_mut() = Some(editor);
        }
    };
    suspend_focused();
    let token = renderer::on_focused_renderable(suspend_focused);
    opencode_tui_core::reactive::on_cleanup(move || {
        renderer::off_focused_renderable(token);
        if let Some(editor) = suspended.borrow_mut().take()
            && let Some(state) = previous.borrow_mut().remove(&editor.el)
            && !editor.el.is_destroyed()
        {
            set_suspend(&editor, state);
        }
    });

    let enabled = || Some(Enabled::Fn(Rc::new(|| focused_editor().is_some())));
    // The default table (`createDefaultTextareaBindings`), the older layer.
    create_layer(move || Layer {
        mode: Some("global"),
        enabled: enabled(),
        commands: default_key_bindings()
            .into_iter()
            .map(|binding| {
                let mut key: Vec<&str> = Vec::new();
                if binding.ctrl {
                    key.push("ctrl");
                }
                if binding.shift {
                    key.push("shift");
                }
                if binding.meta {
                    key.push("meta");
                }
                if binding.super_ {
                    key.push("super");
                }
                key.push(&binding.name);
                let action = binding.action;
                Command::inline(&key.join("+"), move || run(action)).group("Text Editing")
            })
            .collect(),
        ..Layer::default()
    });
    // The edit-buffer commands with the config's `input.*` bindings.
    create_layer(move || Layer {
        mode: Some("global"),
        enabled: enabled(),
        commands: COMMANDS
            .iter()
            .map(|(action, name)| {
                let action: &'static str = action;
                Command::with_input(name, move |_| run(action)).group("Text Editing")
            })
            .collect(),
        ..Layer::default()
    });
}
