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
//! A configured URL (skill/discovery.ts) names an index (`<url>/index.json`: `{skills: [{name, version?, files}]}`)
//! whose skills are downloaded into the cache (`$XDG_CACHE_HOME/opencode/skills/<sha1 of the URL>/<name>`) and then
//! scanned like directories; [`pull`] runs when the config loads, before the inventory is built. A skill with a
//! version is replaced as a whole when its version changes (and kept when the new one fails to download); one
//! without is completed file by file. Unsafe names and files skip the skill.
//!
//! Every source is watched while it loads (its tree, or the first missing directory on the way to it); a change
//! rescans the sources (pulling the URLs again) after 100 ms of quiet and announces `skill.updated` when the skills
//! changed ([`crate::inventory::Shared`]).
//!
//! TODO(port): watches of symlinked skill files outside their roots; downloads run one at a time (TS: 4 skills, 8
//! files at once).

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

/// A configured skill source (`Skill.DirectorySource` / `Skill.UrlSource`).
#[derive(Debug, Clone, PartialEq)]
enum Source {
    Directory(String),
    Url(String),
}

/// `ConfigSkillPlugin.sources`: config directories' `skill` / `skills`, then the items of config `skills`, unique.
fn configured_sources(entries: &[Entry], location: &str) -> Vec<Source> {
    let options = config::Options::get();
    let mut out: Vec<Source> = vec![];
    let mut push = |source: Source| {
        if !out.contains(&source) {
            out.push(source);
        }
    };
    for directory in config::directories(entries) {
        push(Source::Directory(config::join(directory, "skill")));
        push(Source::Directory(config::join(directory, "skills")));
    }
    for item in config::documents(entries).flat_map(|d| d.skills.iter().flatten()) {
        if crate::url::is_http(item) {
            push(Source::Url(item.clone()));
            continue;
        }
        let expanded = match item.strip_prefix("~/") {
            Some(rest) => config::join(&options.home, rest),
            None => item.clone(),
        };
        let path = if expanded.starts_with('/') { crate::project::normalize(&expanded) } else { config::join(location, &expanded) };
        push(Source::Directory(path));
    }
    out
}

/// The skill directories each configured URL pulled into the cache, by URL.
pub type Pulled = Vec<(String, Vec<String>)>;

/// `SkillScan.ignore` for directory watches: ignored directories and hidden entries anywhere below the root.
fn watch_ignored(relative: &str) -> bool {
    relative.split('/').any(|c| c.starts_with('.') || IGNORED.contains(&c))
}

/// The first missing directory on the way to `target` (`firstMissing`): the entry whose creation brings it closer.
fn first_missing(target: &str) -> Option<String> {
    let parent = config::dirname(target);
    if parent == target {
        return None;
    }
    if config::is_dir(&parent) { Some(target.to_owned()) } else { first_missing(&parent) }
}

/// `watchDirectory`: the directory's real path (and the path itself when it is a link), or, when it does not exist,
/// the first missing directory on the way to it.
fn watch_directory(directory: &str, send: &crate::watch::Sender, watches: &mut Vec<crate::watch::Watch>) {
    let target = crate::project::normalize(directory);
    match std::fs::canonicalize(&target).map(|p| p.to_string_lossy().into_owned()) {
        Ok(resolved) => {
            watches.extend(crate::watch::directory(&resolved, watch_ignored, send));
            if resolved != target {
                watches.extend(crate::watch::file(&target, send));
            }
        }
        Err(_) => {
            if let Some(missing) = first_missing(&target) {
                watches.extend(crate::watch::file(&missing, send));
            }
        }
    }
}

/// Pulls every configured skill URL (`SkillDiscovery.pull`) in source order, watching each source as it is loaded
/// when `send` is given (`ConfigSkillPlugin.refresh`).
pub async fn pull_all(entries: &[Entry], location: &str, send: Option<&crate::watch::Sender>) -> (Pulled, Vec<crate::watch::Watch>) {
    let mut out = Pulled::new();
    let mut watches = Vec::new();
    for source in configured_sources(entries, location) {
        match source {
            Source::Directory(path) => {
                if let Some(send) = send {
                    watch_directory(&path, send, &mut watches);
                }
            }
            Source::Url(url) => {
                let directories = pull(&url).await;
                if let Some(send) = send {
                    for directory in &directories {
                        watch_directory(directory, send, &mut watches);
                    }
                }
                out.push((url, directories));
            }
        }
    }
    (out, watches)
}

