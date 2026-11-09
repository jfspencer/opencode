//! Jobs (packages/core/src/job.ts, job/list.ts, job/recovery.ts, job/sql.ts): one process-local registry of
//! long-running work (subagents, background shells, image requests) keyed by job ID, backed by the durable `job`
//! table for work that was moved to the background until its completion notice is admitted.
//!
//! A job runs one future per generation ([`Jobs::start`] joins a running job instead of starting another). Callers
//! wait for it ([`Jobs::wait`]), block on it from a session until it finishes or is moved to the background
//! ([`Jobs::block`], [`Jobs::background`], [`Jobs::background_all`]), or cancel it ([`Jobs::cancel`],
//! [`Jobs::cancel_session`]). A backgrounded job with a recovery gets a notification ID (the ID of the notice its
//! completion admits into its owner session) and a `job` row, removed once the notice is admitted
//! ([`Jobs::complete_background`]). Finished jobs are kept until consumed (waited for or blocked on), and at most 25
//! consumed ones without a notification are remembered.
//!
//! Restart recovery works from the durable rows ([`Jobs::pending_background`], [`crate::session::restart`]).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use opencode_protocol::Json;
use rusqlite::params;
use tokio::sync::watch;

use crate::app::App;
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

/// How backgrounded work is recovered and listed (`Job.Recovery`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Shell and image jobs are the shell and image tools'.
pub enum Recovery {
    Shell { session_id: String, shell_id: String, command: String },
    Subagent { parent_session_id: String, child_session_id: String, agent: String, description: String },
    Image { session_id: String, path: String, model: String },
}

impl Recovery {
    pub fn kind(&self) -> &'static str {
        match self {
            Recovery::Shell { .. } => "shell",
            Recovery::Subagent { .. } => "subagent",
            Recovery::Image { .. } => "image",
        }
    }

    /// The session the work belongs to (`ownerSessionID`): a subagent's parent.
    pub fn owner(&self) -> &str {
        match self {
            Recovery::Shell { session_id, .. } | Recovery::Image { session_id, .. } => session_id,
            Recovery::Subagent { parent_session_id, .. } => parent_session_id,
        }
    }

    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        o.str("kind", self.kind());
        match self {
            Recovery::Shell { session_id, shell_id, command } => {
                o.str("sessionID", session_id).str("shellID", shell_id).str("command", command);
            }
            Recovery::Subagent { parent_session_id, child_session_id, agent, description } => {
                o.str("parentSessionID", parent_session_id)
                    .str("childSessionID", child_session_id)
                    .str("agent", agent)
                    .str("description", description);
            }
            Recovery::Image { session_id, path, model } => {
                o.str("sessionID", session_id).str("path", path).str("model", model);
            }
        }
        o.done()
    }

    /// `RecoverySchema` decoding of a stored row.
    fn from_json(value: &Json) -> Option<Recovery> {
        let field = |key: &str| json::get_str(value, key).map(str::to_owned);
        let session = |key: &str| field(key).filter(|id| id.starts_with("ses"));
        Some(match json::get_str(value, "kind")? {
            "shell" => Recovery::Shell { session_id: session("sessionID")?, shell_id: field("shellID")?, command: field("command")? },
            "subagent" => Recovery::Subagent {
                parent_session_id: session("parentSessionID")?,
                child_session_id: session("childSessionID")?,
                agent: field("agent")?,
                description: field("description")?,
            },
            "image" => Recovery::Image { session_id: session("sessionID")?, path: field("path")?, model: field("model")? },
            _ => return None,
        })
    }

    /// `recoveryTitle`'s fallback when the job has no title.
    fn title(&self) -> &str {
        match self {
            Recovery::Shell { command, .. } => command,
            Recovery::Subagent { description, .. } => description,
            Recovery::Image { path, .. } => path,
        }
    }
}

