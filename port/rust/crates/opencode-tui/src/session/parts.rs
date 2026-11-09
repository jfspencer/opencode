//! The part dispatch of the transcript (`SessionPartView` in routes/session/index.tsx) and the extension points the
//! tools slice fills.
//!
//! **Extension API.** A part row resolves its part ([`super::rows::resolve_part`]) and dispatches on `part.type`:
//! `text` → [`super::message_parts::text_part`], `reasoning` → [`super::message_parts::reasoning_part`], `tool` →
//! [`tool_part`]. The tool side is the tools slice's: [`tool_part`] (`<ToolPart part images>`), [`tool_images`]
//! (`<ToolImages parts>`), [`shell_display`] (`<ShellDisplay>` of shell messages), [`permission_prompt`] and
//! [`form_prompt`] (the prompts the Session's composer switch shows). Each forwards to the tools slice's module;
//! each is a one-line call into the tools slice's module.
//!
//! The part is read reactively: `part()` returns the current JSON record (`SessionMessageAssistantTool` for tools),
//! so a tool row updates in place as its state streams.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::reactive::*;
use opencode_tui_core::solid::*;
use opencode_tui_sync::json::JsonExt;

use super::message_parts::{PartProps, reasoning_part, text_part};
use super::rows::resolve_part;

/// Record identity, as a TS memo compares objects (the store makes a new record on every change).
pub fn same_record(a: &Option<Rc<Json>>, b: &Option<Rc<Json>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// `<SessionPartView partRef message images>`.
pub fn session_part_view(message: Rc<dyn Fn() -> Option<Rc<Json>>>, part_id: String, images: Option<bool>) -> View {
    let message_memo = {
        let message = message.clone();
        create_memo_equals(move || message(), Some(same_record))
    };
    let part = create_memo(move || {
        let item = message_memo.get()?;
        if item.str("type") != Some("assistant") {
            return None;
        }
        resolve_part(&item, &part_id).map(Rc::new)
    });
    show_value(
        move || part.get(),
        move |item| {
            let current: Rc<dyn Fn() -> Rc<Json>> = Rc::new(move || item.get().unwrap_or_else(|| Rc::new(Json::Null)));
            let message: Rc<dyn Fn() -> Rc<Json>> = Rc::new(move || message_memo.get().unwrap_or_else(|| Rc::new(Json::Null)));
            let kind = {
                let current = current.clone();
                create_memo(move || current().str("type").unwrap_or_default().to_string())
            };
            let (c1, c2, c3) = (current.clone(), current.clone(), current.clone());
            let (m1, m2) = (message.clone(), message.clone());
            switch(
                vec![
                    when(move || kind.get() == "text", move || text_part(PartProps { part: c1.clone(), message: m1.clone(), last: false })),
                    when(move || kind.get() == "reasoning", move || {
                        reasoning_part(PartProps { part: c2.clone(), message: m2.clone(), last: false })
                    }),
                    when(move || kind.get() == "tool", move || tool_part(c3.clone(), images)),
                ],
                None,
            )
        },
        None,
    )
}

/// `<ToolPart part images>` (the tools slice's `crate::tools::tool_part`).
pub fn tool_part(part: Rc<dyn Fn() -> Rc<Json>>, images: Option<bool>) -> View {
    crate::tools::tool_part(part, images)
}

/// `<ToolImages parts>` (the tools slice's `crate::tools::tool_images`).
pub fn tool_images(parts: Rc<dyn Fn() -> Vec<Rc<Json>>>) -> View {
    crate::tools::tool_images(parts)
}

/// `<ShellMessage message>` (index.tsx): the message's command through the tools slice's `<ShellDisplay>`.
pub fn shell_message(message: Rc<dyn Fn() -> Rc<Json>>) -> View {
    let error = {
        let message = message.clone();
        create_memo(move || {
            let message = message();
            match message.str("status") {
                Some("killed") => return Some("Command cancelled".to_string()),
                Some("timeout") => return Some("Command timed out".to_string()),
                _ => {}
            }
            match message.num("exit") {
                Some(exit) if exit != 0.0 => Some(format!("Command exited with code {}", opencode_tui_sync::js::number(exit))),
                _ => None,
            }
        })
    };
    let (m1, m2, m3, m4) = (message.clone(), message.clone(), message.clone(), message.clone());
    crate::tools::parts::shell_display(crate::tools::parts::ShellDisplayProps {
        part: None,
        shell_id: Rc::new(move || m1().str("shellID").map(str::to_string)),
        command: Rc::new(move || m2().str("command").map(str::to_string)),
        workdir: Rc::new(|| None),
        status: Rc::new(move || if m3().str("status") == Some("running") { "running".into() } else { "completed".into() }),
        background: Rc::new(|| false),
        output: Rc::new(move || m4().get("output").and_then(|o| o.str("output")).map(str::to_string)),
        error: Some(Rc::new(move || error.get())),
    })
}

/// `<PermissionPrompt request directory>` (the tools slice's routes/session/permission.tsx).
pub fn permission_prompt(request: Rc<Json>, directory: Option<String>) -> View {
    crate::permission::permission_prompt(request, directory)
}

/// `<FormPrompt form>` (the tools slice's routes/session/form.tsx).
pub fn form_prompt(form: Rc<Json>) -> View {
    crate::form::form_prompt(form)
}
