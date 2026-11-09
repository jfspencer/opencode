//! Sessions (packages/core/src/session.ts, session/session.ts, session/store.ts, session/projector.ts): every change is
//! a durable session event, projected into `session_v2` in the event's transaction. The transcript and the execution
//! loop live in the submodules: [`inbox`] (admitted input), [`message`] (the transcript), [`execution`] (busy
//! periods), [`runner`] and [`step`] (the agent loop), [`context`] (what a step sends), [`instructions`],
//! [`retry`], [`title`].

pub mod attachment;
pub mod blob;
pub mod command;
pub mod context;
pub mod entries;
pub mod execution;
pub mod inbox;
pub mod instructions;
pub mod message;
pub mod moves;
pub mod plan;
pub mod prompt;
pub mod retry;
pub mod runner;
pub mod skill;
pub mod stats;
pub mod step;
pub mod title;
pub mod transfer;

use std::collections::HashSet;
use std::sync::Mutex;

use base64::Engine;
use opencode_protocol::types::{self as t, SessionInfo};
use opencode_protocol::{Json, Wire};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, params_from_iter};

use crate::app::{App, fatal};
use crate::bus::{Event, LocationRef, PublishOptions};
use crate::http::{Failure, Result};
use crate::json::{self, Obj};

pub const DEFAULT_LIST_LIMIT: i64 = 50;

#[derive(Default)]
pub struct Sessions {
    /// Sessions being removed: background work must not admit input into them.
    removing: Mutex<HashSet<String>>,
    pub executions: execution::Executions,
}

pub fn missing(session_id: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "SessionNotFoundError");
    o.str("sessionID", session_id);
    o.str("message", &format!("Session not found: {session_id}"));
    Failure::new(404, o.done())
}

const COLUMNS: &str = "id, project_id, workspace_id, parent_id, fork_session_id, fork_boundary, directory, path, title, \
    metadata, cost, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write, revert, \
    permission, agent, model, time_created, time_updated, time_busy, time_idle, time_viewed, idle_outcome, time_archived";

/// `Session.Info` of a row (session/info.ts `fromRow`).
fn info_from_row(row: &Row) -> rusqlite::Result<Json> {
    let text = |name: &str| row.get::<_, Option<String>>(name);
    let parsed = |name: &str| -> rusqlite::Result<Option<Json>> { Ok(text(name)?.and_then(|t| json::parse(&t))) };
    let int = |name: &str| row.get::<_, Option<i64>>(name);
    let mut o = Obj::new();
    o.str("id", &row.get::<_, String>("id")?);
    o.opt_str("parentID", text("parent_id")?.as_deref());
    if let (Some(fork), Some(boundary)) = (text("fork_session_id")?, parsed("fork_boundary")?) {
        let mut f = Obj::new();
        f.str("sessionID", &fork).set("boundary", boundary);
        o.set("fork", f.done());
    }
    o.str("projectID", &row.get::<_, String>("project_id")?);
    o.opt_str("agent", text("agent")?.as_deref());
    if let Some(model) = parsed("model")? {
        let mut m = Obj::new();
        m.str("id", json::get_str(&model, "id").unwrap_or_default());
        m.str("providerID", json::get_str(&model, "providerID").unwrap_or_default());
        m.str("variant", json::get_str(&model, "variant").unwrap_or("default"));
        o.set("model", m.done());
    }
    o.set("cost", Json::Number(row.get::<_, f64>("cost")?));
    let mut cache = Obj::new();
    cache.int("read", row.get("tokens_cache_read")?).int("write", row.get("tokens_cache_write")?);
    let mut tokens = Obj::new();
    tokens
        .int("input", row.get("tokens_input")?)
        .int("output", row.get("tokens_output")?)
        .int("reasoning", row.get("tokens_reasoning")?)
        .set("cache", cache.done());
    o.set("tokens", tokens.done());
    o.opt_str("outcome", text("idle_outcome")?.as_deref());
    let mut time = Obj::new();
    time.int("created", row.get("time_created")?).int("updated", row.get("time_updated")?);
    for (key, column) in [("busy", "time_busy"), ("idle", "time_idle"), ("viewed", "time_viewed"), ("archived", "time_archived")] {
        if let Some(value) = int(column)? {
            time.int(key, value);
        }
    }
    o.set("time", time.done());
    o.opt_str("title", text("title")?.as_deref());
    o.opt_str("subpath", text("path")?.filter(|p| !p.is_empty()).as_deref());
    o.opt("metadata", parsed("metadata")?);
    o.opt("permissions", parsed("permission")?);
    // TODO(port): legacy (v1) reverts are upgraded on read (decodeRevert).
    o.opt("revert", parsed("revert")?);
    let mut location = Obj::new();
    location.str("directory", &row.get::<_, String>("directory")?);
    location.opt_str("workspaceID", text("workspace_id")?.as_deref());
    o.set("location", location.done());
    Ok(o.done())
}

