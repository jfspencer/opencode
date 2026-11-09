//! `@opentui/keymap` 0.8.1 (`packages/keymap/src`), as the TUI configures it: layers of bindings and commands,
//! binding strings through the default parser and the registered expanders, key matching through the event match
//! resolvers, multi-key sequences with a pending prefix, tokens (the leader), layer precedence (priority, then the
//! newest registration), focus targets, conditions (`enabled`, `require`d data), key intercepts, command dispatch by
//! name, and the queries the TUI reads (pending sequence, active keys, commands and their bindings).
//!
//! The engine is one `Keymap` (an `Rc` handle, so commands and intercepts can call back into it) generic over the
//! host's target type. Each service of the TS (`compiler`, `layers`, `activation`, `dispatch`, `command-catalog`,
//! `command-executor`, `conditions`, `notify`) is a group of methods here, with the TS names.
//!
//! # Not ported (unused by the TUI)
//!
//! Sequence patterns (`registerSequencePattern`, `{name}` parts: always an unknown token here), disambiguation
//! resolvers (`registerNeovimDisambiguation`), command resolvers, binding/command/layer-bindings transformers, layer
//! analyzers and their diagnostics, `key:after` and `raw` intercepts, reactive matchers (subscriptions), `dispatch`
//! events, query filters (`namespace`, `search`, `filter`, `limit`), payloads. `registerManagedTextareaLayer` and
//! the edit-buffer commands belong with the textarea renderable.
//!
//! # Divergences
//!
//! A destroyed host makes queries empty and mutations no-ops where TS throws "Cannot use a keymap after its host
//! was destroyed". A matcher, field compiler, expander or parser must not mutate the keymap (they run inside its
//! bookkeeping; TS allows it). Errors and warnings go to `on_error`/`on_warning` listeners, or nowhere (TS
//! falls back to `console`).
#![allow(clippy::type_complexity, clippy::too_many_arguments)]

pub mod addons;
pub mod format;
pub mod keys;
pub mod parser;
pub mod types;

pub use keys::*;
pub use parser::{ParserContext, ParserResult, ResolvedToken, default_binding_parser};
pub use types::*;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

/// A field compiler (`LayerFieldCompiler`, `BindingFieldCompiler`, `CommandFieldCompiler`).
pub type FieldCompiler = Rc<dyn Fn(&Field, &mut FieldContext<'_>) -> Result<(), String>>;
/// `BindingExpander`: `None` leaves the key as it is.
pub type BindingExpander = Rc<dyn Fn(&ExpanderContext<'_>) -> Result<Option<Vec<Expansion>>, String>>;
/// `BindingParser`.
pub type BindingParser = Rc<dyn Fn(&ParserContext<'_>) -> Result<Option<ParserResult>, String>>;
/// `EventMatchResolver`: the match ids an event can take, through `resolve_key`.
pub type EventMatchResolver =
    Rc<dyn Fn(&dyn KeymapEvent, &dyn Fn(&KeyStrokeInput) -> Result<KeyMatch, String>) -> Result<Vec<KeyMatch>, String>>;
/// A `key` intercept.
pub type KeyIntercept = Rc<dyn Fn(&KeyInputContext<'_>)>;
/// A diagnostic listener: the code and the message.
pub type DiagnosticListener = Rc<dyn Fn(&str, &str)>;
/// `() => void` of a registration.
pub type Dispose = Box<dyn FnOnce()>;

/// `BindingExpanderContext`.
pub struct ExpanderContext<'a> {
    pub input: &'a str,
    pub displays: Option<&'a [String]>,
    pub layer: &'a Fields,
}

/// `BindingExpansion`.
#[derive(Clone, Debug, PartialEq)]
pub struct Expansion {
    pub key: String,
    pub displays: Option<Vec<String>>,
}

/// The context of a field compiler (`require`, `attr`, `activeWhen`).
pub struct FieldContext<'a> {
    source: String,
    requires: &'a mut Vec<(String, Data)>,
    attrs: Option<&'a mut Attrs>,
    matchers: &'a mut Vec<Matcher>,
}

impl FieldContext<'_> {
    pub fn require(&mut self, name: &str, value: Data) -> Result<(), String> {
        if let Some((_, existing)) = self.requires.iter().find(|(key, _)| key == name) {
            if *existing != value {
                return Err(format!("Conflicting keymap requirement for \"{name}\" from {}", self.source));
            }
            return Ok(());
        }
        self.requires.push((name.to_owned(), value));
        Ok(())
    }

    pub fn attr(&mut self, name: &str, value: String) -> Result<(), String> {
        let source = self.source.clone();
        let Some(attrs) = self.attrs.as_deref_mut() else {
            return Err(format!("Keymap {source} cannot publish attrs"));
        };
        if let Some((_, existing)) = attrs.iter().find(|(key, _)| key == name) {
            if *existing != value {
                return Err(format!("Conflicting keymap attribute for \"{name}\" from {source}"));
            }
            return Ok(());
        }
        attrs.push((name.to_owned(), value));
        Ok(())
    }

    pub fn active_when(&mut self, matcher: Matcher) {
        self.matchers.push(matcher);
    }
}

/// `KeyInputContext`: what a `key` intercept sees.
pub struct KeyInputContext<'a> {
    pub event: &'a dyn KeymapEvent,
    data: Rc<dyn Fn(&str, Option<Data>)>,
    get: Rc<dyn Fn(&str) -> Option<Data>>,
}

impl KeyInputContext<'_> {
    /// `consume(options)`: prevent the default and stop propagation (each unless false).
    pub fn consume(&self, prevent_default: bool, stop_propagation: bool) {
        if prevent_default {
            self.event.prevent_default();
        }
        if stop_propagation {
            self.event.stop_propagation();
        }
    }

    pub fn set_data(&self, name: &str, value: Option<Data>) {
        (self.data)(name, value);
    }

    pub fn get_data(&self, name: &str) -> Option<Data> {
        (self.get)(name)
    }
}

const RESERVED_BINDING_FIELDS: [&str; 5] = ["key", "cmd", "event", "preventDefault", "fallthrough"];
const RESERVED_LAYER_FIELDS: [&str; 5] = ["target", "targetMode", "priority", "bindings", "commands"];
const RESERVED_COMMAND_FIELDS: [&str; 5] = ["name", "run", "fields", "attrs", "rejectedResult"];

/// A condition-bearing thing (`RuntimeMatchable`).
#[derive(Default)]
struct Conditions {
    requires: Vec<(String, Data)>,
    matchers: Vec<Matcher>,
}

struct BindingState<T> {
    sequence: Vec<KeySequencePart>,
    command: Option<BindingCommand<T>>,
    release: bool,
    attrs: Option<Rc<Attrs>>,
    conditions: Conditions,
    prevent_default: bool,
    fallthrough: bool,
    source: Binding<T>,
}

struct CommandState<T> {
    command: Rc<Command<T>>,
    attrs: Option<Rc<Attrs>>,
    conditions: Conditions,
}

struct RegisteredLayer<T> {
    order: u64,
    target: Option<T>,
    target_mode: Option<TargetMode>,
    priority: i32,
    conditions: Conditions,
    fields: Fields,
    commands: Vec<Rc<CommandState<T>>>,
    source_bindings: Vec<Binding<T>>,
    bindings: RefCell<Vec<Rc<BindingState<T>>>>,
    has_token_bindings: Cell<bool>,
}

struct Capture<T> {
    layer: Rc<RegisteredLayer<T>>,
    binding: Rc<BindingState<T>>,
    index: usize,
    parts: Vec<KeySequencePart>,
}

impl<T> Clone for Capture<T> {
    fn clone(&self) -> Self {
        Capture {
            layer: self.layer.clone(),
            binding: self.binding.clone(),
            index: self.index,
            parts: self.parts.clone(),
        }
    }
}

impl<T> Capture<T> {
    fn has_continuations(&self) -> bool {
        self.index + 1 < self.binding.sequence.len()
    }

    fn is_exact(&self) -> bool {
        self.index + 1 == self.binding.sequence.len()
    }
}

struct Registered<V> {
    id: u64,
    value: V,
}

struct KeyHook {
    id: u64,
    priority: i32,
    release: bool,
    order: u64,
    listener: KeyIntercept,
}

struct State<T> {
    order: u64,
    next_id: u64,
    tokens: HashMap<String, ResolvedToken>,
    layer_fields: HashMap<String, FieldCompiler>,
    binding_fields: HashMap<String, FieldCompiler>,
    command_fields: HashMap<String, FieldCompiler>,
    expanders: Vec<Registered<BindingExpander>>,
    parsers: Vec<Registered<BindingParser>>,
    resolvers: Vec<Registered<EventMatchResolver>>,
    key_hooks: Vec<KeyHook>,
    layers: Vec<Rc<RegisteredLayer<T>>>,
    sorted_layers: Vec<Rc<RegisteredLayer<T>>>,
    pending: Option<Vec<Capture<T>>>,
    data: Rc<BTreeMap<String, Data>>,
    state_change_depth: u32,
    state_change_pending: bool,
    flushing: bool,
    used_warning_keys: HashSet<String>,
    resources: HashMap<&'static str, (u32, Option<Dispose>)>,
    state_listeners: Vec<Registered<Rc<dyn Fn()>>>,
    pending_listeners: Vec<Registered<Rc<dyn Fn(&[KeySequencePart])>>>,
    error_listeners: Vec<Registered<DiagnosticListener>>,
    warning_listeners: Vec<Registered<DiagnosticListener>>,
    cleaned_up: bool,
    /// `cacheVersion` and `derivedVersion`: bumped by each queued state change (a focus change bumps only the
    /// second). Like TS, the caches below see a layer or data change only once its state change is queued.
    cache_version: u64,
    derived_version: u64,
    active_layers_cache: Option<(u64, Option<T>, Vec<Rc<RegisteredLayer<T>>>)>,
    command_view_cache: Option<(u64, Option<T>, Rc<CommandView<T>>)>,
    /// `activeCommandViewCacheBlockers`: layers with commands and matchers.
    command_view_blockers: usize,
}

struct Inner<T> {
    host: Rc<dyn KeymapHost<T>>,
    state: RefCell<State<T>>,
}

/// `Keymap`.
pub struct Keymap<T>(Rc<Inner<T>>);

impl<T> Clone for Keymap<T> {
    fn clone(&self) -> Self {
        Keymap(self.0.clone())
    }
}

/// A mount position whose layer `replace_layer` swaps, keeping its order (`LayerSlot`).
pub struct LayerSlot<T> {
    order: u64,
    layer: RefCell<Option<Rc<RegisteredLayer<T>>>>,
}

/// `createKeyMatcher`'s matcher.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyMatcher(KeyMatch);

impl KeyMatcher {
    pub fn match_id(&self) -> &KeyMatch {
        &self.0
    }

    /// An event, by its normalized stroke (`resolveKeyMatch` of a key event); an event without a name is the
    /// error TS throws.
    pub fn matches_event(&self, event: &dyn KeymapEvent) -> Result<bool, String> {
        Ok(create_key_match(&event_stroke_input(event))? == self.0)
    }

    /// A sequence part, by its match id.
    pub fn matches_part(&self, part: Option<&KeySequencePart>) -> bool {
        part.is_some_and(|part| part.match_ == self.0)
    }
}

