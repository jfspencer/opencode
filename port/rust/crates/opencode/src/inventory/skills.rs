//! Skills (packages/core/src/skill.ts and the plugins that fill it, in activation order): `opencode` and `report`
//! (plugin/skill.ts), `browser` (packages/plugin-browser), the compatibility roots' `skills` directories
//! (`~/.claude`, `~/.agents` and the project's `.claude` / `.agents`, config/plugin/compatibility.ts), then config
//! (config/plugin/skill.ts): the `skill` and `skills` directories of every config directory and the paths of config
//! `skills`. Policies (`skill.use`) remove denied skills. A skill added under an ID that exists replaces it in place.
//!
//! A skill root is scanned breadth-first to depth 6 (config/plugin/skill-scan.ts): a directory with a `SKILL.md` is one
//! skill (named after the directory) and is not descended; at the root, every `.md` file is a skill too (named after
//! the file); hidden entries and ignored directories are skipped.
//!
//! TODO(port): skill URLs (`skills: ["https://..."]`, skill/discovery.ts: an `index.json` downloaded into the cache);
//! the file watcher that rescans skill roots.

use opencode_protocol::types as t;
use opencode_protocol::{Json, Wire};

use super::config::{self, Entry, Sources};
use super::markdown;
use crate::json::Obj;

const OPENCODE_CONTENT: &str = include_str!("builtin/opencode.md");
const REPORT_CONTENT: &str = include_str!("builtin/report.md");
const BROWSER_CONTENT: &str = include_str!("builtin/browser.md");

const OPENCODE_DESCRIPTION: &str = "Use this skill for any question about OpenCode itself, including how OpenCode works, using or configuring it, migrating from V1 to V2, troubleshooting it, developing plugins or integrations, using the OpenCode SDK, clients, server, or API, and contributing to the OpenCode codebase. Also use it for OpenCode agents, commands, skills, tools, permissions, MCP servers, providers, models, themes, keybinds, formatters, the CLI, TUI, desktop app, and web app.";
const REPORT_DESCRIPTION: &str = "Use when the user wants to report an opencode issue or bug. Collect standard diagnostics, add user-specific reproduction context, and publish the issue with GitHub CLI.";
const BROWSER_DESCRIPTION: &str = "Use the in-app browser: open and read pages, click and fill forms, take screenshots, check console and network logs, and profile performance and memory. Load it before your first browser tool call.";

const MAX_DEPTH: usize = 6;
const IGNORED: [&str; 5] = ["node_modules", ".git", ".venv", "venv", "__pycache__"];

fn skill(id: &str, name: &str, description: Option<&str>, autoinvoke: Option<bool>, path: &str, content: &str) -> t::SkillInfo {
    let mut o = Obj::new();
    o.str("id", id).str("name", name).opt_str("description", description);
    o.opt("autoinvoke", autoinvoke.map(Json::Bool));
    o.str("path", path).str("content", content);
    t::SkillInfo::decode(&o.done()).expect("a valid skill")
}

fn add(skills: &mut Vec<t::SkillInfo>, skill: t::SkillInfo) {
    match skills.iter_mut().find(|s| s.id == skill.id) {
        Some(existing) => *existing = skill,
        None => skills.push(skill),
    }
}

/// The skills of a location (`location`: its directory, which relative config paths resolve against).
pub fn build(entries: &[Entry], sources: &Sources, location: &str) -> Vec<t::SkillInfo> {
    let mut skills = vec![];
    add(&mut skills, skill("opencode", "OpenCode", Some(OPENCODE_DESCRIPTION), None, "skill://opencode/SKILL.md", OPENCODE_CONTENT));
    add(&mut skills, skill("report", "Report", Some(REPORT_DESCRIPTION), None, "skill://report/SKILL.md", &report(entries)));
    add(&mut skills, skill("browser", "Browser", Some(BROWSER_DESCRIPTION), None, "skill://browser/SKILL.md", BROWSER_CONTENT));

    // Compatibility roots (the ones that exist), each `skills` directory by its real path.
    let roots = sources.claude.iter().chain(sources.agents.iter()).filter(|r| config::is_dir(r));
    let mut compat = vec![];
    for root in roots {
        let Ok(resolved) = std::fs::canonicalize(config::join(root, "skills")) else { continue };
        let resolved = resolved.to_string_lossy().into_owned();
        for file in scan(&resolved) {
            if let Some(parsed) = config::read(&file).filter(|c| !c.is_empty()).and_then(|c| parse(&resolved, &file, &c)) {
                add(&mut compat, parsed);
            }
        }
    }
    for s in compat {
        add(&mut skills, s);
    }

    // Config: config directories' `skill` / `skills`, then the paths of config `skills`.
    let options = config::Options::get();
    let mut directories: Vec<String> = vec![];
    let mut push = |path: String| {
        if !directories.contains(&path) {
            directories.push(path);
        }
    };
    for directory in config::directories(entries) {
        push(config::join(directory, "skill"));
        push(config::join(directory, "skills"));
    }
    for item in config::documents(entries).flat_map(|d| d.skills.iter().flatten()) {
        if is_url(item) {
            // TODO(port): URL skill sources.
            continue;
        }
        let expanded = match item.strip_prefix("~/") {
            Some(rest) => config::join(&options.home, rest),
            None => item.clone(),
        };
        let path = if expanded.starts_with('/') { crate::project::normalize(&expanded) } else { config::join(location, &expanded) };
        push(path);
    }
    let mut configured = vec![];
    for directory in &directories {
        for file in scan(directory) {
            if let Some(parsed) = config::read(&file).filter(|c| !c.is_empty()).and_then(|c| parse(directory, &file, &c)) {
                add(&mut configured, parsed);
            }
        }
    }
    for s in configured {
        add(&mut skills, s);
    }

    // Policies.
    let statements = super::providers::statements(entries);
    skills.retain(|s| super::providers::decision(&statements, "skill.use", &s.id.0) != "deny");
    skills
}

