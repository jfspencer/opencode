//! `routes/session/activity-summary.ts`: low verbosity's summary of a run of tools, thoughts and instruction loads.
use std::collections::HashSet;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_sync::json::JsonExt;

use super::anchors::visit_entries;
use super::grouping::{Group, PartRef, SessionEntry, instruction_paths};
use super::message_parts::{canonical_tool_name, reasoning_content};
use super::rows::resolve_part;

struct Item {
    message: Rc<Json>,
    part: Json,
}

/// `summarizeActivity(node, message, pending, closed)`: (label, active).
pub fn summarize_activity(
    node: &Group<SessionEntry>,
    message: &dyn Fn(&str) -> Option<Rc<Json>>,
    pending: &[PartRef],
    closed: bool,
) -> (String, bool) {
    let mut entries = Vec::new();
    visit_entries(&node.children, &mut |entry| entries.push(entry.clone()));
    let items: Vec<Item> = entries
        .iter()
        .filter_map(|entry| {
            let SessionEntry::Part { r#ref } = entry else { return None };
            if pending.contains(r#ref) {
                return None;
            }
            let item = message(&r#ref.message_id)?;
            if item.str("type") != Some("assistant") {
                return None;
            }
            let part = resolve_part(&item, &r#ref.part_id)?;
            matches!(part.str("type"), Some("reasoning" | "tool")).then_some(Item { message: item, part })
        })
        .collect();
    let files: HashSet<String> = entries
        .iter()
        .flat_map(|entry| match entry {
            SessionEntry::Message { message_id } => instruction_paths(message(message_id).as_deref()),
            _ => Vec::new(),
        })
        .collect();
    let (label, active) = activity_summary(&items, files.len(), closed);
    let label = if label.is_empty() {
        items.iter().find(|item| is_active(item, closed)).map(|item| busy_label(&item.part)).unwrap_or_default()
    } else {
        label
    };
    (label, active)
}

/// `busyLabel(part)`: the status of a running item, independent of its details.
pub fn busy_label(part: &Json) -> String {
    if part.str("type") == Some("reasoning") {
        return "Thinking…".into();
    }
    let name = canonical_tool_name(part.str("name").unwrap_or_default());
    let noun = match name {
        "shell" => "command",
        "execute" => "code",
        other => other,
    };
    let status = part.get("state").and_then(|s| s.str("status"));
    format!("{} {noun}…", if status == Some("streaming") { "Preparing" } else { "Running" })
}

/// `executeCalls(value)` (util/tool-display.ts): the nested calls of a code-mode execute.
fn execute_calls(value: Option<&Json>) -> Vec<(String, String)> {
    let Some(Json::Array(calls)) = value else { return Vec::new() };
    calls
        .iter()
        .filter_map(|call| {
            let tool = call.str("tool")?;
            let status = call.str("status")?;
            matches!(status, "running" | "completed" | "error").then(|| (tool.to_string(), status.to_string()))
        })
        .collect()
}

/// `activitySummary(items, instructions, closed)`.
fn activity_summary(items: &[Item], instructions: usize, closed: bool) -> (String, bool) {
    let mut counts = [("command", 0usize), ("edit", 0), ("thought", 0), ("read", 0), ("tool", 0), ("instruction", instructions)];
    let mut add = |name: &str| {
        if let Some(slot) = counts.iter_mut().find(|(n, _)| *n == name) {
            slot.1 += 1;
        }
    };
    for item in items {
        if item.part.str("type") == Some("reasoning") {
            // Redacted-only reasoning renders nothing, so it isn't a visible thought.
            if !is_active(item, closed) && !reasoning_content(&item.part).is_empty() {
                add("thought");
            }
            continue;
        }
        let name = canonical_tool_name(item.part.str("name").unwrap_or_default());
        if name == "execute" {
            let state = item.part.get("state");
            let streaming = state.and_then(|s| s.str("status")) == Some("streaming");
            let metadata = if streaming { None } else { state.and_then(|s| s.get("metadata")).and_then(|m| m.get("toolCalls")) };
            let calls: Vec<_> = execute_calls(metadata).into_iter().filter(|(_, status)| status != "running").collect();
            for (tool, _) in &calls {
                if canonical_tool_name(tool) == "read" {
                    add("read");
                } else {
                    add("tool");
                }
            }
            // With no finished nested calls to count, a finished execute counts as itself.
            if calls.is_empty() && !is_active(item, closed) {
                add("tool");
            }
            continue;
        }
        if is_active(item, closed) {
            continue;
        }
        match name {
            "shell" => add("command"),
            "read" => add("read"),
            "edit" | "write" | "patch" => add("edit"),
            _ => add("tool"),
        }
    }
    let label = counts
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(name, count)| format!("{count} {name}{}", if *count == 1 { "" } else { "s" }))
        .collect::<Vec<_>>()
        .join(", ");
    (label, items.iter().any(|item| is_active(item, closed)))
}

fn is_active(item: &Item, closed: bool) -> bool {
    if item.part.str("type") == Some("reasoning") {
        let part_done = item.part.get("time").and_then(|t| t.num("completed")).is_some();
        let message_done = item.message.get("time").and_then(|t| t.num("completed")).is_some();
        return !closed && !part_done && !message_done;
    }
    matches!(item.part.get("state").and_then(|s| s.str("status")), Some("streaming" | "running"))
}
