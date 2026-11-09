//! `routes/session/anchors.ts` and `anchor-view.tsx`: scroll anchors (mounted parts and group headers by identity)
//! and the components that register them.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::elements::box_;
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection};

use super::grouping::{Group, GroupNode, PartRef, SessionEntry, SessionNode};
use super::render_context::use_ctx;

/// `AnchorTarget`.
#[derive(Clone, Debug, PartialEq)]
pub enum AnchorTarget {
    Part(PartRef),
    Group(String),
}

/// A registered anchor: its target and node.
#[derive(Clone, Debug)]
pub struct Anchor {
    pub target: AnchorTarget,
    pub node: Element,
}

fn json_string(text: &str) -> String {
    opencode_tui_sync::client::stringify(&Json::String(text.to_string()))
}

/// `anchorKey(target)`.
pub fn anchor_key(target: &AnchorTarget) -> String {
    match target {
        AnchorTarget::Part(r) => format!("[\"part\",{},{}]", json_string(&r.message_id), json_string(&r.part_id)),
        AnchorTarget::Group(id) => format!("[\"group\",{}]", json_string(id)),
    }
}

/// `entryRef(entry)`: whole non-assistant messages have one canonical body part.
pub fn entry_ref(entry: &SessionEntry) -> Option<PartRef> {
    match entry {
        SessionEntry::Part { r#ref } => Some(r#ref.clone()),
        SessionEntry::Message { message_id } => Some(PartRef::new(message_id, "message")),
        _ => None,
    }
}

/// `groupID(node, level)`.
pub fn group_id(node: &Group<SessionEntry>, level: usize) -> Option<String> {
    let r = first_ref(&node.children)?;
    Some(format!("[{},{},{},{}]", json_string(&r.message_id), json_string(&r.part_id), json_string(node.kind.as_str()), level))
}

fn first_ref(nodes: &[SessionNode]) -> Option<PartRef> {
    for node in nodes {
        let r = match node {
            GroupNode::Entry(entry) => entry_ref(entry),
            GroupNode::Group(g) => first_ref(&g.children),
        };
        if r.is_some() {
            return r;
        }
    }
    None
}

/// `containsAnchor(node, target, level)` for an entry.
pub fn entry_contains_anchor(entry: &SessionEntry, target: &AnchorTarget) -> bool {
    match (entry_ref(entry), target) {
        (Some(r), AnchorTarget::Part(t)) => &r == t,
        _ => false,
    }
}

/// `containsAnchor(node, target, level)` for a group.
pub fn group_contains_anchor(node: &Group<SessionEntry>, target: &AnchorTarget, level: usize) -> bool {
    if let AnchorTarget::Group(id) = target {
        if group_id(node, level).as_deref() == Some(id.as_str()) {
            return true;
        }
    }
    node.children.iter().any(|child| match child {
        GroupNode::Entry(entry) => entry_contains_anchor(entry, target),
        GroupNode::Group(g) => group_contains_anchor(g, target, level + 1),
    })
}

fn shown(node: Element) -> bool {
    !node.is_destroyed() && node.height() > 0.0
}

/// `createTimelineAnchors()`.
#[derive(Clone, Default)]
pub struct TimelineAnchors {
    entries: Rc<RefCell<HashMap<String, (u64, Anchor)>>>,
    next: Rc<std::cell::Cell<u64>>,
}

impl TimelineAnchors {
    pub fn new() -> TimelineAnchors {
        TimelineAnchors::default()
    }

    /// `register(anchor)`: returns the unregister function.
    pub fn register(&self, anchor: Anchor) -> impl FnOnce() + 'static {
        let key = anchor_key(&anchor.target);
        let token = self.next.get() + 1;
        self.next.set(token);
        self.entries.borrow_mut().insert(key.clone(), (token, anchor));
        let entries = self.entries.clone();
        move || {
            let mut entries = entries.borrow_mut();
            if entries.get(&key).is_some_and(|(t, _)| *t == token) {
                entries.remove(&key);
            }
        }
    }

    /// Mounted anchors in screen order.
    pub fn list(&self) -> Vec<Anchor> {
        let mut list: Vec<Anchor> =
            self.entries.borrow().values().map(|(_, a)| a.clone()).filter(|a| shown(a.node)).collect();
        list.sort_by_key(|a| a.node.y());
        list
    }

    pub fn get(&self, target: &AnchorTarget) -> Option<Anchor> {
        self.entries.borrow().get(&anchor_key(target)).map(|(_, a)| a.clone()).filter(|a| shown(a.node))
    }

    pub fn for_message(&self, message_id: &str) -> Option<Anchor> {
        self.list().into_iter().find(|a| matches!(&a.target, AnchorTarget::Part(r) if r.message_id == message_id))
    }

    /// `messagePositions()`: the first anchor of each message, with its screen row.
    pub fn message_positions(&self) -> Vec<(String, i32)> {
        let mut seen = std::collections::HashSet::new();
        self.list()
            .into_iter()
            .filter_map(|a| match &a.target {
                AnchorTarget::Part(r) if seen.insert(r.message_id.clone()) => Some((r.message_id.clone(), a.node.y())),
                _ => None,
            })
            .collect()
    }
}

