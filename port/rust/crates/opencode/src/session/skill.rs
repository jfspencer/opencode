//! Skills in a session (packages/core/src/skill.ts `prepare` / `toModelOutput`, session/skill.ts, session.ts `skill`):
//! a skill's content as the model reads it (attached to a prompt or loaded by the skill tool), and activating a skill
//! (`session.skill.activated`, projected as a `skill` message).
//!
//! TODO(port): the virtual files of skills that plugins add with files (none of the built-in ones have any).

use opencode_protocol::Json;
use opencode_protocol::types as t;

use crate::app::{App, fatal};
use crate::http::{Failure, Result};
use crate::json::{self, Obj};
use crate::session;

const IGNORED: [&str; 5] = ["node_modules", ".git", ".venv", "venv", "__pycache__"];

/// `Skill.uri`.
pub fn uri(id: &str, file: Option<&str>) -> String {
    match file {
        Some(file) => format!("skill://{id}/{file}"),
        None => format!("skill://{id}"),
    }
}

/// The files under a skill's directory (`fs.scan("**/*", { dot, include: "file", ignore: FSUtil.ignoreGlobs })`),
/// as absolute paths; symbolic links are not followed.
fn scan(directory: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else { return };
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if IGNORED.contains(&name.as_str()) {
            continue;
        }
        let path = format!("{directory}/{name}");
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => scan(&path, out),
            Ok(kind) if kind.is_file() => out.push(path),
            Ok(kind) if kind.is_symlink() => {
                if std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
                    out.push(path);
                }
            }
            _ => {}
        }
    }
}

/// `Skill.prepare(...).output` (`toModelOutput`): the skill's content with its base directory and up to ten of its
/// files.
pub fn prepare(skill: &t::SkillInfo) -> String {
    let path = skill.path.0.as_str();
    let (directory, files) = if path.starts_with("skill://") {
        (uri(&skill.id.0, None), Vec::new())
    } else {
        let directory = std::path::Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let mut files = Vec::new();
        if std::path::Path::new(path).file_name().is_some_and(|n| n == "SKILL.md") {
            scan(&directory, &mut files);
            files.retain(|file| std::path::Path::new(file).file_name().is_none_or(|n| n != "SKILL.md"));
            files.sort();
            files.truncate(10);
        }
        (directory, files)
    };
    let mut lines = vec![
        format!("<skill_content name=\"{}\">", skill.name.0),
        format!("# Skill: {}", skill.name.0),
        String::new(),
        skill.content.trim().to_owned(),
        String::new(),
        format!("Base directory for this skill: {directory}"),
        "Relative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.".to_owned(),
        "Note: file list is sampled.".to_owned(),
        String::new(),
        "<skill_files>".to_owned(),
    ];
    lines.extend(files.iter().map(|file| format!("<file>{file}</file>")));
    lines.push("</skill_files>".to_owned());
    lines.push("</skill_content>".to_owned());
    lines.join("\n")
}

/// The prompt's skill attachments (`SessionPrompt.prepare`): each skill's prepared content, once per skill (a repeated
/// skill keeps only its mention); `Err` names a skill that does not exist.
pub fn attachments(skills: &[t::SkillInfo], requested: &[Json]) -> std::result::Result<Vec<Json>, String> {
    let mut prepared: Vec<(String, String)> = Vec::new();
    let mut out = Vec::new();
    for attachment in requested {
        let id = json::get_str(attachment, "id").unwrap_or_default();
        let mention = json::get(attachment, "mention").filter(|m| !matches!(m, Json::Null)).cloned();
        let mut o = Obj::new();
        o.str("id", id);
        if let Some((_, name)) = prepared.iter().find(|(known, _)| known == id) {
            o.str("name", name);
        } else {
            let Some(skill) = skills.iter().find(|s| s.id.0 == id) else { return Err(id.to_owned()) };
            prepared.push((skill.id.0.clone(), skill.name.0.clone()));
            o.str("name", &skill.name.0).str("text", &prepare(skill));
        }
        o.opt("mention", mention);
        out.push(o.done());
    }
    Ok(out)
}

/// `Session.skill`: records the skill's activation, then resumes the session unless `resume` is `false`.
pub async fn activate(app: &App, session_id: &str, input: &Json) -> Result<()> {
    let stored = session::require(&app.db.lock().unwrap(), session_id)?;
    let id = json::get_str(input, "id").unwrap_or_default();
    let location = app.locations.get(app, &stored.location).await?;
    let inventory = location.inventory();
    let Some(skill) = inventory.skill(id) else {
        let mut o = Obj::new();
        o.str("_tag", "SkillNotFoundError").str("skill", id).str("message", &format!("Skill not found: {id}"));
        return Err(Failure::new(404, o.done()));
    };
    let mut data = Obj::new();
    data.str("sessionID", session_id).str("id", &skill.id.0).str("name", &skill.name.0).str("text", &skill.content);
    session::publish_event(app, "session.skill.activated", data.done(), Some(stored.location.clone()), None).map_err(fatal)?;
    if json::get_bool(input, "resume") != Some(false) {
        app.sessions.executions.resume(app, session_id);
    }
    Ok(())
}
