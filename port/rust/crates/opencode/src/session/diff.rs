//! Turn diffs (packages/core/src/session/diff.ts, `Session.diff`): the files changed by the turn containing a user
//! message.
//!
//! A turn runs from the first prompt after the session was last idle until the next idle marker, so prompts steered
//! in while it was busy belong to the same turn; `to` extends the range through the turn containing a later user
//! message. The range's first recorded start snapshot is compared with its last recorded end snapshot; only a step
//! still running in the active session compares against the working copy. An omitted `context` yields full-file
//! patches. A session without idle markers predates them: its prompts span until the next user message. Snapshot
//! trees live in the store of the location that captured them, so a range spanning a location switch is rejected.

use opencode_protocol::Json;
use rusqlite::{Connection, OptionalExtension, params};

use crate::app::{App, defect, fatal};
use crate::bus::LocationRef;
use crate::http::{Failure, Result};
use crate::json;
use crate::session::{self, revert};
use crate::snapshot;

/// `Session.TurnRangeError`, as the endpoint answers it.
fn range(field: &str, message: String) -> Failure {
    Failure::invalid(message, Some(field), None)
}

struct Step {
    seq: i64,
    start: Option<String>,
    end: Option<String>,
    completed: bool,
}

/// Where the snapshots of a range live, and the trees to compare.
struct Range {
    location: LocationRef,
    from: String,
    recorded: Option<String>,
    final_completed: bool,
}

/// `SessionDiff.turn` for `GET /api/session/:id/diff` (`from`, `to`, `context` from the query).
pub async fn turn(app: &App, session_id: &str, query: &Json) -> Result<Json> {
    let stored = session::require(&app.db.lock().unwrap(), session_id)?;
    let active = app.sessions.executions.is_active(session_id);
    let context = match json::get(query, "context") {
        Some(Json::Number(n)) => Some(*n as i64),
        Some(Json::String(s)) => s.parse::<f64>().ok().map(|n| n as i64),
        _ => None,
    };
    let range = resolve(&app.db.lock().unwrap(), &stored, json::get_str(query, "from"), json::get_str(query, "to"))?;
    let Some(range) = range else { return Ok(Json::Array(vec![])) };
    let location = app.locations.get(app, &range.location).await?;
    let running = active && !range.final_completed;
    let to = if running { snapshot::capture(&location).await.or(range.recorded) } else { range.recorded };
    let Some(to) = to else { return Ok(Json::Array(vec![])) };
    let diffs = snapshot::diff(&location, &range.from, &to, Some(context.unwrap_or(crate::vcs::PATCH_CONTEXT_LINES)), None)
        .await
        .map_err(|error| revert::failed_snapshot("diff session turn", session_id, &error))?;
    Ok(Json::Array(diffs.iter().map(|d| d.to_json()).collect()))
}

