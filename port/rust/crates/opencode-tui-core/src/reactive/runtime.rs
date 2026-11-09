//! The reactive graph: a port of solid-js 1.9.17's `dist/solid.js` core (createRoot, createSignal, createMemo,
//! createComputed, createRenderEffect, createEffect, batch, untrack, onCleanup, runWithOwner and the scheduler:
//! readSignal, writeSignal, updateComputation, runComputation, runTop, runUpdates, completeUpdates,
//! runUserEffects, lookUpstream, markDownstream, cleanNode). Function by function, the order of every queue and
//! every state change is Solid's, so effects run in the same order, batch the same way and clean up at the same
//! moments.
//!
//! Divergences:
//! - Nodes live in a thread-local arena behind generational ids instead of being garbage collected. A signal or
//!   computation belongs to the owner that was current when it was created and is freed when that owner is cleaned
//!   (re-run or disposed); Solid would collect it then unless something still referenced it. Reading a freed signal
//!   panics; writing one is ignored. Nodes created with no owner (or under an unowned root) live for the process.
//! - Signal and memo comparators default to `PartialEq` (Solid compares with `===`, identity for objects): a value
//!   set to an equal one does not notify. Use `equals: None` to always notify (Solid's `equals: false`), or an Rc
//!   newtype comparing pointers for identity semantics.
//! - No transitions, suspense, hydration, external sources or dev hooks (the TUI uses none of them).
//! - An error thrown in a computation is a panic. Solid's catchError/onError/ErrorBoundary handlers are not
//!   reached (TODO(port-tui): error boundaries with catch_unwind if the app's ErrorBoundary must render).
use std::any::Any;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

pub(crate) const STALE: u8 = 1;
pub(crate) const PENDING: u8 = 2;

/// A node of the graph: a signal or a computation (memo, computed, render effect, effect, root).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId {
    index: u32,
    generation: u32,
}

/// A reactive scope: the owner computations, cleanups and contexts attach to (Solid's `Owner`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Owner(pub(crate) OwnerRef);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OwnerRef {
    /// `UNOWNED`: computations created under it are owned by nothing.
    Unowned,
    Node(NodeId),
}

type Value = Rc<dyn Any>;
type Comparator = Rc<dyn Fn(&dyn Any, &dyn Any) -> bool>;
type ComputeFn = Box<dyn FnMut(Option<Value>) -> Value>;

/// Context values visible from a node: Solid's `Owner.context` object, copied onto each computation at creation.
#[derive(Clone, Default)]
pub(crate) struct Contexts {
    pub(crate) entries: Vec<(usize, Rc<dyn Any>)>,
}

impl Contexts {
    pub(crate) fn get(&self, id: usize) -> Option<Rc<dyn Any>> {
        self.entries.iter().rev().find(|(key, _)| *key == id).map(|(_, value)| value.clone())
    }

    pub(crate) fn with(&self, id: usize, value: Rc<dyn Any>) -> Contexts {
        let mut entries: Vec<_> = self.entries.iter().filter(|(key, _)| *key != id).cloned().collect();
        entries.push((id, value));
        Contexts { entries }
    }
}

struct Node {
    generation: u32,
    live: bool,
    /// The value, as `Rc<RefCell<T>>`.
    value: Option<Value>,
    comparator: Option<Comparator>,
    observers: Option<Vec<NodeId>>,
    observer_slots: Vec<usize>,
    // Computations only.
    computation: bool,
    func: Option<ComputeFn>,
    state: u8,
    updated_at: Option<u64>,
    sources: Option<Vec<NodeId>>,
    source_slots: Vec<usize>,
    owned: Option<Vec<NodeId>>,
    /// Signals created while this node was the owner (freed with its scope).
    owned_signals: Vec<NodeId>,
    cleanups: Option<Vec<Box<dyn FnOnce()>>>,
    owner: Option<NodeId>,
    context: Option<Rc<Contexts>>,
    pure: bool,
    user: bool,
    /// Whether this computation is a memo (Solid: `"observers" in node`).
    memo: bool,
}

impl Node {
    fn empty(generation: u32) -> Node {
        Node {
            generation,
            live: true,
            value: None,
            comparator: None,
            observers: None,
            observer_slots: Vec::new(),
            computation: false,
            func: None,
            state: 0,
            updated_at: None,
            sources: None,
            source_slots: Vec::new(),
            owned: None,
            owned_signals: Vec::new(),
            cleanups: None,
            owner: None,
            context: None,
            pure: false,
            user: false,
            memo: false,
        }
    }
}

