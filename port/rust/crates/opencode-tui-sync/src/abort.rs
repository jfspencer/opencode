//! `AbortController` and `AbortSignal`, as much of the DOM API as the client uses: aborting with a reason (an
//! `Error`, or the default `AbortError` DOMException "The operation was aborted."), `abort` listeners in
//! registration order, `AbortSignal.any`, and a future that settles when a signal aborts.
//!
//! Single-threaded (`Rc`): signals live on the TUI's current-thread runtime.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

/// An abort reason: the `name` and `message` of the JS value (every reason the client uses is an `Error`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reason {
    pub name: String,
    pub message: String,
}

impl Reason {
    /// `new Error(message)`.
    pub fn error(message: impl Into<String>) -> Self {
        Self { name: "Error".into(), message: message.into() }
    }

    /// The reason of `controller.abort()` without one: `new DOMException("The operation was aborted.", "AbortError")`.
    pub fn abort_error() -> Self {
        Self { name: "AbortError".into(), message: "The operation was aborted.".into() }
    }
}

type Listener = Rc<dyn Fn()>;

struct State {
    reason: RefCell<Option<Reason>>,
    listeners: RefCell<Vec<(u64, Listener)>>,
    wakers: RefCell<Vec<Waker>>,
    next: Cell<u64>,
    /// Signals made by `AbortSignal.any` from this one.
    dependents: RefCell<Vec<std::rc::Weak<State>>>,
}

#[derive(Clone)]
pub struct AbortSignal(Rc<State>);

/// The handle of an `abort` listener, for `remove_listener`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenerId(u64);

impl AbortSignal {
    fn new() -> Self {
        Self(Rc::new(State {
            reason: RefCell::new(None),
            listeners: RefCell::new(Vec::new()),
            wakers: RefCell::new(Vec::new()),
            next: Cell::new(0),
            dependents: RefCell::new(Vec::new()),
        }))
    }

    pub fn aborted(&self) -> bool {
        self.0.reason.borrow().is_some()
    }

    /// Whether both are the same signal.
    pub fn ptr_eq(&self, other: &AbortSignal) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    pub fn reason(&self) -> Option<Reason> {
        self.0.reason.borrow().clone()
    }

    /// `addEventListener("abort", listener)`. A listener runs once, when the signal aborts.
    pub fn add_listener(&self, listener: impl Fn() + 'static) -> ListenerId {
        let id = self.0.next.get();
        self.0.next.set(id + 1);
        self.0.listeners.borrow_mut().push((id, Rc::new(listener)));
        ListenerId(id)
    }

    /// `removeEventListener("abort", listener)`.
    pub fn remove_listener(&self, id: ListenerId) {
        self.0.listeners.borrow_mut().retain(|(other, _)| *other != id.0);
    }

    /// `AbortSignal.any(signals)`: aborts with the reason of the first of them to abort.
    pub fn any(signals: &[&AbortSignal]) -> AbortSignal {
        let result = AbortSignal::new();
        if let Some(aborted) = signals.iter().find(|signal| signal.aborted()) {
            *result.0.reason.borrow_mut() = aborted.reason();
            return result;
        }
        for signal in signals {
            signal.0.dependents.borrow_mut().push(Rc::downgrade(&result.0));
        }
        result
    }

    /// Settles once the signal has aborted (at once when it already has).
    pub fn wait(&self) -> Aborted {
        Aborted(self.clone())
    }

    fn signal_abort(&self, reason: Reason) {
        if self.aborted() {
            return;
        }
        *self.0.reason.borrow_mut() = Some(reason.clone());
        // Dependents take the reason before any listener runs, then fire after this signal's listeners.
        let dependents: Vec<AbortSignal> = self
            .0
            .dependents
            .borrow_mut()
            .drain(..)
            .filter_map(|weak| weak.upgrade().map(AbortSignal))
            .filter(|dependent| !dependent.aborted())
            .collect();
        for dependent in &dependents {
            *dependent.0.reason.borrow_mut() = Some(reason.clone());
        }
        self.run_abort_steps();
        for dependent in &dependents {
            dependent.run_abort_steps();
        }
    }

    fn run_abort_steps(&self) {
        for waker in self.0.wakers.borrow_mut().drain(..) {
            waker.wake();
        }
        let listeners: Vec<Listener> = self.0.listeners.borrow_mut().drain(..).map(|(_, l)| l).collect();
        for listener in listeners {
            listener();
        }
    }
}

/// The future of [`AbortSignal::wait`]: the abort reason.
pub struct Aborted(AbortSignal);

impl Future for Aborted {
    type Output = Reason;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Reason> {
        if let Some(reason) = self.0.reason() {
            return Poll::Ready(reason);
        }
        let mut wakers = self.0.0.wakers.borrow_mut();
        if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

#[derive(Clone)]
pub struct AbortController {
    signal: AbortSignal,
}

impl Default for AbortController {
    fn default() -> Self {
        Self::new()
    }
}

impl AbortController {
    pub fn new() -> Self {
        Self { signal: AbortSignal::new() }
    }

    pub fn signal(&self) -> &AbortSignal {
        &self.signal
    }

    /// `controller.abort(reason)`; without a reason, the `AbortError` DOMException. Aborting twice does nothing.
    pub fn abort(&self, reason: Option<Reason>) {
        self.signal.signal_abort(reason.unwrap_or_else(Reason::abort_error));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listeners_and_any() {
        let a = AbortController::new();
        let b = AbortController::new();
        let any = AbortSignal::any(&[a.signal(), b.signal()]);
        let order = Rc::new(RefCell::new(Vec::new()));
        let o = order.clone();
        a.signal().add_listener(move || o.borrow_mut().push("a"));
        let o = order.clone();
        any.add_listener(move || o.borrow_mut().push("any"));
        let o = order.clone();
        let id = a.signal().add_listener(move || o.borrow_mut().push("removed"));
        a.signal().remove_listener(id);
        a.abort(Some(Reason::error("x")));
        assert_eq!(*order.borrow(), vec!["a", "any"]);
        assert_eq!(any.reason(), Some(Reason::error("x")));
        b.abort(None);
        assert_eq!(any.reason(), Some(Reason::error("x")));
    }
}
