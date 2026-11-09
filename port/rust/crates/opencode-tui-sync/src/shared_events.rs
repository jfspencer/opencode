//! `SharedEvents` (packages/client/src/shared-events.ts): one event stream shared by every subscriber of a client.
//! The first subscriber opens it, the last one to leave closes it; a late subscriber first receives the stream's
//! `server.connected`; `server.connected` passes every type filter, so a filtered subscriber still sees
//! reconnects. A subscriber that falls 4096 events behind fails with "Event subscriber exceeded its 4096-event
//! capacity".
//!
//! The stream is read by a local task (`tokio::task::spawn_local`), so subscribing needs a `LocalSet`.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use crate::abort::{AbortController, AbortSignal, ListenerId, Reason};
use crate::client::Error;
use crate::event::Event;
use crate::transport::LocalBoxFuture;

/// The source of a shared stream: one connection's events, read with `next()`.
pub trait Source {
    fn next(&mut self) -> LocalBoxFuture<'_, Option<Result<Event, Error>>>;
}

/// Opens a connection: `connect(signal, onActivity)`.
pub type Connect = Rc<dyn Fn(AbortSignal, Rc<dyn Fn()>) -> Box<dyn Source>>;

const CAPACITY: usize = 4_096;

/// How a subscription ended: `{}` or `{ error }`.
#[derive(Clone, Debug)]
pub enum Completion {
    Done,
    Error(SubscriptionError),
}

/// What ends a subscription with an error: the stream's own error, or falling behind.
#[derive(Clone, Debug, PartialEq)]
pub enum SubscriptionError {
    Client(Error),
    /// `new Error("Event subscriber exceeded its 4096-event capacity")`.
    Capacity,
}

impl SubscriptionError {
    pub fn message(&self) -> String {
        match self {
            SubscriptionError::Client(error) => error.message(),
            SubscriptionError::Capacity => format!("Event subscriber exceeded its {CAPACITY}-event capacity"),
        }
    }

    pub fn reason(&self) -> Reason {
        match self {
            SubscriptionError::Client(error) => Reason { name: error.name(), message: error.message() },
            SubscriptionError::Capacity => Reason::error(self.message()),
        }
    }
}

struct Connection {
    controller: AbortController,
    subscribers: RefCell<Vec<Rc<Subscriber>>>,
    connected: RefCell<Option<Event>>,
}

struct Subscriber {
    types: Option<Vec<String>>,
    activity: Option<Rc<dyn Fn()>>,
    queued: RefCell<VecDeque<Event>>,
    completion: RefCell<Option<Completion>>,
    waker: RefCell<Option<Waker>>,
    connection: RefCell<Option<Rc<Connection>>>,
    abort: RefCell<Option<(AbortSignal, ListenerId)>>,
    shared: std::rc::Weak<Shared>,
}

struct Shared {
    connect: Connect,
    current: RefCell<Option<Rc<Connection>>>,
}

/// `SharedEvents.make(connect)`.
#[derive(Clone)]
pub struct SharedEvents(Rc<Shared>);

/// `SubscribeOptions`.
#[derive(Clone, Default)]
pub struct SubscribeOptions {
    pub types: Vec<String>,
    pub signal: Option<AbortSignal>,
    /// Reports transport activity on the shared stream, including keepalive frames that carry no event.
    pub on_activity: Option<Rc<dyn Fn()>>,
}

impl SharedEvents {
    pub fn make(connect: Connect) -> Self {
        Self(Rc::new(Shared { connect, current: RefCell::new(None) }))
    }

    /// `subscribe(options)`: nothing is opened until the first `next()`.
    pub fn subscribe(&self, options: SubscribeOptions) -> Subscription {
        let subscriber = Rc::new(Subscriber {
            types: if options.types.is_empty() { None } else { Some(options.types) },
            activity: options.on_activity,
            queued: RefCell::new(VecDeque::new()),
            completion: RefCell::new(None),
            waker: RefCell::new(None),
            connection: RefCell::new(None),
            abort: RefCell::new(None),
            shared: Rc::downgrade(&self.0),
        });
        Subscription { subscriber, signal: options.signal, started: Cell::new(false) }
    }
}

fn stop(shared: &Shared, connection: &Rc<Connection>) {
    *connection.connected.borrow_mut() = None;
    connection.controller.abort(None);
    let is_current = shared.current.borrow().as_ref().is_some_and(|current| Rc::ptr_eq(current, connection));
    if is_current {
        *shared.current.borrow_mut() = None;
    }
}

async fn run(shared: Rc<Shared>, connection: Rc<Connection>) {
    let mut completion = Completion::Done;
    if !connection.controller.signal().aborted() {
        let activity_connection = Rc::downgrade(&connection);
        let on_activity: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(connection) = activity_connection.upgrade() {
                let subscribers: Vec<Rc<Subscriber>> = connection.subscribers.borrow().clone();
                for subscriber in subscribers {
                    if let Some(activity) = &subscriber.activity {
                        activity();
                    }
                }
            }
        });
        let mut source = (shared.connect)(connection.controller.signal().clone(), on_activity);
        while !connection.controller.signal().aborted() {
            let item = source.next().await;
            let event = match item {
                None => break,
                Some(Err(error)) => {
                    completion = Completion::Error(SubscriptionError::Client(error));
                    break;
                }
                Some(Ok(event)) => event,
            };
            if connection.controller.signal().aborted() {
                break;
            }
            if event.r#type() == "server.connected" {
                *connection.connected.borrow_mut() = Some(event.clone());
            }
            let subscribers: Vec<Rc<Subscriber>> = connection.subscribers.borrow().clone();
            let mut behind = false;
            for subscriber in &subscribers {
                push(subscriber, &event);
                behind |= !subscriber.queued.borrow().is_empty();
            }
            // Let subscribers take what they were given before reading on, as a JS consumer's awaits interleave
            // with the stream's.
            if behind {
                tokio::task::yield_now().await;
            }
        }
        drop(source);
    }
    stop(&shared, &connection);
    let subscribers: Vec<Rc<Subscriber>> = connection.subscribers.borrow().clone();
    for subscriber in subscribers {
        finish(&subscriber, completion.clone(), false);
    }
}