/// A job's state (`Job.Info`).
#[derive(Debug, Clone)]
pub struct Info {
    pub id: String,
    pub ty: String,
    pub title: Option<String>,
    pub status: Status,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub metadata: Option<Json>,
    pub notification_id: Option<String>,
}

/// Why a job's run ended without output: its error text (`errorText`), or interruption (a cancelled job).
#[derive(Debug, Clone)]
pub enum RunError {
    Failed(String),
    Cancelled,
}

pub type Run = Pin<Box<dyn Future<Output = Result<String, RunError>> + Send>>;

pub struct StartInput {
    pub id: String,
    pub ty: String,
    pub title: Option<String>,
    pub metadata: Option<Json>,
    pub recovery: Option<Recovery>,
    pub notification_id: Option<String>,
}

pub enum Blocked {
    Finished(Info),
    Backgrounded(Info),
}

struct Active {
    info: Info,
    /// The job's generation (`scope`): a restart of the same ID is a new one.
    generation: u64,
    done: watch::Sender<Option<Info>>,
    backgrounded: watch::Sender<Option<Info>>,
    task: Option<tokio::task::AbortHandle>,
    /// Sessions blocked on the job, with their count.
    blocking: Vec<(String, usize)>,
    is_backgrounded: bool,
    consumed: bool,
    recovery: Option<Recovery>,
}

impl Active {
    fn matches_session(&self, session: &str) -> bool {
        if self.info.metadata.as_ref().and_then(|m| json::get_str(m, "sessionID")) == Some(session) {
            return true;
        }
        if self.blocking.iter().any(|(s, _)| s == session) {
            return true;
        }
        match &self.recovery {
            None => false,
            Some(Recovery::Subagent { parent_session_id, child_session_id, .. }) => {
                parent_session_id == session || child_session_id == session
            }
            Some(recovery) => recovery.owner() == session,
        }
    }
}

#[derive(Default)]
struct State {
    /// In `Map` order: insertion, with a consumed job moved to the end.
    jobs: Vec<Active>,
    generations: u64,
}

impl State {
    fn find(&mut self, id: &str) -> Option<&mut Active> {
        self.jobs.iter_mut().find(|job| job.info.id == id)
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.jobs.iter().position(|job| job.info.id == id)
    }

    /// `consume`: a finished job is consumed once; history keeps the latest consumed jobs without a notification.
    fn consume(&mut self, id: &str, generation: u64) {
        let Some(index) = self.position(id) else { return };
        let job = &self.jobs[index];
        if job.generation != generation || job.info.status == Status::Running || job.consumed {
            return;
        }
        let mut job = self.jobs.remove(index);
        job.consumed = true;
        self.jobs.push(job);
        let completed: Vec<String> = self
            .jobs
            .iter()
            .filter(|job| job.consumed && job.info.notification_id.is_none())
            .map(|job| job.info.id.clone())
            .collect();
        if completed.len() > COMPLETED_LIMIT {
            let stale = &completed[..completed.len() - COMPLETED_LIMIT];
            self.jobs.retain(|job| !(stale.contains(&job.info.id) && job.consumed && job.info.notification_id.is_none()));
        }
    }
}

#[derive(Default)]
pub struct Jobs {
    state: Mutex<State>,
}

/// `persistBackground`: upserts the job's row while it is backgrounded with a recovery.
fn persist(app: &App, job: &Active) {
    let (Some(recovery), Some(notification)) = (&job.recovery, &job.info.notification_id) else { return };
    let now = crate::id::now();
    let child = match recovery {
        Recovery::Subagent { child_session_id, .. } => Some(child_session_id.as_str()),
        _ => None,
    };
    let title = job.info.title.as_deref().unwrap_or(recovery.title());
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
            now,
        ],
    );
    if let Err(error) = result {
        eprintln!("failed to persist background job {}: {error}", job.info.id);
    }
}