/// `URL.canParse(item) && /^(https?:)$/.test(new URL(item).protocol)`.
fn is_url(item: &str) -> bool {
    let lower = item.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")) && item.len() > lower.find("//").unwrap_or(0) + 2
}

/// The built-in report skill, with the runtime diagnostics captured when it registers.
fn report(entries: &[Entry]) -> String {
    let mut plugins: Vec<String> = config::documents(entries)
        .flat_map(|d| d.plugins.iter().flatten())
        .map(|p| match crate::json::of(p) {
            Json::String(s) => s,
            other => crate::json::get_str(&other, "package").unwrap_or_default().to_owned(),
        })
        .collect();
    plugins.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    let env = |name: &str| std::env::var(name).ok();
    let terminal: Vec<String> = [("TERM_PROGRAM", env("TERM_PROGRAM")), ("TERM", env("TERM")), ("COLORTERM", env("COLORTERM"))]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| format!("{k}={v}")))
        .collect();
    let terminal = if terminal.is_empty() { "Unavailable: terminal environment variables are not set".to_owned() } else { terminal.join(", ") };
    let shell = env("SHELL")
        .or_else(|| env("ComSpec"))
        .or_else(|| env("COMSPEC"))
        .unwrap_or_else(|| "Unavailable: shell environment variable is not set".to_owned());
    let read = |path: &str| std::fs::read_to_string(path).map(|s| s.trim().to_owned()).unwrap_or_default();
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    [
        REPORT_CONTENT.to_owned(),
        String::new(),
        "## Runtime Diagnostics Snapshot".into(),
        String::new(),
        "These values were captured when the built-in report skill was registered. Verify them before publishing.".into(),
        String::new(),
        format!("- opencode version: {}", crate::app::VERSION),
        format!("- install/channel: {}", crate::app::CHANNEL),
        format!("- OS: {} {} ({} {arch})", read("/proc/sys/kernel/ostype"), read("/proc/sys/kernel/osrelease"), std::env::consts::OS),
        format!("- Terminal: {terminal}"),
        format!("- Shell: {shell}"),
        format!("- Active plugins: {}", if plugins.is_empty() { "None found in config".to_owned() } else { plugins.join(", ") }),
    ]
    .join("\n")
}

/// `SkillScan.scan`: the skill files of a root, sorted.
pub fn scan(root: &str) -> Vec<String> {
    let mut files = vec![];
    let mut queue = vec![(root.to_owned(), 0usize)];
    let mut index = 0;
    while index < queue.len() {
        let (directory, depth) = queue[index].clone();
        index += 1;
        let Ok(list) = std::fs::read_dir(&directory) else { continue };
        let mut children: Vec<(String, String, Option<bool>)> = vec![];
        for entry in list.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || IGNORED.contains(&name.as_str()) {
                continue;
            }
            let path = config::join(&directory, &name);
            // Some(true): a directory, Some(false): a file, None: anything else (a dangling link included).
            let kind = std::fs::metadata(&path).ok().and_then(|m| {
                if m.is_dir() {
                    Some(true)
                } else if m.is_file() {
                    Some(false)
                } else {
                    None
                }
            });
            children.push((name, path, kind));
        }
        if let Some((_, path, _)) = children.iter().find(|(name, _, kind)| *kind == Some(false) && name == "SKILL.md") {
            files.push(path.clone());
            continue;
        }
        if depth == 0 {
            files.extend(children.iter().filter(|(n, _, k)| *k == Some(false) && n.ends_with(".md")).map(|(_, p, _)| p.clone()));
        }
        if depth < MAX_DEPTH {
            queue.extend(children.iter().filter(|(_, _, k)| *k == Some(true)).map(|(_, p, _)| (p.clone(), depth + 1)));
        }
    }
    files.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    files
}

/// `booleanValue` of skill frontmatter.
fn boolean(value: Option<&Json>) -> Option<bool> {
    match value? {
        Json::Bool(b) => Some(*b),
        Json::Number(n) if *n == 1.0 => Some(true),
        Json::Number(n) if *n == 0.0 => Some(false),
        Json::String(s) => match config::js_trim(s).to_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// `SkillFile.parse`.
pub fn parse(directory: &str, file: &str, content: &str) -> Option<t::SkillInfo> {
    let parsed = markdown::parse_option(content)?;
    let data = match &parsed.data {
        Json::Object(o) => o.clone(),
        // Only an object decodes as the frontmatter struct.
        _ => return None,
    };
    let string = |key: &str| match data.get(key) {
        None => Ok(None),
        Some(Json::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(()),
    };
    let name = string("name").ok()?;
    let description = string("description").ok()?;
    let parent = config::dirname(file);
    let basename = config::basename(file);
    let id = if parent == directory && basename != "SKILL.md" {
        basename.strip_suffix(".md").unwrap_or(basename).to_owned()
    } else {
        config::basename(&parent).to_owned()
    };
    let metadata = data.get("metadata").and_then(|m| m.as_object()).and_then(|m| boolean(m.get("opencode/autoinvoke")));
    let disabled = boolean(data.get("disable-model-invocation"));
    let autoinvoke = metadata.or(if disabled == Some(true) { Some(false) } else { None });
    Some(skill(&id, name.as_deref().unwrap_or(&id), description.as_deref(), autoinvoke, file, &parsed.content))
}
