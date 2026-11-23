//! `routes/session/dialog-fork.tsx`: fork the whole session or a chosen user prompt (restoring that prompt into the
//! new session's composer), with the forking spinner.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::text_node_from_string;
use opencode_tui_core::solid::*;
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;

use crate::ctx::tui;
use crate::prompt::restore::restore_prompt;
use crate::theme::SurfaceName;
use crate::toast::ToastInput;
use crate::ui::dialog::{DIALOG_LIST_INSET, DialogSize, HeaderProps, LayoutProps, dialog_header, dialog_layout, use_dialog};
use crate::ui::dialog_select::{Footer, SelectOption, SelectProps, dialog_select};
use crate::ui::locale;
use crate::ui::theme::use_theme;
use crate::util::rgba;

fn created(message: &Json) -> f64 {
    message
        .get("time")
        .and_then(|time| time.get("created"))
        .and_then(|value| match value {
            Json::Number(number) => Some(*number),
            _ => None,
        })
        .unwrap_or(0.0)
}

/// `fork(messageID)`.
pub async fn fork(t: crate::ctx::Tui, session_id: String, message_id: Option<String>, dialog: crate::ui::dialog::DialogContext) {
    let data = t.data.clone();
    let toast = t.toast.clone();
    let message = message_id.as_deref().and_then(|id| data.store.session.message.get(&session_id, id));
    let prompt = match message.as_ref().filter(|message| message.str("type") == Some("user")) {
        Some(message) => match restore_prompt(&t, message, false).wait().await {
            Ok(prompt) => Some(prompt),
            Err(error) => {
                toast.show(ToastInput::new(crate::toast::Variant::Error, error.message()).duration(5000));
                dialog.clear();
                return;
            }
        },
        None => None,
    };
    let mut input = obj! { "sessionID" => session_id.as_str() };
    if let Some(message_id) = &message_id {
        input.set("before", Json::String(message_id.clone()));
    }
    let result = t.data.client.call_named("session.fork", &input, RequestOptions::default()).await;
    let id = match result {
        Ok(result) => result.json().str("id").unwrap_or_default().to_string(),
        Err(error) => {
            toast.show(ToastInput::new(crate::toast::Variant::Error, error.message()).duration(5000));
            dialog.clear();
            return;
        }
    };
    let prompt = prompt.map(|mut prompt| {
        if prompt.agents.is_none() {
            prompt.agents = Some(Vec::new());
        }
        prompt
    });
    crate::startup::navigate_session(&t, id, prompt);
    dialog.clear();
    toast.show(ToastInput::new(crate::toast::Variant::Success, "Forked session").duration(4000));
}

/// `<DialogFork sessionID messageID onMove>`.
pub fn dialog_fork(session_id: String, message_id: Option<String>, on_move: Option<Rc<dyn Fn(Option<String>)>>) -> View {
    let t = tui();
    let dialog = use_dialog();
    let data = t.data.clone();
    let theme = use_theme().surface(SurfaceName::Dialog);
    dialog.set_size(DialogSize::Large);
    let (pending, _set_pending) = create_signal(message_id.is_some());
    if let Some(message_id) = message_id.clone() {
        let session_id = session_id.clone();
        let dialog = dialog.clone();
        let t = t.clone();
        on_mount(move || {
            let session_id = session_id.clone();
            let message_id = message_id.clone();
            let dialog = dialog.clone();
            let t = t.clone();
            tokio::task::spawn_local(async move {
                fork(t, session_id, Some(message_id), dialog).await;
            });
        });
    }
    let options = {
        let session_id = session_id.clone();
        let dialog = dialog.clone();
        move || {
            let session_id_for_fork = session_id.clone();
            let mut options: Vec<SelectOption<Option<String>>> =
                vec![SelectOption::new("Full session", None)];
            {
                let option = &mut options[0];
                let dialog = dialog.clone();
                let session_id = session_id_for_fork.clone();
                let t = t.clone();
                option.on_select = Some(Rc::new(move |_| {
                    let (session_id, dialog, t) = (session_id.clone(), dialog.clone(), t.clone());
                    tokio::task::spawn_local(async move {
                        fork(t, session_id, None, dialog).await;
                    });
                }));
            }
            let stored = data.store.session.message.list(&session_id);
            let mut messages: Vec<SelectOption<Option<String>>> = stored
                .iter()
                .filter(|message| message.str("type") == Some("user"))
                .map(|message| {
                    let id = message.str("id").unwrap_or_default().to_string();
                    let mut option = SelectOption::new(message.str("text").unwrap_or_default().replace('\n', " "), Some(id.clone()));
                    option.footer = Some(Footer::Text(locale::time(created(&message))));
                    let dialog = dialog.clone();
                    let session_id = session_id.clone();
                    let t = t.clone();
                    option.on_select = Some(Rc::new(move |_| {
                        let (session_id, dialog, message_id, t) = (session_id.clone(), dialog.clone(), id.clone(), t.clone());
                        tokio::task::spawn_local(async move {
                            fork(t, session_id, Some(message_id), dialog).await;
                        });
                    }));
                    option
                })
                .collect();
            messages.reverse();
            options.extend(messages);
            options
        }
    };
    let fallback = {
        let theme = theme;
        move || {
            let body = box_();
            body.set_flex_direction(opencode_tui_core::tree::FlexDirection::Row);
            body.set_gap(2, Some(Value::Point(1.0)));
            let line = text();
            line.set_fg(Some(rgba(theme.get().text.muted)));
            insert_node(line, text_node_from_string("Forking session…"), None);
            insert_node(body, line, None);
            dialog_layout(LayoutProps {
                id: None,
                header: Box::new(move || {
                    dialog_header(HeaderProps {
                        title: Some(Rc::new(|| "Fork session".to_string())),
                        ..HeaderProps::default()
                    })
                }),
                footer: None,
                legacy_footer: None,
                inset: Some(DIALOG_LIST_INSET),
                footer_gap: None,
                children: Box::new(move || View::Node(body)),
            })
        }
    };
    show(move || !pending.get(), move || {
        let mut props = SelectProps::new("Fork session", options.clone());
        props.on_move = on_move.clone().map(|f| Rc::new(move |option: &SelectOption<Option<String>>| f(option.value.clone())) as Rc<dyn Fn(&SelectOption<Option<String>>)>);
        // `DialogFork`'s current is `undefined`, which marks the `undefined`-valued "Full session" row.
        props.current = Some(Rc::new(|| Some(None)));
        dialog_select(props)
    }, Some(Rc::new(fallback)))
}
