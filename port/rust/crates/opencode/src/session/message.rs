//! Session messages (packages/core/src/session/message-updater.ts, the message half of session/projector.ts and
//! session/store.ts's message reads): each durable session event that changes the transcript is folded into
//! `session_message` in the event's transaction.
//!
//! Messages are held as JSON in their stored (plain-encoded) shape, `SessionMessage.Info` without `id` and `type`
//! (which have their own columns); every write goes through the generated `SessionMessageInfo` so the stored text is
//! exactly what `Schema.encodeSync(SessionMessage.Info)` writes. The write path is the one TS uses with
//! `experimental.part_rows` off: assistant parts stay inline in `content`. TODO(port): the part-row write path, media
//! blobs (`SessionBlob.externalize`), legacy row upgrades (`SessionLegacy.message`).

use opencode_protocol::types as t;
use opencode_protocol::{Json, Wire};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::bus::Event;
use crate::json::{self, Obj};

/// A message ID from an event ID (`SessionMessage.ID.fromEvent`).
pub fn id_from_event(event_id: &str) -> String {
    match event_id.strip_prefix("evt_") {
        Some(rest) => format!("msg_{rest}"),
        None => event_id.to_owned(),
    }
}

pub fn create_id() -> String {
    format!("msg_{}", crate::id::ascending())
}

/// The canonical stored form of a message (`encodeSync(SessionMessage.Info)`), or the value unchanged when it does not
/// decode (which would be a bug in the projector).
fn canonical(message: &Json) -> Json {
    match t::SessionMessageInfo::decode(message) {
        Ok(info) => json::of(&info),
        Err(issue) => {
            eprintln!("session message does not decode: {issue}");
            message.clone()
        }
    }
}

/// `{id, type, ...data}` of a row.
pub fn from_row(id: String, ty: String, data: &str) -> Json {
    let mut o = Obj::new();
    o.str("id", &id).str("type", &ty);
    let mut message = o.done();
    if let Some(Json::Object(fields)) = json::parse(data) {
        for (key, value) in fields {
            json::set(&mut message, &key, value);
        }
    }
    message
}

/// The public encoding of a stored message (`SessionMessage.Info` through the response schema).
pub fn wire(message: &Json) -> Json {
    match t::SessionMessageInfo2::decode(message) {
        Ok(info) => json::of(&info),
        Err(_) => message.clone(),
    }
}

fn data_text(message: &Json) -> (String, String, i64) {
    let canonical = canonical(message);
    let id = json::get_str(&canonical, "id").unwrap_or_default().to_owned();
    let ty = json::get_str(&canonical, "type").unwrap_or_default().to_owned();
    let created = json::get(&canonical, "time").and_then(|t| json::get_num(t, "created")).unwrap_or(0.0) as i64;
    let mut data = canonical;
    json::remove(&mut data, "id");
    json::remove(&mut data, "type");
    let _ = id;
    (ty, json::text(&data), created)
}

/// `insertMessage`: a new row at the event's sequence.
pub fn insert(tx: &Transaction, event: &Event, message: &Json) -> rusqlite::Result<()> {
    let id = json::get_str(message, "id").unwrap_or_default().to_owned();
    let session = json::get_str(&event.data, "sessionID").unwrap_or_default();
    let seq = event.durable.as_ref().map(|d| d.seq).unwrap_or(0);
    let (ty, data, created) = data_text(message);
    tx.execute(
        "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, session, ty, seq, created, crate::id::now(), data],
    )?;
    Ok(())
}

/// `updateMessage`: rewrites a message's row.
pub fn update(tx: &Transaction, session: &str, message: &Json) -> rusqlite::Result<()> {
    let id = json::get_str(message, "id").unwrap_or_default().to_owned();
    let (ty, data, created) = data_text(message);
    tx.execute(
        "UPDATE session_message SET type = ?1, time_created = ?2, time_updated = ?3, data = ?4 WHERE id = ?5 AND session_id = ?6",
        params![ty, created, crate::id::now(), data, id, session],
    )?;
    Ok(())
}

fn row(db: &Connection, sql: &str, args: impl rusqlite::Params) -> rusqlite::Result<Option<Json>> {
    db.query_row(sql, args, |row| Ok(from_row(row.get(0)?, row.get(1)?, &row.get::<_, String>(2)?))).optional()
}

