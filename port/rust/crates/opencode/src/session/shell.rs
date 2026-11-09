//! Shell commands a user runs in a session (packages/core/src/session/shell.ts and `Session.shell` in
//! session/session.ts), the session's environment variables (session/environment.ts), and the projection of the
//! session's shell messages (`session.shell.started` / `ended` in message-updater.ts).
//!
//! A user's command runs in the session's location with no timeout. Its start and end are durable session events (the
//! `shell` message), and its outcome is left in the inbox as a synthetic notice that does not wake the session; the
//! model sees it with the next prompt. The server records the completion even when the client goes away.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use opencode_protocol::Json;
use rusqlite::{OptionalExtension, Transaction, params};

use crate::app::{App, fatal};
use crate::bus::{Event, PublishOptions};
use crate::http::{Failure, Result};
use crate::json::{self, Obj};
use crate::session;
use crate::shell::{self, CreateError, CreateInput};

static ENVIRONMENTS: Mutex<Option<HashMap<String, IndexMap<String, String>>>> = Mutex::new(None);

/// `SessionEnvironment.get`: the variables that replace the process environment of the session's shell commands.
pub fn environment(session_id: &str) -> Option<IndexMap<String, String>> {
    ENVIRONMENTS.lock().unwrap().as_ref().and_then(|map| map.get(session_id).cloned())
}

/// `SessionEnvironment.set`.
pub fn set_environment(session_id: &str, variables: IndexMap<String, String>) {
    ENVIRONMENTS.lock().unwrap().get_or_insert_with(HashMap::new).insert(session_id.to_owned(), variables);
}

/// `SessionEnvironment.clear`.
pub fn clear_environment(session_id: &str) {
    if let Some(map) = ENVIRONMENTS.lock().unwrap().as_mut() {
        map.remove(session_id);
    }
}

/// The environment a command created with `metadata` starts from (`Shell.create`): none for a workspace-placed
/// location, else its session's environment when the metadata names a session that has one, else the server's.
pub fn base_environment(workspace: bool, metadata: Option<&Json>) -> Option<IndexMap<String, String>> {
    if workspace {
        return None;
    }
    let session = metadata.and_then(|m| json::get_str(m, "sessionID")).filter(|id| id.starts_with("ses"));
    if let Some(variables) = session.and_then(environment) {
        return Some(variables);
    }
    Some(std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect())
}

/// `{tool_output}`'s line and byte limits (`Config.latest`), with `ToolOutput`'s defaults.
pub fn output_limits(inventory: &crate::inventory::Inventory) -> (u64, u64) {
    let limits = inventory.latest(|c| c.tool_output.as_ref());
    let lines = limits.and_then(|l| l.max_lines).map(|v| v.max(0) as u64);
    let bytes = limits.and_then(|l| l.max_bytes).map(|v| v.max(0) as u64);
    (
        lines.unwrap_or(crate::tool::output::MAX_LINES as u64),
        bytes.unwrap_or(crate::tool::output::MAX_BYTES as u64),
    )
}

/// Why a synthetic notice was not admitted (`Session.synthetic`'s errors).
#[derive(Debug)]
pub enum SyntheticError {
    NotFound,
    Conflict(String),
}

/// `Session.synthetic`: admits a synthetic input; unless `resume` is false (or a revert is staged) it wakes the session.
/// Its event carries the location of the services that admitted it, if any.
#[allow(clippy::too_many_arguments)]
pub fn synthetic(
    app: &App,
    location: Option<crate::bus::LocationRef>,
    session_id: &str,
    id: Option<String>,
    text: &str,
    description: Option<&str>,
    metadata: Option<Json>,
    resume: bool,
) -> std::result::Result<Json, SyntheticError> {
    let exists = session::get(&app.db.lock().unwrap(), session_id).ok().flatten().is_some();
    if !exists || app.sessions.is_removing(session_id) {
        return Err(SyntheticError::NotFound);
    }
    let input_id = id.unwrap_or_else(session::message::create_id);
    let mut payload = Obj::new();
    payload.str("text", text);
    payload.opt_str("description", description);
    payload.opt("metadata", metadata);
    let mut item = Obj::new();
    item.str("type", "synthetic").set("payload", payload.done()).str("delivery", "steer");
    let admitted = match session::inbox::admit(app, &input_id, session_id, item.done(), location) {
        Ok(Ok(admitted)) => admitted,
        Ok(Err(_)) => return Err(SyntheticError::Conflict(input_id)),
        Err(_) => return Err(SyntheticError::NotFound),
    };
    let reverted = session::get(&app.db.lock().unwrap(), session_id)
        .ok()
        .flatten()
        .is_some_and(|s| json::get(&s.info, "revert").is_some_and(|r| !matches!(r, Json::Null)));
    if resume && !reverted {
        app.sessions.executions.wake(app, session_id, session::inbox::Promotable::Input);
    }
    Ok(admitted)
}

