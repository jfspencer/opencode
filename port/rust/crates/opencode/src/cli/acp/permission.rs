//! Permission asks through the ACP client (packages/cli/src/acp/permission.ts): `session/request_permission` with
//! the tool call, its locations and diff previews of file changes; the client's choice becomes the reply. Also the
//! diffs a completed edit carries.

use opencode_protocol::Json;

use super::tool::{self, absolute_path, canonical, file_path, patch_hunks};
use super::translate::{Child, DiffSource, Tool, child_meta, child_tool_call_id, prefix_title};
use crate::cli::client::Client;
use crate::json::{self, Obj, get, get_str};
use crate::jsdiff;
use crate::tool::patch_text::Hunk;

pub struct Ask {
    pub event: Json,
    pub session_id: String,
    pub client_session_id: String,
    pub cwd: String,
    pub tool: Option<Tool>,
    pub child: Option<Child>,
}

fn options() -> Json {
    let option = |id: &str, kind: &str, name: &str| {
        let mut o = Obj::new();
        o.str("optionId", id).str("kind", kind).str("name", name);
        o.done()
    };
    Json::Array(vec![option("once", "allow_once", "Allow once"), option("always", "allow_always", "Always allow"), option("reject", "reject_once", "Reject")])
}

/// The `session/request_permission` parameters (`ACPPermission.ask`).
pub async fn request(ask: &Ask) -> Json {
    let data = get(&ask.event, "data").cloned().unwrap_or(Json::Null);
    let tool_name = ask.tool.as_ref().map(|t| t.name.clone()).unwrap_or_else(|| get_str(&data, "action").unwrap_or("").to_owned());
    let tool_input = ask.tool.as_ref().map(|t| t.input.clone()).or_else(|| get(&data, "metadata").cloned()).unwrap_or_else(json::object);
    let previews = previews(&tool_name, &tool_input, get(&data, "metadata"), &ask.cwd).await.unwrap_or_default();
    let title = title(&tool_name, &tool_input, &previews);
    let child = ask.child.as_ref();
    let hint = match &title {
        Some(title) => Some(prefix_title(child, title)),
        None => child.and_then(|c| c.title.clone()),
    };
    let id = child_tool_call_id(child, ask.tool.as_ref().map(|t| t.id.as_str()).unwrap_or_else(|| get_str(&data, "id").unwrap_or("")));
    let mut call = tool::pending(&id, &tool_name, &tool_input, hint.as_deref(), &ask.cwd).done();
    json::remove(&mut call, "name");
    if let Some(tool) = &ask.tool {
        json::set(&mut call, "name", Json::String(tool.name.clone()));
    }
    if ask.tool.is_none() {
        json::remove(&mut call, "rawInput");
    }
    json::set(&mut call, "locations", tool::locations_json(&locations(&tool_name, &tool_input, &data, &ask.cwd)));
    if !previews.is_empty() {
        json::set(&mut call, "content", Json::Array(previews.iter().map(Preview::json).collect()));
    }
    if let Some(child) = child {
        json::set(&mut call, "_meta", child_meta(child));
    }
    let mut params = Obj::new();
    params.str("sessionId", &ask.client_session_id).set("toolCall", call).set("options", options());
    params.done()
}

/// The decision a client's answer stands for: only a selected allow option allows.
pub fn decision(answer: &std::result::Result<Json, Json>) -> &'static str {
    let selected = answer.as_ref().ok().and_then(|r| get(r, "outcome")).filter(|o| get_str(o, "outcome") == Some("selected")).and_then(|o| get_str(o, "optionId"));
    match selected {
        Some("once") => "once",
        Some("always") => "always",
        _ => "reject",
    }
}

/// `ACPPermission.respond`.
pub async fn respond(client: &Client, ask: &Ask, decision: &str) {
    let mut body = Obj::new();
    body.str("decision", decision);
    let request = get(&ask.event, "data").and_then(|d| get_str(d, "id")).unwrap_or("").to_owned();
    let _ = client.call("session.permission.reply", &[("sessionID", &ask.session_id), ("requestID", &request)], None, Some(body.done())).await;
}

