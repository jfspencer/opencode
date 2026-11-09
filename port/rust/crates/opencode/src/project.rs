//! Projects (packages/core/src/project.ts): the identity that owns a directory, persisted in the `project` table.
//!
//! A git checkout is identified by its `origin` remote, else the ID cached in `<common dir>/opencode`, else its first
//! root commit; a repository with none of these is the `global` project. A directory outside any repository is its
//! own project, identified by the SHA-1 of `directory:<path>`.
//!
//! A Mercurial checkout is identified by the ID cached in `.hg/opencode`, else its first root changeset (`hg log`).
//! Icons are discovered when a project is resolved (`crate::project_icon`).
//!
//! TODO(port): the archive sweep, the fingerprint cache of git discovery (it only saves spawning git), remote-derived
//! Mercurial identity (also missing upstream).

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
    /// The repository's store (git's common directory, `.hg`). TODO(port): the location watcher watches its HEAD.
    #[allow(dead_code)]
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

fn discover_git(dot_git: &str) -> Option<Resolved> {
    let repo = discover_repository(dot_git)?;
    let previous = cached(&repo.common_directory);
    let remote = git(&repo.worktree, &["remote", "get-url", "origin"])
        .filter(|r| r.code == 0)
        .and_then(|r| Some(r.text.trim().to_owned()).filter(|t| !t.is_empty()))
        .and_then(|origin| remote_identity(&origin))
        .map(|normalized| hash(&format!("git-remote:{normalized}")));
    let id = remote.or_else(|| previous.clone()).or_else(|| {
        let result = git(&repo.worktree, &["rev-list", "--max-parents=0", "HEAD"])?;
        if result.code != 0 {
            return None;
        }
        let mut roots: Vec<&str> = result.text.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect();
        roots.sort();
        roots.first().map(|r| r.to_string())
    });
    let canonical = if repo.git_directory == repo.common_directory {
        repo.worktree.clone()
    } else {
        git(&repo.worktree, &["worktree", "list", "--porcelain"])
            .filter(|r| r.code == 0)
            .and_then(|r| {
                r.text
                    .split('\n')
                    .find_map(|line| line.strip_prefix("worktree ").map(|d| resolve_against(&repo.worktree, d.trim())))
            })
            .unwrap_or_else(|| repo.worktree.clone())
    };
    Some(Resolved {
        previous,
        id: id.unwrap_or_else(|| GLOBAL.to_owned()),
        directory: repo.worktree,
        canonical,
        vcs: Some(Vcs { kind: "git", store: repo.common_directory }),
    })
}

/// The project owning `input`, without touching the database (`Project.resolve` without `persist`). It spawns git or
/// hg for repositories: off the runtime's thread, call it through `tokio::task::spawn_blocking`.
pub fn discover(input: &str) -> Resolved {
    let directory = resolve_path(input);
    if let Some(native) = up(&[".git", ".hg"], &directory, None) {
        if native.ends_with("/.git") {
            if let Some(resolved) = discover_git(&native) {
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