/// The skills of a location (`location`: its directory, which relative config paths resolve against; `pulled`: the
/// URL sources' directories).
pub fn build(entries: &[Entry], sources: &Sources, location: &str, pulled: &Pulled) -> Vec<t::SkillInfo> {
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

    // Config: config directories' `skill` / `skills`, then the paths and URLs of config `skills`.
    let directories: Vec<String> = configured_sources(entries, location)
        .into_iter()
        .flat_map(|source| match source {
            Source::Directory(path) => vec![path],
            Source::Url(url) => pulled.iter().find(|(u, _)| *u == url).map(|(_, d)| d.clone()).unwrap_or_default(),
        })
        .collect();
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

// --- URL sources (skill/discovery.ts) -----------------------------------------------------------------------------

/// `isSafeSegment`.
fn safe_segment(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\', '\0'])
}

/// `decodeURIComponent`: `None` where JS throws (a malformed escape or invalid UTF-8).
fn decode_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && !(i + 2 < bytes.len() && bytes[i + 1].is_ascii_hexdigit() && bytes[i + 2].is_ascii_hexdigit()) {
            return None;
        }
        i += if bytes[i] == b'%' { 3 } else { 1 };
    }
    String::from_utf8(crate::url::percent_decode(value)).ok()
}

/// `path.win32.isAbsolute`.
fn win32_absolute(value: &str) -> bool {
    let b = value.as_bytes();
    matches!(b.first(), Some(b'/') | Some(b'\\'))
        || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'/' | b'\\'))
}

/// `isSafeRelativePath`.
fn safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['\\', '\0', '?', '#'])
        && crate::url::Url::parse(value).is_none()
        && !value.starts_with('/')
        && !win32_absolute(value)
        && value.split('/').all(|segment| decode_component(segment).is_some_and(|s| safe_segment(&s)))
}

/// `encodeURIComponent`.
fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for &b in value.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `FSUtil.contains(parent, child)`.
fn contains(parent: &str, child: &str) -> bool {
    child == parent || child.starts_with(&format!("{}/", parent.trim_end_matches('/')))
}

/// A GET through the discovery client (`retryTransient` twice, exponential from 200 ms, then `filterStatusOk`): the
/// body of a 2xx response.
async fn get(url: &str, accept_json: bool) -> Option<Vec<u8>> {
    let headers = if accept_json { vec![("Accept".to_owned(), "application/json".to_owned())] } else { vec![] };
    let mut delay = std::time::Duration::from_millis(200);
    for attempt in 0..3 {
        let transient = match crate::llm::http::send("GET", url, &headers, String::new()).await {
            Ok(mut response) => {
                if (200..300).contains(&response.status) {
                    let mut body = Vec::new();
                    let mut failed = false;
                    while let Some(chunk) = response.body.next().await {
                        match chunk {
                            Ok(chunk) => body.extend_from_slice(&chunk),
                            Err(_) => {
                                failed = true;
                                break;
                            }
                        }
                    }
                    if !failed {
                        return Some(body);
                    }
                    true
                } else {
                    matches!(response.status, 408 | 429 | 500 | 502 | 503 | 504)
                }
            }
            Err(_) => true,
        };
        if !transient || attempt == 2 {
            return None;
        }
        tokio::time::sleep(delay).await;
        delay *= 2;
    }
    None
}

/// `download`: a file already in place is kept.
async fn download(url: &str, destination: &str) -> bool {
    if std::path::Path::new(destination).exists() {
        return true;
    }
    let Some(body) = get(url, false).await else { return false };
    let _ = std::fs::create_dir_all(config::dirname(destination));
    std::fs::write(destination, body).is_ok()
}

struct Pending {
    name: String,
    version: Option<String>,
    root: String,
    /// (url, destination relative path)
    files: Vec<(String, String)>,
}