/// A `Job.Info` list item (`toItem`).
fn item(id: &str, status: Status, title: Option<&str>, started: i64, completed: Option<i64>, recovery: &Recovery) -> Json {
    let mut time = Obj::new();
    time.int("started", started);
    if let Some(completed) = completed {
        time.int("completed", completed);
    }
    let mut o = Obj::new();
    o.str("kind", recovery.kind()).str("id", id).str("sessionID", recovery.owner()).str("status", status.as_str());
    o.opt_str("title", title);
    o.set("time", time.done());
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

impl Jobs {
    /// `Job.get`.
    pub fn get(&self, id: &str) -> Option<Info> {
        self.state.lock().unwrap().find(id).map(|job| job.info.clone())
    }

    /// `Job.start`: a running job with this ID is joined (its snapshot returned, `run` never called); otherwise a new
    /// generation starts, and `run` is called at once (as TS starts the run immediately) and driven to settlement.
    pub fn start(&self, app: &App, input: StartInput, run: impl FnOnce() -> Run) -> Info {
        let (info, generation) = {
            let mut state = self.state.lock().unwrap();
            if let Some(existing) = state.find(&input.id).filter(|job| job.info.status == Status::Running) {
                return existing.info.clone();
            }
            state.generations += 1;
            let generation = state.generations;
            let info = Info {
                id: input.id.clone(),
                ty: input.ty,
                title: input.title,
                status: Status::Running,
                started_at: crate::id::now(),
                completed_at: None,
                output: None,
                error: None,
                metadata: input.metadata,
                notification_id: input.notification_id,
            };
            let job = Active {
                info: info.clone(),
                generation,
                done: watch::channel(None).0,
                backgrounded: watch::channel(None).0,
                task: None,
                blocking: Vec::new(),
                is_backgrounded: false,
                consumed: false,
                recovery: input.recovery,
            };
            match state.position(&input.id) {
                Some(index) => state.jobs[index] = job,
                None => state.jobs.push(job),
            }
            (info, generation)
        };
        let future = run();
        let owner = app.arc();
        let id = input.id;
        let task = {
            let id = id.clone();
            tokio::spawn(async move {
                let result = future.await;
                owner.sessions.jobs.settle(&owner, &id, generation, result);
            })
        };
        let mut state = self.state.lock().unwrap();
        match state.find(&id).filter(|job| job.generation == generation) {
            Some(job) if job.info.status == Status::Running => job.task = Some(task.abort_handle()),
            _ => task.abort(),
        }
        info
    }

    /// `settle`: records the run's outcome for its generation.
    fn settle(&self, app: &App, id: &str, generation: u64, result: Result<String, RunError>) {
        let mut state = self.state.lock().unwrap();
        let Some(job) = state.find(id) else { return };
        if job.generation != generation || job.info.status != Status::Running {
            return;
        }
        job.blocking.clear();
        job.info.completed_at = Some(crate::id::now());
        match result {
            Ok(output) => {
                job.info.status = Status::Completed;
                job.info.output = Some(output);
            }
            Err(RunError::Cancelled) => job.info.status = Status::Cancelled,
            Err(RunError::Failed(error)) => {
                job.info.status = Status::Error;
                job.info.error = Some(error);
            }
        }
        job.task = None;
        if job.info.status != Status::Cancelled {
            persist(app, job);
        }
        let info = job.info.clone();
        job.done.send_replace(Some(info));
    }

    /// `Job.wait` without a timeout: the job's outcome once it finishes (`None` for an unknown job).
    pub async fn wait(&self, id: &str) -> Option<Info> {
        self.wait_timeout(id, None).await.0
    }

    /// `Job.wait`: the job's outcome once it finishes, or after `timeout` its state then (`true`: timed out; a zero
    /// timeout only looks). `None` for an unknown job.
    pub async fn wait_timeout(&self, id: &str, timeout: Option<std::time::Duration>) -> (Option<Info>, bool) {
        let found = {
            let mut state = self.state.lock().unwrap();
            state.find(id).map(|job| (job.done.subscribe(), job.generation, job.recovery.is_some(), job.info.clone()))
        };
        let Some((mut done, generation, recoverable, snapshot)) = found else { return (None, false) };
        let (info, timed_out) = if snapshot.status != Status::Running {
            (Some(snapshot), false)
        } else {
            match timeout {
                None => (done.wait_for(Option::is_some).await.ok().and_then(|info| info.clone()), false),
                Some(timeout) if timeout.is_zero() => (Some(snapshot), true),
                Some(timeout) => match tokio::time::timeout(timeout, done.wait_for(Option::is_some)).await {
                    Ok(Ok(info)) => (info.clone(), false),
                    _ => (self.get(id).or(Some(snapshot)), true),
                },
            }
        };
        // A recoverable job may still be backgrounded after it finished.
        if info.as_ref().is_some_and(|info| info.status != Status::Running) && !recoverable {
            self.state.lock().unwrap().consume(id, generation);
        }
        (info, timed_out)
    }

    /// `Job.block`: waits on a running job from a session until it finishes or is moved to the background; the
    /// session counts as blocked meanwhile. `None` for an unknown job.
    pub async fn block(self: &Arc<Self>, id: &str, session: &str) -> Option<Blocked> {
        let (mut done, mut backgrounded, generation) = {
            let mut state = self.state.lock().unwrap();
            let job = state.find(id)?;
            if job.info.status != Status::Running {
                let (info, generation) = (job.info.clone(), job.generation);
                state.consume(id, generation);
                return Some(Blocked::Finished(info));
            }
            if job.is_backgrounded {
                return Some(Blocked::Backgrounded(job.info.clone()));
            }
            match job.blocking.iter_mut().find(|(s, _)| s == session) {
                Some((_, count)) => *count += 1,
                None => job.blocking.push((session.to_owned(), 1)),
            }
            (job.done.subscribe(), job.backgrounded.subscribe(), job.generation)
        };
        // `Effect.ensuring(removeBlock)`, also when the caller is dropped mid-wait.
        let _unblock = Unblock { jobs: self.clone(), id: id.to_owned(), session: session.to_owned() };
        tokio::select! {
            info = done.wait_for(Option::is_some) => {
                let info = info.ok()?.clone()?;
                self.state.lock().unwrap().consume(id, generation);
                Some(Blocked::Finished(info))
            }
            info = backgrounded.wait_for(Option::is_some) => Some(Blocked::Backgrounded(info.ok()?.clone()?)),
        }
    }

    fn remove_block(&self, id: &str, session: &str) {
        let mut state = self.state.lock().unwrap();
        let Some(job) = state.find(id) else { return };
        if job.info.status != Status::Running || job.is_backgrounded {
            return;
        }
        if let Some(index) = job.blocking.iter().position(|(s, _)| s == session) {
            if job.blocking[index].1 <= 1 {
                job.blocking.remove(index);
            } else {
                job.blocking[index].1 -= 1;
            }
        }
    }

    /// `markBackground`: backgrounded work with a recovery gets its notification ID and its row.
    fn mark_background(app: &App, job: &mut Active) {
        job.is_backgrounded = true;
        job.blocking.clear();
        if job.recovery.is_some() && job.info.notification_id.is_none() {
            job.info.notification_id = Some(crate::session::message::create_id());
        }
        persist(app, job);
    }

    /// `Job.background`: moves a job to the background (a recoverable one even after it finished).
    pub fn background(&self, app: &App, id: &str) -> Option<Info> {
        let mut state = self.state.lock().unwrap();
        let job = state.find(id)?;
        if job.info.status != Status::Running && job.recovery.is_none() {
            return None;
        }
        if job.is_backgrounded {
            return Some(job.info.clone());
        }
        Self::mark_background(app, job);
        let info = job.info.clone();
        job.backgrounded.send_replace(Some(info.clone()));
        Some(info)
    }

    /// `Job.backgroundAll`: moves every running job the session is blocked on to the background.
    pub fn background_all(&self, app: &App, session: &str, ty: Option<&str>) -> Vec<Info> {
        let mut state = self.state.lock().unwrap();
        let mut moved = Vec::new();
        for job in state.jobs.iter_mut() {
            if job.info.status != Status::Running || job.is_backgrounded {
                continue;
            }
            if ty.is_some_and(|ty| job.info.ty != ty) || !job.blocking.iter().any(|(s, _)| s == session) {
                continue;
            }
            Self::mark_background(app, job);
            moved.push(job.info.clone());
        }
        for info in &moved {
            if let Some(job) = state.find(&info.id) {
                job.backgrounded.send_replace(Some(info.clone()));
            }
        }
        moved
    }

    /// `cancelInternal`.
    fn cancel_internal(&self, app: &App, id: &str, save: bool) -> Option<Info> {
        let mut state = self.state.lock().unwrap();
        let job = state.find(id)?;
        let generation = job.generation;
        if job.info.status != Status::Running {
            let info = job.info.clone();
            state.consume(id, generation);
            return Some(info);
        }
        job.blocking.clear();
        job.info.status = Status::Cancelled;
        job.info.completed_at = Some(crate::id::now());
        if save {
            persist(app, job);
        }
        let info = job.info.clone();
        job.done.send_replace(Some(info.clone()));
        if let Some(task) = job.task.take() {
            task.abort();
        }
        state.consume(id, generation);
        Some(info)
    }

    /// `Job.cancel`.
    pub fn cancel(&self, app: &App, id: &str) -> Option<Info> {
        self.cancel_internal(app, id, true)
    }

    /// `Job.cancelSession`: cancels and forgets the session's jobs, its rows included.
    pub fn cancel_session(&self, app: &App, session: &str) -> Vec<Info> {
        let ids: Vec<String> = {
            let state = self.state.lock().unwrap();
            state
                .jobs
                .iter()
                .filter(|job| job.info.status == Status::Running && job.matches_session(session))
                .map(|job| job.info.id.clone())
                .collect()
        };
        let cancelled = ids.iter().filter_map(|id| self.cancel_internal(app, id, false)).collect();
        if let Err(error) = app
            .db
            .lock()
            .unwrap()
            .execute("DELETE FROM job WHERE session_id = ?1 OR child_session_id = ?1", params![session])
        {
            eprintln!("failed to delete the jobs of {session}: {error}");
        }
        self.state.lock().unwrap().jobs.retain(|job| !job.matches_session(session));
        cancelled
    }

    /// `Job.runningBackground`: the recoveries of running backgrounded jobs.
    #[allow(dead_code)] // The idle hook's (`SessionBackground.outstanding`).
    pub fn running_background(&self) -> Vec<Recovery> {
        let state = self.state.lock().unwrap();
        state
            .jobs
            .iter()
            .filter(|job| job.is_backgrounded && job.info.status == Status::Running)
            .filter_map(|job| job.recovery.clone())
            .collect()
    }

    /// `Job.backgroundJobs` (`JobList.read`): the stored rows (overlaid with the live state of their generation),
    /// then live backgrounded jobs without a row.
    pub fn background_jobs(&self, app: &App) -> Vec<(String, Status, Json)> {
        type Row = (String, String, Option<String>, String, Option<String>, i64, i64);
        let rows: Vec<Row> = {
            let db = app.db.lock().unwrap();
            let statement = db.prepare(
                "SELECT notification_id, id, title, status, recovery, time_created, time_updated FROM job \
                 ORDER BY time_created ASC, notification_id ASC",
            );
            match statement {
                Ok(mut statement) => statement
                    .query_map([], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
                    })
                    .map(|rows| rows.filter_map(Result::ok).collect())
                    .unwrap_or_default(),
                Err(_) => vec![],
            }
        };
        let state = self.state.lock().unwrap();
        let mut seen: Vec<String> = Vec::new();
        let mut items = Vec::new();
        for (notification, id, title, status, recovery, created, updated) in rows {
            let Some(recovery) = recovery.as_deref().and_then(json::parse).as_ref().and_then(Recovery::from_json) else {
                continue;
            };
            seen.push(notification.clone());
            let live = state.jobs.iter().find(|job| job.info.id == id);
            let same = live.filter(|job| job.info.notification_id.as_deref() == Some(notification.as_str()));
            let stored = Status::parse(&status);
            let status = match (same, stored) {
                (Some(job), _) => job.info.status,
                (None, Some(status)) => status,
                (None, None) => continue,
            };
            let completed = match same {
                Some(job) => job.info.completed_at,
                None => (status != Status::Running).then_some(updated),
            };
            let title = match same.and_then(|job| job.info.title.clone()) {
                Some(title) => Some(title),
                None => title,
            };
            items.push((recovery.owner().to_owned(), status, item(&id, status, title.as_deref(), created, completed, &recovery)));
        }
        for job in &state.jobs {
            let Some(recovery) = job.recovery.as_ref().filter(|_| job.is_backgrounded) else { continue };
            if job.info.notification_id.as_ref().is_some_and(|n| seen.contains(n)) {
                continue;
            }
            let info = &job.info;
            items.push((
                recovery.owner().to_owned(),
                info.status,
                item(&info.id, info.status, info.title.as_deref(), info.started_at, info.completed_at, recovery),
            ));
        }
        items
    }

    /// `Job.completeBackground`: the completion notice was admitted; the row goes, and so does a finished job.
    pub fn complete_background(&self, app: &App, notification_id: &str) {
        let mut state = self.state.lock().unwrap();
        if let Err(error) = app.db.lock().unwrap().execute("DELETE FROM job WHERE notification_id = ?1", params![notification_id]) {
            eprintln!("failed to complete background job {notification_id}: {error}");
        }
        let Some(index) = state.jobs.iter().position(|job| job.info.notification_id.as_deref() == Some(notification_id)) else {
            return;
        };
        if state.jobs[index].info.status != Status::Running {
            state.jobs.remove(index);
        }
    }
}

