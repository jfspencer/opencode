//! How `opencode run` shows a tool call (packages/tui/src/mini/tool.ts `toolInlineInfo`, `toolOutputText`, and
//! util/tool-display.ts, util/path-format.ts): an icon, a title and either a dim description or a block body.

use opencode_protocol::Json;
use opencode_protocol::wire::number::js_trim;

use crate::json::{self, get, get_str};

pub struct Inline {
    pub icon: &'static str,
    pub title: String,
    pub description: Option<String>,
    pub block: bool,
    pub body: Option<String>,
}

pub fn canonical(name: &str) -> &str {
    match name {
        "bash" => "shell",
        "task" => "subagent",
        "apply_patch" => "patch",
        other => other,
    }
}

/// `toolOutputText`: the visible text of a tool's content.
pub fn output_text(name: &str, content: Option<&Json>) -> String {
    let Some(items) = content.and_then(json::array) else { return String::new() };
    let texts = || items.iter().filter(|i| get_str(i, "type") == Some("text"));
    if canonical(name) == "shell" {
        return texts().next().and_then(|i| get_str(i, "text")).unwrap_or("").to_owned();
    }
    let joined = texts().filter_map(|i| get_str(i, "text")).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n");
    if canonical(name) == "read" {
        return read_display_text(&joined).unwrap_or(joined);
    }
    joined
}