fn event_stroke_input(event: &dyn KeymapEvent) -> KeyStrokeInput {
    KeyStrokeInput {
        name: event.name().to_owned(),
        ctrl: Some(event.ctrl()),
        shift: Some(event.shift()),
        meta: Some(event.meta()),
        super_: Some(event.super_key()),
        hyper: event.hyper().then_some(true),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reason {
    NoMatch,
    Handled,
}

struct RunResult {
    handled: bool,
    stop: bool,
}

/// The active command view (`ActiveCommandView`).
struct CommandView<T> {
    layers: Vec<Rc<RegisteredLayer<T>>>,
    reachable: Vec<(Rc<RegisteredLayer<T>>, Rc<CommandState<T>>)>,
    active: Vec<(Rc<RegisteredLayer<T>>, Rc<CommandState<T>>)>,
}

impl<T> CommandView<T> {
    fn reachable_by_name(&self, name: &str) -> Option<&(Rc<RegisteredLayer<T>>, Rc<CommandState<T>>)> {
        self.reachable.iter().find(|(_, command)| command.command.name == name)
    }

    fn chain(&self, name: &str) -> Vec<(Rc<RegisteredLayer<T>>, Rc<CommandState<T>>)> {
        self.active.iter().filter(|(_, command)| command.command.name == name).cloned().collect()
    }
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

impl<T: Clone + PartialEq + 'static> Keymap<T> {
    /// `new Keymap(host)`, without any parser, matcher or field: see `create_default_keymap`.
    pub fn new(host: Rc<dyn KeymapHost<T>>) -> Keymap<T> {
        Keymap(Rc::new(Inner {
            host,
            state: RefCell::new(State {
                order: 0,
                next_id: 0,
                tokens: HashMap::new(),
                layer_fields: HashMap::new(),
                binding_fields: HashMap::new(),
                command_fields: HashMap::new(),
                expanders: Vec::new(),
                parsers: Vec::new(),
                resolvers: Vec::new(),
                key_hooks: Vec::new(),
                layers: Vec::new(),
                sorted_layers: Vec::new(),
                pending: None,
                data: Rc::new(BTreeMap::new()),
                state_change_depth: 0,
                state_change_pending: false,
                flushing: false,
                used_warning_keys: HashSet::new(),
                resources: HashMap::new(),
                state_listeners: Vec::new(),
                pending_listeners: Vec::new(),
                error_listeners: Vec::new(),
                warning_listeners: Vec::new(),
                cleaned_up: false,
                cache_version: 0,
                derived_version: 0,
                active_layers_cache: None,
                command_view_cache: None,
                command_view_blockers: 0,
            }),
        }))
    }

    /// `createDefaultOpenTuiKeymap`: the default parser and matcher, `enabled` and metadata fields.
    pub fn create_default(host: Rc<dyn KeymapHost<T>>) -> Keymap<T> {
        let keymap = Keymap::new(host);
        let _ = addons::register_default_keys(&keymap);
        let _ = addons::register_enabled_fields(&keymap);
        let _ = addons::register_metadata_fields(&keymap);
        keymap
    }

    fn host(&self) -> &Rc<dyn KeymapHost<T>> {
        &self.0.host
    }

    fn next_id(&self) -> u64 {
        let mut state = self.0.state.borrow_mut();
        state.next_id += 1;
        state.next_id
    }

    // ----- notify

    fn emit_error(&self, code: &str, message: &str) {
        let listeners: Vec<DiagnosticListener> =
            self.0.state.borrow().error_listeners.iter().map(|entry| entry.value.clone()).collect();
        for listener in listeners {
            listener(code, message);
        }
    }

    fn warn_once(&self, key: String, code: &str, message: &str) {
        if !self.0.state.borrow_mut().used_warning_keys.insert(key) {
            return;
        }
        let listeners: Vec<DiagnosticListener> =
            self.0.state.borrow().warning_listeners.iter().map(|entry| entry.value.clone()).collect();
        for listener in listeners {
            listener(code, message);
        }
    }

    fn batch<R>(&self, operation: impl FnOnce() -> R) -> R {
        self.0.state.borrow_mut().state_change_depth += 1;
        let result = operation();
        let flush = {
            let mut state = self.0.state.borrow_mut();
            state.state_change_depth -= 1;
            state.state_change_depth == 0
        };
        if flush {
            self.flush_state_change();
        }
        result
    }

    fn queue_state_change(&self) {
        self.queue_state_change_with(true);
    }

    fn queue_state_change_with(&self, invalidate_caches: bool) {
        let flush = {
            let mut state = self.0.state.borrow_mut();
            state.derived_version += 1;
            if invalidate_caches {
                state.cache_version += 1;
            }
            if state.state_listeners.is_empty() {
                return;
            }
            state.state_change_pending = true;
            state.state_change_depth == 0 && !state.flushing
        };
        if flush {
            self.flush_state_change();
        }
    }

    fn flush_state_change(&self) {
        {
            let mut state = self.0.state.borrow_mut();
            if !state.state_change_pending || state.state_change_depth > 0 || state.flushing {
                return;
            }
            state.flushing = true;
        }
        let mut iterations = 0;
        loop {
            let listeners = {
                let mut state = self.0.state.borrow_mut();
                if !state.state_change_pending || state.state_change_depth != 0 {
                    break;
                }
                if iterations >= 1000 {
                    state.state_change_pending = false;
                    drop(state);
                    self.emit_error(
                        "state-change-feedback-loop",
                        "[Keymap] Possible infinite state listener feedback loop detected after 1000 iterations; pending state notifications were dropped",
                    );
                    break;
                }
                iterations += 1;
                state.state_change_pending = false;
                state.state_listeners.iter().map(|entry| entry.value.clone()).collect::<Vec<_>>()
            };
            for listener in listeners {
                listener();
            }
        }
        self.0.state.borrow_mut().flushing = false;
    }

    /// `on("state", fn)`: called after each batched change.
    pub fn on_state(&self, listener: Rc<dyn Fn()>) -> Dispose {
        let id = self.next_id();
        self.0.state.borrow_mut().state_listeners.push(Registered { id, value: listener });
        let keymap = self.clone();
        Box::new(move || keymap.0.state.borrow_mut().state_listeners.retain(|entry| entry.id != id))
    }

    /// `on("pendingSequence", fn)`.
    pub fn on_pending_sequence(&self, listener: Rc<dyn Fn(&[KeySequencePart])>) -> Dispose {
        let id = self.next_id();
        self.0.state.borrow_mut().pending_listeners.push(Registered { id, value: listener });
        let keymap = self.clone();
        Box::new(move || keymap.0.state.borrow_mut().pending_listeners.retain(|entry| entry.id != id))
    }

    /// `on("error", fn)`.
    pub fn on_error(&self, listener: DiagnosticListener) -> Dispose {
        let id = self.next_id();
        self.0.state.borrow_mut().error_listeners.push(Registered { id, value: listener });
        let keymap = self.clone();
        Box::new(move || keymap.0.state.borrow_mut().error_listeners.retain(|entry| entry.id != id))
    }

    /// `on("warning", fn)`.
    pub fn on_warning(&self, listener: DiagnosticListener) -> Dispose {
        let id = self.next_id();
        self.0.state.borrow_mut().warning_listeners.push(Registered { id, value: listener });
        let keymap = self.clone();
        Box::new(move || keymap.0.state.borrow_mut().warning_listeners.retain(|entry| entry.id != id))
    }

    // ----- registries

    /// `acquireResource(key, setup)`: one setup per key, disposed when the last holder releases it.
    pub fn acquire_resource(&self, key: &'static str, setup: impl FnOnce() -> Dispose) -> Dispose {
        let exists = {
            let mut state = self.0.state.borrow_mut();
            match state.resources.get_mut(key) {
                Some(entry) => {
                    entry.0 += 1;
                    true
                }
                None => false,
            }
        };
        if !exists {
            let dispose = setup();
            self.0.state.borrow_mut().resources.insert(key, (1, Some(dispose)));
        }
        let keymap = self.clone();
        Box::new(move || {
            let dispose = {
                let mut state = keymap.0.state.borrow_mut();
                let Some(entry) = state.resources.get_mut(key) else { return };
                entry.0 -= 1;
                if entry.0 > 0 {
                    return;
                }
                state.resources.remove(key).and_then(|(_, dispose)| dispose)
            };
            if let Some(dispose) = dispose {
                dispose();
            }
        })
    }

    fn register_fields(&self, kind: &'static str, fields: Vec<(&'static str, FieldCompiler)>) -> Dispose {
        let reserved: &[&str] = match kind {
            "layer" => &RESERVED_LAYER_FIELDS,
            "binding" => &RESERVED_BINDING_FIELDS,
            _ => &RESERVED_COMMAND_FIELDS,
        };
        let mut registered: Vec<(&'static str, FieldCompiler)> = Vec::new();
        for (name, compiler) in fields {
            if reserved.contains(&name) {
                self.emit_error(
                    &format!("reserved-{kind}-field"),
                    &format!("Keymap {kind} field \"{name}\" is reserved"),
                );
                continue;
            }
            let duplicate = {
                let mut state = self.0.state.borrow_mut();
                let map = match kind {
                    "layer" => &mut state.layer_fields,
                    "binding" => &mut state.binding_fields,
                    _ => &mut state.command_fields,
                };
                if map.contains_key(name) {
                    true
                } else {
                    map.insert(name.to_owned(), compiler.clone());
                    false
                }
            };
            if duplicate {
                self.emit_error(
                    &format!("duplicate-{kind}-field"),
                    &format!("Keymap {kind} field \"{name}\" is already registered"),
                );
                continue;
            }
            registered.push((name, compiler));
        }
        let keymap = self.clone();
        Box::new(move || {
            let mut state = keymap.0.state.borrow_mut();
            let map = match kind {
                "layer" => &mut state.layer_fields,
                "binding" => &mut state.binding_fields,
                _ => &mut state.command_fields,
            };
            for (name, compiler) in registered {
                if map.get(name).is_some_and(|current| Rc::ptr_eq(current, &compiler)) {
                    map.remove(name);
                }
            }
        })
    }

    /// `registerLayerFields`.
    pub fn register_layer_fields(&self, fields: Vec<(&'static str, FieldCompiler)>) -> Dispose {
        self.register_fields("layer", fields)
    }

    /// `registerBindingFields`.
    pub fn register_binding_fields(&self, fields: Vec<(&'static str, FieldCompiler)>) -> Dispose {
        self.register_fields("binding", fields)
    }

    /// `registerCommandFields`.
    pub fn register_command_fields(&self, fields: Vec<(&'static str, FieldCompiler)>) -> Dispose {
        self.register_fields("command", fields)
    }

    fn ordered<V: Clone + 'static>(
        &self,
        select: fn(&mut State<T>) -> &mut Vec<Registered<V>>,
        value: V,
        prepend: bool,
    ) -> Dispose {
        let id = self.next_id();
        {
            let mut state = self.0.state.borrow_mut();
            let list = select(&mut state);
            let entry = Registered { id, value };
            if prepend { list.insert(0, entry) } else { list.push(entry) }
        }
        let keymap = self.clone();
        Box::new(move || select(&mut keymap.0.state.borrow_mut()).retain(|entry| entry.id != id))
    }

    /// `appendBindingExpander`.
    pub fn append_binding_expander(&self, expander: BindingExpander) -> Dispose {
        self.ordered(|state| &mut state.expanders, expander, false)
    }

    /// `prependBindingExpander`.
    pub fn prepend_binding_expander(&self, expander: BindingExpander) -> Dispose {
        self.ordered(|state| &mut state.expanders, expander, true)
    }

    /// `appendBindingParser`.
    pub fn append_binding_parser(&self, parser: BindingParser) -> Dispose {
        self.ordered(|state| &mut state.parsers, parser, false)
    }

    /// `prependBindingParser`.
    pub fn prepend_binding_parser(&self, parser: BindingParser) -> Dispose {
        self.ordered(|state| &mut state.parsers, parser, true)
    }

    /// `appendEventMatchResolver`.
    pub fn append_event_match_resolver(&self, resolver: EventMatchResolver) -> Dispose {
        self.ordered(|state| &mut state.resolvers, resolver, false)
    }

    /// `prependEventMatchResolver`.
    pub fn prepend_event_match_resolver(&self, resolver: EventMatchResolver) -> Dispose {
        self.ordered(|state| &mut state.resolvers, resolver, true)
    }

    /// `intercept("key", fn, { priority, release })`: higher priorities first, then registration order.
    pub fn intercept_key(&self, listener: KeyIntercept, priority: i32, release: bool) -> Dispose {
        let id = self.next_id();
        {
            let mut state = self.0.state.borrow_mut();
            state.key_hooks.push(KeyHook { id, priority, release, order: id, listener });
            state.key_hooks.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.order.cmp(&b.order)));
        }
        let keymap = self.clone();
        Box::new(move || keymap.0.state.borrow_mut().key_hooks.retain(|hook| hook.id != id))
    }

    // ----- data

    /// `setData(name, value)`: `None` is `undefined` (removes it).
    pub fn set_data(&self, name: &str, value: Option<Data>) {
        self.batch(|| {
            {
                let mut state = self.0.state.borrow_mut();
                match &value {
                    None => {
                        if !state.data.contains_key(name) {
                            return;
                        }
                        Rc::make_mut(&mut state.data).remove(name);
                    }
                    Some(value) => {
                        if state.data.get(name) == Some(value) {
                            return;
                        }
                        Rc::make_mut(&mut state.data).insert(name.to_owned(), value.clone());
                    }
                }
            }
            self.ensure_valid_pending_sequence();
            self.queue_state_change();
        })
    }

    /// `getData(name)`.
    pub fn get_data(&self, name: &str) -> Option<Data> {
        self.0.state.borrow().data.get(name).cloned()
    }

    pub fn get_host_metadata(&self) -> HostMetadata {
        self.host().metadata()
    }

    // ----- conditions

    fn matches(&self, conditions: &Conditions) -> bool {
        if conditions.requires.is_empty() && conditions.matchers.is_empty() {
            return true;
        }
        let data = self.0.state.borrow().data.clone();
        conditions.requires.iter().all(|(name, value)| data.get(name) == Some(value))
            && conditions.matchers.iter().all(|matcher| matcher())
    }

    // ----- compiler

    fn parse_with_parsers(&self, key: &str) -> Result<ParserResult, String> {
        let input = units(key);
        if input.is_empty() {
            return Err("Invalid key sequence: sequence cannot be empty".to_owned());
        }
        let (parsers, tokens) = {
            let state = self.0.state.borrow();
            (state.parsers.iter().map(|entry| entry.value.clone()).collect::<Vec<_>>(), state.tokens.clone())
        };
        if parsers.is_empty() {
            return Err("No keymap binding parsers are registered".to_owned());
        }
        let mut out = ParserResult::default();
        let mut index = 0;
        while index < input.len() {
            let mut matched = false;
            for parser in &parsers {
                let Some(result) = parser(&ParserContext { input: &input, index, tokens: &tokens })? else { continue };
                if result.next_index <= index || result.next_index > input.len() {
                    return Err(format!("Keymap binding parser must advance the input for \"{key}\" at index {index}"));
                }
                out.parts.extend(result.parts);
                for token in result.used_tokens {
                    if !out.used_tokens.contains(&token) {
                        out.used_tokens.push(token);
                    }
                }
                for token in result.unknown_tokens {
                    if !out.unknown_tokens.contains(&token) {
                        out.unknown_tokens.push(token);
                    }
                }
                index = result.next_index;
                matched = true;
                break;
            }
            if !matched {
                return Err(format!("No keymap binding parser handled input at index {index} in \"{key}\""));
            }
        }
        out.next_index = input.len();
        Ok(out)
    }

    /// `parseTokenKey`: one stroke.
    fn parse_token_key(&self, key: &KeyLike) -> Result<KeySequencePart, String> {
        match key {
            KeyLike::Stroke(stroke) => create_key_sequence_part(stroke, None, None, None),
            KeyLike::Str(text) => {
                let parsed = self.parse_with_parsers(text)?;
                if parsed.parts.len() != 1 {
                    return Err(format!("Invalid key \"{text}\": expected a single key stroke"));
                }
                Ok(parsed.parts.into_iter().next().unwrap())
            }
        }
    }

    /// `parseKeySequence`: the parts of a binding string (none when it names an unknown token).
    pub fn parse_key_sequence(&self, key: &KeyLike) -> Result<Vec<KeySequencePart>, String> {
        match key {
            KeyLike::Stroke(stroke) => Ok(vec![create_key_sequence_part(stroke, None, None, None)?]),
            KeyLike::Str(text) => {
                let parsed = self.parse_with_parsers(text)?;
                for token in &parsed.unknown_tokens {
                    self.warn_unknown_token(token, text);
                }
                if !parsed.unknown_tokens.is_empty() {
                    return Ok(Vec::new());
                }
                Ok(parsed.parts)
            }
        }
    }

    /// `formatKey`.
    pub fn format_key(&self, key: &KeyLike, prefer_display: bool, separator: &str) -> Result<String, String> {
        Ok(stringify_key_sequence(&self.parse_key_sequence(key)?, prefer_display, separator))
    }

    /// `createKeyMatcher(key)`.
    pub fn create_key_matcher(&self, key: &KeyLike) -> Result<KeyMatcher, String> {
        Ok(KeyMatcher(self.parse_token_key(key)?.match_))
    }

    fn warn_unknown_token(&self, token: &str, sequence: &str) {
        self.warn_once(
            format!("token:{token}"),
            "unknown-token",
            &format!(
                "[Keymap] Unknown token \"{token}\" in key sequence \"{sequence}\"; binding was skipped until the token is registered"
            ),
        );
    }

    fn warn_unknown_field(&self, kind: &str, field: &str) {
        self.warn_once(
            format!("{kind}:{field}"),
            &format!("unknown-{kind}-field"),
            &format!("[Keymap] Unknown {kind} field \"{field}\" was ignored"),
        );
    }

    fn expand(&self, key: &str, layer: &Fields) -> Result<Vec<Expansion>, String> {
        let expanders: Vec<BindingExpander> =
            self.0.state.borrow().expanders.iter().map(|entry| entry.value.clone()).collect();
        let mut candidates = vec![Expansion { key: key.to_owned(), displays: None }];
        for expander in expanders {
            let mut next = Vec::new();
            for candidate in candidates {
                let result = expander(&ExpanderContext {
                    input: &candidate.key,
                    displays: candidate.displays.as_deref(),
                    layer,
                })?;
                match result {
                    None => next.push(candidate),
                    Some(expanded) if expanded.is_empty() => {
                        return Err(format!(
                            "Keymap binding expander must return at least one key sequence for \"{}\"",
                            candidate.key
                        ));
                    }
                    Some(expanded) => next.extend(expanded),
                }
            }
            candidates = next;
        }
        Ok(candidates)
    }

    fn run_field_compilers(
        &self,
        kind: &str,
        fields: &Fields,
        requires: &mut Vec<(String, Data)>,
        attrs: &mut Attrs,
        matchers: &mut Vec<Matcher>,
        warn: bool,
    ) -> Result<(), String> {
        for (name, value) in fields {
            let compiler = {
                let state = self.0.state.borrow();
                let map = match kind {
                    "layer" => &state.layer_fields,
                    "binding" => &state.binding_fields,
                    _ => &state.command_fields,
                };
                map.get(name).cloned()
            };
            let Some(compiler) = compiler else {
                if warn {
                    self.warn_unknown_field(kind, name);
                }
                continue;
            };
            let mut context = FieldContext {
                source: format!("field {name}"),
                requires: &mut *requires,
                attrs: Some(&mut *attrs),
                matchers: &mut *matchers,
            };
            compiler(value, &mut context)?;
        }
        Ok(())
    }

    /// `compileBindings`: each binding expanded, parsed, its fields compiled; `(bindings, hasTokenBindings)`.
    fn compile_bindings(&self, bindings: &[Binding<T>], layer: &Fields) -> (Vec<Rc<BindingState<T>>>, bool) {
        let mut states: Vec<Rc<BindingState<T>>> = Vec::new();
        let mut has_token_bindings = false;
        for binding in bindings {
            let keys: Vec<(KeyLike, Option<Vec<String>>)> = match &binding.key {
                KeyLike::Str(text) => match self.expand(text, layer) {
                    Ok(expanded) => expanded.into_iter().map(|e| (KeyLike::Str(e.key), e.displays)).collect(),
                    Err(error) => {
                        self.emit_error("binding-expand-error", &error);
                        continue;
                    }
                },
                stroke => vec![(stroke.clone(), None)],
            };
            for (key, displays) in keys {
                let parsed = match &key {
                    KeyLike::Str(text) => self.parse_with_parsers(text),
                    KeyLike::Stroke(stroke) => create_key_sequence_part(stroke, None, None, None)
                        .map(|part| ParserResult { parts: vec![part], ..ParserResult::default() }),
                };
                let parsed = parsed.and_then(|mut parsed| {
                    if let Some(displays) = &displays {
                        if displays.len() != parsed.parts.len() {
                            let name = match &key {
                                KeyLike::Str(text) => text.clone(),
                                KeyLike::Stroke(stroke) => stroke.name.clone(),
                            };
                            return Err(format!(
                                "Keymap binding expansion displays length must match parsed sequence length for \"{name}\""
                            ));
                        }
                        for (part, display) in parsed.parts.iter_mut().zip(displays) {
                            part.display = display.clone();
                        }
                    }
                    Ok(parsed)
                });
                let parsed = match parsed {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        self.emit_error("binding-parse-error", &error);
                        continue;
                    }
                };
                has_token_bindings |= !parsed.used_tokens.is_empty() || !parsed.unknown_tokens.is_empty();
                let source = match &key {
                    KeyLike::Str(text) => text.clone(),
                    KeyLike::Stroke(stroke) => stroke.name.clone(),
                };
                for token in &parsed.unknown_tokens {
                    self.warn_unknown_token(token, &source);
                }
                if !parsed.unknown_tokens.is_empty() {
                    continue;
                }
                match self.compile_binding(binding, parsed.parts, &states) {
                    Ok(Some(state)) => states.push(Rc::new(state)),
                    Ok(None) => {}
                    Err(error) => self.emit_error("binding-compile-error", &error),
                }
            }
        }
        (states, has_token_bindings)
    }

    fn compile_binding(
        &self,
        binding: &Binding<T>,
        sequence: Vec<KeySequencePart>,
        previous: &[Rc<BindingState<T>>],
    ) -> Result<Option<BindingState<T>>, String> {
        let release = match binding.event.as_deref() {
            None | Some("press") => false,
            Some("release") => true,
            Some(other) => {
                return Err(format!("Invalid keymap binding event \"{other}\": expected \"press\" or \"release\""));
            }
        };
        let mut requires = Vec::new();
        let mut attrs = Vec::new();
        let mut matchers = Vec::new();
        self.run_field_compilers("binding", &binding.fields, &mut requires, &mut attrs, &mut matchers, true)?;
        let command = match &binding.cmd {
            Some(BindingCommand::Name(name)) => {
                let trimmed = js_trim(name);
                if trimmed.is_empty() {
                    return Err("Invalid keymap command: command cannot be empty".to_owned());
                }
                Some(BindingCommand::Name(trimmed.to_owned()))
            }
            other => other.clone(),
        };
        if sequence.is_empty() {
            return Ok(None);
        }
        if release && sequence.len() > 1 {
            return Err("Keymap release bindings only support a single key stroke".to_owned());
        }
        let state = BindingState {
            sequence,
            command,
            release,
            attrs: (!attrs.is_empty()).then(|| Rc::new(attrs)),
            conditions: Conditions { requires, matchers },
            prevent_default: binding.prevent_default != Some(false),
            fallthrough: binding.fallthrough.unwrap_or(false),
            source: binding.clone(),
        };
        if !release {
            let prefix = |left: &[KeySequencePart], right: &[KeySequencePart]| {
                left.len() < right.len() && left.iter().zip(right).all(|(a, b)| a.match_ == b.match_)
            };
            for existing in previous {
                if existing.release {
                    continue;
                }
                if (existing.command.is_some() && prefix(&existing.sequence, &state.sequence))
                    || (state.command.is_some() && prefix(&state.sequence, &existing.sequence))
                {
                    return Err(
                        "Keymap bindings cannot use the same sequence as both an exact match and a prefix in the same layer"
                            .to_owned(),
                    );
                }
            }
        }
        Ok(Some(state))
    }

    // ----- layers

    fn normalize_commands(&self, commands: &[Command<T>]) -> Vec<Rc<CommandState<T>>> {
        let mut normalized: Vec<Rc<CommandState<T>>> = Vec::new();
        for command in commands {
            let result = (|| -> Result<Option<CommandState<T>>, String> {
                let trimmed = js_trim(&command.name);
                if trimmed.is_empty() {
                    return Err("Invalid keymap command name: name cannot be empty".to_owned());
                }
                if trimmed.chars().any(crate::js::is_js_space) {
                    return Err(format!(
                        "Invalid keymap command name \"{}\": command names cannot contain whitespace",
                        command.name
                    ));
                }
                if normalized.iter().any(|state| state.command.name == trimmed) {
                    self.emit_error(
                        "duplicate-command",
                        &format!("Duplicate keymap command \"{trimmed}\" in the same layer"),
                    );
                    return Ok(None);
                }
                let mut requires = Vec::new();
                let mut attrs = Vec::new();
                let mut matchers = Vec::new();
                let fields: Fields = command
                    .fields
                    .iter()
                    .filter(|(name, _)| !RESERVED_COMMAND_FIELDS.contains(&name.as_str()))
                    .cloned()
                    .collect();
                self.run_field_compilers("command", &fields, &mut requires, &mut attrs, &mut matchers, false)?;
                let mut exposed = command.clone();
                exposed.name = trimmed.to_owned();
                Ok(Some(CommandState {
                    command: Rc::new(exposed),
                    attrs: (!attrs.is_empty()).then(|| Rc::new(attrs)),
                    conditions: Conditions { requires, matchers },
                }))
            })();
            match result {
                Ok(Some(state)) => normalized.push(Rc::new(state)),
                Ok(None) => {}
                Err(error) => self.emit_error("register-command-failed", &error),
            }
        }
        normalized
    }

    fn compile_layer(&self, layer: &Layer<T>, slot_order: Option<u64>) -> Option<Rc<RegisteredLayer<T>>> {
        if let Some(target) = &layer.target
            && self.host().is_target_destroyed(target)
        {
            self.emit_error("destroyed-layer-target", "Cannot register a keymap layer for a destroyed keymap target");
            return None;
        }
        let target_mode = match (layer.target_mode, &layer.target) {
            (Some(mode), None) => {
                let name = if mode == TargetMode::Focus { "focus" } else { "focus-within" };
                self.emit_error("register-layer-failed", &format!("Keymap targetMode \"{name}\" requires a target"));
                return None;
            }
            (Some(mode), Some(_)) => Some(mode),
            (None, Some(_)) => Some(TargetMode::FocusWithin),
            (None, None) => None,
        };
        let commands = if layer.commands.is_empty() { Vec::new() } else { self.normalize_commands(&layer.commands) };
        let fields: Fields =
            layer.fields.iter().filter(|(name, _)| !RESERVED_LAYER_FIELDS.contains(&name.as_str())).cloned().collect();
        let mut requires = Vec::new();
        let mut attrs = Vec::new();
        let mut matchers = Vec::new();
        if let Err(error) = self.run_field_compilers("layer", &fields, &mut requires, &mut attrs, &mut matchers, true) {
            self.emit_error("register-layer-failed", &error);
            return None;
        }
        let order = slot_order.unwrap_or_else(|| {
            let mut state = self.0.state.borrow_mut();
            state.order += 1;
            state.order - 1
        });
        let (bindings, has_token_bindings) = self.compile_bindings(&layer.bindings, &fields);
        if bindings.is_empty() && !has_token_bindings && commands.is_empty() {
            return None;
        }
        Some(Rc::new(RegisteredLayer {
            order,
            target: layer.target.clone(),
            target_mode,
            priority: layer.priority.unwrap_or(0),
            conditions: Conditions { requires, matchers },
            fields,
            commands,
            source_bindings: layer.bindings.clone(),
            bindings: RefCell::new(bindings),
            has_token_bindings: Cell::new(has_token_bindings),
        }))
    }

    fn add_layer(&self, layer: Rc<RegisteredLayer<T>>) {
        {
            let mut state = self.0.state.borrow_mut();
            if blocks_command_view_cache(&layer) {
                state.command_view_blockers += 1;
            }
            state.layers.push(layer.clone());
            state.layers.sort_by_key(|candidate| candidate.order);
            state.sorted_layers.push(layer.clone());
            state.sorted_layers.sort_by(|a, b| b.priority.cmp(&a.priority).then(b.order.cmp(&a.order)));
        }
        if !layer.commands.is_empty() {
            self.ensure_valid_pending_sequence();
        }
        self.queue_state_change();
    }

    fn unregister(&self, layer: &Rc<RegisteredLayer<T>>) {
        self.batch(|| {
            let captured = {
                let mut state = self.0.state.borrow_mut();
                let before = state.layers.len();
                state.layers.retain(|candidate| !Rc::ptr_eq(candidate, layer));
                if state.layers.len() == before {
                    return;
                }
                state.sorted_layers.retain(|candidate| !Rc::ptr_eq(candidate, layer));
                if blocks_command_view_cache(layer) {
                    state.command_view_blockers -= 1;
                }
                state
                    .pending
                    .as_ref()
                    .is_some_and(|captures| captures.iter().any(|capture| Rc::ptr_eq(&capture.layer, layer)))
            };
            if captured {
                self.set_pending_sequence(None);
            } else if !layer.commands.is_empty() && !self.host().is_destroyed() {
                self.ensure_valid_pending_sequence();
            }
            self.queue_state_change();
        })
    }

    /// `registerLayer(layer)`: the returned function unregisters it.
    pub fn register_layer(&self, layer: Layer<T>) -> Dispose {
        self.batch(|| {
            let Some(registered) = self.compile_layer(&layer, None) else { return Box::new(|| {}) as Dispose };
            self.add_layer(registered.clone());
            let keymap = self.clone();
            Box::new(move || keymap.unregister(&registered)) as Dispose
        })
    }

    /// `createSlot` (`createLayerSlot`): a position for `replace_layer`, ordered at its creation.
    pub fn create_layer_slot(&self) -> LayerSlot<T> {
        let mut state = self.0.state.borrow_mut();
        state.order += 1;
        LayerSlot { order: state.order - 1, layer: RefCell::new(None) }
    }

    /// `replaceLayer(slot, layer)`: swaps the slot's layer in one batch. A layer whose compiled data equals the
    /// slot's (functions aside, which take the new ones) keeps its place, so a pending sequence through it stays.
    pub fn replace_layer(&self, slot: &LayerSlot<T>, layer: Option<Layer<T>>) {
        self.batch(|| {
            let kept = slot
                .layer
                .borrow()
                .clone()
                .filter(|kept| self.0.state.borrow().layers.iter().any(|candidate| Rc::ptr_eq(candidate, kept)));
            let next = layer.as_ref().and_then(|layer| self.compile_layer(layer, Some(slot.order)));
            if let (Some(kept), Some(next)) = (&kept, &next)
                && same_layer_data(kept, next)
            {
                self.swap_layer(kept, next.clone());
                *slot.layer.borrow_mut() = Some(next.clone());
                return;
            }
            if let Some(previous) = slot.layer.borrow_mut().take() {
                self.unregister(&previous);
            }
            *slot.layer.borrow_mut() = next.clone();
            if let Some(next) = next {
                self.add_layer(next);
            }
        })
    }

    /// The kept-layer refresh: `next` takes `kept`'s place and its pending captures.
    fn swap_layer(&self, kept: &Rc<RegisteredLayer<T>>, next: Rc<RegisteredLayer<T>>) {
        {
            let mut guard = self.0.state.borrow_mut();
            let state: &mut State<T> = &mut guard;
            for list in [&mut state.layers, &mut state.sorted_layers] {
                for candidate in list.iter_mut() {
                    if Rc::ptr_eq(candidate, kept) {
                        *candidate = next.clone();
                    }
                }
            }
            // TS keeps one object; here `next` replaces `kept` everywhere it is held, caches included.
            if let Some((_, _, layers)) = state.active_layers_cache.as_mut() {
                for candidate in layers.iter_mut() {
                    if Rc::ptr_eq(candidate, kept) {
                        *candidate = next.clone();
                    }
                }
            }
            if let Some((_, _, view)) = state.command_view_cache.as_mut() {
                let remap = |entries: &[(Rc<RegisteredLayer<T>>, Rc<CommandState<T>>)]| {
                    entries
                        .iter()
                        .map(|(layer, command)| {
                            if !Rc::ptr_eq(layer, kept) {
                                return (layer.clone(), command.clone());
                            }
                            let index = kept.commands.iter().position(|c| Rc::ptr_eq(c, command)).unwrap();
                            (next.clone(), next.commands[index].clone())
                        })
                        .collect::<Vec<_>>()
                };
                let layers = view
                    .layers
                    .iter()
                    .map(|layer| if Rc::ptr_eq(layer, kept) { next.clone() } else { layer.clone() })
                    .collect();
                *view = Rc::new(CommandView { layers, reachable: remap(&view.reachable), active: remap(&view.active) });
            }
            if let Some(captures) = state.pending.as_mut() {
                let old = kept.bindings.borrow();
                let new = next.bindings.borrow();
                for capture in captures.iter_mut() {
                    if !Rc::ptr_eq(&capture.layer, kept) {
                        continue;
                    }
                    if let Some(position) = old.iter().position(|binding| Rc::ptr_eq(binding, &capture.binding)) {
                        capture.binding = new[position].clone();
                    }
                    capture.layer = next.clone();
                }
            }
        }
        if !self.host().is_destroyed() {
            self.ensure_valid_pending_sequence();
        }
        let has_matchers = !next.conditions.matchers.is_empty()
            || next.commands.iter().any(|command| !command.conditions.matchers.is_empty())
            || next.bindings.borrow().iter().any(|binding| !binding.conditions.matchers.is_empty());
        if has_matchers {
            self.queue_state_change();
        }
    }

    /// The host's `onTargetDestroy`: unregisters the target's layers.
    pub fn handle_target_destroyed(&self, target: &T) {
        let layers: Vec<Rc<RegisteredLayer<T>>> = self
            .0
            .state
            .borrow()
            .layers
            .iter()
            .filter(|layer| layer.target.as_ref() == Some(target))
            .cloned()
            .collect();
        for layer in layers {
            self.unregister(&layer);
        }
    }

    /// `registerToken({ name, key })`: a `<name>` the default parser resolves.
    pub fn register_token(&self, name: &str, key: &KeyLike) -> Dispose {
        let normalized = match normalize_binding_token_name(name) {
            Ok(normalized) => normalized,
            Err(error) => {
                self.emit_error("token-name-normalize-error", &error);
                return Box::new(|| {});
            }
        };
        if self.0.state.borrow().tokens.contains_key(&normalized) {
            self.emit_error("duplicate-token", &format!("Keymap token \"{normalized}\" is already registered"));
            return Box::new(|| {});
        }
        let part = match self.parse_token_key(key) {
            Ok(part) => part,
            Err(error) => {
                self.emit_error("token-parse-error", &error);
                return Box::new(|| {});
            }
        };
        let token = ResolvedToken { stroke: part.stroke, match_: part.match_ };
        let mut next = self.0.state.borrow().tokens.clone();
        next.insert(normalized.clone(), token.clone());
        self.apply_token_state(next);
        let keymap = self.clone();
        Box::new(move || {
            let current = keymap.0.state.borrow().tokens.get(&normalized).cloned();
            if current.as_ref() != Some(&token) {
                return;
            }
            let mut next = keymap.0.state.borrow().tokens.clone();
            next.remove(&normalized);
            keymap.apply_token_state(next);
        })
    }

    fn apply_token_state(&self, tokens: HashMap<String, ResolvedToken>) {
        self.batch(|| {
            let layers: Vec<Rc<RegisteredLayer<T>>> =
                self.0.state.borrow().layers.iter().filter(|layer| layer.has_token_bindings.get()).cloned().collect();
            let previous = std::mem::replace(&mut self.0.state.borrow_mut().tokens, tokens.clone());
            // Compile against the new tokens (TS compiles before switching; parsers read `state.tokens` here).
            let compilations: Vec<_> =
                layers.iter().map(|layer| self.compile_bindings(&layer.source_bindings, &layer.fields)).collect();
            drop(previous);
            let mut clear = false;
            for (layer, (bindings, has_tokens)) in layers.iter().zip(compilations) {
                *layer.bindings.borrow_mut() = bindings;
                layer.has_token_bindings.set(has_tokens);
                clear |= self
                    .0
                    .state
                    .borrow()
                    .pending
                    .as_ref()
                    .is_some_and(|captures| captures.iter().any(|capture| Rc::ptr_eq(&capture.layer, layer)));
            }
            if clear {
                self.set_pending_sequence(None);
            }
            if !layers.is_empty() {
                self.queue_state_change();
            }
        })
    }

    // ----- activation

    fn focused(&self) -> Option<T> {
        if self.host().is_destroyed() { None } else { self.host().focused_target() }
    }

    fn activation_path(&self, focused: Option<&T>) -> Vec<T> {
        let mut path = Vec::new();
        let mut current = Some(focused.cloned().unwrap_or_else(|| self.host().root_target()));
        while let Some(target) = current {
            current = self.host().parent_target(&target);
            path.push(target);
        }
        path
    }

    fn layer_active(&self, layer: &RegisteredLayer<T>, focused: Option<&T>, path: &[T]) -> bool {
        let Some(target) = &layer.target else { return true };
        if self.host().is_target_destroyed(target) {
            return false;
        }
        if layer.target_mode == Some(TargetMode::Focus) {
            return focused == Some(target);
        }
        path.contains(target)
    }

    /// `getActiveLayers` (`activeLayersCache`).
    fn active_layers(&self, focused: Option<&T>) -> Vec<Rc<RegisteredLayer<T>>> {
        {
            let state = self.0.state.borrow();
            if let Some((version, cached_focused, layers)) = &state.active_layers_cache
                && *version == state.cache_version
                && cached_focused.as_ref() == focused
            {
                return layers.clone();
            }
        }
        let path = self.activation_path(focused);
        let sorted = self.0.state.borrow().sorted_layers.clone();
        let layers: Vec<Rc<RegisteredLayer<T>>> =
            sorted.into_iter().filter(|layer| self.layer_active(layer, focused, &path)).collect();
        let mut state = self.0.state.borrow_mut();
        state.active_layers_cache = Some((state.cache_version, focused.cloned(), layers.clone()));
        layers
    }

    /// `getActiveCommandView`: cached per derived version and focus while no layer with commands has matchers.
    fn command_view(&self, focused: Option<&T>) -> Rc<CommandView<T>> {
        {
            let state = self.0.state.borrow();
            if let Some((version, cached_focused, view)) = &state.command_view_cache
                && *version == state.derived_version
                && cached_focused.as_ref() == focused
            {
                return view.clone();
            }
        }
        let layers = self.active_layers(focused);
        let mut reachable: Vec<(Rc<RegisteredLayer<T>>, Rc<CommandState<T>>)> = Vec::new();
        let mut active = Vec::new();
        for layer in &layers {
            if layer.commands.is_empty() || !self.matches(&layer.conditions) {
                continue;
            }
            for command in &layer.commands {
                if !self.matches(&command.conditions) {
                    continue;
                }
                active.push((layer.clone(), command.clone()));
                if !reachable.iter().any(|(_, existing)| existing.command.name == command.command.name) {
                    reachable.push((layer.clone(), command.clone()));
                }
            }
        }
        let view = Rc::new(CommandView { layers, reachable, active });
        let mut state = self.0.state.borrow_mut();
        if state.command_view_blockers == 0 {
            state.command_view_cache = Some((state.derived_version, focused.cloned(), view.clone()));
        }
        view
    }

    fn binding_visible(&self, binding: &BindingState<T>, view: &CommandView<T>) -> bool {
        match &binding.command {
            None | Some(BindingCommand::Run(_)) => true,
            Some(BindingCommand::Name(name)) => view.reachable_by_name(name).is_some(),
        }
    }

    fn binding_live(&self, binding: &BindingState<T>, view: &CommandView<T>) -> bool {
        self.matches(&binding.conditions) && self.binding_visible(binding, view)
    }

    fn binding_command_attrs(&self, binding: &BindingState<T>, view: &CommandView<T>) -> Option<Rc<Attrs>> {
        let name = binding.command.as_ref()?.name()?;
        view.reachable_by_name(name).and_then(|(_, command)| command.attrs.clone())
    }

    fn set_pending_sequence(&self, next: Option<Vec<Capture<T>>>) {
        {
            let state = self.0.state.borrow();
            if same_pending(state.pending.as_deref(), next.as_deref()) {
                return;
            }
        }
        self.0.state.borrow_mut().pending = next;
        let listeners: Vec<Rc<dyn Fn(&[KeySequencePart])>> =
            self.0.state.borrow().pending_listeners.iter().map(|entry| entry.value.clone()).collect();
        if !listeners.is_empty() {
            let sequence = self.0.state.borrow().pending.as_deref().map(sequence_parts).unwrap_or_default();
            for listener in listeners {
                listener(&sequence);
            }
        }
        self.queue_state_change();
    }

    fn ensure_valid_pending_sequence(&self) -> Option<Vec<Capture<T>>> {
        let pending = self.0.state.borrow().pending.clone()?;
        let focused = self.focused();
        let view = self.command_view(focused.as_ref());
        let path = self.activation_path(focused.as_ref());
        let layers = self.0.state.borrow().layers.clone();
        let captures: Vec<Capture<T>> = pending
            .iter()
            .filter(|capture| {
                layers.iter().any(|layer| Rc::ptr_eq(layer, &capture.layer))
                    && self.layer_active(&capture.layer, focused.as_ref(), &path)
                    && self.matches(&capture.layer.conditions)
                    && self.binding_live(&capture.binding, &view)
                    && capture.has_continuations()
            })
            .cloned()
            .collect();
        if captures.is_empty() {
            self.set_pending_sequence(None);
            return None;
        }
        if captures.len() != pending.len() {
            self.set_pending_sequence(Some(captures));
        }
        self.0.state.borrow().pending.clone()
    }

    /// `hasPendingSequence()`.
    pub fn has_pending_sequence(&self) -> bool {
        !self.host().is_destroyed() && self.ensure_valid_pending_sequence().is_some()
    }

    /// `getPendingSequence()`.
    pub fn get_pending_sequence(&self) -> Vec<KeySequencePart> {
        if self.host().is_destroyed() {
            return Vec::new();
        }
        self.ensure_valid_pending_sequence().as_deref().map(sequence_parts).unwrap_or_default()
    }

    /// `clearPendingSequence()`.
    pub fn clear_pending_sequence(&self) {
        self.set_pending_sequence(None);
    }

    /// `popPendingSequence()`: drops the last key of the pending sequence.
    pub fn pop_pending_sequence(&self) -> bool {
        let Some(pending) = self.ensure_valid_pending_sequence() else { return false };
        if pending.first().is_none_or(|capture| capture.parts.len() <= 1) {
            self.set_pending_sequence(None);
            return true;
        }
        let next: Vec<Capture<T>> = pending
            .iter()
            .filter(|capture| capture.parts.len() > 1)
            .map(|capture| Capture {
                layer: capture.layer.clone(),
                binding: capture.binding.clone(),
                index: capture.index - 1,
                parts: capture.parts[..capture.parts.len() - 1].to_vec(),
            })
            .collect();
        self.set_pending_sequence((!next.is_empty()).then_some(next));
        true
    }

    // ----- active keys

    fn select_active_bindings(
        &self,
        bindings: &[Rc<BindingState<T>>],
        view: &CommandView<T>,
    ) -> Option<(Vec<Rc<BindingState<T>>>, Option<Rc<BindingState<T>>>, bool)> {
        let mut selected = Vec::new();
        let mut command_binding: Option<Rc<BindingState<T>>> = None;
        for binding in bindings {
            if !self.binding_live(binding, view) {
                continue;
            }
            selected.push(binding.clone());
            if binding.command.is_none() {
                continue;
            }
            command_binding.get_or_insert_with(|| binding.clone());
            if !binding.fallthrough {
                return Some((selected, command_binding, true));
            }
        }
        if selected.is_empty() {
            return None;
        }
        Some((selected, command_binding, false))
    }

    /// `getActiveKeys({ includeBindings, includeMetadata })`: the keys that do something now, from the root or
    /// after the pending sequence.
    pub fn get_active_keys(&self, include_bindings: bool, include_metadata: bool) -> Vec<ActiveKey<T>> {
        if self.host().is_destroyed() {
            return Vec::new();
        }
        let focused = self.focused();
        let view = self.command_view(focused.as_ref());
        let pending = self.ensure_valid_pending_sequence();
        let mut states: Vec<ActiveKeyState<T>> = Vec::new();
        let mut stopped: HashSet<KeyMatch> = HashSet::new();
        match pending {
            Some(captures) => self.collect_pending_active_keys(&captures, &view, &mut states, &mut stopped),
            None => {
                for layer in self.active_layers(focused.as_ref()) {
                    let root = build_tree(&layer.bindings.borrow());
                    if root.children.is_empty() || !self.matches(&layer.conditions) {
                        continue;
                    }
                    for node in &root.children {
                        if stopped.contains(&node.match_) {
                            continue;
                        }
                        let Some(selection) = self.select_active_key_node(node, &view) else { continue };
                        merge_active_key(&mut states, &node.stroke, &node.match_, &selection);
                        if selection.stop {
                            stopped.insert(node.match_.clone());
                        }
                    }
                }
            }
        }
        states
            .into_iter()
            .filter(|state| state.command_binding.is_some() || state.continues)
            .map(|state| self.materialize_active_key(state, include_bindings, include_metadata, &view))
            .collect()
    }

    fn select_active_key_node(&self, node: &Node<T>, view: &CommandView<T>) -> Option<Selection<T>> {
        let continues = !node.children.is_empty();
        if !continues {
            let (bindings, command_binding, stop) = self.select_active_bindings(&node.bindings, view)?;
            let (display, token_name) = part_presentation(&bindings, node.depth - 1);
            return Some(Selection {
                display,
                token_name,
                continues: false,
                first_binding: bindings.first().cloned(),
                command_binding,
                bindings,
                stop,
            });
        }
        let reachable: Vec<Rc<BindingState<T>>> =
            node.reachable.iter().filter(|binding| self.binding_live(binding, view)).cloned().collect();
        if reachable.is_empty() {
            return None;
        }
        let selected = self.select_active_bindings(&node.bindings, view);
        let (display, token_name) = part_presentation(&reachable, node.depth - 1);
        let (bindings, command_binding) = match selected {
            Some((bindings, command_binding, _)) => (bindings, command_binding),
            None => (Vec::new(), None),
        };
        Some(Selection {
            display,
            token_name,
            continues: true,
            first_binding: bindings.first().cloned(),
            command_binding,
            bindings,
            stop: true,
        })
    }

    fn collect_pending_active_keys(
        &self,
        captures: &[Capture<T>],
        view: &CommandView<T>,
        states: &mut Vec<ActiveKeyState<T>>,
        stopped: &mut HashSet<KeyMatch>,
    ) {
        // activeOptionsForCaptures
        let options: Vec<(KeySequencePart, Rc<BindingState<T>>, bool, bool)> = captures
            .iter()
            .filter_map(|capture| {
                let index = capture.index + 1;
                let part = capture.binding.sequence.get(index)?.clone();
                let length = capture.binding.sequence.len();
                Some((part, capture.binding.clone(), index == length - 1, index < length - 1))
            })
            .collect();
        let mut seen: HashSet<KeyMatch> = HashSet::new();
        for (part, _, _, _) in &options {
            if !seen.insert(part.match_.clone()) || stopped.contains(&part.match_) {
                continue;
            }
            let matching: Vec<&(KeySequencePart, Rc<BindingState<T>>, bool, bool)> =
                options.iter().filter(|candidate| candidate.0.match_ == part.match_).collect();
            let exact: Vec<Rc<BindingState<T>>> =
                matching.iter().filter(|candidate| candidate.2).map(|candidate| candidate.1.clone()).collect();
            let selected = self.select_active_bindings(&exact, view);
            let continues = matching.iter().any(|candidate| candidate.3);
            if !continues && selected.is_none() {
                continue;
            }
            let parts: Vec<&KeySequencePart> = matching.iter().map(|candidate| &candidate.0).collect();
            let (display, token_name) = merged_presentation(&parts);
            let selected_stop = selected.as_ref().is_some_and(|(_, _, stop)| *stop);
            let (bindings, command_binding) = match selected {
                Some((bindings, command_binding, _)) => (bindings, command_binding),
                None => (Vec::new(), None),
            };
            let selection = Selection {
                display,
                token_name,
                continues,
                first_binding: bindings.first().cloned(),
                command_binding,
                bindings,
                stop: continues || selected_stop,
            };
            merge_active_key(states, &part.stroke, &part.match_, &selection);
            if selection.stop {
                stopped.insert(part.match_.clone());
            }
        }
    }

    fn materialize_active_key(
        &self,
        state: ActiveKeyState<T>,
        include_bindings: bool,
        include_metadata: bool,
        view: &CommandView<T>,
    ) -> ActiveKey<T> {
        let bindings = (include_bindings && !state.bindings.is_empty())
            .then(|| state.bindings.iter().map(|binding| self.active_binding(binding, view)).collect());
        ActiveKey {
            stroke: state.stroke,
            display: state.display,
            token_name: state.token_name.filter(|name| !name.is_empty()),
            continues: state.continues,
            command: state.command_binding.as_ref().and_then(|binding| binding.command.clone()),
            binding_attrs: if include_metadata {
                state.first_binding.as_ref().and_then(|b| b.attrs.clone())
            } else {
                None
            },
            command_attrs: if include_metadata {
                state.command_binding.as_ref().and_then(|binding| self.binding_command_attrs(binding, view))
            } else {
                None
            },
            bindings,
        }
    }

    fn active_binding(&self, binding: &BindingState<T>, view: &CommandView<T>) -> ActiveBinding<T> {
        ActiveBinding {
            sequence: binding.sequence.clone(),
            command: binding.command.clone(),
            command_attrs: self.binding_command_attrs(binding, view),
            attrs: binding.attrs.clone(),
            release: binding.release,
            prevent_default: binding.prevent_default,
            fallthrough: binding.fallthrough,
        }
    }

    // ----- commands

    /// `getCommands({ visibility, focused })`.
    pub fn get_commands(&self, visibility: Visibility, focused: Option<Option<T>>) -> Vec<Rc<Command<T>>> {
        self.command_entries_for(visibility, focused).into_iter().map(|(_, command)| command.command.clone()).collect()
    }

    fn command_entries_for(
        &self,
        visibility: Visibility,
        focused: Option<Option<T>>,
    ) -> Vec<(Rc<RegisteredLayer<T>>, Rc<CommandState<T>>)> {
        if self.host().is_destroyed() && visibility != Visibility::Registered {
            return Vec::new();
        }
        let focused = focused.unwrap_or_else(|| self.focused());
        match visibility {
            Visibility::Registered => {
                let layers = self.0.state.borrow().layers.clone();
                layers
                    .iter()
                    .flat_map(|layer| layer.commands.iter().map(move |command| (layer.clone(), command.clone())))
                    .collect()
            }
            Visibility::Active => self.command_view(focused.as_ref()).active.clone(),
            Visibility::Reachable => self.command_view(focused.as_ref()).reachable.clone(),
        }
    }

    /// `getCommandEntries({ visibility, focused })`: each command with the bindings that reach it.
    pub fn get_command_entries(&self, visibility: Visibility, focused: Option<Option<T>>) -> Vec<CommandEntry<T>> {
        let entries = self.command_entries_for(visibility, focused.clone());
        if entries.is_empty() {
            return Vec::new();
        }
        let mut grouped: Vec<CommandEntry<T>> = entries
            .iter()
            .map(|(_, command)| CommandEntry { command: command.command.clone(), bindings: Vec::new() })
            .collect();
        let attrs: Vec<Option<Rc<Attrs>>> = entries.iter().map(|(_, command)| command.attrs.clone()).collect();
        self.visit_query_bindings(visibility, focused, |binding, _| {
            let Some(name) = binding.command.as_ref().and_then(BindingCommand::name) else { return };
            for (index, entry) in grouped.iter_mut().enumerate() {
                if entry.command.name == name {
                    entry.bindings.push(ActiveBinding {
                        sequence: binding.sequence.clone(),
                        command: binding.command.clone(),
                        command_attrs: attrs[index].clone(),
                        attrs: binding.attrs.clone(),
                        release: binding.release,
                        prevent_default: binding.prevent_default,
                        fallthrough: binding.fallthrough,
                    });
                }
            }
        });
        grouped
    }

    fn visit_query_bindings(
        &self,
        visibility: Visibility,
        focused: Option<Option<T>>,
        mut visit: impl FnMut(&BindingState<T>, Option<&CommandView<T>>),
    ) {
        if visibility == Visibility::Registered {
            let layers = self.0.state.borrow().layers.clone();
            for layer in &layers {
                for binding in layer.bindings.borrow().iter() {
                    visit(binding, None);
                }
            }
            return;
        }
        let focused = focused.unwrap_or_else(|| self.focused());
        let view = self.command_view(focused.as_ref());
        for layer in &view.layers {
            let bindings = layer.bindings.borrow().clone();
            if bindings.is_empty() || !self.matches(&layer.conditions) {
                continue;
            }
            for binding in &bindings {
                if self.binding_live(binding, &view) {
                    visit(binding, Some(&view));
                }
            }
        }
    }

    /// `getCommandBindings({ visibility, commands, focused })`.
    pub fn get_command_bindings(
        &self,
        visibility: Visibility,
        commands: &[String],
        focused: Option<Option<T>>,
    ) -> Vec<(String, Vec<ActiveBinding<T>>)> {
        let mut out: Vec<(String, Vec<ActiveBinding<T>>)> = Vec::new();
        for command in commands {
            if !out.iter().any(|(name, _)| name == command) {
                out.push((command.clone(), Vec::new()));
            }
        }
        if out.is_empty() {
            return out;
        }
        let registered_attrs = |name: &str| -> Option<Rc<Attrs>> {
            let layers = self.0.state.borrow().layers.clone();
            layers
                .iter()
                .flat_map(|layer| layer.commands.iter())
                .find(|command| command.command.name == name)?
                .attrs
                .clone()
        };
        self.visit_query_bindings(visibility, focused, |binding, view| {
            let Some(name) = binding.command.as_ref().and_then(BindingCommand::name) else { return };
            let Some((_, bindings)) = out.iter_mut().find(|(command, _)| command == name) else { return };
            let command_attrs = match view {
                None => registered_attrs(name),
                Some(view) => self.binding_command_attrs(binding, view),
            };
            bindings.push(ActiveBinding {
                sequence: binding.sequence.clone(),
                command: binding.command.clone(),
                command_attrs,
                attrs: binding.attrs.clone(),
                release: binding.release,
                prevent_default: binding.prevent_default,
                fallthrough: binding.fallthrough,
            });
        });
        out
    }

    fn execute(
        &self,
        run: &CommandFn<T>,
        event: &dyn KeymapEvent,
        focused: Option<T>,
        target: Option<T>,
        input: &str,
        command: Option<Rc<Command<T>>>,
        include_command: bool,
    ) -> (bool, RunCommandResult<T>) {
        let data = self.0.state.borrow().data.clone();
        let result_command = if include_command { command.clone() } else { None };
        let context = CommandContext { keymap: self, event, focused, target, data, input: input.to_owned(), command };
        match run(&context) {
            CommandResult::Rejected => {
                (false, RunCommandResult { ok: false, reason: Some(RunReason::Rejected), command: result_command })
            }
            CommandResult::Handled => (true, RunCommandResult { ok: true, reason: None, command: result_command }),
            CommandResult::Result(mut result) => {
                if include_command
                    && result.command.is_none()
                    && (result.ok || result.reason != Some(RunReason::NotFound))
                {
                    result.command = result_command;
                }
                (result.ok, result)
            }
        }
    }

    fn run_command_entry(
        &self,
        command: &Rc<Command<T>>,
        event: &dyn KeymapEvent,
        focused: Option<T>,
        target: Option<T>,
        input: &str,
        include_command: bool,
    ) -> (bool, RunCommandResult<T>) {
        match &command.run {
            Some(run) => self.execute(run, event, focused, target, input, Some(command.clone()), include_command),
            // A command without `run`: calling it throws, an error result.
            None => (
                true,
                RunCommandResult {
                    ok: false,
                    reason: Some(RunReason::Error),
                    command: include_command.then(|| command.clone()),
                },
            ),
        }
    }

    fn programmatic(
        &self,
        cmd: &str,
        registered: bool,
        focused: Option<Option<T>>,
        include_command: bool,
    ) -> RunCommandResult<T> {
        let normalized = js_trim(cmd).to_owned();
        if normalized.is_empty() {
            return RunCommandResult::fail(RunReason::InvalidArgs);
        }
        let focused = focused.unwrap_or_else(|| self.focused());
        let event = self.host().create_command_event();
        let chain: Vec<(Rc<RegisteredLayer<T>>, Rc<CommandState<T>>)> = if registered {
            let sorted = self.0.state.borrow().sorted_layers.clone();
            sorted
                .iter()
                .flat_map(|layer| {
                    layer
                        .commands
                        .iter()
                        .filter(|command| command.command.name == normalized)
                        .map(move |command| (layer.clone(), command.clone()))
                })
                .collect()
        } else {
            self.command_view(focused.as_ref()).chain(&normalized)
        };
        let mut rejected: Option<RunCommandResult<T>> = None;
        for (layer, command) in chain {
            let (handled, result) = self.run_command_entry(
                &command.command,
                event.as_ref(),
                focused.clone(),
                layer.target.clone(),
                &normalized,
                include_command,
            );
            if handled {
                return result;
            }
            rejected = Some(result);
        }
        if !registered {
            let path = self.activation_path(focused.as_ref());
            let layers = self.0.state.borrow().layers.clone();
            let mut inactive: Option<Rc<Command<T>>> = None;
            let mut disabled: Option<Rc<Command<T>>> = None;
            for layer in &layers {
                for command in layer.commands.iter().filter(|command| command.command.name == normalized) {
                    if !self.layer_active(layer, focused.as_ref(), &path) {
                        inactive.get_or_insert_with(|| command.command.clone());
                        continue;
                    }
                    if !self.matches(&layer.conditions) || !self.matches(&command.conditions) {
                        disabled.get_or_insert_with(|| command.command.clone());
                    }
                }
            }
            if let Some(command) = disabled.clone().or(inactive.clone()) {
                let reason = if disabled.is_some() { RunReason::Disabled } else { RunReason::Inactive };
                return RunCommandResult {
                    ok: false,
                    reason: Some(reason),
                    command: include_command.then_some(command),
                };
            }
        }
        rejected.unwrap_or_else(|| RunCommandResult::fail(RunReason::NotFound))
    }

    /// `runCommand(cmd)`: any registered command of that name, active or not, by precedence.
    pub fn run_command(&self, cmd: &str, focused: Option<Option<T>>, include_command: bool) -> RunCommandResult<T> {
        self.programmatic(cmd, true, focused, include_command)
    }

    /// `dispatchCommand(cmd)`: the active, enabled commands of that name, by precedence.
    pub fn dispatch_command(
        &self,
        cmd: &str,
        focused: Option<Option<T>>,
        include_command: bool,
    ) -> RunCommandResult<T> {
        self.programmatic(cmd, false, focused, include_command)
    }

    /// `runBinding`: an inline handler, or the active commands of the binding's command name.
    fn run_binding(
        &self,
        layer: &RegisteredLayer<T>,
        binding: &BindingState<T>,
        event: &dyn KeymapEvent,
        focused: Option<T>,
    ) -> bool {
        let handled = match &binding.command {
            Some(BindingCommand::Run(run)) => {
                self.execute(run, event, focused, layer.target.clone(), "<function>", None, false).0
            }
            Some(BindingCommand::Name(name)) => {
                let chain = self.command_view(focused.as_ref()).chain(name);
                let mut handled = false;
                for (command_layer, command) in chain {
                    let target = command_layer.target.clone().or_else(|| layer.target.clone());
                    if self.run_command_entry(&command.command, event, focused.clone(), target, name, false).0 {
                        handled = true;
                        break;
                    }
                }
                handled
            }
            None => false,
        };
        if handled && binding.prevent_default {
            event.prevent_default();
            event.stop_propagation();
        }
        handled
    }

    // ----- dispatch

    /// The host's key listener (`onKeyPress`/`onKeyRelease`, prepended to the renderer's): intercepts, then the
    /// layers.
    pub fn handle_key_event(&self, event: &dyn KeymapEvent, release: bool) {
        if self.host().is_destroyed() || self.0.state.borrow().cleaned_up {
            return;
        }
        let hooks: Vec<(bool, KeyIntercept)> =
            self.0.state.borrow().key_hooks.iter().map(|hook| (hook.release, hook.listener.clone())).collect();
        let setter = self.clone();
        let getter = self.clone();
        let context = KeyInputContext {
            event,
            data: Rc::new(move |name, value| setter.set_data(name, value)),
            get: Rc::new(move |name| getter.get_data(name)),
        };
        for (hook_release, hook) in hooks {
            if hook_release != release {
                continue;
            }
            hook(&context);
            if event.propagation_stopped() {
                return;
            }
        }
        if release {
            self.dispatch_release(event);
        } else {
            self.dispatch_press(event);
        }
    }

    fn resolve_event_match_keys(&self, event: &dyn KeymapEvent) -> Vec<KeyMatch> {
        let resolvers: Vec<EventMatchResolver> =
            self.0.state.borrow().resolvers.iter().map(|entry| entry.value.clone()).collect();
        let resolve_key = |input: &KeyStrokeInput| -> Result<KeyMatch, String> {
            Ok(create_key_sequence_part(input, None, None, None)?.match_)
        };
        let mut keys: Vec<KeyMatch> = Vec::new();
        for resolver in resolvers {
            match resolver(event, &resolve_key) {
                Ok(resolved) => {
                    for key in resolved {
                        if !keys.contains(&key) {
                            keys.push(key);
                        }
                    }
                }
                Err(_) => self.emit_error("event-match-resolver-error", "[Keymap] Error in event match resolver:"),
            }
        }
        keys
    }

    fn dispatch_release(&self, event: &dyn KeymapEvent) {
        let focused = self.focused();
        let layers = self.active_layers(focused.as_ref());
        for key in self.resolve_event_match_keys(event) {
            for layer in &layers {
                let bindings = layer.bindings.borrow().clone();
                if bindings.is_empty() || !self.matches(&layer.conditions) {
                    continue;
                }
                for binding in &bindings {
                    if !binding.release || binding.sequence.first().is_none_or(|part| part.match_ != key) {
                        continue;
                    }
                    if !self.matches(&binding.conditions) {
                        continue;
                    }
                    if !self.run_binding(layer, binding, event, focused.clone()) {
                        continue;
                    }
                    if !binding.fallthrough {
                        return;
                    }
                }
            }
        }
    }

    fn dispatch_press(&self, event: &dyn KeymapEvent) {
        let focused = self.focused();
        let pending = self.ensure_valid_pending_sequence();
        let keys = self.resolve_event_match_keys(event);
        if let Some(pending) = pending {
            self.dispatch_pending(&pending, &keys, event, focused);
            return;
        }
        let layers = self.active_layers(focused.as_ref());
        for key in &keys {
            if self.dispatch_from_root(&layers, 0, std::slice::from_ref(key), event, focused.clone()).0 {
                return;
            }
        }
    }

    /// `collectRootSequenceCaptures` with reachability: every press binding whose first part matches, if one
    /// of them is reachable.
    fn root_captures(
        &self,
        layer: &Rc<RegisteredLayer<T>>,
        keys: &[KeyMatch],
        view: &CommandView<T>,
    ) -> Vec<Capture<T>> {
        let mut captures: Vec<Capture<T>> = Vec::new();
        let mut best = usize::MAX;
        for binding in layer.bindings.borrow().iter() {
            if binding.release {
                continue;
            }
            let Some(part) = binding.sequence.first() else { continue };
            let Some(priority) = keys.iter().position(|key| *key == part.match_) else { continue };
            if priority < best {
                best = priority;
                captures.clear();
            }
            if priority == best {
                captures.push(Capture {
                    layer: layer.clone(),
                    binding: binding.clone(),
                    index: 0,
                    parts: vec![part.clone()],
                });
            }
        }
        if captures.iter().any(|capture| self.binding_live(&capture.binding, view)) { captures } else { Vec::new() }
    }

    fn hold_sequence(&self, captures: Vec<Capture<T>>, event: &dyn KeymapEvent) -> (bool, Reason) {
        self.set_pending_sequence(Some(captures));
        event.prevent_default();
        event.stop_propagation();
        (true, Reason::Handled)
    }

    fn run_captures(
        &self,
        layer: &RegisteredLayer<T>,
        captures: &[Capture<T>],
        event: &dyn KeymapEvent,
        focused: Option<T>,
    ) -> RunResult {
        let mut handled = false;
        for capture in captures {
            if !self.matches(&capture.binding.conditions) {
                continue;
            }
            if !self.run_binding(layer, &capture.binding, event, focused.clone()) {
                continue;
            }
            handled = true;
            if !capture.binding.fallthrough {
                return RunResult { handled: true, stop: true };
            }
        }
        RunResult { handled, stop: false }
    }

    /// `dispatchFromRootAtIndex`: `(handled, reason)`.
    fn dispatch_from_root(
        &self,
        layers: &[Rc<RegisteredLayer<T>>],
        start: usize,
        keys: &[KeyMatch],
        event: &dyn KeymapEvent,
        focused: Option<T>,
    ) -> (bool, Reason) {
        let mut outcome = (false, Reason::NoMatch);
        let view = self.command_view(focused.as_ref());
        for index in start..layers.len() {
            let layer = &layers[index];
            if !self.matches(&layer.conditions) {
                continue;
            }
            let captures = self.root_captures(layer, keys, &view);
            if captures.is_empty() {
                continue;
            }
            if captures.iter().any(Capture::has_continuations) {
                let mut continuation = Vec::new();
                let view = self.command_view(focused.as_ref());
                for later in &layers[index..] {
                    if !self.matches(&later.conditions) {
                        continue;
                    }
                    continuation
                        .extend(self.root_captures(later, keys, &view).into_iter().filter(Capture::has_continuations));
                }
                return self.hold_sequence(continuation, event);
            }
            let exact: Vec<Capture<T>> = captures.into_iter().filter(Capture::is_exact).collect();
            let result = self.run_captures(layer, &exact, event, focused.clone());
            if result.handled || outcome.1 == Reason::NoMatch {
                outcome = (result.handled, if result.handled { Reason::Handled } else { Reason::NoMatch });
            }
            if !result.handled {
                continue;
            }
            if result.stop {
                return outcome;
            }
        }
        outcome
    }

    fn dispatch_pending(&self, pending: &[Capture<T>], keys: &[KeyMatch], event: &dyn KeymapEvent, focused: Option<T>) {
        let view = self.command_view(focused.as_ref());
        let advanced: Vec<Capture<T>> = pending
            .iter()
            .filter_map(|capture| {
                let part = capture.binding.sequence.get(capture.index + 1)?;
                if !keys.contains(&part.match_) {
                    return None;
                }
                let mut parts = capture.parts.clone();
                parts.push(part.clone());
                Some(Capture {
                    layer: capture.layer.clone(),
                    binding: capture.binding.clone(),
                    index: capture.index + 1,
                    parts,
                })
            })
            .collect();
        let priority = |capture: &Capture<T>| {
            let last = &capture.parts[capture.parts.len() - 1];
            keys.iter().position(|key| *key == last.match_).unwrap_or(keys.len())
        };
        let best = advanced.iter().map(priority).min().unwrap_or(usize::MAX);
        let prioritized: Vec<Capture<T>> =
            advanced.iter().filter(|capture| priority(capture) == best).cloned().collect();
        if prioritized.is_empty() || !prioritized.iter().any(|capture| self.binding_live(&capture.binding, &view)) {
            self.set_pending_sequence(None);
            return;
        }
        let same_parts = |a: &[KeySequencePart], b: &[KeySequencePart]| {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.match_ == y.match_)
        };
        let mut handled_exact = false;
        let mut processed: Vec<usize> = Vec::new();
        for index in 0..prioritized.len() {
            if processed.contains(&index) {
                continue;
            }
            let capture = &prioritized[index];
            let continuation: Vec<Capture<T>> = prioritized
                .iter()
                .enumerate()
                .filter(|(other, candidate)| {
                    *other >= index && candidate.has_continuations() && same_parts(&candidate.parts, &capture.parts)
                })
                .map(|(_, candidate)| candidate.clone())
                .collect();
            if !continuation.is_empty() {
                if handled_exact {
                    continue;
                }
                self.hold_sequence(continuation, event);
                return;
            }
            if !capture.is_exact() {
                continue;
            }
            let exact: Vec<(usize, Capture<T>)> = prioritized
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    Rc::ptr_eq(&candidate.layer, &capture.layer)
                        && candidate.is_exact()
                        && same_parts(&candidate.parts, &capture.parts)
                })
                .map(|(other, candidate)| (other, candidate.clone()))
                .collect();
            processed.extend(exact.iter().map(|(other, _)| *other));
            let exact: Vec<Capture<T>> = exact.into_iter().map(|(_, candidate)| candidate).collect();
            let result = self.run_captures(&capture.layer, &exact, event, focused.clone());
            if !result.handled {
                continue;
            }
            handled_exact = true;
            if result.stop {
                self.set_pending_sequence(None);
                return;
            }
        }
        self.set_pending_sequence(None);
    }

    /// The host's focus listener: a focus change ends a pending sequence.
    pub fn handle_focus_change(&self) {
        self.batch(|| {
            self.set_pending_sequence(None);
            self.queue_state_change_with(false);
        });
    }

    /// The host's destroy listener (`#cleanup`).
    pub fn handle_host_destroyed(&self) {
        if std::mem::replace(&mut self.0.state.borrow_mut().cleaned_up, true) {
            return;
        }
        self.set_pending_sequence(None);
        let resources: Vec<Dispose> =
            self.0.state.borrow_mut().resources.drain().filter_map(|(_, (_, dispose))| dispose).collect();
        for dispose in resources {
            dispose();
        }
    }
}