fn resolve(db: &Connection, stored: &session::Stored, from: Option<&str>, to: Option<&str>) -> Result<Option<Range>> {
    let session_id = stored.id();
    // Selecting only seq lets the (session_id, type, seq) index answer these without reading message rows.
    let boundaries = |ty: &str| -> Result<Vec<i64>> {
        let mut statement = db
            .prepare("SELECT seq FROM session_message WHERE session_id = ?1 AND type = ?2 ORDER BY seq ASC")
            .map_err(fatal)?;
        let rows = statement.query_map(params![session_id, ty], |row| row.get(0)).map_err(fatal)?;
        rows.collect::<rusqlite::Result<Vec<i64>>>().map_err(fatal)
    };
    let users = boundaries("user")?;
    let markers = boundaries("idle")?;
    let message = |field: &str, id: &str| -> Result<(String, i64)> {
        let row: Option<(String, String, i64)> = db
            .query_row(
                "SELECT id, type, seq FROM session_message WHERE id = ?1 AND session_id = ?2",
                params![id, session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(fatal)?;
        let Some((id, ty, seq)) = row else { return Err(revert::missing_message(session_id, id)) };
        if ty != "user" {
            return Err(range(field, format!("Message {id} is not a user message")));
        }
        Ok((id, seq))
    };
    let anchor = match from {
        Some(id) => Some(message("from", id)?),
        None => db
            .query_row(
                "SELECT id, seq FROM session_message WHERE session_id = ?1 AND type = 'user' ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(fatal)?,
    };
    let Some(anchor) = anchor else { return Ok(None) };
    let last = match to {
        Some(id) => message("to", id)?,
        None => anchor.clone(),
    };
    if last.1 < anchor.1 {
        return Err(range("to", format!("Message {} precedes {}", last.0, anchor.0)));
    }
    // Without any marker, history predates idle markers and a prompt's turn ends at the next prompt.
    let legacy = markers.is_empty();
    // The turn opens with the first prompt after the previous idle marker; the anchor itself is the latest candidate.
    let opened = markers.iter().rev().find(|seq| **seq < anchor.1).copied().unwrap_or(-1);
    let start = if legacy { anchor.1 } else { users.iter().find(|seq| **seq > opened).copied().unwrap_or(anchor.1) };
    let end = if legacy { users.iter().find(|seq| **seq > last.1) } else { markers.iter().find(|seq| **seq > last.1) };
    let steps: Vec<Step> = {
        let mut statement = db
            .prepare(
                "SELECT seq, json_extract(data, '$.snapshot.start'), json_extract(data, '$.snapshot.end'), \
                 json_extract(data, '$.time.completed') IS NOT NULL FROM session_message \
                 WHERE session_id = ?1 AND type = 'assistant' AND seq > ?2 AND (?3 IS NULL OR seq < ?3) ORDER BY seq ASC",
            )
            .map_err(fatal)?;
        let rows = statement
            .query_map(params![session_id, start, end.copied()], |row| {
                Ok(Step { seq: row.get(0)?, start: text(row.get_ref(1)?), end: text(row.get_ref(2)?), completed: row.get(3)? })
            })
            .map_err(fatal)?;
        rows.collect::<rusqlite::Result<_>>().map_err(fatal)?
    };
    let (Some(first), Some(last_step)) = (steps.first(), steps.last()) else { return Ok(None) };
    let Some(from) = steps.iter().find_map(|step| step.start.clone()) else { return Ok(None) };
    let switches: Vec<(i64, Option<String>, Option<String>)> = {
        let mut statement = db
            .prepare(
                "SELECT seq, json_extract(data, '$.location'), json_extract(data, '$.previous.location') \
                 FROM session_message WHERE session_id = ?1 AND type = 'location-switched' ORDER BY seq ASC",
            )
            .map_err(fatal)?;
        let rows = statement
            .query_map(params![session_id], |row| Ok((row.get(0)?, text(row.get_ref(1)?), text(row.get_ref(2)?))))
            .map_err(fatal)?;
        rows.collect::<rusqlite::Result<_>>().map_err(fatal)?
    };
    if switches.iter().any(|(seq, _, _)| *seq > first.seq && *seq < last_step.seq) {
        return Err(range("to", "Turn range spans a location change".into()));
    }
    let before = switches.iter().rev().find(|(seq, _, _)| *seq < first.seq).and_then(|s| s.1.clone());
    let after = switches.iter().find(|(seq, _, _)| *seq > first.seq).and_then(|s| s.2.clone());
    let location = match before.or(after) {
        Some(text) => decode_location(&text)?,
        None => stored.location.clone(),
    };
    let recorded = steps.iter().rev().find_map(|step| step.end.clone());
    Ok(Some(Range { location, from, recorded, final_completed: last_step.completed }))
}

/// A column that is text when present and truthy (`json_extract` of a string).
fn text(value: rusqlite::types::ValueRef<'_>) -> Option<String> {
    match value {
        rusqlite::types::ValueRef::Text(bytes) if !bytes.is_empty() => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

/// `Location.Ref` from its JSON text (a defect when it does not decode).
fn decode_location(text: &str) -> Result<LocationRef> {
    let value = json::parse(text).ok_or_else(|| defect(format!("invalid location: {text}")))?;
    let directory = json::get_str(&value, "directory").ok_or_else(|| defect(format!("invalid location: {text}")))?;
    Ok(LocationRef { directory: directory.to_owned(), workspace_id: json::get_str(&value, "workspaceID").map(str::to_owned) })
}