/// Read's model content may be a JSON page envelope; its human-facing text.
fn read_display_text(text: &str) -> Option<String> {
    if !text.starts_with('{') {
        return None;
    }
    let envelope = json::parse(text)?;
    if let Some(content) = get_str(&envelope, "content") {
        if get_str(&envelope, "type") == Some("text-page") || get_str(&envelope, "encoding") == Some("utf8") {
            return Some(content.to_owned());
        }
    }
    let entries = get(&envelope, "entries").and_then(json::array)?;
    Some(
        entries
            .iter()
            .filter_map(|entry| match entry {
                Json::String(s) => Some(s.clone()),
                other => get_str(other, "path").map(String::from),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn normalize_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = vec![];
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute { format!("/{joined}") } else if joined.is_empty() { ".".into() } else { joined }
}

/// `path.posix.resolve(base, input)`.
fn resolve(base: &str, input: &str) -> String {
    if input.starts_with('/') { normalize_path(input) } else { normalize_path(&format!("{base}/{input}")) }
}

/// `path.posix.relative(from, to)` for absolute paths.
fn relative(from: &str, to: &str) -> String {
    let from: Vec<&str> = from.split('/').filter(|p| !p.is_empty()).collect();
    let to: Vec<&str> = to.split('/').filter(|p| !p.is_empty()).collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = vec![".."; from.len() - common];
    parts.extend(&to[common..]);
    parts.join("/")
}

/// `toolPath(input, { directory })`: relative to the directory when inside it, else absolute.
pub fn tool_path(input: Option<&str>, directory: &str) -> String {
    let Some(input) = input.filter(|i| !i.is_empty()) else { return String::new() };
    let base = normalize_path(directory);
    let absolute = resolve(&base, input);
    let relative = relative(&base, &absolute);
    if relative.is_empty() {
        return ".".into();
    }
    if relative != ".." && !relative.starts_with("../") {
        return relative;
    }
    absolute
}

/// `primitiveInputSummary`.
fn primitive_summary(input: &Json, omit: &[&str]) -> String {
    let Json::Object(object) = input else { return String::new() };
    let entries: Vec<String> = object
        .iter()
        .filter(|(key, _)| !omit.contains(&key.as_str()))
        .filter_map(|(key, value)| match value {
            Json::String(s) => Some(format!("{key}={s}")),
            Json::Number(_) | Json::Bool(_) => Some(format!("{key}={}", json::text(value))),
            _ => None,
        })
        .collect();
    if entries.is_empty() { String::new() } else { format!("[{}]", entries.join(", ")) }
}

fn text(value: Option<&Json>) -> &str {
    value.and_then(json::as_str).unwrap_or("")
}

fn titlecase(text: &str) -> String {
    let mut out = String::new();
    let mut previous_word = false;
    for ch in text.chars() {
        let word = ch.is_ascii_alphanumeric() || ch == '_';
        out.push(if word && !previous_word { ch.to_ascii_uppercase() } else { ch });
        previous_word = word;
    }
    out
}

/// The tool frame's normalized parts.
struct Frame<'a> {
    name: String,
    input: Json,
    metadata: Json,
    status: &'a str,
    output: String,
}

fn normalize_input(name: &str, value: Option<&Json>) -> Json {
    let mut input = match value {
        Some(Json::Object(o)) => Json::Object(o.clone()),
        _ => json::object(),
    };
    let path = match get(&input, "path") {
        Some(Json::String(s)) => s.clone(),
        _ => text(get(&input, "filePath")).to_owned(),
    };
    if ["read", "write", "edit", "lsp"].contains(&name) && !path.is_empty() {
        json::set(&mut input, "path", Json::String(path));
    }
    let agent = match get(&input, "agent") {
        Some(Json::String(s)) => s.clone(),
        _ => text(get(&input, "subagent_type")).to_owned(),
    };
    if name == "subagent" && !agent.is_empty() {
        json::set(&mut input, "agent", Json::String(agent));
    }
    input
}

fn plural(n: usize, singular: &str, suffix: &str) -> String {
    format!("{n} {singular}{}", if n == 1 { "" } else { suffix })
}

/// `toolInlineInfo(part, directory)` for a finished tool: `state` is `{status, input, metadata, content, error}`.
pub fn inline(name: &str, state: &Json, directory: &str) -> Inline {
    let name = canonical(name).to_owned();
    let metadata = match get(state, "metadata") {
        Some(Json::Object(o)) => Json::Object(o.clone()),
        _ => json::object(),
    };
    let frame = Frame {
        input: normalize_input(&name, get(state, "input")),
        output: output_text(&name, get(state, "content")),
        status: get_str(state, "status").unwrap_or(""),
        metadata,
        name,
    };
    let p = &frame;
    let input = |key: &str| get_str(&p.input, key);
    let path = |value: Option<&str>| tool_path(value, directory);
    let simple = |icon: &'static str, title: String| Inline { icon, title, description: None, block: false, body: None };
    let block = |icon: &'static str, title: String, body: Option<String>| Inline { icon, title, description: None, block: true, body };
    let completed_output = || (p.status == "completed").then(|| p.output.clone());
    match p.name.as_str() {
        "invalid" => block("✗", "Invalid Tool".into(), completed_output()),
        "shell" => block(
            "$",
            input("command").unwrap_or("").to_owned(),
            (p.status == "completed").then(|| js_trim(&p.output).to_owned()),
        ),
        "write" => block("←", format!("Write {}", path(input("path"))), completed_output()),
        "edit" => {
            let patch = get(&p.metadata, "files")
                .and_then(json::array)
                .and_then(|files| files.first())
                .and_then(|file| get_str(file, "patch"))
                .or_else(|| get_str(&p.metadata, "diff"))
                .map(String::from);
            block("←", format!("Edit {}", path(input("path"))), patch)
        }
        "patch" => {
            let files = get(&p.metadata, "files").and_then(json::array).map_or(0, |f| f.len());
            if files == 0 {
                simple("%", "Patch".into())
            } else {
                simple("%", format!("Patch {}", plural(files, "file", "s")))
            }
        }
        "batch" => {
            let calls = get(&p.input, "tool_calls").and_then(json::array).map_or(0, |c| c.len());
            let title = if calls > 0 { format!("Batch {}", plural(calls, "tool", "s")) } else { "Batch".into() };
            block("#", title, completed_output())
        }
        "subagent" => {
            let kind = titlecase(input("agent").filter(|a| !a.is_empty()).unwrap_or("unknown"));
            let description = input("description").filter(|d| !d.is_empty());
            let icon = match p.status {
                "error" => "✗",
                "running" => "•",
                _ => "✓",
            };
            Inline {
                icon,
                title: description.map(String::from).unwrap_or_else(|| format!("{kind} Subagent")),
                description: description.map(|_| format!("{kind} Agent")),
                block: false,
                body: None,
            }
        }
        "todowrite" => {
            let todos = get(&p.input, "todos").and_then(json::array).unwrap_or(&[]);
            let body = todos
                .iter()
                .filter_map(|todo| {
                    let content = get_str(todo, "content")?;
                    let marker = match get_str(todo, "status") {
                        Some("completed") => "[✓]",
                        Some("in_progress") => "[•]",
                        Some("cancelled") => "[-]",
                        _ => "[ ]",
                    };
                    Some(format!("{marker} {content}"))
                })
                .collect::<Vec<_>>()
                .join("\n");
            block("#", "Todos".into(), Some(body))
        }
        "question" => {
            let total = get(&p.input, "questions").and_then(json::array).map_or(0, |q| q.len());
            simple("→", format!("Asked {}", plural(total, "question", "s")))
        }
        "read" => {
            let summary = primitive_summary(&p.input, &["path"]);
            Inline {
                icon: "→",
                title: format!("Read {}", path(input("path"))),
                description: (!summary.is_empty()).then_some(summary),
                block: false,
                body: None,
            }
        }
        "glob" | "grep" => {
            let root = input("path").unwrap_or("");
            let title = format!("{} \"{}\"", if p.name == "glob" { "Glob" } else { "Grep" }, input("pattern").unwrap_or(""));
            let suffix = if root.is_empty() { String::new() } else { format!("in {}", path(Some(root))) };
            let matches = json::get_num(&p.metadata, if p.name == "glob" { "count" } else { "matches" });
            let description = match matches {
                None => suffix,
                Some(n) => {
                    let n = n as usize;
                    format!("{suffix}{}{}", if suffix.is_empty() { "" } else { " · " }, plural(n, "match", "es"))
                }
            };
            Inline { icon: "✱", title, description: (!description.is_empty()).then_some(description), block: false, body: None }
        }
        "list" => {
            let dir = input("path").unwrap_or("");
            simple("→", if dir.is_empty() { "List".into() } else { format!("List {}", path(Some(dir))) })
        }
        "lsp" => {
            let op = input("operation").filter(|o| !o.is_empty()).unwrap_or("request");
            let file = path(input("path"));
            let line = json::get_num(&p.input, "line");
            let character = json::get_num(&p.input, "character");
            let pos = match (line, character) {
                (Some(l), Some(c)) => format!(":{}:{}", json::text(&Json::Number(l)), json::text(&Json::Number(c))),
                _ => String::new(),
            };
            simple("→", if file.is_empty() { format!("LSP {op}") } else { format!("LSP {op} {file}{pos}") })
        }
        "webfetch" => {
            let url = input("url").unwrap_or("");
            simple("%", if url.is_empty() { "WebFetch".into() } else { format!("WebFetch {url}") })
        }
        "websearch" => {
            let provider = get_str(&p.metadata, "provider").unwrap_or("");
            let label = match provider {
                "" => "Web Search".to_owned(),
                "opencode" => "Web Search via OpenCode".to_owned(),
                other => {
                    let mut chars = other.chars();
                    let first = chars.next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
                    format!("Web Search via {first}{}", chars.as_str())
                }
            };
            let query = input("query").filter(|q| !q.is_empty());
            simple("◈", match query {
                Some(query) => format!("{label} \"{query}\""),
                None => label,
            })
        }
        "skill" => {
            let name = get_str(&p.metadata, "name").or_else(|| input("id")).unwrap_or("");
            simple("→", format!("Skill \"{name}\""))
        }
        _ => {
            let title = match &p.input {
                Json::Object(o) if !o.is_empty() => json::text(&p.input),
                _ => "Unknown".into(),
            };
            simple("⚙", format!("{} {title}", p.name))
        }
    }
}