/// A stored session: its info and where it lives.
#[derive(Debug, Clone)]
pub struct Stored {
    pub info: Json,
    pub location: LocationRef,
}

impl Stored {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            info: info_from_row(row)?,
            location: LocationRef {
                directory: row.get("directory")?,
                workspace_id: row.get("workspace_id")?,
            },
        })
    }

    pub fn id(&self) -> &str {
        json::get_str(&self.info, "id").unwrap_or_default()
    }

    pub fn time(&self, key: &str) -> Option<i64> {
        json::get(&self.info, "time").and_then(|t| json::get(t, key)).and_then(|v| match v {
            Json::Number(n) => Some(*n as i64),
            _ => None,
        })
    }

    /// The public encoding (`Session.Info` through the response schema).
    pub fn wire(&self) -> Json {
        match SessionInfo::decode(&self.info) {
            Ok(info) => json::of(&info),
            Err(_) => self.info.clone(),
        }
    }
}

pub fn get(db: &Connection, session_id: &str) -> rusqlite::Result<Option<Stored>> {
    db.query_row(&format!("SELECT {COLUMNS} FROM session_v2 WHERE id = ?1"), params![session_id], Stored::from_row)
        .optional()
}

pub fn has_message(db: &Connection, session_id: &str, message_id: &str) -> rusqlite::Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM session_message WHERE id = ?1 AND session_id = ?2",
            params![message_id, session_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub fn require(db: &Connection, session_id: &str) -> Result<Stored> {
    get(db, session_id).map_err(fatal)?.ok_or_else(|| missing(session_id))
}

/// `setSession` (projector.ts): an update that bumps `time_updated` unless the caller pins it.
fn set_session(tx: &Transaction, session_id: &str, assignments: &str, values: Vec<Value>) -> rusqlite::Result<()> {
    let mut values = values;
    values.push(Value::Text(session_id.to_owned()));
    let sql = format!("UPDATE session_v2 SET {assignments} WHERE id = ?{}", values.len());
    tx.execute(&sql, params_from_iter(values))?;
    Ok(())
}

fn json_value(value: Option<&Json>) -> Value {
    match value {
        Some(v) => Value::Text(json::text(v)),
        None => Value::Null,
    }
}

/// The session projector: how each durable session event changes the session's row.
pub fn project(tx: &Transaction, event: &Event) -> rusqlite::Result<()> {
    let data = &event.data;
    let id = json::get_str(data, "sessionID").unwrap_or_default();
    let created = event.created;
    match event.ty {
        "session.created" => {
            let location = json::get(data, "location");
            let inserted = tx.execute(
                "INSERT INTO session_v2 (id, project_id, workspace_id, parent_id, slug, directory, path, title, agent, \
                 model, metadata, permission, version, time_created, time_updated) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14) ON CONFLICT DO NOTHING",
                params![
                    id,
                    json::get_str(data, "projectID"),
                    location.and_then(|l| json::get_str(l, "workspaceID")),
                    json::get_str(data, "parentID"),
                    json::get_str(data, "slug"),
                    location.and_then(|l| json::get_str(l, "directory")),
                    json::get_str(data, "subpath"),
                    json::get_str(data, "title"),
                    json::get_str(data, "agent"),
                    json_value(json::get(data, "model")),
                    json_value(json::get(data, "metadata")),
                    json_value(json::get(data, "permissions")),
                    json::get_str(data, "version"),
                    created,
                ],
            )?;
            if inserted == 0 {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                    Some("session already projected".into()),
                ));
            }
        }
        "session.deleted" => {
            blob::release_session(tx, id)?;
            tx.execute("DELETE FROM session_v2 WHERE id = ?1", params![id])?;
        }
        "session.renamed" => set_session(
            tx,
            id,
            "title = ?1, time_updated = ?2",
            vec![Value::Text(json::get_str(data, "title").unwrap_or_default().into()), Value::Integer(created)],
        )?,
        "session.metadata.updated" => set_session(
            tx,
            id,
            "metadata = ?1, time_updated = ?2",
            vec![json_value(json::get(data, "metadata")), Value::Integer(created)],
        )?,
        "session.permissions" => set_session(
            tx,
            id,
            "permission = ?1, time_updated = ?2",
            vec![json_value(json::get(data, "permissions")), Value::Integer(created)],
        )?,
        "session.viewed" => {
            let idle = match json::get(data, "idle") {
                Some(Json::Number(n)) => *n as i64,
                _ => 0,
            };
            set_session(tx, id, "time_viewed = max(?1, coalesce(time_viewed, ?1))", vec![Value::Integer(idle)])?
        }
        "session.archived" => set_session(
            tx,
            id,
            "time_archived = ?1, time_updated = ?1",
            vec![Value::Integer(created)],
        )?,
        "session.unarchived" => {
            set_session(tx, id, "time_archived = NULL, time_updated = ?1", vec![Value::Integer(created)])?
        }
        "session.forked" => transfer::project_fork(tx, event)?,
        "worktree.resolved" => adopt(tx, data)?,
        "session.inbox.enqueued" => inbox::project_admitted(tx, event)?,
        "session.inbox.delivered" => inbox::project_delivered(tx, event)?,
        "session.inbox.cancelled" => inbox::project_cancelled(tx, event)?,
        "session.inbox.delivery.changed" => inbox::project_delivery_changed(tx, event)?,
        "session.execution.started" => {
            set_session(tx, id, "time_busy = ?1", vec![Value::Integer(created)])?;
            // The execution claim (`SessionStore.claim`).
            tx.execute(
                "UPDATE session_v2 SET time_suspended = ?1 WHERE id = ?2 AND time_suspended IS NULL",
                params![crate::id::now(), id],
            )?;
        }
        "session.execution.succeeded" | "session.execution.failed" | "session.execution.interrupted" => {
            message::project(tx, event)?;
            if event.ty == "session.execution.interrupted" && json::get_str(data, "reason") == Some("shutdown") {
                return Ok(());
            }
            let outcome = match event.ty {
                "session.execution.succeeded" => "succeeded",
                "session.execution.failed" => "failed",
                _ => "interrupted",
            };
            // Every terminal advances the idle time, even within one millisecond.
            set_session(
                tx,
                id,
                "time_idle = max(?1, coalesce(time_idle + 1, ?1)), idle_outcome = ?2",
                vec![Value::Integer(created), Value::Text(outcome.into())],
            )?;
            // Deliberate stops release the claim (`SessionStore.release`).
            tx.execute("UPDATE session_v2 SET time_suspended = NULL, resume_attempts = 0 WHERE id = ?1", params![id])?;
        }
        "session.instructions.updated" => {
            message::project(tx, event)?;
            instructions::apply(tx, event)?;
        }
        "session.step.started" => {
            message::project(tx, event)?;
            set_session(tx, id, "time_updated = ?1", vec![Value::Integer(created)])?;
        }
        "session.step.ended" => {
            message::project(tx, event)?;
            apply_usage(tx, id, data, Some(created))?;
        }
        "session.step.failed" => {
            message::project(tx, event)?;
            if json::get(data, "cost").is_some() && json::get(data, "tokens").is_some() {
                apply_usage(tx, id, data, Some(created))?;
            } else {
                set_session(tx, id, "time_updated = ?1", vec![Value::Integer(created)])?;
            }
        }
        "session.usage.recorded" => apply_usage(tx, id, data, None)?,
        "session.agent.selected" => {
            message::project(tx, event)?;
            set_session(
                tx,
                id,
                "agent = ?1, time_updated = ?2",
                vec![Value::Text(json::get_str(data, "agent").unwrap_or_default().into()), Value::Integer(created)],
            )?;
        }
        "session.model.selected" => {
            message::project(tx, event)?;
            set_session(tx, id, "model = ?1, time_updated = ?2", vec![json_value(json::get(data, "model")), Value::Integer(created)])?;
        }
        "session.moved" => {
            // The message records the location the session leaves, so it is written first.
            message::project(tx, event)?;
            let location = json::get(data, "location");
            let text = |value: Option<&str>| value.map_or(Value::Null, |v| Value::Text(v.to_owned()));
            set_session(
                tx,
                id,
                "directory = ?1, path = ?2, project_id = COALESCE(?3, project_id), workspace_id = ?4, time_updated = ?5",
                vec![
                    text(location.and_then(|l| json::get_str(l, "directory"))),
                    text(json::get_str(data, "subpath")),
                    text(json::get_str(data, "projectID").filter(|p| !p.is_empty())),
                    text(location.and_then(|l| json::get_str(l, "workspaceID"))),
                    Value::Integer(created),
                ],
            )?;
        }
        _ => message::project(tx, event)?,
    }
    Ok(())
}

