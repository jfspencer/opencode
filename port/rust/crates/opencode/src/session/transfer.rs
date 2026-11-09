//! Moving a session's history (packages/core/src/session/transfer.ts, `Session.fork` in session.ts and
//! `projectFork` in session/projector.ts): export (optionally sanitized), import under the recorded IDs, and fork.
//!
//! An export holds the session's info and its settled messages (an assistant that has completed, a shell command or
//! compaction that is not running). An import recreates the session at the given location with the exported
//! history, totals and times; message IDs are global, so importing a transcript twice conflicts. A fork copies the
//! settled history up to a boundary into a new session (messages `msg_<fork event>_<seq>`), with the parent's newest
//! instruction values and its API-managed instruction entries.
//!
//! Where `experimental.media_blob` is on, an import's inline media and a fork's copy are stored as blobs.
//!
//! Transcripts exported before v2.1 are upgraded on import ([`legacy_messages`], applied by the API pipeline before
//! the payload is decoded, so the upgrade decides what is valid, as `ImportedMessages` does in TS).

use opencode_protocol::types as t;
use opencode_protocol::{Json, Wire};
use rusqlite::types::Value;
use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};

use crate::app::{App, fatal};
use crate::bus::{Event, LocationRef};
use crate::http::{Failure, Result};
use crate::json::{self, Obj};
use crate::session::{self, blob, message};

fn conflict(message: String, resource: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "ConflictError").str("message", &message).str("resource", resource);
    Failure::new(409, o.done())
}

/// `isSettled`: what an export or a fork carries.
pub fn settled(message: &Json) -> bool {
    match json::get_str(message, "type") {
        Some("assistant") => json::get(message, "time").and_then(|t| json::get(t, "completed")).is_some(),
        Some("shell" | "compaction") => json::get_str(message, "status") != Some("running"),
        _ => true,
    }
}

/// `SessionTransfer.export`: `{info, messages}`, the messages oldest first.
pub fn export(app: &App, session_id: &str, sanitized: bool) -> Result<Json> {
    let db = app.db.lock().unwrap();
    let stored = session::require(&db, session_id)?;
    let input = message::ListInput { session: session_id.to_owned(), limit: None, order: "asc".into(), ty: None, cursor: None };
    let mut messages: Vec<Json> = message::list(&db, &input).map_err(fatal)?;
    session::blob::prepare(&db, &mut messages, false).map_err(fatal)?;
    let messages: Vec<Json> = messages.into_iter().filter(settled).collect();
    drop(db);
    let mut o = Obj::new();
    if sanitized {
        o.set("info", sanitize_info(&stored.info));
        o.set("messages", Json::Array(messages.iter().map(sanitize_message).collect()));
    } else {
        o.set("info", stored.info);
        o.set("messages", Json::Array(messages));
    }
    Ok(o.done())
}

/// A message's stored row parts (`encodeSync(SessionMessage.Info)` without `id` and `type`): its type, data and
/// creation time.
fn stored(message: &Json) -> (String, String, i64) {
    let mut data = match t::SessionMessageInfo::decode(message) {
        Ok(info) => json::of(&info),
        Err(_) => message.clone(),
    };
    let ty = json::get_str(&data, "type").unwrap_or_default().to_owned();
    let created = json::get(&data, "time").and_then(|t| json::get_num(t, "created")).unwrap_or(0.0) as i64;
    json::remove(&mut data, "id");
    json::remove(&mut data, "type");
    (ty, json::text(&data), created)
}

fn millis(value: Option<&Json>) -> Option<i64> {
    match value {
        Some(Json::Number(n)) => Some(*n as i64),
        _ => None,
    }
}