fn blocks_command_view_cache<T>(layer: &RegisteredLayer<T>) -> bool {
    !layer.commands.is_empty()
        && (!layer.conditions.matchers.is_empty()
            || layer.commands.iter().any(|command| !command.conditions.matchers.is_empty()))
}

fn same_pending<T>(current: Option<&[Capture<T>]>, next: Option<&[Capture<T>]>) -> bool {
    match (current, next) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.len() == b.len()
                && a.iter().zip(b).all(|(left, right)| {
                    Rc::ptr_eq(&left.layer, &right.layer)
                        && Rc::ptr_eq(&left.binding, &right.binding)
                        && left.index == right.index
                        && left.parts.len() == right.parts.len()
                        && left.parts.iter().zip(&right.parts).all(|(x, y)| x.match_ == y.match_)
                })
        }
        _ => false,
    }
}

/// `collectSequencePartsFromPending`.
fn sequence_parts<T>(captures: &[Capture<T>]) -> Vec<KeySequencePart> {
    let Some(first) = captures.first() else { return Vec::new() };
    (0..first.parts.len())
        .map(|index| {
            let parts: Vec<&KeySequencePart> = captures.iter().filter_map(|capture| capture.parts.get(index)).collect();
            let (display, token_name) = merged_presentation(&parts);
            let part = &first.parts[index];
            KeySequencePart { stroke: part.stroke.clone(), display, match_: part.match_.clone(), token_name }
        })
        .collect()
}

