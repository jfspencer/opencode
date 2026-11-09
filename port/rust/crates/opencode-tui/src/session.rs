//! Submitting from the home prompt (`component/prompt/index.tsx` `submit` without a session).
//!
//! TODO(port-tui): the session feature slice: creating the session (with `--session`'s new ID), selecting its
//! model and agent, sending the prompt, and the session route (`routes/session`) with its transcript.
use crate::ctx::Tui;

pub fn submit_from_home(t: &Tui, value: String) {
    let trimmed = value.trim();
    // `exitWord`, and the `app.exit` command's slash name and aliases.
    if matches!(trimmed, "exit" | "quit" | ":q" | "/exit" | "/quit" | "/q") {
        (t.exit)();
        return;
    }
    // `promptModelWarning()`: nothing to send the prompt to. TODO(port-tui): the integration dialog when no
    // provider is connected.
    let has_model = t.data.models.with_untracked(|models| models.as_ref().is_some_and(|m| !m.is_empty()));
    if !has_model {
        t.toast.show(
            crate::toast::ToastInput::new(crate::toast::Variant::Warning, "Connect an integration to send prompts")
                .duration(3000),
        );
    }
}
