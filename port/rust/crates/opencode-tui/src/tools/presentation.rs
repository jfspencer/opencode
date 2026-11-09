//! `util/permission.ts`: how a permission request is presented (icon, title, detail lines, the edit's diff), the
//! "always allow" lines and the option labels.
use opencode_protocol::Json;

use super::display::{canonical_tool_name, finite_number, web_search_provider_label};
use crate::util::titlecase;

/// `PermissionPresentation`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PermissionPresentation {
    pub icon: &'static str,
    pub title: String,
    pub lines: Vec<String>,
    pub diff: Option<String>,
    pub patch: Option<String>,
    pub file: Option<String>,
}

/// `PermissionPresentationInput`.
pub struct PresentationInput<'a> {
    pub action: &'a str,
    pub resources: &'a [Json],
    pub metadata: Option<&'a Json>,
    pub input: Option<&'a Json>,
    pub tool_metadata: Option<&'a Json>,
}

type Dict = opencode_protocol::wire::Object;

/// `dict(value)`.
fn dict(value: Option<&Json>) -> Dict {
    match value {
        Some(Json::Object(o)) => o.clone(),
        _ => Dict::default(),
    }
}

/// `text(value)`.
fn text(value: Option<&Json>) -> String {
    match value {
        Some(Json::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// `a || b || ...` over strings.
fn first(values: &[String]) -> String {
    values.iter().find(|v| !v.is_empty()).cloned().unwrap_or_default()
}

/// `normalizeInput(action, value)`.
fn normalize_input(action: &str, value: Option<&Json>) -> Dict {
    let mut input = dict(value);
    let path = first(&[text(input.get("path")), text(input.get("filePath")), text(input.get("filepath"))]);
    let agent = first(&[text(input.get("agent")), text(input.get("subagent_type"))]);
    if ["read", "edit", "list", "lsp"].contains(&action) && !path.is_empty() {
        input.insert("path".into(), Json::String(path));
    }
    if action == "subagent" && !agent.is_empty() {
        input.insert("agent".into(), Json::String(agent));
    }
    input
}

/// `wildcardDirectory(value)`.
fn wildcard_directory(value: &str) -> String {
    let Some(wildcard) = value.find('*') else { return value.to_string() };
    let prefix = &value[..wildcard];
    let all_separators = !prefix.is_empty() && prefix.chars().all(|c| c == '/' || c == '\\');
    let drive = prefix.len() == 3 && {
        let b = prefix.as_bytes();
        b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
    };
    if all_separators || drive {
        return prefix.to_string();
    }
    prefix.trim_end_matches(['/', '\\']).to_string()
}

/// `permissionPresentation(source, formatPath)`.
pub fn permission_presentation(source: &PresentationInput, format_path: &dyn Fn(&str) -> String) -> PermissionPresentation {
    let action = canonical_tool_name(source.action);
    let input = normalize_input(action, source.input);
    let mut metadata = dict(source.tool_metadata);
    for (key, value) in dict(source.metadata) {
        metadata.insert(key, value);
    }
    let resources: Vec<String> = source
        .resources
        .iter()
        .filter_map(|item| match item {
            Json::String(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    let resource0 = resources.first().cloned().unwrap_or_default();
    let mut out = PermissionPresentation::default();

    if action == "edit" {
        let file = first(&[text(input.get("path")), resource0]);
        let files = metadata.get("files");
        let first_file = match files {
            Some(Json::Array(items)) => dict(items.first()),
            _ => Dict::default(),
        };
        let diff = first(&[text(first_file.get("patch")), text(first_file.get("diff")), text(metadata.get("diff"))]);
        let patch_text = text(input.get("patchText"));
        out.icon = "→";
        out.title = format!("Edit {}", format_path(&file));
        out.patch = if diff.is_empty() && !patch_text.is_empty() { Some(patch_text) } else { None };
        out.diff = (!diff.is_empty()).then_some(diff);
        out.file = Some(file);
        return out;
    }

    if action == "read" || action == "list" {
        let value = first(&[text(input.get("path")), resource0]);
        out.icon = "→";
        out.title = format!("{} {}", if action == "read" { "Read" } else { "List" }, format_path(&value));
        if !value.is_empty() {
            out.lines = vec![format!("Path: {}", format_path(&value))];
        }
        return out;
    }

    if action == "glob" || action == "grep" {
        let pattern = first(&[text(input.get("pattern")), resource0]);
        out.icon = "✱";
        out.title = format!("{} \"{pattern}\"", if action == "glob" { "Glob" } else { "Grep" });
        if !pattern.is_empty() {
            out.lines = vec![format!("Pattern: {pattern}")];
        }
        return out;
    }

    if action == "shell" {
        let command = text(input.get("command"));
        out.icon = "#";
        out.title = "Shell command".into();
        out.lines =
            if !command.is_empty() { vec![format!("$ {command}")] } else { resources.iter().map(|item| format!("- {item}")).collect() };
        return out;
    }

    if action == "subagent" {
        let agent = first(&[text(input.get("agent")), "general".into()]);
        let description = text(input.get("description"));
        out.icon = "#";
        out.title = format!("{} Subagent", titlecase(&agent));
        if !description.is_empty() {
            out.lines = vec![format!("◉ {description}")];
        }
        return out;
    }

    if action == "webfetch" {
        let url = first(&[text(input.get("url")), text(metadata.get("url"))]);
        out.icon = "%";
        out.title = format!("WebFetch {url}");
        if !url.is_empty() {
            out.lines = vec![format!("URL: {url}")];
        }
        return out;
    }

    if action == "websearch" {
        let query = first(&[text(input.get("query")), text(metadata.get("query"))]);
        let title = web_search_provider_label(metadata.get("provider"));
        out.icon = "◈";
        out.title = if query.is_empty() { title } else { format!("{title} \"{query}\"") };
        if !query.is_empty() {
            out.lines = vec![format!("Query: {query}")];
        }
        return out;
    }

    if action == "lsp" {
        let file = text(input.get("path"));
        let operation = first(&[text(input.get("operation")), "request".into()]);
        let line = finite_number(input.get("line"));
        let character = finite_number(input.get("character"));
        let position = match (line, character) {
            (Some(l), Some(c)) => Some(format!("{}:{}", opencode_tui_sync::js::number(l), opencode_tui_sync::js::number(c))),
            _ => None,
        };
        out.icon = "→";
        let location = if file.is_empty() {
            String::new()
        } else {
            format!(" {}{}", format_path(&file), position.as_ref().map(|p| format!(":{p}")).unwrap_or_default())
        };
        out.title = format!("LSP {operation}{location}");
        if opencode_tui_sync::json::truthy(input.get("operation").unwrap_or(&Json::Null)) {
            out.lines.push(format!("Operation: {operation}"));
        }
        if !file.is_empty() {
            out.lines.push(format!("Path: {}", format_path(&file)));
        }
        if let Some(position) = position {
            out.lines.push(format!("Position: {position}"));
        }
        return out;
    }

    if action == "external_directory" {
        let raw = first(&[text(metadata.get("parentDir")), text(metadata.get("filepath")), resource0]);
        let directory = wildcard_directory(&raw);
        out.icon = "←";
        out.title = format!("Access external directory {}", format_path(&directory));
        out.lines = resources.iter().map(|item| format!("- {item}")).collect();
        return out;
    }

    if action == "doom_loop" {
        out.icon = "⟳";
        out.title = "Continue after repeated failures".into();
        out.lines = vec!["This keeps the session running despite repeated failures.".into()];
        return out;
    }

    out.icon = "⚙";
    out.title = format!("Call tool {}", source.action);
    out.lines = vec![format!("Tool: {}", source.action)];
    out
}

/// `permissionAlwaysLines(input)`.
pub fn permission_always_lines(action: &str, save: &[String]) -> Vec<String> {
    if save.len() == 1 && save[0] == "*" {
        return vec![format!("This will always allow {action} for this project.")];
    }
    let mut lines = vec!["This will always allow the following patterns for this project.".to_string()];
    lines.extend(save.iter().map(|item| format!("- {item}")));
    lines
}

/// `permissionOptionLabel(option)`.
pub fn permission_option_label(option: &str) -> &'static str {
    match option {
        "once" => "Allow once",
        "always" => "Always allow",
        "reject" => "Reject",
        "confirm" => "Confirm",
        _ => "Cancel",
    }
}
