//! `routes/session/location-missing.tsx`: the composer's question when the session's directory is unavailable,
//! which moves the session to another directory or a new worktree.
//!
//! TODO(port-tui): the dialogs slice: `open()` shows `<DialogWorkspaces projectID location current onSelect>`, whose
//! selection goes to [`select`]. Until it lands, choosing the action does nothing.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::renderables::text::text_node_from_string;
use opencode_tui_core::solid::*;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;

use crate::ctx::{Tui, tui};
use crate::permission::{SessionQuestionProps, session_question};
use crate::util::{abbreviate_home, rgba};

/// `WorkspaceSelection`: an existing directory, or a new worktree by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Directory(String),
    New(String),
}

/// `select(selection)`: creates the worktree when asked, then moves the session there; failures toast.
pub async fn select(t: Tui, project_id: String, session_id: String, selection: Selection) {
    let directory = match selection {
        Selection::Directory(directory) => Some(directory),
        Selection::New(name) => {
            let input = obj! { "projectID" => project_id.as_str(), "name" => name.as_str() };
            let created = t.data.client.call_named("worktree.create", &input, Default::default()).await.map(|r| r.json());
            match created {
                Ok(result) => match result.str("directory").filter(|d| !d.is_empty()) {
                    Some(directory) => Some(directory.to_string()),
                    None => {
                        t.toast.show(
                            crate::toast::ToastInput::new(crate::toast::Variant::Error, "No worktree directory returned")
                                .title("Creating workspace failed"),
                        );
                        None
                    }
                },
                Err(error) => {
                    t.toast.show(
                        crate::toast::ToastInput::new(crate::toast::Variant::Error, error.message()).title("Creating workspace failed"),
                    );
                    None
                }
            }
        }
    };
    let Some(directory) = directory else { return };
    let input = obj! { "sessionID" => session_id.as_str(), "directory" => directory.as_str() };
    if let Err(error) = t.data.client.call_named("session.move", &input, Default::default()).await {
        t.toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Error, error.message()).title("Failed to move session"));
    }
}

/// `<SessionLocationMissing directory projectID sessionID>`.
pub fn session_location_missing(directory: String, project_id: String, session_id: String) -> View {
    let _ = (project_id, session_id);
    // TODO(port-tui): the dialogs slice: `dialog.replace(() => <DialogWorkspaces ... onSelect={select} />)`.
    let open: Rc<dyn Fn()> = Rc::new(|| {});
    session_location_unavailable(directory, open)
}

/// `<SessionLocationUnavailable directory onMove>`.
pub fn session_location_unavailable(directory: String, on_move: Rc<dyn Fn()>) -> View {
    let t = tui();
    let theme = t.theme;
    let shown = crate::ui::locale::truncate_middle(&abbreviate_home(&directory, &t.paths.home), 72);
    session_question(SessionQuestionProps {
        title: "Session location unavailable".into(),
        semantic_label: None,
        instance: directory,
        id: Some("session.location-missing".into()),
        group: Some("Session recovery".into()),
        choices_label: Some("Recovery actions".into()),
        header: None,
        body: Rc::new(move |_| {
            let el = box_();
            el.set_padding(opencode_tui_core::tree::Edge::Left, Some(Value::Point(1.0)));
            el.set_gap(2, Some(Value::Point(1.0)));
            let path = text();
            bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| path.set_fg(Some(c)));
            insert_node(path, text_node_from_string(&shown), None);
            insert_node(el, path, None);
            let hint = text();
            bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| hint.set_fg(Some(c)));
            insert_node(hint, text_node_from_string("Choose another directory to continue this session."), None);
            insert_node(el, hint, None);
            View::Node(el)
        }),
        options: vec![("move", "Choose directory".into())],
        escape_key: None,
        fullscreen: false,
        on_select: Rc::new(move |_| on_move()),
    })
}

/// Whether the location context's error is the session's directory (`currentLocation.error?.location.directory ===
/// session().location.directory`).
pub fn location_missing(error_location: Option<&Json>, session: Option<&Json>) -> bool {
    let Some(session) = session else { return false };
    let directory = session.get("location").and_then(|l| l.str("directory"));
    error_location.is_some_and(|location| location.str("directory") == directory)
}