/// `applyUsage`: adds a step's or a side request's usage to the session totals.
fn apply_usage(tx: &Transaction, id: &str, data: &Json, time_updated: Option<i64>) -> rusqlite::Result<()> {
    let tokens = json::get(data, "tokens");
    let n = |key: &str| tokens.and_then(|t| json::get_num(t, key)).unwrap_or(0.0) as i64;
    let cache = |key: &str| tokens.and_then(|t| json::get(t, "cache")).and_then(|c| json::get_num(c, key)).unwrap_or(0.0) as i64;
    let cost = json::get_num(data, "cost").unwrap_or(0.0);
    let mut sql = String::from(
        "cost = cost + ?1, tokens_input = tokens_input + ?2, tokens_output = tokens_output + ?3, \
         tokens_reasoning = tokens_reasoning + ?4, tokens_cache_read = tokens_cache_read + ?5, \
         tokens_cache_write = tokens_cache_write + ?6",
    );
    let mut values = vec![
        Value::Real(cost),
        Value::Integer(n("input")),
        Value::Integer(n("output")),
        Value::Integer(n("reasoning")),
        Value::Integer(cache("read")),
        Value::Integer(cache("write")),
    ];
    if let Some(time) = time_updated {
        sql.push_str(", time_updated = ?7");
        values.push(Value::Integer(time));
    }
    set_session(tx, id, &sql, values)
}