/// `startFailure` (server/src/handlers/shell-error.ts): what the API answers for a command that could not start.
pub fn start_failure(error: &shell::StartError, cwd: &str) -> Failure {
    if error.cause.bad_argument() {
        return Failure::invalid("The command contains a NUL character", Some("command"), Some("invalid_command"));
    }
    if error.cause.permission_denied() {
        return Failure::invalid(format!("cwd is not accessible: {cwd}"), Some("cwd"), Some("invalid_cwd"));
    }
    if error.cause.too_long() {
        return Failure::invalid("The command is too long to run", Some("command"), Some("invalid_command"));
    }
    let reference = format!("err_{}", &crate::id::uuid()[..8]);
    eprintln!("failed to start shell command ({reference}): {}", error.message());
    let mut o = Obj::new();
    o.str("_tag", "UnknownError").str("message", "Unexpected server error. Check server logs for details.");
    o.str("ref", &reference);
    Failure::new(500, o.done())
}

/// `Session.shell`: runs `command` in the session's location and records it; resolves once the command has ended.
pub async fn run(app: &Arc<App>, session_id: &str, id: Option<String>, command: &str) -> Result<()> {
    let stored = session::require(&app.db.lock().unwrap(), session_id)?;
    let location = app.locations.get(app, &stored.location).await?;
    let app = app.clone();
    let session_id = session_id.to_owned();
    let command = command.to_owned();
    // The server owns completion recording even if the submitting client disconnects.
    let task = tokio::spawn(async move {
        let mut metadata = Obj::new();
        metadata.str("sessionID", &session_id).set("background", Json::Bool(true));
        let metadata = metadata.done();
        let inventory = location.inventory();
        let base = base_environment(location.workspace_id.is_some(), Some(&metadata));
        let input = CreateInput {
            command: command.clone(),
            cwd: Some(location.directory.clone()),
            timeout: Some(0),
            shell: None,
            metadata: Some(metadata),
        };
        let default_shell = || shell::select::resolve(shell::select::Priority::Config, &inventory);
        let started = location
            .shell
            .create(input, base, &location.directory, default_shell, |invocation| async { Ok::<_, ()>(invocation) })
            .await;
        let info = match started {
            Ok(info) => info,
            Err(CreateError::Before(())) => unreachable!("no preparation"),
            Err(CreateError::Start(error)) => {
                let text = format!("User shell command failed to start:\n{command}\n\n{}", error.message());
                let mut meta = Obj::new();
                meta.str("source", "shell").str("state", "error");
                let _ = synthetic(&app, Some(location.reference()), &session_id, None, &text, Some(&command), Some(meta.done()), false);
                return Err(start_failure(&error, &location.directory));
            }
        };
        let mut data = Obj::new();
        data.str("sessionID", &session_id).set("shell", info.to_json());
        let event_id = id.map(|id| match id.strip_prefix("msg_") {
            Some(rest) => format!("evt_{rest}"),
            None => id,
        });
        publish(&app, "session.shell.started", data.done(), event_id, &location)?;
        let (max_lines, max_bytes) = output_limits(&inventory);
        let terminal = location.shell.result(&info, max_lines, max_bytes).await;
        let preview = match location.shell.output(&info.id, None, Some(1024 * 1024)).await {
            Ok(page) => page,
            Err(_) => opencode_shell::result::unavailable(),
        };
        let mut data = Obj::new();
        data.str("sessionID", &session_id).set("shell", terminal.info.to_json()).set("output", page_json(&preview));
        publish(&app, "session.shell.ended", data.done(), None, &location)?;
        let notice = opencode_shell::result::user_notification(
            &terminal.info.id,
            &terminal.info.command,
            &terminal.info.result_info(),
            terminal.capture.as_ref(),
        );
        let metadata = notification_metadata(&notice);
        let _ = synthetic(&app, Some(location.reference()), &session_id, None, &notice.text, None, Some(metadata), false);
        Ok(())
    });
    task.await.unwrap_or_else(|_| Err(Failure::defect()))
}

