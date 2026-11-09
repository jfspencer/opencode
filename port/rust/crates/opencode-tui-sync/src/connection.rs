//! `createClientConnection` (packages/client/src/solid/connection.ts): keeps the event stream open and publishes its
//! events in 10 ms batches.
//!
//! An attempt subscribes to `api.event`; it must start with `server.connected` within 2 s ("Timed out connecting to
//! server") and is aborted after 45 s without a byte ("Event stream stalled"; keepalives count). When an attempt
//! ends, the attempt counter goes up (it restarts from 1 when the stream had stayed connected for at least 1 s), the
//! status turns `reconnecting` with the error's message, `reconnect` (a managed service) may replace the client, and
//! the next attempt starts after 1 s (at once after a `resync`, or right after a first failure replaced the
//! client). `stop` makes the status `connecting` until `start` reconnects. The page-lifecycle option is the
//! browser's and is not ported.
//!
//! The status, attempt and error are observable state ([`Key::ConnectionStatus`], [`Key::ConnectionAttempt`],
//! [`Key::ConnectionError`]); events are delivered to `on_event` inside the reactive layer's batch.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_protocol::Json;

use crate::abort::{AbortController, AbortSignal, Reason};
use crate::client::Client;
use crate::event::Event;
use crate::reactive::{self, Key, Reactivity};
use crate::runtime::{self, Deferred, Log, LogLevel, Timeout};
use crate::shared_events::SubscribeOptions;
use crate::transport::LocalBoxFuture;

/// `ClientConnectionStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Connected,
    Connecting,
    Reconnecting,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Connected => "connected",
            Status::Connecting => "connecting",
            Status::Reconnecting => "reconnecting",
        }
    }
}

/// One entry of the connection history (`ClientConnectionEvent`).
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEvent {
    pub created: f64,
    /// `connecting`, `connected`, `disconnected` or `reconnecting`.
    pub status: &'static str,
    pub attempt: u32,
    pub error: Option<String>,
}

/// Resolves the server again after a disconnect (a managed service's `reconnect`), giving the client to use.
pub type Reconnect = Rc<dyn Fn(AbortSignal) -> LocalBoxFuture<'static, Result<Client, Reason>>>;

/// `ClientConnectionOptions`.
#[derive(Clone)]
pub struct ConnectionOptions {
    pub reconnect: Option<Reconnect>,
    pub on_event: Rc<dyn Fn(&Event)>,
    /// Milliseconds events wait to be published together (10).
    pub flush_interval: Option<f64>,
    /// Abort and reconnect a stream that receives no bytes for this long (45 s).
    pub idle_timeout: Option<f64>,
    pub log: Option<Log>,
    pub reactivity: Rc<dyn Reactivity>,
}

const CONNECT_TIMEOUT: f64 = 2_000.0;
const RECONNECT_DELAY: f64 = 1_000.0;
const HISTORY_LIMIT: usize = 50;
pub const DEFAULT_IDLE_TIMEOUT: f64 = 45_000.0;

struct State {
    options: ConnectionOptions,
    abort: AbortController,
    history: RefCell<Vec<HistoryEvent>>,
    idle_timeout: f64,
    status: Cell<Status>,
    attempt: Cell<u32>,
    error: RefCell<Option<String>>,
    api: RefCell<Client>,
    pending: RefCell<Vec<Event>>,
    flush: RefCell<Option<Timeout>>,
    stream: RefCell<Option<AbortController>>,
    current: RefCell<Option<AbortController>>,
    run: RefCell<Option<Deferred<()>>>,
    started: Cell<bool>,
    generation: Cell<u64>,
    last_activity: Cell<f64>,
    forced: Cell<bool>,
    /// Called when the status changes, before the reactive layer hears of it (the data layer's own effect).
    status_listeners: RefCell<Vec<Rc<dyn Fn(Status)>>>,
}

/// The handle `createClientConnection` returns.
#[derive(Clone)]
pub struct ClientConnection(Rc<State>);

/// The part of a connection result `connect` reports.
struct Outcome {
    error: Option<Reason>,
    connected_at: Option<f64>,
}

