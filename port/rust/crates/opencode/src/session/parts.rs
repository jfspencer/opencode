//! Assistant parts stored as their own `session_part` rows (packages/core/src/session/projector.ts: `insertMessage`,
//! `messageWrites`' part rows, `partRowWrites`; session/history.ts `encodePart`, `decodePartRowSync`).
//!
//! Any assistant inserted with content keeps an empty envelope in `session_message` and its parts here. With
//! `experimental.part_rows` on, every assistant part is written here as it streams (a legacy inline row moves here
//! first, [`prepare`]); with it off, a message whose parts already live here is written back into those rows
//! ([`write`]). Reads fold the rows into `content` ([`crate::session::blob::with_parts`]).

use opencode_protocol::types as t;
use opencode_protocol::{Json, Wire};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::bus::Event;
use crate::json::{self, Obj};
use crate::session::blob;

/// `SessionHistory.encodePart`: a part's stored form (the value unchanged when it does not decode).
pub fn encode(part: &Json) -> Json {
    match t::SessionMessageAssistantContentItem::decode(part) {
        Ok(item) => json::of(&item),
        Err(_) => part.clone(),
    }
}

/// `decodePartRowSync`: a row's part, its column type filling in a missing `type`, legacy fields upgraded.
fn decode(ty: &str, text: &str) -> Json {
    let mut part = json::parse(text).unwrap_or(Json::Null);
    json::set(&mut part, "type", json::string(ty));
    crate::session::transfer::legacy_part(part)
}

fn call_id(part: &Json) -> Option<&str> {
    (json::get_str(part, "type") == Some("tool")).then(|| json::get_str(part, "id")).flatten()
}

/// One `session_part` row: its ordinal and its data as stored.
pub struct Row {
    ordinal: i64,
    data: String,
}

/// A message's part rows in ordinal order (`partRows`).
pub fn rows(db: &Connection, message_id: &str) -> rusqlite::Result<Vec<Row>> {
    let mut statement = db.prepare("SELECT ordinal, data FROM session_part WHERE message_id = ?1 ORDER BY ordinal ASC")?;
    statement.query_map(params![message_id], |row| Ok(Row { ordinal: row.get(0)?, data: row.get(1)? }))?.collect()
}

/// `insertParts`: `parts` (already encoded) from `first` on.
pub fn insert(
    tx: &Transaction,
    session: &str,
    message_id: &str,
    first: i64,
    parts: &[Json],
    time_created: i64,
    time_updated: i64,
) -> rusqlite::Result<()> {
    for (index, part) in parts.iter().enumerate() {
        tx.execute(
            "INSERT INTO session_part (message_id, session_id, ordinal, type, call_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message_id,
                session,
                first + index as i64,
                json::get_str(part, "type").unwrap_or_default(),
                call_id(part),
                time_created,
                time_updated,
                json::text(part)
            ],
        )?;
    }
    Ok(())
}

/// `writeParts` (`experimental.part_rows` off): writes `content` over a message's existing part rows, touching only
/// the rows that changed (at `created`, the event's time).
pub fn write(tx: &Transaction, created: i64, session: &str, message_id: &str, existing: &[Row], content: &[Json]) -> rusqlite::Result<()> {
    let mut next = existing.last().map_or(0, |row| row.ordinal + 1);
    for (index, part) in content.iter().enumerate() {
        let ty = json::get_str(part, "type").unwrap_or_default();
        match existing.get(index) {
            None => {
                insert(tx, session, message_id, next, std::slice::from_ref(part), created, created)?;
                next += 1;
            }
            Some(current) if json::parse(&current.data).map(|data| json::text(&data)) != Some(json::text(part)) => {
                tx.execute(
                    "UPDATE session_part SET type = ?1, call_id = ?2, data = ?3, time_updated = ?4 WHERE message_id = ?5 AND ordinal = ?6",
                    params![ty, call_id(part), json::text(part), created, message_id, current.ordinal],
                )?;
            }
            Some(_) => {}
        }
    }
    for stale in existing.iter().skip(content.len()) {
        tx.execute("DELETE FROM session_part WHERE message_id = ?1 AND ordinal = ?2", params![message_id, stale.ordinal])?;
    }
    Ok(())
}

