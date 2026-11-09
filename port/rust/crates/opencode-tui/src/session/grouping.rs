//! `routes/session/grouping/session.ts` and `grouping/tree.ts`: transcript entries, their grouping paths, and the
//! group tree (build, merge at a seam, split at a leaf offset).
//!
//! The tree is plain data: TS shares untouched subtrees by object identity, which only spares allocations; the
//! rows store reconciles by key either way.
use opencode_protocol::Json;
use opencode_tui_sync::json::JsonExt;

/// `PartRef`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PartRef {
    pub message_id: String,
    pub part_id: String,
}

impl PartRef {
    pub fn new(message_id: &str, part_id: &str) -> PartRef {
        PartRef { message_id: message_id.to_string(), part_id: part_id.to_string() }
    }
}

/// A model reference (`ModelRef`: providerID, id, variant).
#[derive(Clone, Debug, PartialEq)]
pub struct ModelRef {
    pub provider_id: String,
    pub id: String,
    pub variant: Option<String>,
}

impl ModelRef {
    pub fn of(value: Option<&Json>) -> ModelRef {
        ModelRef {
            provider_id: value.and_then(|m| m.str("providerID")).unwrap_or_default().to_string(),
            id: value.and_then(|m| m.str("id")).unwrap_or_default().to_string(),
            variant: value.and_then(|m| m.str("variant")).map(str::to_string),
        }
    }
}

/// `CacheUsage`.
#[derive(Clone, Debug, PartialEq)]
pub struct CacheUsage {
    pub read: f64,
    pub model: ModelRef,
}

/// `SessionEntry`.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionEntry {
    Message { message_id: String },
    CompactionQueued { inbox_id: String },
    Part { r#ref: PartRef },
    AssistantFooter { message_id: String },
    TurnUsage { message_ids: Vec<String>, previous_cache: Option<CacheUsage> },
}

/// `GroupKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GroupKind {
    Activity,
    Reasoning,
    Exploration,
    Instructions,
}

impl GroupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GroupKind::Activity => "activity",
            GroupKind::Reasoning => "reasoning",
            GroupKind::Exploration => "exploration",
            GroupKind::Instructions => "instructions",
        }
    }
}

/// `GroupNode<Entry, Kind>`.
#[derive(Clone, Debug, PartialEq)]
pub enum GroupNode<E> {
    Entry(E),
    Group(Group<E>),
}

/// A group node: its kind, children, and the number of descendant leaves.
#[derive(Clone, Debug, PartialEq)]
pub struct Group<E> {
    pub kind: GroupKind,
    pub children: Vec<GroupNode<E>>,
    pub size: usize,
}

impl<E> GroupNode<E> {
    pub fn size(&self) -> usize {
        match self {
            GroupNode::Entry(_) => 1,
            GroupNode::Group(g) => g.size,
        }
    }
}

/// `SessionNode`.
pub type SessionNode = GroupNode<SessionEntry>;

/// `SessionGroup`: a top-level group row. `pending` is kept for exploration and activity groups only.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionGroup {
    pub kind: GroupKind,
    pub children: Vec<SessionNode>,
    pub size: usize,
    pub completed: bool,
    pub pending: Vec<PartRef>,
}

impl SessionGroup {
    /// The group as a tree node.
    pub fn node(&self) -> Group<SessionEntry> {
        Group { kind: self.kind, children: self.children.clone(), size: self.size }
    }

    pub fn has_pending(&self) -> bool {
        matches!(self.kind, GroupKind::Exploration | GroupKind::Activity)
    }
}

/// `Verbosity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verbosity {
    Low,
    Medium,
    High,
}

impl Verbosity {
    pub fn parse(value: Option<&str>) -> Verbosity {
        match value {
            Some("low") => Verbosity::Low,
            Some("high") => Verbosity::High,
            _ => DEFAULT_VERBOSITY,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Verbosity::Low => "low",
            Verbosity::Medium => "medium",
            Verbosity::High => "high",
        }
    }
}

