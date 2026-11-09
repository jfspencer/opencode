//! `@opentui/solid`: how JSX becomes renderables. The universal renderer (`src/renderer/universal.js`:
//! `insert`, `insertExpression`, `normalizeIncomingArray`, `reconcileArrays`, `cleanChildren`, `appendNodes`,
//! `replaceNode`) and the reconciler's node operations (`src/reconciler.ts`: insertNode, removeNode, text nodes,
//! slot nodes), ported so children are inserted, moved and removed in the same order.
//!
//! What the Solid compiler emits for a template becomes direct calls (see `opencode-tui`'s module doc for the
//! porting recipe):
//!
//! | compiled JSX | here |
//! |---|---|
//! | `createElement("box")` | `elements::box_()` (and `text`, `span`, `b`, `i`, `u`, `br`, `a`, `scrollbox`, ...) |
//! | `insertNode(el, child)` | `insert_node(el, child, None)` |
//! | `setProp(el, "flexGrow", 1)` | `el.set_flex_grow(Some(1.0))` (typed setters, see `elements`) |
//! | `effect(() => setProp(el, "fg", x()))` | `bind(move \|\| x.get(), move \|v\| el.set_fg(v))` |
//! | `insert(el, expr)` / `insert(el, expr, null)` / `insert(el, expr, sibling)` | `insert(el, view, At::Only)` / `At::End` / `At::Before(sibling)` |
//! | `createComponent(Comp, props)` | `component(\|\| comp(props))` (runs untracked) |
//! | `memo(() => cond)` | `create_memo(move \|\| cond)` |
use std::cell::RefCell;
use std::rc::Rc;

use crate::reactive::{create_memo, create_render_effect, create_render_effect_with, untrack};
use crate::tree::Element;

pub mod flow;

pub use flow::*;

/// What JSX evaluates to: nothing, a node, text, a list, or a function inserted reactively.
#[derive(Clone, Default)]
pub enum View {
    #[default]
    Empty,
    Node(Element),
    Text(Rc<str>),
    List(Rc<Vec<View>>),
    /// A function child (`() => expr`, a memo accessor): inserted with a render effect that tracks it.
    Dyn(Rc<dyn Fn() -> View>),
}

impl PartialEq for View {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (View::Empty, View::Empty) => true,
            (View::Node(a), View::Node(b)) => a == b,
            (View::Text(a), View::Text(b)) => a == b,
            (View::List(a), View::List(b)) => Rc::ptr_eq(a, b) || a == b,
            (View::Dyn(a), View::Dyn(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            View::Empty => write!(f, "Empty"),
            View::Node(e) => write!(f, "Node({e:?})"),
            View::Text(t) => write!(f, "Text({t:?})"),
            View::List(l) => write!(f, "List({l:?})"),
            View::Dyn(_) => write!(f, "Dyn"),
        }
    }
}

impl From<Element> for View {
    fn from(e: Element) -> Self {
        View::Node(e)
    }
}
impl From<&str> for View {
    fn from(s: &str) -> Self {
        View::Text(s.into())
    }
}
impl From<String> for View {
    fn from(s: String) -> Self {
        View::Text(s.into())
    }
}
impl From<Vec<View>> for View {
    fn from(v: Vec<View>) -> Self {
        View::List(Rc::new(v))
    }
}
impl<T: Into<View>> From<Option<T>> for View {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(View::Empty)
    }
}

impl View {
    /// A function child: `() => f()`.
    pub fn dynamic(f: impl Fn() -> View + 'static) -> View {
        View::Dyn(Rc::new(f))
    }

    pub fn list(items: impl IntoIterator<Item = View>) -> View {
        View::List(Rc::new(items.into_iter().collect()))
    }
}

/// `createComponent(Comp, props)`: the component body runs untracked.
pub fn component(f: impl FnOnce() -> View) -> View {
    untrack(f)
}

/// `memo(fn)` as the compiler emits for a conditional expression: a memo read through a function child.
pub fn memo_view(f: impl Fn() -> View + 'static) -> View {
    let memo = create_memo(f);
    View::dynamic(move || memo.get())
}