pub struct Preview {
    path: String,
    old_text: Option<String>,
    new_text: String,
}

impl Preview {
    fn json(&self) -> Json {
        let mut o = Obj::new();
        o.str("type", "diff").str("path", &self.path).set("oldText", self.old_text.clone().map(Json::String).unwrap_or(Json::Null)).str("newText", &self.new_text);
        o.done()
    }
}

/// A file change a tool reports (`FileDiff.Info`).
struct FileDiff {
    file: String,
    patch: String,
    status: String,
}

/// `Schema.decodeUnknownOption(Schema.Array(FileDiff.Info))(metadata?.files)`: all of them, or none.
fn decode_files(metadata: Option<&Json>) -> Vec<FileDiff> {
    let Some(files) = metadata.and_then(|m| get(m, "files")).and_then(json::array) else { return vec![] };
    let count = |v: &Json, key: &str| matches!(get(v, key), Some(Json::Number(n)) if *n >= 0.0 && n.fract() == 0.0);
    let mut out = vec![];
    for file in files {
        let (Some(name), Some(patch), Some(status)) = (get_str(file, "file"), get_str(file, "patch"), get_str(file, "status")) else { return vec![] };
        let more = match get(file, "more") {
            None => true,
            Some(value) => *value == Json::Bool(true),
        };
        if !count(file, "additions") || !count(file, "deletions") || !matches!(status, "added" | "deleted" | "modified") || !more {
            return vec![];
        }
        out.push(FileDiff { file: name.to_owned(), patch: patch.to_owned(), status: status.to_owned() });
    }
    out
}

async fn read_text(path: &str) -> Option<String> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || std::fs::read(&path).ok().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())).await.ok().flatten()
}

/// `permissionPreviews`; `None` where TS's preview effect fails (and shows none).
async fn previews(tool_name: &str, input: &Json, metadata: Option<&Json>, cwd: &str) -> Option<Vec<Preview>> {
    if canonical(tool_name) == "patch" {
        return patch_previews(input, cwd).await;
    }
    let mut out = vec![];
    for file in decode_files(metadata) {
        let path = absolute_path(&file.file, cwd);
        let old_text = if file.status == "added" { None } else { Some(read_text(&path).await?) };
        let hunks = jsdiff::parse_patch(&file.patch).ok()?;
        if hunks.len() > 1 {
            return None;
        }
        if let Some(new_text) = jsdiff::apply_patch(old_text.as_deref().unwrap_or(""), hunks.first().map(Vec::as_slice).unwrap_or(&[])) {
            out.push(Preview { path, old_text, new_text });
        }
    }
    Some(out)
}

async fn patch_previews(input: &Json, cwd: &str) -> Option<Vec<Preview>> {
    let mut out = vec![];
    for hunk in patch_hunks(input) {
        let path = absolute_path(hunk.path(), cwd);
        out.push(match &hunk {
            Hunk::Add { contents, .. } => {
                let new_text = if contents.ends_with('\n') || contents.is_empty() { contents.clone() } else { format!("{contents}\n") };
                Preview { path, old_text: None, new_text }
            }
            Hunk::Delete { .. } => Preview { path: path.clone(), old_text: Some(read_text(&path).await?), new_text: String::new() },
            Hunk::Update { path: relative, move_path, chunks } => {
                let old_text = read_text(&path).await?;
                let derived = crate::tool::patch_text::derive(relative, chunks, &old_text).ok()?;
                let target = move_path.as_deref().map(|m| absolute_path(m, cwd)).unwrap_or(path);
                Preview { path: target, old_text: Some(old_text), new_text: derived.content }
            }
        });
    }
    Some(out)
}

/// `permissionTitle`.
fn title(tool_name: &str, input: &Json, previews: &[Preview]) -> Option<String> {
    if previews.len() > 1 {
        return Some(format!("{} files", previews.len()));
    }
    let value = |key: &str| get_str(input, key).map(String::from);
    match canonical(tool_name).as_str() {
        "external_directory" => value("description").or_else(|| value("command")).or_else(|| value("parentDir")),
        "webfetch" => value("url"),
        "websearch" => value("query"),
        "grep" | "glob" => value("pattern"),
        "read" | "edit" | "write" | "patch" => file_path(input).map(String::from).or_else(|| previews.first().map(|p| p.path.clone())),
        _ => None,
    }
}

