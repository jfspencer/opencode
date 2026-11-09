//! Staged reverts (packages/core/src/session/revert.ts, the `revert` operations of session/session.ts, and their
//! projection in session/projector.ts).
//!
//! Staging a revert at a message undoes the file changes of every step after it (each path back to the start
//! snapshot of the first step that changed it) and records the boundary, the snapshot of the files as they were
//! (`snapshot`) and the diff from there to the reverted files. Moving the boundary first puts the previously
//! reverted files back. Clearing restores the files the revert touched and wakes the session; committing (also done
//! by the next prompt or compaction request) drops the messages from the boundary on, with the inbox items admitted
//! after it and the instruction states and blob references they recorded.

use indexmap::IndexMap;
use opencode_protocol::Json;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::app::{App, fatal};
use crate::bus::{Event, LocationRef};
use crate::http::{Failure, Result};
use crate::json::{self, Obj};
use crate::session::{self, Stored};
use crate::snapshot;

/// `SessionBusyError` (409).
pub fn busy(session_id: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "SessionBusyError").str("sessionID", session_id).str("message", &format!("Session is busy: {session_id}"));
    Failure::new(409, o.done())
}

/// `MessageNotFoundError` (404).
pub fn missing_message(session_id: &str, message_id: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "MessageNotFoundError").str("sessionID", session_id).str("messageID", message_id);
    o.str("message", &format!("Message not found: {message_id}"));
    Failure::new(404, o.done())
}

/// `failedSnapshot` (server/src/handlers/session-error.ts): snapshot stores are host state clients cannot repair, so
/// only a log reference is surfaced.
pub fn failed_snapshot(operation: &str, session_id: &str, error: &snapshot::Error) -> Failure {
    let reference = format!("err_{}", &crate::id::uuid()[..8]);
    eprintln!("failed to {operation} ({session_id}, {reference}): {}: {}", error.operation, error.message);
    let mut o = Obj::new();
    o.str("_tag", "UnknownError").str("message", "Unexpected server error. Check server logs for details.").str("ref", &reference);
    Failure::new(500, o.done())
}

/// The session, unless it is executing (revert operations refuse a busy session).
fn idle(app: &App, session_id: &str) -> Result<Stored> {
    let stored = session::require(&app.db.lock().unwrap(), session_id)?;
    if app.sessions.executions.is_active(session_id) {
        return Err(busy(session_id));
    }
    Ok(stored)
}

fn staged(stored: &Stored) -> Option<&Json> {
    json::get(&stored.info, "revert").filter(|r| !matches!(r, Json::Null))
}

/// The paths a staged revert's diff lists.
fn reverted_files(revert: &Json) -> Vec<String> {
    json::get(revert, "files")
        .and_then(json::array)
        .unwrap_or_default()
        .iter()
        .filter_map(|file| json::get_str(file, "file").map(str::to_owned))
        .collect()
}

/// `Session.revert.stage`.
pub async fn stage(app: &App, session_id: &str, message_id: &str, files: Option<bool>) -> Result<Json> {
    let stored = idle(app, session_id)?;
    let location = app.locations.get(app, &stored.location).await?;
    let revert = staged(&stored);
    let original = match revert.and_then(|r| json::get_str(r, "snapshot")) {
        Some(snapshot) => Some(snapshot.to_owned()),
        None => snapshot::capture(&location).await,
    };
    let next = plan(&app.db.lock().unwrap(), session_id, message_id)
        .map_err(fatal)?
        .ok_or_else(|| missing_message(session_id, message_id))?;
    let mut restore: IndexMap<String, String> = IndexMap::new();
    if let Some(original) = &original {
        for file in revert.map(reverted_files).unwrap_or_default() {
            restore.insert(file, original.clone());
        }
    }
    if files != Some(false) {
        for (file, tree) in &next {
            restore.insert(file.clone(), tree.clone());
        }
    }
    let failed = |error: snapshot::Error| failed_snapshot("stage session revert", session_id, &error);
    if !restore.is_empty() {
        snapshot::restore(&location, &restore).await.map_err(failed)?;
    }
    let paths: Vec<String> = if files == Some(false) { vec![] } else { next.keys().cloned().collect() };
    let diffs = match &original {
        Some(original) => {
            let to = snapshot::capture(&location).await.unwrap_or_else(|| original.clone());
            snapshot::diff(&location, original, &to, None, Some(&paths)).await.map_err(failed)?
        }
        None => vec![],
    };
    let mut value = Obj::new();
    value.str("messageID", message_id).opt_str("snapshot", original.as_deref());
    value.set("files", Json::Array(diffs.iter().map(|d| d.to_json()).collect()));
    let value = value.done();
    let mut data = Obj::new();
    data.str("sessionID", session_id).set("revert", value.clone());
    session::publish_event(app, "session.revert.staged", data.done(), Some(stored.location.clone()), None).map_err(fatal)?;
    Ok(value)
}