/// The display of parts merged from several bindings: a shared display and token, or the canonical stroke and
/// none.
fn merged_presentation(parts: &[&KeySequencePart]) -> (String, Option<String>) {
    let Some(first) = parts.first() else { return (String::new(), None) };
    let display_conflict = parts.iter().any(|part| part.display != first.display);
    let token_conflict = parts.iter().any(|part| part.token_name != first.token_name);
    let display = if display_conflict { stringify_key_stroke(&first.stroke) } else { first.display.clone() };
    (display, if token_conflict { None } else { first.token_name.clone() })
}

fn part_presentation<T>(bindings: &[Rc<BindingState<T>>], index: usize) -> (String, Option<String>) {
    let parts: Vec<&KeySequencePart> = bindings.iter().filter_map(|binding| binding.sequence.get(index)).collect();
    if parts.is_empty() {
        let display = bindings
            .first()
            .and_then(|binding| binding.sequence.get(index))
            .map(|part| stringify_key_stroke(&part.stroke));
        return (display.unwrap_or_default(), None);
    }
    merged_presentation(&parts)
}

struct Node<T> {
    stroke: KeyStroke,
    match_: KeyMatch,
    depth: usize,
    children: Vec<Node<T>>,
    bindings: Vec<Rc<BindingState<T>>>,
    reachable: Vec<Rc<BindingState<T>>>,
}