/// The index's skills that can be installed (`data.skills.flatMap(...)`).
fn plan(index: &Json, source: &crate::url::Url, source_root: &str) -> Option<Vec<Pending>> {
    let Some(Json::Array(skills)) = crate::json::get(index, "skills") else { return None };
    let mut decoded = Vec::new();
    for skill in skills {
        let name = crate::json::get_str(skill, "name")?.to_owned();
        let version = match crate::json::get(skill, "version") {
            None => None,
            Some(Json::String(v)) => Some(v.clone()),
            Some(_) => return None,
        };
        let Some(Json::Array(files)) = crate::json::get(skill, "files") else { return None };
        let files: Vec<String> = files.iter().map(|f| crate::json::as_str(f).map(str::to_owned)).collect::<Option<_>>()?;
        decoded.push((name, version, files));
    }
    let mut out = Vec::new();
    for (name, version, files) in decoded {
        if !safe_segment(&name) || !(files.iter().any(|f| f == "SKILL.md") || files.iter().any(|f| *f == format!("{name}.md"))) {
            continue;
        }
        let root = config::join(source_root, &name);
        if !contains(source_root, &root) || root == source_root {
            continue;
        }
        let Some(skill_url) = source.join(&format!("{}/", encode_component(&name))) else { continue };
        let resolved: Option<Vec<(String, String)>> = files
            .iter()
            .map(|file| {
                if !safe_relative_path(file) {
                    return None;
                }
                let resource = skill_url.join(file)?;
                if resource.origin() != source.origin() {
                    return None;
                }
                let destination = config::join(&root, file);
                (contains(&root, &destination) && destination != root).then(|| (resource.href(), file.clone()))
            })
            .collect();
        let Some(files) = resolved else { continue };
        out.push(Pending { name, version, root, files });
    }
    Some(out)
}

/// Installs a versioned skill afresh: downloaded into a staging directory, then swapped in.
async fn replace(skill: &Pending, version: &str) {
    let token: String = {
        let mut bytes = [0u8; 16];
        let _ = getrandom::fill(&mut bytes);
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    };
    let staging = format!("{}.tmp-{token}", skill.root);
    let backup = format!("{}.old-{token}", skill.root);
    let mut downloaded = true;
    for (url, file) in &skill.files {
        downloaded &= download(url, &config::join(&staging, file)).await;
    }
    let exists = |dir: &str| {
        std::path::Path::new(&config::join(dir, "SKILL.md")).exists()
            || std::path::Path::new(&config::join(dir, &format!("{}.md", skill.name))).exists()
    };
    if downloaded && exists(&staging) && std::fs::write(config::join(&staging, ".opencode-version"), version).is_ok() {
        let cached = std::path::Path::new(&skill.root).exists();
        if !cached || std::fs::rename(&skill.root, &backup).is_ok() {
            if std::fs::rename(&staging, &skill.root).is_err() {
                if cached {
                    let _ = std::fs::rename(&backup, &skill.root);
                }
            } else if cached {
                let _ = std::fs::remove_dir_all(&backup);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&staging);
}

/// `SkillDiscovery.pull`: downloads the index's skills into the cache; the directories of the installed ones.
pub async fn pull(url: &str) -> Vec<String> {
    let base = if url.ends_with('/') { url.to_owned() } else { format!("{url}/") };
    let Some(source) = crate::url::Url::parse(&base) else { return vec![] };
    let Some(index) = source.join("index.json") else { return vec![] };
    let Some(body) = get(&index.href(), true).await else { return vec![] };
    let Some(data) = crate::json::parse(&String::from_utf8_lossy(&body)) else { return vec![] };
    let digest: String = {
        use sha1::Digest;
        sha1::Sha1::digest(base.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
    };
    let source_root = config::join(&config::Options::get().cache, &format!("skills/{digest}"));
    let Some(skills) = plan(&data, &source, &source_root) else { return vec![] };
    let mut out = Vec::new();
    for skill in skills {
        let current = skill.version.as_ref().and_then(|_| config::read(&config::join(&skill.root, ".opencode-version")));
        match &skill.version {
            Some(version) if current.as_deref() != Some(version.as_str()) => replace(&skill, version).await,
            _ => {
                for (url, file) in &skill.files {
                    download(url, &config::join(&skill.root, file)).await;
                }
            }
        }
        let installed = std::path::Path::new(&config::join(&skill.root, "SKILL.md")).exists()
            || std::path::Path::new(&config::join(&skill.root, &format!("{}.md", skill.name))).exists();
        if installed {
            out.push(skill.root);
        }
    }
    out
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
