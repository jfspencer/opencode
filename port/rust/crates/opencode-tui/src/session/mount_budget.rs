//! `routes/session/mount-budget.ts`: transcript mounting budgeted by rendered entries, not rows.
use super::anchors::group_id;
use super::grouping::{Group, GroupKind, GroupNode, SessionEntry, SessionRow};

/// `rowWeight(row, { expanded, grouped })`: a collapsed group costs one; an expanded or ungrouped one its children.
pub fn row_weight(row: &SessionRow, expanded: &dyn Fn(&str, GroupKind) -> bool, grouped: &dyn Fn(GroupKind) -> bool) -> usize {
    let SessionRow::Group(group) = row else { return 1 };
    fn visit(
        node: &Group<SessionEntry>,
        level: usize,
        expanded: &dyn Fn(&str, GroupKind) -> bool,
        grouped: &dyn Fn(GroupKind) -> bool,
    ) -> usize {
        if !grouped(node.kind) {
            return node.size;
        }
        let Some(id) = group_id(node, level).filter(|id| expanded(id, node.kind)) else { return 1 };
        let _ = id;
        node.children.iter().fold(1, |total, child| {
            total
                + match child {
                    GroupNode::Group(g) => visit(g, level + 1, expanded, grouped),
                    GroupNode::Entry(_) => 1,
                }
        })
    }
    visit(&group.node(), 0, expanded, grouped)
}

/// `rowsBefore(weights, end, budget)`: the first row index such that rows [index, end) spend at least `budget`.
pub fn rows_before(weights: &[usize], end: usize, budget: usize) -> usize {
    let mut index = end;
    let mut spent = 0;
    while index > 0 && spent < budget {
        index -= 1;
        spent += weights[index];
    }
    index
}

/// `rowsAfter(weights, start, budget)`: the end row index such that rows [start, index) spend at least `budget`.
pub fn rows_after(weights: &[usize], start: usize, budget: usize) -> usize {
    let mut index = start;
    let mut spent = 0;
    while index < weights.len() && spent < budget {
        spent += weights[index];
        index += 1;
    }
    index
}
