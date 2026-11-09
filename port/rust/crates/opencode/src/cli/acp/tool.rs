//! Tool calls as ACP tool call updates (packages/cli/src/acp/tool.ts): kinds, locations, titles, raw input and the
//! pending, running, completed and failed shapes.

use opencode_protocol::Json;

use crate::json::{self, Obj, get, get_str};
use crate::tool::patch_text::{self, Hunk};

/// `canonicalName`: V1 tool names map to their current ones.
pub fn canonical(name: &str) -> String {
    match name.to_lowercase().as_str() {
        "bash" => "shell".into(),
        "task" => "subagent".into(),
        "apply_patch" => "patch".into(),
        other => other.to_owned(),
    }
}

fn kind(name: &str) -> &'static str {
    match canonical(name).as_str() {
        "shell" => "execute",
        "webfetch" => "fetch",
        "edit" | "patch" | "write" => "edit",
        "grep" | "glob" | "context" | "context7_resolve_library_id" | "context7_get_library_docs" => "search",
        "read" => "read",
        "subagent" => "think",
        _ => "other",
    }
}

/// `filePath`: sessions migrated from V1 keep their `filePath` inputs.
pub fn file_path(input: &Json) -> Option<&str> {
    get_str(input, "path").or_else(|| get_str(input, "filePath"))
}

/// `patchHunks`: the hunks of a patch tool's `patchText`, none when it does not parse.
pub fn patch_hunks(input: &Json) -> Vec<Hunk> {
    match get_str(input, "patchText").filter(|t| !t.is_empty()) {
        Some(text) => patch_text::parse(text).unwrap_or_default(),
        None => vec![],
    }
}

/// `absolutePath`: `path.resolve(cwd, path)` unless already absolute.
pub fn absolute_path(path: &str, cwd: &str) -> String {
    if path.starts_with('/') {
        return path.to_owned();
    }
    crate::project::normalize(&format!("{cwd}/{path}"))
}

