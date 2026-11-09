//! Worktrees (packages/core/src/worktree.ts, worktree/{directory,git,strategies,sql}.ts, the worktree half of
//! git.ts): the directories a project is checked out in, kept in the `worktree` table. The project's canonical
//! checkout and checkouts found by `refresh` have no strategy; worktrees a strategy created (git's linked worktrees)
//! are owned by it. Creating one runs the project's start command in it.
//!
//! TODO(port): worktree strategies registered by external plugins (only git's built-in strategy exists).

use std::path::Path;

use opencode_protocol::Json;
use rusqlite::{Connection, OptionalExtension, params};

use crate::app::{App, fatal};
use crate::bus::PublishOptions;
use crate::http::Failure;
use crate::json::Obj;
use crate::process::Command;
use crate::project::Repository;

const GIT: &str = "git";

/// `Worktree.Error` as the handler reports it.
#[derive(Debug)]
pub enum Error {
    ProjectNotFound(String),
    SourceNotFound { project_id: String, directory: Option<String> },
    DestinationExists(String),
    DirectoryUnavailable(String),
    InvalidDirectory(String),
    StrategyUnavailable(String),
    /// `Git.WorktreeError` (and `Worktree.OperationError`): git's own message.
    Operation { message: String, force_required: Option<bool> },
    /// `AppProcessError` of the start command.
    Process(String),
    Fatal(Failure),
}

impl Error {
    /// `ProjectNotFoundError` (404) or `WorktreeError` (400) (handlers/worktree.ts).
    pub fn failure(self) -> Failure {
        let (message, force_required) = match self {
            Error::ProjectNotFound(id) => {
                let mut o = Obj::new();
                o.str("_tag", "ProjectNotFoundError").str("projectID", &id).str("message", &format!("Project not found: {id}"));
                return Failure::new(404, o.done());
            }
            Error::Fatal(failure) => return failure,
            Error::SourceNotFound { project_id, directory } => (
                match directory {
                    Some(directory) => format!(
                        "Cannot create a worktree from {directory}: it is not a checkout of this project that supports worktrees, such as a git repository"
                    ),
                    None => format!("Worktree source not found for project: {project_id}"),
                },
                None,
            ),
            Error::DestinationExists(directory) => (format!("Worktree destination already exists: {directory}"), None),
            Error::DirectoryUnavailable(directory) => (format!("Worktree directory unavailable: {directory}"), None),
            Error::InvalidDirectory(directory) => (format!("Invalid worktree directory: {directory}"), None),
            Error::StrategyUnavailable(strategy) => (format!("Worktree strategy unavailable: {strategy}"), None),
            Error::Operation { message, force_required } => (message, force_required),
            Error::Process(message) => (message, None),
        };
        let mut data = Obj::new();
        data.str("message", &message);
        // The JSON codec writes the undefined `forceRequired` the handler passes as `null`.
        data.set("forceRequired", force_required.map(Json::Bool).unwrap_or(Json::Null));
        let mut o = Obj::new();
        o.str("_tag", "WorktreeError").str("name", "WorktreeError").set("data", data.done());
        Failure::new(400, o.done())
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Error::Fatal(fatal(error))
    }
}

/// A row of the inventory (`Worktree.ListEntry` of the list endpoint).
pub struct Stored {
    pub directory: String,
    pub strategy: Option<String>,
}

impl Stored {
    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        o.str("directory", &self.directory).opt_str("strategy", self.strategy.as_deref());
        o.done()
    }
}

fn project_row(app: &App, id: &str) -> Result<crate::project::ProjectRow, Error> {
    crate::project::row(&app.db.lock().unwrap(), id)?.ok_or_else(|| Error::ProjectNotFound(id.to_owned()))
}

/// `WorktreeDirectory.canonical`: the directory with symlinks resolved, which must exist.
fn canonical(input: &str) -> Result<String, Error> {
    let resolved = crate::project::resolve_path(input);
    if !Path::new(&resolved).is_dir() {
        return Err(Error::DirectoryUnavailable(input.to_owned()));
    }
    Ok(resolved)
}

fn stored_list(db: &Connection, project_id: &str) -> rusqlite::Result<Vec<Stored>> {
    let mut statement = db.prepare(
        "SELECT directory, strategy FROM worktree WHERE project_id = ?1 ORDER BY time_created DESC, directory ASC",
    )?;
    let rows = statement.query_map(params![project_id], |row| Ok(Stored { directory: row.get(0)?, strategy: row.get(1)? }))?.collect();
    rows
}

fn find(db: &Connection, project_id: &str, directory: &str) -> rusqlite::Result<Option<Stored>> {
    db.query_row(
        "SELECT directory, strategy FROM worktree WHERE project_id = ?1 AND directory = ?2",
        params![project_id, directory],
        |row| Ok(Stored { directory: row.get(0)?, strategy: row.get(1)? }),
    )
    .optional()
}