/// `SessionTransfer.import`: the session as `data.info` describes it, at `location`, with `data.messages`' settled
/// history. Returns the stored session.
pub async fn import(app: &App, data: &Json, location: LocationRef) -> Result<session::Stored> {
    let info = json::get(data, "info").cloned().unwrap_or(Json::Null);
    let session_id = json::get_str(&info, "id").unwrap_or_default().to_owned();
    let already = || conflict(format!("Session already exists: {session_id}"), &session_id);
    if session::get(&app.db.lock().unwrap(), &session_id).map_err(fatal)?.is_some() {
        return Err(already());
    }
    let parent_id = json::get_str(&info, "parentID").map(str::to_owned);
    if let Some(parent) = &parent_id {
        session::require(&app.db.lock().unwrap(), parent)?;
    }
    // Message IDs are global, so a transcript imported again under another session ID still collides.
    let messages: Vec<Json> =
        json::get(data, "messages").and_then(json::array).unwrap_or_default().iter().filter(|m| settled(m)).cloned().collect();
    if !messages.is_empty() {
        let ids: Vec<Value> = messages.iter().map(|m| Value::Text(json::get_str(m, "id").unwrap_or_default().to_owned())).collect();
        let placeholders = (1..=ids.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
        let existing: Option<String> = app
            .db
            .lock()
            .unwrap()
            .query_row(
                &format!("SELECT id FROM session_message WHERE id IN ({placeholders}) ORDER BY id LIMIT 1"),
                params_from_iter(ids),
                |row| row.get(0),
            )
            .optional()
            .map_err(fatal)?;
        if let Some(existing) = existing {
            return Err(conflict(format!("Message already exists: {existing}"), &existing));
        }
    }
    let project = app.projects.discover(&location.directory);
    app.projects.persist(&mut app.db.lock().unwrap(), &app.bus, &project).map_err(fatal)?;
    let imported_at = crate::id::now();
    let subpath = session::relative(&project.directory, &location.directory).replace('\\', "/");
    let mut rows: Vec<(String, String, String, i64)> = messages
        .iter()
        .map(|m| {
            let (ty, data, created) = stored(m);
            (json::get_str(m, "id").unwrap_or_default().to_owned(), ty, data, created)
        })
        .collect();
    // Only inline media the import carries becomes blobs, and only its blobs get refs: refs it carried in could
    // otherwise keep or expose blobs it never held. The destination's config is read only when needed.
    let split: Vec<(String, Vec<blob::Pending>)> = rows
        .iter()
        .map(|(_, _, data, _)| {
            let mut value = json::parse(data).unwrap_or(Json::Null);
            let pending = blob::split(&mut value);
            (json::text(&value), pending)
        })
        .collect();
    let media = split.iter().any(|(_, pending)| !pending.is_empty()) && blob::enabled_for(app, &location).await;
    if media {
        for (row, (data, _)) in rows.iter_mut().zip(&split) {
            row.2 = data.clone();
        }
    }

    let mut event = Obj::new();
    event.str("sessionID", &session_id);
    event.opt_str("parentID", parent_id.as_deref());
    event.str("slug", &crate::id::slug()).str("version", crate::app::VERSION).str("projectID", &project.id);
    event.set("location", location.to_json()).str("subpath", &subpath);
    for key in ["title", "agent", "model", "metadata", "permissions"] {
        event.opt(key, json::get(&info, key).filter(|v| !matches!(v, Json::Null)).cloned());
    }
    let time = json::get(&info, "time").cloned().unwrap_or(Json::Null);
    let at = |key: &str| millis(json::get(&time, key));
    let tokens = json::get(&info, "tokens").cloned().unwrap_or(Json::Null);
    let count = |key: &str| json::get_num(&tokens, key).unwrap_or(0.0) as i64;
    let cache = |key: &str| json::get(&tokens, "cache").and_then(|c| json::get_num(c, key)).unwrap_or(0.0) as i64;
    let idle = at("idle");
    let commit = |tx: &Transaction, event: &Event| -> rusqlite::Result<()> {
        if !rows.is_empty() {
            if media {
                for (_, pending) in &split {
                    blob::save(tx, pending)?;
                }
            }
            for (index, (id, ty, data, created)) in rows.iter().enumerate() {
                tx.execute(
                    "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![id, session_id, ty, index as i64 + 1, created, imported_at, data],
                )?;
            }
            if media {
                for (index, ((id, ..), (_, pending))) in rows.iter().zip(&split).enumerate() {
                    let hashes = pending.iter().map(|p| p.hash.clone()).collect();
                    blob::record_hashes(tx, &session_id, id, index as i64 + 1, &hashes)?;
                }
            }
            let seq = event.durable.as_ref().map_or(0, |d| d.seq) + rows.len() as i64;
            reserve_sequence(tx, &session_id, seq)?;
        }
        tx.execute(
            "UPDATE session_v2 SET cost = ?1, tokens_input = ?2, tokens_output = ?3, tokens_reasoning = ?4, \
             tokens_cache_read = ?5, tokens_cache_write = ?6, time_created = ?7, time_updated = ?8, time_busy = ?9, \
             time_idle = ?10, time_viewed = ?11, idle_outcome = ?12, time_archived = ?13, revert = ?14 WHERE id = ?15",
            params_from_iter([
                Value::Real(json::get_num(&info, "cost").unwrap_or(0.0)),
                Value::Integer(count("input")),
                Value::Integer(count("output")),
                Value::Integer(count("reasoning")),
                Value::Integer(cache("read")),
                Value::Integer(cache("write")),
                Value::Integer(at("created").unwrap_or(imported_at)),
                Value::Integer(imported_at),
                idle.zip(at("busy")).map_or(Value::Null, |(idle, busy)| Value::Integer(idle.min(busy))),
                idle.map_or(Value::Null, Value::Integer),
                idle.zip(at("viewed")).map_or(Value::Null, |(idle, viewed)| Value::Integer(idle.min(viewed))),
                match (idle, json::get_str(&info, "outcome")) {
                    (Some(_), Some(outcome)) => Value::Text(outcome.to_owned()),
                    _ => Value::Null,
                },
                at("archived").map_or(Value::Null, Value::Integer),
                json::get(&info, "revert").filter(|v| !matches!(v, Json::Null)).map_or(Value::Null, |r| Value::Text(json::text(r))),
                Value::Text(session_id.clone()),
            ]),
        )?;
        Ok(())
    };
    match session::publish_committed(app, "session.created", event.done(), Some(location), None, commit) {
        Ok(_) => {}
        // A concurrent import projected the session first.
        Err(rusqlite::Error::SqliteFailure(_, Some(text))) if text == "session already projected" => return Err(already()),
        Err(error) => return Err(fatal(error)),
    }
    session::require(&app.db.lock().unwrap(), &session_id)
}

/// `Bus.reserveSequence`: the aggregate's next event follows `seq`.
fn reserve_sequence(tx: &Transaction, aggregate: &str, seq: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?1, ?2) \
         ON CONFLICT (aggregate_id) DO UPDATE SET seq = max(event_sequence.seq, ?2)",
        params![aggregate, seq],
    )?;
    Ok(())
}