fn selected(subscriber: &Subscriber, event: &Event) -> bool {
    let kind = event.r#type();
    match &subscriber.types {
        None => true,
        Some(types) => kind == "server.connected" || types.iter().any(|t| t == kind),
    }
}

fn push(subscriber: &Rc<Subscriber>, event: &Event) {
    if subscriber.completion.borrow().is_some() || !selected(subscriber, event) {
        return;
    }
    if subscriber.queued.borrow().len() == CAPACITY {
        finish(subscriber, Completion::Error(SubscriptionError::Capacity), true);
        return;
    }
    subscriber.queued.borrow_mut().push_back(event.clone());
    if let Some(waker) = subscriber.waker.borrow_mut().take() {
        waker.wake();
    }
}

fn finish(subscriber: &Rc<Subscriber>, result: Completion, discard: bool) {
    *subscriber.completion.borrow_mut() = Some(result);
    if discard {
        subscriber.queued.borrow_mut().clear();
    }
    if let Some((signal, id)) = subscriber.abort.borrow_mut().take() {
        signal.remove_listener(id);
    }
    let connection = subscriber.connection.borrow().clone();
    if let Some(connection) = connection {
        let removed = {
            let mut subscribers = connection.subscribers.borrow_mut();
            let before = subscribers.len();
            subscribers.retain(|other| !Rc::ptr_eq(other, subscriber));
            before != subscribers.len()
        };
        if removed && connection.subscribers.borrow().is_empty() {
            if let Some(shared) = subscriber.shared.upgrade() {
                stop(&shared, &connection);
            }
        }
    }
    if let Some(waker) = subscriber.waker.borrow_mut().take() {
        waker.wake();
    }
}

fn start(subscriber: &Rc<Subscriber>) {
    if subscriber.completion.borrow().is_some() {
        return;
    }
    let Some(shared) = subscriber.shared.upgrade() else { return };
    let existing = shared.current.borrow().clone();
    let fresh = existing.is_none();
    let connection = existing.unwrap_or_else(|| {
        Rc::new(Connection {
            controller: AbortController::new(),
            subscribers: RefCell::new(Vec::new()),
            connected: RefCell::new(None),
        })
    });
    *shared.current.borrow_mut() = Some(connection.clone());
    *subscriber.connection.borrow_mut() = Some(connection.clone());
    connection.subscribers.borrow_mut().push(subscriber.clone());
    let connected = connection.connected.borrow().clone();
    if let Some(connected) = connected {
        push(subscriber, &connected);
    }
    if fresh {
        tokio::task::spawn_local(run(shared, connection));
    }
}

/// A subscriber's iterator. Dropping it is `iterator.return()`.
pub struct Subscription {
    subscriber: Rc<Subscriber>,
    signal: Option<AbortSignal>,
    started: Cell<bool>,
}

impl Subscription {
    /// `iterator.next()`: the next event, `None` once the subscription is done, or the error that ended it.
    pub fn next(&mut self) -> Next<'_> {
        Next(self)
    }

    /// `iterator.return()`.
    pub fn close(&mut self) {
        finish(&self.subscriber, Completion::Done, true);
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if self.subscriber.completion.borrow().is_none() {
            finish(&self.subscriber, Completion::Done, true);
        }
    }
}

pub struct Next<'a>(&'a mut Subscription);

impl Future for Next<'_> {
    type Output = Option<Result<Event, SubscriptionError>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let subscription = &*self.0;
        let subscriber = &subscription.subscriber;
        if let Some(event) = subscriber.queued.borrow_mut().pop_front() {
            return Poll::Ready(Some(Ok(event)));
        }
        if let Some(completion) = subscriber.completion.borrow().clone() {
            return Poll::Ready(match completion {
                Completion::Done => None,
                Completion::Error(error) => Some(Err(error)),
            });
        }
        if subscription.signal.as_ref().is_some_and(AbortSignal::aborted) {
            finish(subscriber, Completion::Done, true);
            return Poll::Ready(None);
        }
        *subscriber.waker.borrow_mut() = Some(cx.waker().clone());
        if !subscription.started.get() {
            subscription.started.set(true);
            if let Some(signal) = &subscription.signal {
                let weak = Rc::downgrade(subscriber);
                let id = signal.add_listener(move || {
                    if let Some(subscriber) = weak.upgrade() {
                        finish(&subscriber, Completion::Done, true);
                    }
                });
                *subscriber.abort.borrow_mut() = Some((signal.clone(), id));
            }
            start(subscriber);
            // `start` may have queued the stream's `server.connected` already.
            if let Some(event) = subscriber.queued.borrow_mut().pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }
        }
        Poll::Pending
    }
}