fn publish(app: &App, ty: &'static str, data: Json, id: Option<String>, location: &crate::location::Location) -> Result<()> {
    let mut db = app.db.lock().unwrap();
    app.bus
        .publish(&mut db, ty, data, PublishOptions { id, location: Some(location.reference()) }, session::project)
        .map(drop)
        .map_err(fatal)
}

/// `Shell.Output` on the wire.
pub fn page_json(page: &shell::Page) -> Json {
    let mut o = Obj::new();
    o.str("output", &page.output).int("cursor", page.cursor as i64).int("size", page.size as i64);
    o.set("truncated", Json::Bool(page.truncated));
    o.done()
}

/// A notification's metadata: `{source: "shell", shellID, jobID?, state, truncated?, exit?, signal?, timeout?}`.
pub fn notification_metadata(notice: &opencode_shell::result::Notification) -> Json {
    let mut o = Obj::new();
    o.str("source", "shell").str("shellID", &notice.shell_id);
    o.opt_str("jobID", notice.job_id.as_deref());
    o.str("state", notice.state.as_str());
    if let Some(output) = &notice.output {
        o.set("truncated", Json::Bool(output.truncated));
        if let Some(exit) = output.exit {
            o.int("exit", exit);
        }
        o.opt_str("signal", output.signal.as_deref());
        if let Some(timeout) = output.timeout {
            o.set("timeout", Json::Bool(timeout));
        }
    }
    o.done()
}

/// The shell message projection (`session.shell.started`, `session.shell.ended`).
pub fn project(tx: &Transaction, event: &Event) -> rusqlite::Result<()> {
    let data = &event.data;
    let session = json::get_str(data, "sessionID").unwrap_or_default();
    let shell = json::get(data, "shell").cloned().unwrap_or(Json::Null);
    let shell_id = json::get_str(&shell, "id").unwrap_or_default().to_owned();
    match event.ty {
        "session.shell.started" => {
            let background = json::get(&shell, "metadata").and_then(|m| json::get(m, "background")) == Some(&Json::Bool(true));
            let metadata = match (&event.metadata, background) {
                (Some(Json::Object(fields)), true) => {
                    let mut m = Json::Object(fields.clone());
                    json::set(&mut m, "background", Json::Bool(true));
                    Some(m)
                }
                (_, true) => {
                    let mut m = Obj::new();
                    m.set("background", Json::Bool(true));
                    Some(m.done())
                }
                (metadata, false) => metadata.clone(),
            };
            let mut time = Obj::new();
            time.int("created", event.created);
            let mut o = Obj::new();
            o.str("id", &session::message::id_from_event(&event.id));
            o.opt("metadata", metadata);
            o.str("type", "shell").str("shellID", &shell_id);
            o.str("command", json::get_str(&shell, "command").unwrap_or_default());
            o.str("status", json::get_str(&shell, "status").unwrap_or("running"));
            o.set("time", time.done());
            session::message::insert(tx, event, &o.done())
        }
        "session.shell.ended" => {
            let row: Option<(String, String)> = tx
                .query_row(
                    "SELECT id, data FROM session_message WHERE session_id = ?1 AND type = 'shell' \
                     AND json_extract(data, '$.shellID') = ?2 ORDER BY seq DESC LIMIT 1",
                    params![session, shell_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((id, text)) = row else { return Ok(()) };
            let mut message = session::message::from_row(id, "shell".into(), &text);
            json::set(&mut message, "status", json::get(&shell, "status").cloned().unwrap_or(Json::Null));
            match json::get(&shell, "exit") {
                Some(exit) => json::set(&mut message, "exit", exit.clone()),
                None => {
                    json::remove(&mut message, "exit");
                }
            }
            json::set(&mut message, "output", json::get(data, "output").cloned().unwrap_or(Json::Null));
            if let Some(time) = json::get_mut(&mut message, "time") {
                json::set(time, "completed", Json::Number(event.created as f64));
            }
            session::message::update(tx, session, &message)
        }
        _ => Ok(()),
    }
}