impl ClientConnection {
    /// `createClientConnection(api, options)`: starts connecting at once.
    pub fn create(api: Client, options: ConnectionOptions) -> Self {
        let idle_timeout = options.idle_timeout.unwrap_or(DEFAULT_IDLE_TIMEOUT);
        let connection = Self(Rc::new(State {
            options,
            abort: AbortController::new(),
            history: RefCell::new(Vec::new()),
            idle_timeout,
            status: Cell::new(Status::Connecting),
            attempt: Cell::new(0),
            error: RefCell::new(None),
            api: RefCell::new(api),
            pending: RefCell::new(Vec::new()),
            flush: RefCell::new(None),
            stream: RefCell::new(None),
            current: RefCell::new(None),
            run: RefCell::new(None),
            started: Cell::new(false),
            generation: Cell::new(0),
            last_activity: Cell::new(0.0),
            forced: Cell::new(false),
            status_listeners: RefCell::new(Vec::new()),
        }));
        connection.start();
        connection
    }

    pub fn status(&self) -> Status {
        self.0.options.reactivity.track(&Key::ConnectionStatus);
        self.0.status.get()
    }

    pub fn attempt(&self) -> u32 {
        self.0.options.reactivity.track(&Key::ConnectionAttempt);
        self.0.attempt.get()
    }

    pub fn error(&self) -> Option<String> {
        self.0.options.reactivity.track(&Key::ConnectionError);
        self.0.error.borrow().clone()
    }

    /// The status without tracking it.
    pub fn status_untracked(&self) -> Status {
        self.0.status.get()
    }

    /// `internal.history()`.
    pub fn history(&self) -> Vec<HistoryEvent> {
        self.0.history.borrow().clone()
    }

    /// The client the stream currently uses (a managed service's reconnect may have replaced it).
    pub fn api(&self) -> Client {
        self.0.api.borrow().clone()
    }

    /// `setConnection({ status, attempt, error })`; `error: None` leaves the error as it is (a merge without the
    /// key), `Some(None)` clears it.
    fn set(&self, status: Status, attempt: u32, error: Option<Option<String>>) {
        let state = &self.0;
        let mut changed = Vec::new();
        if state.status.get() != status {
            state.status.set(status);
            changed.push(Key::ConnectionStatus);
        }
        if state.attempt.get() != attempt {
            state.attempt.set(attempt);
            changed.push(Key::ConnectionAttempt);
        }
        if let Some(error) = error {
            if *state.error.borrow() != error {
                *state.error.borrow_mut() = error;
                changed.push(Key::ConnectionError);
            }
        }
        if changed.contains(&Key::ConnectionStatus) {
            let listeners = state.status_listeners.borrow().clone();
            listeners.iter().for_each(|listener| listener(status));
        }
        if !changed.is_empty() {
            let reactivity = state.options.reactivity.clone();
            reactive::batch(&*reactivity, || changed.iter().for_each(|key| reactivity.trigger(key)));
        }
    }

    /// Calls `listener` with the new status whenever it changes (a `createEffect` reading `status()`).
    pub fn on_status(&self, listener: impl Fn(Status) + 'static) {
        self.0.status_listeners.borrow_mut().push(Rc::new(listener));
    }

    fn log(&self, level: LogLevel, message: &str, tags: Json) {
        if let Some(log) = &self.0.options.log {
            log(level, message, &tags);
        }
    }

    fn record(&self, status: &'static str, attempt: u32, error: Option<String>) {
        let mut history = self.0.history.borrow_mut();
        history.push(HistoryEvent { created: runtime::now(), status, attempt, error });
        if history.len() > HISTORY_LIMIT {
            history.remove(0);
        }
    }

    fn publish(&self, event: Event) {
        self.0.pending.borrow_mut().push(event);
        if self.0.flush.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        let timer = runtime::set_timeout(self.0.options.flush_interval.unwrap_or(10.0), move || {
            let Some(state) = weak.upgrade() else { return };
            *state.flush.borrow_mut() = None;
            let events = std::mem::take(&mut *state.pending.borrow_mut());
            let on_event = state.options.on_event.clone();
            reactive::batch(&*state.options.reactivity, || events.iter().for_each(|event| on_event(event)));
        });
        *self.0.flush.borrow_mut() = Some(timer);
    }

