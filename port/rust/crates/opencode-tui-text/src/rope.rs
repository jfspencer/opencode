//! Port of `rope.zig` (instantiated for [`Segment`]): a persistent balanced rope whose edits share every subtree
//! they do not touch, with undo/redo history as a list of old roots.
//!
//! Divergences: nodes are reference counted instead of arena allocated, so replaced text is freed as soon as no
//! root (current or in history) holds it. A discarded redo list is dropped instead of being kept on the undo node
//! that replaced it (nothing reads it). The balancing, split and join rules are the Zig ones, since leaf
//! boundaries are observable (grapheme joins at chunk edges, zero-width leaves at a split weight).

use crate::segment::{rewrite_boundary, rewrite_ends, SegMetrics, Segment};
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Metrics {
    pub count: u32,
    pub depth: u32,
    pub custom: SegMetrics,
}

impl Metrics {
    fn new() -> Self {
        Metrics { count: 0, depth: 1, custom: SegMetrics::default() }
    }
    fn add(&mut self, other: &Metrics) {
        self.count += other.count;
        self.depth = self.depth.max(other.depth);
        self.custom.add(&other.custom);
    }
    pub fn weight(&self) -> u32 {
        self.custom.weight()
    }
    fn size(&self) -> u64 {
        self.weight() as u64 + self.count as u64
    }
}

pub(crate) struct Branch {
    pub left: Rc<Node>,
    pub right: Rc<Node>,
    metrics: Cell<Metrics>,
}

pub(crate) enum Node {
    Branch(Branch),
    Leaf { data: Segment, sentinel: bool },
}

pub(crate) type NodeRef = Rc<Node>;

impl Node {
    pub fn metrics(&self) -> Metrics {
        match self {
            Node::Branch(b) => b.metrics.get(),
            Node::Leaf { data, sentinel } => Metrics { count: if *sentinel { 0 } else { 1 }, depth: 1, custom: data.measure() },
        }
    }

    pub fn count(&self) -> u32 {
        match self {
            Node::Branch(b) => b.metrics.get().count,
            Node::Leaf { sentinel, .. } => u32::from(!*sentinel),
        }
    }

    pub fn weight(&self) -> u32 {
        self.metrics().weight()
    }

    fn branch(left: NodeRef, right: NodeRef) -> NodeRef {
        let mut metrics = Metrics::new();
        metrics.add(&left.metrics());
        metrics.add(&right.metrics());
        metrics.depth += 1;
        Rc::new(Node::Branch(Branch { left, right, metrics: Cell::new(metrics) }))
    }

    fn leaf(data: Segment) -> NodeRef {
        Rc::new(Node::Leaf { data, sentinel: false })
    }

    pub fn get(&self, index: u32) -> Option<&Segment> {
        let mut node = self;
        let mut index = index;
        loop {
            match node {
                Node::Branch(b) => {
                    let left_count = b.left.count();
                    if index < left_count {
                        node = &b.left;
                    } else {
                        index -= left_count;
                        node = &b.right;
                    }
                }
                Node::Leaf { data, .. } => return if index == 0 { Some(data) } else { None },
            }
        }
    }

    fn first_leaf(&self) -> Option<&Segment> {
        match self {
            Node::Branch(b) => b.left.first_leaf(),
            Node::Leaf { data, sentinel } => (!*sentinel).then_some(data),
        }
    }

    fn last_leaf(&self) -> Option<&Segment> {
        match self {
            Node::Branch(b) => b.right.last_leaf(),
            Node::Leaf { data, sentinel } => (!*sentinel).then_some(data),
        }
    }

    fn remeasure(&self) -> Metrics {
        match self {
            Node::Leaf { .. } => self.metrics(),
            Node::Branch(b) => {
                let mut metrics = Metrics::new();
                metrics.add(&b.left.remeasure());
                metrics.add(&b.right.remeasure());
                metrics.depth += 1;
                b.metrics.set(metrics);
                metrics
            }
        }
    }

    /// As `walk_from`, passing the leaf nodes.
    fn walk_leaves_from(self: &Rc<Self>, start: u32, index: &mut u32, f: &mut dyn FnMut(&NodeRef, u32) -> bool) -> bool {
        match &**self {
            Node::Branch(b) => {
                let left_count = b.left.count();
                if start >= left_count {
                    return b.right.walk_leaves_from(start - left_count, index, f);
                }
                if !b.left.walk_leaves_from(start, index, f) {
                    return false;
                }
                b.right.walk_leaves_from(0, index, f)
            }
            Node::Leaf { sentinel, .. } => {
                if *sentinel || start != 0 {
                    return true;
                }
                let keep = f(self, *index);
                *index += 1;
                keep
            }
        }
    }

