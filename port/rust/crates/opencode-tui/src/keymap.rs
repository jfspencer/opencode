//! Keybinds (`context/keymap.tsx` over `@opentui/keymap`).
//!
//! TODO(port-tui): opencode-tui-input's keymap (`@opentui/keymap`: layers, leader sequences, priorities,
//! `formatKeySequence`). Until it lands, hints print the first binding of each command's default as written in
//! `config/keybind.ts`, and the only bindings dispatched are the prompt's `prompt.clear` (ctrl+c with text) and
//! `app.exit` (ctrl+c on an empty prompt, ctrl+d on an empty prompt, the leader then q).
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

/// Whether a key event is one written binding (`ctrl+x`, `shift+tab`, `f3`; no sequences).
fn matches(binding: &str, key: &opencode_tui_core::input::KeyEvent) -> bool {
    let parts: Vec<String> = binding.to_lowercase().split('+').map(str::to_string).collect();
    let Some((name, modifiers)) = parts.split_last() else { return false };
    let has = |m: &str| modifiers.iter().any(|x| x == m);
    let name = match name.as_str() {
        "enter" => "return",
        "esc" => "escape",
        other => other,
    };
    key.name == name
        && key.ctrl == has("ctrl")
        && key.shift == has("shift")
        && key.meta == (has("alt") || has("meta") || has("option"))
        && key.super_ == has("super")
}

/// The commands the stand-in dispatches through the configured bindings (`config.keybinds.get(command)`).
fn configured_commands(t: &Tui) -> Vec<(String, std::rc::Rc<dyn Fn()>)> {
    let mut out: Vec<(String, std::rc::Rc<dyn Fn()>)> = Vec::new();
    let local = crate::local::local();
    let handlers: Vec<(&str, std::rc::Rc<dyn Fn()>)> = vec![
        ("model.cycle_favorite", std::rc::Rc::new({
            let local = local.clone();
            move || local.cycle_favorite(1)
        })),
        ("model.cycle_favorite_reverse", std::rc::Rc::new(move || local.cycle_favorite(-1))),
    ];
    for (command, handler) in handlers {
        for binding in t.config.keybinds.get(command) {
            if let opencode_protocol::Json::String(key) = binding.key() {
                if !key.contains("<leader>") && key != "none" {
                    out.push((key.clone(), handler.clone()));
                }
            }
        }
    }
    out
}

/// Registers the global bindings the stand-in dispatches.
pub fn install(t: &Tui) {
    let configured = configured_commands(t);
    let prompt = t.prompt.clone();
    let exit = t.exit.clone();
    // The leader (ctrl+x) arms a second key for `leader.timeout` ms.
    let leader = std::rc::Rc::new(std::cell::Cell::new(false));
    let timeout = t.config.leader.timeout;
    renderer::on_key(move |key| {
        if leader.replace(false) {
            if key.name == "q" && !key.ctrl && !key.meta {
                key.prevent_default();
                exit();
            }
            key.prevent_default();
            return;
        }
        if key.ctrl && !key.meta && !key.shift && key.name == "x" {
            leader.set(true);
            let leader = leader.clone();
            renderer::set_timeout(timeout as u64, move || leader.set(false));
            key.prevent_default();
            return;
        }
        if let Some((_, run)) = configured.iter().find(|(binding, _)| matches(binding, key)) {
            key.prevent_default();
            run();
            return;
        }
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