/// `session.usage.updated`: the session's totals, after the usage a step or a side request added.
fn publish_usage(app: &App, session_id: &str) {
    let row: Option<(f64, i64, i64, i64, i64, i64)> = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT cost, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write \
             FROM session_v2 WHERE id = ?1",
            params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()
        .ok()
        .flatten();
    let Some((cost, input, output, reasoning, read, write)) = row else { return };
    let mut cache = Obj::new();
    cache.int("read", read).int("write", write);
    let mut tokens = Obj::new();
    tokens.int("input", input).int("output", output).int("reasoning", reasoning).set("cache", cache.done());
    let mut data = Obj::new();
    data.str("sessionID", session_id).set("cost", Json::Number(cost)).set("tokens", tokens.done());
    app.bus.emit("session.usage.updated", data.done(), PublishOptions::default());
}

/// Publishes a durable session event and runs the session projector in its transaction; `commit` runs last in the
/// same transaction (TS's `PublishOptions.commit`), and an error from it aborts the publication.
pub fn publish_committed(
    app: &App,
    ty: &'static str,
    data: Json,
    location: Option<LocationRef>,
    metadata: Option<Json>,
    commit: impl FnOnce(&Transaction, &Event) -> rusqlite::Result<()>,
) -> rusqlite::Result<Event> {
    let event = {
        let mut db = app.db.lock().unwrap();
        app.bus.publish_with(&mut db, ty, data, PublishOptions { id: None, location }, metadata, |tx, event| {
            project(tx, event)?;
            commit(tx, event)
        })?
    };
    let session = json::get_str(&event.data, "sessionID").unwrap_or_default();
    let usage = match ty {
        "session.step.ended" | "session.usage.recorded" => true,
        "session.step.failed" => json::get(&event.data, "cost").is_some() && json::get(&event.data, "tokens").is_some(),
        _ => false,
    };
    if usage {
        publish_usage(app, session);
    }
    Ok(event)
}

/// Publishes a durable session event (see [`publish_committed`]), or an ephemeral one.
pub fn publish_event(
    app: &App,
    ty: &'static str,
    data: Json,
    location: Option<LocationRef>,
    metadata: Option<Json>,
) -> rusqlite::Result<Event> {
    if crate::bus::durable_version(ty).is_none() {
        return Ok(app.bus.emit(ty, data, PublishOptions { id: None, location }));
    }
    publish_committed(app, ty, data, location, metadata, |_, _| Ok(()))
}