#[derive(Default)]
struct Runtime {
    nodes: Vec<Node>,
    /// Freed slots, reused oldest first: a freed node's last value stays readable until its slot is reused.
    free: std::collections::VecDeque<u32>,
    owner: Option<OwnerRef>,
    listener: Option<NodeId>,
    updates: Option<Vec<NodeId>>,
    effects: Option<Vec<NodeId>>,
    exec_count: u64,
    /// `runEffects === runUserEffects` (set by the first createEffect).
    user_effects: bool,
    /// Cleanups and contexts of the unowned root, which Solid keeps on the shared UNOWNED object.
    next_context_id: usize,
}

thread_local! {
    static RT: RefCell<Runtime> = RefCell::new(Runtime::default());
}

fn rt<R>(f: impl FnOnce(&mut Runtime) -> R) -> R {
    RT.with(|cell| f(&mut cell.borrow_mut()))
}

impl Runtime {
    fn alloc(&mut self) -> NodeId {
        if let Some(index) = self.free.pop_front() {
            let generation = self.nodes[index as usize].generation.wrapping_add(1);
            self.nodes[index as usize] = Node::empty(generation);
            NodeId { index, generation }
        } else {
            let index = self.nodes.len() as u32;
            self.nodes.push(Node::empty(0));
            NodeId { index, generation: 0 }
        }
    }

    fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index as usize).filter(|node| node.live && node.generation == id.generation)
    }

    fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(id.index as usize).filter(|node| node.live && node.generation == id.generation)
    }

    /// A node by id, also after it was freed, until its slot is reused (for reads of a freed node's last value).
    fn retained(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index as usize).filter(|node| node.generation == id.generation)
    }

    fn release(&mut self, id: NodeId) {
        if let Some(node) = self.get_mut(id) {
            node.live = false;
            node.func = None;
            node.comparator = None;
            node.observers = None;
            node.sources = None;
            node.owned = None;
            node.cleanups = None;
            node.context = None;
            node.owned_signals = Vec::new();
            self.free.push_back(id.index);
        }
    }

    fn current_owner_node(&self) -> Option<NodeId> {
        match self.owner {
            Some(OwnerRef::Node(id)) => Some(id),
            _ => None,
        }
    }

    fn state(&self, id: NodeId) -> u8 {
        self.get(id).map_or(0, |node| node.state)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Creation
// ---------------------------------------------------------------------------------------------------------------

/// `createComputation(fn, init, pure, state)`.
fn create_computation(func: ComputeFn, init: Option<Value>, pure: bool, state: u8) -> NodeId {
    rt(|rt| {
        let owner = rt.owner;
        let context = match owner {
            Some(OwnerRef::Node(id)) => rt.get(id).and_then(|node| node.context.clone()),
            _ => None,
        };
        let id = rt.alloc();
        let node = rt.get_mut(id).unwrap();
        node.computation = true;
        node.func = Some(func);
        node.state = state;
        node.value = init;
        node.pure = pure;
        node.context = context;
        if let Some(OwnerRef::Node(owner_id)) = owner {
            node.owner = Some(owner_id);
            if let Some(owner_node) = rt.get_mut(owner_id) {
                owner_node.owned.get_or_insert_with(Vec::new).push(id);
            }
        }
        id
    })
}

fn signal_node(value: Value, comparator: Option<Comparator>) -> NodeId {
    rt(|rt| {
        let owner = rt.current_owner_node();
        let id = rt.alloc();
        let node = rt.get_mut(id).unwrap();
        node.value = Some(value);
        node.comparator = comparator;
        if let Some(owner_id) = owner {
            if let Some(owner_node) = rt.get_mut(owner_id) {
                owner_node.owned_signals.push(id);
            }
        }
        id
    })
}

fn comparator_of<T: PartialEq + 'static>() -> Comparator {
    Rc::new(|a: &dyn Any, b: &dyn Any| {
        match (a.downcast_ref::<RefCell<T>>(), b.downcast_ref::<RefCell<T>>()) {
            (Some(a), Some(b)) => *a.borrow() == *b.borrow(),
            _ => false,
        }
    })
}

