//! Idle locations (packages/core/src/location-activity.ts): a booted location's last durable session activity is
//! tracked, and every minute the locations unused for longer than their idle timeout are shut down
//! ([`crate::location::Locations::invalidate`]); the next request boots them again.
//!
//! The timeout is the location's config `experimental.location_idle_timeout` (the last document that sets it; `false`
//! never unloads), else an hour. A location is kept (and counts as touched) while a session placed there executes or
//! owns running background work (waiting on a question, a long tool call or a job emits no durable activity), and
//! while a client connection retains it (an open terminal socket, [`Retain`]). A location the sweep finds untracked
//! counts as touched then.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;

use indexmap::IndexMap;
use opencode_protocol::types as t;
use opencode_protocol::wire::codec::Duration as Timeout;

use crate::app::App;
use crate::bus::LocationRef;
use crate::inventory::config::Entry;
use crate::location::{Location, canonical};

/// The default idle timeout (`timeToLive`).
const TIME_TO_LIVE_MS: f64 = 60.0 * 60_000.0;
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// When each booted location last saw durable session activity, in first-touch order (`entries`).
#[derive(Default)]
pub struct Activity {
    touched: Mutex<IndexMap<LocationRef, i64>>,
}

impl Activity {
    fn touch(&self, reference: LocationRef) {
        self.touched.lock().unwrap().insert(reference, crate::id::now());
    }
}

/// Keeps a location from the idle sweep for as long as it lives (`LocationLifecycle.retain`).
pub struct Retain(Arc<Location>);

impl Retain {
    pub fn new(location: Arc<Location>) -> Self {
        location.retained.fetch_add(1, Ordering::SeqCst);
        Self(location)
    }
}

impl Drop for Retain {
    fn drop(&mut self) {
        self.0.retained.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Starts tracking activity and sweeping idle locations (the `LocationActivity` layer of the server).
pub fn start(app: &App) {
    let mut events = app.bus.subscribe();
    let listener = app.arc();
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            let session_event = event.durable.is_some() && crate::bus::aggregate_field(event.ty) == "sessionID";
            let Some(location) = event.location.as_ref().filter(|_| session_event) else { continue };
            let location = canonical(location);
            if listener.locations.cached(&location).is_some() {
                listener.locations.activity.touch(location);
            }
        }
    });
    let app = app.arc();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SWEEP_INTERVAL).await;
            sweep(&app);
        }
    });
}

/// The location's idle timeout in milliseconds, `None` when it never unloads (`idleTimeout`).
fn idle_timeout(app: &App, reference: &LocationRef) -> Option<f64> {
    let Some(location) = app.locations.cached(reference) else { return Some(TIME_TO_LIVE_MS) };
    let configured = location.inventory().entries.iter().rev().find_map(|entry| match entry {
        Entry::Document { info, .. } => info.experimental.as_ref().and_then(|e| e.location_idle_timeout.clone()),
        _ => None,
    });
    match configured {
        None => Some(TIME_TO_LIVE_MS),
        Some(t::ConfigExperimentalInfoLocationIdleTimeout::False(_)) => None,
        Some(t::ConfigExperimentalInfoLocationIdleTimeout::Duration(duration)) => Some(match duration {
            Timeout::Millis(ms) => ms,
            Timeout::Nanos(nanos) => nanos as f64 / 1e6,
            Timeout::Infinity => f64::INFINITY,
            Timeout::NegativeInfinity => f64::NEG_INFINITY,
        }),
    }
}

/// One sweep: idle locations nothing holds are shut down.
fn sweep(app: &App) {
    let references = app.locations.keys();
    let activity = &app.locations.activity;
    for reference in &references {
        let tracked = activity.touched.lock().unwrap().contains_key(reference);
        if !tracked {
            activity.touch(reference.clone());
        }
    }
    activity.touched.lock().unwrap().retain(|reference, _| references.contains(reference));
    let now = crate::id::now();
    let entries: Vec<(LocationRef, i64)> = activity.touched.lock().unwrap().iter().map(|(r, at)| (r.clone(), *at)).collect();
    let expired: Vec<LocationRef> = entries
        .into_iter()
        .filter(|(reference, touched)| idle_timeout(app, reference).is_some_and(|timeout| *touched as f64 + timeout <= now as f64))
        .map(|(reference, _)| reference)
        .collect();
    if expired.is_empty() {
        return;
    }
    let mut owners = app.sessions.executions.active();
    for recovery in app.sessions.jobs.running_background() {
        match recovery {
            crate::job::Recovery::Subagent { parent_session_id, child_session_id, .. } => {
                owners.push(child_session_id);
                owners.push(parent_session_id);
            }
            crate::job::Recovery::Shell { session_id, .. } | crate::job::Recovery::Image { session_id, .. } => {
                owners.push(session_id)
            }
        }
    }
    let occupied: Vec<LocationRef> = {
        let db = app.db.lock().unwrap();
        owners.iter().filter_map(|id| crate::session::require(&db, id).ok()).map(|session| canonical(&session.location)).collect()
    };
    for reference in expired {
        let retained = app.locations.cached(&reference).is_some_and(|location| location.retained.load(Ordering::SeqCst) > 0);
        if occupied.contains(&reference) || retained {
            activity.touch(reference);
            continue;
        }
        activity.touched.lock().unwrap().shift_remove(&reference);
        app.locations.invalidate(app, &reference);
    }
}