/// Sessions whose ownership came from the directory's previous resolution follow its new identity; location,
/// transcript and recency are untouched (projector.ts, `Worktree.Event.Resolved`).
fn adopt(tx: &Transaction, data: &Json) -> rusqlite::Result<()> {
    let project_id = json::get_str(data, "projectID").unwrap_or_default();
    let directory = json::get_str(data, "directory").unwrap_or_default();
    let adopted: Vec<String> = match json::get(data, "adopted") {
        Some(Json::Array(items)) => items.iter().filter_map(|i| match i {
            Json::String(s) => Some(s.clone()),
            _ => None,
        }).collect(),
        _ => vec![],
    };
    let mut candidates: Vec<String> = Vec::new();
    for id in [json::get_str(data, "previous").unwrap_or_default(), crate::project::GLOBAL]
        .into_iter()
        .map(str::to_owned)
        .chain(adopted.iter().cloned())
    {
        if id != project_id && !candidates.contains(&id) {
            candidates.push(id);
        }
    }
    if candidates.is_empty() {
        return Ok(());
    }
    let list = |values: &[String]| values.iter().map(|v| format!("'{}'", v.replace('\'', "''"))).collect::<Vec<_>>().join(", ");
    let mut condition = format!("(s.directory >= ?1 AND s.directory <= ?2)");
    if !adopted.is_empty() {
        condition = format!("(s.project_id IN ({}) OR {condition})", list(&adopted));
    }
    let sql = format!(
        "SELECT s.id, s.directory, s.project_id, p.worktree FROM session_v2 s JOIN project p ON s.project_id = p.id \
         WHERE s.project_id IN ({}) AND s.workspace_id IS NULL AND {condition}",
        list(&candidates)
    );
    let rows: Vec<(String, String, String, String)> = {
        let mut statement = tx.prepare(&sql)?;
        statement
            .query_map(params![directory, format!("{directory}\u{ffff}")], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, session_directory, session_project, canonical) in rows {
        let target = if adopted.contains(&session_project) { canonical } else { crate::project::absolutize(&session_directory) };
        if !crate::project::contains(directory, &target) {
            continue;
        }
        // Adoption is not activity: time_updated stays.
        tx.execute(
            "UPDATE session_v2 SET project_id = ?1, path = ?2 WHERE id = ?3",
            params![project_id, relative(directory, &target), id],
        )?;
    }
    Ok(())
}

fn publish(app: &App, ty: &'static str, data: Json, location: Option<LocationRef>) -> Result<Event> {
    let mut db = app.db.lock().unwrap();
    app.bus.publish(&mut db, ty, data, PublishOptions { id: None, location }, project).map_err(fatal)
}

pub struct CreateInput {
    pub id: Option<String>,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub model: Option<Json>,
    pub location: Option<LocationRef>,
    pub metadata: Option<Json>,
    pub permissions: Option<Json>,
}

/// `path.relative(from, to)` for absolute POSIX paths.
pub fn relative(from: &str, to: &str) -> String {
    let from: Vec<&str> = from.split('/').filter(|s| !s.is_empty()).collect();
    let to: Vec<&str> = to.split('/').filter(|s| !s.is_empty()).collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = std::iter::repeat_n("..", from.len() - common).collect();
    parts.extend(&to[common..]);
    parts.join("/")
}

pub fn create(app: &App, input: CreateInput) -> Result<Stored> {
    let session_id = input.id.clone().unwrap_or_else(crate::id::session);
    let (parent, location) = {
        let db = app.db.lock().unwrap();
        if let Some(recorded) = get(&db, &session_id).map_err(fatal)? {
            return Ok(recorded);
        }
        let parent = match &input.parent_id {
            Some(parent_id) => Some(get(&db, parent_id).map_err(fatal)?.ok_or_else(|| missing(parent_id))?),
            None => None,
        };
        let location = parent
            .as_ref()
            .map(|p| p.location.clone())
            .or_else(|| input.location.as_ref().map(crate::location::canonical))
            .expect("Session.create requires either location or an existing parentID");
        (parent, location)
    };
    let project = app.projects.discover(&location.directory);
    app.projects.persist(&mut app.db.lock().unwrap(), &app.bus, &project).map_err(fatal)?;
    let mut data = Obj::new();
    data.str("sessionID", &session_id);
    data.str("slug", &crate::id::slug());
    data.str("version", crate::app::VERSION);
    data.str("projectID", &project.id);
    data.opt_str("parentID", input.parent_id.as_deref());
    data.set("location", location.to_json());
    data.str("subpath", &relative(&project.directory, &location.directory));
    data.opt_str("title", input.title.as_deref());
    data.opt_str("agent", input.agent.as_deref());
    // Children inherit metadata and permissions the way they inherit location.
    data.opt("metadata", input.metadata.or_else(|| parent.as_ref().and_then(|p| json::get(&p.info, "metadata").cloned())));
    data.opt(
        "permissions",
        input.permissions.or_else(|| parent.as_ref().and_then(|p| json::get(&p.info, "permissions").cloned())),
    );
    data.opt("model", input.model);
    match publish(app, "session.created", data.done(), Some(location.clone())) {
        // The location's plugins react to the new session (Plan mode's reminder) once they are running.
        Ok(_) if app.locations.cached(&location).is_some() => plan::created(app, &session_id, input.agent.as_deref(), &location),
        Ok(_) => {}
        // Concurrent creation lost the projection race: the existing session wins.
        Err(failure) => {
            let db = app.db.lock().unwrap();
            return get(&db, &session_id).map_err(fatal)?.ok_or(failure);
        }
    }
    let db = app.db.lock().unwrap();
    require(&db, &session_id)
}

pub fn rename(app: &App, session_id: &str, title: &str) -> Result<()> {
    let session = require(&app.db.lock().unwrap(), session_id)?;
    let mut data = Obj::new();
    data.str("sessionID", session.id()).str("title", title);
    publish(app, "session.renamed", data.done(), None).map(drop)
}

pub fn set_metadata(app: &App, session_id: &str, metadata: Json) -> Result<()> {
    let session = require(&app.db.lock().unwrap(), session_id)?;
    let mut data = Obj::new();
    data.str("sessionID", session.id()).set("metadata", metadata);
    publish(app, "session.metadata.updated", data.done(), None).map(drop)
}

pub fn set_permissions(app: &App, session_id: &str, permissions: Json) -> Result<()> {
    let session = require(&app.db.lock().unwrap(), session_id)?;
    let mut data = Obj::new();
    data.str("sessionID", session.id()).set("permissions", permissions);
    publish(app, "session.permissions", data.done(), None).map(drop)
}

pub fn archive(app: &App, session_id: &str, archived: bool) -> Result<()> {
    let session = require(&app.db.lock().unwrap(), session_id)?;
    if session.time("archived").is_some() == archived {
        return Ok(());
    }
    let mut data = Obj::new();
    data.str("sessionID", session.id());
    publish(app, if archived { "session.archived" } else { "session.unarchived" }, data.done(), None).map(drop)
}

pub fn view(app: &App, session_id: &str, idle: i64) -> Result<()> {
    let session = require(&app.db.lock().unwrap(), session_id)?;
    let Some(last) = session.time("idle") else { return Ok(()) };
    if idle > last || session.time("viewed").is_some_and(|viewed| viewed >= idle) {
        return Ok(());
    }
    let mut data = Obj::new();
    data.str("sessionID", session.id()).int("idle", idle);
    publish(app, "session.viewed", data.done(), None).map(drop)
}

pub fn remove(app: &App, session_id: &str) -> Result<()> {
    let session = require(&app.db.lock().unwrap(), session_id)?;
    app.sessions.removing.lock().unwrap().insert(session_id.to_owned());
    let result = (|| {
        // The handler stops the session's own execution first. TODO(port): interrupt and await descendants'
        // executions, close its model transport.
        crate::job::jobs().cancel_session(app, session_id);
        let children: Vec<String> = {
            let db = app.db.lock().unwrap();
            let mut statement =
                db.prepare("SELECT id FROM session_v2 WHERE parent_id = ?1 ORDER BY time_updated DESC, id DESC").map_err(fatal)?;
            statement
                .query_map(params![session_id], |row| row.get(0))
                .map_err(fatal)?
                .collect::<rusqlite::Result<_>>()
                .map_err(fatal)?
        };
        for child in children {
            remove(app, &child)?;
        }
        // TODO(port): clear the session's environment variables.
        let mut data = Obj::new();
        data.str("sessionID", session.id());
        publish(app, "session.deleted", data.done(), None)?;
        let mut db = app.db.lock().unwrap();
        app.bus.remove(&mut db, session_id).map_err(fatal)
    })();
    app.sessions.removing.lock().unwrap().remove(session_id);
    result
}

/// The filter of a session list (`Session.ListFilter`) and its anchor, as a cursor carries them.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub order: Option<String>,
    pub search: Option<String>,
    /// `Some(None)` is `parentID=null`: root sessions only.
    pub parent_id: Option<Option<String>>,
    pub archived: Option<bool>,
    pub directory: Option<String>,
    pub project: Option<String>,
    pub subpath: Option<String>,
    pub anchor: Option<Anchor>,
}