/// `ops.create`: records a directory; on conflict a strategy may claim an unowned row (or `replace` it outright),
/// but never take another strategy's. Whether the row changed.
fn store(db: &Connection, project_id: &str, directory: &str, strategy: Option<&str>, replace: bool, now: i64) -> rusqlite::Result<bool> {
    let condition = if replace { "" } else if strategy.is_some() { " WHERE worktree.strategy IS NULL" } else { " WHERE false" };
    let sql = format!(
        "INSERT INTO worktree (project_id, directory, strategy, time_created) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (project_id, directory) DO UPDATE SET strategy = ?3{condition} RETURNING directory"
    );
    Ok(db.query_row(&sql, params![project_id, directory, strategy, now], |_| Ok(())).optional()?.is_some())
}

fn forget(db: &Connection, project_id: &str, directory: &str) -> rusqlite::Result<bool> {
    Ok(db
        .query_row("DELETE FROM worktree WHERE project_id = ?1 AND directory = ?2 RETURNING directory", params![project_id, directory], |_| Ok(()))
        .optional()?
        .is_some())
}

fn changed(app: &App, project_id: &str, update: bool) {
    if update {
        let mut data = Obj::new();
        data.str("projectID", project_id);
        app.bus.emit("worktree.updated", data.done(), PublishOptions::default());
    }
}

/// What the canonical checkout's location configured (`WorktreeStrategies.get()`): where new worktrees go.
struct Settings {
    directory: String,
}

/// `load`: boots the canonical checkout's location and reads its worktree settings.
async fn load(app: &App, canonical: &str) -> Result<Settings, Error> {
    if !Path::new(canonical).is_dir() {
        return Err(Error::DirectoryUnavailable(canonical.to_owned()));
    }
    let location = app.locations.get(app, &crate::bus::LocationRef::directory(canonical)).await.map_err(Error::Fatal)?;
    let data = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    let id: String = location.project.id.encode_utf16().take(6).map(|u| char::from_u32(u as u32).unwrap_or('\u{fffd}')).collect();
    let mut directory = format!("{data}/opencode/worktree/{id}");
    // The `worktree.directory` config option (config/plugin/worktree.ts).
    let inventory = location.inventory();
    if let Some(configured) = inventory.latest(|config| config.worktree.as_ref()).map(|w| w.directory.as_str()) {
        directory = match configured.strip_prefix("~/") {
            Some(rest) => {
                let home = std::env::var("OPENCODE_TEST_HOME").or_else(|_| std::env::var("HOME")).unwrap_or_default();
                crate::project::normalize(&format!("{home}/{rest}"))
            }
            None => crate::fs::resolve(&location.project.canonical, configured),
        };
    }
    Ok(Settings { directory })
}

async fn discover(input: &str) -> Option<Repository> {
    let input = input.to_owned();
    tokio::task::spawn_blocking(move || crate::project::repository(&input)).await.ok().flatten()
}

/// `worktreeRun`: git's output, or its error message.
async fn git(cwd: &str, args: Vec<String>, operation: &str) -> Result<String, Error> {
    let output = Command::new(GIT, cwd)
        .args(args)
        .run()
        .await
        .map_err(|e| Error::Operation { message: e.to_string(), force_required: None })?;
    if output.code == 0 {
        return Ok(output.text());
    }
    let stderr = output.error_text();
    let stdout = output.text();
    let message = [crate::text::trim(&stderr), crate::text::trim(&stdout)].into_iter().find(|m| !m.is_empty()).unwrap_or("Git failed").to_owned();
    let lower = message.to_lowercase();
    let force = operation == "remove" && (lower.contains("contains modified or untracked files") || lower.contains("is dirty"));
    Err(Error::Operation { message, force_required: Some(force) })
}

/// `Worktree.list`.
pub fn list(app: &App, project_id: &str) -> Result<Vec<Stored>, Error> {
    project_row(app, project_id)?;
    Ok(stored_list(&app.db.lock().unwrap(), project_id)?)
}

pub struct CreateInput {
    pub project_id: String,
    pub name: Option<String>,
    pub directory: Option<String>,
    pub from: Option<String>,
    pub branch: Option<String>,
}

