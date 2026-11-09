//! Pending prompts of the session route (routes/session/index.tsx): `requestPending` / `mutatePending` (steer,
//! queue or cancel an inbox item, one request per item at a time, as `createSingleFlight`), the queued prompts
//! dialog (`openQueuedPrompts`) and the pending-steer dialog of a user message.
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_sync::runtime::Deferred;

use super::render_context::PendingAction;
use crate::ctx::Tui;
use crate::ui::dialog::DialogContext;
use crate::ui::dialog_select::{FooterHint, Footer, Group, SelectAction, SelectOption, SelectProps, dialog_select};

/// The single-flight pending mutations of one session.
#[derive(Clone)]
pub struct Pending {
    t: Tui,
    session_id: String,
    running: Rc<RefCell<HashSet<String>>>,
}

fn object(entries: Vec<(&str, Json)>) -> Json {
    Json::Object(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

impl Pending {
    pub fn new(t: Tui, session_id: String) -> Pending {
        Pending { t, session_id, running: Rc::default() }
    }

    /// `requestPending(action, inboxID, failureLabel)`: true when the server took it.
    async fn request(t: Tui, session_id: String, action: PendingAction, inbox_id: String, failure_label: Option<&'static str>) -> bool {
        let base = vec![("sessionID", Json::String(session_id)), ("inboxID", Json::String(inbox_id))];
        let (name, input) = match action {
            PendingAction::Steer => ("session.inbox.update", object([base, vec![("delivery", Json::String("steer".into()))]].concat())),
            PendingAction::Queue => ("session.inbox.update", object([base, vec![("delivery", Json::String("queue".into()))]].concat())),
            PendingAction::Cancel => ("session.inbox.cancel", object(base)),
        };
        match t.data.client.call_named(name, &input, Default::default()).await {
            Ok(_) => true,
            Err(error) => {
                let label = failure_label.unwrap_or(match action {
                    PendingAction::Steer => "steer",
                    PendingAction::Queue => "queue",
                    PendingAction::Cancel => "delete",
                });
                t.toast.show(
                    crate::toast::ToastInput::new(crate::toast::Variant::Error, error.message())
                        .title(format!("Failed to {label} pending prompt")),
                );
                false
            }
        }
    }

    /// `mutatePending(action, inboxID)`: false when the request failed or one for the item is still running.
    pub fn mutate(&self, action: PendingAction, inbox_id: &str) -> Deferred<bool> {
        if !self.running.borrow_mut().insert(inbox_id.to_string()) {
            return Deferred::resolved(false);
        }
        let (t, session_id, running, inbox_id) = (self.t.clone(), self.session_id.clone(), self.running.clone(), inbox_id.to_string());
        Deferred::spawn(async move {
            let ok = Self::request(t, session_id, action, inbox_id.clone(), None).await;
            running.borrow_mut().remove(&inbox_id);
            ok
        })
    }

    /// `openQueuedPrompts()`: the queued prompts, steered on enter, deleted or (TODO) undone with their actions.
    pub fn open_queued(&self, dialog: &DialogContext, queued: Rc<dyn Fn() -> Vec<(String, String)>>) {
        let this = self.clone();
        let inner = dialog.clone();
        dialog.replace(
            move || {
                let queued_for_options = queued.clone();
                let mut props = SelectProps::new("Queued prompts", move || {
                    let list = queued_for_options();
                    let count = list.len();
                    list.into_iter()
                        .enumerate()
                        .map(|(index, (id, text))| {
                            let mut option = SelectOption::new(text, id);
                            option.footer = Some(Footer::Text(format!("{} of {count}", index + 1)));
                            option
                        })
                        .collect()
                });
                props.search_in_title = true;
                props.title_placeholder = Some("Search queued prompts".into());
                let (select_this, select_dialog) = (this.clone(), inner.clone());
                props.on_select = Some(Rc::new(move |option: &SelectOption<String>| {
                    let steered = select_this.mutate(PendingAction::Steer, &option.value);
                    let dialog = select_dialog.clone();
                    tokio::task::spawn_local(async move {
                        if steered.wait().await {
                            dialog.clear();
                        }
                    });
                }));
                let (delete_this, delete_dialog, delete_queued) = (this.clone(), inner.clone(), queued.clone());
                props.actions = Some(Rc::new(move || {
                    let (this, dialog, queued) = (delete_this.clone(), delete_dialog.clone(), delete_queued.clone());
                    vec![
                        SelectAction::new("queued_prompt.delete", "delete", move |option: &SelectOption<String>| {
                            let last = queued().len() == 1;
                            let cancelled = this.mutate(PendingAction::Cancel, &option.value);
                            let dialog = dialog.clone();
                            tokio::task::spawn_local(async move {
                                if cancelled.wait().await && last {
                                    dialog.clear();
                                }
                            });
                        }),
                        // TODO(port-tui): the prompt slice's restore_prompt and append_to_session_prompt (`undoPending`).
                        SelectAction::new("queued_prompt.undo", "undo", |_: &SelectOption<String>| {}),
                    ]
                }));
                props.footer_hints =
                    vec![FooterHint { title: "steer".into(), label: "enter".into(), side: None, group: Some(Group::Primary) }];
                dialog_select(props)
            },
            None,
            None,
            None,
        );
    }

    /// The pending-steer dialog of a user message: move it to the queue or delete it.
    pub fn open_steer(&self, dialog: &DialogContext, inbox_id: String, render_filter: bool) {
        let this = self.clone();
        let inner = dialog.clone();
        dialog.replace(
            move || {
                let mut props = SelectProps::new("Pending steer", || {
                    vec![SelectOption::new("Move to queue", PendingAction::Queue), SelectOption::new("Delete", PendingAction::Cancel)]
                });
                props.render_filter = render_filter;
                let (this, dialog, inbox_id) = (this.clone(), inner.clone(), inbox_id.clone());
                props.on_select = Some(Rc::new(move |option: &SelectOption<PendingAction>| {
                    let done = this.mutate(option.value, &inbox_id);
                    let dialog = dialog.clone();
                    tokio::task::spawn_local(async move {
                        if done.wait().await {
                            dialog.clear();
                        }
                    });
                }));
                dialog_select(props)
            },
            None,
            None,
            None,
        );
    }
}
