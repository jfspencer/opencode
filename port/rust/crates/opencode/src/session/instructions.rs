//! Instructions (packages/core/src/instructions/index.ts, instructions/builtins.ts, session/instruction-state.ts,
//! codemode/instructions.ts, skill/instructions.ts): keyed sources whose JSON values are hashed and admitted into a
//! session's instruction state. The first admission is the session's baseline (rendered at the start of every model
//! request); later changes are chronological updates (`session.instructions.updated` with `text`, a system message).
//!
//! The sources, in TS's order (most shared first): the Code Mode catalog (`core/codemode`), reference guidance
//! (`core/reference-guidance`, reference/instructions.ts), skill guidance (`core/skill-guidance`), discovered
//! instructions (`core/instructions`: AGENTS.md files and config `instructions`, see [`crate::instruction_discovery`],
//! rendered by instruction-discovery.ts with jsdiff patches for edits), the built-ins (`core/date`,
//! `core/environment`), then the session's API-managed entries. A source that is unavailable (a discovery that
//! failed) keeps its stored value; with nothing stored it blocks the session's first admission. TODO(port): MCP
//! instructions (out of scope, always empty in the reference configuration).

use opencode_protocol::Json;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use crate::app::{App, fatal};
use crate::bus::{Event, LocationRef};
use crate::http::Result;
use crate::json::{self, Obj};

/// One source: its key, its current value (`None` when the source is gone), and how it renders.
pub struct Source {
    pub key: String,
    pub value: Option<Json>,
    /// The read failed temporarily (`Instructions.unavailable`): the stored value stands.
    pub unavailable: bool,
    render: Render,
}

/// How a source renders: as its initial text, as a change, or as its removal.
enum Render {
    Core { initial: fn(&Json) -> Option<String>, changed: fn(&Json, &Json) -> Option<String>, removed: fn(&Json) -> Option<String> },
    /// An API-managed entry (session/instruction-entry.ts), by its client-facing key.
    Entry(String),
}

impl Source {
    fn core(
        key: &str,
        value: Option<Json>,
        initial: fn(&Json) -> Option<String>,
        changed: fn(&Json, &Json) -> Option<String>,
        removed: fn(&Json) -> Option<String>,
    ) -> Self {
        Source { key: key.to_owned(), value, unavailable: false, render: Render::Core { initial, changed, removed } }
    }

    /// An instruction entry's source (`api/<key>`); a removed entry has no value.
    pub fn entry(key: &str, value: Option<Json>) -> Self {
        Source { key: format!("api/{key}"), value, unavailable: false, render: Render::Entry(key.to_owned()) }
    }

    fn initial(&self, value: &Json) -> Option<String> {
        match &self.render {
            Render::Core { initial, .. } => initial(value),
            Render::Entry(key) => Some(entry_block(key, value)),
        }
    }

    fn changed(&self, previous: &Json, value: &Json) -> Option<String> {
        match &self.render {
            Render::Core { changed, .. } => changed(previous, value),
            Render::Entry(key) => Some(format!(
                "The context under \"{key}\" changed and supersedes the previous value:\n{}",
                entry_block(key, value)
            )),
        }
    }

    fn removed(&self, previous: &Json) -> Option<String> {
        match &self.render {
            Render::Core { removed, .. } => removed(previous),
            Render::Entry(key) => Some(format!("The context under \"{key}\" no longer applies. Disregard it.")),
        }
    }
}

/// `renderBlock`: an entry's value as session context (strings as they are, other values as indented JSON).
fn entry_block(key: &str, value: &Json) -> String {
    let rendered = match value {
        Json::String(text) => text.clone(),
        other => crate::tool::arguments::pretty(other),
    };
    format!("<context key=\"{key}\">\n{rendered}\n</context>")
}

/// `canonical`: JSON with object keys sorted (UTF-16 code unit order, as JS `<` compares).
fn canonical(value: &Json) -> String {
    match value {
        Json::Array(items) => format!("[{}]", items.iter().map(canonical).collect::<Vec<_>>().join(",")),
        Json::Object(fields) => {
            let mut keys: Vec<&String> = fields.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let entries: Vec<String> =
                keys.into_iter().map(|key| format!("{}:{}", json::text(&json::string(key)), canonical(&fields[key]))).collect();
            format!("{{{}}}", entries.join(","))
        }
        other => json::text(other),
    }
}