/// A backgrounded job's durable row (`Job.Background`), what restart recovery works from.
#[derive(Debug, Clone)]
pub struct Background {
    pub id: String,
    pub notification_id: String,
    pub recovery: Recovery,
    pub status: Status,
    pub output: Option<String>,
    pub error: Option<String>,
}

impl Jobs {
    /// `Job.pendingBackground`: the stored rows that decode, oldest first.
    pub fn pending_background(&self, app: &App) -> Vec<Background> {
        type Row = (String, String, String, String, Option<String>, Option<String>);
        let db = app.db.lock().unwrap();
        let statement = db.prepare(
            "SELECT notification_id, id, recovery, status, output, error FROM job ORDER BY time_created ASC, notification_id ASC",
        );
        let rows: Vec<Row> = match statement {
            Ok(mut statement) => statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))
                .map(|rows| rows.filter_map(Result::ok).collect())
                .unwrap_or_default(),
            Err(_) => vec![],
        };
        rows.into_iter()
            .filter_map(|(notification_id, id, recovery, status, output, error)| {
                Some(Background {
                    id,
                    notification_id,
                    recovery: json::parse(&recovery).as_ref().and_then(Recovery::from_json)?,
                    status: Status::parse(&status)?,
                    output,
                    error,
                })
            })
            .collect()
    }
}

/// Removes a session's block on a job when its wait ends (`removeBlock`).
struct Unblock {
    jobs: Arc<Jobs>,
    id: String,
    session: String,
}

impl Drop for Unblock {
    fn drop(&mut self) {
        self.jobs.remove_block(&self.id, &self.session);
    }
}