fn location_from(cwd: &str, values: &[Option<&str>]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for value in values.iter().flatten().filter(|v| !v.is_empty()) {
        let path = absolute_path(value, cwd);
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// `toLocations`, as paths.
pub fn location_paths(name: &str, input: &Json, cwd: &str) -> Vec<String> {
    match canonical(name).as_str() {
        "shell" => location_from(cwd, &[Some(get_str(input, "workdir").or_else(|| get_str(input, "cwd")).unwrap_or(cwd))]),
        "read" | "edit" | "write" => location_from(cwd, &[file_path(input)]),
        "patch" => {
            let hunks = patch_hunks(input);
            let values: Vec<Option<&str>> = hunks
                .iter()
                .flat_map(|hunk| {
                    let moved = match hunk {
                        Hunk::Update { move_path, .. } => move_path.as_deref(),
                        _ => None,
                    };
                    [Some(hunk.path()), moved]
                })
                .collect();
            location_from(cwd, &values)
        }
        "external_directory" => location_from(cwd, &[get_str(input, "filepath")]),
        "grep" | "glob" | "context" | "context7_resolve_library_id" | "context7_get_library_docs" => location_from(cwd, &[get_str(input, "path")]),
        _ => vec![],
    }
}

pub fn locations_json(paths: &[String]) -> Json {
    Json::Array(
        paths
            .iter()
            .map(|path| {
                let mut o = Obj::new();
                o.str("path", path);
                o.done()
            })
            .collect(),
    )
}

pub fn locations(name: &str, input: &Json, cwd: &str) -> Json {
    locations_json(&location_paths(name, input, cwd))
}

fn is_shell(name: &str) -> bool {
    canonical(name) == "shell"
}

/// `toolTitle`.
pub fn title(name: &str, input: &Json, fallback: Option<&str>) -> String {
    if is_shell(name) {
        return get_str(input, "command").or_else(|| get_str(input, "cmd")).or(fallback).unwrap_or(name).to_owned();
    }
    fallback.filter(|f| !f.is_empty()).unwrap_or(name).to_owned()
}

fn truthy(value: Option<&Json>) -> bool {
    match value {
        None | Some(Json::Null) | Some(Json::Bool(false)) => false,
        Some(Json::Number(n)) => *n != 0.0 && !n.is_nan(),
        Some(Json::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `rawInput`: a shell call's input names the directory it runs in.
pub fn raw_input(name: &str, input: &Json, cwd: &str) -> Json {
    if !is_shell(name) || truthy(get(input, "cwd")) || truthy(get(input, "workdir")) {
        return input.clone();
    }
    let mut out = input.clone();
    json::set(&mut out, "cwd", Json::String(cwd.into()));
    out
}

/// `pendingToolCall`.
pub fn pending(id: &str, name: &str, input: &Json, title_hint: Option<&str>, cwd: &str) -> Obj {
    let mut o = Obj::new();
    o.str("toolCallId", id)
        .str("name", name)
        .str("title", &title(name, input, title_hint))
        .str("kind", kind(name))
        .str("status", "pending")
        .set("locations", locations(name, input, cwd))
        .set("rawInput", raw_input(name, input, cwd));
    o
}

/// `runningToolUpdate`.
pub fn running(id: &str, name: &str, input: &Json, cwd: &str) -> Obj {
    let mut o = Obj::new();
    o.str("toolCallId", id)
        .str("status", "in_progress")
        .str("kind", kind(name))
        .str("title", &title(name, input, None))
        .set("locations", locations(name, input, cwd))
        .set("rawInput", raw_input(name, input, cwd));
    o
}

/// `completedToolUpdate`.
pub fn completed(id: &str, name: &str, input: &Json, content: &[Json], metadata: Option<&Json>, cwd: &str) -> Obj {
    let normalized = tool_content(content);
    let first_text = content.iter().find(|c| get_str(c, "type") == Some("text")).and_then(|c| get_str(c, "text"));
    let read = if canonical(name) == "read" { first_text.and_then(crate::cli::tool::read_display_text) } else { None };
    let is_image = |part: &Json| get(part, "content").and_then(|c| get_str(c, "type")) == Some("image");
    let images: Vec<Json> = normalized.iter().filter(|p| is_image(p)).cloned().collect();
    let primary: Vec<Json> = match read {
        None => normalized.iter().filter(|p| !is_image(p)).cloned().collect(),
        Some(text) => vec![text_content(&text)],
    };
    let mut raw = Obj::new();
    raw.opt("metadata", metadata.cloned());
    let mut o = Obj::new();
    o.str("toolCallId", id)
        .str("status", "completed")
        .set("locations", locations(name, input, cwd))
        .set("content", Json::Array(primary.into_iter().chain(images).collect()))
        .set("rawOutput", raw.done());
    o
}

/// `errorToolUpdate`.
pub fn failed(id: &str, name: &str, input: &Json, content: &[Json], metadata: Option<&Json>, error: &str, cwd: &str) -> Obj {
    let mut parts = tool_content(content);
    parts.push(text_content(error));
    let mut raw = Obj::new();
    raw.opt("metadata", metadata.cloned()).str("error", error);
    let mut o = Obj::new();
    o.str("toolCallId", id)
        .str("status", "failed")
        .str("kind", kind(name))
        .str("title", &title(name, input, None))
        .set("locations", locations(name, input, cwd))
        .set("rawInput", raw_input(name, input, cwd))
        .set("content", Json::Array(parts))
        .set("rawOutput", raw.done());
    o
}

pub fn text_content(text: &str) -> Json {
    let mut inner = Obj::new();
    inner.str("type", "text").str("text", text);
    let mut part = Obj::new();
    part.str("type", "content").set("content", inner.done());
    part.done()
}

/// `toolContent`: text parts, and images given as base64 data URLs.
fn tool_content(content: &[Json]) -> Vec<Json> {
    content
        .iter()
        .filter_map(|part| {
            if get_str(part, "type") == Some("text") {
                return Some(text_content(get_str(part, "text").unwrap_or("")));
            }
            let (mime, data) = image_data(get_str(part, "uri")?)?;
            let mut inner = Obj::new();
            inner.str("type", "image").str("mimeType", mime).str("data", data);
            let mut o = Obj::new();
            o.str("type", "content").set("content", inner.done());
            Some(o.done())
        })
        .collect()
}

/// `/^data:([^;,]+)(?:;[^,]*)*;base64,(.*)$/` for an image type.
fn image_data(uri: &str) -> Option<(&str, &str)> {
    let rest = uri.strip_prefix("data:")?;
    let (header, data) = rest.split_once(',')?;
    let mime = header.split(';').next()?;
    if mime.is_empty() || !mime.starts_with("image/") || !header.ends_with(";base64") || data.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }
    Some((mime, data))
}
