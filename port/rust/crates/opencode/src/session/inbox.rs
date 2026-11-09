//! The session inbox (packages/core/src/session/inbox.ts, session/prompt.ts): admitted input waits in `session_inbox`
//! until a step boundary promotes it into a visible message. `steer` input joins the running turn at its next step;
//! `queue` input waits for the turn to end. Admission is `session.inbox.enqueued`, promotion
//! `session.inbox.delivered` (whose projection moves the row into `session_message`).
//!
//! TODO(port): attachments (files, agents, skills) and their media blobs, move items, the `session.prompt` plugin
//! hook.

use opencode_protocol::Json;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::app::{App, fatal};
use crate::bus::{Event, LocationRef};
use crate::http::Result;
use crate::json::{self, Obj};

/// Which pending input a promotion may take (`SessionInbox.Promotable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Promotable {
    /// Steers only (a step boundary mid-work).
    Steer,
    /// Also one queued input when no steer waits (the idle boundary).
    Input,
}

/// A pending row at a boundary (`SessionInbox.Promotion`).
#[derive(Debug, Clone)]
pub struct Promotion {
    pub id: String,
    pub ty: String,
    pub delivery: String,
}

impl Promotion {
    pub fn control(&self) -> bool {
        self.ty == "compaction" || self.ty == "move"
    }
}

#[derive(Debug, Clone)]
struct Row {
    id: String,
    ty: String,
    payload: String,
    delivery: String,
    time_created: i64,
}

impl Row {
    fn promotion(&self) -> Promotion {
        Promotion { id: self.id.clone(), ty: self.ty.clone(), delivery: self.delivery.clone() }
    }

    fn control(&self) -> bool {
        self.ty == "compaction" || self.ty == "move"
    }

    /// `Info` (`{id, sessionID, time, type, payload, delivery}`).
    fn info(&self, session: &str) -> Json {
        let mut time = Obj::new();
        time.int("created", self.time_created);
        let mut o = Obj::new();
        o.str("id", &self.id).str("sessionID", session).set("time", time.done()).str("type", &self.ty);
        o.set("payload", json::parse(&self.payload).unwrap_or_else(json::object));
        o.str("delivery", &self.delivery);
        o.done()
    }
}

fn rows(db: &Connection, sql: &str, args: impl rusqlite::Params) -> rusqlite::Result<Vec<Row>> {
    let mut statement = db.prepare(sql)?;
    statement
        .query_map(args, |r| {
            Ok(Row { id: r.get(0)?, ty: r.get(1)?, payload: r.get(2)?, delivery: r.get(3)?, time_created: r.get(4)? })
        })?
        .collect()
}

const COLUMNS: &str = "id, type, payload, delivery, time_created";

fn find(db: &Connection, id: &str) -> rusqlite::Result<Option<(String, Row)>> {
    db.query_row(&format!("SELECT {COLUMNS}, session_id FROM session_inbox WHERE id = ?1"), params![id], |r| {
        Ok((r.get(5)?, Row { id: r.get(0)?, ty: r.get(1)?, payload: r.get(2)?, delivery: r.get(3)?, time_created: r.get(4)? }))
    })
    .optional()
}

/// Pending steers in delivery order; a compaction before a move moves first (`pendingSteers`).
fn pending_steers(db: &Connection, session: &str) -> rusqlite::Result<Vec<Row>> {
    let mut steers = rows(
        db,
        &format!("SELECT {COLUMNS} FROM session_inbox WHERE session_id = ?1 AND delivery = 'steer' ORDER BY enqueued_seq ASC"),
        params![session],
    )?;
    if let Some(control) = steers.iter().position(Row::control) {
        if control > 0 && steers[control].ty == "compaction" {
            let row = steers.remove(control);
            steers.insert(0, row);
        }
    }
    Ok(steers)
}

fn first_queued(db: &Connection, session: &str) -> rusqlite::Result<Option<Row>> {
    Ok(rows(
        db,
        &format!(
            "SELECT {COLUMNS} FROM session_inbox WHERE session_id = ?1 AND delivery = 'queue' ORDER BY enqueued_seq ASC LIMIT 1"
        ),
        params![session],
    )?
    .into_iter()
    .next())
}