pub const DEFAULT_VERBOSITY: Verbosity = Verbosity::Medium;

/// `SessionRow`.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionRow {
    Entry(SessionEntry),
    Group(SessionGroup),
}

/// `AppendPart`.
#[derive(Clone, Debug, PartialEq)]
pub enum AppendPart {
    Text,
    Reasoning { completed: Option<f64> },
    Tool { name: String },
}

/// `ProjectionEntry`.
#[derive(Clone, Debug)]
pub struct ProjectionEntry {
    pub entry: SessionEntry,
    pub part: Option<AppendPart>,
    pub path: Vec<GroupKind>,
    /// `closesPrevious` (`undefined` counts as true where TS compares `!== false`).
    pub closes_previous: Option<bool>,
}

/// `projectEntries(entries)`: a fresh history batch in one pass.
pub fn project_entries(entries: Vec<ProjectionEntry>) -> Vec<SessionRow> {
    let nodes = group_entries(entries, |item| item.path.clone());
    let count = nodes.len();
    let mut rows = Vec::with_capacity(count);
    let mut iter = nodes.into_iter().peekable();
    while let Some(node) = iter.next() {
        match node {
            GroupNode::Entry(item) => rows.push(SessionRow::Entry(item.entry)),
            GroupNode::Group(group) => {
                let next_closes = match iter.peek() {
                    Some(GroupNode::Group(_)) => true,
                    Some(GroupNode::Entry(next)) => next.closes_previous != Some(false),
                    None => false,
                };
                let completed = next_closes
                    || (group.kind == GroupKind::Reasoning
                        && group.children.iter().all(|child| match child {
                            GroupNode::Entry(item) => {
                                matches!(item.part, Some(AppendPart::Reasoning { completed: Some(_) }))
                            }
                            GroupNode::Group(_) => false,
                        }));
                let node = Group { kind: group.kind, size: group.size, children: group.children.into_iter().map(unwrap).collect() };
                rows.push(SessionRow::Group(session_group(node, completed)));
            }
        }
    }
    rows
}

fn session_group(node: Group<SessionEntry>, completed: bool) -> SessionGroup {
    SessionGroup { kind: node.kind, children: node.children, size: node.size, completed, pending: Vec::new() }
}

fn unwrap(node: GroupNode<ProjectionEntry>) -> SessionNode {
    match node {
        GroupNode::Entry(item) => GroupNode::Entry(item.entry),
        GroupNode::Group(g) => GroupNode::Group(Group { kind: g.kind, size: g.size, children: g.children.into_iter().map(unwrap).collect() }),
    }
}

const EXPLORATION_TOOLS: [&str; 5] = ["read", "glob", "grep", "webfetch", "websearch"];

/// `partPath(part, verbosity)`.
pub fn part_path(part: &AppendPart, verbosity: Verbosity) -> Vec<GroupKind> {
    if let AppendPart::Tool { name } = part {
        if name.to_lowercase() == "question" {
            return Vec::new();
        }
    }
    let mut path = if verbosity == Verbosity::Low && !matches!(part, AppendPart::Text) {
        vec![GroupKind::Activity]
    } else {
        Vec::new()
    };
    match part {
        AppendPart::Reasoning { .. } => path.push(GroupKind::Reasoning),
        AppendPart::Tool { name } if EXPLORATION_TOOLS.contains(&name.to_lowercase().as_str()) => {
            path.push(GroupKind::Exploration)
        }
        _ => {}
    }
    path
}

/// `messagePath(message, verbosity)`.
pub fn message_path(message: &Json, verbosity: Verbosity) -> Vec<GroupKind> {
    if instruction_paths(Some(message)).is_empty() {
        return Vec::new();
    }
    if verbosity == Verbosity::Low {
        vec![GroupKind::Activity, GroupKind::Instructions]
    } else {
        vec![GroupKind::Instructions]
    }
}

