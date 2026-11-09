//! Background jobs (packages/core/src/job.ts, job/list.ts, job/recovery.ts, job/sql.ts, session/background.ts): a
//! process-local registry of running work (a subagent, a shell command, an image) that a caller can wait on, block a
//! session on, move to the background, or cancel. Backgrounded work that can be recovered is also recorded in the
//! durable `job` table until its completion notification is admitted ([`Jobs::complete_background`]).
//!
//! # Interface (for the subagent, shell and image tools)
//!
//! - [`Jobs::start`] registers a job and runs its future; a running job with the same ID is returned instead. Its
//!   output (`Ok(text)`) or error settles the job as `completed` or `error`; [`Jobs::cancel`] aborts it (`cancelled`).
//! - [`Jobs::wait`] waits for it (optionally with a timeout); [`Jobs::block`] waits on behalf of a session until it
//!   finishes or is moved to the background ([`Jobs::background`], [`Jobs::background_all`] for every job a session
//!   is blocked on). Finished jobs are consumed by the first wait or block that sees them; at most 25 consumed
//!   history entries are kept.
//! - [`Jobs::cancel_session`] cancels a session's jobs and forgets its durable records (a removed session).
//! - [`list`] is the `GET /api/experimental/job` listing (`SessionBackground.list`).

// The registry is the background tools' (the subagent and shell tools, ported concurrently): allow it to be unused
// until then.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use opencode_protocol::Json;
use rusqlite::{OptionalExtension, params};
use tokio::sync::watch;

use crate::app::{App, fatal};
use crate::json::{self, Obj};

const COMPLETED_LIMIT: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Completed,
    Error,
    Cancelled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Completed => "completed",
            Status::Error => "error",
            Status::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Option<Status> {
        Some(match text {
            "running" => Status::Running,
            "completed" => Status::Completed,
            "error" => Status::Error,
            "cancelled" => Status::Cancelled,
            _ => return None,
        })
    }
}

/// How backgrounded work is found again after a restart (`Job.Recovery`).
#[derive(Debug, Clone, PartialEq)]
pub enum Recovery {
    Shell { session_id: String, shell_id: String, command: String },
    Subagent { parent_session_id: String, child_session_id: String, agent: String, description: String },
    Image { session_id: String, path: String, model: String },
}

impl Recovery {
    fn kind(&self) -> &'static str {
        match self {
            Recovery::Shell { .. } => "shell",
            Recovery::Subagent { .. } => "subagent",
            Recovery::Image { .. } => "image",
        }
    }

    /// `ownerSessionID`.
    pub fn owner(&self) -> &str {
        match self {
            Recovery::Subagent { parent_session_id, .. } => parent_session_id,
            Recovery::Shell { session_id, .. } | Recovery::Image { session_id, .. } => session_id,
        }
    }

    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        match self {
            Recovery::Shell { session_id, shell_id, command } => {
                o.str("kind", "shell").str("sessionID", session_id).str("shellID", shell_id).str("command", command);
            }
            Recovery::Subagent { parent_session_id, child_session_id, agent, description } => {
                o.str("kind", "subagent").str("parentSessionID", parent_session_id).str("childSessionID", child_session_id);
                o.str("agent", agent).str("description", description);
            }
            Recovery::Image { session_id, path, model } => {
                o.str("kind", "image").str("sessionID", session_id).str("path", path).str("model", model);
            }
        }
        o.done()
    }

    /// `RecoverySchema` decoding (`None` when the stored value does not fit).
    pub fn from_json(value: &Json) -> Option<Recovery> {
        let text = |key: &str| json::get_str(value, key).map(str::to_owned);
        let session = |key: &str| text(key).filter(|id| id.starts_with("ses"));
        Some(match json::get_str(value, "kind")? {
            "shell" => Recovery::Shell { session_id: session("sessionID")?, shell_id: text("shellID")?, command: text("command")? },
            "subagent" => Recovery::Subagent {
                parent_session_id: session("parentSessionID")?,
                child_session_id: session("childSessionID")?,
                agent: text("agent")?,
                description: text("description")?,
            },
            "image" => Recovery::Image { session_id: session("sessionID")?, path: text("path")?, model: text("model")? },
            _ => return None,
        })
    }
}