fn before_control(rows: Vec<Row>) -> Vec<Row> {
    match rows.iter().position(Row::control) {
        Some(index) => rows.into_iter().take(index).collect(),
        None => rows,
    }
}

/// `SessionInbox.nextPromotable`.
pub fn next_promotable(db: &Connection, session: &str, promotable: Promotable) -> rusqlite::Result<Option<Promotion>> {
    if let Some(steer) = pending_steers(db, session)?.first() {
        return Ok(Some(steer.promotion()));
    }
    if promotable != Promotable::Input {
        return Ok(None);
    }
    Ok(first_queued(db, session)?.map(|row| row.promotion()))
}

/// `SessionInbox.has`: whether input of a scope waits (`None` is any delivery).
pub fn has(db: &Connection, session: &str, delivery: Option<&str>) -> rusqlite::Result<bool> {
    let found: Option<i64> = match delivery {
        None => db
            .query_row(
                "SELECT 1 FROM session_inbox WHERE session_id = ?1 AND delivery IN ('steer', 'queue') LIMIT 1",
                params![session],
                |r| r.get(0),
            )
            .optional()?,
        Some(delivery) => db
            .query_row(
                "SELECT 1 FROM session_inbox WHERE session_id = ?1 AND delivery = ?2 LIMIT 1",
                params![session, delivery],
                |r| r.get(0),
            )
            .optional()?,
    };
    Ok(found.is_some())
}

/// `SessionInbox.list`: pending input in admission order.
pub fn list(db: &Connection, session: &str) -> rusqlite::Result<Vec<Json>> {
    Ok(rows(
        db,
        &format!("SELECT {COLUMNS} FROM session_inbox WHERE session_id = ?1 ORDER BY enqueued_seq ASC"),
        params![session],
    )?
    .iter()
    .map(|row| row.info(session))
    .collect())
}

/// A lifecycle conflict: the ID belongs to another record (`SessionInbox.LifecycleConflict`).
#[derive(Debug)]
pub struct Conflict;