pub fn get_assistant(db: &Connection, session: &str, message_id: &str) -> rusqlite::Result<Option<Json>> {
    row(
        db,
        "SELECT id, type, data FROM session_message WHERE id = ?1 AND session_id = ?2 AND type = 'assistant'",
        params![message_id, session],
    )
}

fn completed(message: &Json) -> bool {
    json::get(message, "time").and_then(|t| json::get(t, "completed")).is_some()
}

fn retrying(message: &Json) -> bool {
    json::get(message, "retry").is_some_and(|r| !matches!(r, Json::Null))
}

/// `getCurrentAssistant`: the newest assistant, while it is incomplete or its failed step waits for a retry.
pub fn current_assistant(db: &Connection, session: &str) -> rusqlite::Result<Option<Json>> {
    let latest = row(
        db,
        "SELECT id, type, data FROM session_message WHERE session_id = ?1 AND type = 'assistant' ORDER BY seq DESC LIMIT 1",
        params![session],
    )?;
    Ok(latest.filter(|m| !completed(m) || retrying(m)))
}

fn time_mut(message: &mut Json) -> &mut Json {
    if json::get(message, "time").is_none() {
        json::set(message, "time", json::object());
    }
    json::get_mut(message, "time").expect("time")
}

fn content_mut(message: &mut Json) -> Option<&mut Vec<Json>> {
    match json::get_mut(message, "content") {
        Some(Json::Array(items)) => Some(items),
        _ => None,
    }
}

fn set_opt(value: &mut Json, key: &str, item: Option<&Json>) {
    match item {
        Some(item) if !matches!(item, Json::Null) => json::set(value, key, item.clone()),
        _ => {
            json::remove(value, key);
        }
    }
}

fn edit_assistant(
    tx: &Transaction,
    session: &str,
    message_id: &str,
    recipe: impl FnOnce(&mut Json),
) -> rusqlite::Result<()> {
    let Some(mut assistant) = get_assistant(tx, session, message_id)? else { return Ok(()) };
    let before = assistant.clone();
    recipe(&mut assistant);
    if assistant != before {
        update(tx, session, &assistant)?;
    }
    Ok(())
}

/// `updateAssistantPart`: the last part of `kind` that matches.
fn edit_part(
    tx: &Transaction,
    session: &str,
    message_id: &str,
    kind: &str,
    call_id: Option<&str>,
    incomplete_reasoning: bool,
    recipe: impl FnOnce(&mut Json),
) -> rusqlite::Result<()> {
    edit_assistant(tx, session, message_id, |assistant| {
        let Some(content) = content_mut(assistant) else { return };
        let found = content.iter_mut().rev().find(|item| {
            json::get_str(item, "type") == Some(kind)
                && call_id.is_none_or(|id| json::get_str(item, "id") == Some(id))
                && (!incomplete_reasoning
                    || json::get(item, "time").and_then(|t| json::get(t, "completed")).is_none())
        });
        if let Some(part) = found {
            recipe(part);
        }
    })
}

fn append_part(tx: &Transaction, session: &str, message_id: &str, part: Json) -> rusqlite::Result<()> {
    edit_assistant(tx, session, message_id, |assistant| {
        if let Some(content) = content_mut(assistant) {
            content.push(part);
        }
    })
}

fn clear_current_retry(tx: &Transaction, session: &str) -> rusqlite::Result<()> {
    let Some(mut assistant) = current_assistant(tx, session)? else { return Ok(()) };
    if !retrying(&assistant) {
        return Ok(());
    }
    json::remove(&mut assistant, "retry");
    update(tx, session, &assistant)
}

fn base(id: &str, ty: &str, metadata: Option<&Json>, created: i64) -> Obj {
    let mut o = Obj::new();
    o.str("id", id);
    o.opt("metadata", metadata.cloned());
    let mut time = Obj::new();
    time.int("created", created);
    o.set("time", time.done());
    o.str("type", ty);
    o
}

fn terminal_snapshot(draft: &mut Json, data: &Json) {
    let snapshot = json::get(data, "snapshot").filter(|v| !matches!(v, Json::Null));
    let files = json::get(data, "files").filter(|v| !matches!(v, Json::Null));
    if snapshot.is_none() && files.is_none() {
        return;
    }
    let mut next = json::get(draft, "snapshot").cloned().unwrap_or_else(json::object);
    set_opt(&mut next, "end", snapshot);
    set_opt(&mut next, "files", files);
    json::set(draft, "snapshot", next);
}

