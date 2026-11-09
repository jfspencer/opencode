//! A tool call's argument validation failure (tool/runtime.ts `decodeInput` / `formatInputIssues`): the input schema's
//! issues (Effect's Standard Schema formatter under `errors: "all"`, so every key reports, without the input value)
//! become one tool error that restates the arguments and asks the model to call again.

use opencode_protocol::Json;

use crate::json;

/// One schema issue: its path (`url`, `items[0].name`; empty for the root) and message.
pub struct Issue {
    pub path: String,
    pub message: String,
}

impl Issue {
    pub fn new(path: &str, message: impl Into<String>) -> Self {
        Self { path: path.to_owned(), message: message.into() }
    }
}

/// `formatInputIssues`: at most five issues, then a count of the rest.
pub fn invalid(tool: &str, issues: &[Issue], value: &Json) -> super::ToolError {
    let mut details: Vec<String> = issues
        .iter()
        .take(5)
        .map(|issue| format!("- {}: {}", if issue.path.is_empty() { "root" } else { &issue.path }, issue.message))
        .collect();
    if issues.len() > 5 {
        let more = issues.len() - 5;
        details.push(format!("- ...and {more} more {}", if more == 1 { "issue" } else { "issues" }));
    }
    super::ToolError::new(format!(
        "Invalid arguments for tool \"{tool}\":\n{}\n\nArguments provided:\n{}\n\nUpdate the arguments and call the tool again.",
        details.join("\n"),
        pretty(value)
    ))
}

/// `JSON.stringify(value, null, 2)`.
pub fn pretty(value: &Json) -> String {
    let mut out = String::new();
    write(value, 0, &mut out);
    out
}

fn write(value: &Json, indent: usize, out: &mut String) {
    let pad = |n: usize| "  ".repeat(n);
    match value {
        Json::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                write(item, indent + 1, out);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(indent));
            out.push(']');
        }
        Json::Object(fields) if !fields.is_empty() => {
            out.push_str("{\n");
            for (i, (key, item)) in fields.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                out.push_str(&json::text(&json::string(key)));
                out.push_str(": ");
                write(item, indent + 1, out);
                out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(indent));
            out.push('}');
        }
        other => out.push_str(&json::text(other)),
    }
}

/// The input as an object, or the root issue Effect reports for anything else (`Expected <the struct's text>`).
pub fn object<'a>(input: &'a Json, expected: &str) -> Result<&'a indexmap::IndexMap<String, Json>, Issue> {
    match input {
        Json::Object(fields) => Ok(fields),
        _ => Err(Issue::new("", format!("Expected {expected}"))),
    }
}