    /// The segment of a leaf node.
    pub fn segment(&self) -> Option<&Segment> {
        match self {
            Node::Leaf { data, .. } => Some(data),
            Node::Branch(_) => None,
        }
    }

    /// Calls `f` on each counted leaf from `start` (leaf index) with its index relative to `start`; stops when
    /// `f` returns false. Returns false if stopped.
    fn walk_from(&self, start: u32, index: &mut u32, f: &mut dyn FnMut(&Segment, u32) -> bool) -> bool {
        match self {
            Node::Branch(b) => {
                let left_count = b.left.count();
                if start >= left_count {
                    return b.right.walk_from(start - left_count, index, f);
                }
                if !b.left.walk_from(start, index, f) {
                    return false;
                }
                b.right.walk_from(0, index, f)
            }
            Node::Leaf { data, sentinel } => {
                if *sentinel || start != 0 {
                    return true;
                }
                let keep = f(data, *index);
                *index += 1;
                keep
            }
        }
    }
}

fn is_balanced(left: &Node, right: &Node) -> bool {
    let left_size = left.metrics().size();
    let right_size = right.metrics().size();
    let max_side = ((left_size + right_size) * 3) / 4;
    left_size <= max_side && right_size <= max_side
}

/// `join_balanced`.
pub(crate) fn join(left: NodeRef, right: NodeRef) -> NodeRef {
    if left.count() == 0 {
        return right;
    }
    if right.count() == 0 {
        return left;
    }
    if is_balanced(&left, &right) {
        return Node::branch(left, right);
    }
    let left_size = left.metrics().size();
    let right_size = right.metrics().size();
    if left_size > right_size * 3 {
        let Node::Branch(b) = &*left else { return Node::branch(left, right) };
        let new_right = join(b.right.clone(), right);
        if new_right.metrics().size() > b.left.metrics().size() * 3
            && let Node::Branch(nr) = &*new_right
        {
            let new_left = join(b.left.clone(), nr.left.clone());
            return Node::branch(new_left, nr.right.clone());
        }
        return Node::branch(b.left.clone(), new_right);
    }
    let Node::Branch(b) = &*right else { return Node::branch(left, right) };
    let new_left = join(left, b.left.clone());
    if new_left.metrics().size() > b.right.metrics().size() * 3
        && let Node::Branch(nl) = &*new_left
    {
        let new_right = join(nl.right.clone(), b.right.clone());
        return Node::branch(nl.left.clone(), new_right);
    }
    Node::branch(new_left, b.right.clone())
}

/// `rootFromSlice`: a balanced tree over `items`, split at the middle.
pub(crate) fn from_slice(items: &[Segment]) -> NodeRef {
    debug_assert!(!items.is_empty());
    if items.len() == 1 {
        return Node::leaf(items[0].clone());
    }
    let mid = items.len() / 2;
    Node::branch(from_slice(&items[..mid]), from_slice(&items[mid..]))
}

/// The side that takes the zero-weight leaves at a split weight (`Bias`); `None` leaves it to the tree shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Bias {
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RopeError {
    OutOfBounds,
    OutOfMemory,
}

/// Leaves that replace two neighbors (`LeafJoin`).
pub(crate) struct LeafJoin {
    pub left: Option<Segment>,
    pub joined: Segment,
    pub right: Option<Segment>,
    /// The weights, within `joined`, of the grapheme that crosses the old boundary.
    pub span: Option<[u32; 2]>,
    /// Check the last of the new leaves against the next leaf.
    pub more: bool,
}

/// The leaf split and join callbacks of weight edits (`LeafSplitFn`).
pub(crate) trait LeafOps {
    fn split(&mut self, leaf: &Segment, weight: u32) -> Result<(Segment, Segment), RopeError>;
    fn join(&mut self, left: &Segment, right: &Segment) -> Result<Option<LeafJoin>, RopeError>;
}

/// The weights, in a joined rope, of the grapheme that holds the junction (`JoinSpan`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct JoinSpan {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Position {
    pub leaf_index: u32,
    pub weight: u32,
}

