//! Checkouts of Git references (packages/core/src/repository-cache.ts, the refresh of reference.ts): each reference's
//! repository is cloned into the cache directory its listing names (`<data>/opencode/repos/<host>/<path>[@<branch>]`),
//! tracking one ref for good (the requested branch, else the remote's default branch). A location refreshes its
//! references in the background when it boots, when their listing changes, and every hour; a checkout already there
//! is fetched and hard-reset at most once a day (the last attempt is kept in `kv` as `repository-cache:<path>`, before
//! any network work, so failures wait a day too). A checkout whose origin is another repository is replaced.
//!
//! Failures are not reported (TS logs them). TS also holds a lock directory under `<state>/opencode/locks` for each
//! checkout, so processes sharing the data directory take turns; here concurrent refreshes of one process take turns.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use opencode_protocol::types as t;
use rusqlite::{OptionalExtension, params};

use crate::app::App;
use crate::inventory::references;
use crate::location::Location;

const REFRESH_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;
const PERIOD: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// What `ensure` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Cached,
    Cloned,
    Refreshed,
}

/// Refreshes the location's Git references in the background, four at a time (`Reference.refresh`).
pub fn refresh(app: &Arc<App>, references: &[t::ReferenceInfo]) {
    let targets: Vec<(String, Option<String>, String)> = references
        .iter()
        .filter_map(|reference| match &reference.source {
            t::ReferenceSource::ReferenceGitSource(git) => Some((git.repository.clone(), git.branch.clone(), reference.path.0.clone())),
            t::ReferenceSource::ReferenceLocalSource(_) => None,
        })
        .collect();
    if targets.is_empty() {
        return;
    }
    let app = app.clone();
    tokio::spawn(async move {
        let mut running = tokio::task::JoinSet::new();
        for (repository, branch, path) in targets {
            if running.len() >= 4 {
                running.join_next().await;
            }
            let app = app.clone();
            running.spawn(async move {
                let _ = ensure(&app, &repository, branch.as_deref(), &path).await;
            });
        }
        while running.join_next().await.is_some() {}
    });
}

/// The hourly refresh of a booted location's references, for as long as it stays booted.
pub fn schedule(app: &Arc<App>, location: &Arc<Location>) {
    let (app, weak): (Arc<App>, Weak<Location>) = (app.clone(), Arc::downgrade(location));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(PERIOD).await;
            let Some(location) = weak.upgrade() else { return };
            let current = app.locations.cached(&location.reference());
            if !current.is_some_and(|current| Arc::ptr_eq(&current, &location)) {
                return;
            }
            refresh(&app, &location.inventory().references);
        }
    });
}

/// One checkout at a time per cache path.
fn lock(path: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = std::sync::OnceLock::new();
    LOCKS.get_or_init(Default::default).lock().unwrap().entry(path.to_owned()).or_default().clone()
}

struct Output {
    code: i32,
    stdout: String,
}

async fn git(cwd: &str, args: &[&str]) -> Option<Output> {
    let output = tokio::process::Command::new(crate::vcs::git_executable())
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    Some(Output { code: output.status.code().unwrap_or(-1), stdout: String::from_utf8_lossy(&output.stdout).into_owned() })
}

/// A git operation that must succeed.
async fn operation(cwd: &str, args: &[&str]) -> Result<(), String> {
    match git(cwd, args).await {
        Some(output) if output.code == 0 => Ok(()),
        Some(output) => Err(format!("git {} exited with {}", args.join(" "), output.code)),
        None => Err(format!("git {} could not start", args.join(" "))),
    }
}

/// `git <args>` in a checkout, its trimmed output when it succeeds.
async fn read(cwd: &str, args: &[&str]) -> Option<String> {
    git(cwd, args).await.filter(|o| o.code == 0).map(|o| o.stdout.trim().to_owned()).filter(|t| !t.is_empty())
}

async fn discover(path: &str) -> Option<crate::project::Repository> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || crate::project::repository(&path)).await.ok()?
}