/// `visitEntries(nodes, visit)`.
pub fn visit_entries(nodes: &[SessionNode], visit: &mut dyn FnMut(&SessionEntry)) {
    for node in nodes {
        match node {
            GroupNode::Entry(entry) => visit(entry),
            GroupNode::Group(g) => visit_entries(&g.children, visit),
        }
    }
}

/// `useEntryAnchor({ entry, node })`.
pub fn use_entry_anchor(entry: impl Fn() -> Option<SessionEntry> + 'static, node: Signal<Option<Element>>) {
    let ctx = use_ctx();
    create_effect(move || {
        let entry = entry();
        let node = node.get();
        let r = entry.as_ref().and_then(entry_ref);
        let (Some(r), Some(node)) = (r, node) else { return };
        let unregister = ctx.anchors.register(Anchor { target: AnchorTarget::Part(r), node });
        let unregister = RefCell::new(Some(unregister));
        on_cleanup(move || {
            if let Some(f) = unregister.borrow_mut().take() {
                f()
            }
        });
    });
}

/// `<EntryAnchor entry marginTop>`.
pub fn entry_anchor(entry: SessionEntry, margin_top: Option<f32>, children: View) -> View {
    let (node, set_node) = create_signal(None::<Element>);
    let _ = node;
    let entry_for = entry.clone();
    use_entry_anchor(move || Some(entry_for.clone()), set_node);
    let el = box_();
    set_node.set(Some(el));
    if let Some(top) = margin_top {
        el.set_margin(Edge::Top, Some(Value::Point(top)));
    }
    el.set_flex_shrink(Some(0.0));
    insert(el, children, At::Only);
    View::Node(el)
}

/// `<GroupAnchor groupID active>`.
pub fn group_anchor(group_id: impl Fn() -> Option<String> + 'static, active: impl Fn() -> bool + 'static, children: View) -> View {
    let ctx = use_ctx();
    let (node, set_node) = create_signal(None::<Element>);
    create_effect(move || {
        let target = node.get();
        let id = group_id();
        let (Some(target), Some(id)) = (target, id) else { return };
        if !active() {
            return;
        }
        let unregister = ctx.anchors.register(Anchor { target: AnchorTarget::Group(id), node: target });
        let unregister = RefCell::new(Some(unregister));
        on_cleanup(move || {
            if let Some(f) = unregister.borrow_mut().take() {
                f()
            }
        });
    });
    let el = box_();
    set_node.set(Some(el));
    el.set_flex_direction(FlexDirection::Column);
    el.set_flex_shrink(Some(0.0));
    insert(el, children, At::Only);
    View::Node(el)
}