/// A rope's tree: its root and the shared empty sentinel.
#[derive(Clone)]
pub(crate) struct Tree {
    pub root: NodeRef,
    pub empty: NodeRef,
}

impl Tree {
    /// `Rope.init`: an empty document (one line-start marker).
    pub fn new() -> Tree {
        let empty = Rc::new(Node::Leaf { data: Segment::empty(), sentinel: true });
        let mut tree = Tree { root: empty.clone(), empty };
        tree.apply_ends_invariant();
        tree
    }

    pub fn from_segments(items: &[Segment]) -> Tree {
        if items.is_empty() {
            return Tree::new();
        }
        let empty = Rc::new(Node::Leaf { data: Segment::empty(), sentinel: true });
        Tree { root: from_slice(items), empty }
    }

    pub fn metrics(&self) -> Metrics {
        self.root.metrics()
    }

    pub fn count(&self) -> u32 {
        self.root.count()
    }

    pub fn total_weight(&self) -> u32 {
        self.root.weight()
    }

    pub fn get(&self, index: u32) -> Option<&Segment> {
        if index >= self.count() {
            return None;
        }
        self.root.get(index)
    }

    pub fn walk(&self, f: &mut dyn FnMut(&Segment, u32) -> bool) {
        let mut index = 0;
        self.root.walk_from(0, &mut index, f);
    }

    pub fn walk_from(&self, start: u32, f: &mut dyn FnMut(&Segment, u32) -> bool) {
        let mut index = 0;
        self.root.walk_from(start, &mut index, f);
    }

    pub fn walk_leaves_from(&self, start: u32, f: &mut dyn FnMut(&NodeRef, u32) -> bool) {
        let mut index = 0;
        self.root.walk_leaves_from(start, &mut index, f);
    }

    /// A leaf node holding `segment`, outside any tree (the truncation ellipsis).
    pub fn detached_leaf(segment: Segment) -> NodeRef {
        Node::leaf(segment)
    }

    pub fn to_vec(&self) -> Vec<Segment> {
        let mut out = Vec::with_capacity(self.count() as usize);
        self.walk(&mut |segment, _| {
            out.push(segment.clone());
            true
        });
        out
    }

    pub fn remeasure(&self) {
        self.root.remeasure();
    }

    fn split_at(&self, node: &NodeRef, index: u32) -> (NodeRef, NodeRef) {
        if index == 0 {
            return (self.empty.clone(), node.clone());
        }
        if index >= node.count() {
            return (node.clone(), self.empty.clone());
        }
        let Node::Branch(b) = &**node else { unreachable!() };
        let left_count = b.left.count();
        if index < left_count {
            let (l, r) = self.split_at(&b.left, index);
            (l, join(r, b.right.clone()))
        } else if index > left_count {
            let (l, r) = self.split_at(&b.right, index - left_count);
            (join(b.left.clone(), l), r)
        } else {
            (b.left.clone(), b.right.clone())
        }
    }

    fn split_at_weight(
        &self,
        node: &NodeRef,
        target: u32,
        bias: Option<Bias>,
        ops: &mut dyn LeafOps,
    ) -> Result<(NodeRef, NodeRef), RopeError> {
        match &**node {
            Node::Leaf { data, .. } => {
                let leaf_weight = node.weight();
                if target == 0 && (leaf_weight > 0 || bias != Some(Bias::Left)) {
                    return Ok((self.empty.clone(), node.clone()));
                } else if target >= leaf_weight {
                    return Ok((node.clone(), self.empty.clone()));
                }
                let (left, right) = ops.split(data, target)?;
                debug_assert!(!right.is_void());
                let left = if left.is_void() { self.empty.clone() } else { Node::leaf(left) };
                Ok((left, Node::leaf(right)))
            }
            Node::Branch(b) => {
                let left_weight = b.left.weight();
                if target < left_weight || (target == left_weight && bias == Some(Bias::Right)) {
                    let (l, r) = self.split_at_weight(&b.left, target, bias, ops)?;
                    Ok((l, join(r, b.right.clone())))
                } else if target > left_weight || bias == Some(Bias::Left) {
                    let (l, r) = self.split_at_weight(&b.right, target - left_weight, bias, ops)?;
                    Ok((join(b.left.clone(), l), r))
                } else {
                    Ok((b.left.clone(), b.right.clone()))
                }
            }
        }
    }

    fn drop_first(&self, node: NodeRef) -> NodeRef {
        if node.count() == 0 {
            return node;
        }
        self.split_at(&node, 1).1
    }

