//! Instructions (packages/core/src/instructions/index.ts, instructions/builtins.ts, session/instruction-state.ts,
//! codemode/instructions.ts, skill/instructions.ts): keyed sources whose JSON values are hashed and admitted into a
//! session's instruction state. The first admission is the session's baseline (rendered at the start of every model
//! request); later changes are chronological updates (`session.instructions.updated` with `text`, a system message).
//!
//! The sources, in TS's order (most shared first): the Code Mode catalog (`core/codemode`), skill guidance
//! (`core/skill-guidance`), then the built-ins (`core/date`, `core/environment`). TODO(port): MCP and reference
//! instructions (out of scope, always empty in the reference configuration), instruction discovery (AGENTS.md and
//! friends), session instruction entries.

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
        Source { key: key.to_owned(), value, render: Render::Core { initial, changed, removed } }
    }

    /// An instruction entry's source (`api/<key>`); a removed entry has no value.
    pub fn entry(key: &str, value: Option<Json>) -> Self {
        Source { key: format!("api/{key}"), value, render: Render::Entry(key.to_owned()) }
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

/// `JSON.stringify(value, null, 2)`.
fn pretty(value: &Json, indent: usize, out: &mut String) {
    let pad = |n: usize| "  ".repeat(n);
    match value {
        Json::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                pretty(item, indent + 1, out);
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
                pretty(item, indent + 1, out);
                out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(indent));
            out.push('}');
        }
        other => out.push_str(&json::text(other)),
    }
}

/// `renderBlock`: an entry's value as session context (strings as they are, other values as indented JSON).
fn entry_block(key: &str, value: &Json) -> String {
    let rendered = match value {
        Json::String(text) => text.clone(),
        other => {
            let mut out = String::new();
            pretty(other, 0, &mut out);
            out
        }
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

fn code_mode_render(catalog: &Json) -> Option<String> {
    let total = json::get_num(catalog, "total").unwrap_or(0.0);
    let shown = json::get_num(catalog, "shown").unwrap_or(0.0);
    if total == 0.0 {
        return Some("No Code Mode tools are currently available. Later Code Mode catalog updates may add or remove tools. Do not call `execute` unless there is at least one available Code Mode tool.".into());
    }
    if shown < total {
        // TODO(port): the partial catalog's `search(...)` guidance (needs the codemode search signature).
        return None;
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
    Some(format!(
        "# Code Mode\n\nThe tools listed below cannot be called directly: they only work as JavaScript inside the `execute` tool. Every other tool you have is a normal tool call: call it directly, never inside `execute`.\n\nThe catalog is complete. Do not guess tool names.\n\n## Code Mode tools (call only inside `execute`)\n\n{}",
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
    vec![
        Source::core("core/codemode", catalog, code_mode_render, code_mode_changed, code_mode_removed),
        Source::core("core/skill-guidance", skills, skills_render, skills_changed, skills_removed),
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

/// Observes the sources against the session's recorded state; `None` when nothing changed since it.
fn observe(db: &Connection, session: &str, sources: &[Source]) -> rusqlite::Result<Option<Admission>> {
    let stored = state(db, session)?;
    let initial = stored.is_none();
    let previous = stored.as_ref().map(|s| s.current.clone()).unwrap_or_else(json::object);
    let mut delta = Obj::new();
    let mut blobs: Vec<(String, Json)> = Vec::new();
    let mut changes: Vec<(&Source, Option<Json>)> = Vec::new();
    for source in sources {
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
    let observed = observe(&app.db.lock().unwrap(), session, sources).map_err(fatal)?;
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
pub fn preview(db: &Connection, session: &str, sources: &[Source]) -> rusqlite::Result<(Vec<(String, String)>, String)> {
    if state(db, session)?.is_none() {
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