/// A job as callers see it (`Job.Info`).
#[derive(Debug, Clone)]
pub struct Info {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub status: Status,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub metadata: Option<Json>,
    pub notification_id: Option<String>,
}

struct Active {
    info: Info,
    /// Changes once when the job finishes or is backgrounded, so waiters can tell which.
    events: watch::Sender<Signal>,
    task: Option<tokio::task::AbortHandle>,
    /// A token per start of this ID: settling a job checks it is still the same run (`job.scope`).
    generation: u64,
    blocking: HashMap<String, usize>,
    backgrounded: bool,
    consumed: bool,
    recovery: Option<Recovery>,
}

#[derive(Debug, Clone, Default)]
struct Signal {
    done: Option<Info>,
    backgrounded: Option<Info>,
}

/// The process's job registry (`Job.Service`).
#[derive(Default)]
pub struct Jobs {
    jobs: Mutex<IndexMap<String, Active>>,
    generations: Mutex<u64>,
}

/// What [`Jobs::start`] runs: the job's output text, or its error message.
pub type Run = std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>;

pub struct StartInput {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub metadata: Option<Json>,
    pub recovery: Option<Recovery>,
    pub notification_id: Option<String>,
    pub run: Run,
}

/// [`Jobs::block`]'s result.
#[derive(Debug, Clone)]
pub enum Blocked {
    Finished(Info),
    Backgrounded(Info),
}

pub fn jobs() -> &'static Jobs {
    static JOBS: std::sync::OnceLock<Jobs> = std::sync::OnceLock::new();
    JOBS.get_or_init(Jobs::default)
}

fn matches_session(job: &Active, session_id: &str) -> bool {
    if job.info.metadata.as_ref().and_then(|m| json::get_str(m, "sessionID")) == Some(session_id) {
        return true;
    }
    if job.blocking.contains_key(session_id) {
        return true;
    }
    match &job.recovery {
        None => false,
        Some(Recovery::Subagent { parent_session_id, child_session_id, .. }) => {
            parent_session_id == session_id || child_session_id == session_id
        }
        Some(recovery) => recovery.owner() == session_id,
    }
}

/// `persistBackground`: the durable record of a backgrounded, recoverable job.
fn persist(app: &App, job: &Active) {
    let (Some(recovery), Some(notification)) = (&job.recovery, &job.info.notification_id) else { return };
    let now = crate::id::now();
    let title = job.info.title.clone().or_else(|| match recovery {
        Recovery::Shell { command, .. } => Some(command.clone()),
        Recovery::Subagent { description, .. } => Some(description.clone()),
        Recovery::Image { path, .. } => Some(path.clone()),
    });
    let child = match recovery {
        Recovery::Subagent { child_session_id, .. } => Some(child_session_id.clone()),
        _ => None,
    };
    let result = app.db.lock().unwrap().execute(
        "INSERT INTO job (notification_id, id, session_id, child_session_id, kind, title, status, output, error, recovery, \
         time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
         ON CONFLICT (notification_id) DO UPDATE SET status = excluded.status, output = excluded.output, \
         error = excluded.error, recovery = excluded.recovery, time_updated = excluded.time_updated",
        params![
            notification,
            job.info.id,
            recovery.owner(),
            child,
            recovery.kind(),
            title,
            job.info.status.as_str(),
            job.info.output,
            job.info.error,
            json::text(&recovery.to_json()),
            job.info.started_at,
            now
        ],
    );
    if let Err(error) = result {
        let _ = fatal(error);
    }
}