    fn drop_last(&self, node: NodeRef) -> NodeRef {
        let count = node.count();
        if count == 0 {
            return node;
        }
        self.split_at(&node, count - 1).0
    }

    /// `joinWithBoundary`: joins under the segment boundary rules, then lets `ops` join or reshape the leaves
    /// that meet. `span` receives the grapheme that holds the junction.
    fn join_with_boundary(
        &self,
        left: NodeRef,
        right: NodeRef,
        ops: Option<&mut dyn LeafOps>,
        span: Option<&mut JoinSpan>,
    ) -> Result<NodeRef, RopeError> {
        let mut span = span;
        if let Some(held) = span.as_deref_mut() {
            *held = JoinSpan { start: left.weight(), end: left.weight() };
        }
        let action = rewrite_boundary(left.last_leaf(), right.first_leaf());
        let mut l = left;
        let mut r = right;
        if action.delete_left {
            l = self.drop_last(l);
        }
        if action.delete_right {
            r = self.drop_first(r);
        }
        if action.insert_linestart {
            return Ok(join(join(l, Node::leaf(Segment::LineStart)), r));
        }
        let Some(ops) = ops else { return Ok(join(l, r)) };
        let mut open = span.is_some();
        let mut held = false;
        loop {
            let (Some(last), Some(first)) = (l.last_leaf(), r.first_leaf()) else { break };
            let Some(joined) = ops.join(last, first)? else { break };
            l = self.drop_last(l);
            r = self.drop_first(r);
            let LeafJoin { left: jl, joined: jj, right: jr, span: within, more } = joined;
            let right_none = jr.is_none();
            if let Some(leaf) = jl {
                l = join(l, Node::leaf(leaf));
            }
            if open
                && let Some(within) = within
                && let Some(s) = span.as_deref_mut()
            {
                let base = l.weight();
                if !held {
                    s.start = base + within[0];
                }
                s.end = base + within[1];
                held = true;
            }
            l = join(l, Node::leaf(jj));
            if let Some(leaf) = jr {
                l = join(l, Node::leaf(leaf));
            }
            open = open && within.is_some() && right_none && span.as_deref().is_some_and(|s| s.end == l.weight());
            if !more {
                break;
            }
        }
        Ok(join(l, r))
    }

    fn apply_ends_invariant(&mut self) {
        let first = if self.count() == 0 { None } else { self.root.get(0) };
        if rewrite_ends(first) {
            self.root = join(Node::leaf(Segment::LineStart), self.root.clone());
        }
    }

    pub fn insert_slice(&mut self, index: u32, items: &[Segment]) {
        if items.is_empty() {
            return;
        }
        let insert = from_slice(items);
        let (l, r) = self.split_at(&self.root.clone(), index);
        let joined = self.join_with_boundary(l, insert, None, None).expect("no ops");
        self.root = self.join_with_boundary(joined, r, None, None).expect("no ops");
        self.apply_ends_invariant();
    }

    pub fn delete_range(&mut self, start: u32, end: u32) {
        if start >= end {
            return;
        }
        let (l, rest) = self.split_at(&self.root.clone(), start);
        let (_, r) = self.split_at(&rest, end - start);
        self.root = self.join_with_boundary(l, r, None, None).expect("no ops");
        self.apply_ends_invariant();
    }

    pub fn replace(&mut self, index: u32, data: Segment) {
        if index >= self.count() {
            return;
        }
        self.delete_range(index, index + 1);
        self.insert_slice(index, &[data]);
    }

    pub fn delete(&mut self, index: u32) {
        self.delete_range(index, index + 1);
    }

    pub fn append(&mut self, data: Segment) {
        let count = self.count();
        self.insert_slice(count, &[data]);
    }

    /// `replaceRangeByWeight`: `items` in place of the weight range; returns the grapheme that holds the end.
    pub fn replace_range_by_weight(
        &mut self,
        start: u32,
        end: u32,
        start_bias: Bias,
        end_bias: Bias,
        items: &[Segment],
        ops: &mut dyn LeafOps,
    ) -> Result<JoinSpan, RopeError> {
        debug_assert!(start <= end);
        let (first_left, first_right) = self.split_at_weight(&self.root.clone(), start, Some(start_bias), ops)?;
        let (_, second_right) = self.split_at_weight(&first_right, end - start, Some(end_bias), ops)?;
        let left = if items.is_empty() {
            first_left
        } else {
            self.join_with_boundary(first_left, from_slice(items), Some(&mut *ops), None)?
        };
        let mut span = JoinSpan::default();
        self.root = self.join_with_boundary(left, second_right, Some(ops), Some(&mut span))?;
        self.apply_ends_invariant();
        Ok(span)
    }