/// `Instructions.hash`: SHA-256 of the canonical JSON, hex.
pub fn hash(value: &Json) -> String {
    let digest = Sha256::digest(canonical(value).as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// --- Code Mode (codemode/instructions.ts) -------------------------------------------------------------------------

fn namespace_line(namespace: &Json) -> String {
    let count = json::get_num(namespace, "count").unwrap_or(0.0) as i64;
    let shown = match json::get(namespace, "entries") {
        Some(Json::Array(entries)) => entries.len() as i64,
        _ => 0,
    };
    let count_text = if count == 1 { "1 tool".to_owned() } else { format!("{count} tools") };
    let label = if shown == count {
        count_text
    } else if shown == 0 {
        format!("{count_text}, none shown")
    } else {
        format!("{count_text}, {shown} shown")
    };
    let description = json::get_str(namespace, "description").map(|d| format!(" // {d}")).unwrap_or_default();
    format!("- {} ({label}){description}", json::get_str(namespace, "name").unwrap_or_default())
}

/// `searchSignature` (@opencode/codemode): the global `search` function a partial catalog points to.
const SEARCH_SIGNATURE: &str = "search({\n  query?: string,\n  namespace?: string,\n  /** @integer @exclusiveMinimum 0 */\n  limit?: number,\n  /** @integer @minimum 0 */\n  offset?: number,\n}): {\n  items: Array<{\n      path: string,\n      description: string,\n      signature: string,\n    }>,\n  /** @integer @minimum 0 */\n  remaining: number,\n  next: {\n      /** @integer @minimum 0 */\n      offset: number,\n    } | null,\n}";

fn code_mode_render(catalog: &Json) -> Option<String> {
    let total = json::get_num(catalog, "total").unwrap_or(0.0);
    let shown = json::get_num(catalog, "shown").unwrap_or(0.0);
    if total == 0.0 {
        return Some("No Code Mode tools are currently available. Later Code Mode catalog updates may add or remove tools. Do not call `execute` unless there is at least one available Code Mode tool.".into());
    }
    let mut lines = Vec::new();
    if let Some(Json::Array(namespaces)) = json::get(catalog, "namespaces") {
        for namespace in namespaces {
            lines.push(namespace_line(namespace));
            if let Some(Json::Array(entries)) = json::get(namespace, "entries") {
                lines.extend(entries.iter().filter_map(|e| json::get_str(e, "line").map(str::to_owned)));
            }
        }
    }
    let partial = shown < total;
    let guidance = if partial {
        format!("The catalog is partial. Before calling a tool that is not listed below, find it with the global `search(...)` function. Do not guess tool names. Each result's `path` is the complete call expression: write it out as code, for example `await tools.ns.tool_name({{ ... }})`. Do not use it as a key such as `tools[path]`.\n\n- {SEARCH_SIGNATURE}")
    } else {
        "The catalog is complete. Do not guess tool names.".to_owned()
    };
    Some(format!(
        "# Code Mode\n\nThe tools listed below{} cannot be called directly: they only work as JavaScript inside the `execute` tool. Every other tool you have is a normal tool call: call it directly, never inside `execute`.\n\n{guidance}\n\n## Code Mode tools (call only inside `execute`)\n\n{}",
        if partial { ", and `search`," } else { "" },
        lines.join("\n")
    ))
}

fn code_mode_changed(_previous: &Json, current: &Json) -> Option<String> {
    // TODO(port): the catalog diff (`CodeModeInstructions.update`); the replacement is always correct.
    code_mode_render(current).map(|text| {
        format!("The Code Mode tool catalog has changed. This catalog supersedes the previous Code Mode tool catalog.\n\n{text}")
    })
}

fn code_mode_removed(_previous: &Json) -> Option<String> {
    Some("Code Mode tools are no longer available. Do not use any previously listed Code Mode tools.".into())
}

// --- Skills (skill/instructions.ts) -------------------------------------------------------------------------------

fn skill_entries(skills: &[Json]) -> Vec<String> {
    skills
        .iter()
        .flat_map(|skill| {
            let get = |key| json::get_str(skill, key).unwrap_or_default().to_owned();
            vec![
                "  <skill>".to_owned(),
                format!("    <id>{}</id>", get("id")),
                format!("    <name>{}</name>", get("name")),
                format!("    <description>{}</description>", get("description")),
                "  </skill>".to_owned(),
            ]
        })
        .collect()
}

fn skills_render(value: &Json) -> Option<String> {
    let Json::Array(skills) = value else { return None };
    let mut lines = vec![
        "Skills provide specialized instructions and workflows for specific tasks.".to_owned(),
        "Use the skill tool to load a skill when a task matches its description.".to_owned(),
        "The user may also invoke a skill directly. When that happens, its instructions appear in the conversation as a <skill_content> block, the same shape the skill tool returns. A skill that is already present this way does not need to be invoked again.".to_owned(),
    ];
    if skills.is_empty() {
        lines.push("No skills are currently available.".into());
    } else {
        lines.push("<available_skills>".into());
        lines.extend(skill_entries(skills));
        lines.push("</available_skills>".into());
    }
    Some(lines.join("\n"))
}

fn skills_changed(previous: &Json, current: &Json) -> Option<String> {
    let (Json::Array(before), Json::Array(after)) = (previous, current) else { return skills_render(current) };
    let id = |s: &Json| json::get_str(s, "id").unwrap_or_default().to_owned();
    let differs = |a: &Json, b: &Json| json::get(a, "name") != json::get(b, "name") || json::get(a, "description") != json::get(b, "description");
    let added: Vec<Json> = after.iter().filter(|s| !before.iter().any(|b| id(b) == id(s))).cloned().collect();
    let removed: Vec<String> = before.iter().filter(|s| !after.iter().any(|a| id(a) == id(s))).map(id).collect();
    let changed: Vec<Json> =
        after.iter().filter(|s| before.iter().find(|b| id(b) == id(s)).is_some_and(|b| differs(b, s))).cloned().collect();
    if added.is_empty() && changed.is_empty() && removed.is_empty() {
        return skills_render(current).map(|text| {
            format!("The available skills have changed. This list supersedes the previous available skills list.\n{text}")
        });
    }
    let mut lines = Vec::new();
    if !added.is_empty() {
        lines.push("New skills are available in addition to those previously listed:".to_owned());
        lines.extend(skill_entries(&added));
    }
    if !changed.is_empty() {
        lines.push("The following skills have changed. Each entry replaces the previously listed skill with the same ID:".to_owned());
        lines.extend(skill_entries(&changed));
    }
    if !removed.is_empty() {
        lines.push(format!("The following skill IDs are no longer available and must not be used: {}.", removed.join(", ")));
    }
    Some(lines.join("\n"))
}

fn skills_removed(_previous: &Json) -> Option<String> {
    Some("Skill guidance is no longer available. Do not use any previously listed skill.".into())
}

// --- References (reference/instructions.ts) -----------------------------------------------------------------------

fn reference_entries(references: &[Json]) -> Vec<String> {
    references
        .iter()
        .flat_map(|reference| {
            let mut lines = vec![
                "  <reference>".to_owned(),
                format!("    <name>{}</name>", json::get_str(reference, "name").unwrap_or_default()),
                format!("    <path>{}</path>", json::get_str(reference, "path").unwrap_or_default()),
            ];
            if let Some(description) = json::get_str(reference, "description") {
                lines.push(format!("    <description>{description}</description>"));
            }
            lines.push("  </reference>".to_owned());
            lines
        })
        .collect()
}

fn references_render(value: &Json) -> Option<String> {
    let Json::Array(references) = value else { return None };
    let mut lines = vec![
        "Project references provide additional directories that can be accessed when relevant.".to_owned(),
        "<available_references>".to_owned(),
    ];
    lines.extend(reference_entries(references));
    lines.push("</available_references>".to_owned());
    Some(lines.join("\n"))
}

fn references_changed(previous: &Json, current: &Json) -> Option<String> {
    let (Json::Array(before), Json::Array(after)) = (previous, current) else { return references_render(current) };
    let name = |r: &Json| json::get_str(r, "name").unwrap_or_default().to_owned();
    let differs =
        |a: &Json, b: &Json| json::get(a, "path") != json::get(b, "path") || json::get(a, "description") != json::get(b, "description");
    let added: Vec<Json> = after.iter().filter(|r| !before.iter().any(|b| name(b) == name(r))).cloned().collect();
    let removed: Vec<String> = before.iter().filter(|r| !after.iter().any(|a| name(a) == name(r))).map(name).collect();
    let changed: Vec<Json> =
        after.iter().filter(|r| before.iter().find(|b| name(b) == name(r)).is_some_and(|b| differs(b, r))).cloned().collect();
    if added.is_empty() && changed.is_empty() && removed.is_empty() {
        return references_render(current).map(|text| {
            format!("The available project references have changed. This list supersedes the previous reference list.\n{text}")
        });
    }
    let mut lines = Vec::new();
    if !added.is_empty() {
        lines.push("New project references are available in addition to those previously listed:".to_owned());
        lines.extend(reference_entries(&added));
    }
    if !changed.is_empty() {
        lines.push(
            "The following project references have changed. Each entry replaces the previously listed reference with the same name:"
                .to_owned(),
        );
        lines.extend(reference_entries(&changed));
    }
    if !removed.is_empty() {
        lines.push(format!(
            "The following project references are no longer available and must not be used: {}.",
            removed.join(", ")
        ));
    }
    Some(lines.join("\n"))
}

fn references_removed(_previous: &Json) -> Option<String> {
    Some("Project reference guidance is no longer available. Do not use previously listed references.".into())
}

/// `ReferenceInstructions.load`: the references that describe themselves, by name (`localeCompare`), as
/// `{name, path, description}`; `None` (removed) when there are none.
pub fn reference_guidance(references: &[opencode_protocol::types::ReferenceInfo]) -> Option<Json> {
    let mut available: Vec<&opencode_protocol::types::ReferenceInfo> =
        references.iter().filter(|reference| reference.description.is_some()).collect();
    available.sort_by(|a, b| crate::collate::compare(&a.name, &b.name));
    if available.is_empty() {
        return None;
    }
    Some(Json::Array(
        available
            .into_iter()
            .map(|reference| {
                let mut o = Obj::new();
                o.str("name", &reference.name).str("path", &reference.path);
                o.opt_str("description", reference.description.as_deref());
                o.done()
            })
            .collect(),
    ))
}

// --- Discovered instructions (instruction-discovery.ts) -----------------------------------------------------------

fn files(value: &Json) -> Option<Vec<(String, String)>> {
    let Json::Array(items) = value else { return None };
    items
        .iter()
        .map(|item| Some((json::get_str(item, "path")?.to_owned(), json::get_str(item, "content")?.to_owned())))
        .collect()
}

fn files_text(files: &[(String, String)]) -> String {
    files.iter().map(|(path, content)| format!("Instructions from: {path}\n{content}")).collect::<Vec<_>>().join("\n\n")
}

fn discovered_render(value: &Json) -> Option<String> {
    files(value).map(|files| files_text(&files))
}

fn discovered_changed(previous: &Json, current: &Json) -> Option<String> {
    let (Some(before), Some(after)) = (files(previous), files(current)) else { return discovered_render(current) };
    let mut parts = Vec::new();
    for (path, _) in before.iter().filter(|(path, _)| !after.iter().any(|(p, _)| p == path)) {
        parts.push(format!("The instructions from {path} no longer apply."));
    }
    for file in after.iter().filter(|(path, _)| !before.iter().any(|(p, _)| p == path)) {
        parts.push(format!("New instructions apply from:\n{}", files_text(std::slice::from_ref(file))));
    }
    for (path, content) in &after {
        let Some((_, old)) = before.iter().find(|(p, c)| p == path && c != content) else { continue };
        let patch = crate::jsdiff::create_patch(path, old, content, "", "", 3);
        let hunks = patch.find("@@").map(|i| &patch[i..]).unwrap_or_default();
        let diff = format!("The instructions from {path} changed. Here's the diff:\n```diff\n{}\n```", hunks.trim_end());
        let replacement = format!("The instructions changed:\n{}", files_text(&[(path.clone(), content.clone())]));
        // The shorter of the two, in UTF-16 units as JS measures.
        parts.push(if diff.encode_utf16().count() < replacement.encode_utf16().count() { diff } else { replacement });
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn discovered_removed(_previous: &Json) -> Option<String> {
    Some("Previously loaded instructions no longer apply.".into())
}

// --- Built-ins (instructions/builtins.ts) -------------------------------------------------------------------------

fn date_render(value: &Json) -> Option<String> {
    json::as_str(value).map(|date| format!("Today's date: {date}"))
}

fn date_changed(_previous: &Json, value: &Json) -> Option<String> {
    json::as_str(value).map(|date| format!("Today's date is now: {date}"))
}

fn environment_render(value: &Json) -> Option<String> {
    json::as_str(value).map(|env| format!("Here is some useful information about the environment you are running in:\n{env}"))
}

fn environment_changed(_previous: &Json, value: &Json) -> Option<String> {
    json::as_str(value).map(|env| format!("The environment you are running in is now:\n{env}"))
}

fn none(_: &Json) -> Option<String> {
    None
}

/// `new Date().toDateString()` in the local time zone (SQLite's `localtime`, which honors `TZ` as the C library does).
pub fn date_string(db: &Connection) -> String {
    let parts: Option<String> = db
        .query_row("SELECT strftime('%w %m %d %Y', 'now', 'localtime')", [], |row| row.get(0))
        .optional()
        .ok()
        .flatten();
    let parts = parts.unwrap_or_default();
    let mut fields = parts.split(' ');
    let weekday: usize = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let month: usize = fields.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    let day = fields.next().unwrap_or("01");
    let year = fields.next().unwrap_or("1970");
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{} {} {day} {year}", DAYS[weekday % 7], MONTHS[(month + 11) % 12])
}

/// `Global.Path.tmp` as the running server canonicalized it: `realpath($TMPDIR/opencode)`.
pub fn global_tmp() -> String {
    let base = std::env::var("TMPDIR").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/tmp".into());
    let base = base.trim_end_matches('/');
    let base = if base.is_empty() { "/" } else { base };
    let path = format!("{base}/opencode");
    let _ = std::fs::create_dir_all(&path);
    std::fs::canonicalize(&path).map(|p| p.to_string_lossy().into_owned()).unwrap_or(path)
}

/// `SkillInstructions.load`: the skills the ruleset does not deny that describe themselves and may be invoked by the
/// model, by ID (`localeCompare`), as `{id, name, description}`; `None` (removed) when there are none.
pub fn skill_guidance(skills: &[opencode_protocol::types::SkillInfo], rules: &[Json]) -> Option<Json> {
    let mut available: Vec<&opencode_protocol::types::SkillInfo> = skills
        .iter()
        .filter(|skill| crate::tool::evaluate("skill", &skill.id.0, rules) != "deny")
        .filter(|skill| skill.description.is_some() && skill.autoinvoke != Some(false))
        .collect();
    available.sort_by(|a, b| crate::collate::compare(&a.id.0, &b.id.0));
    if available.is_empty() {
        return None;
    }
    Some(Json::Array(
        available
            .into_iter()
            .map(|skill| {
                let mut o = Obj::new();
                o.str("id", &skill.id.0).str("name", &skill.name.0);
                o.str("description", skill.description.as_deref().unwrap_or_default());
                o.done()
            })
            .collect(),
    ))
}

/// The session's sources (`SessionContext.select`'s `Instructions.combine`).
pub fn sources(
    db: &Connection,
    directory: &str,
    project_directory: &str,
    git: bool,
    catalog: Option<Json>,
    skills: Option<Json>,
    inventory: &crate::inventory::Inventory,
) -> Vec<Source> {
    let environment = [
        "<env>".to_owned(),
        format!("  Working directory: {directory}"),
        format!("  Workspace root folder: {project_directory}"),
        format!("  Is directory a git repo: {}", if git { "yes" } else { "no" }),
        format!("  Platform: {}", std::env::consts::OS),
        format!(
            "  Prefer {} over generic system temporary directories such as /tmp; it is pre-created and approved for external access.",
            global_tmp()
        ),
        "</env>".to_owned(),
    ]
    .join("\n");
    let discovered = inventory.instructions.value();
    let mut instructions =
        Source::core("core/instructions", discovered.clone().flatten(), discovered_render, discovered_changed, discovered_removed);
    instructions.unavailable = discovered.is_none();
    let references = reference_guidance(&inventory.references);
    vec![
        Source::core("core/codemode", catalog, code_mode_render, code_mode_changed, code_mode_removed),
        Source::core("core/reference-guidance", references, references_render, references_changed, references_removed),
        Source::core("core/skill-guidance", skills, skills_render, skills_changed, skills_removed),
        instructions,
        Source::core("core/date", Some(json::string(&date_string(db))), date_render, date_changed, none),
        Source::core("core/environment", Some(json::string(&environment)), environment_render, environment_changed, none),
    ]
}

/// A session's instruction state (`instruction_state`): key → hash maps.
struct State {
    initial: Json,
    current: Json,
}

fn state(db: &Connection, session: &str) -> rusqlite::Result<Option<State>> {
    db.query_row(
        "SELECT initial_values, current_values FROM instruction_state WHERE session_id = ?1",
        params![session],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .optional()
    .map(|found| {
        found.map(|(initial, current)| State {
            initial: json::parse(&initial).unwrap_or_else(json::object),
            current: json::parse(&current).unwrap_or_else(json::object),
        })
    })
}

fn blob(db: &Connection, hash: &str) -> rusqlite::Result<Option<Json>> {
    let text: Option<Option<String>> =
        db.query_row("SELECT value FROM instruction_blob WHERE hash = ?1", params![hash], |r| r.get(0)).optional()?;
    Ok(text.flatten().and_then(|t| json::parse(&t)))
}

/// What observing the sources against the recorded state found (`Instructions.diff`).
struct Admission {
    /// No state was recorded: the observation is the epoch's baseline.
    initial: bool,
    delta: Json,
    blobs: Vec<(String, Json)>,
    /// `Instructions.renderUpdate`: the changed sources' texts (empty for the baseline).
    text: String,
}

/// Why sources could not be observed.
#[derive(Debug)]
pub enum Error {
    Db(rusqlite::Error),
    /// `Instructions.InitializationBlocked`: a source is unavailable and the session has no instructions yet.
    Blocked(Vec<String>),
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Error::Db(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Db(error) => write!(f, "{error}"),
            Error::Blocked(keys) => write!(f, "Instruction initialization blocked by unavailable sources: {}", keys.join(", ")),
        }
    }
}

/// The keys of the unavailable sources.
fn blocked(sources: &[Source]) -> Vec<String> {
    sources.iter().filter(|s| s.unavailable).map(|s| s.key.clone()).collect()
}

/// Observes the sources against the session's recorded state; `None` when nothing changed since it.
fn observe(db: &Connection, session: &str, sources: &[Source]) -> std::result::Result<Option<Admission>, Error> {
    let stored = state(db, session)?;
    let initial = stored.is_none();
    if initial && !blocked(sources).is_empty() {
        return Err(Error::Blocked(blocked(sources)));
    }
    let previous = stored.as_ref().map(|s| s.current.clone()).unwrap_or_else(json::object);
    let mut delta = Obj::new();
    let mut blobs: Vec<(String, Json)> = Vec::new();
    let mut changes: Vec<(&Source, Option<Json>)> = Vec::new();
    for source in sources.iter().filter(|source| !source.unavailable) {
        match &source.value {
            None => {
                if json::get(&previous, &source.key).is_some() {
                    delta.str(&source.key, "removed");
                    changes.push((source, None));
                }
            }
            Some(value) => {
                let next = hash(value);
                if json::get_str(&previous, &source.key) == Some(next.as_str()) {
                    continue;
                }
                delta.str(&source.key, &next);
                blobs.push((next, value.clone()));
                changes.push((source, Some(value.clone())));
            }
        }
    }
    let delta = delta.done();
    let empty = matches!(&delta, Json::Object(o) if o.is_empty());
    if !initial && empty {
        return Ok(None);
    }
    let mut parts = Vec::new();
    if !initial {
        for (source, next) in &changes {
            let before = match json::get_str(&previous, &source.key) {
                Some(hash) => blob(db, hash)?,
                None => None,
            };
            let text = match (before, next) {
                (Some(before), None) => source.removed(&before),
                (None, None) => None,
                (Some(before), Some(next)) => source.changed(&before, next),
                (None, Some(next)) => source.initial(next),
            };
            parts.extend(text);
        }
    }
    Ok(Some(Admission { initial, delta, blobs, text: parts.join("\n\n") }))
}

/// `InstructionState.prepare`: observes the sources and admits what changed (the whole baseline the first time).
pub fn prepare(app: &App, session: &str, sources: &[Source], location: &LocationRef) -> Result<()> {
    let observed = observe(&app.db.lock().unwrap(), session, sources).map_err(|error| match error {
        Error::Db(error) => fatal(error),
        blocked => {
            let mut o = Obj::new();
            o.str("_tag", "Instructions.InitializationBlocked").str("message", &blocked.to_string());
            crate::http::Failure::new(500, o.done())
        }
    })?;
    let Some(Admission { initial, delta, blobs, text }) = observed else { return Ok(()) };
    let mut data = Obj::new();
    data.str("sessionID", session).set("delta", delta);
    if !text.is_empty() {
        data.str("text", &text);
    }
    let metadata = initial.then(|| {
        let mut inner = Obj::new();
        inner.set("initial", Json::Bool(true));
        let mut o = Obj::new();
        o.set("instructions", inner.done());
        o.done()
    });
    crate::session::publish_committed(app, "session.instructions.updated", data.done(), Some(location.clone()), metadata, |tx, _| {
        for (hash, value) in &blobs {
            tx.execute(
                "INSERT INTO instruction_blob (hash, value) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
                params![hash, json::text(value)],
            )?;
        }
        Ok(())
    })
    .map_err(fatal)?;
    Ok(())
}

/// `InstructionState.preview`: the initial instructions and the pending update as the next step would see them,
/// without recording anything.
pub fn preview(db: &Connection, session: &str, sources: &[Source]) -> std::result::Result<(Vec<(String, String)>, String), Error> {
    if state(db, session)?.is_none() {
        if !blocked(sources).is_empty() {
            return Err(Error::Blocked(blocked(sources)));
        }
        let initial = sources
            .iter()
            .filter_map(|source| source.value.as_ref().and_then(|value| source.initial(value)).map(|text| (source.key.clone(), text)))
            .collect();
        return Ok((initial, String::new()));
    }
    let update = observe(db, session, sources)?.map(|admission| admission.text).unwrap_or_default();
    Ok((initial(db, session, sources)?, update))
}

/// `InstructionState.apply` (the projection of `session.instructions.updated`).
pub fn apply(tx: &Transaction, event: &Event) -> rusqlite::Result<()> {
    let session = json::get_str(&event.data, "sessionID").unwrap_or_default();
    let seq = event.durable.as_ref().map(|d| d.seq).unwrap_or(0);
    let stored = state(tx, session)?;
    let mut current = stored.as_ref().map(|s| s.current.clone()).unwrap_or_else(json::object);
    if let Some(Json::Object(delta)) = json::get(&event.data, "delta") {
        for (key, value) in delta {
            if json::as_str(value) == Some("removed") {
                json::remove(&mut current, key);
            } else {
                json::set(&mut current, key, value.clone());
            }
        }
    }
    let text = json::text(&current);
    if stored.is_none() {
        tx.execute(
            "INSERT INTO instruction_state (session_id, epoch_start, through_seq, initial_values, current_values) \
             VALUES (?1, ?2, ?2, ?3, ?3) ON CONFLICT DO NOTHING",
            params![session, seq, text],
        )?;
    } else {
        tx.execute(
            "UPDATE instruction_state SET through_seq = ?1, current_values = ?2 WHERE session_id = ?3",
            params![seq, text, session],
        )?;
    }
    Ok(())
}

/// `InstructionState.initial`: the epoch baseline's rendered texts, in source order.
pub fn initial(db: &Connection, session: &str, sources: &[Source]) -> rusqlite::Result<Vec<(String, String)>> {
    let Some(state) = state(db, session)? else { return Ok(vec![]) };
    let mut out = Vec::new();
    for source in sources {
        let Some(hash) = json::get_str(&state.initial, &source.key) else { continue };
        let Some(value) = blob(db, hash)? else { continue };
        if let Some(text) = source.initial(&value) {
            out.push((source.key.clone(), text));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_sorts_keys() {
        let value = json::parse(r#"{"b":1,"a":[{"d":true,"c":null}]}"#).unwrap();
        assert_eq!(canonical(&value), r#"{"a":[{"c":null,"d":true}],"b":1}"#);
        assert_eq!(hash(&json::string("x")).len(), 64);
    }

    #[test]
    fn entries_render_as_context_blocks() {
        let value = json::parse(r#"{"lead":"sam","size":4,"tags":[],"nested":{"a":[1,"b"]}}"#).unwrap();
        assert_eq!(
            entry_block("team", &value),
            "<context key=\"team\">\n{\n  \"lead\": \"sam\",\n  \"size\": 4,\n  \"tags\": [],\n  \"nested\": {\n    \"a\": [\n      1,\n      \"b\"\n    ]\n  }\n}\n</context>"
        );
        assert_eq!(entry_block("style", &json::string("Answer tersely.")), "<context key=\"style\">\nAnswer tersely.\n</context>");
        let source = Source::entry("style", None);
        assert_eq!(source.key, "api/style");
        assert_eq!(source.removed(&Json::Null).as_deref(), Some("The context under \"style\" no longer applies. Disregard it."));
    }
}