impl Jobs {
    pub fn get(&self, id: &str) -> Option<Info> {
        self.jobs.lock().unwrap().get(id).map(|job| job.info.clone())
    }

    /// `Job.start`: registers the job and runs it; a running job with the same ID is returned as it is.
    pub fn start(&'static self, app: Arc<App>, input: StartInput) -> Info {
        let generation = {
            let mut generations = self.generations.lock().unwrap();
            *generations += 1;
            *generations
        };
        let info = {
            let mut jobs = self.jobs.lock().unwrap();
            if let Some(existing) = jobs.get(&input.id).filter(|job| job.info.status == Status::Running) {
                return existing.info.clone();
            }
            let info = Info {
                id: input.id.clone(),
                kind: input.kind,
                title: input.title,
                status: Status::Running,
                started_at: crate::id::now(),
                completed_at: None,
                output: None,
                error: None,
                metadata: input.metadata,
                notification_id: input.notification_id,
            };
            let (events, _) = watch::channel(Signal::default());
            // A restarted ID keeps its place (`Map.set`).
            jobs.insert(
                input.id.clone(),
                Active {
                    info: info.clone(),
                    events,
                    task: None,
                    generation,
                    blocking: HashMap::new(),
                    backgrounded: false,
                    consumed: false,
                    recovery: input.recovery,
                },
            );
            info
        };
        let id = input.id.clone();
        let run = input.run;
        let task = tokio::spawn(async move {
            let result = run.await;
            self.settle(&app, &id, generation, result);
        });
        if let Some(job) = self.jobs.lock().unwrap().get_mut(&input.id).filter(|job| job.generation == generation) {
            job.task = Some(task.abort_handle());
        }
        info
    }

    fn settle(&self, app: &App, id: &str, generation: u64, result: Result<String, String>) {
        let mut jobs = self.jobs.lock().unwrap();
        let Some(job) = jobs.get_mut(id).filter(|job| job.generation == generation) else { return };
        if job.info.status != Status::Running {
            return;
        }
        job.blocking.clear();
        job.info.completed_at = Some(crate::id::now());
        match result {
            Ok(output) => {
                job.info.status = Status::Completed;
                job.info.output = Some(output);
            }
            Err(error) => {
                job.info.status = Status::Error;
                job.info.error = Some(if error.is_empty() { "Unknown error".into() } else { error });
            }
        }
        persist(app, job);
        let info = job.info.clone();
        job.events.send_modify(|signal| signal.done = Some(info));
        job.task = None;
    }

    /// `consume`: the first consumer of a finished job moves it to the end of the history.
    fn consume(&self, id: &str, generation: u64) {
        let mut jobs = self.jobs.lock().unwrap();
        let Some(job) = jobs.get(id) else { return };
        if job.generation != generation || job.info.status == Status::Running || job.consumed {
            return;
        }
        let Some(mut job) = jobs.shift_remove(id) else { return };
        job.consumed = true;
        jobs.insert(id.to_owned(), job);
        let completed: Vec<String> =
            jobs.iter().filter(|(_, job)| job.consumed && job.info.notification_id.is_none()).map(|(id, _)| id.clone()).collect();
        if completed.len() > COMPLETED_LIMIT {
            for id in &completed[..completed.len() - COMPLETED_LIMIT] {
                jobs.shift_remove(id);
            }
        }
    }

