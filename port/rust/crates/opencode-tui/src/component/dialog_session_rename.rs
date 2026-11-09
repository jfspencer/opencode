//! `component/dialog-session-rename.tsx`.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::solid::*;

use crate::ctx::tui;
use crate::data::object;
use crate::ui::dialog::{DialogContext, use_dialog};
use crate::ui::dialog_prompt::{PromptProps, dialog_prompt};

/// `<DialogSessionRename sessionID currentTitle>`.
pub fn dialog_session_rename(session_id: String, current_title: Option<String>) -> View {
    let dialog = use_dialog();
    let client = tui().data.client.clone();
    let toast = tui().toast;
    let cancel = dialog.clone();
    dialog_prompt(PromptProps {
        title: "Rename session".into(),
        placeholder: Some("Session title".into()),
        value: current_title,
        on_confirm: Some(Rc::new(move |value: String| {
            let title = value.trim().to_string();
            if title.is_empty() {
                return;
            }
            let client = client.clone();
            let toast = toast.clone();
            let dialog = dialog.clone();
            let input = object(vec![("sessionID", Json::String(session_id.clone())), ("title", Json::String(title))]);
            tokio::task::spawn_local(async move {
                match client.call_named("session.update", &input, Default::default()).await {
                    Ok(_) => dialog.clear(),
                    Err(error) => toast.show(
                        crate::toast::ToastInput::new(crate::toast::Variant::Error, format!("Failed to rename session: {}", error.message()))
                            .duration(5000),
                    ),
                }
            });
        })),
        on_cancel: Some(Rc::new(move || cancel.clear())),
        ..PromptProps::default()
    })
}

/// `DialogSessionRename.show(dialog, sessionID, currentTitle)`.
pub fn show(dialog: &DialogContext, session_id: &str, current_title: Option<String>) {
    let session_id = session_id.to_string();
    dialog.show(move || {
        let session_id = session_id.clone();
        let title = current_title.clone();
        component(move || dialog_session_rename(session_id, title))
    });
}
