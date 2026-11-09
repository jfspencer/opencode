//! `lastAssistantWithUsage` (packages/client/src/solid/usage.ts): the assistant step whose tokens fill the context
//! window now: the latest one after the last completed compaction and before the revert boundary.

use std::rc::Rc;

use opencode_protocol::Json;

use crate::json::JsonExt;

/// The index of that step in `messages`.
pub fn last_assistant_with_usage(messages: &[Rc<Json>], boundary: Option<&str>) -> Option<usize> {
    let boundary = boundary.filter(|b| !b.is_empty());
    let boundary_index = boundary.and_then(|b| messages.iter().position(|m| m.str("id") == Some(b)));
    if boundary.is_some() && boundary_index.is_none() {
        return None;
    }
    let end = boundary_index.unwrap_or(messages.len());
    let compaction = messages[..end]
        .iter()
        .rposition(|m| m.str("type") == Some("compaction") && m.str("status") == Some("completed"));
    let start = compaction.map_or(0, |c| c + 1);
    messages[start..end]
        .iter()
        .rposition(|m| m.str("type") == Some("assistant") && m.get("tokens").is_some())
        .map(|i| i + start)
}