    async fn connect(&self, signal: AbortSignal, attempt: u32) -> Outcome {
        let state = &self.0;
        let connected_at: Rc<Cell<Option<f64>>> = Rc::new(Cell::new(None));
        let request = AbortController::new();
        *state.current.borrow_mut() = Some(request.clone());
        let cancel_request = request.clone();
        let cancel_signal = signal.clone();
        let cancel = signal.add_listener(move || cancel_request.abort(cancel_signal.reason()));
        let timeout_request = request.clone();
        let timeout = runtime::set_timeout(CONNECT_TIMEOUT, move || {
            timeout_request.abort(Some(Reason::error("Timed out connecting to server")))
        });
        // Any received bytes, including keepalive comments, push the stall deadline out.
        let watchdog: Rc<RefCell<Option<Timeout>>> = Rc::new(RefCell::new(None));
        let touch: Rc<dyn Fn()> = {
            let weak = Rc::downgrade(state);
            let connected_at = connected_at.clone();
            let watchdog = watchdog.clone();
            let request = request.clone();
            let idle = state.idle_timeout;
            Rc::new(move || {
                let Some(state) = weak.upgrade() else { return };
                state.last_activity.set(runtime::now());
                if connected_at.get().is_none() {
                    return;
                }
                if let Some(timer) = watchdog.borrow_mut().take() {
                    timer.clear();
                }
                let request = request.clone();
                *watchdog.borrow_mut() =
                    Some(runtime::set_timeout(idle, move || request.abort(Some(Reason::error("Event stream stalled")))));
            })
        };

        let outcome = async {
            self.record(if attempt == 0 { "connecting" } else { "reconnecting" }, attempt, None);
            self.log(LogLevel::Info, "event stream connecting", obj([("attempt", Json::Number(attempt as f64))]));
            let api = state.api.borrow().clone();
            let mut iterator = api.event().subscribe(SubscribeOptions {
                types: Vec::new(),
                signal: Some(request.signal().clone()),
                on_activity: Some(touch.clone()),
            });
            let disconnected = |request: &AbortController| {
                Some(request.signal().reason().unwrap_or_else(|| Reason::error("Event stream disconnected")))
            };
            let first = iterator.next().await;
            if signal.aborted() {
                return None;
            }
            let first = match first {
                None => return disconnected(&request),
                Some(Err(error)) => return Some(error.reason()),
                Some(Ok(event)) => event,
            };
            if first.r#type() != "server.connected" {
                return Some(Reason::error("Event stream did not start with server.connected"));
            }
            timeout.clear();
            self.record("connected", attempt, None);
            connected_at.set(Some(runtime::now()));
            touch();
            self.log(LogLevel::Info, "event stream connected", Json::Null);
            self.publish(first);
            self.set(Status::Connected, 0, Some(None));
            while !signal.aborted() {
                let event = iterator.next().await;
                if signal.aborted() {
                    return None;
                }
                let event = match event {
                    None => return disconnected(&request),
                    Some(Err(error)) => return Some(error.reason()),
                    Some(Ok(event)) => event,
                };
                touch();
                if let Some(durable) = event.durable() {
                    self.log(LogLevel::Debug, "event", event_tags(&event, durable));
                }
                self.publish(event);
            }
            None
        }
        .await;