fn custom_comparator<T: 'static>(equals: fn(&T, &T) -> bool) -> Comparator {
    Rc::new(move |a: &dyn Any, b: &dyn Any| {
        match (a.downcast_ref::<RefCell<T>>(), b.downcast_ref::<RefCell<T>>()) {
            (Some(a), Some(b)) => equals(&a.borrow(), &b.borrow()),
            _ => false,
        }
    })
}

fn wrap<T: 'static>(value: T) -> Value {
    Rc::new(RefCell::new(value))
}

fn unwrap_ref<T: 'static>(value: &Value) -> &RefCell<T> {
    value.downcast_ref::<RefCell<T>>().expect("reactive value of another type")
}

/// `createRoot(fn, detachedOwner)`. `f` receives the root's dispose function. A root created with
/// [`create_root_unowned`] is Solid's `createRoot(() => ...)` (no dispose parameter): it is never disposed.
pub fn create_root<R>(f: impl FnOnce(Dispose) -> R) -> R {
    let (listener, owner) = rt(|rt| (rt.listener, rt.owner));
    // The root is a computation-less node holding owned computations, cleanups and contexts.
    let root = rt(|rt| {
        let context = match owner {
            Some(OwnerRef::Node(id)) => rt.get(id).and_then(|node| node.context.clone()),
            _ => None,
        };
        let id = rt.alloc();
        let node = rt.get_mut(id).unwrap();
        node.context = context;
        // A root is not in its owner's `owned`: only its own dispose cleans it.
        node.owner = owner.and_then(|owner| match owner {
            OwnerRef::Node(id) => Some(id),
            OwnerRef::Unowned => None,
        });
        id
    });
    rt(|rt| {
        rt.owner = Some(OwnerRef::Node(root));
        rt.listener = None;
    });
    let dispose = Dispose(root);
    let result = run_updates(|| f(dispose), true);
    rt(|rt| {
        rt.listener = listener;
        rt.owner = owner;
    });
    result
}

/// `createRoot(() => ...)`: computations created inside are owned by nothing and live for the process.
pub fn create_root_unowned<R>(f: impl FnOnce() -> R) -> R {
    let (listener, owner) = rt(|rt| (rt.listener, rt.owner));
    rt(|rt| {
        rt.owner = Some(OwnerRef::Unowned);
        rt.listener = None;
    });
    let result = run_updates(f, true);
    rt(|rt| {
        rt.listener = listener;
        rt.owner = owner;
    });
    result
}

/// The dispose function a root receives: `() => untrack(() => cleanNode(root))`.
#[derive(Clone, Copy, Debug)]
pub struct Dispose(NodeId);

impl Dispose {
    pub fn dispose(&self) {
        let root = self.0;
        untrack(|| clean_node(root));
        rt(|rt| rt.release(root));
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------------------------------------------

/// A signal handle (Solid's `[getter, setter]` pair in one `Copy` handle).
pub struct Signal<T> {
    pub(crate) id: NodeId,
    _type: PhantomData<fn() -> T>,
}

impl<T> Clone for Signal<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Signal<T> {}
impl<T> PartialEq for Signal<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl<T> std::fmt::Debug for Signal<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Signal({:?})", self.id)
    }
}

/// `createSignal(value)`: compared with `PartialEq`.
pub fn create_signal<T: PartialEq + 'static>(value: T) -> (Signal<T>, Signal<T>) {
    let signal = Signal { id: signal_node(wrap(value), Some(comparator_of::<T>())), _type: PhantomData };
    (signal, signal)
}

/// `createSignal(value, { equals })`: `None` notifies on every write (`equals: false`).
pub fn create_signal_with<T: 'static>(value: T, equals: Option<fn(&T, &T) -> bool>) -> (Signal<T>, Signal<T>) {
    let signal = Signal { id: signal_node(wrap(value), equals.map(custom_comparator::<T>)), _type: PhantomData };
    (signal, signal)
}

