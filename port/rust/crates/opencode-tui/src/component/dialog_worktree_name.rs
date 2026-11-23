//! `component/dialog-worktree-name.tsx`: the worktree name prompt, with the generated-name action.
use std::rc::Rc;

use opencode_tui_core::solid::*;

use crate::ui::dialog::{DialogSize, use_dialog};
use crate::ui::dialog_prompt::{PromptAction, PromptProps, dialog_prompt, PromptHandle};

const ADJECTIVES: [&str; 30] = [
    "brave", "calm", "clever", "cosmic", "crisp", "curious", "eager", "gentle", "glowing", "happy", "hidden", "jolly", "kind",
    "lucky", "mighty", "misty", "neon", "nimble", "playful", "proud", "quick", "quiet", "shiny", "silent", "stellar", "sunny",
    "swift", "tidy", "witty", "bright",
];

const NOUNS: [&str; 30] = [
    "cabin", "cactus", "canyon", "circuit", "comet", "eagle", "engine", "falcon", "forest", "garden", "harbor", "island",
    "knight", "lagoon", "meadow", "moon", "mountain", "nebula", "orchid", "otter", "panda", "pixel", "planet", "river",
    "rocket", "sailor", "squid", "star", "tiger", "wizard",
];

/// `Slug.create()`.
fn slug_create() -> String {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    let first = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let second = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    format!("{}-{}", ADJECTIVES[first % ADJECTIVES.len()], NOUNS[second % NOUNS.len()])
}

/// `slugify(input)`.
pub fn slugify(input: &str) -> String {
    let lowered = input.trim().to_lowercase();
    let mut out = String::new();
    let mut dash = false;
    for character in lowered.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// `<DialogWorktreeName onConfirm>`.
pub fn dialog_worktree_name(on_confirm: Rc<dyn Fn(String)>) -> View {
    let dialog = use_dialog();
    dialog.set_size(DialogSize::Medium);
    let actions = vec![PromptAction {
        title: "generate one".into(),
        command: "dialog.worktree.generate".into(),
        command_title: Some("Generate worktree name".into()),
        on_trigger: Rc::new(move |prompt: PromptHandle| prompt.set_value(&slug_create())),
    }];
    dialog_prompt(PromptProps {
        title: "Name worktree".into(),
        placeholder: Some("Worktree name".into()),
        actions,
        on_confirm: Some(Rc::new(move |value: String| {
            let slug = slugify(&value);
            on_confirm(if slug.is_empty() { slug_create() } else { slug });
        })),
        on_cancel: Some(Rc::new(move || dialog.clear())),
        ..PromptProps::default()
    })
}

/// `DialogWorktreeName.show(dialog)`: the prompt's resolved name, or `None` when cancelled.
pub fn show_worktree_name(dialog: &crate::ui::dialog::DialogContext) -> opencode_tui_sync::runtime::Deferred<Option<String>> {
    let result = opencode_tui_sync::runtime::Deferred::new();
    let resolve = result.clone();
    let cancel = result.clone();
    dialog.replace(
        move || {
            let resolve = resolve.clone();
            component(move || dialog_worktree_name(Rc::new(move |name| resolve.resolve(Some(name)))))
        },
        Some(Rc::new(move || cancel.resolve(None))),
        None,
        Some(DialogSize::Medium),
    );
    result
}