/// `buildSequenceTree` (children in insertion order).
fn build_tree<T>(bindings: &[Rc<BindingState<T>>]) -> Node<T> {
    let mut root = Node {
        stroke: KeyStroke { name: String::new(), ctrl: false, shift: false, meta: false, super_: false, hyper: false },
        match_: "".into(),
        depth: 0,
        children: Vec::new(),
        bindings: Vec::new(),
        reachable: Vec::new(),
    };
    for binding in bindings.iter().filter(|binding| !binding.release) {
        let mut node = &mut root;
        for part in &binding.sequence {
            let position = match node.children.iter().position(|child| child.match_ == part.match_) {
                Some(position) => position,
                None => {
                    let depth = node.depth + 1;
                    node.children.push(Node {
                        stroke: part.stroke.clone(),
                        match_: part.match_.clone(),
                        depth,
                        children: Vec::new(),
                        bindings: Vec::new(),
                        reachable: Vec::new(),
                    });
                    node.children.len() - 1
                }
            };
            node = &mut node.children[position];
            node.reachable.push(binding.clone());
        }
        node.bindings.push(binding.clone());
    }
    root
}

struct Selection<T> {
    display: String,
    token_name: Option<String>,
    continues: bool,
    first_binding: Option<Rc<BindingState<T>>>,
    command_binding: Option<Rc<BindingState<T>>>,
    bindings: Vec<Rc<BindingState<T>>>,
    stop: bool,
}