impl<T: 'static> Signal<T> {
    /// Read and track (`signal()`).
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.with(T::clone)
    }

    /// Read without tracking (`untrack(signal)`).
    pub fn get_untracked(&self) -> T
    where
        T: Clone,
    {
        untrack(|| self.get())
    }

    /// Read by reference, tracking.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let value = read_signal(self.id);
        f(&unwrap_ref::<T>(&value).borrow())
    }

    pub fn with_untracked<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        untrack(|| self.with(f))
    }

    /// `setSignal(value)`.
    pub fn set(&self, value: T) {
        write_signal(self.id, wrap(value), false);
    }

    /// `setSignal(prev => next)`, compared like `set`.
    pub fn set_with(&self, f: impl FnOnce(&T) -> T) {
        let next = {
            let value = rt(|rt| rt.get(self.id).and_then(|node| node.value.clone()));
            let Some(value) = value else { return };
            f(&unwrap_ref::<T>(&value).borrow())
        };
        self.set(next);
    }

    /// Mutate in place and notify (Solid's `setSignal(prev => (mutate(prev), prev), { equals: false })` idiom).
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        let value = rt(|rt| rt.get(self.id).and_then(|node| node.value.clone()));
        let Some(value) = value else { return };
        f(&mut unwrap_ref::<T>(&value).borrow_mut());
        notify(self.id);
    }

    /// Whether the signal's node is still alive (its owner has not been cleaned).
    pub fn is_alive(&self) -> bool {
        rt(|rt| rt.get(self.id).is_some())
    }
}

/// `readSignal`, returning the node's value (`Rc<RefCell<T>>`).
fn read_signal(id: NodeId) -> Value {
    let live = rt(|rt| rt.get(id).map(|node| (node.sources.is_some(), node.state)));
    let Some((has_sources, state)) = live else {
        // A freed node (its owner was cleaned): its last value, untracked, as a collected-later JS object reads.
        return rt(|rt| rt.retained(id).and_then(|node| node.value.clone()))
            .expect("read of a signal or memo whose slot was reused (a handle kept past its owner)");
    };
    if has_sources && state != 0 {
        if state == STALE {
            update_computation(id);
        } else {
            let updates = rt(|rt| rt.updates.take());
            run_updates(|| look_upstream(id, None), false);
            rt(|rt| rt.updates = updates);
        }
    }
    rt(|rt| {
        if let Some(listener) = rt.listener {
            let last_is_listener = rt
                .get(id)
                .and_then(|node| node.observers.as_ref())
                .is_some_and(|observers| observers.last() == Some(&listener));
            if !last_is_listener && rt.get(listener).is_some() {
                let s_slot = rt.get(id).unwrap().observers.as_ref().map_or(0, Vec::len);
                let listener_node = rt.get_mut(listener).unwrap();
                let sources = listener_node.sources.get_or_insert_with(Vec::new);
                sources.push(id);
                listener_node.source_slots.push(s_slot);
                let source_len = sources.len();
                let node = rt.get_mut(id).unwrap();
                node.observers.get_or_insert_with(Vec::new).push(listener);
                node.observer_slots.push(source_len - 1);
            }
        }
        rt.get(id).and_then(|node| node.value.clone()).expect("read of a disposed signal or memo")
    })
}

/// `writeSignal(node, value, isComp)`.
fn write_signal(id: NodeId, value: Value, _is_comp: bool) {
    let (current, comparator) = match rt(|rt| rt.get(id).map(|node| (node.value.clone(), node.comparator.clone()))) {
        Some(found) => found,
        None => return,
    };
    if let (Some(current), Some(comparator)) = (&current, &comparator) {
        if comparator(current.as_ref(), value.as_ref()) {
            return;
        }
    }
    rt(|rt| {
        if let Some(node) = rt.get_mut(id) {
            node.value = Some(value);
        }
    });
    notify(id);
}

/// The observer half of writeSignal: mark observers stale and queue them.
fn notify(id: NodeId) {
    let has_observers = rt(|rt| rt.get(id).and_then(|node| node.observers.as_ref()).is_some_and(|o| !o.is_empty()));
    if !has_observers {
        return;
    }
    run_updates(
        || {
            let mut i = 0;
            loop {
                let next = rt(|rt| rt.get(id).and_then(|node| node.observers.as_ref()).and_then(|o| o.get(i).copied()));
                let Some(observer) = next else { break };
                let mark = rt(|rt| {
                    let Some(node) = rt.get(observer) else { return false };
                    let fresh = node.state == 0;
                    let pure = node.pure;
                    let has_observers = node.observers.is_some();
                    if fresh {
                        if pure {
                            rt.updates.get_or_insert_with(Vec::new).push(observer);
                        } else {
                            rt.effects.get_or_insert_with(Vec::new).push(observer);
                        }
                    }
                    rt.get_mut(observer).unwrap().state = STALE;
                    fresh && has_observers
                });
                if mark {
                    mark_downstream(observer);
                }
                i += 1;
            }
            let overflow = rt(|rt| rt.updates.as_ref().is_some_and(|u| u.len() > 1_000_000));
            if overflow {
                rt(|rt| rt.updates = Some(Vec::new()));
                panic!("Potential Infinite Loop");
            }
        },
        false,
    );
}

