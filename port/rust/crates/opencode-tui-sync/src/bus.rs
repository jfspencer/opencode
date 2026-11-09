//! `createGlobalEmitter` (@solid-primitives/event-bus 1.1.2), the client context's event bus: `emit(type, event)`
//! calls every `listen` handler (with `{ name, details }`), then the handlers `on` that type, each in subscription
//! order. A handler removed while an emit runs is skipped if it has not run yet.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::event::Event;

type Handler = Rc<dyn Fn(&Event)>;

#[derive(Default)]
struct State {
    global: RefCell<Vec<(u64, Handler)>>,
    typed: RefCell<HashMap<String, Vec<(u64, Handler)>>>,
    next: Cell<u64>,
}

#[derive(Clone, Default)]
pub struct Emitter(Rc<State>);

/// Removes its handler when called (`on`/`listen` return it); dropping it keeps the handler.
#[must_use = "keep the unsubscribe handle to remove the handler later"]
pub struct Unsubscribe(Option<Box<dyn FnOnce()>>);

impl Unsubscribe {
    pub fn new(f: impl FnOnce() + 'static) -> Self {
        Self(Some(Box::new(f)))
    }

    pub fn call(mut self) {
        if let Some(f) = self.0.take() {
            f()
        }
    }
}

impl Emitter {
    pub fn new() -> Self {
        Self::default()
    }

    fn id(&self) -> u64 {
        let id = self.0.next.get();
        self.0.next.set(id + 1);
        id
    }

    /// `on(type, handler)`.
    pub fn on(&self, r#type: &str, handler: impl Fn(&Event) + 'static) -> Unsubscribe {
        let id = self.id();
        self.0.typed.borrow_mut().entry(r#type.to_owned()).or_default().push((id, Rc::new(handler)));
        let state = Rc::downgrade(&self.0);
        let r#type = r#type.to_owned();
        Unsubscribe::new(move || {
            if let Some(state) = state.upgrade() {
                let mut typed = state.typed.borrow_mut();
                if let Some(list) = typed.get_mut(&r#type) {
                    list.retain(|(other, _)| *other != id);
                    if list.is_empty() {
                        typed.remove(&r#type);
                    }
                }
            }
        })
    }

    /// `listen(handler)`: every event.
    pub fn listen(&self, handler: impl Fn(&Event) + 'static) -> Unsubscribe {
        let id = self.id();
        self.0.global.borrow_mut().push((id, Rc::new(handler)));
        let state = Rc::downgrade(&self.0);
        Unsubscribe::new(move || {
            if let Some(state) = state.upgrade() {
                state.global.borrow_mut().retain(|(other, _)| *other != id);
            }
        })
    }

    /// `emit(event.type, event)`.
    pub fn emit(&self, event: &Event) {
        let global: Vec<(u64, Handler)> = self.0.global.borrow().clone();
        for (id, handler) in global {
            if self.0.global.borrow().iter().any(|(other, _)| *other == id) {
                handler(event);
            }
        }
        let r#type = event.r#type();
        let typed: Vec<(u64, Handler)> = self.0.typed.borrow().get(r#type).cloned().unwrap_or_default();
        for (id, handler) in typed {
            let present = self.0.typed.borrow().get(r#type).is_some_and(|list| list.iter().any(|(other, _)| *other == id));
            if present {
                handler(event);
            }
        }
    }

    /// `clear()`: removes every typed handler (the emitter's cleanup).
    pub fn clear(&self) {
        self.0.typed.borrow_mut().clear();
    }
}