    /// `Job.wait`: the job's info once it finishes (`timed_out` when the timeout passed first); `None` for an unknown
    /// job.
    pub async fn wait(&self, id: &str, timeout: Option<u64>) -> (Option<Info>, bool) {
        let (mut events, generation, recoverable, current) = {
            let jobs = self.jobs.lock().unwrap();
            let Some(job) = jobs.get(id) else { return (None, false) };
            (job.events.subscribe(), job.generation, job.recovery.is_some(), job.info.clone())
        };
        let result = if current.status != Status::Running {
            (Some(current), false)
        } else {
            let done = async {
                loop {
                    if let Some(info) = events.borrow_and_update().done.clone() {
                        return Some(info);
                    }
                    if events.changed().await.is_err() {
                        return None;
                    }
                }
            };
            match timeout {
                None => (done.await.or(Some(current)), false),
                Some(0) => (Some(current), true),
                Some(millis) => match tokio::time::timeout(std::time::Duration::from_millis(millis), done).await {
                    Ok(info) => (info.or(Some(current)), false),
                    Err(_) => (self.get(id).or(Some(current)), true),
                },
            }
        };
        // Recoverable wait -> background is a supported handoff, even after failure.
        if result.0.as_ref().is_some_and(|info| info.status != Status::Running) && !recoverable {
            self.consume(id, generation);
        }
        result
    }

    /// `Job.block`: waits for the job on behalf of a session until it finishes or is backgrounded.
    pub async fn block(&self, id: &str, session_id: &str) -> Option<Blocked> {
        let (mut events, generation) = {
            let mut jobs = self.jobs.lock().unwrap();
            let job = jobs.get_mut(id)?;
            if job.info.status != Status::Running {
                let (info, generation) = (job.info.clone(), job.generation);
                drop(jobs);
                self.consume(id, generation);
                return Some(Blocked::Finished(info));
            }
            if job.backgrounded {
                return Some(Blocked::Backgrounded(job.info.clone()));
            }
            *job.blocking.entry(session_id.to_owned()).or_default() += 1;
            (job.events.subscribe(), job.generation)
        };
        let outcome = loop {
            let signal = events.borrow_and_update().clone();
            if let Some(info) = signal.done {
                break Some(Blocked::Finished(info));
            }
            if let Some(info) = signal.backgrounded {
                break Some(Blocked::Backgrounded(info));
            }
            if events.changed().await.is_err() {
                break None;
            }
        };
        if let Some(Blocked::Finished(_)) = &outcome {
            self.consume(id, generation);
        }
        // `removeBlock`.
        if let Some(job) = self.jobs.lock().unwrap().get_mut(id) {
            if job.info.status == Status::Running && !job.backgrounded {
                if let Some(count) = job.blocking.get_mut(session_id) {
                    *count -= 1;
                    if *count == 0 {
                        job.blocking.remove(session_id);
                    }
                }
            }
        }
        outcome
    }

    /// `markBackground`.
    fn mark_background(app: &App, job: &mut Active) {
        job.backgrounded = true;
        job.blocking.clear();
        if job.recovery.is_some() && job.info.notification_id.is_none() {
            job.info.notification_id = Some(crate::session::message::create_id());
        }
        persist(app, job);
    }

    /// `Job.background`: moves a job to the background, releasing whoever blocks on it.
    pub fn background(&self, app: &App, id: &str) -> Option<Info> {
        let mut jobs = self.jobs.lock().unwrap();
        let job = jobs.get_mut(id)?;
        // Recoverable work may finish before the caller backgrounds it.
        if job.info.status != Status::Running && job.recovery.is_none() {
            return None;
        }
        if job.backgrounded {
            return Some(job.info.clone());
        }
        Self::mark_background(app, job);
        let info = job.info.clone();
        job.events.send_modify(|signal| signal.backgrounded = Some(info.clone()));
        Some(info)
    }

    /// `Job.backgroundAll`: every running job the session is blocked on (of one kind, when given).
    pub fn background_all(&self, app: &App, session_id: &str, kind: Option<&str>) -> Vec<Info> {
        let mut jobs = self.jobs.lock().unwrap();
        let mut moved = Vec::new();
        for job in jobs.values_mut() {
            if job.info.status != Status::Running || job.backgrounded {
                continue;
            }
            if kind.is_some_and(|kind| job.info.kind != kind) || !job.blocking.contains_key(session_id) {
                continue;
            }
            Self::mark_background(app, job);
            let info = job.info.clone();
            job.events.send_modify(|signal| signal.backgrounded = Some(info.clone()));
            moved.push(info);
        }
        moved
    }