// ---------------------------------------------------------------------------------------------------------------
// Computations
// ---------------------------------------------------------------------------------------------------------------

/// `updateComputation(node)`.
fn update_computation(id: NodeId) {
    let has_fn = rt(|rt| rt.get(id).is_some_and(|node| node.func.is_some()));
    if !has_fn {
        return;
    }
    clean_node(id);
    let time = rt(|rt| rt.exec_count);
    run_computation(id, time);
}

/// `runComputation(node, value, time)`.
fn run_computation(id: NodeId, time: u64) {
    let taken = rt(|rt| {
        let prev = (rt.owner, rt.listener);
        let node = rt.get_mut(id)?;
        let func = node.func.take()?;
        let value = node.value.clone();
        rt.owner = Some(OwnerRef::Node(id));
        rt.listener = Some(id);
        Some((func, value, prev))
    });
    let Some((mut func, value, (owner, listener))) = taken else { return };
    let next = func(value);
    let (memo, updated_at) = rt(|rt| {
        rt.owner = owner;
        rt.listener = listener;
        match rt.get_mut(id) {
            Some(node) => {
                if node.func.is_none() {
                    node.func = Some(func);
                }
                (Some(node.memo), node.updated_at)
            }
            None => (None, None),
        }
    });
    let Some(memo) = memo else { return };
    if updated_at.is_none_or(|at| at <= time) {
        if updated_at.is_some() && memo {
            write_signal(id, next, true);
        } else {
            rt(|rt| {
                if let Some(node) = rt.get_mut(id) {
                    node.value = Some(next);
                }
            });
        }
        rt(|rt| {
            if let Some(node) = rt.get_mut(id) {
                node.updated_at = Some(time);
            }
        });
    }
}

/// `runTop(node)`.
fn run_top(id: NodeId) {
    let state = rt(|rt| rt.state(id));
    if state == 0 {
        return;
    }
    if state == PENDING {
        return look_upstream(id, None);
    }
    let ancestors = rt(|rt| {
        let exec = rt.exec_count;
        let mut ancestors = vec![id];
        let mut node = rt.get(id).and_then(|n| n.owner);
        while let Some(current) = node {
            let Some(n) = rt.get(current) else { break };
            if !(n.updated_at.is_none_or(|at| at < exec)) {
                break;
            }
            if n.state != 0 {
                ancestors.push(current);
            }
            node = n.owner;
        }
        ancestors
    });
    for i in (0..ancestors.len()).rev() {
        let node = ancestors[i];
        let state = rt(|rt| rt.state(node));
        if state == STALE {
            update_computation(node);
        } else if state == PENDING {
            let updates = rt(|rt| rt.updates.take());
            let ignore = ancestors[0];
            run_updates(|| look_upstream(node, Some(ignore)), false);
            rt(|rt| rt.updates = updates);
        }
    }
}

/// `runUpdates(fn, init)`.
pub(crate) fn run_updates<R>(f: impl FnOnce() -> R, init: bool) -> R {
    if rt(|rt| rt.updates.is_some()) {
        return f();
    }
    let wait = rt(|rt| {
        if !init {
            rt.updates = Some(Vec::new());
        }
        let wait = rt.effects.is_some();
        if !wait {
            rt.effects = Some(Vec::new());
        }
        rt.exec_count += 1;
        wait
    });
    let result = f();
    complete_updates(wait);
    result
}

/// `completeUpdates(wait)`.
fn complete_updates(wait: bool) {
    if rt(|rt| rt.updates.is_some()) {
        run_queue();
        rt(|rt| rt.updates = None);
    }
    if wait {
        return;
    }
    let effects = rt(|rt| rt.effects.take()).unwrap_or_default();
    if !effects.is_empty() {
        run_updates(
            || {
                if rt(|rt| rt.user_effects) {
                    run_user_effects(effects);
                } else {
                    for id in effects {
                        run_top(id);
                    }
                }
            },
            false,
        );
    }
}