fn missing_message(session_id: &str, message_id: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "MessageNotFoundError").str("sessionID", session_id).str("messageID", message_id);
    o.str("message", &format!("Message not found: {message_id}"));
    Failure::new(404, o.done())
}

thread_local! {
    /// `SessionBlob.ForkMedia`: set while a fork is published, so its projection stores the copy's media as blobs.
    static FORK_MEDIA: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `Session.fork`: a new session with the parent's settled history through its last message, or up to (not
/// including) `before`.
pub async fn fork(app: &App, session_id: &str, before: Option<&str>) -> Result<session::Stored> {
    let (parent, boundary, instructions, entries) = {
        let db = app.db.lock().unwrap();
        let parent = session::require(&db, session_id)?;
        let boundary: Option<String> = match before {
            Some(before) => db.query_row(
                "SELECT id FROM session_message WHERE session_id = ?1 AND id = ?2 ORDER BY seq DESC LIMIT 1",
                params![session_id, before],
                |row| row.get(0),
            ),
            None => db.query_row(
                "SELECT id FROM session_message WHERE session_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| row.get(0),
            ),
        }
        .optional()
        .map_err(fatal)?;
        let instructions: Option<String> = db
            .query_row("SELECT current_values FROM instruction_state WHERE session_id = ?1", params![session_id], |row| row.get(0))
            .optional()
            .map_err(fatal)?;
        let entries = entry_snapshot(&db, session_id).map_err(fatal)?;
        (parent, boundary, instructions, entries)
    };
    let Some(boundary) = boundary else {
        return match before {
            Some(before) => Err(missing_message(session_id, before)),
            None => Err(Failure::invalid(format!("Cannot fork empty session: {session_id}"), None, Some("empty_session"))),
        };
    };
    let id = crate::id::session();
    let mut point = Obj::new();
    point.str("type", if before.is_some() { "before" } else { "through" }).str("messageID", &boundary);
    let mut data = Obj::new();
    data.str("sessionID", &id).str("parentID", parent.id()).set("boundary", point.done());
    data.opt("instructions", instructions.as_deref().and_then(json::parse));
    data.set("instructionEntries", entries);
    // The parent's location, not the caller's, decides whether the copy's media is stored as blobs.
    let media = blob::enabled_for(app, &parent.location).await;
    FORK_MEDIA.with(|cell| cell.set(media));
    let published = session::publish_event(app, "session.forked", data.done(), None, None);
    FORK_MEDIA.with(|cell| cell.set(false));
    published.map_err(fatal)?;
    session::require(&app.db.lock().unwrap(), &id)
}

/// The `session_part` rows of a copied assistant message, with their media stored as blobs when `media` is on and
/// their refs recorded for the copy.
fn copy_parts(tx: &Transaction, parent: &str, original: &str, child: &str, message: &str, seq: i64, media: bool) -> rusqlite::Result<()> {
    type Part = (i64, String, Option<String>, i64, i64, String);
    let parts: Vec<Part> = {
        let mut statement = tx.prepare(
            "SELECT ordinal, type, call_id, time_created, time_updated, data FROM session_part \
             WHERE session_id = ?1 AND message_id = ?2 ORDER BY ordinal ASC",
        )?;
        statement
            .query_map(params![parent, original], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    if parts.is_empty() {
        return Ok(());
    }
    let mut content = Vec::with_capacity(parts.len());
    for (ordinal, ty, call, created, updated, text) in parts {
        let mut value = json::parse(&text).unwrap_or(Json::Null);
        let mut stored = text;
        if media {
            let mut wrapper = Obj::new();
            wrapper.set("content", Json::Array(vec![value.clone()]));
            let mut wrapper = wrapper.done();
            let pending = blob::split(&mut wrapper);
            if !pending.is_empty() {
                blob::save(tx, &pending)?;
                if let Some(Json::Array(mut items)) = json::remove(&mut wrapper, "content") {
                    value = items.remove(0);
                    stored = json::text(&value);
                }
            }
        }
        tx.execute(
            "INSERT INTO session_part (message_id, session_id, ordinal, type, call_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![message, child, ordinal, ty, call, created, updated, stored],
        )?;
        content.push(value);
    }
    let mut owner = Obj::new();
    owner.set("content", Json::Array(content));
    blob::record_refs(tx, child, message, seq, &owner.done())
}

/// `InstructionEntry.snapshot`: every entry of a session, removed ones included, in key order.
fn entry_snapshot(db: &rusqlite::Connection, session_id: &str) -> rusqlite::Result<Json> {
    let mut statement = db.prepare("SELECT key, value, removed FROM instruction_entry WHERE session_id = ?1 ORDER BY key ASC")?;
    let rows = statement.query_map(params![session_id], |row| {
        let value: Option<String> = row.get(1)?;
        let mut o = Obj::new();
        o.str("key", &row.get::<_, String>(0)?);
        o.set("value", value.and_then(|text| json::parse(&text)).unwrap_or(Json::Null));
        o.set("removed", Json::Bool(row.get::<_, bool>(2)?));
        Ok(o.done())
    })?;
    Ok(Json::Array(rows.collect::<rusqlite::Result<_>>()?))
}

/// `forkTitle`: `title (fork #n)`, counting up.
fn fork_title(value: Option<&str>) -> Option<String> {
    let value = value?;
    if let Some(open) = value.rfind(" (fork #") {
        let number = &value[open + 8..];
        if open > 0 && number.ends_with(')') && number.len() > 1 && number[..number.len() - 1].bytes().all(|b| b.is_ascii_digit()) {
            let n: u64 = number[..number.len() - 1].parse().unwrap_or(0);
            return Some(format!("{} (fork #{})", &value[..open], n + 1));
        }
    }
    Some(format!("{value} (fork #1)"))
}

/// `projectFork`: the child's row, its instruction entries, the copied history and parts, and its instruction state.
pub fn project_fork(tx: &Transaction, event: &Event) -> rusqlite::Result<()> {
    let data = &event.data;
    let id = json::get_str(data, "sessionID").unwrap_or_default();
    let parent_id = json::get_str(data, "parentID").unwrap_or_default();
    let boundary = json::get(data, "boundary").cloned().unwrap_or(Json::Null);
    let boundary_id = json::get_str(&boundary, "messageID").unwrap_or_default();
    let defect = |text: String| rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR), Some(text));
    let parent_title: Option<String> = tx
        .query_row("SELECT title FROM session_v2 WHERE id = ?1", params![parent_id], |row| row.get(0))
        .optional()?
        .ok_or_else(|| defect(format!("Fork parent session not found: {parent_id}")))?;
    let boundary_seq: i64 = tx
        .query_row("SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2", params![parent_id, boundary_id], |row| row.get(0))
        .optional()?
        .ok_or_else(|| defect(format!("Fork boundary message not found: {boundary_id}")))?;
    let comparison = if json::get_str(&boundary, "type") == Some("before") { "<" } else { "<=" };
    let copied: Option<i64> = tx
        .query_row(
            &format!("SELECT seq FROM session_message WHERE session_id = ?1 AND seq {comparison} ?2 ORDER BY seq DESC LIMIT 1"),
            params![parent_id, boundary_seq],
            |row| row.get(0),
        )
        .optional()?;
    let inserted = tx.execute(
        "INSERT INTO session_v2 (id, parent_id, fork_session_id, fork_boundary, project_id, workspace_id, slug, directory, \
         path, title, agent, model, metadata, permission, version, cost, tokens_input, tokens_output, tokens_reasoning, \
         tokens_cache_read, tokens_cache_write, time_created, time_updated) \
         SELECT ?1, NULL, id, ?2, project_id, workspace_id, ?3, directory, path, ?4, agent, model, metadata, permission, \
         version, 0, 0, 0, 0, 0, 0, ?5, ?5 FROM session_v2 WHERE id = ?6 ON CONFLICT DO NOTHING",
        params![id, json::text(&boundary), crate::id::slug(), fork_title(parent_title.as_deref()), event.created, parent_id],
    )?;
    if inserted == 0 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some("session already projected".into()),
        ));
    }
    if let Some(Json::Array(entries)) = json::get(data, "instructionEntries") {
        for entry in entries {
            let value = json::get(entry, "value").filter(|v| !matches!(v, Json::Null)).map(json::text);
            tx.execute(
                "INSERT INTO instruction_entry (key, value, removed, session_id, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![json::get_str(entry, "key").unwrap_or_default(), value, json::get_bool(entry, "removed") == Some(true), id, event.created],
            )?;
        }
    }
    let prefix = message::id_from_event(&event.id);
    let forked = |seq: i64| format!("{prefix}_{seq}");
    if let Some(copied) = copied {
        // Terminal events for active projections stay on the parent, so forks copy only settled history.
        type Copied = (String, String, i64, i64, i64, String);
        let rows: Vec<Copied> = {
            let mut statement = tx.prepare(
                "SELECT id, type, seq, time_created, time_updated, data FROM session_message WHERE session_id = ?1 AND seq <= ?2 \
                 AND (type != 'assistant' OR json_extract(data, '$.time.completed') IS NOT NULL) \
                 AND (type != 'shell' OR json_extract(data, '$.status') != 'running') \
                 AND (type != 'compaction' OR json_extract(data, '$.status') != 'running') ORDER BY seq ASC",
            )?;
            statement
                .query_map(params![parent_id, copied], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let media = FORK_MEDIA.with(|cell| cell.get());
        for (original, ty, seq, created, updated, text) in &rows {
            let mut data_text = text.clone();
            if media {
                // Split parents' envelopes carry no content, so only unsplit parents' inline media moves here.
                let mut value = json::parse(text).unwrap_or(Json::Null);
                let pending = blob::split(&mut value);
                if !pending.is_empty() {
                    blob::save(tx, &pending)?;
                    data_text = json::text(&value);
                }
            }
            // A summary references the first message it kept, which the fork copied under a new ID.
            if ty == "compaction" {
                if let Some(mut value) = json::parse(text) {
                    let keep = json::get(&value, "metadata").and_then(|m| json::get_str(m, "keepID")).filter(|k| k.starts_with("msg")).map(str::to_owned);
                    if let Some(keep) = keep {
                        let kept: Option<i64> = tx
                            .query_row("SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2", params![parent_id, keep], |row| row.get(0))
                            .optional()?;
                        if let (Some(kept), Some(metadata)) = (kept, json::get_mut(&mut value, "metadata")) {
                            json::set(metadata, "keepID", json::string(&forked(kept)));
                            data_text = json::text(&value);
                        }
                    }
                }
            }
            tx.execute(
                "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![forked(*seq), id, ty, seq, created, updated, data_text],
            )?;
            blob::record_refs(tx, id, &forked(*seq), *seq, &json::parse(&data_text).unwrap_or(Json::Null))?;
            if ty == "assistant" {
                copy_parts(tx, parent_id, original, id, &forked(*seq), *seq, media)?;
            }
        }
        reserve_sequence(tx, id, copied)?;
    }
    if let Some(instructions) = json::get(data, "instructions").filter(|v| !matches!(v, Json::Null)) {
        let seq = event.durable.as_ref().map_or(0, |d| d.seq);
        let values = json::text(instructions);
        tx.execute(
            "INSERT INTO instruction_state (session_id, epoch_start, through_seq, initial_values, current_values) \
             VALUES (?1, ?2, ?2, ?3, ?3) ON CONFLICT DO NOTHING",
            params![id, seq, values],
        )?;
    }
    Ok(())
}

// --- Transcripts from before v2.1 (packages/schema/src/session-legacy.ts) ---------------------------------------

/// `SessionLegacy.messages`: upgrades complete messages written before v2.1, before they are decoded. Native data
/// was then named `state` or `providerState`, a tool's result native data `providerResultState`, a summary
/// compaction kept the text of the messages it kept in `recent`, and a provider compaction its window in
/// `providerContext`.
pub fn legacy_messages(values: &[Json]) -> Vec<Json> {
    values
        .iter()
        .map(|value| match value {
            Json::Object(_) => legacy_message(json::get_str(value, "type").unwrap_or_default(), value.clone()),
            other => other.clone(),
        })
        .collect()
}

/// `SessionLegacy.message`: a projected message's data by message type.
pub fn legacy_message(ty: &str, data: Json) -> Json {
    match ty {
        "assistant" => legacy_content(rename(data, "providerState", "native")),
        "compaction" => provider_context(recent(rename(data, "providerState", "native"))),
        _ => data,
    }
}

/// The current name wins when a payload carries both.
fn rename(mut value: Json, from: &str, to: &str) -> Json {
    let Some(moved) = json::remove(&mut value, from) else { return value };
    if json::get(&value, to).is_none() {
        json::set(&mut value, to, moved);
    }
    value
}

fn legacy_content(mut data: Json) -> Json {
    if let Some(Json::Array(parts)) = json::get_mut(&mut data, "content") {
        for part in parts.iter_mut() {
            let current = std::mem::take(part);
            *part = legacy_part(current);
        }
    }
    data
}

/// `SessionLegacy.part`: an assistant content part written before v2.1.
pub fn legacy_part(value: Json) -> Json {
    if !matches!(value, Json::Object(_)) {
        return value;
    }
    match json::get_str(&value, "type") {
        Some("text" | "reasoning") => rename(value, "state", "native"),
        // A tool part's own `state` is its lifecycle status, not native data.
        Some("tool") => tool_result(rename(value, "providerState", "native")),
        _ => value,
    }
}

/// Result data merges into the tool's single `native`.
fn tool_result(mut value: Json) -> Json {
    let Some(result) = json::remove(&mut value, "providerResultState") else { return value };
    let Json::Object(result) = result else { return value };
    let mut native = match json::get(&value, "native") {
        Some(Json::Object(native)) => native.clone(),
        _ => Default::default(),
    };
    for (key, item) in result {
        native.insert(key, item);
    }
    json::set(&mut value, "native", Json::Object(native));
    value
}

/// Kept text a summary compaction stored before it referenced `metadata.keepID`.
fn recent(mut data: Json) -> Json {
    let text = match json::remove(&mut data, "recent") {
        Some(Json::String(text)) if !text.is_empty() => text,
        _ => return data,
    };
    let mut metadata = Obj::new();
    metadata.str("recent", &text);
    let mut metadata = metadata.done();
    if let Some(Json::Object(existing)) = json::get(&data, "metadata") {
        for (key, item) in existing.clone() {
            json::set(&mut metadata, &key, item);
        }
    }
    json::set(&mut data, "metadata", metadata);
    data
}

/// The window moves to `native.messages`.
fn provider_context(mut data: Json) -> Json {
    let context = match json::remove(&mut data, "providerContext") {
        Some(context @ Json::Object(_)) => context,
        _ => return data,
    };
    let mut native = Obj::new();
    match json::get(&context, "messages") {
        Some(Json::Array(messages)) => {
            native.set("messages", Json::Array(messages.iter().cloned().map(legacy_media).collect()));
        }
        other => {
            native.opt("messages", other.cloned());
        }
    }
    json::set(&mut data, "native", native.done());
    data
}

/// Before 2.0.15, window media parts stored `mediaType` and base64 `data` directly.
fn legacy_media(mut message: Json) -> Json {
    if let Some(Json::Array(parts)) = json::get_mut(&mut message, "content") {
        for part in parts.iter_mut() {
            let convert = matches!(part, Json::Object(_))
                && json::get_str(part, "type") == Some("media")
                && json::get(part, "media").is_none()
                && matches!(json::get(part, "data"), Some(Json::String(_)));
            if !convert {
                continue;
            }
            let media_type = json::remove(part, "mediaType");
            let data = json::remove(part, "data");
            let mut source = Obj::new();
            source.str("type", "base64").opt("data", data).opt("mediaType", media_type);
            let mut media = Obj::new();
            media.set("source", source.done());
            json::set(part, "media", media.done());
        }
    }
    message
}

// --- Sanitized exports --------------------------------------------------------------------------------------------

fn trimmed_empty(value: &str) -> bool {
    value.trim_matches(crate::text::is_js_whitespace).is_empty()
}

/// `redact`: non-blank text becomes `[redacted:<kind>:<id>]`.
fn redact(kind: &str, id: &str, value: &str) -> Json {
    if trimmed_empty(value) { json::string(value) } else { json::string(&format!("[redacted:{kind}:{id}]")) }
}

fn marker(text: String) -> Json {
    let mut o = Obj::new();
    o.str("redacted", &text);
    o.done()
}

/// `metadata`: a non-empty record becomes `{redacted: "<kind>:<id>"}`.
fn metadata(kind: &str, id: &str, value: Option<&Json>) -> Option<Json> {
    match value {
        Some(Json::Object(o)) if !o.is_empty() => Some(marker(format!("{kind}:{id}"))),
        other => other.cloned(),
    }
}

/// Replaces a string field with its redaction (absent stays absent).
fn redact_field(value: &mut Json, key: &str, kind: &str, id: &str) {
    if let Some(text) = json::get_str(value, key).map(str::to_owned) {
        json::set(value, key, redact(kind, id, &text));
    }
}

fn set_or_remove(value: &mut Json, key: &str, item: Option<Json>) {
    match item {
        Some(item) => json::set(value, key, item),
        None => {
            json::remove(value, key);
        }
    }
}

fn sanitize_info(info: &Json) -> Json {
    let mut out = info.clone();
    let id = json::get_str(info, "id").unwrap_or_default().to_owned();
    redact_field(&mut out, "title", "session-title", &id);
    let meta = metadata("session-metadata", &id, json::get(info, "metadata"));
    set_or_remove(&mut out, "metadata", meta);
    if let Some(location) = json::get_mut(&mut out, "location") {
        let directory = json::get_str(location, "directory").unwrap_or_default().to_owned();
        let redacted = redact("session-directory", &id, &directory);
        json::set(location, "directory", json::string(&format!("/{}", json::as_str(&redacted).unwrap_or_default())));
    }
    if let Some(revert) = json::get_mut(&mut out, "revert") {
        if let Some(Json::Array(files)) = json::get_mut(revert, "files") {
            for (index, file) in files.iter_mut().enumerate() {
                redact_field(file, "file", "revert-file", &index.to_string());
                redact_field(file, "patch", "revert-patch", &index.to_string());
            }
        }
    }
    out
}

fn redact_mention(value: &mut Json, kind: &str, index: &str) {
    if let Some(mention) = json::get_mut(value, "mention") {
        redact_field(mention, "text", kind, index);
    }
}

fn sanitize_message(message: &Json) -> Json {
    let mut out = message.clone();
    let id = json::get_str(message, "id").unwrap_or_default().to_owned();
    let meta = metadata("message-metadata", &id, json::get(message, "metadata"));
    set_or_remove(&mut out, "metadata", meta);
    match json::get_str(message, "type").unwrap_or_default() {
        "user" => {
            redact_field(&mut out, "text", "text", &id);
            if let Some(Json::Array(files)) = json::get_mut(&mut out, "files") {
                for (index, file) in files.iter_mut().enumerate() {
                    let index = index.to_string();
                    json::set(file, "data", json::string(""));
                    let mut source = Obj::new();
                    source.str("type", "inline");
                    json::set(file, "source", source.done());
                    redact_field(file, "name", "file-name", &index);
                    redact_field(file, "description", "file-description", &index);
                    redact_mention(file, "file-mention", &index);
                }
            }
            if let Some(Json::Array(agents)) = json::get_mut(&mut out, "agents") {
                for (index, agent) in agents.iter_mut().enumerate() {
                    let index = index.to_string();
                    redact_field(agent, "name", "agent-name", &index);
                    redact_mention(agent, "agent-mention", &index);
                }
            }
            if let Some(Json::Array(skills)) = json::get_mut(&mut out, "skills") {
                for (index, skill) in skills.iter_mut().enumerate() {
                    let index = index.to_string();
                    redact_field(skill, "name", "skill-name", &index);
                    redact_field(skill, "text", "skill", &index);
                    redact_mention(skill, "skill-mention", &index);
                }
            }
        }
        "synthetic" => {
            redact_field(&mut out, "text", "synthetic", &id);
            redact_field(&mut out, "description", "synthetic-description", &id);
        }
        "system" => redact_field(&mut out, "text", "system", &id),
        "skill" => redact_field(&mut out, "text", "skill", &id),
        "shell" => {
            redact_field(&mut out, "command", "shell-command", &id);
            if let Some(output) = json::get_mut(&mut out, "output") {
                redact_field(output, "output", "shell-output", &id);
            }
        }
        "assistant" => {
            if let Some(Json::Array(content)) = json::get_mut(&mut out, "content") {
                for part in content.iter_mut() {
                    let kind = json::get_str(part, "type").unwrap_or_default().to_owned();
                    let native = |name: &str, part: &Json| {
                        json::get(part, "native").filter(|v| !matches!(v, Json::Null)).map(|_| marker(format!("{name}-native:{id}")))
                    };
                    match kind.as_str() {
                        "text" | "reasoning" => {
                            redact_field(part, "text", &kind, &id);
                            let native = native(&kind, part);
                            set_or_remove(part, "native", native);
                        }
                        _ => {
                            let native = native("tool", part);
                            set_or_remove(part, "native", native);
                            if let Some(state) = json::get_mut(part, "state") {
                                sanitize_tool_state(&id, state);
                            }
                        }
                    }
                }
            }
        }
        "compaction" => {
            if json::get_str(message, "status") != Some("failed") {
                redact_field(&mut out, "summary", "compaction-summary", &id);
                if json::get_str(message, "status") == Some("completed") {
                    let native = metadata("compaction-native", &id, json::get(message, "native"));
                    set_or_remove(&mut out, "native", native);
                }
            }
        }
        _ => {}
    }
    out
}

fn sanitize_tool_state(id: &str, state: &mut Json) {
    let status = json::get_str(state, "status").unwrap_or_default().to_owned();
    if status == "streaming" {
        redact_field(state, "input", "tool-input", id);
        return;
    }
    json::set(state, "input", marker(format!("tool-input:{id}")));
    if status == "running" {
        json::set(state, "metadata", marker(format!("tool-metadata:{id}")));
        return;
    }
    if json::get(state, "metadata").is_some() {
        json::set(state, "metadata", marker(format!("tool-metadata:{id}")));
    }
    if let Some(Json::Array(content)) = json::get_mut(state, "content") {
        for item in content.iter_mut() {
            if json::get_str(item, "type") == Some("text") {
                redact_field(item, "text", "tool-output", id);
            } else {
                redact_field(item, "uri", "tool-file-uri", id);
                redact_field(item, "name", "tool-file-name", id);
                // `name: undefined` is written out, which the response's JSON codec encodes as `null`.
                if json::get(item, "name").is_none() {
                    json::set(item, "name", Json::Null);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_titles_count_up() {
        assert_eq!(fork_title(Some("notes")).as_deref(), Some("notes (fork #1)"));
        assert_eq!(fork_title(Some("notes (fork #1)")).as_deref(), Some("notes (fork #2)"));
        assert_eq!(fork_title(Some(" (fork #1)")).as_deref(), Some(" (fork #1) (fork #1)"));
        assert_eq!(fork_title(None), None);
    }
}