#[derive(Debug, Clone)]
pub struct Anchor {
    pub id: String,
    pub time: f64,
    pub direction: String,
}

impl ListFilter {
    /// The cursor (`ListCursor.make`): base64url of the JSON of the first union member the filter fits.
    fn cursor(&self, anchor: Anchor) -> String {
        let mut o = Obj::new();
        o.opt_str("order", self.order.as_deref());
        o.opt_str("search", self.search.as_deref());
        match &self.parent_id {
            Some(Some(id)) => {
                o.str("parentID", id);
            }
            Some(None) => {
                o.str("parentID", "null");
            }
            None => {}
        }
        o.opt_str("archived", self.archived.map(|a| if a { "true" } else { "false" }));
        if let Some(directory) = &self.directory {
            o.str("directory", directory);
        } else if let Some(project) = &self.project {
            o.str("project", project);
            o.opt_str("subpath", self.subpath.as_deref());
        }
        let mut a = Obj::new();
        a.str("id", &anchor.id).set("time", Json::Number(anchor.time)).str("direction", &anchor.direction);
        o.set("anchor", a.done());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json::text(&o.done()))
    }

    /// `ListCursor.parse`.
    pub fn parse_cursor(text: &str) -> Option<ListFilter> {
        use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
        const LENIENT: GeneralPurpose = GeneralPurpose::new(
            &base64::alphabet::URL_SAFE,
            GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
        );
        let bytes = LENIENT.decode(text).ok()?;
        let value = json::parse(std::str::from_utf8(&bytes).ok()?)?;
        let object = value.as_object()?;
        let string = |key: &str| -> Option<Option<String>> {
            match object.get(key) {
                None => Some(None),
                Some(Json::String(s)) => Some(Some(s.clone())),
                Some(_) => None,
            }
        };
        let anchor = object.get("anchor")?.as_object()?;
        let id = match anchor.get("id")? {
            Json::String(s) if s.starts_with("ses") => s.clone(),
            _ => return None,
        };
        let time = match anchor.get("time")? {
            Json::Number(n) if n.is_finite() => *n,
            _ => return None,
        };
        let direction = match anchor.get("direction")? {
            Json::String(s) if s == "previous" || s == "next" => s.clone(),
            _ => return None,
        };
        let order = string("order")?;
        if order.as_deref().is_some_and(|o| o != "asc" && o != "desc") {
            return None;
        }
        let parent_id = match string("parentID")? {
            None => None,
            Some(value) if value == "null" => Some(None),
            Some(value) if value.starts_with("ses") => Some(Some(value)),
            Some(_) => return None,
        };
        let archived = match string("archived")? {
            None => None,
            Some(value) if value == "true" => Some(true),
            Some(value) if value == "false" => Some(false),
            Some(_) => return None,
        };
        let mut filter = ListFilter {
            order,
            search: string("search")?,
            parent_id,
            archived,
            anchor: Some(Anchor { id, time, direction }),
            ..Default::default()
        };
        // The first union member that decodes: a directory, a project (with an optional subpath), or neither.
        match (string("directory")?, string("project")?) {
            (Some(directory), _) => filter.directory = Some(directory),
            (None, Some(project)) => {
                filter.project = Some(project);
                filter.subpath = string("subpath")?;
            }
            (None, None) => {}
        }
        Some(filter)
    }
}

