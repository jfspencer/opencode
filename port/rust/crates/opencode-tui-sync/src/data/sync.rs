//! `createSync()` (solid/data.ts): loads by key, once until invalidated. An invalidated load in flight revalidates
//! serially, after the load it replaces, so an older response cannot commit after its replacement.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use indexmap::IndexMap;

use crate::client::Error;
use crate::runtime::{self, Deferred};
use crate::transport::LocalBoxFuture;

pub type Promise = Deferred<Result<(), Error>>;

/// A load: its synchronous part runs when it is called, the returned future is the rest.
pub type Load = Box<dyn FnOnce() -> LocalBoxFuture<'static, Result<(), Error>>>;

struct Pending {
    promise: Promise,
    invalidated: Cell<bool>,
    /// False while a reload waits for the load it replaces: invalidations then are already covered.
    started: Cell<bool>,
}

#[derive(Clone)]
enum Entry {
    Done,
    Pending(Rc<Pending>),
}

#[derive(Default)]
pub struct Sync {
    state: RefCell<IndexMap<String, Entry>>,
}

impl Sync {
    fn is_entry(&self, key: &str, entry: &Rc<Pending>) -> bool {
        matches!(self.state.borrow().get(key), Some(Entry::Pending(e)) if Rc::ptr_eq(e, entry))
    }

    fn start(self: &Rc<Self>, key: &str, load: Load, wait: Option<Promise>) -> Promise {
        let entry = Rc::new(Pending { promise: Deferred::new(), invalidated: Cell::new(false), started: Cell::new(wait.is_none()) });
        self.state.borrow_mut().insert(key.to_owned(), Entry::Pending(entry.clone()));
        let this = self.clone();
        let key = key.to_owned();
        let promise = entry.promise.clone();
        // A load that waits starts in its own task; one that does not runs its synchronous part now.
        let (running, mut load) = match wait {
            None => (Some(load()), None),
            Some(_) => (None, Some(load)),
        };
        runtime::spawn(async move {
            let result = match running {
                Some(running) => running.await,
                None => {
                    if let Some(wait) = wait {
                        let _ = wait.wait().await;
                    }
                    entry.started.set(true);
                    (load.take().expect("load runs once"))().await
                }
            };
            if result.is_ok() && this.is_entry(&key, &entry) && !entry.invalidated.get() {
                this.state.borrow_mut().insert(key.clone(), Entry::Done);
            }
            if this.is_entry(&key, &entry) {
                this.state.borrow_mut().shift_remove(&key);
            }
            entry.promise.resolve(result);
        });
        promise
    }

    /// `sync.run(key, load)`.
    pub fn run(self: &Rc<Self>, key: &str, load: Load) -> Promise {
        let active = self.state.borrow().get(key).cloned();
        match active {
            Some(Entry::Done) => Deferred::resolved(Ok(())),
            None => self.start(key, load, None),
            Some(Entry::Pending(active)) if !active.invalidated.get() => active.promise.clone(),
            Some(Entry::Pending(active)) => self.start(key, load, Some(active.promise.clone())),
        }
    }

    /// `sync.complete(key)`: marks a key loaded unless it is known.
    pub fn complete(&self, key: &str) {
        if self.state.borrow().contains_key(key) {
            return;
        }
        self.state.borrow_mut().insert(key.to_owned(), Entry::Done);
    }

    pub fn has(&self, key: &str) -> bool {
        self.state.borrow().contains_key(key)
    }

    /// Whether a load of the key is in flight.
    pub fn pending(&self, key: &str) -> bool {
        matches!(self.state.borrow().get(key), Some(Entry::Pending(_)))
    }

    /// `sync.invalidate(key)`; every key without one.
    pub fn invalidate(&self, key: Option<&str>) {
        let mut state = self.state.borrow_mut();
        match key {
            Some(key) => match state.get(key).cloned() {
                Some(Entry::Done) => {
                    state.shift_remove(key);
                }
                Some(Entry::Pending(active)) if active.started.get() => active.invalidated.set(true),
                _ => {}
            },
            None => state.retain(|_, entry| match entry {
                Entry::Done => false,
                Entry::Pending(active) => {
                    if active.started.get() {
                        active.invalidated.set(true);
                    }
                    true
                }
            }),
        }
    }

    /// The state of every key, for the harness: `done`, `pending`, `invalidated` or `waiting`.
    pub fn snapshot(&self) -> Vec<(String, &'static str)> {
        self.state
            .borrow()
            .iter()
            .map(|(key, entry)| {
                let state = match entry {
                    Entry::Done => "done",
                    Entry::Pending(p) if !p.started.get() => "waiting",
                    Entry::Pending(p) if p.invalidated.get() => "invalidated",
                    Entry::Pending(_) => "pending",
                };
                (key.clone(), state)
            })
            .collect()
    }
}