/// `Session.revert.clear`: the files the staged revert touched are restored, then the session is woken.
pub async fn clear(app: &App, session_id: &str) -> Result<()> {
    let stored = idle(app, session_id)?;
    let location = app.locations.get(app, &stored.location).await?;
    if let Some(revert) = staged(&stored) {
        if let Some(original) = json::get_str(revert, "snapshot") {
            let files: IndexMap<String, String> =
                reverted_files(revert).into_iter().map(|file| (file, original.to_owned())).collect();
            snapshot::restore(&location, &files)
                .await
                .map_err(|error| failed_snapshot("clear session revert", session_id, &error))?;
        }
        let mut data = Obj::new();
        data.str("sessionID", session_id);
        session::publish_event(app, "session.revert.cleared", data.done(), Some(stored.location.clone()), None).map_err(fatal)?;
    }
    app.sessions.executions.wake(app, session_id, session::inbox::Promotable::Input);
    Ok(())
}

/// `Session.revert.commit` (the endpoint).
pub fn commit_staged(app: &App, session_id: &str) -> Result<()> {
    let stored = idle(app, session_id)?;
    commit(app, &stored, None)
}

/// `SessionRevert.commit`: publishes the commit of a staged revert, if any. `location` is the location the commit is
/// published in (a prompt's; none for the endpoint and compaction requests).
pub fn commit(app: &App, stored: &Stored, location: Option<LocationRef>) -> Result<()> {
    let Some(to) = staged(stored).and_then(|r| json::get_str(r, "messageID")) else { return Ok(()) };
    let mut data = Obj::new();
    data.str("sessionID", stored.id()).str("to", to);
    session::publish_event(app, "session.revert.committed", data.done(), location, None).map_err(fatal)?;
    Ok(())
}

/// `SessionRevert.plan`: each path the steps after the boundary changed, with the start snapshot of the first step
/// that changed it. `None` when the boundary is not a message of the session.
fn plan(db: &Connection, session_id: &str, message_id: &str) -> rusqlite::Result<Option<IndexMap<String, String>>> {
    let Some(boundary) = seq(db, session_id, message_id)? else { return Ok(None) };
    let mut statement = db.prepare(
        "SELECT id, type, data FROM session_message WHERE session_id = ?1 AND type = 'assistant' AND seq > ?2 ORDER BY seq ASC",
    )?;
    let rows = statement.query_map(params![session_id, boundary], |row| {
        Ok(session::message::from_row(row.get(0)?, row.get(1)?, &row.get::<_, String>(2)?))
    })?;
    let mut files = IndexMap::new();
    for message in rows {
        let message = message?;
        let snapshot = json::get(&message, "snapshot");
        let Some(start) = snapshot.and_then(|s| json::get_str(s, "start")) else { continue };
        for file in snapshot.and_then(|s| json::get(s, "files")).and_then(json::array).unwrap_or_default() {
            if let Some(file) = json::as_str(file) {
                files.entry(file.to_owned()).or_insert_with(|| start.to_owned());
            }
        }
    }
    Ok(Some(files))
}

fn seq(db: &Connection, session_id: &str, message_id: &str) -> rusqlite::Result<Option<i64>> {
    db.query_row(
        "SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2",
        params![session_id, message_id],
        |row| row.get(0),
    )
    .optional()
}

/// The projection of the revert events into the session's row (and, for a commit, its transcript).
pub fn project(tx: &Transaction, event: &Event) -> rusqlite::Result<()> {
    let data = &event.data;
    let id = json::get_str(data, "sessionID").unwrap_or_default();
    let set = |revert: Option<String>| {
        tx.execute("UPDATE session_v2 SET revert = ?1, time_updated = ?2 WHERE id = ?3", params![revert, event.created, id])
    };
    match event.ty {
        "session.revert.staged" => {
            set(json::get(data, "revert").map(json::text))?;
        }
        "session.revert.cleared" => {
            set(None)?;
        }
        "session.revert.committed" => {
            let to = json::get_str(data, "to").unwrap_or_default();
            let Some(boundary) = seq(tx, id, to)? else {
                eprintln!("Revert boundary message not found: {to}");
                return Err(rusqlite::Error::QueryReturnedNoRows);
            };
            tx.execute("DELETE FROM session_message WHERE session_id = ?1 AND seq >= ?2", params![id, boundary])?;
            tx.execute("DELETE FROM session_inbox WHERE session_id = ?1 AND enqueued_seq >= ?2", params![id, boundary])?;
            set(None)?;
            // `InstructionState.rewind`: states recorded through the dropped history go with it.
            tx.execute("DELETE FROM instruction_state WHERE session_id = ?1 AND through_seq >= ?2", params![id, boundary])?;
            // `SessionBlob.releaseFrom`: the blob references from the boundary on, then the blobs nothing names.
            let hashes: Vec<String> = {
                let mut statement =
                    tx.prepare("DELETE FROM blob_ref WHERE session_id = ?1 AND seq >= ?2 RETURNING hash")?;
                statement.query_map(params![id, boundary], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
            };
            let mut seen = std::collections::HashSet::new();
            for hash in hashes.into_iter().filter(|h| seen.insert(h.clone())) {
                tx.execute(
                    "DELETE FROM blob WHERE hash = ?1 AND NOT EXISTS (SELECT 1 FROM blob_ref WHERE blob_ref.hash = blob.hash)",
                    params![hash],
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}
