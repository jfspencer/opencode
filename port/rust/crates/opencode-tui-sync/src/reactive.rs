//! Change notification: how the TUI's reactive layer (opencode-tui-core's Solid-like signals) observes this
//! crate's state at fine grain.
//!
//! State here is plain Rust data. Every read through an accessor reports a [`Key`] to [`Reactivity::track`], and
//! every change reports the keys whose values changed to [`Reactivity::trigger`]. A reactive layer implements
//! [`Reactivity`] by mapping keys to signals: `track` subscribes the running computation (when there is one; reads
//! under `untrack` are not subscriptions), `trigger` notifies. Keys form a tree ([`Key::parent`]): a change is
//! reported at the most specific key that changed, and it also changes every ancestor of that key, so reading
//! an ancestor is a deep read. `session.message.list(id)` tracks [`Key::Messages`], which every change to that
//! transcript triggers; `session.message.get(id, messageID)` tracks one message; a part renderer can track a single
//! part with [`Key::Part`]. Sibling keys are independent: appending a message triggers [`Key::MessageIds`] (the
//! transcript's shape) but no other message's key.
//!
//! Changes that TS makes inside `batch(...)` are reported inside [`Reactivity::batch`], so effects run once the
//! batch completes, as in Solid. A reactive layer may run effects synchronously from `trigger` outside a batch:
//! no borrow of this crate's state is held while `trigger` runs.
//!
//! [`Observers`] is a ready implementation: callbacks subscribed to keys, called after the outermost batch.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// A cheap shared string for keys.
pub type Id = Rc<str>;

/// What a read observed, or what a change changed. See the module docs for the tree.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    // ---- the data store (`createData`) ----
    /// Every session info (`session.list()`); parent of [`Key::Session`].
    Sessions,
    /// `session.get(id)`.
    Session(Id),
    /// `session.missing(id)`.
    Missing(Id),
    /// Every family (parent of [`Key::Family`]).
    Families,
    /// The family index entry of a root (`session.family(id)` reads the entry of `root(id)`).
    Family(Id),
    /// Every session status (`session.active()`); parent of [`Key::Status`].
    Statuses,
    /// `session.status(id)`.
    Status(Id),
    /// A session's whole transcript (`session.message.list(id)`); parent of [`Key::MessageIds`] and [`Key::Message`].
    Messages(Id),
    /// The transcript's shape: which messages, in which order.
    MessageIds(Id),
    /// One message (`session.message.get(id, messageID)`); parent of [`Key::MessageHead`], [`Key::Parts`] and
    /// [`Key::Part`].
    Message(Id, Id),
    /// A message's fields other than an assistant's `content`.
    MessageHead(Id, Id),
    /// An assistant message's parts list: how many, which kinds.
    Parts(Id, Id),
    /// One part of an assistant message, by index.
    Part(Id, Id, usize),
    /// `session.message.more(id)` (the cursor).
    MessageCursor(Id),
    /// `session.message.loading(id)`.
    MessageLoading(Id),
    /// `session.pending.list(id)` and `session.input.*`.
    Pending(Id),
    /// Every session's permission requests (`permission.sessions()`); parent of [`Key::Permission`].
    Permissions,
    Permission(Id),
    /// Every session's forms (`form.sessions()`); parent of [`Key::Form`].
    Forms,
    /// The forms of a session, or of the `global` sentinel.
    Form(Id),
    /// Every project (`project.list()`); parent of [`Key::Project`].
    Projects,
    Project(Id),
    ProjectPermission(Id),
    /// Every location's data; parent of [`Key::Location`].
    Locations,
    /// The data of one location (by `locationKey`); parent of [`Key::LocationField`].
    Location(Id),
    LocationField(Id, LocationField),
    /// `location.default()`.
    DefaultLocation,

    // ---- the event stream connection (`createClientConnection`) ----
    ConnectionStatus,
    ConnectionAttempt,
    ConnectionError,

    // ---- the TUI's data contexts ----
    /// `session.title.pending(id)` (context/data.tsx).
    TitlePending(Id),
    /// `location.ref`, `location.error` (context/location.tsx).
    LocationRef,
    LocationError,
    /// A `storage.store` or `storage.memory` entry (context/storage.tsx), by key.
    Storage(Id),
    /// `sessionTerminals.available()` and `shouldFocus` (context/session-terminals.tsx).
    TerminalsAvailable,
    TerminalFocus,
    /// Any other state of a context, by name.
    Custom(&'static str),
}

/// The fields of a location's data (`LocationData`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocationField {
    Info,
    Vcs,
    Agent,
    Command,
    Config,
    Integration,
    McpServer,
    McpResource,
    Model,
    Provider,
    Reference,
    Websearch,
    UsageLimits,
    Shell,
    Skill,
}

impl LocationField {
    /// The TS field name (also the `location.<field>:` sync key).
    pub fn name(self) -> &'static str {
        match self {
            LocationField::Info => "info",
            LocationField::Vcs => "vcs",
            LocationField::Agent => "agent",
            LocationField::Command => "command",
            LocationField::Config => "config",
            LocationField::Integration => "integration",
            LocationField::McpServer => "mcpServer",
            LocationField::McpResource => "mcpResource",
            LocationField::Model => "model",
            LocationField::Provider => "provider",
            LocationField::Reference => "reference",
            LocationField::Websearch => "websearch",
            LocationField::UsageLimits => "usageLimits",
            LocationField::Shell => "shell",
            LocationField::Skill => "skill",
        }
    }
}

