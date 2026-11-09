//! The Code Mode catalog instructions (packages/core/src/codemode/instructions.ts): the `core/codemode` source's
//! text for a catalog summary ([`super::catalog`]), and the delta when it changes.

use opencode_codemode::tools::{SEARCH_SIGNATURE, tool_expression};
use opencode_protocol::Json;

use crate::json;

pub const REMOVED: &str = "Code Mode tools are no longer available. Do not use any previously listed Code Mode tools.";

fn number(value: &Json, key: &str) -> usize {
    json::get_num(value, key).unwrap_or(0.0) as usize
}

fn namespaces(summary: &Json) -> &[Json] {
    json::get(summary, "namespaces").and_then(json::array).unwrap_or_default()
}

fn entries(namespace: &Json) -> &[Json] {
    json::get(namespace, "entries").and_then(json::array).unwrap_or_default()
}

fn text<'a>(value: &'a Json, key: &str) -> &'a str {
    json::get_str(value, key).unwrap_or_default()
}

fn namespace_line(namespace: &Json) -> String {
    super::namespace_line(
        text(namespace, "name"),
        json::get_str(namespace, "description"),
        number(namespace, "count"),
        entries(namespace).len(),
    )
}

fn prompt(more: bool) -> String {
    let search = if more { ", and `search`," } else { "" };
    let guidance = if more {
        format!(
            "The catalog is partial. Before calling a tool that is not listed below, find it with the global `search(...)` function. Do not guess tool names. Each result's `path` is the complete call expression: write it out as code, for example `await tools.ns.tool_name({{ ... }})`. Do not use it as a key such as `tools[path]`.\n\n- {SEARCH_SIGNATURE}"
        )
    } else {
        "The catalog is complete. Do not guess tool names.".to_owned()
    };
    format!(
        "# Code Mode\n\nThe tools listed below{search} cannot be called directly: they only work as JavaScript inside the `execute` tool. Every other tool you have is a normal tool call: call it directly, never inside `execute`.\n\n{guidance}\n\n## Code Mode tools (call only inside `execute`)"
    )
}

/// `render`.
pub fn render(summary: &Json) -> String {
    let total = number(summary, "total");
    if total == 0 {
        return "No Code Mode tools are currently available. Later Code Mode catalog updates may add or remove tools. Do not call `execute` unless there is at least one available Code Mode tool.".into();
    }
    let lines: Vec<String> = namespaces(summary)
        .iter()
        .flat_map(|namespace| {
            std::iter::once(namespace_line(namespace)).chain(entries(namespace).iter().map(|e| text(e, "line").to_owned()))
        })
        .collect();
    format!("{}\n\n{}", prompt(number(summary, "shown") < total), lines.join("\n"))
}

/// `Instructions.diffByKey`.
struct Diff<'a> {
    added: Vec<&'a Json>,
    removed: Vec<&'a Json>,
    changed: Vec<&'a Json>,
}

impl Diff<'_> {
    fn any(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty() || !self.changed.is_empty()
    }
}

fn diff<'a>(previous: &[&'a Json], current: &[&'a Json], key: &str, field: &str) -> Diff<'a> {
    let before = |value: &Json| previous.iter().rev().find(|p| json::get(p, key) == json::get(value, key)).copied();
    Diff {
        added: current.iter().filter(|value| before(value).is_none()).copied().collect(),
        removed: previous.iter().filter(|p| !current.iter().any(|c| json::get(c, key) == json::get(p, key))).copied().collect(),
        changed: current
            .iter()
            .filter(|value| before(value).is_some_and(|b| json::get(b, field) != json::get(value, field)))
            .copied()
            .collect(),
    }
}

/// `update`: the delta from `previous` to `current`, or the whole catalog restated when that is shorter or the
/// change cannot be described as one.
pub fn update(previous: &Json, current: &Json) -> String {
    let replacement = format!(
        "The Code Mode tool catalog has changed. This catalog supersedes the previous Code Mode tool catalog.\n\n{}",
        render(current)
    );
    if number(current, "total") == 0 {
        return replacement;
    }
    let complete = |summary: &Json| number(summary, "shown") == number(summary, "total");
    if complete(previous) != complete(current) {
        return replacement;
    }
    fn described(summary: &Json) -> Vec<&Json> {
        namespaces(summary).iter().filter(|n| json::get(n, "description").is_some()).collect()
    }
    if diff(&described(previous), &described(current), "name", "description").any() {
        return replacement;
    }
    fn all_entries(summary: &Json) -> Vec<&Json> {
        namespaces(summary).iter().flat_map(entries).collect()
    }
    let entries_diff = diff(&all_entries(previous), &all_entries(current), "path", "line");
    let shorter = |parts: Vec<String>| {
        let delta = parts.join("\n\n");
        if delta.encode_utf16().count() < replacement.encode_utf16().count() { delta } else { replacement.clone() }
    };
    if !complete(current) {
        if entries_diff.any() {
            return replacement;
        }
        fn all(summary: &Json) -> Vec<&Json> {
            namespaces(summary).iter().collect()
        }
        let namespace_diff = diff(&all(previous), &all(current), "name", "count");
        if !namespace_diff.any() {
            return replacement;
        }
        let mut parts = vec!["The Code Mode tool catalog has changed.".to_owned()];
        if !namespace_diff.added.is_empty() {
            let added: Vec<String> = namespace_diff
                .added
                .iter()
                .map(|n| format!("`{}` ({} tools)", text(n, "name"), number(n, "count")))
                .collect();
            parts.push(format!("New tool namespaces are available: {}.", added.join(", ")));
        }
        if !namespace_diff.changed.is_empty() {
            let changed: Vec<String> = namespace_diff
                .changed
                .iter()
                .map(|n| format!("`{}` now has {} tools", text(n, "name"), number(n, "count")))
                .collect();
            parts.push(format!(
                "The following namespace inventories changed; search them again before relying on previous results: {}.",
                changed.join(", ")
            ));
        }
        if !namespace_diff.removed.is_empty() {
            let removed: Vec<String> = namespace_diff.removed.iter().map(|n| format!("`{}`", text(n, "name"))).collect();
            parts.push(format!(
                "The following tool namespaces are no longer available and must not be used: {}.",
                removed.join(", ")
            ));
        }
        return shorter(parts);
    }
    if !entries_diff.any() {
        return replacement;
    }
    let mut parts = vec!["The Code Mode tool catalog has changed.".to_owned()];
    if !entries_diff.added.is_empty() {
        let lines = entries_diff.added.iter().map(|e| text(e, "line").to_owned());
        parts.push(std::iter::once("New tools are available in addition to those previously listed:".to_owned()).chain(lines).collect::<Vec<_>>().join("\n"));
    }
    if !entries_diff.changed.is_empty() {
        let lines = entries_diff.changed.iter().map(|e| text(e, "line").to_owned());
        parts.push(std::iter::once("Changed tool listings supersede the previously listed ones:".to_owned()).chain(lines).collect::<Vec<_>>().join("\n"));
    }
    if !entries_diff.removed.is_empty() {
        let removed: Vec<String> = entries_diff.removed.iter().map(|e| tool_expression(text(e, "path"))).collect();
        parts.push(format!("The following tools are no longer available and must not be called: {}.", removed.join(", ")));
    }
    shorter(parts)
}
