//! The event bus (packages/core/src/bus.ts): every event is published here; durable events get the next sequence
//! number of their aggregate and are projected into the database in the same transaction; live subscribers (the
//! SSE feed) receive every event after it commits.
//!
//! Like `opencode serve` (whose bus is built without `persist`), durable payloads are not retained in the `event`
//! table: the per-aggregate sequence (`event_sequence`) is, so a session's log replays only its sync marker.

use std::sync::Mutex;

use opencode_protocol::Json;
use opencode_protocol::events::EVENTS;
use opencode_protocol::wire::Out;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use tokio::sync::broadcast;

use crate::json::{self, Obj};

/// A Location reference on an event envelope (`{directory, workspaceID?}`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocationRef {
    pub directory: String,
    pub workspace_id: Option<String>,
}

impl LocationRef {
    pub fn directory(directory: impl Into<String>) -> Self {
        Self { directory: directory.into(), workspace_id: None }
    }

    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        o.str("directory", &self.directory);
        if let Some(w) = &self.workspace_id {
            o.str("workspaceID", w);
        }
        o.done()
    }
}

#[derive(Debug, Clone)]
pub struct Durable {
    pub aggregate_id: String,
    pub seq: i64,
    pub version: u32,
}

/// An event payload (`Event.Payload`), in the key order the TS object is built with.
#[derive(Debug, Clone)]
pub struct Event {
    pub id: String,
    pub created: i64,
    pub ty: &'static str,
    pub location: Option<LocationRef>,
    pub data: Json,
    pub durable: Option<Durable>,
}

impl Event {
    /// `JSON.stringify(event)`.
    pub fn to_json_text(&self) -> String {
        let mut out = Out::new();
        {
            let mut o = out.object();
            o.key("id").string(&self.id);
            o.key("created").int(self.created);
            o.key("type").string(self.ty);
            if let Some(location) = &self.location {
                o.key("location").json(&location.to_json());
            }
            o.key("data").json(&self.data);
            if let Some(d) = &self.durable {
                let mut e = o.key("durable").object();
                e.key("aggregateID").string(&d.aggregate_id);
                e.key("seq").int(d.seq);
                e.key("version").int(d.version as i64);
            }
        }
        out.finish()
    }
}

#[derive(Default)]
pub struct PublishOptions {
    pub id: Option<String>,
    pub location: Option<LocationRef>,
}

pub struct Bus {
    live: broadcast::Sender<std::sync::Arc<Event>>,
    /// Serializes publication: the database lock already orders commits, and notification keeps that order.
    order: Mutex<()>,
}

/// The data field naming a durable event's aggregate (the manifest's `durable.aggregate`).
pub fn aggregate_field(ty: &str) -> &'static str {
    EVENTS.iter().find(|e| e.r#type == ty).and_then(|e| e.aggregate).unwrap_or_else(|| panic!("{ty} is not a durable event"))
}

/// Durable version of an event type from the manifest; `None` for ephemeral events.
pub fn durable_version(ty: &str) -> Option<u32> {
    EVENTS.iter().find(|e| e.r#type == ty).and_then(|e| e.version)
}

impl Bus {
    pub fn new() -> Self {
        // The SSE feed drops a subscriber that falls this far behind (packages/server/src/event-feed.ts).
        let (live, _) = broadcast::channel(4096);
        Self { live, order: Mutex::new(()) }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<std::sync::Arc<Event>> {
        self.live.subscribe()
    }

    fn make(ty: &'static str, data: Json, options: PublishOptions) -> Event {
        Event {
            id: options.id.unwrap_or_else(crate::id::event),
            created: crate::id::now(),
            ty,
            location: options.location,
            data,
            durable: None,
        }
    }

    /// Publishes an ephemeral event.
    pub fn emit(&self, ty: &'static str, data: Json, options: PublishOptions) -> Event {
        debug_assert!(durable_version(ty).is_none(), "{ty} is durable");
        let _order = self.order.lock().unwrap();
        let event = Self::make(ty, data, options);
        let _ = self.live.send(std::sync::Arc::new(event.clone()));
        event
    }

    /// Publishes a durable event of its aggregate: the projection runs in the commit's transaction (`project` gets
    /// it and the sequenced event).
    pub fn publish(
        &self,
        db: &mut Connection,
        ty: &'static str,
        data: Json,
        options: PublishOptions,
        project: impl FnOnce(&Transaction, &Event) -> rusqlite::Result<()>,
    ) -> rusqlite::Result<Event> {
        let version = durable_version(ty).unwrap_or_else(|| panic!("{ty} is not a durable event"));
        let field = aggregate_field(ty);
        let aggregate =
            json::get_str(&data, field).unwrap_or_else(|| panic!("{ty} without its aggregate {field}")).to_owned();
        let _order = self.order.lock().unwrap();
        let mut event = Self::make(ty, data, options);
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let latest = latest_sequence(&tx, &aggregate)?;
        let seq = latest + 1;
        event.durable = Some(Durable { aggregate_id: aggregate.clone(), seq, version });
        project(&tx, &event)?;
        tx.execute(
            "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?1, ?2) \
             ON CONFLICT (aggregate_id) DO UPDATE SET seq = max(event_sequence.seq, ?2)",
            params![aggregate, seq],
        )?;
        tx.commit()?;
        let _ = self.live.send(std::sync::Arc::new(event.clone()));
        Ok(event)
    }

    /// Forgets an aggregate's log (a deleted session).
    pub fn remove(&self, db: &mut Connection, aggregate: &str) -> rusqlite::Result<()> {
        let tx = db.transaction()?;
        tx.execute("DELETE FROM event_sequence WHERE aggregate_id = ?1", params![aggregate])?;
        tx.execute("DELETE FROM event WHERE aggregate_id = ?1", params![aggregate])?;
        tx.commit()
    }
}

pub fn latest_sequence(db: &Connection, aggregate: &str) -> rusqlite::Result<i64> {
    Ok(db
        .query_row("SELECT seq FROM event_sequence WHERE aggregate_id = ?1", params![aggregate], |row| row.get(0))
        .optional()?
        .unwrap_or(-1))
}
