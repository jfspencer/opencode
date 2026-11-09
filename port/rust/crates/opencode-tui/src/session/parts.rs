//! The part dispatch of the transcript (`SessionPartView` in routes/session/index.tsx) and the extension points the
//! tools slice fills.
//!
//! **Extension API.** A part row resolves its part ([`super::rows::resolve_part`]) and dispatches on `part.type`:
//! `text` → [`super::message_parts::text_part`], `reasoning` → [`super::message_parts::reasoning_part`], `tool` →
//! [`tool_part`]. The tool side is the tools slice's: [`tool_part`] (`<ToolPart part images>`), [`tool_images`]
//! (`<ToolImages parts>`), [`shell_display`] (`<ShellDisplay>` of shell messages), [`permission_prompt`] and
//! [`form_prompt`] (the prompts the Session's composer switch shows). Each forwards to the tools slice's module;
//! until it lands, a minimal inline row stands in (`TODO(port-tui): tools`), so replace the body of each with the
//! one-line call into `crate::tools`.
//!
//! The part is read reactively: `part()` returns the current JSON record (`SessionMessageAssistantTool` for tools),
//! so a tool row updates in place as its state streams.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::reactive::*;
use opencode_tui_core::solid::*;
use opencode_tui_sync::json::JsonExt;

use super::message_parts::{InlineToolRowProps, PartProps, inline_tool_row, reasoning_part, text_part};
use super::rows::resolve_part;
use crate::ctx::tui;
use crate::util::rgba;

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

/// `<ToolPart part images>` (the tools slice's).
///
/// TODO(port-tui): tools: `crate::tools::tool_part(part, images)`. This stand-in shows the tool's name as an inline
/// row (`✓`/`✗` once settled).
pub fn tool_part(part: Rc<dyn Fn() -> Rc<Json>>, images: Option<bool>) -> View {
    let _ = images;
    let theme = tui().theme;
    let status = {
        let part = part.clone();
        create_memo(move || part().get("state").and_then(|s| s.str("status")).unwrap_or_default().to_string())
    };
    let name = {
        let part = part.clone();
        create_memo(move || part().str("name").unwrap_or_default().to_string())
    };
    inline_tool_row(InlineToolRowProps {
        icon: Rc::new(move || if status.get() == "error" { "✗".into() } else { "✓".into() }),
        color: Some(Rc::new(move || Some(rgba(theme.with(|th| th.text.muted))))),
        complete: Rc::new(move || status.get() == "completed"),
        pending: Rc::new(move || name.get()),
        spinner: Rc::new(move || matches!(status.get().as_str(), "streaming" | "running")),
        children: Rc::new(move || View::from(name.get())),
        ..InlineToolRowProps::default()
    })
}

/// `<ToolImages parts>` (the tools slice's). TODO(port-tui): tools: `crate::tools::tool_images(parts)`.
pub fn tool_images(parts: Rc<dyn Fn() -> Vec<Rc<Json>>>) -> View {
    let _ = parts;
    View::Empty
}

/// `<ShellMessage message>` (index.tsx), whose `<ShellDisplay>` is the tools slice's.
///
/// TODO(port-tui): tools: `<ShellDisplay shellID command status output error>`; this stand-in shows the command.
pub fn shell_message(message: Rc<dyn Fn() -> Rc<Json>>) -> View {
    let theme = tui().theme;
    let running = {
        let message = message.clone();
        create_memo(move || message().str("status") == Some("running"))
    };
    inline_tool_row(InlineToolRowProps {
        icon: Rc::new(|| "$".into()),
        color: Some(Rc::new(move || Some(rgba(theme.with(|th| th.text.muted))))),
        complete: Rc::new(move || !running.get()),
        pending: Rc::new(|| "Shell".into()),
        spinner: Rc::new(move || running.get()),
        children: Rc::new(move || View::from(message().str("command").unwrap_or_default().to_string())),
        ..InlineToolRowProps::default()
    })
}

/// `<PermissionPrompt request directory>` (the tools slice's routes/session/permission.tsx).
/// TODO(port-tui): tools: `crate::permission::permission_prompt(request, directory)`.
pub fn permission_prompt(request: Rc<Json>, directory: Option<String>) -> View {
    let _ = (request, directory);
    View::Empty
}

/// `<FormPrompt form>` (the tools slice's routes/session/form.tsx). TODO(port-tui): tools:
/// `crate::form::form_prompt(form)`.
pub fn form_prompt(form: Rc<Json>) -> View {
    let _ = form;
    View::Empty
}