struct ActiveKeyState<T> {
    match_: KeyMatch,
    stroke: KeyStroke,
    display: String,
    token_name: Option<String>,
    continues: bool,
    first_binding: Option<Rc<BindingState<T>>>,
    command_binding: Option<Rc<BindingState<T>>>,
    bindings: Vec<Rc<BindingState<T>>>,
}

fn merge_active_key<T>(
    states: &mut Vec<ActiveKeyState<T>>,
    stroke: &KeyStroke,
    match_: &KeyMatch,
    selection: &Selection<T>,
) {
    match states.iter_mut().find(|state| state.match_ == *match_) {
        None => states.push(ActiveKeyState {
            match_: match_.clone(),
            stroke: stroke.clone(),
            display: selection.display.clone(),
            token_name: selection.token_name.clone(),
            continues: selection.continues,
            first_binding: selection.first_binding.clone(),
            command_binding: selection.command_binding.clone(),
            bindings: selection.bindings.clone(),
        }),
        Some(state) => {
            if state.first_binding.is_none() {
                state.first_binding = selection.first_binding.clone();
            }
            if state.command_binding.is_none() {
                state.command_binding = selection.command_binding.clone();
            }
            if selection.continues {
                state.continues = true;
            }
            state.bindings.extend(selection.bindings.iter().cloned());
        }
    }
}