/// A dynamic prop: `effect(_p$ => { const v = expr; v !== _p$.e && setProp(el, name, v); })`.
pub fn bind<T: Clone + PartialEq + 'static>(value: impl Fn() -> T + 'static, set: impl Fn(T) + 'static) {
    create_render_effect_with::<Option<T>>(None, move |prev| {
        let next = value();
        if prev.as_ref().and_then(|p| p.as_ref()) != Some(&next) {
            set(next.clone());
        }
        Some(next)
    });
}

/// A dynamic prop whose value has no `PartialEq` (it is set on every change of what it reads).
pub fn bind_always<T: 'static>(value: impl Fn() -> T + 'static, set: impl Fn(T) + 'static) {
    create_render_effect(move || set(value()));
}

// ---------------------------------------------------------------------------------------------------------------
// Node operations (reconciler.ts)
// ---------------------------------------------------------------------------------------------------------------

fn is_text_parent(el: Element) -> bool {
    el.is_text_node() || el.is::<crate::renderables::text::TextKind>()
}

/// `getNodeChildren`: a text's text children, else the children.
fn node_children(parent: Element) -> Vec<Element> {
    parent.children()
}

/// `_insertNode(parent, node, anchor)`.
pub fn insert_node(parent: Element, node: Element, anchor: Option<Element>) {
    if node.is_text_node() && !is_text_parent(parent) {
        panic!(
            "Orphan text error: \"{}\" must have a <text> as a parent: {} above {}",
            crate::renderables::text::text_of(node),
            parent.id(),
            node.id()
        );
    }
    let index = anchor.and_then(|a| node_children(parent).iter().position(|c| *c == a));
    if parent.is::<crate::renderables::text::TextKind>() || parent.is_text_node() {
        crate::renderables::text::add_text_child(parent, node, index);
    } else {
        parent.add(node, index);
    }
}

/// `_removeNode(parent, node)`: removed now, destroyed at the end of the tick unless attached again.
pub fn remove_node(parent: Element, node: Element) {
    if parent.is::<crate::renderables::text::TextKind>() || parent.is_text_node() {
        crate::renderables::text::remove_text_child(parent, node);
        crate::tree::with_tree(|t| t.pending_destroy.push(node));
        crate::renderer::schedule_tick();
    } else {
        parent.remove_and_schedule_destroy(node);
    }
}

fn get_first_child(parent: Element) -> Option<Element> {
    node_children(parent).first().copied()
}

/// `getParentNode`.
fn get_parent_node(node: Element) -> Option<Element> {
    node.parent()
}

fn get_next_sibling(node: Element) -> Option<Element> {
    let parent = get_parent_node(node)?;
    let siblings = node_children(parent);
    let index = siblings.iter().position(|c| *c == node)?;
    siblings.get(index + 1).copied()
}

/// `createDynamicTextNode(value)`.
fn create_text_node(value: &str) -> Element {
    crate::renderables::text::text_node_from_string(value)
}

/// `replaceText(textNode, value)`.
fn replace_text(node: Element, value: &str) {
    crate::renderables::text::replace_text(node, value);
}

fn is_text_node(node: Element) -> bool {
    node.is_text_node()
}

/// `createSlotNode()`: an invisible placeholder (a hidden layout node, or an empty text node under text).
fn create_slot_node(parent: Element) -> Element {
    if is_text_parent(parent) {
        create_text_node("")
    } else {
        let slot = crate::elements::box_();
        slot.set_visible(false);
        slot
    }
}

// ---------------------------------------------------------------------------------------------------------------
// insert / insertExpression (universal.js)
// ---------------------------------------------------------------------------------------------------------------

/// What an insertion currently holds (`current`).
#[derive(Clone, Default)]
enum Current {
    #[default]
    Undefined,
    /// Text content (`current` is a string): the parent's first child holds it.
    Str(Rc<str>),
    Node(Element),
    Nodes(Vec<Element>),
    /// `() => current` returned by a nested insertion.
    Fn(Rc<RefCell<Current>>),
}