/// `instructionPaths(message)`.
pub fn instruction_paths(message: Option<&Json>) -> Vec<String> {
    let Some(message) = message.filter(|m| m.str("type") == Some("synthetic")) else { return Vec::new() };
    let Some(instruction @ Json::Object(_)) = message.get("metadata").and_then(|m| m.get("instruction")) else {
        return Vec::new();
    };
    match instruction.get("paths") {
        Some(Json::Array(paths)) => paths.iter().filter_map(|p| match p { Json::String(s) => Some(s.clone()), _ => None }).collect(),
        _ => Vec::new(),
    }
}

/// `append(rows, ref, part, index, verbosity)`.
pub fn append(rows: &mut Vec<SessionRow>, r#ref: PartRef, part: &AppendPart, index: usize, verbosity: Verbosity) {
    let path = part_path(part, verbosity);
    let mut nodes = group_entries(vec![SessionEntry::Part { r#ref }], |_| path.clone());
    let node = nodes.remove(0);
    let group = match node {
        GroupNode::Entry(entry) => {
            complete_previous(rows, index);
            rows.insert(index, SessionRow::Entry(entry));
            return;
        }
        GroupNode::Group(group) => group,
    };
    if index > 0 {
        if let Some(SessionRow::Group(previous)) = rows.get_mut(index - 1) {
            if previous.kind == group.kind {
                // Permission-blocked tools remain at the end, just as the former refs/pending partition did.
                // Inserting a new ref must precede those blocked tools.
                let pending = if previous.kind == GroupKind::Exploration { previous.pending.len() } else { 0 };
                let whole = vec![GroupNode::Group(previous.node())];
                let (left, right) = split_groups(&whole, previous.size - pending);
                let merged = merge_groups(merge_groups(left, vec![GroupNode::Group(group)]), right);
                let Some(GroupNode::Group(merged)) = merged.into_iter().next() else {
                    panic!("Expected merged session group");
                };
                previous.children = merged.children;
                previous.size = merged.size;
                if let AppendPart::Reasoning { completed } = part {
                    previous.completed = previous.completed && completed.is_some();
                }
                return;
            }
        }
    }
    complete_previous(rows, index);
    let completed = group.kind == GroupKind::Reasoning && matches!(part, AppendPart::Reasoning { completed: Some(_) });
    rows.insert(index, SessionRow::Group(session_group(group, completed)));
}

/// `completePrevious(rows, index)`.
pub fn complete_previous(rows: &mut [SessionRow], index: usize) {
    if index > 0 {
        if let Some(SessionRow::Group(previous)) = rows.get_mut(index - 1) {
            previous.completed = true;
        }
    }
}

/// `groupRefs(row, includePending)`.
pub fn group_refs(row: &SessionGroup, include_pending: bool) -> Vec<PartRef> {
    let pending: &[PartRef] = if !include_pending && row.has_pending() { &row.pending } else { &[] };
    let mut out = Vec::new();
    fn visit(nodes: &[SessionNode], pending: &[PartRef], out: &mut Vec<PartRef>) {
        for node in nodes {
            match node {
                GroupNode::Group(g) => visit(&g.children, pending, out),
                GroupNode::Entry(SessionEntry::Part { r#ref }) => {
                    if !pending.contains(r#ref) {
                        out.push(r#ref.clone());
                    }
                }
                GroupNode::Entry(_) => {}
            }
        }
    }
    visit(&row.children, pending, &mut out);
    out
}

/// `partitionPending(rows, pending)`.
pub fn partition_pending(rows: &mut [SessionRow], pending: &std::collections::HashSet<String>) {
    for row in rows.iter_mut() {
        let SessionRow::Group(row) = row else { continue };
        if !row.has_pending() {
            continue;
        }
        if row.kind == GroupKind::Exploration {
            let blocked = |node: &SessionNode| matches!(node, GroupNode::Entry(SessionEntry::Part { r#ref }) if pending.contains(&r#ref.part_id));
            let (mut open, closed): (Vec<_>, Vec<_>) = row.children.drain(..).partition(|n| !blocked(n));
            open.extend(closed);
            row.children = open;
        }
        row.pending = group_refs(row, true).into_iter().filter(|r| pending.contains(&r.part_id)).collect();
    }
}

/// `hasPart(rows, ref)`.
pub fn has_part(rows: &[SessionRow], r#ref: &PartRef) -> bool {
    rows.iter().any(|row| match row {
        SessionRow::Entry(SessionEntry::Part { r#ref: item }) => item == r#ref,
        SessionRow::Group(group) => group_refs(group, true).iter().any(|item| item == r#ref),
        _ => false,
    })
}

// ---- tree.ts --------------------------------------------------------------------------------------------------

/// `groupEntries(entries, path)`.
pub fn group_entries<E>(entries: Vec<E>, path: impl Fn(&E) -> Vec<GroupKind>) -> Vec<GroupNode<E>> {
    let mut result = Vec::new();
    for entry in entries {
        let p = path(&entry);
        append_entry(&mut result, entry, &p, 0);
    }
    result
}

fn append_entry<E>(nodes: &mut Vec<GroupNode<E>>, entry: E, path: &[GroupKind], depth: usize) {
    let Some(&kind) = path.get(depth) else {
        nodes.push(GroupNode::Entry(entry));
        return;
    };
    if let Some(GroupNode::Group(previous)) = nodes.last_mut() {
        if previous.kind == kind {
            previous.size += 1;
            append_entry(&mut previous.children, entry, path, depth + 1);
            return;
        }
    }
    let mut children = Vec::new();
    append_entry(&mut children, entry, path, depth + 1);
    nodes.push(GroupNode::Group(Group { kind, children, size: 1 }));
}

/// `mergeGroups(left, right)`.
pub fn merge_groups<E>(mut left: Vec<GroupNode<E>>, mut right: Vec<GroupNode<E>>) -> Vec<GroupNode<E>> {
    if left.is_empty() {
        return right;
    }
    if right.is_empty() {
        return left;
    }
    let compatible = matches!((left.last(), right.first()), (Some(GroupNode::Group(a)), Some(GroupNode::Group(b))) if a.kind == b.kind);
    if !compatible {
        left.extend(right);
        return left;
    }
    let Some(GroupNode::Group(a)) = left.pop() else { unreachable!() };
    let GroupNode::Group(b) = right.remove(0) else { unreachable!() };
    left.push(GroupNode::Group(Group { kind: a.kind, size: a.size + b.size, children: merge_groups(a.children, b.children) }));
    left.extend(right);
    left
}

/// `splitGroups(nodes, count)`.
pub fn split_groups<E: Clone>(nodes: &[GroupNode<E>], count: usize) -> (Vec<GroupNode<E>>, Vec<GroupNode<E>>) {
    if count == 0 {
        return (Vec::new(), nodes.to_vec());
    }
    let mut offset = 0;
    for (index, node) in nodes.iter().enumerate() {
        let end = offset + node.size();
        if count == end {
            return (nodes[..=index].to_vec(), nodes[index + 1..].to_vec());
        }
        if count < end {
            let GroupNode::Group(group) = node else { panic!("Cannot split inside an entry") };
            let size = count - offset;
            let (left, right) = split_groups(&group.children, size);
            let mut a = nodes[..index].to_vec();
            a.push(GroupNode::Group(Group { kind: group.kind, children: left, size }));
            let mut b = vec![GroupNode::Group(Group { kind: group.kind, children: right, size: group.size - size })];
            b.extend_from_slice(&nodes[index + 1..]);
            return (a, b);
        }
        offset = end;
    }
    panic!("Group split exceeds entry count")
}