/// `runQueue(Updates)`: the queue may grow while it runs (computations queued by writes inside it).
fn run_queue() {
    let mut i = 0;
    loop {
        let next = rt(|rt| rt.updates.as_ref().and_then(|queue| queue.get(i).copied()));
        let Some(id) = next else { break };
        run_top(id);
        i += 1;
    }
}

/// `runUserEffects(queue)`: render effects first, then user effects, each in queue order.
fn run_user_effects(queue: Vec<NodeId>) {
    let mut user = Vec::new();
    for id in queue {
        let is_user = rt(|rt| rt.get(id).is_some_and(|node| node.user));
        if !is_user {
            run_top(id);
        } else {
            user.push(id);
        }
    }
    for id in user {
        run_top(id);
    }
}

/// `lookUpstream(node, ignore)`.
fn look_upstream(id: NodeId, ignore: Option<NodeId>) {
    rt(|rt| {
        if let Some(node) = rt.get_mut(id) {
            node.state = 0;
        }
    });
    let mut i = 0;
    loop {
        let next = rt(|rt| rt.get(id).and_then(|node| node.sources.as_ref()).and_then(|s| s.get(i).copied()));
        let Some(source) = next else { break };
        let info = rt(|rt| {
            let exec = rt.exec_count;
            rt.get(source).filter(|node| node.sources.is_some()).map(|node| (node.state, node.updated_at.is_none_or(|at| at < exec)))
        });
        if let Some((state, outdated)) = info {
            if state == STALE {
                if Some(source) != ignore && outdated {
                    run_top(source);
                }
            } else if state == PENDING {
                look_upstream(source, ignore);
            }
        }
        i += 1;
    }
}

/// `markDownstream(node)`.
fn mark_downstream(id: NodeId) {
    let observers = rt(|rt| rt.get(id).and_then(|node| node.observers.clone()).unwrap_or_default());
    for observer in observers {
        let mark = rt(|rt| {
            let Some(node) = rt.get_mut(observer) else { return false };
            if node.state != 0 {
                return false;
            }
            node.state = PENDING;
            let pure = node.pure;
            let has_observers = node.observers.is_some();
            if pure {
                rt.updates.get_or_insert_with(Vec::new).push(observer);
            } else {
                rt.effects.get_or_insert_with(Vec::new).push(observer);
            }
            has_observers
        });
        if mark {
            mark_downstream(observer);
        }
    }
}

/// `cleanNode(node)`: unsubscribe from sources, clean owned computations (last first), run cleanups (last
/// first), reset the state. Owned computations and signals are freed (see the module doc).
pub(crate) fn clean_node(id: NodeId) {
    rt(|rt| {
        let Some(node) = rt.get_mut(id) else { return };
        let mut sources = node.sources.take();
        let mut source_slots = std::mem::take(&mut node.source_slots);
        if let Some(list) = sources.as_mut() {
            while let Some(source) = list.pop() {
                let index = source_slots.pop().unwrap_or(0);
                let Some(source_node) = rt.get_mut(source) else { continue };
                let Some(observers) = source_node.observers.as_mut() else { continue };
                if observers.is_empty() {
                    continue;
                }
                let n = observers.pop().unwrap();
                let s = source_node.observer_slots.pop().unwrap_or(0);
                if index < observers.len() {
                    observers[index] = n;
                    source_node.observer_slots[index] = s;
                    if let Some(moved) = rt.get_mut(n) {
                        if s < moved.source_slots.len() {
                            moved.source_slots[s] = index;
                        }
                    }
                }
            }
        }
        // Solid keeps the (now empty) array: `sources` stays truthy.
        if let Some(node) = rt.get_mut(id) {
            if sources.is_some() {
                node.sources = Some(Vec::new());
            }
            node.source_slots = source_slots;
        }
    });
    let owned = rt(|rt| rt.get_mut(id).and_then(|node| node.owned.take()));
    if let Some(owned) = owned {
        for child in owned.into_iter().rev() {
            clean_node(child);
            rt(|rt| rt.release(child));
        }
    }
    let signals = rt(|rt| rt.get_mut(id).map(|node| std::mem::take(&mut node.owned_signals)).unwrap_or_default());
    let cleanups = rt(|rt| rt.get_mut(id).and_then(|node| node.cleanups.take()));
    if let Some(cleanups) = cleanups {
        for cleanup in cleanups.into_iter().rev() {
            cleanup();
        }
    }
    rt(|rt| {
        for signal in signals {
            rt.release(signal);
        }
        if let Some(node) = rt.get_mut(id) {
            node.state = 0;
        }
    });
}

