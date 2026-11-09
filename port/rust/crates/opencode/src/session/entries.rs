//! API-managed instruction entries (packages/core/src/session/instruction-entry.ts, the `instructions.entry.*`
//! endpoints): JSON values a client attaches to a session's instructions under a key. Each stored entry is an
//! instruction source keyed `api/<key>`, after the core ones; a removed entry stays as a tombstone so its removal is
//! announced to the model once.
//!
//! TODO(port): copying entries into a forked or imported session (`InstructionEntry.initialize`).

use opencode_protocol::Json;
use rusqlite::{Connection, params};

use crate::app::{App, fatal};
use crate::http::{Failure, Result};
use crate::json::{self, Obj};
use crate::session::instructions::Source;

const MAX_VALUE_BYTES: usize = 256 * 1024;

/// The session's entries in key order, removed ones included (with no value).
fn rows(db: &Connection, session: &str) -> rusqlite::Result<Vec<(String, Option<Json>, bool)>> {
    let mut statement =
        db.prepare("SELECT key, value, removed FROM instruction_entry WHERE session_id = ?1 ORDER BY key ASC")?;
    let rows = statement.query_map(params![session], |row| {
        let value: Option<String> = row.get(1)?;
        Ok((row.get::<_, String>(0)?, value.and_then(|text| json::parse(&text)), row.get::<_, bool>(2)?))
    })?;
    rows.collect()
}

/// `InstructionEntry.load`: one source per stored entry.
pub fn sources(db: &Connection, session: &str) -> rusqlite::Result<Vec<Source>> {
    Ok(rows(db, session)?
        .into_iter()
        .map(|(key, value, removed)| Source::entry(&key, if removed { None } else { Some(value.unwrap_or(Json::Null)) }))
        .collect())
}

/// `InstructionEntry.list`: the entries in effect, as `{key, value}`.
pub fn list(db: &Connection, session: &str) -> rusqlite::Result<Json> {
    let entries = rows(db, session)?
        .into_iter()
        .filter(|(_, _, removed)| !removed)
        .map(|(key, value, _)| {
            let mut o = Obj::new();
            o.str("key", &key).set("value", value.unwrap_or(Json::Null));
            o.done()
        })
        .collect();
    Ok(Json::Array(entries))
}

/// `InstructionEntry.put`: stores the value (a JSON `null` is stored as SQL `NULL`); the row is only touched when the
/// value changes or the entry was removed.
pub fn put(app: &App, session: &str, key: &str, value: &Json) -> Result<()> {
    let text = json::text(value);
    if text.len() > MAX_VALUE_BYTES {
        let mut o = Obj::new();
        o.str("_tag", "InstructionEntryValueTooLargeError");
        o.int("actualBytes", text.len() as i64).int("maxBytes", MAX_VALUE_BYTES as i64);
        o.str("message", &format!("Instruction entry value is {} bytes; the limit is {MAX_VALUE_BYTES} bytes", text.len()));
        return Err(Failure::new(413, o.done()));
    }
    let stored = (!matches!(value, Json::Null)).then_some(text);
    let now = crate::id::now();
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO instruction_entry (session_id, key, value, removed, time_created, time_updated) \
             VALUES (?1, ?2, ?3, 0, ?4, ?4) \
             ON CONFLICT (session_id, key) DO UPDATE SET value = excluded.value, removed = 0, time_updated = ?4 \
             WHERE instruction_entry.removed = 1 OR instruction_entry.value IS NOT excluded.value",
            params![session, key, stored, now],
        )
        .map_err(fatal)?;
    Ok(())
}

/// `InstructionEntry.remove`: tombstones an entry in effect; removing an unknown key does nothing.
pub fn remove(app: &App, session: &str, key: &str) -> Result<()> {
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE instruction_entry SET value = NULL, removed = 1, time_updated = ?3 \
             WHERE session_id = ?1 AND key = ?2 AND removed = 0",
            params![session, key, crate::id::now()],
        )
        .map_err(fatal)?;
    Ok(())
}