/// `Worktree.create`: a linked worktree of the source checkout (the canonical one unless `from` names another known
/// checkout), at `<directory>/<name>` (`-2`, `-3`, ... when taken), then the project's start command in it.
pub async fn create(app: &App, input: CreateInput) -> Result<String, Error> {
    let row = project_row(app, &input.project_id)?;
    let settings = load(app, &row.canonical).await?;
    let directory = input.directory.unwrap_or(settings.directory);
    let from = input.from.unwrap_or_else(|| row.canonical.clone());
    let source = canonical(&from)?;
    if find(&app.db.lock().unwrap(), &input.project_id, &source)?.is_none() {
        return Err(Error::SourceNotFound { project_id: input.project_id, directory: Some(source) });
    }
    std::fs::create_dir_all(&directory).map_err(|e| Error::Fatal(crate::app::defect(e)))?;
    let name = input.name.unwrap_or_else(crate::id::slug);
    let mut suffix = 1;
    let mut target = crate::project::normalize(&format!("{directory}/{name}"));
    while Path::new(&target).exists() {
        suffix += 1;
        if suffix > 10 {
            return Err(Error::DestinationExists(target));
        }
        target = crate::project::normalize(&format!("{directory}/{name}-{suffix}"));
    }
    // The git strategy (worktree/git.ts).
    let Some(repository) = discover(&source).await else { return Err(Error::DirectoryUnavailable(source)) };
    let reference = input.branch.unwrap_or_else(|| "HEAD".into());
    git(&repository.worktree, vec!["worktree".into(), "add".into(), "--detach".into(), "--".into(), target.clone(), reference], "create").await?;
    if discover(&target).await.is_none() {
        return Err(Error::Operation { message: "Created worktree could not be opened".into(), force_required: None });
    }
    let created = canonical(&canonical(&target)?)?;
    let updated = store(&app.db.lock().unwrap(), &input.project_id, &created, Some("git"), true, crate::id::now())?;
    changed(app, &input.project_id, updated);
    if let Some(command) = row.start.as_deref().map(crate::text::trim).filter(|c| !c.is_empty()) {
        let process = Command::new("bash", &created)
            .args(["-lc", command])
            .env("OPENCODE_WORKTREE_BASE", &source)
            .env("OPENCODE_WORKTREE_PATH", &created);
        let description = process.describe();
        let output = process.run().await.map_err(|e| Error::Process(format!("Command failed: {description}: {e}")))?;
        if output.code != 0 {
            let stderr = output.error_text();
            let detail = crate::text::trim(&stderr);
            let detail = if detail.is_empty() { String::new() } else { format!(": {detail}") };
            return Err(Error::Process(format!("Command failed (exit {}): {description}{detail}", output.code)));
        }
    }
    Ok(created)
}

/// `Worktree.remove`: only worktrees a strategy owns; git refuses a dirty one unless forced.
pub async fn remove(app: &App, project_id: &str, directory: &str, force: bool) -> Result<(), Error> {
    let row = project_row(app, project_id)?;
    let target = canonical(directory)?;
    let stored = find(&app.db.lock().unwrap(), project_id, &target)?;
    let Some(strategy) = stored.and_then(|s| s.strategy) else { return Err(Error::InvalidDirectory(target)) };
    load(app, &row.canonical).await?;
    if strategy != "git" {
        return Err(Error::StrategyUnavailable(strategy));
    }
    let Some(repository) = discover(&target).await else { return Err(Error::DirectoryUnavailable(target)) };
    let mut args = vec!["worktree".to_owned(), "remove".into()];
    if force {
        args.push("--force".into());
    }
    args.push(target.clone());
    git(&repository.common_directory, args, "remove").await?;
    let removed = forget(&app.db.lock().unwrap(), project_id, &target)?;
    changed(app, project_id, removed);
    Ok(())
}

/// `Worktree.refresh`: records the checkouts git reports from each known root and forgets the directories that are
/// gone.
pub async fn refresh(app: &App, project_id: &str) -> Result<(), Error> {
    let row = project_row(app, project_id)?;
    load(app, &row.canonical).await?;
    let stored = stored_list(&app.db.lock().unwrap(), project_id)?;
    let checked: Vec<(Stored, bool)> = stored.into_iter().map(|s| { let exists = Path::new(&s.directory).is_dir(); (s, exists) }).collect();
    let mut roots = indexmap::IndexSet::new();
    roots.insert(row.canonical.clone());
    roots.extend(checked.iter().filter(|(s, exists)| *exists && s.strategy.is_none()).map(|(s, _)| s.directory.clone()));
    let mut discovered: indexmap::IndexMap<String, Option<&'static str>> = indexmap::IndexMap::new();
    for root in &roots {
        if !Path::new(root).is_dir() {
            continue;
        }
        // The git strategy's `list`; a failure is only logged.
        let Some(repository) = discover(root).await else { continue };
        let Ok(text) = git(&repository.worktree, vec!["worktree".into(), "list".into(), "--porcelain".into()], "list").await else {
            continue;
        };
        let entries = text.split('\n').filter_map(|line| line.strip_prefix("worktree ")).enumerate();
        for (index, entry) in entries {
            let path = crate::fs::resolve(&repository.worktree, crate::text::trim(entry));
            let Ok(directory) = canonical(&path) else { continue };
            discovered.entry(directory).or_insert(if index == 0 { None } else { Some("git") });
        }
    }
    let removed: Vec<String> = checked.into_iter().filter(|(_, exists)| !exists).map(|(s, _)| s.directory).collect();
    let mut changes = false;
    {
        let mut db = app.db.lock().unwrap();
        let tx = db.transaction()?;
        let now = crate::id::now();
        for (directory, strategy) in &discovered {
            changes |= store(&tx, project_id, directory, *strategy, false, now)?;
        }
        for directory in &removed {
            changes |= forget(&tx, project_id, directory)?;
        }
        tx.commit()?;
    }
    changed(app, project_id, changes);
    Ok(())
}