/// `RepositoryCache.ensure` with the daily refresh.
async fn ensure(app: &App, repository: &str, branch: Option<&str>, local_path: &str) -> Result<Status, String> {
    if branch.is_some_and(|b| !references::valid_branch(b)) {
        return Err("Branch must contain only alphanumeric characters, /, _, ., and -, and cannot start with - or contain ..".into());
    }
    let reference = references::parse(repository).ok_or("Repository must be a git URL, host/path reference, or GitHub owner/repo shorthand")?;
    let key = format!("repository-cache:{local_path}");
    let target = references::identity(&reference.remote).unwrap_or_else(|| reference.identity());
    let lock = lock(local_path);
    let _turn = lock.lock().await;
    let parent = crate::inventory::config::dirname(local_path);
    std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
    let existing = discover(local_path).await;
    let origin = match &existing {
        Some(existing) => read(&existing.worktree, &["remote", "get-url", "origin"]).await,
        None => None,
    };
    // Discovery walks upward, so an enclosing repository with a matching origin could masquerade as the checkout:
    // reuse requires the checkout to live exactly at the cache path.
    let worktree = crate::project::resolve_path(local_path);
    let reuse = existing.as_ref().is_some_and(|e| e.worktree == worktree)
        && origin.as_deref().and_then(references::identity).is_some_and(|identity| identity == target);
    if !reuse && std::path::Path::new(local_path).exists() {
        std::fs::remove_dir_all(local_path).map_err(|e| e.to_string())?;
    }
    let now = crate::id::now();
    let previous = attempted(app, &key);
    let refresh = previous.is_none_or(|at| now - at >= REFRESH_INTERVAL_MS);
    let status = if !reuse {
        Status::Cloned
    } else if refresh {
        Status::Refreshed
    } else {
        Status::Cached
    };
    if status == Status::Cached {
        return Ok(status);
    }
    // The attempt is recorded before any network work, so failures wait out the interval too.
    record(app, &key, now);
    match status {
        Status::Cloned => {
            let mut args = vec!["clone", "--depth", "100"];
            if let Some(branch) = branch {
                args.extend(["--branch", branch]);
            }
            args.extend(["--", &reference.remote, local_path]);
            operation(&parent, &args).await?;
            discover(local_path).await.ok_or("Cloned repository could not be opened")?;
        }
        _ => {
            let existing = existing.ok_or("Repository is unavailable")?;
            let cwd = existing.worktree.as_str();
            operation(cwd, &["fetch", "--all", "--prune"]).await?;
            if let Some(branch) = branch {
                let spec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
                operation(cwd, &["fetch", "origin", &spec]).await?;
            }
            // Checking out the tracked ref before resetting heals a checkout left on another branch.
            let tracked = match branch {
                Some(branch) => Some(branch.to_owned()),
                None => read(cwd, &["symbolic-ref", "refs/remotes/origin/HEAD"])
                    .await
                    .map(|r| r.strip_prefix("refs/remotes/origin/").map(str::to_owned).unwrap_or(r))
                    .filter(|b| !b.is_empty()),
            };
            if let Some(tracked) = &tracked {
                operation(cwd, &["checkout", "-B", tracked, &format!("origin/{tracked}")]).await?;
            }
            let target = match tracked {
                Some(tracked) => Some(tracked),
                None => read(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await,
            };
            let revision = target.map(|t| format!("origin/{t}")).unwrap_or_else(|| "HEAD".into());
            operation(cwd, &["reset", "--hard", &revision]).await?;
        }
    }
    Ok(status)
}

/// The last refresh attempt of a checkout (`{attemptedAt}` in `kv`).
fn attempted(app: &App, key: &str) -> Option<i64> {
    let value: Option<String> =
        app.db.lock().unwrap().query_row("SELECT value FROM kv WHERE key = ?1", params![key], |row| row.get(0)).optional().ok()??;
    let at = crate::json::get_num(&crate::json::parse(&value?)?, "attemptedAt")?;
    Some(at as i64)
}

fn record(app: &App, key: &str, now: i64) {
    let mut value = crate::json::Obj::new();
    value.int("attemptedAt", now);
    let _ = app.db.lock().unwrap().execute(
        "INSERT INTO kv (key, value, time_created, time_updated) VALUES (?1, ?2, ?3, ?3) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, time_updated = excluded.time_updated",
        params![key, crate::json::text(&value.done()), now],
    );
}