impl PartialEq for Current {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Current::Undefined, Current::Undefined) => true,
            (Current::Str(a), Current::Str(b)) => a == b,
            (Current::Node(a), Current::Node(b)) => a == b,
            (Current::Nodes(a), Current::Nodes(b)) => a == b,
            (Current::Fn(a), Current::Fn(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// Where an insertion goes among the parent's children: the compiler's marker argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    /// No marker (`undefined`): the expression is the parent's only child and owns all its children.
    Only,
    /// `null`: after the parent's static children (the expression is the last child).
    End,
    /// Before a static sibling.
    Before(Element),
}

impl At {
    fn multi(self) -> bool {
        !matches!(self, At::Only)
    }
    fn anchor(self) -> Option<Element> {
        match self {
            At::Before(el) => Some(el),
            _ => None,
        }
    }
}

/// `insert(parent, accessor, marker)`.
pub fn insert(parent: Element, value: View, marker: At) {
    let multi = marker.multi();
    let initial = if multi { Current::Nodes(Vec::new()) } else { Current::Undefined };
    match value {
        View::Dyn(f) => {
            create_render_effect_with::<CurrentCell>(Some(CurrentCell(Rc::new(RefCell::new(initial)))), move |current| {
                let cell = current.unwrap();
                let value = f();
                let prev = cell.0.borrow().clone();
                let next = insert_expression(parent, value, prev, marker, false);
                *cell.0.borrow_mut() = next;
                cell
            });
        }
        other => {
            insert_expression(parent, other, initial, marker, false);
        }
    }
}

#[derive(Clone)]
struct CurrentCell(Rc<RefCell<Current>>);

fn unwrap_current(mut current: Current) -> Current {
    while let Current::Fn(cell) = current {
        current = cell.borrow().clone();
    }
    current
}

/// `insertExpression(parent, value, current, marker, unwrapArray)`.
fn insert_expression(parent: Element, value: View, current: Current, marker: At, unwrap_array: bool) -> Current {
    let mut current = unwrap_current(current);
    let multi = marker.multi();
    // `value === current`
    match (&value, &current) {
        (View::Node(a), Current::Node(b)) if a == b => return current,
        (View::Text(a), Current::Str(b)) if a == b && !multi => return current,
        _ => {}
    }
    match value {
        View::Text(text) => {
            if multi {
                let first = match &current {
                    Current::Nodes(nodes) => nodes.first().copied(),
                    _ => None,
                };
                let node = match first {
                    Some(node) if is_text_node(node) => {
                        replace_text(node, &text);
                        node
                    }
                    _ => create_text_node(&text),
                };
                current = clean_children(parent, current, marker, Some(node));
            } else if matches!(&current, Current::Str(s) if !s.is_empty()) {
                if let Some(first) = get_first_child(parent) {
                    replace_text(first, &text);
                }
                current = Current::Str(text);
            } else {
                let node = create_text_node(&text);
                clean_children(parent, current, marker, Some(node));
                current = Current::Str(text);
            }
        }
        View::Empty => {
            current = clean_children(parent, current, marker, None);
        }
        View::Dyn(f) => {
            let cell = Rc::new(RefCell::new(current));
            let inner = cell.clone();
            create_render_effect(move || {
                let value = f();
                let prev = inner.borrow().clone();
                let next = insert_expression(parent, value, prev, marker, false);
                *inner.borrow_mut() = next;
            });
            return Current::Fn(cell);
        }
        View::List(items) => {
            let mut array = Vec::new();
            if normalize_incoming_array(&mut array, &items, unwrap_array) {
                let array = Rc::new(array);
                let cell = Rc::new(RefCell::new(current));
                let inner = cell.clone();
                create_render_effect(move || {
                    let prev = inner.borrow().clone();
                    let next = insert_expression(parent, View::List(array.clone()), prev, marker, true);
                    *inner.borrow_mut() = next;
                });
                return Current::Fn(cell);
            }
            let nodes: Vec<Element> = array
                .iter()
                .filter_map(|v| match v {
                    View::Node(n) => Some(*n),
                    _ => None,
                })
                .collect();
            if nodes.is_empty() {
                let replacement = clean_children(parent, current.clone(), marker, None);
                if multi {
                    return replacement;
                }
            } else {
                match &current {
                    Current::Nodes(existing) => {
                        if existing.is_empty() {
                            append_nodes(parent, &nodes, marker);
                        } else {
                            reconcile_arrays(parent, existing.clone(), &nodes);
                        }
                    }
                    Current::Undefined => append_nodes(parent, &nodes, At::End),
                    _ if get_first_child(parent).is_none() => append_nodes(parent, &nodes, At::End),
                    _ => {
                        let a = match (&current, multi) {
                            (Current::Nodes(n), true) => n.clone(),
                            _ => get_first_child(parent).into_iter().collect(),
                        };
                        reconcile_arrays(parent, a, &nodes);
                    }
                }
            }
            current = Current::Nodes(nodes);
        }
        View::Node(node) => {
            if let Current::Nodes(_) = &current {
                if multi {
                    return clean_children(parent, current, marker, Some(node));
                }
                clean_children(parent, current, At::End, Some(node));
            } else if matches!(current, Current::Undefined) || get_first_child(parent).is_none() {
                insert_node(parent, node, None);
            } else if let Some(first) = get_first_child(parent) {
                replace_node(parent, node, first);
            }
            current = Current::Node(node);
        }
    }
    current
}

/// `normalizeIncomingArray(normalized, array, unwrap)`: true when it holds functions (a dynamic array).
fn normalize_incoming_array(normalized: &mut Vec<View>, array: &[View], unwrap: bool) -> bool {
    let mut dynamic = false;
    for item in array {
        match item {
            View::Empty => {}
            View::List(inner) => dynamic = normalize_incoming_array(normalized, inner, false) || dynamic,
            View::Text(text) => normalized.push(View::Node(create_text_node(text))),
            View::Dyn(f) => {
                if unwrap {
                    let mut value = f();
                    while let View::Dyn(g) = value {
                        value = g();
                    }
                    let items = match value {
                        View::List(items) => items.as_ref().clone(),
                        other => vec![other],
                    };
                    dynamic = normalize_incoming_array(normalized, &items, false) || dynamic;
                } else {
                    normalized.push(item.clone());
                    dynamic = true;
                }
            }
            View::Node(_) => normalized.push(item.clone()),
        }
    }
    dynamic
}

/// `reconcileArrays(parentNode, a, b)`.
fn reconcile_arrays(parent: Element, mut a: Vec<Element>, b: &[Element]) {
    let b_length = b.len();
    let mut a_end = a.len();
    let mut b_end = b_length;
    let mut a_start = 0;
    let mut b_start = 0;
    let after = a.last().and_then(|last| get_next_sibling(*last));
    let mut map: Option<std::collections::HashMap<Element, usize>> = None;
    while a_start < a_end || b_start < b_end {
        if a_start < a_end && b_start < b_end && a[a_start] == b[b_start] {
            a_start += 1;
            b_start += 1;
            continue;
        }
        while a_end > 0 && b_end > 0 && a_end > a_start && b_end > b_start && a[a_end - 1] == b[b_end - 1] {
            a_end -= 1;
            b_end -= 1;
        }
        if a_end == a_start {
            let node = if b_end < b_length {
                if b_start > 0 { get_next_sibling(b[b_start - 1]) } else { Some(b[b_end - b_start]) }
            } else {
                after
            };
            while b_start < b_end {
                insert_node(parent, b[b_start], node);
                b_start += 1;
            }
        } else if b_end == b_start {
            while a_start < a_end {
                if map.as_ref().is_none_or(|m| !m.contains_key(&a[a_start])) {
                    remove_node(parent, a[a_start]);
                }
                a_start += 1;
            }
        } else if a[a_start] == b[b_end - 1] && b[b_start] == a[a_end - 1] {
            a_end -= 1;
            let node = get_next_sibling(a[a_end]);
            let anchor = get_next_sibling(a[a_start]);
            insert_node(parent, b[b_start], anchor);
            a_start += 1;
            b_start += 1;
            b_end -= 1;
            insert_node(parent, b[b_end], node);
            a[a_end] = b[b_end];
        } else {
            if map.is_none() {
                let mut m = std::collections::HashMap::new();
                for (i, item) in b.iter().enumerate().take(b_end).skip(b_start) {
                    m.insert(*item, i);
                }
                map = Some(m);
            }
            let index = map.as_ref().unwrap().get(&a[a_start]).copied();
            match index {
                Some(index) => {
                    if b_start < index && index < b_end {
                        let mut i = a_start;
                        let mut sequence = 1;
                        loop {
                            i += 1;
                            if !(i < a_end && i < b_end) {
                                break;
                            }
                            match map.as_ref().unwrap().get(&a[i]) {
                                Some(t) if *t == index + sequence => sequence += 1,
                                _ => break,
                            }
                        }
                        if sequence > index - b_start {
                            let node = a[a_start];
                            while b_start < index {
                                insert_node(parent, b[b_start], Some(node));
                                b_start += 1;
                            }
                        } else {
                            replace_node(parent, b[b_start], a[a_start]);
                            b_start += 1;
                            a_start += 1;
                        }
                    } else {
                        a_start += 1;
                    }
                }
                None => {
                    remove_node(parent, a[a_start]);
                    a_start += 1;
                }
            }
        }
    }
}

/// `cleanChildren(parent, current, marker, replacement)`.
fn clean_children(parent: Element, current: Current, marker: At, replacement: Option<Element>) -> Current {
    if !marker.multi() {
        while let Some(removed) = get_first_child(parent) {
            remove_node(parent, removed);
        }
        if let Some(node) = replacement {
            insert_node(parent, node, None);
            return Current::Node(node);
        }
        return Current::Str("".into());
    }
    let node = replacement.unwrap_or_else(|| create_slot_node(parent));
    let current_nodes = match current {
        Current::Nodes(nodes) => nodes,
        Current::Node(node) => vec![node],
        _ => Vec::new(),
    };
    if !current_nodes.is_empty() {
        let mut inserted = false;
        for i in (0..current_nodes.len()).rev() {
            let el = current_nodes[i];
            if node != el {
                let is_parent = get_parent_node(el) == Some(parent);
                if !inserted && i == 0 {
                    if is_parent {
                        replace_node(parent, node, el);
                    } else {
                        insert_node(parent, node, marker.anchor());
                    }
                } else if is_parent {
                    remove_node(parent, el);
                }
            } else {
                inserted = true;
            }
        }
    } else {
        insert_node(parent, node, marker.anchor());
    }
    Current::Nodes(vec![node])
}

fn append_nodes(parent: Element, array: &[Element], marker: At) {
    for node in array {
        insert_node(parent, *node, marker.anchor());
    }
}

fn replace_node(parent: Element, new_node: Element, old_node: Element) {
    insert_node(parent, new_node, Some(old_node));
    remove_node(parent, old_node);
}

/// `render(code, root)`: a root whose disposal is returned.
pub fn render(code: impl FnOnce() -> View, root: Element) -> crate::reactive::Dispose {
    crate::reactive::create_root(|dispose| {
        let view = code();
        insert(root, view, At::Only);
        dispose
    })
}

/// `<Portal mount>{children}</Portal>` (`@opentui/solid`'s `elements/extras.ts`): the children, created once, go
/// into a new `<box>` added to `mount` (the renderer's root by default) and removed when the owner is disposed;
/// the portal itself renders nothing where it stands.
pub fn portal(mount: Option<Element>, children: impl Fn() -> View + 'static) -> View {
    let children = Rc::new(children);
    let content: Rc<RefCell<Option<View>>> = Rc::new(RefCell::new(None));
    create_render_effect(move || {
        let Some(el) = mount.or_else(crate::tree::root) else { return };
        let view = {
            let mut slot = content.borrow_mut();
            if slot.is_none() {
                let children = children.clone();
                *slot = Some(untrack(move || children()));
            }
            slot.clone().unwrap_or_default()
        };
        let container = crate::elements::box_();
        insert(container, view, At::Only);
        el.add(container, None);
        crate::reactive::on_cleanup(move || remove_node(el, container));
    });
    View::Empty
}
