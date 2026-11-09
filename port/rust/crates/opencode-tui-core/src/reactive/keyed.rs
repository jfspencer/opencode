//! Change notification by key: one signal per key that has been read, for state outside the reactive graph (the
//! client store's `Reactivity`: a read tracks its key, a change triggers it).
//!
//! The signals live for the process (created unowned): a store's keys outlive the components that read them.
//! TODO(port-tui): drop the signals of keys a store forgets (evicted sessions).
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;

use super::runtime::{Signal, batch, create_root_unowned, create_signal_with, untrack};

pub struct KeyedSignals<K> {
    signals: RefCell<HashMap<K, Signal<()>>>,
}

impl<K: Clone + Eq + Hash> Default for KeyedSignals<K> {
    fn default() -> Self {
        KeyedSignals { signals: RefCell::new(HashMap::new()) }
    }
}

impl<K: Clone + Eq + Hash> KeyedSignals<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// A read of `key`: subscribes the running computation.
    pub fn track(&self, key: &K) {
        let existing = self.signals.borrow().get(key).copied();
        let signal = existing.unwrap_or_else(|| {
            let signal = create_root_unowned(|| create_signal_with((), None).0);
            self.signals.borrow_mut().insert(key.clone(), signal);
            signal
        });
        signal.get();
    }

    /// `key` changed: its readers re-run.
    pub fn trigger(&self, key: &K) {
        let signal = self.signals.borrow().get(key).copied();
        if let Some(signal) = signal {
            signal.set(());
        }
    }

    /// Every tracked key that passes `f` (descendants of a changed key).
    pub fn keys_where(&self, f: impl Fn(&K) -> bool) -> Vec<K> {
        self.signals.borrow().keys().filter(|k| f(k)).cloned().collect()
    }

    pub fn batch(&self, f: &mut dyn FnMut()) {
        batch(f);
    }

    pub fn untrack(&self, f: &mut dyn FnMut()) {
        untrack(f);
    }
}