/// A delivered input, from its message row (`promotedFromMessage`): `Err` when the ID is another kind of record.
fn promoted(db: &Connection, session: &str, id: &str, delivery: &str) -> rusqlite::Result<std::result::Result<Option<Json>, Conflict>> {
    let row: Option<(String, String, String, String)> = db
        .query_row(
            "SELECT session_id, type, data, id FROM session_message WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((owner, ty, data, _)) = row else { return Ok(Ok(None)) };
    if owner != session || (ty != "user" && ty != "synthetic") {
        return Ok(Err(Conflict));
    }
    let message = crate::session::message::from_row(id.to_owned(), ty.clone(), &data);
    let created = json::get(&message, "time").and_then(|t| json::get_num(t, "created")).unwrap_or(0.0) as i64;
    let mut payload = Obj::new();
    payload.str("text", json::get_str(&message, "text").unwrap_or_default());
    if ty == "user" {
        for key in ["files", "agents", "skills", "metadata"] {
            payload.opt(key, json::get(&message, key).cloned());
        }
    } else {
        for key in ["description", "metadata"] {
            payload.opt(key, json::get(&message, key).cloned());
        }
    }
    let mut time = Obj::new();
    time.int("created", created);
    let mut o = Obj::new();
    o.str("id", id).str("sessionID", session).set("time", time.done()).str("type", &ty);
    o.set("payload", payload.done()).str("delivery", delivery);
    Ok(Ok(Some(o.done())))
}

/// `reconcile`: the first admission of an ID wins for the same session and type.
pub fn reconcile(
    db: &Connection,
    id: &str,
    session: &str,
    ty: &str,
    delivery: &str,
) -> rusqlite::Result<std::result::Result<Option<Json>, Conflict>> {
    if let Some((owner, row)) = find(db, id)? {
        if row.ty == "compaction" || owner != session || row.ty != ty {
            return Ok(Err(Conflict));
        }
        return Ok(Ok(Some(row.info(session))));
    }
    match promoted(db, session, id, delivery)? {
        Ok(Some(existing)) if json::get_str(&existing, "type") != Some(ty) => Ok(Err(Conflict)),
        other => Ok(other),
    }
}

/// `admit`: records an input item (`{type, payload, delivery}`) unless its ID was already admitted.
pub fn admit(
    app: &App,
    id: &str,
    session: &str,
    item: Json,
    location: Option<LocationRef>,
) -> Result<std::result::Result<Json, Conflict>> {
    let ty = json::get_str(&item, "type").unwrap_or_default().to_owned();
    let delivery = json::get_str(&item, "delivery").unwrap_or_default().to_owned();
    {
        let db = app.db.lock().unwrap();
        match reconcile(&db, id, session, &ty, &delivery).map_err(fatal)? {
            Ok(Some(existing)) => return Ok(Ok(existing)),
            Ok(None) => {}
            Err(conflict) => return Ok(Err(conflict)),
        }
    }
    let mut data = Obj::new();
    data.str("sessionID", session).str("inboxID", id).set("item", item.clone());
    match crate::session::publish_event(app, "session.inbox.enqueued", data.done(), location, None) {
        Ok(event) => {
            let mut time = Obj::new();
            time.int("created", event.created);
            let mut o = Obj::new();
            o.str("id", id).str("sessionID", session).set("time", time.done());
            if let Json::Object(fields) = item {
                for (key, value) in fields {
                    o.set(&key, value);
                }
            }
            Ok(Ok(o.done()))
        }
        // The projection found the ID taken (a concurrent admission, or a message with this ID).
        Err(_) => {
            let db = app.db.lock().unwrap();
            match find(&db, id).map_err(fatal)? {
                Some((owner, row)) if owner == session && row.ty == ty => Ok(Ok(row.info(session))),
                _ => Ok(Err(Conflict)),
            }
        }
    }
}

/// `projectAdmitted` (`session.inbox.enqueued`).
pub fn project_admitted(tx: &Transaction, event: &Event) -> rusqlite::Result<()> {
    let data = &event.data;
    let id = json::get_str(data, "inboxID").unwrap_or_default();
    let session = json::get_str(data, "sessionID").unwrap_or_default();
    let exists: Option<i64> =
        tx.query_row("SELECT 1 FROM session_message WHERE id = ?1", params![id], |r| r.get(0)).optional()?;
    if exists.is_some() {
        return Err(conflict_error());
    }
    let item = json::get(data, "item").cloned().unwrap_or_else(json::object);
    let payload = json::get(&item, "payload").cloned().unwrap_or_else(json::object);
    let inserted = tx.execute(
        "INSERT INTO session_inbox (id, session_id, type, payload, delivery, enqueued_seq, time_created) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT DO NOTHING",
        params![
            id,
            session,
            json::get_str(&item, "type").unwrap_or_default(),
            json::text(&payload),
            json::get_str(&item, "delivery").unwrap_or_default(),
            event.durable.as_ref().map(|d| d.seq).unwrap_or(0),
            event.created,
        ],
    )?;
    if inserted == 0 {
        return Err(conflict_error());
    }
    tx.execute("UPDATE session_v2 SET time_updated = ?1 WHERE id = ?2", params![event.created, session])?;
    Ok(())
}

fn conflict_error() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT), Some("inbox lifecycle conflict".into()))
}

