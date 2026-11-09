//! `@opentui/keymap/addons/opentui`'s edit-buffer bindings (`edit-buffer-bindings.ts`): the `input.*` commands
//! running EditBufferRenderable's edit methods on the renderer's focused editor, the suspension of a focused
//! textarea's own key bindings, and `registerManagedTextareaLayer`, which installs both with the default textarea
//! bindings.
//!
//! TODO(port-tui): `EditBufferCommandOptions` (renamed commands, descriptions, category, group and fine groups);
//! the TUI passes none.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use opencode_tui_input::keymap as km;

use crate::renderables::textarea::{TextareaAction, TextareaKeyBinding, DEFAULT_TEXTAREA_KEY_BINDINGS};
use crate::renderer;
use crate::tree::Element;

const DEFAULT_EDIT_BUFFER_METADATA_GROUP: &str = "Text Editing";
const EDIT_BUFFER_COMMANDS_RESOURCE: &str = "keymap:edit-buffer-commands";
const TEXTAREA_MAPPING_SUSPENSION_RESOURCE: &str = "keymap:textarea-mapping-suspension";

/// `editBufferCommandNames`.
pub fn edit_buffer_command_name(action: TextareaAction) -> &'static str {
    use TextareaAction as A;
    match action {
        A::MoveLeft => "input.move.left",
        A::MoveRight => "input.move.right",
        A::MoveUp => "input.move.up",
        A::MoveDown => "input.move.down",
        A::SelectLeft => "input.select.left",
        A::SelectRight => "input.select.right",
        A::SelectUp => "input.select.up",
        A::SelectDown => "input.select.down",
        A::LineHome => "input.line.home",
        A::LineEnd => "input.line.end",
        A::SelectLineHome => "input.select.line.home",
        A::SelectLineEnd => "input.select.line.end",
        A::VisualLineHome => "input.visual.line.home",
        A::VisualLineEnd => "input.visual.line.end",
        A::SelectVisualLineHome => "input.select.visual.line.home",
        A::SelectVisualLineEnd => "input.select.visual.line.end",
        A::BufferHome => "input.buffer.home",
        A::BufferEnd => "input.buffer.end",
        A::SelectBufferHome => "input.select.buffer.home",
        A::SelectBufferEnd => "input.select.buffer.end",
        A::DeleteLine => "input.delete.line",
        A::DeleteToLineEnd => "input.delete.to.line.end",
        A::DeleteToLineStart => "input.delete.to.line.start",
        A::Backspace => "input.backspace",
        A::Delete => "input.delete",
        A::Newline => "input.newline",
        A::Undo => "input.undo",
        A::Redo => "input.redo",
        A::WordForward => "input.word.forward",
        A::WordBackward => "input.word.backward",
        A::SelectWordForward => "input.select.word.forward",
        A::SelectWordBackward => "input.select.word.backward",
        A::DeleteWordForward => "input.delete.word.forward",
        A::DeleteWordBackward => "input.delete.word.backward",
        A::SelectAll => "input.select.all",
        A::Submit => "input.submit",
    }
}

/// `editBufferCommandDescriptions`.
pub fn edit_buffer_command_description(action: TextareaAction) -> &'static str {
    use TextareaAction as A;
    match action {
        A::MoveLeft => "Cursor left",
        A::MoveRight => "Cursor right",
        A::MoveUp => "Cursor up",
        A::MoveDown => "Cursor down",
        A::SelectLeft => "Select left",
        A::SelectRight => "Select right",
        A::SelectUp => "Select up",
        A::SelectDown => "Select down",
        A::LineHome => "Line start",
        A::LineEnd => "Line end",
        A::SelectLineHome => "Select to line start",
        A::SelectLineEnd => "Select to line end",
        A::VisualLineHome => "Visual line start",
        A::VisualLineEnd => "Visual line end",
        A::SelectVisualLineHome => "Select to visual line start",
        A::SelectVisualLineEnd => "Select to visual line end",
        A::BufferHome => "Buffer start",
        A::BufferEnd => "Buffer end",
        A::SelectBufferHome => "Select to buffer start",
        A::SelectBufferEnd => "Select to buffer end",
        A::DeleteLine => "Delete line",
        A::DeleteToLineEnd => "Delete to line end",
        A::DeleteToLineStart => "Delete to line start",
        A::Backspace => "Delete backward",
        A::Delete => "Delete forward",
        A::Newline => "New line",
        A::Undo => "Undo",
        A::Redo => "Redo",
        A::WordForward => "Next word",
        A::WordBackward => "Previous word",
        A::SelectWordForward => "Select next word",
        A::SelectWordBackward => "Select previous word",
        A::DeleteWordForward => "Delete next word",
        A::DeleteWordBackward => "Delete previous word",
        A::SelectAll => "Select all",
        A::Submit => "Submit",
    }
}

/// `keyBindingToString`.
fn key_binding_to_string(binding: &TextareaKeyBinding) -> String {
    let mut parts = Vec::new();
    if binding.ctrl {
        parts.push("ctrl");
    }
    if binding.shift {
        parts.push("shift");
    }
    if binding.meta {
        parts.push("meta");
    }
    if binding.super_ {
        parts.push("super");
    }
    parts.push(binding.name);
    parts.join("+")
}