impl Key {
    /// The key whose value contains this one's.
    pub fn parent(&self) -> Option<Key> {
        Some(match self {
            Key::Session(_) => Key::Sessions,
            Key::Family(_) => Key::Families,
            Key::Status(_) => Key::Statuses,
            Key::MessageIds(session) | Key::Message(session, _) => Key::Messages(session.clone()),
            Key::MessageHead(session, message) | Key::Parts(session, message) | Key::Part(session, message, _) => {
                Key::Message(session.clone(), message.clone())
            }
            Key::Permission(_) => Key::Permissions,
            Key::Form(_) => Key::Forms,
            Key::Project(_) => Key::Projects,
            Key::Location(_) => Key::Locations,
            Key::LocationField(location, _) => Key::Location(location.clone()),
            _ => return None,
        })
    }

    /// This key and its ancestors, most specific first.
    pub fn lineage(&self) -> impl Iterator<Item = Key> {
        std::iter::successors(Some(self.clone()), Key::parent)
    }
}

/// The reactive layer this crate reports to.
pub trait Reactivity {
    /// A read of `key` (and everything under it).
    fn track(&self, key: &Key);
    /// `key` changed (and with it every ancestor).
    fn trigger(&self, key: &Key);
    /// Solid's `batch`: changes inside report their effects once the outermost batch ends.
    fn batch(&self, f: &mut dyn FnMut());
}

/// No reactive layer: reads are not tracked and changes notify nobody.
#[derive(Clone, Copy, Debug, Default)]
pub struct Inert;

impl Reactivity for Inert {
    fn track(&self, _: &Key) {}
    fn trigger(&self, _: &Key) {}
    fn batch(&self, f: &mut dyn FnMut()) {
        f()
    }
}

/// Runs `f` inside the reactive layer's batch and returns its result.
pub fn batch<R>(reactivity: &dyn Reactivity, f: impl FnOnce() -> R) -> R {
    let mut f = Some(f);
    let mut result = None;
    reactivity.batch(&mut || result = Some((f.take().expect("batch runs once"))()));
    result.expect("batch ran")
}

type Callback = Rc<dyn Fn(&Key)>;

#[derive(Default)]
struct ObserverState {
    subscribers: RefCell<HashMap<Key, Vec<(u64, Callback)>>>,
    next: Cell<u64>,
    depth: Cell<usize>,
    queued: RefCell<Vec<Key>>,
}

/// A [`Reactivity`] of key subscriptions: `subscribe(key, callback)` calls back when `key` or anything under it
/// changes, once per change (deduplicated within a batch, delivered when the outermost batch ends). Reads are
/// reported to an optional tracking hook.
#[derive(Clone, Default)]
pub struct Observers {
    state: Rc<ObserverState>,
    on_track: Option<Rc<dyn Fn(&Key)>>,
}

/// Cancels its subscription when dropped (or with `cancel`).
pub struct Subscription {
    state: std::rc::Weak<ObserverState>,
    key: Key,
    id: u64,
}

impl Subscription {
    pub fn cancel(self) {}
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            let mut subscribers = state.subscribers.borrow_mut();
            if let Some(list) = subscribers.get_mut(&self.key) {
                list.retain(|(id, _)| *id != self.id);
                if list.is_empty() {
                    subscribers.remove(&self.key);
                }
            }
        }
    }
}

impl Observers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reports every tracked read to `hook` (an auto-tracking reactive layer's dependency collector).
    pub fn with_tracking(mut self, hook: impl Fn(&Key) + 'static) -> Self {
        self.on_track = Some(Rc::new(hook));
        self
    }

    pub fn subscribe(&self, key: Key, callback: impl Fn(&Key) + 'static) -> Subscription {
        let id = self.state.next.get();
        self.state.next.set(id + 1);
        self.state.subscribers.borrow_mut().entry(key.clone()).or_default().push((id, Rc::new(callback)));
        Subscription { state: Rc::downgrade(&self.state), key, id }
    }

    fn deliver(&self, key: &Key) {
        for key in key.lineage() {
            let callbacks: Vec<Callback> = self
                .state
                .subscribers
                .borrow()
                .get(&key)
                .map(|list| list.iter().map(|(_, c)| c.clone()).collect())
                .unwrap_or_default();
            for callback in callbacks {
                callback(&key);
            }
        }
    }
}

impl Reactivity for Observers {
    fn track(&self, key: &Key) {
        if let Some(hook) = &self.on_track {
            hook(key);
        }
    }

    fn trigger(&self, key: &Key) {
        if self.state.depth.get() > 0 {
            let mut queued = self.state.queued.borrow_mut();
            if !queued.contains(key) {
                queued.push(key.clone());
            }
            return;
        }
        self.deliver(key);
    }

    fn batch(&self, f: &mut dyn FnMut()) {
        self.state.depth.set(self.state.depth.get() + 1);
        f();
        self.state.depth.set(self.state.depth.get() - 1);
        if self.state.depth.get() == 0 {
            let queued = std::mem::take(&mut *self.state.queued.borrow_mut());
            for key in queued {
                self.deliver(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_reach_ancestors_once_per_batch() {
        let observers = Observers::new();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        let _messages = observers.subscribe(Key::Messages("s".into()), move |k| s.borrow_mut().push(format!("{k:?}")));
        let s = seen.clone();
        let _other = observers.subscribe(Key::Message("s".into(), "b".into()), move |k| s.borrow_mut().push(format!("{k:?}")));
        let part = Key::Part("s".into(), "a".into(), 0);
        batch(&observers, || {
            observers.trigger(&part);
            observers.trigger(&part);
        });
        assert_eq!(*seen.borrow(), vec!["Messages(\"s\")"]);
    }
}
