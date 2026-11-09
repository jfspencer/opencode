//! The JS runtime pieces the ported code leans on, over the TUI's current-thread tokio runtime: `Date.now()`,
//! `setTimeout`/`clearTimeout`, fire-and-forget promises (local tasks, so a `LocalSet` is required) and a shared,
//! settle-once promise ([`Deferred`]).
//!
//! `Date.now()` reads the system clock unless [`set_clock`] installs another (the harness runs on tokio's paused
//! clock). Timers are tokio timers.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

thread_local! {
    static CLOCK: RefCell<Option<Rc<dyn Fn() -> f64>>> = const { RefCell::new(None) };
}

/// `Date.now()`: milliseconds since the Unix epoch.
pub fn now() -> f64 {
    if let Some(clock) = CLOCK.with(|clock| clock.borrow().clone()) {
        return clock();
    }
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

/// Replaces `Date.now()` on this thread (`None` restores the system clock).
pub fn set_clock(clock: Option<Rc<dyn Fn() -> f64>>) {
    CLOCK.with(|slot| *slot.borrow_mut() = clock);
}

thread_local! {
    static RANDOM: RefCell<Option<Rc<dyn Fn(&mut [u8])>>> = const { RefCell::new(None) };
}

/// `crypto.getRandomValues(bytes)`.
pub fn random(bytes: &mut [u8]) {
    if let Some(source) = RANDOM.with(|r| r.borrow().clone()) {
        return source(bytes);
    }
    getrandom::fill(bytes).expect("system random source");
}

/// Replaces the random source on this thread (`None` restores the system's).
pub fn set_random(source: Option<Rc<dyn Fn(&mut [u8])>>) {
    RANDOM.with(|slot| *slot.borrow_mut() = source);
}

/// Identifiers (packages/schema/src/identifier.ts): 12 hex digits of `timestamp * 0x1000 + counter` (negated for
/// descending IDs), then 14 random base-62 characters.
pub mod id {
    use std::cell::Cell;

    const CHARS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

    thread_local! {
        static LAST: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
    }

    pub fn create(descending: bool) -> String {
        let timestamp = super::now() as u64;
        let counter = LAST.with(|last| {
            let (at, count) = last.get();
            let count = if at == timestamp { count + 1 } else { 1 };
            last.set((timestamp, count));
            count
        });
        let current = (timestamp as u128) * 0x1000 + counter as u128;
        let value = if descending { !current } else { current };
        let mut out = String::with_capacity(26);
        for index in 0..6 {
            out.push_str(&format!("{:02x}", (value >> (40 - 8 * index)) & 0xff));
        }
        let mut bytes = [0u8; 14];
        super::random(&mut bytes);
        out.extend(bytes.iter().map(|b| CHARS[(*b % 62) as usize] as char));
        out
    }

    /// `SessionID.create()`.
    pub fn session() -> String {
        format!("ses_{}", create(true))
    }

    /// `SessionMessage.ID.create()`.
    pub fn message() -> String {
        format!("msg_{}", create(false))
    }

    /// `SessionMessage.ID.fromEvent(eventID)`.
    pub fn from_event(event: &str) -> String {
        match event.strip_prefix("evt_") {
            Some(rest) => format!("msg_{rest}"),
            None => event.to_owned(),
        }
    }
}

/// Runs a future to completion in the background (`void promise`): a local task.
pub fn spawn(future: impl Future<Output = ()> + 'static) {
    tokio::task::spawn_local(future);
}

/// A pending `setTimeout`. Clearing it after it fired does nothing.
#[derive(Clone, Default)]
pub struct Timeout(Rc<Cell<Option<tokio::task::AbortHandle>>>);

impl Timeout {
    /// `clearTimeout(timer)`.
    pub fn clear(&self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }

    /// Whether the timer is still waiting to fire.
    pub fn pending(&self) -> bool {
        let handle = self.0.take();
        let pending = handle.as_ref().is_some_and(|h| !h.is_finished());
        self.0.set(handle);
        pending
    }
}

/// `setTimeout(callback, delay)`.
pub fn set_timeout(delay: f64, callback: impl FnOnce() + 'static) -> Timeout {
    let timeout = Timeout::default();
    let slot = timeout.0.clone();
    let task = tokio::task::spawn_local(async move {
        sleep(delay).await;
        slot.set(None);
        callback();
    });
    timeout.0.set(Some(task.abort_handle()));
    timeout
}

thread_local! {
    /// The deadlines of the pending sleeps, so a test clock can step from one to the next ([`next_deadline`]).
    static DEADLINES: RefCell<std::collections::BTreeMap<tokio::time::Instant, usize>> = const { RefCell::new(std::collections::BTreeMap::new()) };
}

struct Deadline(tokio::time::Instant);

impl Deadline {
    fn new(at: tokio::time::Instant) -> Self {
        DEADLINES.with(|d| *d.borrow_mut().entry(at).or_default() += 1);
        Self(at)
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        DEADLINES.with(|d| {
            let mut deadlines = d.borrow_mut();
            if let Some(count) = deadlines.get_mut(&self.0) {
                *count -= 1;
                if *count == 0 {
                    deadlines.remove(&self.0);
                }
            }
        });
    }
}

/// Waits `delay` milliseconds (a timer; `0` waits for the next turn).
pub async fn sleep(delay: f64) {
    let millis = if delay.is_finite() && delay > 0.0 { delay as u64 } else { 0 };
    if millis == 0 {
        tokio::task::yield_now().await;
        return;
    }
    let at = tokio::time::Instant::now() + Duration::from_millis(millis);
    let _deadline = Deadline::new(at);
    tokio::time::sleep_until(at).await;
}

/// The earliest deadline of a pending timer.
pub fn next_deadline() -> Option<tokio::time::Instant> {
    DEADLINES.with(|d| d.borrow().keys().next().copied())
}

struct DeferredState<T> {
    value: RefCell<Option<T>>,
    wakers: RefCell<Vec<Waker>>,
}

/// A promise settled once, awaited by any number of waiters (each gets a clone of the value).
pub struct Deferred<T>(Rc<DeferredState<T>>);

impl<T> Clone for Deferred<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Clone> Default for Deferred<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> Deferred<T> {
    pub fn new() -> Self {
        Self(Rc::new(DeferredState { value: RefCell::new(None), wakers: RefCell::new(Vec::new()) }))
    }

    /// A deferred already settled with `value` (`Promise.resolve(value)`).
    pub fn resolved(value: T) -> Self {
        let deferred = Self::new();
        deferred.resolve(value);
        deferred
    }

    /// Settles; later calls are ignored.
    pub fn resolve(&self, value: T) {
        if self.0.value.borrow().is_some() {
            return;
        }
        *self.0.value.borrow_mut() = Some(value);
        for waker in self.0.wakers.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    pub fn settled(&self) -> bool {
        self.0.value.borrow().is_some()
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// Waits for the value.
    pub fn wait(&self) -> Wait<T> {
        Wait(self.clone())
    }

    /// Runs `future` as a local task and settles with its output: an eagerly started promise.
    pub fn spawn(future: impl Future<Output = T> + 'static) -> Self
    where
        T: 'static,
    {
        let deferred = Self::new();
        let settle = deferred.clone();
        spawn(async move { settle.resolve(future.await) });
        deferred
    }
}

pub struct Wait<T>(Deferred<T>);

impl<T: Clone> Future for Wait<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        if let Some(value) = self.0.0.value.borrow().as_ref() {
            return Poll::Ready(value.clone());
        }
        let mut wakers = self.0.0.wakers.borrow_mut();
        if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

/// A log sink (`LogSink`: level, message, tags).
pub type Log = Rc<dyn Fn(LogLevel, &str, &opencode_protocol::Json)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}
