//! `routes/session/dialog-message.tsx`: a message's actions: jump to it, revert to it (restoring its prompt), copy its
//! text, and fork the session before it.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::solid::*;
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;

use crate::ctx::tui;
use crate::prompt::draft_stash::append_to_session_prompt;
use crate::prompt::restore::restore_prompt;
use crate::session::dialog_fork::dialog_fork;
use crate::toast::ToastInput;
use crate::ui::dialog::{DialogSize, use_dialog};
use crate::ui::dialog_select::{SelectOption, SelectProps, dialog_select};

fn message_text(message: &Json) -> String {
    match message.str("type") {
        Some("user") => message.str("text").unwrap_or_default().to_string(),
        Some("assistant") => message
            .arr("content")
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|part| part.str("type") == Some("text"))
            .map(|part| part.str("text").unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => message.str("text").unwrap_or_default().to_string(),
    }
}

/// `<DialogMessage messageID sessionID>`.
pub fn dialog_message(session_id: String, message_id: String) -> View {
    let dialog = use_dialog();
    let t = tui();
    let message = {
        let session_id = session_id.clone();
        let message_id = message_id.clone();
        t.data.store.session.message.get(&session_id, &message_id)
    };
    let new_designs = t.config.new_designs();
    dialog.set_size(DialogSize::Medium);
    let option = |title: &str, value: &str, description: &str| {
        let mut option = SelectOption::new(title, value.to_string());
        option.description = Some(description.into());
        option
    };
    let mut jump = option("Jump to", "message.jump", "view message in session");
    {
        let dialog = dialog.clone();
        jump.on_select = Some(Rc::new(move |_| dialog.clear()));
    }
    let mut revert = option("Revert", "session.revert", "undo messages and file changes");
    {
        let dialog = dialog.clone();
        let session_id = session_id.clone();
        let message_id = message_id.clone();
        let message = message.clone();
        let t = t.clone();
        revert.on_select = Some(Rc::new(move |_| {
            let (dialog, t, session_id, message_id, message) = (dialog.clone(), t.clone(), session_id.clone(), message_id.clone(), message.clone());
            tokio::task::spawn_local(async move {
                let restored = match message.as_ref().filter(|message| message.str("type") == Some("user")) {
                    Some(message) => match restore_prompt(&t, message, true).wait().await {
                        Ok(prompt) => Some(prompt),
                        Err(error) => {
                            t.toast.show(ToastInput::new(crate::toast::Variant::Error, error.message()).duration(5000));
                            return;
                        }
                    },
                    None => None,
                };
                let input = obj! { "sessionID" => session_id.as_str(), "messageID" => message_id.as_str() };
                let result = t.data.client.call_named("session.revert.stage", &input, RequestOptions::default()).await;
                match result {
                    Ok(_) => {
                        if let Some(restored) = restored {
                            append_to_session_prompt(&session_id, restored);
                        }
                        dialog.clear();
                    }
                    Err(error) => {
                        t.toast.show(ToastInput::new(crate::toast::Variant::Error, error.message()).duration(5000));
                    }
                }
            });
        }));
    }
    let mut copy = option("Copy", "message.copy", "message text to clipboard");
    {
        let dialog = dialog.clone();
        let toast = t.toast.clone();
        copy.on_select = Some(Rc::new(move |_| {
            let Some(message) = message.clone() else { return };
            let text = message_text(&message);
            let dialog = dialog.clone();
            let toast = toast.clone();
            tokio::task::spawn_local(async move {
                match crate::selection::write_clipboard(&text).await {
                    Ok(()) => dialog.clear(),
                    Err(error) => toast.error(error),
                }
            });
        }));
    }
    let mut fork = option("Fork", "session.fork", "create a new session");
    {
        let dialog = dialog.clone();
        let session_id = session_id.clone();
        let message_id = message_id.clone();
        fork.on_select = Some(Rc::new(move |_| {
            let (session_id, message_id) = (session_id.clone(), message_id.clone());
            dialog.replace(
                move || {
                    let (session_id, message_id) = (session_id.clone(), message_id.clone());
                    component(move || dialog_fork(session_id.clone(), Some(message_id.clone()), None))
                },
                None,
                None,
                None,
            );
        }));
    }
    let mut props = SelectProps::new("Message Actions", move || vec![jump.clone(), revert.clone(), copy.clone(), fork.clone()]);
    props.render_filter = !new_designs;
    dialog_select(props)
}