pub struct ListPage {
    pub data: Vec<Stored>,
    pub previous: Option<String>,
    pub next: Option<String>,
}

pub fn list(app: &App, filter: ListFilter, limit: Option<i64>) -> Result<ListPage> {
    let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT);
    let direction = filter.anchor.as_ref().map(|a| a.direction.as_str()).unwrap_or("next");
    let requested = filter.order.as_deref().unwrap_or("desc");
    let order = if direction == "previous" { if requested == "asc" { "desc" } else { "asc" } } else { requested };
    let mut conditions: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    let bind = |values: &mut Vec<Value>, value: Value| {
        values.push(value);
        format!("?{}", values.len())
    };
    if let Some(directory) = &filter.directory {
        conditions.push(format!("directory = {}", bind(&mut values, Value::Text(directory.clone()))));
    }
    if let Some(project) = &filter.project {
        conditions.push(format!("project_id = {}", bind(&mut values, Value::Text(project.clone()))));
        if let Some(subpath) = &filter.subpath {
            conditions.push(format!("path = {}", bind(&mut values, Value::Text(subpath.clone()))));
        }
    }
    if let Some(search) = filter.search.as_ref().filter(|s| !s.is_empty()) {
        conditions.push(format!("title LIKE {}", bind(&mut values, Value::Text(format!("%{search}%")))));
    }
    match &filter.parent_id {
        Some(None) => conditions.push("parent_id IS NULL".into()),
        Some(Some(parent)) => conditions.push(format!("parent_id = {}", bind(&mut values, Value::Text(parent.clone())))),
        None => {}
    }
    match filter.archived {
        Some(true) => conditions.push("time_archived IS NOT NULL".into()),
        Some(false) => conditions.push("time_archived IS NULL".into()),
        None => {}
    }
    if let Some(anchor) = &filter.anchor {
        let time = bind(&mut values, Value::Real(anchor.time));
        let id = bind(&mut values, Value::Text(anchor.id.clone()));
        let op = if order == "asc" { ">" } else { "<" };
        conditions.push(format!("(time_updated {op} {time} OR (time_updated = {time} AND id {op} {id}))"));
    }
    let direction_sql = if order == "asc" { "ASC" } else { "DESC" };
    let mut sql = format!("SELECT {COLUMNS} FROM session_v2");
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(&format!(" ORDER BY time_updated {direction_sql}, id {direction_sql} LIMIT {limit}"));
    let mut data: Vec<Stored> = {
        let db = app.db.lock().unwrap();
        let mut statement = db.prepare(&sql).map_err(fatal)?;
        statement
            .query_map(params_from_iter(values), Stored::from_row)
            .map_err(fatal)?
            .collect::<rusqlite::Result<_>>()
            .map_err(fatal)?
    };
    if direction == "previous" {
        data.reverse();
    }
    let anchor = |session: &Stored, direction: &str| Anchor {
        id: session.id().to_owned(),
        time: session.time("updated").unwrap_or(0) as f64,
        direction: direction.to_owned(),
    };
    let base = ListFilter { anchor: None, ..filter };
    Ok(ListPage {
        previous: data.first().map(|s| base.cursor(anchor(s, "previous"))),
        next: data.last().map(|s| base.cursor(anchor(s, "next"))),
        data,
    })
}