    /// `insertSliceByWeight`: returns the grapheme that holds the end of `items`.
    pub fn insert_slice_by_weight(
        &mut self,
        weight: u32,
        bias: Bias,
        items: &[Segment],
        ops: &mut dyn LeafOps,
    ) -> Result<JoinSpan, RopeError> {
        if items.is_empty() {
            return Ok(JoinSpan { start: weight, end: weight });
        }
        let insert = from_slice(items);
        let (l, r) = self.split_at_weight(&self.root.clone(), weight, Some(bias), ops)?;
        let joined = self.join_with_boundary(l, insert, Some(&mut *ops), None)?;
        let mut span = JoinSpan::default();
        self.root = self.join_with_boundary(joined, r, Some(ops), Some(&mut span))?;
        self.apply_ends_invariant();
        Ok(span)
    }

    /// `findByWeight`: the leaf that holds `weight`, its start weight and index.
    pub fn find_by_weight(&self, weight: u32) -> Option<(&Segment, u32, u32)> {
        if weight >= self.total_weight() {
            return None;
        }
        let mut node: &Node = &self.root;
        let mut start_weight = 0;
        let mut leaf_index = 0;
        loop {
            match node {
                Node::Branch(b) => {
                    let left_weight = b.left.weight();
                    if weight < start_weight + left_weight {
                        node = &b.left;
                    } else {
                        start_weight += left_weight;
                        leaf_index += b.left.count();
                        node = &b.right;
                    }
                }
                Node::Leaf { data, .. } => return Some((data, start_weight, leaf_index)),
            }
        }
    }

    /// `seekCount`: the leaf that holds occurrence `n` of the leaves `counted` counts.
    pub fn seek_count(&self, counted: fn(&Metrics) -> u32, n: u32) -> Option<Position> {
        if n >= counted(&self.root.metrics()) {
            return None;
        }
        let mut node: &Node = &self.root;
        let mut rest = n;
        let mut position = Position { leaf_index: 0, weight: 0 };
        while let Node::Branch(b) = node {
            let left = b.left.metrics();
            if rest < counted(&left) {
                node = &b.left;
            } else {
                rest -= counted(&left);
                position.leaf_index += left.count;
                position.weight += left.weight();
                node = &b.right;
            }
        }
        Some(position)
    }

    /// `countThrough`: leaves `counted` counts that start at or before `target` along `key`.
    pub fn count_through(&self, key: fn(&Metrics) -> u32, counted: fn(&Metrics) -> u32, target: u32) -> u32 {
        let mut node: &Node = &self.root;
        let mut rest = target;
        let mut total = 0;
        while let Node::Branch(b) = node {
            let left = b.left.metrics();
            if rest < key(&left) {
                node = &b.left;
            } else {
                rest -= key(&left);
                total += counted(&left);
                node = &b.right;
            }
        }
        total + counted(&node.metrics())
    }

    /// The empty document of `clearWithUndo`.
    pub fn cleared(&self) -> Tree {
        let mut tree = Tree { root: self.empty.clone(), empty: self.empty.clone() };
        tree.apply_ends_invariant();
        tree
    }
}

struct UndoNode {
    root: NodeRef,
    metrics_generation: u64,
    next: Option<Box<UndoNode>>,
    meta: String,
}

/// A rope with undo history (`Rope(Segment)`).
pub(crate) struct Rope {
    pub tree: Tree,
    metrics_generation: u64,
    undo_history: Option<Box<UndoNode>>,
    redo_history: Option<Box<UndoNode>>,
    curr_history: Option<Box<UndoNode>>,
    undo_depth: usize,
    max_undo_depth: Option<usize>,
}

impl Rope {
    pub fn new() -> Rope {
        Rope::with_tree(Tree::new())
    }

    pub fn with_tree(tree: Tree) -> Rope {
        Rope {
            tree,
            metrics_generation: 0,
            undo_history: None,
            redo_history: None,
            curr_history: None,
            undo_depth: 0,
            max_undo_depth: None,
        }
    }

