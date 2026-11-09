//! Projects (packages/core/src/project.ts): the identity that owns a directory, persisted in the `project` table.
//!
//! A git checkout is identified by its `origin` remote, else the ID cached in `<common dir>/opencode`, else its first
//! root commit; a repository with none of these is the `global` project. A directory outside any repository is its
//! own project, identified by the SHA-1 of `directory:<path>`.
//!
//! A Mercurial checkout is identified by the ID cached in `.hg/opencode`, else its first root changeset (`hg log`).
//! Icons are discovered when a project is resolved (`crate::project_icon`).
//!
//! Git discoveries are cached by the fingerprint of what they read, so repeated resolutions spawn no git. The archive
//! sweep ([`sweep`]) archives or moves projects whose directories are gone. As upstream, Mercurial identity is never
//! derived from a remote (TS leaves that as a follow-up).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use opencode_protocol::Json;
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha1::{Digest, Sha1};

use crate::bus::{Bus, LocationRef, PublishOptions};
use crate::json::Obj;

pub const GLOBAL: &str = "global";

#[derive(Debug, Clone)]
pub struct Resolved {
    /// The ID cached in the repository before this resolution, if any.
    pub previous: Option<String>,
    pub id: String,
    pub directory: String,
    /// This checkout's main directory.
    pub canonical: String,
    pub vcs: Option<Vcs>,
}

#[derive(Debug, Clone)]
pub struct Vcs {
    pub kind: &'static str,
    /// The repository's store (git's common directory, `.hg`): where branch metadata lives ([`crate::vcs::changed`]).
    pub store: String,
}

/// Project activity is recorded at most once per minute per project.
pub const ACTIVATE_INTERVAL: i64 = 60_000;

#[derive(Default)]
pub struct Projects {
    activated: Mutex<HashMap<String, i64>>,
    /// Directory resolutions being announced, so concurrent resolves announce each once.
    announcing: Mutex<HashSet<String>>,
}

