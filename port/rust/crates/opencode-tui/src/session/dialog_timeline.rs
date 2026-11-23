//! `routes/session/dialog-timeline.tsx`: the session's user prompts, newest first, with their time; moving through
//! the list scrolls the transcript to the highlighted prompt and selecting one opens its message actions.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::solid::*;
use opencode_tui_sync::json::JsonExt;

use crate::ctx::tui;
use crate::session::dialog_message::dialog_message;
use crate::ui::dialog::{DialogSize, use_dialog};
use crate::ui::dialog_select::{Footer, SelectOption, SelectProps, dialog_select};
use crate::ui::locale;

fn created(message: &Json) -> f64 {
    message.get("time").and_then(|time| time.get("created")).and_then(|value| match value {
        Json::Number(number) => Some(*number),
        _ => None,
    }).unwrap_or(0.0)
}

/// `<DialogTimeline sessionID onMove>`.
pub fn dialog_timeline(session_id: String, on_move: Rc<dyn Fn(String)>) -> View {
    let dialog = use_dialog();
    let data = tui().data.clone();
    dialog.set_size(DialogSize::Large);
    let session_for_options = session_id.clone();
    let options = move || {
        let messages = data.store.session.message.list(&session_for_options);
        let mut options: Vec<SelectOption<String>> = messages
            .iter()
            .filter(|message| message.str("type") == Some("user"))
            .map(|message| {
                let id = message.str("id").unwrap_or_default().to_string();
                let mut option = SelectOption::new(message.str("text").unwrap_or_default().replace('\n', " "), id.clone());
                option.footer = Some(Footer::Text(locale::time(created(&message))));
                let dialog = dialog.clone();
                let session_id = session_id.clone();
                option.on_select = Some(Rc::new(move |_| {
                    let session_id = session_id.clone();
                    let message_id = id.clone();
                    dialog.replace(
                        move || {
                            let session_id = session_id.clone();
                            let message_id = message_id.clone();
                            component(move || dialog_message(session_id.clone(), message_id.clone()))
                        },
                        None,
                        None,
                        None,
                    );
                }));
                option
            })
            .collect();
        options.reverse();
        options
    };
    let mut props = SelectProps::new("Timeline", options);
    props.search_in_title = true;
    props.title_placeholder = Some(Rc::new(|| "Search timeline".to_string()));
    props.on_move = Some(Rc::new(move |option: &SelectOption<String>| on_move(option.value.clone())));
    dialog_select(props)
}