    pub fn root(&self) -> &NodeRef {
        &self.tree.root
    }

    pub fn metrics_generation(&self) -> u64 {
        self.metrics_generation
    }

    pub fn set_metrics_generation(&mut self, generation: u64) {
        self.metrics_generation = generation;
        if let Some(current) = self.curr_history.as_mut()
            && Rc::ptr_eq(&current.root, &self.tree.root)
        {
            current.metrics_generation = generation;
        }
    }

    pub fn store_undo(&mut self, meta: &str) {
        let node = Box::new(UndoNode {
            root: self.tree.root.clone(),
            metrics_generation: self.metrics_generation,
            next: None,
            meta: meta.to_owned(),
        });
        self.push_undo(node);
        self.curr_history = None;
        self.redo_history = None;
    }

    fn push_undo(&mut self, mut node: Box<UndoNode>) {
        node.next = self.undo_history.take();
        self.undo_history = Some(node);
        self.undo_depth += 1;
        if let Some(max) = self.max_undo_depth
            && self.undo_depth > max
        {
            self.trim_undo_history(max);
        }
    }

    fn trim_undo_history(&mut self, max: usize) {
        let mut depth = 0;
        let mut cursor = &mut self.undo_history;
        while let Some(node) = cursor {
            depth += 1;
            if depth >= max {
                node.next = None;
                self.undo_depth = max;
                return;
            }
            cursor = &mut node.next;
        }
    }

    /// Restores the previous root; `meta` describes the state being left. Returns the restored step's metadata.
    pub fn undo(&mut self, meta: &str) -> Option<String> {
        let mut h = self.undo_history.take()?;
        self.undo_history = h.next.take();
        let mut r = self.curr_history.take().unwrap_or_else(|| {
            Box::new(UndoNode {
                root: self.tree.root.clone(),
                metrics_generation: self.metrics_generation,
                next: None,
                meta: meta.to_owned(),
            })
        });
        self.tree.root = h.root.clone();
        self.metrics_generation = h.metrics_generation;
        let restored = h.meta.clone();
        self.curr_history = Some(h);
        r.next = self.redo_history.take();
        self.redo_history = Some(r);
        if self.undo_depth > 0 {
            self.undo_depth -= 1;
        }
        Some(restored)
    }

    pub fn redo(&mut self) -> Option<String> {
        let current = self.curr_history.as_ref()?;
        self.redo_history.as_ref()?;
        if !Rc::ptr_eq(&current.root, &self.tree.root) {
            return None;
        }
        let u = self.curr_history.take().unwrap();
        let mut h = self.redo_history.take().unwrap();
        self.redo_history = h.next.take();
        self.tree.root = h.root.clone();
        self.metrics_generation = h.metrics_generation;
        let restored = h.meta.clone();
        self.curr_history = Some(h);
        self.push_undo(u);
        Some(restored)
    }

    pub fn can_undo(&self) -> bool {
        self.undo_history.is_some()
    }

    pub fn can_redo(&self) -> bool {
        self.redo_history.is_some() && self.curr_history.is_some()
    }

    /// Whether the redo branch is usable: `curr_history` holds the live root.
    pub fn curr_is_root(&self) -> bool {
        self.curr_history.as_ref().is_some_and(|c| Rc::ptr_eq(&c.root, &self.tree.root))
    }

    pub fn has_history(&self) -> bool {
        self.undo_history.is_some() || self.redo_history.is_some() || self.curr_history.is_some()
    }

    pub fn clear_history(&mut self) {
        self.undo_history = None;
        self.redo_history = None;
        self.curr_history = None;
        self.undo_depth = 0;
    }

    /// `clearWithUndo`.
    pub fn clear_with_undo(&mut self, meta: Option<&str>) {
        let empty = self.tree.cleared();
        if let Some(meta) = meta {
            self.store_undo(meta);
        }
        self.tree.root = empty.root;
    }

    /// `setSegments`: replace the content (without the end invariant, which `items` already keeps).
    pub fn set_segments(&mut self, items: &[Segment]) {
        if items.is_empty() {
            self.clear_with_undo(None);
            return;
        }
        self.tree.root = from_slice(items);
    }
}

impl Drop for Rope {
    fn drop(&mut self) {
        // Long history lists drop iteratively.
        for head in [&mut self.undo_history, &mut self.redo_history] {
            let mut next = head.take();
            while let Some(mut node) = next {
                next = node.next.take();
            }
        }
    }
}