    fn cancel_internal(&self, app: &App, id: &str, persisted: bool) -> Option<Info> {
        let (info, generation, task) = {
            let mut jobs = self.jobs.lock().unwrap();
            let job = jobs.get_mut(id)?;
            if job.info.status != Status::Running {
                let (info, generation) = (job.info.clone(), job.generation);
                drop(jobs);
                self.consume(id, generation);
                return Some(info);
            }
            job.blocking.clear();
            job.info.status = Status::Cancelled;
            job.info.completed_at = Some(crate::id::now());
            if persisted {
                persist(app, job);
            }
            let info = job.info.clone();
            job.events.send_modify(|signal| signal.done = Some(info.clone()));
            (info, job.generation, job.task.take())
        };
        if let Some(task) = task {
            task.abort();
        }
        self.consume(id, generation);
        Some(info)
    }

    /// `Job.cancel`.
    pub fn cancel(&self, app: &App, id: &str) -> Option<Info> {
        self.cancel_internal(app, id, true)
    }

    /// `Job.cancelSession`: cancels the session's running jobs and forgets its jobs and durable records.
    pub fn cancel_session(&self, app: &App, session_id: &str) -> Vec<Info> {
        let ids: Vec<String> = self
            .jobs
            .lock()
            .unwrap()
            .values()
            .filter(|job| job.info.status == Status::Running && matches_session(job, session_id))
            .map(|job| job.info.id.clone())
            .collect();
        let cancelled: Vec<Info> = ids.iter().filter_map(|id| self.cancel_internal(app, id, false)).collect();
        if let Err(error) = app
            .db
            .lock()
            .unwrap()
            .execute("DELETE FROM job WHERE session_id = ?1 OR child_session_id = ?1", params![session_id])
        {
            let _ = fatal(error);
        }
        self.jobs.lock().unwrap().retain(|_, job| !matches_session(job, session_id));
        cancelled
    }

    /// `Job.runningBackground`: the recovery records of running background jobs.
    pub fn running_background(&self) -> Vec<Recovery> {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .filter(|job| job.backgrounded && job.info.status == Status::Running)
            .filter_map(|job| job.recovery.clone())
            .collect()
    }

    /// `Job.completeBackground`: the job's notification was admitted.
    pub fn complete_background(&self, app: &App, notification_id: &str) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Err(error) = app.db.lock().unwrap().execute("DELETE FROM job WHERE notification_id = ?1", params![notification_id]) {
            let _ = fatal(error);
        }
        let found = jobs
            .iter()
            .find(|(_, job)| job.info.notification_id.as_deref() == Some(notification_id))
            .map(|(id, job)| (id.clone(), job.info.status));
        if let Some((id, status)) = found {
            if status != Status::Running {
                jobs.shift_remove(&id);
            }
        }
    }

    /// `JobList.read`: durable records (with the live state of their run), then live backgrounded jobs without one.
    pub fn background_jobs(&self, app: &App) -> Vec<Json> {
        type Stored = (String, String, String, Option<String>, String, i64, i64);
        let rows: Vec<Stored> = {
            let db = app.db.lock().unwrap();
            let rows = db
                .prepare("SELECT notification_id, id, status, title, recovery, time_created, time_updated FROM job ORDER BY time_created ASC, notification_id ASC")
                .and_then(|mut statement| {
                    statement
                        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)))
                        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
                });
            match rows {
                Ok(rows) => rows,
                Err(error) => {
                    let _ = fatal(error);
                    vec![]
                }
            }
        };
        let jobs = self.jobs.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut items = Vec::new();
        for (notification, id, status, title, recovery, created, updated) in rows {
            let Some(recovery) = json::parse(&recovery).as_ref().and_then(Recovery::from_json) else { continue };
            let Some(stored_status) = Status::parse(&status) else { continue };
            seen.insert(notification.clone());
            let live = jobs.get(&id).filter(|live| live.info.notification_id.as_deref() == Some(notification.as_str()));
            let status = live.map_or(stored_status, |live| live.info.status);
            let completed = match live {
                Some(live) => live.info.completed_at,
                None => (stored_status != Status::Running).then_some(updated),
            };
            let title = live.and_then(|live| live.info.title.clone()).or(title);
            items.push(item(&id, status, title.as_deref(), created, completed, &recovery));
        }
        for live in jobs.values() {
            let Some(recovery) = live.recovery.as_ref().filter(|_| live.backgrounded) else { continue };
            if live.info.notification_id.as_ref().is_some_and(|n| seen.contains(n)) {
                continue;
            }
            items.push(item(&live.info.id, live.info.status, live.info.title.as_deref(), live.info.started_at, live.info.completed_at, recovery));
        }
        items
    }
}

