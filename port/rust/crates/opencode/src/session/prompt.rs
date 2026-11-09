//! Prompting a session (packages/core/src/session/session.ts `prompt`, `switchAgent`, `switchModel`, and
//! session/prompt.ts `prepare`): input is prepared (file attachments read inline, skill attachments given their
//! content), admitted into the inbox (idempotently by ID), and the session is woken unless `resume` is `false`.
//!
//! The `session.prompt` plugin hook is out of scope: no built-in plugin registers it, only external plugins do.

use opencode_protocol::Json;

use crate::app::{App, fatal};
use crate::bus::LocationRef;
use crate::http::{Failure, Result};
use crate::json::{self, Obj};
use crate::session::{self, inbox};

pub fn conflict(message: String, resource: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "ConflictError").str("message", &message).str("resource", resource);
    Failure::new(409, o.done())
}

/// `Session.prompt`: `input` has the prompt payload's shape (`text`, `id?`, `agents?`, `metadata?`, `delivery?`,
/// `resume?`). Returns the admitted inbox item.
pub async fn submit(app: &App, session_id: &str, input: &Json) -> Result<Json> {
    let stored = session::require(&app.db.lock().unwrap(), session_id)?;
    let present = |key: &str| json::get(input, key).filter(|v| !matches!(v, Json::Null)).cloned();
    let message_id = json::get_str(input, "id").map(str::to_owned).unwrap_or_else(session::message::create_id);
    let delivery = json::get_str(input, "delivery").unwrap_or("steer").to_owned();
    let conflicted = || conflict(format!("Prompt message ID conflicts with an existing durable record: {message_id}"), &message_id);
    let existing = inbox::reconcile(&app.db.lock().unwrap(), &message_id, session_id, "user", &delivery).map_err(fatal)?;
    let admitted = match existing {
        Err(_) => return Err(conflicted()),
        Ok(Some(existing)) => existing,
        Ok(None) => {
            // Preparation runs in the session's location (its plugins hook the prompt, its skills are attached).
            let location = app.locations.get(app, &stored.location).await?;
            let files = match present("files") {
                Some(Json::Array(files)) => {
                    let mut materialized = Vec::with_capacity(files.len());
                    for file in &files {
                        let file = session::attachment::materialize(&app.db, file).await;
                        materialized.push(file.map_err(|message| Failure::invalid(message, Some("files"), None))?);
                    }
                    Some(Json::Array(materialized))
                }
                _ => None,
            };
            let skills = match present("skills") {
                Some(Json::Array(requested)) if !requested.is_empty() => {
                    let inventory = location.inventory();
                    let prepared = session::skill::attachments(&inventory.skills, &requested)
                        .map_err(|id| Failure::invalid(format!("Skill not found: {id}"), Some("skills"), None))?;
                    Some(Json::Array(prepared))
                }
                _ => None,
            };
            let mut payload = Obj::new();
            payload.str("text", json::get_str(input, "text").unwrap_or_default());
            payload.opt("files", files);
            payload.opt("agents", present("agents"));
            payload.opt("skills", skills);
            payload.opt("metadata", present("metadata"));
            let mut item = Obj::new();
            item.str("type", "user").set("payload", payload.done()).str("delivery", &delivery);
            // Commit a staged revert only after preparation succeeds, before admitting new work.
            session::revert::commit(app, &stored, Some(stored.location.clone()))?;
            match inbox::admit(app, &message_id, session_id, item.done(), Some(stored.location.clone()))? {
                Ok(admitted) => admitted,
                Err(_) => return Err(conflicted()),
            }
        }
    };
    if json::get_bool(input, "resume") != Some(false) {
        app.sessions.executions.wake(app, session_id, inbox::Promotable::Input);
    }
    Ok(admitted)
}

/// `Session.switchAgent` (the agent is not checked here; callers validate it). `location` is the location the switch
/// is published in: a command's, or none for the HTTP endpoint.
pub fn switch_agent(app: &App, session_id: &str, agent: &str, location: Option<LocationRef>) -> Result<()> {
    let stored = session::require(&app.db.lock().unwrap(), session_id)?;
    let previous = json::get_str(&stored.info, "agent").map(str::to_owned);
    let mut data = Obj::new();
    data.str("sessionID", session_id).str("agent", agent);
    data.opt_str("previous", previous.as_deref());
    session::publish_event(app, "session.agent.selected", data.done(), location, None).map_err(fatal)?;
    // The location's plugins react to the switch (Plan mode's reminders) once they are running.
    if app.locations.cached(&stored.location).is_some() {
        session::plan::selected(app, session_id, agent, previous.as_deref(), &stored.location);
    }
    Ok(())
}

/// `Session.switchModel`: a no-op when the model and variant (`default` when unset) are unchanged.
pub fn switch_model(app: &App, session_id: &str, model: Json, location: Option<LocationRef>) -> Result<()> {
    let stored = session::require(&app.db.lock().unwrap(), session_id)?;
    let current = json::get(&stored.info, "model");
    let field = |m: Option<&Json>, key: &str| m.and_then(|m| json::get_str(m, key)).map(str::to_owned);
    let same = field(current, "providerID") == field(Some(&model), "providerID")
        && field(current, "id") == field(Some(&model), "id")
        && field(current, "variant").unwrap_or_else(|| "default".into())
            == field(Some(&model), "variant").unwrap_or_else(|| "default".into());
    if same {
        return Ok(());
    }
    let mut data = Obj::new();
    data.str("sessionID", session_id).set("model", model);
    data.opt("previous", current.cloned());
    session::publish_event(app, "session.model.selected", data.done(), location, None).map_err(fatal)?;
    Ok(())
}
