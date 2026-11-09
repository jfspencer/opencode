//! Keybinds (`context/keymap.tsx` over `@opentui/keymap`).
//!
//! TODO(port-tui): opencode-tui-input's keymap (`@opentui/keymap`: layers, leader sequences, priorities,
//! `formatKeySequence`). Until it lands, hints print the first binding of each command's default as written in
//! `config/keybind.ts`, and the only bindings dispatched are the prompt's `prompt.clear` (ctrl+c with text) and
//! `app.exit` (ctrl+c on an empty prompt, ctrl+d).
use std::collections::HashMap;

use opencode_tui_core::renderer;

use crate::config::keybind;
use crate::ctx::Tui;

/// `keymap.shortcuts(command)[0]` for every command.
pub fn shortcuts() -> HashMap<&'static str, String> {
    let mut out = HashMap::new();
    for (name, default, _) in keybind::DEFINITIONS {
        let value = default.trim_matches('"');
        if value == "none" {
            continue;
        }
        if let Some(first) = value.split(',').next() {
            out.insert(*name, first.replace("<leader>", "ctrl+x "));
        }
    }
    out
}

/// Registers the global bindings the stand-in dispatches.
pub fn install(t: &Tui) {
    let prompt = t.prompt.clone();
    let exit = t.exit.clone();
    renderer::on_key(move |key| {
        let plain_ctrl = key.ctrl && !key.meta && !key.shift && !key.super_;
        if plain_ctrl && key.name == "c" {
            let input = *prompt.borrow();
            if let Some(input) = input.filter(|i| i.is_alive() && !i.textarea_text().is_empty()) {
                input.set_textarea_text("");
                key.prevent_default();
                key.stop_propagation();
                return;
            }
            key.prevent_default();
            exit();
            return;
        }
        if plain_ctrl && key.name == "d" {
            let empty = prompt.borrow().is_none_or(|i| i.textarea_text().is_empty());
            if empty {
                key.prevent_default();
                exit();
            }
        }
    });
}