        request.abort(None);
        let is_current = state.current.borrow().as_ref().is_some_and(|current| current.signal().ptr_eq(request.signal()));
        if is_current {
            *state.current.borrow_mut() = None;
        }
        timeout.clear();
        if let Some(timer) = watchdog.borrow_mut().take() {
            timer.clear();
        }
        signal.remove_listener(cancel);
        Outcome { error: outcome, connected_at: connected_at.get() }
    }

    async fn run_stream(&self, active: u64) {
        let state = &self.0;
        let mut attempt: u32 = 0;
        while !state.abort.signal().aborted() && state.started.get() && state.generation.get() == active {
            self.set(if attempt == 0 { Status::Connecting } else { Status::Reconnecting }, attempt, None);
            let controller = AbortController::new();
            *state.stream.borrow_mut() = Some(controller.clone());
            let forward = controller.clone();
            let abort_signal = state.abort.signal().clone();
            let cancel = state.abort.signal().add_listener(move || forward.abort(abort_signal.reason()));
            let result = self.connect(controller.signal().clone(), attempt).await;
            state.abort.signal().remove_listener(cancel);
            if state.abort.signal().aborted() || !state.started.get() || state.generation.get() != active {
                return;
            }
            if result.connected_at.is_some_and(|at| runtime::now() - at >= RECONNECT_DELAY) {
                attempt = 0;
            }
            attempt += 1;
            let message = result.error.map(|error| error.message);
            self.record("disconnected", attempt, message.clone());
            let mut tags = obj([("attempt", Json::Number(attempt as f64))]);
            if let (Some(message), Json::Object(tags)) = (&message, &mut tags) {
                tags.insert("error".into(), Json::String(message.clone()));
            }
            self.log(LogLevel::Info, "event stream disconnected", tags);
            self.set(Status::Reconnecting, attempt, Some(message));

            if let Some(reconnect) = state.options.reconnect.clone() {
                let next = reconnect(controller.signal().clone()).await;
                let next = match next {
                    Ok(next) => Some(next),
                    Err(error) => {
                        if !controller.signal().aborted() {
                            self.log(
                                LogLevel::Info,
                                "server resolution failed",
                                obj([("attempt", Json::Number(attempt as f64)), ("error", Json::String(error.message))]),
                            );
                        }
                        None
                    }
                };
                if state.abort.signal().aborted()
                    || controller.signal().aborted()
                    || !state.started.get()
                    || state.generation.get() != active
                {
                    return;
                }
                if let Some(next) = next {
                    *state.api.borrow_mut() = next;
                    if attempt == 1 {
                        continue;
                    }
                }
            }
            // A deliberate resync already knows the old socket is gone; reconnect without backing off.
            if state.forced.get() {
                state.forced.set(false);
                continue;
            }
            wait(RECONNECT_DELAY, controller.signal()).await;
        }
    }

    /// `start()`: (re)connects unless already started; waits for a previous run to finish first.
    pub fn start(&self) -> Deferred<()> {
        let state = &self.0;
        if state.started.get() {
            return state.run.borrow().clone().unwrap_or_else(|| Deferred::resolved(()));
        }
        state.started.set(true);
        state.forced.set(false);
        state.generation.set(state.generation.get() + 1);
        let active = state.generation.get();
        let previous = state.run.borrow().clone();
        let current: Deferred<()> = Deferred::new();
        *state.run.borrow_mut() = Some(current.clone());
        let this = self.clone();
        let settle = current.clone();
        runtime::spawn(async move {
            if let Some(previous) = previous {
                previous.wait().await;
            }
            this.run_stream(active).await;
            let is_current = this.0.run.borrow().as_ref().is_some_and(|run| run.ptr_eq(&settle));
            if is_current {
                *this.0.run.borrow_mut() = None;
            }
            settle.resolve(());
        });
        current
    }

    /// `stop()`: nothing is listening until `start()`; consumers treat their data as stale meanwhile.
    pub fn stop(&self) {
        let state = &self.0;
        if !state.started.get() {
            return;
        }
        state.started.set(false);
        state.generation.set(state.generation.get() + 1);
        if let Some(stream) = state.stream.borrow().clone() {
            stream.abort(None);
        }
        self.set(Status::Connecting, 0, Some(None));
    }

    /// `internal.resync(reason)`: drops the live request so the loop replaces it now.
    pub fn resync(&self, reason: &str) {
        let state = &self.0;
        if !state.started.get() || state.status.get() != Status::Connected {
            return;
        }
        self.log(
            LogLevel::Info,
            "event stream resync",
            obj([
                ("reason", Json::String(reason.into())),
                ("idle", Json::Number(runtime::now() - state.last_activity.get())),
            ]),
        );
        state.forced.set(true);
        if let Some(current) = state.current.borrow().clone() {
            current.abort(Some(Reason::error(reason)));
        }
    }

    /// The owner's cleanup: stops, aborts everything and drops unpublished events.
    pub fn dispose(&self) {
        self.stop();
        self.0.abort.abort(None);
        if let Some(timer) = self.0.flush.borrow_mut().take() {
            timer.clear();
        }
        self.0.pending.borrow_mut().clear();
    }
}

fn obj<const N: usize>(entries: [(&str, Json); N]) -> Json {
    Json::Object(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn event_tags(event: &Event, durable: &Json) -> Json {
    let field = |key: &str| durable.as_object().and_then(|d| d.get(key)).cloned().unwrap_or(Json::Null);
    obj([("type", Json::String(event.r#type().into())), ("aggregateID", field("aggregateID")), ("seq", field("seq"))])
}

/// `wait(delay, signal)`: resolves after the delay, or as soon as the signal aborts.
async fn wait(delay: f64, signal: &AbortSignal) {
    if signal.aborted() {
        return;
    }
    tokio::select! {
        _ = runtime::sleep(delay) => {}
        _ = signal.wait() => {}
    }
}