/// `createTextareaBindings(overrides)`: the overrides first, so they take precedence, then the default textarea
/// bindings as commands.
pub fn create_textarea_bindings(overrides: Vec<km::Binding<Element>>) -> Vec<km::Binding<Element>> {
    let mut bindings = overrides;
    bindings.extend(DEFAULT_TEXTAREA_KEY_BINDINGS.iter().map(|binding| {
        let mut generated = km::Binding::new(
            key_binding_to_string(binding).as_str(),
            Some(km::BindingCommand::Name(edit_buffer_command_name(binding.action).to_string())),
        );
        generated.fields.push(("desc".into(), km::Field::Str(edit_buffer_command_description(binding.action).into())));
        generated.fields.push(("group".into(), km::Field::Str(DEFAULT_EDIT_BUFFER_METADATA_GROUP.into())));
        generated
    }));
    bindings
}

/// `registerTextareaMappingSuspension`: suspends the focused textarea's own key bindings so the keymap's take over,
/// restoring its previous suspend state on focus change or cleanup. Reference-counted per keymap.
pub fn register_textarea_mapping_suspension(keymap: &km::Keymap<Element>) -> km::Dispose {
    keymap.acquire_resource(TEXTAREA_MAPPING_SUSPENSION_RESOURCE, || {
        let previous_states: Rc<RefCell<HashMap<Element, bool>>> = Rc::new(RefCell::new(HashMap::new()));
        let suspended: Rc<RefCell<Option<Element>>> = Rc::new(RefCell::new(None));
        let suspend = {
            let previous_states = previous_states.clone();
            let suspended = suspended.clone();
            move |editor: Option<Element>| {
                let Some(editor) = editor.filter(|e| is_managed_textarea(*e) && !e.is_destroyed()) else {
                    *suspended.borrow_mut() = None;
                    return;
                };
                let mut traits = editor.textarea_traits();
                previous_states.borrow_mut().entry(editor).or_insert(traits.suspend);
                traits.suspend = true;
                editor.set_textarea_traits(traits);
                *suspended.borrow_mut() = Some(editor);
            }
        };
        let restore = {
            let previous_states = previous_states.clone();
            let suspended = suspended.clone();
            move |editor: Option<Element>| {
                let Some(editor) = editor else { return };
                let Some(previous) = previous_states.borrow_mut().remove(&editor) else { return };
                if is_managed_textarea(editor) && !editor.is_destroyed() {
                    let mut traits = editor.textarea_traits();
                    traits.suspend = previous;
                    editor.set_textarea_traits(traits);
                }
                let mut current = suspended.borrow_mut();
                if *current == Some(editor) {
                    *current = None;
                }
            }
        };
        let token = {
            let suspend = suspend.clone();
            let restore = restore.clone();
            renderer::on_focused_editor(move |current, previous| {
                restore(previous);
                suspend(current);
            })
        };
        suspend(renderer::current_focused_editor());
        Box::new(move || {
            renderer::off_focused_editor(token);
            let current = *suspended.borrow();
            restore(current);
        })
    })
}

/// `isManagedTextarea`: a textarea that is not an `<input>` (Input keeps its own bindings).
pub fn is_managed_textarea(editor: Element) -> bool {
    editor.is_textarea() && !editor.is_input()
}

/// `withFocusedEditor`.
fn with_focused_editor(run: impl FnOnce(Element) -> bool) -> bool {
    match renderer::current_focused_editor() {
        Some(editor) if !editor.is_destroyed() => run(editor),
        _ => false,
    }
}

/// `registerEditBufferCommands`: the `input.*` commands against the renderer's focused editor. Reference-counted
/// per keymap.
pub fn register_edit_buffer_commands(keymap: &km::Keymap<Element>) -> km::Dispose {
    let layer_keymap = keymap.clone();
    keymap.acquire_resource(EDIT_BUFFER_COMMANDS_RESOURCE, move || {
        let commands = TextareaAction::ALL
            .iter()
            .map(|&action| km::Command {
                name: edit_buffer_command_name(action).to_string(),
                run: Some(Rc::new(move |_context: &km::CommandContext<'_, Element>| {
                    with_focused_editor(|editor| editor.textarea_action(action)).into()
                })),
                fields: vec![
                    ("desc".into(), km::Field::Str(edit_buffer_command_description(action).into())),
                    ("category".into(), km::Field::Str(DEFAULT_EDIT_BUFFER_METADATA_GROUP.into())),
                ],
            })
            .collect();
        layer_keymap.register_layer(km::Layer { commands, ..km::Layer::default() })
    })
}

/// `registerManagedTextareaLayer(keymap, renderer, layer)`: the edit-buffer commands, the focused textarea's
/// suspension, and `layer` with the default textarea bindings after its own. The layer's target is ignored.
pub fn register_managed_textarea_layer(keymap: &km::Keymap<Element>, layer: km::Layer<Element>) -> km::Dispose {
    let off_commands = register_edit_buffer_commands(keymap);
    let off_suspension = register_textarea_mapping_suspension(keymap);
    let off_layer = keymap.register_layer(km::Layer {
        target: None,
        target_mode: None,
        bindings: create_textarea_bindings(layer.bindings),
        ..layer
    });
    Box::new(move || {
        off_layer();
        off_suspension();
        off_commands();
    })
}