// ---------------------------------------------------------------------------------------------------------------
// Public computation API
// ---------------------------------------------------------------------------------------------------------------

fn boxed<T: 'static>(mut f: impl FnMut(Option<T>) -> T + 'static) -> ComputeFn
where
    T: Clone,
{
    Box::new(move |prev: Option<Value>| {
        let prev = prev.map(|value| unwrap_ref::<T>(&value).borrow().clone());
        wrap(f(prev))
    })
}

fn boxed_unit(mut f: impl FnMut() + 'static) -> ComputeFn {
    Box::new(move |_| {
        f();
        wrap(())
    })
}

/// `createComputed(fn)`: runs now, re-runs with the pure queue (before effects).
pub fn create_computed(f: impl FnMut() + 'static) {
    let id = create_computation(boxed_unit(f), None, true, STALE);
    update_computation(id);
}

/// `createComputed(prev => next, init)`.
pub fn create_computed_with<T: Clone + 'static>(init: Option<T>, f: impl FnMut(Option<T>) -> T + 'static) {
    let id = create_computation(boxed(f), init.map(wrap), true, STALE);
    update_computation(id);
}

/// `createRenderEffect(fn)`: runs now, re-runs with the effects queue before user effects.
pub fn create_render_effect(f: impl FnMut() + 'static) {
    let id = create_computation(boxed_unit(f), None, false, STALE);
    update_computation(id);
}

/// `createRenderEffect(prev => next, init)`.
pub fn create_render_effect_with<T: Clone + 'static>(init: Option<T>, f: impl FnMut(Option<T>) -> T + 'static) {
    let id = create_computation(boxed(f), init.map(wrap), false, STALE);
    update_computation(id);
}

/// `createEffect(fn)`: queued until the current update completes (after render effects), or run now outside one.
pub fn create_effect(f: impl FnMut() + 'static) {
    effect_node(boxed_unit(f), None);
}

/// `createEffect(prev => next, init)`.
pub fn create_effect_with<T: Clone + 'static>(init: Option<T>, f: impl FnMut(Option<T>) -> T + 'static) {
    effect_node(boxed(f), init.map(wrap));
}

fn effect_node(func: ComputeFn, init: Option<Value>) {
    rt(|rt| rt.user_effects = true);
    let id = create_computation(func, init, false, STALE);
    let queued = rt(|rt| {
        if let Some(node) = rt.get_mut(id) {
            node.user = true;
        }
        match rt.effects.as_mut() {
            Some(effects) => {
                effects.push(id);
                true
            }
            None => false,
        }
    });
    if !queued {
        update_computation(id);
    }
}

/// `onMount(fn)` = `createEffect(() => untrack(fn))`.
pub fn on_mount(f: impl FnOnce() + 'static) {
    let mut f = Some(f);
    create_effect(move || {
        if let Some(f) = f.take() {
            untrack(f);
        }
    });
}

/// `onCleanup(fn)`: runs when the current owner is cleaned (re-run or disposed). Ignored outside an owner.
pub fn on_cleanup(f: impl FnOnce() + 'static) {
    rt(|rt| {
        if let Some(OwnerRef::Node(id)) = rt.owner {
            if let Some(node) = rt.get_mut(id) {
                node.cleanups.get_or_insert_with(Vec::new).push(Box::new(f));
            }
        }
    });
}

/// A memo handle (`createMemo`'s accessor).
pub struct Memo<T> {
    pub(crate) id: NodeId,
    _type: PhantomData<fn() -> T>,
}

impl<T> Clone for Memo<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Memo<T> {}
impl<T> PartialEq for Memo<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl<T> std::fmt::Debug for Memo<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Memo({:?})", self.id)
    }
}

impl<T: 'static> Memo<T> {
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.with(T::clone)
    }

    pub fn get_untracked(&self) -> T
    where
        T: Clone,
    {
        untrack(|| self.get())
    }

    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let value = read_signal(self.id);
        f(&unwrap_ref::<T>(&value).borrow())
    }

    pub fn with_untracked<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        untrack(|| self.with(f))
    }
}