/// `prepareAssistantParts` (`experimental.part_rows` on): the only place a legacy inline assistant row moves to
/// `session_part`, right before its parts are written; `carry: false` drops the inline parts instead, for a write that
/// replaces them all. Returns the message's sequence (its parts' ref owner), `None` when it is not an assistant here.
pub fn prepare(tx: &Transaction, event: &Event, session: &str, message_id: &str, carry: bool) -> rusqlite::Result<Option<i64>> {
    let row: Option<(i64, Option<i64>)> = tx
        .query_row(
            "SELECT seq, json_array_length(data, '$.content') FROM session_message WHERE id = ?1 AND session_id = ?2 AND type = 'assistant'",
            params![message_id, session],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((seq, inline)) = row else { return Ok(None) };
    if inline.unwrap_or(0) == 0 {
        return Ok(Some(seq));
    }
    let full: Option<(String, i64)> = tx
        .query_row("SELECT data, time_created FROM session_message WHERE id = ?1", params![message_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let Some((data, time_created)) = full else { return Ok(Some(seq)) };
    let mut message = crate::session::message::from_row(message_id.to_owned(), "assistant".into(), &data);
    let content = match json::remove(&mut message, "content") {
        Some(Json::Array(content)) => content,
        _ => vec![],
    };
    if carry {
        // An older build that wrote inline content while part rows existed leaves both shapes on one message: the
        // rows stay, the inline parts follow them.
        let last: Option<i64> =
            tx.query_row("SELECT max(ordinal) FROM session_part WHERE message_id = ?1", params![message_id], |row| row.get(0))?;
        let parts: Vec<Json> = content.iter().map(|part| encode(&crate::session::transfer::legacy_part(part.clone()))).collect();
        insert(tx, session, message_id, last.map_or(0, |last| last + 1), &parts, time_created, event.created)?;
    }
    json::set(&mut message, "content", Json::Array(vec![]));
    let (_, data) = crate::session::message::stored(&message);
    tx.execute(
        "UPDATE session_message SET data = ?1, time_updated = ?2 WHERE id = ?3 AND session_id = ?4",
        params![data, crate::id::now(), message_id, session],
    )?;
    Ok(Some(seq))
}

fn content_of(part: &Json) -> Json {
    let mut owner = Obj::new();
    owner.set("content", Json::Array(vec![part.clone()]));
    owner.done()
}

/// `appendAssistantPart` (`experimental.part_rows` on).
pub fn append(tx: &Transaction, event: &Event, session: &str, message_id: &str, part: Json) -> rusqlite::Result<()> {
    let Some(seq) = prepare(tx, event, session, message_id, true)? else { return Ok(()) };
    let mut stored = encode(&part);
    blob::externalize_part(tx, &mut stored)?;
    let data = encode(&stored);
    let next: i64 = tx.query_row(
        "SELECT coalesce(max(ordinal) + 1, 0) FROM session_part WHERE message_id = ?1",
        params![message_id],
        |row| row.get(0),
    )?;
    insert(tx, session, message_id, next, std::slice::from_ref(&data), event.created, event.created)?;
    blob::record_refs(tx, session, message_id, seq, &content_of(&data))
}

/// Which part an update targets (`updateAssistantPart`'s match): the last of its type, with this call ID, or an
/// incomplete reasoning.
pub struct Match<'a> {
    pub kind: &'a str,
    pub call_id: Option<&'a str>,
    pub incomplete_reasoning: bool,
}

/// `updateAssistantPart` (`experimental.part_rows` on).
pub fn update(tx: &Transaction, event: &Event, session: &str, message_id: &str, target: Match, recipe: impl FnOnce(&mut Json)) -> rusqlite::Result<()> {
    let Some(seq) = prepare(tx, event, session, message_id, true)? else { return Ok(()) };
    let mut sql = String::from("SELECT ordinal, type, data FROM session_part WHERE message_id = ?1 AND session_id = ?2 AND type = ?3");
    if target.call_id.is_some() {
        sql.push_str(" AND call_id = ?4");
    }
    if target.incomplete_reasoning {
        sql.push_str(" AND json_extract(data, '$.time.completed') IS NULL");
    }
    sql.push_str(" ORDER BY ordinal DESC LIMIT 1");
    let found: Option<(i64, String, String)> = match target.call_id {
        Some(call) => tx.query_row(&sql, params![message_id, session, target.kind, call], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))),
        None => tx.query_row(&sql, params![message_id, session, target.kind], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))),
    }
    .optional()?;
    let Some((ordinal, ty, text)) = found else { return Ok(()) };
    let current = decode(&ty, &text);
    let mut updated = current.clone();
    recipe(&mut updated);
    if updated == current {
        return Ok(());
    }
    let mut stored = encode(&updated);
    // Streaming updates move nothing, and skip the ref tables.
    let moved = blob::externalize_part(tx, &mut stored)?;
    let data = encode(&stored);
    tx.execute(
        "UPDATE session_part SET data = ?1, time_updated = ?2 WHERE message_id = ?3 AND ordinal = ?4",
        params![json::text(&data), event.created, message_id, ordinal],
    )?;
    if moved {
        blob::record_refs(tx, session, message_id, seq, &content_of(&data))?;
    }
    Ok(())
}
