//! `KeyEvent`, `PasteEvent` and the renderer's key dispatch (`KeyHandler`, `InternalKeyHandler`) of
//! `packages/core/src/lib/KeyHandler.ts`.
//!
//! Listeners are `Rc` closures registered for one event kind; `on_*` returns the id `off` takes (TS removes a
//! listener by identity). As in Node's `EventEmitter`, an emit runs the listeners registered when it began. A
//! panicking listener is not caught (TS logs a throwing one and goes on).
use crate::keypress::{KeyEventType, ParsedKey};
use crate::paste::PasteMetadata;
use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::rc::Rc;

/// `KeyEvent`: a `ParsedKey` that handlers can mark default-prevented or stopped.
#[derive(Debug)]
pub struct KeyEvent {
    key: ParsedKey,
    default_prevented: Cell<bool>,
    propagation_stopped: Cell<bool>,
}

impl KeyEvent {
    pub fn new(key: ParsedKey) -> KeyEvent {
        KeyEvent { key, default_prevented: Cell::new(false), propagation_stopped: Cell::new(false) }
    }

    pub fn key(&self) -> &ParsedKey {
        &self.key
    }

    pub fn default_prevented(&self) -> bool {
        self.default_prevented.get()
    }

    pub fn propagation_stopped(&self) -> bool {
        self.propagation_stopped.get()
    }

    pub fn prevent_default(&self) {
        self.default_prevented.set(true);
    }

    pub fn stop_propagation(&self) {
        self.propagation_stopped.set(true);
    }
}

impl Deref for KeyEvent {
    type Target = ParsedKey;
    fn deref(&self) -> &ParsedKey {
        &self.key
    }
}

/// `PasteEvent`.
#[derive(Debug)]
pub struct PasteEvent {
    pub bytes: Vec<u8>,
    pub metadata: Option<PasteMetadata>,
    default_prevented: Cell<bool>,
    propagation_stopped: Cell<bool>,
}

impl PasteEvent {
    pub fn new(bytes: Vec<u8>, metadata: Option<PasteMetadata>) -> PasteEvent {
        PasteEvent { bytes, metadata, default_prevented: Cell::new(false), propagation_stopped: Cell::new(false) }
    }

    pub fn default_prevented(&self) -> bool {
        self.default_prevented.get()
    }

    pub fn propagation_stopped(&self) -> bool {
        self.propagation_stopped.get()
    }

    pub fn prevent_default(&self) {
        self.default_prevented.set(true);
    }

    pub fn stop_propagation(&self) {
        self.propagation_stopped.set(true);
    }
}

/// `KeyHandlerEventMap`'s events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyHandlerEvent {
    Keypress,
    Keyrelease,
    Paste,
}

/// What a listener receives.
pub enum KeyHandlerPayload<'a> {
    Key(&'a KeyEvent),
    Paste(&'a PasteEvent),
}

impl KeyHandlerPayload<'_> {
    fn default_prevented(&self) -> bool {
        match self {
            KeyHandlerPayload::Key(event) => event.default_prevented(),
            KeyHandlerPayload::Paste(event) => event.default_prevented(),
        }
    }

    fn propagation_stopped(&self) -> bool {
        match self {
            KeyHandlerPayload::Key(event) => event.propagation_stopped(),
            KeyHandlerPayload::Paste(event) => event.propagation_stopped(),
        }
    }
}