/// Compiled-data equality of two layers (`sameCompiledData` of `compiledData`), functions aside.
fn same_layer_data<T: PartialEq>(a: &RegisteredLayer<T>, b: &RegisteredLayer<T>) -> bool {
    let same_conditions =
        |x: &Conditions, y: &Conditions| x.requires == y.requires && x.matchers.len() == y.matchers.len();
    let same_command = |x: &CommandState<T>, y: &CommandState<T>| {
        x.command.name == y.command.name
            && x.command.run.is_some() == y.command.run.is_some()
            && same_fields(&x.command.fields, &y.command.fields)
            && x.attrs == y.attrs
            && same_conditions(&x.conditions, &y.conditions)
    };
    let same_command_ref = |x: &Option<BindingCommand<T>>, y: &Option<BindingCommand<T>>| match (x, y) {
        (None, None) => true,
        (Some(BindingCommand::Name(p)), Some(BindingCommand::Name(q))) => p == q,
        (Some(BindingCommand::Run(_)), Some(BindingCommand::Run(_))) => true,
        _ => false,
    };
    let same_source = |x: &Binding<T>, y: &Binding<T>| {
        x.key == y.key
            && same_command_ref(&x.cmd, &y.cmd)
            && x.event == y.event
            && x.prevent_default == y.prevent_default
            && x.fallthrough == y.fallthrough
            && same_fields(&x.fields, &y.fields)
    };
    let same_binding = |x: &BindingState<T>, y: &BindingState<T>| {
        x.sequence == y.sequence
            && same_command_ref(&x.command, &y.command)
            && x.release == y.release
            && x.attrs == y.attrs
            && same_conditions(&x.conditions, &y.conditions)
            && x.prevent_default == y.prevent_default
            && x.fallthrough == y.fallthrough
            && same_source(&x.source, &y.source)
    };
    let bindings_a = a.bindings.borrow();
    let bindings_b = b.bindings.borrow();
    a.target == b.target
        && a.target_mode == b.target_mode
        && a.priority == b.priority
        && same_conditions(&a.conditions, &b.conditions)
        && same_fields(&a.fields, &b.fields)
        && a.commands.len() == b.commands.len()
        && a.commands.iter().zip(&b.commands).all(|(x, y)| same_command(x, y))
        && a.source_bindings.len() == b.source_bindings.len()
        && a.source_bindings.iter().zip(&b.source_bindings).all(|(x, y)| same_source(x, y))
        && bindings_a.len() == bindings_b.len()
        && bindings_a.iter().zip(bindings_b.iter()).all(|(x, y)| same_binding(x, y))
        && a.has_token_bindings.get() == b.has_token_bindings.get()
}

#[cfg(test)]
mod tests;
