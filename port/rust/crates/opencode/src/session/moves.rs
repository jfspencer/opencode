//! Moving a session to another directory (packages/core/src/session/move.ts, the runner's move boundary in
//! runner/llm.ts): the destination is checked and booted, then a `move` item waits in the inbox for the next boundary,
//! where `session.moved` switches the session's location (a `location-switched` message tells the model) and the
//! turn continues there. A session whose own location cannot run (its directory is gone, its config is broken) and
//! that is idle moves at once.

use opencode_protocol::Json;

use crate::app::{App, fatal};
use crate::bus::LocationRef;
use crate::http::{Failure, Result};
use crate::json::{self, Obj};
use crate::session::{self, inbox};

fn home() -> String {
    std::env::var("OPENCODE_TEST_HOME").or_else(|_| std::env::var("HOME")).unwrap_or_default()
}

/// `resolveDestination`: the move's payload (`MovePayload`), or why the directory cannot take the session.
async fn destination(app: &App, session: &session::Stored, input: &Json) -> Result<Json> {
    let value = json::get_str(input, "directory").unwrap_or_default().trim();
    let expanded = match value {
        "~" => home(),
        _ => match value.strip_prefix("~/") {
            Some(rest) => format!("{}/{rest}", home()),
            None => value.to_owned(),
        },
    };
    let directory = crate::fs::resolve(&session.location.directory, &expanded);
    let invalid = |message: String| Failure::invalid(message, None, None);
    let Ok(metadata) = std::fs::metadata(&directory) else {
        return Err(invalid(format!("Directory does not exist: {directory}")));
    };
    if !metadata.is_dir() {
        return Err(invalid(format!("Not a directory: {directory}")));
    }
    let resolved = {
        let target = directory.clone();
        tokio::task::spawn_blocking(move || crate::project::discover(&target)).await.expect("project discovery")
    };
    {
        let mut db = app.db.lock().unwrap();
        app.projects.persist(&mut db, &app.bus, &resolved).map_err(fatal)?;
    }
    let workspace = json::get_str(input, "workspaceID").map(str::to_owned);
    let reference = LocationRef { directory: directory.clone(), workspace_id: workspace.clone() };
    if app.locations.get(app, &reference).await.is_err() {
        return Err(invalid(format!("Directory is unavailable: {directory}")));
    }
    let mut location = Obj::new();
    location.str("directory", &directory);
    location.opt_str("workspaceID", workspace.as_deref());
    let mut payload = Obj::new();
    payload.set("location", location.done()).str("projectID", &resolved.id);
    payload.str("subpath", &session::relative(&resolved.directory, &directory));
    Ok(payload.done())
}

/// `sourceUnavailable`: whether the session's own location cannot run it (an idle session only).
async fn source_unavailable(app: &App, session: &session::Stored, session_id: &str) -> bool {
    if app.sessions.executions.is_active(session_id) {
        return false;
    }
    if !std::path::Path::new(&session.location.directory).is_dir() {
        return true;
    }
    app.locations.get(app, &session.location).await.is_err()
}

fn moved(session_id: &str, payload: &Json) -> Json {
    let mut data = Obj::new();
    data.str("sessionID", session_id);
    if let Json::Object(fields) = payload {
        for (key, value) in fields {
            data.set(key, value.clone());
        }
    }
    data.done()
}

/// `SessionMove.move` (`POST /session/:id/move`).
pub async fn request(app: &App, session_id: &str, input: &Json) -> Result<()> {
    let stored = session::require(&app.db.lock().unwrap(), session_id)?;
    let payload = destination(app, &stored, input).await?;
    let unavailable = source_unavailable(app, &stored, session_id).await;
    let latest = session::require(&app.db.lock().unwrap(), session_id)?;
    if unavailable && latest.location == stored.location && !app.sessions.executions.is_active(session_id) {
        // Nothing can run where the session is: it moves now, and earlier pending moves are dropped.
        let pending = inbox::move_ids(&app.db.lock().unwrap(), session_id).map_err(fatal)?;
        for id in pending {
            let mut data = Obj::new();
            data.str("sessionID", session_id).str("inboxID", &id);
            session::publish_event(app, "session.inbox.cancelled", data.done(), None, None).map_err(fatal)?;
        }
        session::publish_event(app, "session.moved", moved(session_id, &payload), None, None).map_err(fatal)?;
    } else {
        let delivery = json::get_str(input, "delivery").unwrap_or("steer");
        let mut item = Obj::new();
        item.str("type", "move").set("payload", payload).str("delivery", delivery);
        let id = session::message::create_id();
        if inbox::admit(app, &id, session_id, item.done(), None)?.is_err() {
            return Err(fatal(rusqlite::Error::QueryReturnedNoRows));
        }
    }
    app.sessions.executions.wake(app, session_id, inbox::Promotable::Input);
    Ok(())
}

/// The runner's move boundary: the item is delivered and the session moves, in the location it leaves.
pub fn deliver(app: &App, session_id: &str, pending: &inbox::Promotion) -> Result<()> {
    let location = session::require(&app.db.lock().unwrap(), session_id)?.location;
    let mut data = Obj::new();
    data.str("sessionID", session_id).str("inboxID", &pending.id);
    session::publish_event(app, "session.inbox.delivered", data.done(), Some(location.clone()), None).map_err(fatal)?;
    let payload = pending.payload.clone().unwrap_or_else(json::object);
    session::publish_event(app, "session.moved", moved(session_id, &payload), Some(location), None).map_err(fatal)?;
    Ok(())
}