pub type Listener = Rc<dyn Fn(&KeyHandlerPayload<'_>)>;

/// A registered listener's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ListenerId(u64);

#[derive(Default)]
struct Listeners {
    next: Cell<u64>,
    global: RefCell<Vec<(ListenerId, KeyHandlerEvent, Listener)>>,
    renderable: RefCell<Vec<(ListenerId, KeyHandlerEvent, Listener)>>,
}

/// `InternalKeyHandler` (the renderer's `KeyHandler`): global listeners run first, in order, each able to stop
/// propagation; then the renderables' handlers, unless an event was default-prevented or stopped.
#[derive(Default)]
pub struct KeyHandler {
    listeners: Listeners,
}

impl KeyHandler {
    pub fn new() -> KeyHandler {
        KeyHandler::default()
    }

    fn add(&self, renderable: bool, event: KeyHandlerEvent, listener: Listener) -> ListenerId {
        let id = ListenerId(self.listeners.next.get());
        self.listeners.next.set(id.0 + 1);
        let list = if renderable { &self.listeners.renderable } else { &self.listeners.global };
        list.borrow_mut().push((id, event, listener));
        id
    }

    /// `on(event, listener)`: a global listener.
    pub fn on(&self, event: KeyHandlerEvent, listener: Listener) -> ListenerId {
        self.add(false, event, listener)
    }

    /// `onInternal(event, handler)`: a renderable's handler. Adding one already registered (the same `Rc`) for
    /// the event keeps the first, as a `Set` does.
    pub fn on_internal(&self, event: KeyHandlerEvent, handler: Listener) -> ListenerId {
        let existing = self
            .listeners
            .renderable
            .borrow()
            .iter()
            .find(|(_, kind, listener)| *kind == event && Rc::ptr_eq(listener, &handler))
            .map(|(id, _, _)| *id);
        existing.unwrap_or_else(|| self.add(true, event, handler))
    }

    /// `off` / `offInternal`.
    pub fn off(&self, id: ListenerId) {
        self.listeners.global.borrow_mut().retain(|(listener, _, _)| *listener != id);
        self.listeners.renderable.borrow_mut().retain(|(listener, _, _)| *listener != id);
    }

    fn snapshot(list: &RefCell<Vec<(ListenerId, KeyHandlerEvent, Listener)>>, event: KeyHandlerEvent) -> Vec<Listener> {
        list.borrow().iter().filter(|(_, kind, _)| *kind == event).map(|(_, _, listener)| listener.clone()).collect()
    }

    /// `emitWithPriority`: whether any listener was registered.
    pub fn emit(&self, event: KeyHandlerEvent, payload: &KeyHandlerPayload<'_>) -> bool {
        let global = Self::snapshot(&self.listeners.global, event);
        let has_global = !global.is_empty();
        for listener in &global {
            listener(payload);
            if payload.propagation_stopped() {
                return has_global;
            }
        }
        let renderable = Self::snapshot(&self.listeners.renderable, event);
        if renderable.is_empty() {
            return has_global;
        }
        if payload.default_prevented() || payload.propagation_stopped() {
            return true;
        }
        for handler in &renderable {
            handler(payload);
            if payload.propagation_stopped() {
                return true;
            }
        }
        true
    }

    /// `processParsedKey`: a press (or repeat) is a `keypress`, a release a `keyrelease`.
    pub fn process_parsed_key(&self, key: ParsedKey) -> bool {
        let event = match key.event_type {
            KeyEventType::Release => KeyHandlerEvent::Keyrelease,
            _ => KeyHandlerEvent::Keypress,
        };
        let key = KeyEvent::new(key);
        self.emit(event, &KeyHandlerPayload::Key(&key));
        true
    }

    /// `processPaste`.
    pub fn process_paste(&self, bytes: Vec<u8>, metadata: Option<PasteMetadata>) {
        let paste = PasteEvent::new(bytes, metadata);
        self.emit(KeyHandlerEvent::Paste, &KeyHandlerPayload::Paste(&paste));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keypress::parse_keypress;

    fn recorder(log: &Rc<RefCell<Vec<String>>>, label: &'static str, stop: bool, prevent: bool) -> Listener {
        let log = log.clone();
        Rc::new(move |payload: &KeyHandlerPayload<'_>| {
            log.borrow_mut().push(label.to_owned());
            if let KeyHandlerPayload::Key(event) = payload {
                if stop {
                    event.stop_propagation();
                }
                if prevent {
                    event.prevent_default();
                }
            }
        })
    }

    #[test]
    fn global_listeners_run_before_renderables() {
        let handler = KeyHandler::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        handler.on_internal(KeyHandlerEvent::Keypress, recorder(&log, "renderable", false, false));
        handler.on(KeyHandlerEvent::Keypress, recorder(&log, "global", false, false));
        handler.process_parsed_key(parse_keypress("a", true).unwrap());
        assert_eq!(*log.borrow(), ["global", "renderable"]);
    }

    #[test]
    fn prevent_default_and_stop_propagation() {
        let handler = KeyHandler::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let first = handler.on(KeyHandlerEvent::Keypress, recorder(&log, "first", false, true));
        handler.on(KeyHandlerEvent::Keypress, recorder(&log, "second", false, false));
        handler.on_internal(KeyHandlerEvent::Keypress, recorder(&log, "renderable", false, false));
        handler.process_parsed_key(parse_keypress("a", true).unwrap());
        assert_eq!(*log.borrow(), ["first", "second"]);
        handler.off(first);
        log.borrow_mut().clear();
        handler.on(KeyHandlerEvent::Keypress, recorder(&log, "stopper", true, false));
        handler.on(KeyHandlerEvent::Keypress, recorder(&log, "after", false, false));
        handler.process_parsed_key(parse_keypress("a", true).unwrap());
        assert_eq!(*log.borrow(), ["second", "stopper"]);
    }

    #[test]
    fn releases_and_pastes() {
        let handler = KeyHandler::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        handler.on(KeyHandlerEvent::Keyrelease, recorder(&log, "release", false, false));
        handler.on(KeyHandlerEvent::Paste, recorder(&log, "paste", false, false));
        handler.process_parsed_key(parse_keypress("\x1b[97;1:3u", true).unwrap());
        handler.process_paste(b"text".to_vec(), None);
        assert_eq!(*log.borrow(), ["release", "paste"]);
    }
}
