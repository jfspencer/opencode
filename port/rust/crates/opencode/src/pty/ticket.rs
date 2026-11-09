//! Connect tickets (packages/core/src/pty/ticket.ts): browsers cannot set headers on a WebSocket upgrade, so a client
//! mints a single-use ticket over an authenticated request and presents it in the upgrade's query instead of the
//! password. A ticket authorizes one upgrade to one terminal, from the location (directory and workspace) it was minted
//! in; it expires after 60 seconds, and at most 10,000 are kept.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use opencode_protocol::Json;

use crate::json::Obj;

/// `DEFAULT_TTL`.
const TTL: Duration = Duration::from_secs(60);
const CAPACITY: usize = 10_000;
/// `PTY_CONNECT_TICKET_QUERY`.
pub const QUERY: &str = "ticket";

/// What a ticket is good for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    /// `pty:<id>`.
    pub target: String,
    pub directory: Option<String>,
    pub workspace_id: Option<String>,
}

impl Scope {
    pub fn pty(id: &str, directory: Option<String>, workspace_id: Option<String>) -> Self {
        Self { target: format!("pty:{id}"), directory, workspace_id }
    }
}

static TICKETS: Mutex<Option<IndexMap<String, (Scope, Instant)>>> = Mutex::new(None);

/// `PtyTicket.issue`: `{ticket, expires_in}`.
pub fn issue(scope: Scope) -> Json {
    let ticket = crate::id::uuid();
    let now = Instant::now();
    let mut guard = TICKETS.lock().unwrap();
    let tickets = guard.get_or_insert_with(IndexMap::new);
    tickets.retain(|_, (_, expires)| *expires > now);
    while tickets.len() >= CAPACITY {
        tickets.shift_remove_index(0);
    }
    tickets.insert(ticket.clone(), (scope, now + TTL));
    let mut o = Obj::new();
    o.str("ticket", &ticket).int("expires_in", TTL.as_secs() as i64);
    o.done()
}

/// `PtyTicket.consume`: whether the ticket is live and its scope matches; a match uses it up.
pub fn consume(ticket: &str, scope: &Scope) -> bool {
    let mut guard = TICKETS.lock().unwrap();
    let Some(tickets) = guard.as_mut() else { return false };
    match tickets.get(ticket) {
        Some((_, expires)) if *expires <= Instant::now() => {
            tickets.shift_remove(ticket);
            false
        }
        Some((stored, _)) if stored == scope => {
            tickets.shift_remove(ticket);
            true
        }
        _ => false,
    }
}

/// Whether a request is a ticketed terminal connect (`/api/pty/:ptyID/connect` or
/// `/api/experimental/persistent-pty/:ptyID/connect` with a `ticket`), which skips the credential check: its handler
/// consumes the ticket (middleware/authorization.ts).
pub fn ticketed(path: &str, query: &[(String, String)]) -> bool {
    let id_then_connect = |rest: &str| rest.strip_suffix("/connect").is_some_and(|id| !id.is_empty() && !id.contains('/'));
    let connect = path.strip_prefix("/api/pty/").is_some_and(id_then_connect)
        || path.strip_prefix("/api/experimental/persistent-pty/").is_some_and(id_then_connect);
    connect && query.iter().find(|(key, _)| key == QUERY).is_some_and(|(_, value)| !value.is_empty())
}