pub fn hash(input: &str) -> String {
    let digest = Sha1::digest(input.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// `FSUtil.resolve`: an absolute path with symlinks resolved where it exists.
pub fn resolve_path(input: &str) -> String {
    let absolute = absolutize(input);
    std::fs::canonicalize(&absolute).map(|p| p.to_string_lossy().into_owned()).unwrap_or(absolute)
}

/// `path.resolve` against the process working directory, normalized.
pub fn absolutize(input: &str) -> String {
    let path = Path::new(input);
    let joined: PathBuf =
        if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    normalize(&joined.to_string_lossy())
}

/// `path.resolve(cwd, value)` for git output (trailing newlines removed).
fn resolve_against(cwd: &str, value: &str) -> String {
    let trimmed = value.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return cwd.to_owned();
    }
    if trimmed.starts_with('/') { normalize(trimmed) } else { normalize(&format!("{cwd}/{trimmed}")) }
}

/// `path.posix.normalize` without a trailing separator (except for the root).
pub fn normalize(input: &str) -> String {
    let absolute = input.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in input.split('/') {
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
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".into()
    } else {
        joined
    }
}

/// Whether `child` is `parent` or inside it (`FSUtil.contains`).
pub fn contains(parent: &str, child: &str) -> bool {
    child == parent || child.starts_with(&format!("{}/", parent.trim_end_matches('/')))
}

/// `FSUtil.up` in `first` mode: the first of `targets` found walking up from `start` (stopping after `stop`).
pub fn up(targets: &[&str], start: &str, stop: Option<&str>) -> Option<String> {
    let mut current = PathBuf::from(start);
    loop {
        for target in targets {
            let candidate = current.join(target);
            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
        if stop.is_some_and(|stop| Path::new(stop) == current) || !current.pop() {
            return None;
        }
    }
}

struct Output {
    code: i32,
    text: String,
}

fn git(cwd: &str, args: &[&str]) -> Option<Output> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    Some(Output { code: output.status.code().unwrap_or(1), text: String::from_utf8_lossy(&output.stdout).into_owned() })
}

/// `Git.Repository`: a checkout, its git directory and the directory its linked worktrees share.
#[derive(Debug, Clone)]
pub struct Repository {
    pub worktree: String,
    #[allow(dead_code)]
    pub git_directory: String,
    pub common_directory: String,
}

/// `Git.repo.discover`: the repository whose `.git` is found walking up from `input`. It spawns git: call it off
/// the runtime's thread.
pub fn repository(input: &str) -> Option<Repository> {
    let dot_git = up(&[".git"], input, None)?;
    discover_repository(&dot_git)
}

fn discover_repository(dot_git: &str) -> Option<Repository> {
    let cwd = Path::new(dot_git).parent()?.to_string_lossy().into_owned();
    let result = git(&cwd, &["rev-parse", "--git-dir", "--git-common-dir", "--show-toplevel"])?;
    let mut lines = result.text.split('\n').map(|l| l.trim_end_matches('\r'));
    let git_dir = lines.next().filter(|l| !l.is_empty())?;
    let common_dir = lines.next().filter(|l| !l.is_empty())?;
    let top = lines.next().unwrap_or("");
    Some(Repository {
        worktree: if top.is_empty() { cwd.clone() } else { resolve_against(&cwd, top) },
        git_directory: resolve_against(&cwd, git_dir),
        common_directory: resolve_against(&cwd, common_dir),
    })
}

/// The remote's `host/path`, normalized (project.ts `url`): a URL with a scheme (not `file:`), or scp-like
/// `[user@]host:path`.
fn remote_identity(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() {
        return None;
    }
    let parts = |host: &str, name: &str| -> Option<String> {
        let pathname = name.trim_start_matches('/');
        let pathname = pathname.strip_suffix(".git/").or_else(|| pathname.strip_suffix(".git")).unwrap_or(pathname);
        let pathname = pathname.trim_end_matches('/');
        if host.is_empty() || pathname.is_empty() {
            return None;
        }
        Some(format!("{}/{pathname}", host.to_lowercase()))
    };
    if let Some((scheme, rest)) = value.split_once(':') {
        let is_scheme = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        // `URL.parse` accepts any `scheme:` prefix.
        if is_scheme {
            if scheme.eq_ignore_ascii_case("file") {
                return None;
            }
            let rest = rest.strip_prefix("//").unwrap_or(rest);
            let (authority, path) = match rest.find(['/', '?', '#']) {
                Some(index) => (&rest[..index], &rest[index..]),
                None => (rest, ""),
            };
            let host = authority.rsplit('@').next().unwrap_or("");
            let host = host.split(':').next().unwrap_or("");
            let path = path.split(['?', '#']).next().unwrap_or("");
            return parts(host, path);
        }
    }
    let (head, path) = value.split_once(':')?;
    let host = head.split_once('@').map(|(_, h)| h).unwrap_or(head);
    if host.is_empty() || host.contains('/') || host.contains('@') || path.is_empty() {
        return None;
    }
    parts(host, path)
}

/// `gitDiscover`: the checkout's project, and whether it may be cached (failures resolve the same way, uncached).
fn discover_git(dot_git: &str) -> (Option<Resolved>, bool) {
    let Some(repo) = discover_repository(dot_git) else { return (None, false) };
    let mut cacheable = true;
    let previous = cached(&repo.common_directory);
    // `Git.remote.get`: exit 2 is no such remote; another failure resolves the same, uncached.
    let remote = match git(&repo.worktree, &["remote", "get-url", "origin"]) {
        Some(result) if result.code == 0 => Some(result.text.trim().to_owned()).filter(|t| !t.is_empty()),
        Some(result) if result.code == 2 => None,
        _ => {
            cacheable = false;
            None
        }
    };
    let remote = remote
        .and_then(|origin| remote_identity(&origin))
        .map(|normalized| hash(&format!("git-remote:{normalized}")));
    let id = remote.or_else(|| previous.clone()).or_else(|| {
        // `Git.history.rootCommits`: none while HEAD is unborn.
        let Some(result) = git(&repo.worktree, &["rev-list", "--max-parents=0", "HEAD"]) else {
            cacheable = false;
            return None;
        };
        if result.code != 0 {
            if git(&repo.worktree, &["rev-parse", "-q", "--verify", "HEAD"]).is_none_or(|verify| verify.code != 1) {
                cacheable = false;
            }
            return None;
        }
        let mut roots: Vec<&str> = result.text.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect();
        roots.sort();
        roots.first().map(|r| r.to_string())
    });
    let canonical = if repo.git_directory == repo.common_directory {
        repo.worktree.clone()
    } else {
        let main = git(&repo.worktree, &["worktree", "list", "--porcelain"]).filter(|r| r.code == 0).map(|r| {
            r.text
                .split('\n')
                .find_map(|line| line.strip_prefix("worktree ").map(|d| resolve_against(&repo.worktree, d.trim())))
        });
        match main {
            Some(main) => main.unwrap_or_else(|| repo.worktree.clone()),
            None => {
                cacheable = false;
                repo.worktree.clone()
            }
        }
    };
    let resolved = Resolved {
        previous,
        id: id.unwrap_or_else(|| GLOBAL.to_owned()),
        directory: repo.worktree,
        canonical,
        vcs: Some(Vcs { kind: "git", store: repo.common_directory }),
    };
    (Some(resolved), cacheable)
}

/// `Git.repo.fingerprint`: a stamp of everything git discovery reads (the `.git` entry, a linked worktree's gitdir and
/// commondir, HEAD and the ref it names, loose, packed and reftable refs, repository, worktree, global, XDG and system
/// config, the cached project ID); `None` when `.git` is no checkout.
fn fingerprint(dot_git: &str) -> Option<String> {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    let dot = std::fs::metadata(dot_git).ok().filter(|m| m.is_file() || m.is_dir())?;
    let git_file = if dot.is_file() {
        let text = read(dot_git)?;
        text.lines().find_map(|line| line.strip_prefix("gitdir:")).map(|rest| rest.trim().to_owned()).filter(|r| !r.is_empty())?
    } else {
        String::new()
    };
    let resolve = |base: &str, relative: &str| {
        let joined = if relative.starts_with('/') { relative.to_owned() } else { format!("{base}/{relative}") };
        resolve_path(&normalize(&joined))
    };
    let parent = Path::new(dot_git).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let git_dir = if git_file.is_empty() { dot_git.to_owned() } else { resolve(&parent, &git_file) };
    let commondir = read(&format!("{git_dir}/commondir")).map(|t| t.trim().to_owned()).unwrap_or_default();
    let common_dir = if commondir.is_empty() { git_dir.clone() } else { resolve(&git_dir, &commondir) };
    let head = read(&format!("{git_dir}/HEAD"))?.trim().to_owned();
    let reference = head.strip_prefix("ref:").map(|r| r.trim().to_owned());
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let home = env("HOME").unwrap_or_default();
    let nosystem = env("GIT_CONFIG_NOSYSTEM").is_some_and(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"));
    let mut files = vec![
        format!("{git_dir}/HEAD"),
        format!("{common_dir}/config"),
        format!("{common_dir}/config.worktree"),
        format!("{git_dir}/config.worktree"),
        format!("{common_dir}/packed-refs"),
        format!("{common_dir}/reftable/tables.list"),
        format!("{common_dir}/opencode"),
    ];
    if let Some(reference) = &reference {
        files.push(normalize(&format!("{common_dir}/{reference}")));
        files.push(normalize(&format!("{git_dir}/{reference}")));
    }
    files.push(env("GIT_CONFIG_GLOBAL").unwrap_or_else(|| format!("{home}/.gitconfig")));
    files.push(format!("{}/git/config", env("XDG_CONFIG_HOME").unwrap_or_else(|| format!("{home}/.config"))));
    if !nosystem {
        files.push(env("GIT_CONFIG_SYSTEM").unwrap_or_else(|| "/etc/gitconfig".into()));
    }
    let stamp = |path: &str| {
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(path) {
            Ok(m) => format!("{}:{}:{}:{}", m.file_type().is_dir(), m.ino(), m.size(), m.mtime() * 1000 + m.mtime_nsec() / 1_000_000),
            Err(_) => "missing".into(),
        }
    };
    let stamps: Vec<String> = std::iter::once(dot_git.to_owned()).chain(files.iter().cloned()).map(|f| stamp(&f)).collect();
    let mut parts = vec![git_dir, common_dir, head];
    parts.extend(files);
    parts.extend(stamps);
    Some(parts.join("\0"))
}

/// Git discoveries by `.git` path and fingerprint, so repeated resolutions spawn no git (`gitCache`, 512 entries).
fn discover_git_cached(dot_git: &str) -> Option<Resolved> {
    static CACHE: std::sync::OnceLock<Mutex<indexmap::IndexMap<String, Resolved>>> = std::sync::OnceLock::new();
    let Some(fingerprint) = fingerprint(dot_git) else { return discover_git(dot_git).0 };
    let key = format!("{dot_git}\0{fingerprint}");
    let cache = CACHE.get_or_init(Default::default);
    if let Some(resolved) = cache.lock().unwrap().get(&key) {
        return Some(resolved.clone());
    }
    let (resolved, cacheable) = discover_git(dot_git);
    if let Some(resolved) = resolved.as_ref().filter(|_| cacheable) {
        let mut cache = cache.lock().unwrap();
        if cache.len() >= 512 {
            cache.shift_remove_index(0);
        }
        cache.insert(key, resolved.clone());
    }
    resolved
}

/// The project owning `input`, without touching the database (`Project.resolve` without `persist`). It spawns git or
/// hg for repositories: off the runtime's thread, call it through `tokio::task::spawn_blocking`.
pub fn discover(input: &str) -> Resolved {
    let directory = resolve_path(input);
    if let Some(native) = up(&[".git", ".hg"], &directory, None) {
        if native.ends_with("/.git") {
            if let Some(resolved) = discover_git_cached(&native) {
                return resolved;
            }
        } else {
            return discover_hg(&native);
        }
    }
    Resolved {
        previous: None,
        id: hash(&format!("directory:{directory}")),
        directory: directory.clone(),
        canonical: directory,
        vcs: None,
    }
}

/// `cached`: the project ID stored in a repository's `opencode` file.
fn cached(store: &str) -> Option<String> {
    std::fs::read_to_string(Path::new(store).join("opencode")).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// `hgDiscover`: the cached ID, else the first root changeset.
fn discover_hg(dot_hg: &str) -> Resolved {
    let worktree = Path::new(dot_hg).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
    let previous = cached(dot_hg);
    let root = || {
        let output = Command::new("hg")
            .args(["log", "-r", "roots(all())", "-T", "{node}\n"])
            .current_dir(&worktree)
            .env("HGPLAIN", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let mut nodes: Vec<String> =
            String::from_utf8_lossy(&output.stdout).split('\n').map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect();
        nodes.sort();
        nodes.into_iter().next()
    };
    let id = previous.clone().or_else(root).unwrap_or_else(|| GLOBAL.to_owned());
    Resolved { previous, id, directory: worktree.clone(), canonical: worktree, vcs: Some(Vcs { kind: "hg", store: dot_hg.to_owned() }) }
}

/// A project row's fields the server reads besides `Project.Info`.
pub struct ProjectRow {
    pub canonical: String,
    pub start: Option<String>,
}

/// The project row, if any.
pub fn row(db: &Connection, id: &str) -> rusqlite::Result<Option<ProjectRow>> {
    db.query_row("SELECT worktree, commands FROM project WHERE id = ?1", params![id], |row| {
        let commands: Option<String> = row.get(1)?;
        let start = commands
            .and_then(|text| crate::json::parse(&text))
            .and_then(|json| crate::json::get_str(&json, "start").map(str::to_owned));
        Ok(ProjectRow { canonical: row.get(0)?, start })
    })
    .optional()
}

/// `Project.list`: by recent activity, then ID; archived projects only when asked for.
pub fn list(db: &Connection, archived: bool) -> rusqlite::Result<Vec<Json>> {
    let sql = if archived {
        "SELECT * FROM project ORDER BY time_active DESC, id ASC"
    } else {
        "SELECT * FROM project WHERE time_archived IS NULL ORDER BY time_active DESC, id ASC"
    };
    let mut statement = db.prepare(sql)?;
    let rows = statement.query_map([], info_from_row)?.collect();
    rows
}

/// What `Project.update` changes (`Project.UpdateInput`): an empty text clears its field.
#[derive(Default)]
pub struct Update {
    pub canonical: Option<String>,
    pub name: Option<String>,
    pub icon_override: Option<String>,
    pub icon_color: Option<String>,
    pub start: Option<String>,
}

/// How long a project sits untouched before the archive sweep considers it, and how often the sweep runs.
const SWEEP_INTERVAL_MS: i64 = 60 * 60_000;

/// `Project.sweep`: a project untouched for an hour (not updated, not active) whose canonical directory is gone moves
/// to a surviving main checkout among its known worktrees, else (when none of them survives) is archived; each change
/// is announced. Unreadable paths count as present.
pub fn sweep(db: &Connection, bus: &Bus) -> rusqlite::Result<()> {
    let cutoff = crate::id::now() - SWEEP_INTERVAL_MS;
    const IDLE: &str = "time_archived IS NULL AND time_updated < ?1 AND time_active < ?1";
    let projects: Vec<(String, String)> = db
        .prepare(&format!("SELECT id, worktree FROM project WHERE {IDLE} AND id <> ?2"))?
        .query_map(params![cutoff, GLOBAL], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let worktrees: Vec<(String, String, Option<String>)> = db
        .prepare("SELECT project_id, directory, strategy FROM worktree")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let exists = |directory: &str| match std::fs::metadata(directory) {
        Ok(_) => true,
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    };
    for (id, canonical) in projects {
        if exists(&canonical) {
            continue;
        }
        let surviving: Vec<&(String, String, Option<String>)> =
            worktrees.iter().filter(|w| w.0 == id && w.1 != canonical && exists(&w.1)).collect();
        // Linked worktrees carry a strategy; only another main checkout can become the canonical directory.
        let main = surviving.iter().find(|w| w.2.is_none());
        if main.is_none() && !surviving.is_empty() {
            continue;
        }
        let now = crate::id::now();
        let info = match main {
            Some(main) => db
                .query_row(
                    &format!("UPDATE project SET worktree = ?2, time_updated = ?3 WHERE id = ?4 AND {IDLE} RETURNING *"),
                    params![cutoff, main.1, now, id],
                    info_from_row,
                )
                .optional()?,
            None => db
                .query_row(
                    &format!("UPDATE project SET time_archived = ?2 WHERE id = ?3 AND {IDLE} RETURNING *"),
                    params![cutoff, now, id],
                    info_from_row,
                )
                .optional()?,
        };
        if let Some(info) = info {
            bus.emit("project.updated", info, PublishOptions::default());
        }
    }
    Ok(())
}

/// The archive sweep runs when the server starts and every hour after (`Project.sweepNode`).
pub fn start_sweep(app: &std::sync::Arc<crate::app::App>) {
    let app = app.clone();
    tokio::spawn(async move {
        loop {
            {
                let db = app.db.lock().unwrap();
                if let Err(error) = sweep(&db, &app.bus) {
                    eprintln!("Project archive sweep failed: {error}");
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(SWEEP_INTERVAL_MS as u64)).await;
        }
    });
}

/// `Project.update`: the updated project, announced; `None` when there is no such project.
pub fn update(db: &Connection, bus: &Bus, id: &str, input: Update) -> rusqlite::Result<Option<Json>> {
    let blank = |value: String| if value.is_empty() { rusqlite::types::Value::Null } else { rusqlite::types::Value::Text(value) };
    let mut sets = vec![];
    let mut values: Vec<rusqlite::types::Value> = vec![];
    let mut set = |column: &str, value: rusqlite::types::Value| {
        values.push(value);
        sets.push(format!("{column} = ?{}", values.len()));
    };
    if let Some(canonical) = input.canonical {
        set("worktree", rusqlite::types::Value::Text(canonical));
    }
    if let Some(name) = input.name {
        set("name", blank(name));
    }
    if let Some(value) = input.icon_override {
        set("icon_url_override", blank(value));
    }
    if let Some(value) = input.icon_color {
        set("icon_color", blank(value));
    }
    if let Some(start) = input.start {
        let mut commands = Obj::new();
        commands.str("start", &start);
        let value = if start.is_empty() {
            rusqlite::types::Value::Null
        } else {
            rusqlite::types::Value::Text(crate::json::text(&commands.done()))
        };
        set("commands", value);
    }
    set("time_updated", rusqlite::types::Value::Integer(crate::id::now()));
    values.push(rusqlite::types::Value::Text(id.to_owned()));
    let sql = format!("UPDATE project SET {} WHERE id = ?{} RETURNING *", sets.join(", "), values.len());
    let Some(info) = db.query_row(&sql, rusqlite::params_from_iter(values), info_from_row).optional()? else {
        return Ok(None);
    };
    bus.emit("project.updated", info.clone(), PublishOptions::default());
    Ok(Some(info))
}

impl Projects {
    /// [`discover`].
    pub fn discover(&self, input: &str) -> Resolved {
        discover(input)
    }

    /// Persists a resolution (`Project.resolve` with `persist`).
    pub fn persist(&self, db: &mut Connection, bus: &Bus, project: &Resolved) -> rusqlite::Result<()> {
        self.persist_at(db, bus, project, None)
    }

    /// [`Projects::persist`] from inside a location (a request placed in it): its events carry the location.
    pub fn persist_at(
        &self,
        db: &mut Connection,
        bus: &Bus,
        project: &Resolved,
        location: Option<LocationRef>,
    ) -> rusqlite::Result<()> {
        let now = crate::id::now();
        let previous: Option<String> = db
            .query_row("SELECT worktree FROM project WHERE id = ?1", params![project.id], |row| row.get(0))
            .optional()?;
        // upsertProject (project/sql.ts): only the VCS type changes on conflict.
        match &project.vcs {
            Some(vcs) => db.execute(
                "INSERT INTO project (id, worktree, vcs, time_created, time_updated, time_active, sandboxes) \
                 VALUES (?1, ?2, ?3, ?4, ?4, ?4, '[]') \
                 ON CONFLICT (id) DO UPDATE SET vcs = ?3 WHERE project.vcs IS NULL OR project.vcs <> ?3",
                params![project.id, project.canonical, vcs.kind, now],
            )?,
            None => db.execute(
                "INSERT INTO project (id, worktree, vcs, time_created, time_updated, time_active, sandboxes) \
                 VALUES (?1, ?2, NULL, ?3, ?3, ?3, '[]') \
                 ON CONFLICT (id) DO UPDATE SET vcs = NULL WHERE project.vcs IS NOT NULL",
                params![project.id, project.canonical, now],
            )?,
        };
        let unarchived = db
            .query_row(
                "UPDATE project SET time_archived = NULL WHERE id = ?1 AND time_archived IS NOT NULL RETURNING *",
                params![project.id],
                info_from_row,
            )
            .optional()?;
        // Clones share a project ID; only replace a canonical directory that is gone.
        let replaced = match &previous {
            Some(canonical) if *canonical != project.canonical && !Path::new(canonical).exists() => db
                .query_row(
                    "UPDATE project SET worktree = ?1, time_updated = ?2 WHERE id = ?3 RETURNING *",
                    params![project.canonical, now, project.id],
                    info_from_row,
                )
                .optional()?,
            _ => None,
        };
        if let Some(changed) = replaced.or(unarchived) {
            bus.emit("project.updated", changed, PublishOptions { id: None, location: location.clone() });
        }
        let Some(vcs) = &project.vcs else { return Ok(()) };
        let mut directories = vec![(project.canonical.clone(), None::<&str>)];
        if project.directory != project.canonical {
            directories.push((project.directory.clone(), (vcs.kind == "git").then_some("git")));
        }
        for (directory, strategy) in directories {
            let key = format!("{}\u{0}{directory}", project.id);
            if !self.announcing.lock().unwrap().insert(key.clone()) {
                continue;
            }
            let result = announce(db, bus, project, &directory, strategy, location.clone());
            self.announcing.lock().unwrap().remove(&key);
            result?;
        }
        Ok(())
    }

    /// Records project activity for recency ordering (throttled), announcing the project.
    pub fn activate(
        &self,
        db: &Connection,
        bus: &Bus,
        project_id: &str,
        location: Option<LocationRef>,
    ) -> rusqlite::Result<()> {
        let now = crate::id::now();
        {
            let mut activated = self.activated.lock().unwrap();
            if activated.get(project_id).is_some_and(|last| now - last < ACTIVATE_INTERVAL) {
                return Ok(());
            }
            activated.insert(project_id.to_owned(), now);
        }
        let row = db
            .query_row("UPDATE project SET time_active = ?1 WHERE id = ?2 RETURNING *", params![now, project_id], info_from_row)
            .optional()?;
        if let Some(info) = row {
            bus.emit("project.updated", info, PublishOptions { id: None, location });
        }
        Ok(())
    }
}

/// A directory's first resolution to a repository project is a durable fact (`worktree.resolved`): sessions of its
/// previous identity follow it, and markerless projects inside it are adopted.
fn announce(
    db: &mut Connection,
    bus: &Bus,
    project: &Resolved,
    directory: &str,
    strategy: Option<&str>,
    location: Option<LocationRef>,
) -> rusqlite::Result<()> {
    let stored: Option<String> = db
        .query_row(
            "SELECT directory FROM worktree WHERE project_id = ?1 AND directory = ?2",
            params![project.id, directory],
            |row| row.get(0),
        )
        .optional()?;
    if stored.is_some() {
        return Ok(());
    }
    let resolved_directory = resolve_path(directory);
    let markerless: Vec<(String, String)> = {
        let mut statement =
            db.prepare("SELECT id, worktree FROM project WHERE vcs IS NULL AND worktree >= ?1 AND worktree <= ?2")?;
        statement
            .query_map(params![resolved_directory, format!("{resolved_directory}\u{ffff}")], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let adopted: Vec<String> = markerless
        .into_iter()
        .filter(|(id, candidate)| {
            if *id == project.id || !contains(&resolved_directory, candidate) {
                return false;
            }
            let Some(found) = up(&[".git", ".hg"], candidate, Some(&resolved_directory)) else { return false };
            let parent = Path::new(&found).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            resolve_path(&parent) == resolved_directory
        })
        .map(|(id, _)| id)
        .collect();
    let mut data = Obj::new();
    data.str("projectID", &project.id);
    data.str("directory", directory);
    data.str("previous", project.previous.as_deref().unwrap_or(GLOBAL));
    if !adopted.is_empty() {
        data.set("adopted", Json::Array(adopted.into_iter().map(Json::String).collect()));
    }
    let (project_id, directory, strategy) = (project.id.clone(), directory.to_owned(), strategy.map(str::to_owned));
    bus.publish(db, "worktree.resolved", data.done(), PublishOptions { id: None, location }, move |tx, event| {
        crate::session::project(tx, event)?;
        tx.execute(
            "INSERT INTO worktree (project_id, directory, strategy, time_created) VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
            params![project_id, directory, strategy, crate::id::now()],
        )?;
        Ok(())
    })?;
    Ok(())
}

/// `Project.Info` of a project row (project.ts `fromRow`), in its key order.
pub fn info_from_row(row: &Row) -> rusqlite::Result<Json> {
    let text = |name: &str| row.get::<_, Option<String>>(name);
    let mut o = Obj::new();
    o.str("id", &row.get::<_, String>("id")?);
    o.str("canonical", &row.get::<_, String>("worktree")?);
    o.opt_str("vcs", text("vcs")?.as_deref());
    o.opt_str("name", text("name")?.as_deref());
    let (url, over, color) = (text("icon_url")?, text("icon_url_override")?, text("icon_color")?);
    if url.is_some() || over.is_some() || color.is_some() {
        let mut icon = Obj::new();
        icon.opt_str("url", url.as_deref());
        icon.opt_str("override", over.as_deref());
        icon.opt_str("color", color.as_deref());
        o.set("icon", icon.done());
    }
    if let Some(commands) = text("commands")?.and_then(|t| crate::json::parse(&t)) {
        o.set("commands", commands);
    }
    let mut time = Obj::new();
    time.int("created", row.get("time_created")?);
    time.int("updated", row.get("time_updated")?);
    time.int("active", row.get("time_active")?);
    if let Some(archived) = row.get::<_, Option<i64>>("time_archived")? {
        time.int("archived", archived);
    }
    o.set("time", time.done());
    let sandboxes = crate::json::parse(&row.get::<_, String>("sandboxes")?).unwrap_or(Json::Array(vec![]));
    o.set("sandboxes", sandboxes);
    Ok(o.done())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_like_node() {
        assert_eq!(normalize("/a/b/../c/./d/"), "/a/c/d");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("/.."), "/");
        assert!(contains("/a/b", "/a/b/c") && contains("/a/b", "/a/b") && !contains("/a/b", "/a/bc"));
    }

    #[test]
    fn remote_identities() {
        assert_eq!(remote_identity("https://GitHub.com/owner/repo.git").as_deref(), Some("github.com/owner/repo"));
        assert_eq!(remote_identity("git@github.com:owner/repo.git").as_deref(), Some("github.com/owner/repo"));
        assert_eq!(remote_identity("ssh://git@host:22/owner/repo/").as_deref(), Some("host/owner/repo"));
        assert_eq!(remote_identity("file:///tmp/repo"), None);
        assert_eq!(remote_identity(""), None);
    }
}