/// `toItem`: a `Job.Info` of the listing.
fn item(id: &str, status: Status, title: Option<&str>, started: i64, completed: Option<i64>, recovery: &Recovery) -> Json {
    let mut time = Obj::new();
    time.int("started", started);
    if let Some(completed) = completed {
        time.int("completed", completed);
    }
    let mut o = Obj::new();
    o.str("kind", recovery.kind()).str("id", id);
    match recovery {
        Recovery::Subagent { parent_session_id, .. } => o.str("sessionID", parent_session_id),
        Recovery::Shell { session_id, .. } | Recovery::Image { session_id, .. } => o.str("sessionID", session_id),
    };
    o.str("status", status.as_str()).opt_str("title", title).set("time", time.done());
    match recovery {
        Recovery::Shell { shell_id, command, .. } => {
            o.str("shellID", shell_id).str("command", command);
        }
        Recovery::Subagent { child_session_id, agent, description, .. } => {
            o.str("childSessionID", child_session_id).str("agent", agent).str("description", description);
        }
        Recovery::Image { path, model, .. } => {
            o.str("path", path).str("model", model);
        }
    }
    o.done()
}

/// `SessionBackground.list`: active and unconsumed background jobs of a session (and, with `descendants`, of its
/// child and nested subagent sessions), by kind and status.
pub fn list(app: &App, session_id: Option<&str>, kind: Option<&str>, status: Option<&str>, descendants: bool) -> Vec<Json> {
    let items: Vec<Json> = jobs()
        .background_jobs(app)
        .into_iter()
        .filter(|item| kind.is_none_or(|kind| json::get_str(item, "kind") == Some(kind)))
        .filter(|item| status.is_none_or(|status| json::get_str(item, "status") == Some(status)))
        .collect();
    let Some(session_id) = session_id else { return items };
    if !descendants {
        return items.into_iter().filter(|item| json::get_str(item, "sessionID") == Some(session_id)).collect();
    }
    let mut parents: HashMap<String, Option<String>> = HashMap::new();
    items
        .into_iter()
        .filter(|item| descends(app, &mut parents, json::get_str(item, "sessionID").unwrap_or_default(), session_id))
        .collect()
}

/// Whether `from` is `ancestor` or one of its descendants.
fn descends(app: &App, parents: &mut HashMap<String, Option<String>>, from: &str, ancestor: &str) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut current = Some(from.to_owned());
    while let Some(id) = current {
        if id == ancestor {
            return true;
        }
        if !seen.insert(id.clone()) {
            return false;
        }
        if let Some(cached) = parents.get(&id) {
            current = cached.clone();
            continue;
        }
        let parent: Option<String> = app
            .db
            .lock()
            .unwrap()
            .query_row("SELECT parent_id FROM session_v2 WHERE id = ?1", params![id], |row| row.get(0))
            .optional()
            .ok()
            .flatten()
            .flatten();
        parents.insert(id, parent.clone());
        current = parent;
    }
    false
}