fn memo_node<T: Clone + 'static>(f: impl FnMut(Option<T>) -> T + 'static, comparator: Option<Comparator>) -> Memo<T> {
    let id = create_computation(boxed(f), None, true, 0);
    rt(|rt| {
        if let Some(node) = rt.get_mut(id) {
            node.memo = true;
            node.comparator = comparator;
        }
    });
    update_computation(id);
    Memo { id, _type: PhantomData }
}

/// `createMemo(fn)`: compared with `PartialEq`.
pub fn create_memo<T: Clone + PartialEq + 'static>(mut f: impl FnMut() -> T + 'static) -> Memo<T> {
    memo_node(move |_| f(), Some(comparator_of::<T>()))
}

/// `createMemo(prev => next)`.
pub fn create_memo_with<T: Clone + PartialEq + 'static>(f: impl FnMut(Option<T>) -> T + 'static) -> Memo<T> {
    memo_node(f, Some(comparator_of::<T>()))
}

/// `createMemo(fn, undefined, { equals })`: `None` notifies on every recomputation (`equals: false`).
pub fn create_memo_equals<T: Clone + 'static>(
    mut f: impl FnMut() -> T + 'static,
    equals: Option<fn(&T, &T) -> bool>,
) -> Memo<T> {
    memo_node(move |_| f(), equals.map(custom_comparator::<T>))
}

/// `batch(fn)`.
pub fn batch<R>(f: impl FnOnce() -> R) -> R {
    run_updates(f, false)
}

/// `untrack(fn)`.
pub fn untrack<R>(f: impl FnOnce() -> R) -> R {
    let listener = rt(|rt| rt.listener.take());
    if listener.is_none() {
        return f();
    }
    let result = f();
    rt(|rt| rt.listener = listener);
    result
}

/// `getOwner()`.
pub fn get_owner() -> Option<Owner> {
    rt(|rt| rt.owner.map(Owner))
}

/// `runWithOwner(owner, fn)`.
pub fn run_with_owner<R>(owner: Option<Owner>, f: impl FnOnce() -> R) -> R {
    let (prev_owner, prev_listener) = rt(|rt| (rt.owner, rt.listener));
    rt(|rt| {
        rt.owner = owner.map(|o| o.0);
        rt.listener = None;
    });
    let result = run_updates(f, true);
    rt(|rt| {
        rt.owner = prev_owner;
        rt.listener = prev_listener;
    });
    result
}

/// Whether a computation is running and tracking reads (`getListener() !== null`).
pub fn is_tracking() -> bool {
    rt(|rt| rt.listener.is_some())
}

// ---------------------------------------------------------------------------------------------------------------
// Contexts
// ---------------------------------------------------------------------------------------------------------------

/// `createContext(defaultValue)`.
pub struct Context<T> {
    pub(crate) id: usize,
    default: Option<fn() -> T>,
}

impl<T> Clone for Context<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Context<T> {}

pub fn create_context<T: Clone + 'static>(default: Option<fn() -> T>) -> Context<T> {
    let id = rt(|rt| {
        rt.next_context_id += 1;
        rt.next_context_id
    });
    Context { id, default }
}

/// `useContext(context)`: the value the nearest provider above the current owner set, else the default.
pub fn use_context<T: Clone + 'static>(context: Context<T>) -> Option<T> {
    let found = rt(|rt| match rt.owner {
        Some(OwnerRef::Node(id)) => rt.get(id).and_then(|node| node.context.as_ref()).and_then(|c| c.get(context.id)),
        _ => None,
    });
    match found {
        Some(value) => value.downcast_ref::<T>().cloned(),
        None => context.default.map(|f| f()),
    }
}

/// Sets a context value on the current owner (the body of Solid's `createProvider`:
/// `Owner.context = { ...Owner.context, [id]: value }`). Computations created afterwards under this owner see it.
pub fn provide_context<T: Clone + 'static>(context: Context<T>, value: T) {
    rt(|rt| {
        if let Some(OwnerRef::Node(id)) = rt.owner {
            if let Some(node) = rt.get_mut(id) {
                let current = node.context.as_deref().cloned().unwrap_or_default();
                node.context = Some(Rc::new(current.with(context.id, Rc::new(value))));
            }
        }
    });
}

/// The number of live nodes (tests and memory checks).
pub fn live_nodes() -> usize {
    rt(|rt| rt.nodes.iter().filter(|node| node.live).count())
}