/// The message half of the session projector (`SessionMessageUpdater.update`).
pub fn project(tx: &Transaction, event: &Event) -> rusqlite::Result<()> {
    let data = &event.data;
    let session = json::get_str(data, "sessionID").unwrap_or_default().to_owned();
    let session = session.as_str();
    let created = event.created;
    let metadata = event.metadata.as_ref();
    let get = |key: &str| json::get(data, key).filter(|v| !matches!(v, Json::Null));
    let get_str = |key: &str| json::get_str(data, key).unwrap_or_default().to_owned();
    match event.ty {
        "session.execution.succeeded" | "session.execution.failed" | "session.execution.interrupted" => {
            let outcome = match event.ty {
                "session.execution.succeeded" => "succeeded",
                "session.execution.failed" => "failed",
                _ => "interrupted",
            };
            clear_current_retry(tx, session)?;
            if outcome == "interrupted" && json::get_str(data, "reason") == Some("shutdown") {
                return Ok(());
            }
            let mut o = base(&id_from_event(&event.id), "idle", metadata, created);
            o.str("outcome", outcome);
            if outcome == "failed" {
                o.opt("error", get("error").cloned());
            }
            insert(tx, event, &o.done())?;
        }
        "session.instructions.updated" => {
            let Some(text) = json::get_str(data, "text") else { return Ok(()) };
            let keys: Vec<String> = match json::get(data, "delta") {
                Some(Json::Object(delta)) => delta.keys().cloned().collect(),
                _ => vec![],
            };
            let mut meta = metadata.cloned().filter(|m| matches!(m, Json::Object(_))).unwrap_or_else(json::object);
            json::set(&mut meta, "notice", json::string("instructions"));
            json::set(&mut meta, "instructionSources", Json::Array(keys.iter().map(|k| json::string(k)).collect()));
            let mut o = base(&id_from_event(&event.id), "system", Some(&meta), created);
            o.str("text", text).str("description", &format!("Instructions updated: {}", keys.join(", ")));
            insert(tx, event, &o.done())?;
        }
        "session.synthetic" => {
            let mut o = base(&id_from_event(&event.id), "synthetic", get("metadata"), created);
            o.str("text", &get_str("text"));
            o.opt("description", get("description").cloned());
            insert(tx, event, &o.done())?;
        }
        "session.moved" => {
            // `adapter.getLocation`: where the session was.
            let previous: Option<(String, Option<String>, String, Option<String>)> = tx
                .query_row(
                    "SELECT directory, workspace_id, project_id, path FROM session_v2 WHERE id = ?1",
                    rusqlite::params![session],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let mut o = base(&id_from_event(&event.id), "location-switched", metadata, created);
            o.opt("location", get("location").cloned());
            o.opt("projectID", get("projectID").cloned());
            o.opt("subpath", get("subpath").cloned());
            if let Some((directory, workspace, project, subpath)) = previous {
                let mut location = Obj::new();
                location.str("directory", &directory);
                location.opt_str("workspaceID", workspace.as_deref());
                let mut before = Obj::new();
                before.set("location", location.done()).str("projectID", &project);
                before.opt_str("subpath", subpath.as_deref());
                o.set("previous", before.done());
            }
            insert(tx, event, &o.done())?;
        }
        "session.shell.started" | "session.shell.ended" => crate::session::shell::project(tx, event)?,
        "session.skill.activated" => {
            let mut o = base(&id_from_event(&event.id), "skill", metadata, created);
            o.str("skill", &get_str("id")).str("name", &get_str("name")).str("text", &get_str("text"));
            insert(tx, event, &o.done())?;
        }
        "session.agent.selected" => {
            let previous = match get("previous") {
                Some(previous) => Some(previous.clone()),
                None => tx
                    .query_row("SELECT agent FROM session_v2 WHERE id = ?1", params![session], |r| r.get::<_, Option<String>>(0))
                    .optional()?
                    .flatten()
                    .map(Json::String),
            };
            let mut o = base(&id_from_event(&event.id), "agent-switched", metadata, created);
            o.str("agent", &get_str("agent"));
            o.opt("previous", previous);
            insert(tx, event, &o.done())?;
        }
        "session.model.selected" => {
            let previous = match get("previous") {
                Some(previous) => Some(previous.clone()),
                None => tx
                    .query_row("SELECT model FROM session_v2 WHERE id = ?1", params![session], |r| r.get::<_, Option<String>>(0))
                    .optional()?
                    .flatten()
                    .and_then(|text| json::parse(&text)),
            };
            let mut o = base(&id_from_event(&event.id), "model-switched", metadata, created);
            o.opt("model", get("model").cloned());
            o.opt("previous", previous);
            insert(tx, event, &o.done())?;
        }
        "session.step.started" => {
            let message_id = get_str("assistantMessageID");
            let started = json::get_num(data, "started").unwrap_or(created as f64) as i64;
            if let Some(mut existing) = get_assistant(tx, session, &message_id)? {
                set_opt(&mut existing, "agent", get("agent"));
                set_opt(&mut existing, "model", get("model"));
                for key in ["retry", "error", "finish", "rawFinish", "native"] {
                    json::remove(&mut existing, key);
                }
                let time = time_mut(&mut existing);
                json::set(time, "created", Json::Number(started as f64));
                json::remove(time, "streamed");
                json::remove(time, "completed");
                if let Some(snapshot) = get("snapshot") {
                    let mut next = json::get(&existing, "snapshot").cloned().unwrap_or_else(json::object);
                    json::set(&mut next, "start", snapshot.clone());
                    json::set(&mut existing, "snapshot", next);
                }
                return update(tx, session, &existing);
            }
            if let Some(mut current) = current_assistant(tx, session)? {
                json::remove(&mut current, "retry");
                let time = time_mut(&mut current);
                if json::get(time, "completed").is_none() {
                    json::set(time, "completed", Json::Number(created as f64));
                }
                update(tx, session, &current)?;
            }
            let mut o = Obj::new();
            o.str("id", &message_id);
            o.opt("metadata", metadata.cloned());
            let mut time = Obj::new();
            time.int("created", started);
            o.set("time", time.done());
            o.str("type", "assistant");
            o.opt("agent", get("agent").cloned());
            o.opt("model", get("model").cloned());
            o.set("content", Json::Array(vec![]));
            if let Some(snapshot) = get("snapshot") {
                let mut s = Obj::new();
                s.set("start", snapshot.clone());
                o.set("snapshot", s.done());
            }
            insert(tx, event, &o.done())?;
        }
        "session.step.streamed" => {
            edit_assistant(tx, session, &get_str("assistantMessageID"), |draft| {
                json::set(time_mut(draft), "streamed", Json::Number(created as f64));
            })?;
        }
        "session.step.ended" => {
            edit_assistant(tx, session, &get_str("assistantMessageID"), |draft| {
                json::set(time_mut(draft), "completed", Json::Number(created as f64));
                set_opt(draft, "finish", get("finish"));
                set_opt(draft, "rawFinish", get("rawFinish"));
                set_opt(draft, "native", get("native"));
                set_opt(draft, "cost", get("cost"));
                set_opt(draft, "tokens", get("tokens"));
                terminal_snapshot(draft, data);
            })?;
        }
        "session.step.failed" => {
            edit_assistant(tx, session, &get_str("assistantMessageID"), |draft| {
                json::set(time_mut(draft), "completed", Json::Number(created as f64));
                json::set(draft, "finish", get("finish").cloned().unwrap_or_else(|| json::string("error")));
                set_opt(draft, "rawFinish", get("rawFinish"));
                set_opt(draft, "native", get("native"));
                set_opt(draft, "error", get("error"));
                json::remove(draft, "retry");
                if get("cost").is_some() && get("tokens").is_some() {
                    set_opt(draft, "cost", get("cost"));
                    set_opt(draft, "tokens", get("tokens"));
                }
                terminal_snapshot(draft, data);
            })?;
        }
        "session.text.started" => {
            let mut part = Obj::new();
            part.str("type", "text").str("text", "");
            append_part(tx, session, &get_str("assistantMessageID"), part.done())?;
        }
        "session.text.ended" => {
            edit_part(tx, session, &get_str("assistantMessageID"), "text", None, false, |part| {
                json::set(part, "text", json::string(&get_str("text")));
                set_opt(part, "native", get("native"));
            })?;
        }
        "session.reasoning.started" => {
            let mut part = Obj::new();
            part.str("type", "reasoning").str("text", "");
            part.opt("native", get("native").cloned());
            let mut time = Obj::new();
            time.int("created", created);
            part.set("time", time.done());
            append_part(tx, session, &get_str("assistantMessageID"), part.done())?;
        }
        "session.reasoning.ended" => {
            edit_part(tx, session, &get_str("assistantMessageID"), "reasoning", None, true, |part| {
                json::set(part, "text", json::string(&get_str("text")));
                let start = json::get(part, "time").and_then(|t| json::get_num(t, "created")).unwrap_or(created as f64);
                let mut time = Obj::new();
                time.set("created", Json::Number(start)).int("completed", created);
                json::set(part, "time", time.done());
                if let Some(native) = get("native") {
                    json::set(part, "native", native.clone());
                }
            })?;
        }
        "session.tool.input.started" => {
            let mut state = Obj::new();
            state.str("status", "streaming").str("input", "");
            let mut time = Obj::new();
            time.int("created", created);
            let mut part = Obj::new();
            part.str("type", "tool").str("id", &get_str("id")).str("name", &get_str("name"));
            part.set("time", time.done()).set("state", state.done());
            append_part(tx, session, &get_str("assistantMessageID"), part.done())?;
        }
        "session.tool.input.ended" => {
            let id = get_str("id");
            edit_part(tx, session, &get_str("assistantMessageID"), "tool", Some(&id), false, |part| {
                if let Some(state) = json::get_mut(part, "state") {
                    if json::get_str(state, "status") == Some("streaming") {
                        json::set(state, "input", json::string(&get_str("text")));
                    }
                }
            })?;
        }
        "session.tool.called" => {
            let id = get_str("id");
            edit_part(tx, session, &get_str("assistantMessageID"), "tool", Some(&id), false, |part| {
                set_opt(part, "executed", get("executed"));
                set_opt(part, "native", get("native"));
                json::set(time_mut(part), "ran", Json::Number(created as f64));
                let mut state = Obj::new();
                state.str("status", "running");
                state.set("input", get("input").cloned().unwrap_or_else(json::object));
                state.set("metadata", json::object());
                json::set(part, "state", state.done());
            })?;
        }
        "session.tool.success" => {
            let id = get_str("id");
            edit_part(tx, session, &get_str("assistantMessageID"), "tool", Some(&id), false, |part| {
                let Some(state) = json::get(part, "state").cloned() else { return };
                if json::get_str(&state, "status") != Some("running") {
                    return;
                }
                let executed = json::get_bool(data, "executed") == Some(true) || json::get_bool(part, "executed") == Some(true);
                json::set(part, "executed", Json::Bool(executed));
                merge_native(part, get("native"));
                json::set(time_mut(part), "completed", Json::Number(created as f64));
                let mut next = Obj::new();
                next.str("status", "completed");
                next.set("input", json::get(&state, "input").cloned().unwrap_or_else(json::object));
                next.set("content", get("content").cloned().unwrap_or(Json::Array(vec![])));
                next.opt("metadata", get("metadata").cloned());
                json::set(part, "state", next.done());
            })?;
        }
        "session.tool.failed" => {
            let id = get_str("id");
            edit_part(tx, session, &get_str("assistantMessageID"), "tool", Some(&id), false, |part| {
                let Some(state) = json::get(part, "state").cloned() else { return };
                let status = json::get_str(&state, "status");
                if status != Some("streaming") && status != Some("running") {
                    return;
                }
                let executed = json::get_bool(data, "executed") == Some(true) || json::get_bool(part, "executed") == Some(true);
                json::set(part, "executed", Json::Bool(executed));
                merge_native(part, get("native"));
                json::set(time_mut(part), "completed", Json::Number(created as f64));
                let input = match json::get(&state, "input") {
                    Some(Json::String(_)) | None => json::object(),
                    Some(input) => input.clone(),
                };
                let mut next = Obj::new();
                next.str("status", "error");
                next.opt("error", get("error").cloned());
                next.set("input", input);
                next.opt("content", get("content").cloned());
                next.opt("metadata", get("metadata").cloned());
                json::set(part, "state", next.done());
            })?;
        }
        "session.retry.scheduled" => {
            edit_assistant(tx, session, &get_str("assistantMessageID"), |draft| {
                let mut retry = Obj::new();
                retry.opt("attempt", get("attempt").cloned());
                retry.opt("at", get("at").cloned());
                retry.opt("error", get("error").cloned());
                json::set(draft, "retry", retry.done());
            })?;
        }
        _ => {}
    }
    Ok(())
}

fn merge_native(part: &mut Json, native: Option<&Json>) {
    let Some(Json::Object(native)) = native else { return };
    let mut merged = json::get(part, "native").cloned().filter(|n| matches!(n, Json::Object(_))).unwrap_or_else(json::object);
    for (key, value) in native {
        json::set(&mut merged, key, value.clone());
    }
    json::set(part, "native", merged);
}

/// `SessionStore.messages` input.
pub struct ListInput {
    pub session: String,
    pub limit: Option<i64>,
    pub order: String,
    pub ty: Option<String>,
    pub cursor: Option<(String, String)>,
}

/// One page of a session's messages in the requested order (`SessionStore.messages`).
pub fn list(db: &Connection, input: &ListInput) -> rusqlite::Result<Vec<Json>> {
    let direction = input.cursor.as_ref().map(|c| c.1.as_str()).unwrap_or("next");
    let requested = input.order.as_str();
    let order = if direction == "previous" { if requested == "asc" { "desc" } else { "asc" } } else { requested };
    let mut sql = String::from("SELECT id, type, data FROM session_message WHERE session_id = ?1");
    let mut values: Vec<rusqlite::types::Value> = vec![input.session.clone().into()];
    if let Some((id, _)) = &input.cursor {
        let anchor: Option<i64> = db
            .query_row(
                "SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2",
                params![input.session, id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(anchor) = anchor else { return Ok(vec![]) };
        values.push(anchor.into());
        sql.push_str(&format!(" AND seq {} ?{}", if order == "asc" { ">" } else { "<" }, values.len()));
    }
    if let Some(ty) = &input.ty {
        values.push(ty.clone().into());
        sql.push_str(&format!(" AND type = ?{}", values.len()));
    }
    sql.push_str(if order == "asc" { " ORDER BY seq ASC" } else { " ORDER BY seq DESC" });
    if let Some(limit) = input.limit {
        sql.push_str(&format!(" LIMIT {limit}"));
    }
    let mut statement = db.prepare(&sql)?;
    let mut rows: Vec<Json> = statement
        .query_map(rusqlite::params_from_iter(values), |row| {
            Ok(from_row(row.get(0)?, row.get(1)?, &row.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if direction == "previous" {
        rows.reverse();
    }
    Ok(rows)
}

/// One message by ID with its session (`SessionStore.message`).
pub fn get(db: &Connection, message_id: &str) -> rusqlite::Result<Option<(String, Json)>> {
    db.query_row(
        "SELECT id, type, data, session_id FROM session_message WHERE id = ?1",
        params![message_id],
        |row| Ok((row.get::<_, String>(3)?, from_row(row.get(0)?, row.get(1)?, &row.get::<_, String>(2)?))),
    )
    .optional()
}

/// The messages later requests see, from the latest completed compaction on (`SessionHistory.load(db, id,
/// "latest")`, see [`crate::session::compaction::history`]).
pub fn history(db: &Connection, session: &str) -> rusqlite::Result<Vec<Json>> {
    crate::session::compaction::history(db, session, None)
}

/// `SessionHistory.firstUserMessage`.
pub fn first_user(db: &Connection, session: &str) -> rusqlite::Result<Option<(String, String)>> {
    db.query_row(
        "SELECT id, json_extract(data, '$.text') FROM session_message WHERE session_id = ?1 AND type = 'user' ORDER BY seq ASC LIMIT 1",
        params![session],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    )
    .optional()
    .map(|found| found.and_then(|(id, text)| text.map(|text| (id, text))))
}

/// `SessionHistory.incompleteAssistants`: the unfinished assistants from the latest completed compaction on.
pub fn incomplete_assistants(db: &Connection, session: &str) -> rusqlite::Result<Vec<Json>> {
    let boundary = crate::session::compaction::boundary(db, session)?;
    let mut statement = db.prepare(
        "SELECT id, type, data FROM session_message WHERE session_id = ?1 AND type = 'assistant' \
         AND json_extract(data, '$.time.completed') IS NULL AND seq >= ?2 ORDER BY seq ASC",
    )?;
    statement
        .query_map(params![session, boundary], |row| Ok(from_row(row.get(0)?, row.get(1)?, &row.get::<_, String>(2)?)))?
        .collect()
}
