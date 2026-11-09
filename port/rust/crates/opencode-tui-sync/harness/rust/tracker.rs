//! A minimal reactive layer for the harness, with Solid's effect semantics as far as the data layer's consumers
//! need them: an effect tracks the keys it reads, re-runs when a change reaches one of them, after the outermost
//! batch (at once outside a batch), and reads under `untrack` are not dependencies.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_tui_sync::reactive::{Key, Reactivity};

struct Effect {
    run: Box<dyn Fn()>,
    deps: RefCell<Vec<Key>>,
}

#[derive(Default)]
pub struct Tracker {
    effects: RefCell<Vec<Rc<Effect>>>,
    running: RefCell<Vec<Rc<Effect>>>,
    untracked: Cell<usize>,
    depth: Cell<usize>,
    dirty: RefCell<Vec<Rc<Effect>>>,
    flushing: Cell<bool>,
}

impl Tracker {
    /// `createEffect(fn)`: runs now, then whenever what it read changes.
    pub fn effect(&self, run: impl Fn() + 'static) {
        let effect = Rc::new(Effect { run: Box::new(run), deps: RefCell::new(Vec::new()) });
        self.effects.borrow_mut().push(effect.clone());
        self.execute(&effect);
    }

    fn execute(&self, effect: &Rc<Effect>) {
        effect.deps.borrow_mut().clear();
        self.running.borrow_mut().push(effect.clone());
        let untracked = self.untracked.replace(0);
        (effect.run)();
        self.untracked.set(untracked);
        self.running.borrow_mut().pop();
    }

    fn flush(&self) {
        if self.flushing.replace(true) {
            return;
        }
        loop {
            let next = {
                let mut dirty = self.dirty.borrow_mut();
                if dirty.is_empty() { None } else { Some(dirty.remove(0)) }
            };
            let Some(effect) = next else { break };
            self.execute(&effect);
        }
        self.flushing.set(false);
    }
}

impl Reactivity for Tracker {
    fn track(&self, key: &Key) {
        if self.untracked.get() > 0 {
            return;
        }
        if let Some(effect) = self.running.borrow().last() {
            let mut deps = effect.deps.borrow_mut();
            if !deps.contains(key) {
                deps.push(key.clone());
            }
        }
    }

    fn trigger(&self, key: &Key) {
        for effect in self.effects.borrow().iter() {
            let reached = effect.deps.borrow().iter().any(|dep| dep.affected_by(key));
            let queued = self.dirty.borrow().iter().any(|d| Rc::ptr_eq(d, effect));
            if reached && !queued {
                self.dirty.borrow_mut().push(effect.clone());
            }
        }
        if self.depth.get() == 0 {
            self.flush();
        }
    }

    fn batch(&self, f: &mut dyn FnMut()) {
        self.depth.set(self.depth.get() + 1);
        f();
        self.depth.set(self.depth.get() - 1);
        if self.depth.get() == 0 {
            self.flush();
        }
    }

    fn untrack(&self, f: &mut dyn FnMut()) {
        self.untracked.set(self.untracked.get() + 1);
        f();
        self.untracked.set(self.untracked.get() - 1);
    }
}