/// `permissionLocations`: the tool's locations, or for path actions the asked resources.
fn locations(tool_name: &str, input: &Json, ask: &Json, cwd: &str) -> Vec<String> {
    let found = tool::location_paths(tool_name, input, cwd);
    if !found.is_empty() || !matches!(get_str(ask, "action"), Some("read" | "edit" | "external_directory")) {
        return found;
    }
    let mut out: Vec<String> = vec![];
    for resource in get(ask, "resources").and_then(json::array).unwrap_or(&[]).iter().filter_map(json::as_str) {
        let path = resource.strip_suffix("/*").unwrap_or(resource);
        if path.is_empty() || path.contains(['*', '?']) {
            continue;
        }
        let path = absolute_path(path, cwd);
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// `ACPPermission.withCompletedDiffs`: a completed tool call with the diffs of the files it changed.
pub async fn with_completed_diffs(mut update: Json, source: Option<&DiffSource>, cwd: &str) -> Json {
    let Some(source) = source else { return update };
    if get_str(&update, "sessionUpdate") != Some("tool_call_update") {
        return update;
    }
    let hunks = if canonical(&source.tool_name) == "patch" { patch_hunks(&source.input) } else { vec![] };
    let mut diffs = vec![];
    for file in decode_files(source.metadata.as_ref()) {
        if let Some(preview) = completed_diff(&file, &hunks, cwd).await {
            diffs.push(preview.json());
        }
    }
    if diffs.is_empty() {
        return update;
    }
    let mut content = get(&update, "content").and_then(json::array).map(<[Json]>::to_vec).unwrap_or_default();
    content.extend(diffs);
    json::set(&mut update, "content", Json::Array(content));
    update
}

async fn completed_diff(file: &FileDiff, hunks: &[Hunk], cwd: &str) -> Option<Preview> {
    let path = absolute_path(&file.file, cwd);
    let new_text = if file.status == "deleted" { String::new() } else { read_text(&path).await? };
    if file.status == "added" {
        return Some(Preview { path, old_text: None, new_text });
    }
    let recorded = jsdiff::parse_patch(&file.patch).ok()?.into_iter().next()?;
    if let Some(old_text) = jsdiff::apply_patch(&new_text, &jsdiff::reverse_patch(&recorded)) {
        return Some(Preview { path, old_text: Some(old_text), new_text });
    }
    // Core trims indentation from patch-tool diffs, so rebuild those from its hunks and keep them if the ranges agree.
    let (relative, chunks) = hunks.iter().find_map(|hunk| match hunk {
        Hunk::Update { path: relative, move_path, chunks } if absolute_path(move_path.as_deref().unwrap_or(relative), cwd) == path => Some((relative, chunks)),
        _ => None,
    })?;
    if chunks.iter().any(|c| c.old_lines.is_empty() || c.new_lines.is_empty()) {
        return None;
    }
    let reversed: Vec<_> = chunks
        .iter()
        .map(|c| crate::tool::patch_text::Chunk { old_lines: c.new_lines.clone(), new_lines: c.old_lines.clone(), ..c.clone() })
        .collect();
    let rebuilt = crate::tool::patch_text::derive(relative, &reversed, &new_text).ok()?.content;
    let ranges = |hunks: Vec<(i64, i64, i64, i64)>| hunks.iter().map(|(a, b, c, d)| format!("{a},{b},{c},{d}")).collect::<Vec<_>>().join(" ");
    let derived = jsdiff::hunks(&rebuilt, &new_text, 4).into_iter().map(|h| (h.old_start as i64, h.old_lines as i64, h.new_start as i64, h.new_lines as i64)).collect();
    let recorded = recorded.iter().map(|h| (h.old_start, h.old_lines, h.new_start, h.new_lines)).collect();
    (ranges(derived) == ranges(recorded)).then_some(Preview { path, old_text: Some(rebuilt), new_text })
}