/// `session.inbox.delivered`: the row becomes a message (`projectDelivered` and the projector's insert).
pub fn project_delivered(tx: &Transaction, event: &Event) -> rusqlite::Result<()> {
    let data = &event.data;
    let id = json::get_str(data, "inboxID").unwrap_or_default();
    let session = json::get_str(data, "sessionID").unwrap_or_default();
    let row: Option<(String, String)> = tx
        .query_row(
            "DELETE FROM session_inbox WHERE id = ?1 AND session_id = ?2 RETURNING type, payload",
            params![id, session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((ty, payload)) = row else { return Err(conflict_error()) };
    let payload = json::parse(&payload).unwrap_or_else(json::object);
    let mut time = Obj::new();
    time.int("created", event.created);
    let message = match ty.as_str() {
        "user" => {
            let mut o = Obj::new();
            o.str("id", id).str("type", "user");
            o.opt("metadata", json::get(&payload, "metadata").cloned());
            o.str("text", json::get_str(&payload, "text").unwrap_or_default());
            for key in ["files", "agents", "skills"] {
                o.opt(key, json::get(&payload, key).cloned());
            }
            o.set("time", time.done());
            o.done()
        }
        "synthetic" => {
            let mut o = Obj::new();
            o.str("id", id).str("type", "synthetic");
            o.str("text", json::get_str(&payload, "text").unwrap_or_default());
            o.opt("description", json::get(&payload, "description").cloned());
            o.opt("metadata", json::get(&payload, "metadata").cloned());
            o.set("time", time.done());
            o.done()
        }
        _ => return Ok(()),
    };
    crate::session::message::insert(tx, event, &message)
}

/// `session.inbox.cancelled`.
pub fn project_cancelled(tx: &Transaction, event: &Event) -> rusqlite::Result<()> {
    let data = &event.data;
    let deleted = tx.execute(
        "DELETE FROM session_inbox WHERE id = ?1 AND session_id = ?2 AND delivery IN ('queue', 'steer')",
        params![json::get_str(data, "inboxID"), json::get_str(data, "sessionID")],
    )?;
    if deleted == 0 {
        return Err(conflict_error());
    }
    Ok(())
}

/// `session.inbox.delivery.changed`.
pub fn project_delivery_changed(tx: &Transaction, event: &Event) -> rusqlite::Result<()> {
    let data = &event.data;
    let to = json::get_str(data, "delivery").unwrap_or_default();
    let from = if to == "steer" { "queue" } else { "steer" };
    let updated = tx.execute(
        "UPDATE session_inbox SET delivery = ?1 WHERE id = ?2 AND session_id = ?3 AND delivery = ?4",
        params![to, json::get_str(data, "inboxID"), json::get_str(data, "sessionID"), from],
    )?;
    if updated == 0 {
        return Err(conflict_error());
    }
    Ok(())
}

fn deliver(app: &App, session: &str, rows: &[Row], location: &LocationRef) -> Result<usize> {
    for row in rows {
        if row.ty != "user" && row.ty != "synthetic" && row.ty != "move" {
            continue;
        }
        let mut data = Obj::new();
        data.str("sessionID", session).str("inboxID", &row.id);
        if crate::session::publish_event(app, "session.inbox.delivered", data.done(), Some(location.clone()), None).is_err() {
            // Delivered concurrently: fine when the message exists.
            let db = app.db.lock().unwrap();
            if !matches!(promoted(&db, session, &row.id, &row.delivery).map_err(fatal)?, Ok(Some(_))) {
                return Err(crate::app::fatal(rusqlite::Error::QueryReturnedNoRows));
            }
        }
    }
    Ok(rows.len())
}

/// `SessionInbox.promote`: promotes pending input, returning how many, or `None` when a control item owns the
/// boundary.
///
/// Synchronous, so it is serialized against every other inbox change on the single-threaded runtime
/// (`SessionInbox.serialized`).
pub fn promote(app: &App, session: &str, scope: Promotable, location: &LocationRef) -> Result<Option<usize>> {
    let steers = pending_steers(&app.db.lock().unwrap(), session).map_err(fatal)?;
    if !steers.is_empty() || scope == Promotable::Steer {
        if steers.first().is_some_and(Row::control) {
            return Ok(None);
        }
        return deliver(app, session, &before_control(steers), location).map(Some);
    }
    let Some(queued) = first_queued(&app.db.lock().unwrap(), session).map_err(fatal)? else { return Ok(Some(0)) };
    if queued.control() {
        return Ok(None);
    }
    let promoted = deliver(app, session, std::slice::from_ref(&queued), location)?;
    let steers = pending_steers(&app.db.lock().unwrap(), session).map_err(fatal)?;
    Ok(Some(promoted + deliver(app, session, &before_control(steers), location)?))
}

/// `cancel` / `steer` / `queue` of a pending item: `Err` when it is not pending in this session.
pub fn mutate(app: &App, session: &str, id: &str, ty: &'static str, delivery: Option<&str>) -> std::result::Result<(), Conflict> {
    let mut data = Obj::new();
    data.str("sessionID", session).str("inboxID", id);
    if let Some(delivery) = delivery {
        data.str("delivery", delivery);
    }
    crate::session::publish_event(app, ty, data.done(), None, None).map(drop).map_err(|_| Conflict)
}