/// The `parentID` filter of a list query: a session ID, or `null` for root sessions.
pub fn query_parent(value: &t::SessionsQueryParentID0) -> Option<String> {
    match value {
        t::SessionsQueryParentID0::SessionID(id) => Some(id.0.clone()),
        t::SessionsQueryParentID0::Null(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(relative("/a/b", "/a/b"), "");
        assert_eq!(relative("/a/b", "/a/b/c/d"), "c/d");
        assert_eq!(relative("/a/b", "/a/x"), "../x");
    }

    #[test]
    fn cursors_round_trip() {
        let filter = ListFilter {
            order: Some("asc".into()),
            parent_id: Some(None),
            archived: Some(false),
            project: Some("p".into()),
            subpath: Some("pkg".into()),
            ..Default::default()
        };
        let cursor = filter.cursor(Anchor { id: "ses_1".into(), time: 5.0, direction: "next".into() });
        let parsed = ListFilter::parse_cursor(&cursor).unwrap();
        assert_eq!(parsed.order.as_deref(), Some("asc"));
        assert_eq!(parsed.parent_id, Some(None));
        assert_eq!(parsed.archived, Some(false));
        assert_eq!(parsed.project.as_deref(), Some("p"));
        assert_eq!(parsed.subpath.as_deref(), Some("pkg"));
        assert_eq!(parsed.anchor.unwrap().id, "ses_1");
        assert!(ListFilter::parse_cursor("not-a-cursor").is_none());
    }
}
