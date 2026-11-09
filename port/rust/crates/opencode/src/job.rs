//! Jobs (packages/core/src/job.ts, job/list.ts, job/recovery.ts, session/background.ts): work a tool call starts that
//! a session may wait for (`block`) or hand to the background (`background`, `backgroundAll`), and whose completion is
//! announced to the session later. One process-wide registry; a backgrounded job with a recovery record is kept in the
//! `job` table until its completion notice is admitted (`completeBackground`), so a restarted server can tell the
//! session what became of it.
//!
//! Jobs are kept in insertion order (a JS `Map`): a job is moved to the end when it is first consumed (its outcome
//! seen by a waiter), and only the 25 most recently consumed jobs without a notification are kept.
//!
//! A job's work is a future run on its own task; cancelling the job aborts the task (TS closes the job's scope, which
//! interrupts it), so the work must clean up when dropped.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use indexmap::IndexMap;
use opencode_protocol::Json;
use rusqlite::params;
use tokio::sync::watch;

use crate::app::App;
use crate::json::{self, Obj};

/// Consumed jobs without a notification kept for reads.
const COMPLETED_LIMIT: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "running" => Status::Running,
            "completed" => Status::Completed,
            "error" => Status::Error,
            "cancelled" => Status::Cancelled,
            _ => return None,
        })
    }
}

/// `Job.Recovery`: what a backgrounded job is, so its owner can be told about it after a restart.
#[derive(Clone, Debug, PartialEq)]
pub enum Recovery {
    Shell { session_id: String, shell_id: String, command: String },
    Subagent { parent_session_id: String, child_session_id: String, agent: String, description: String },
    Image { session_id: String, path: String, model: String },
}

impl Recovery {
    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        match self {
            Recovery::Shell { session_id, shell_id, command } => {
                o.str("kind", "shell").str("sessionID", session_id).str("shellID", shell_id).str("command", command);
            }
            Recovery::Subagent { parent_session_id, child_session_id, agent, description } => {
                o.str("kind", "subagent").str("parentSessionID", parent_session_id);
                o.str("childSessionID", child_session_id).str("agent", agent).str("description", description);
            }
            Recovery::Image { session_id, path, model } => {
                o.str("kind", "image").str("sessionID", session_id).str("path", path).str("model", model);
            }
        }
        o.done()
    }

    /// `RecoverySchema` decoding (`None` for a row whose recovery no longer decodes).
    pub fn from_json(value: &Json) -> Option<Self> {
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

    /// `ownerSessionID`.
    pub fn owner(&self) -> &str {
        match self {
            Recovery::Shell { session_id, .. } | Recovery::Image { session_id, .. } => session_id,
            Recovery::Subagent { parent_session_id, .. } => parent_session_id,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Recovery::Shell { .. } => "shell",
            Recovery::Subagent { .. } => "subagent",
            Recovery::Image { .. } => "image",
        }
    }

    fn child(&self) -> Option<&str> {
        match self {
            Recovery::Subagent { child_session_id, .. } => Some(child_session_id),
            _ => None,
        }
    }
}

/// `Job.Info`.
#[derive(Clone, Debug)]
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

/// `Job.Background`: a row of the `job` table.
#[derive(Clone, Debug)]
pub struct Background {
    pub id: String,
    pub notification_id: String,
    pub recovery: Recovery,
    pub status: Status,
    pub output: Option<String>,
    pub error: Option<String>,
}

/// How a job's work ended.
pub type Exit = Result<String, String>;

pub type Work = Pin<Box<dyn Future<Output = Exit> + Send + 'static>>;

pub struct StartInput {
    pub id: String,
    pub ty: String,
    pub title: Option<String>,
    pub metadata: Option<Json>,
    pub recovery: Option<Recovery>,
    pub notification_id: Option<String>,
    pub run: Work,
}

/// `Job.BlockResult`.
#[derive(Clone, Debug)]
pub enum Blocked {
    Finished(Info),
    Backgrounded(Info),
}

struct Active {
    info: Info,
    /// The job's scope (a fresh one per start of the same ID).
    generation: u64,
    done: watch::Sender<Option<Info>>,
    backgrounded: watch::Sender<Option<Info>>,
    task: Option<tokio::task::AbortHandle>,
    blocking: HashMap<String, usize>,
    is_backgrounded: bool,
    consumed: bool,
    recovery: Option<Recovery>,
}

impl Active {
    fn matches_session(&self, session: &str) -> bool {
        if self.info.metadata.as_ref().and_then(|m| json::get_str(m, "sessionID")) == Some(session) {
            return true;
        }
        if self.blocking.contains_key(session) {
            return true;
        }
        match &self.recovery {
            None => false,
            Some(Recovery::Subagent { parent_session_id, child_session_id, .. }) => {
                parent_session_id == session || child_session_id == session
            }
            Some(Recovery::Shell { session_id, .. } | Recovery::Image { session_id, .. }) => session_id == session,
        }
    }
}

#[derive(Default)]
pub struct Jobs {
    state: Mutex<IndexMap<String, Active>>,
    generations: AtomicU64,
}

/// The process's job registry.
pub fn jobs() -> &'static Jobs {
    static JOBS: OnceLock<Jobs> = OnceLock::new();
    JOBS.get_or_init(Jobs::default)
}

/// `persistBackground`: a backgrounded job with a recovery record is kept until its notice is admitted.
fn persist(app: &App, job: &Active) {
    let (Some(recovery), Some(notification)) = (&job.recovery, &job.info.notification_id) else { return };
    let title = job.info.title.clone().unwrap_or_else(|| match recovery {
        Recovery::Shell { command, .. } => command.clone(),
        Recovery::Subagent { description, .. } => description.clone(),
        Recovery::Image { path, .. } => path.clone(),
    });
    let now = crate::id::now();
    let db = app.db.lock().unwrap();
    let result = db.execute(
        "INSERT INTO job (notification_id, id, session_id, child_session_id, kind, title, status, output, error, recovery, \
         time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
         ON CONFLICT (notification_id) DO UPDATE SET status = excluded.status, output = excluded.output, \
         error = excluded.error, recovery = excluded.recovery, time_updated = excluded.time_updated",
        params![
            notification,
            job.info.id,
            recovery.owner(),
            recovery.child(),
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
        eprintln!("job persistence failed: {error}");
    }
}

impl Jobs {
    /// `consume`: the first read of a finished job moves it to the end; old consumed jobs are forgotten.
    fn consume(&self, id: &str, generation: u64) {
        let mut jobs = self.state.lock().unwrap();
        let Some(job) = jobs.get(id) else { return };
        if job.generation != generation || job.info.status == Status::Running || job.consumed {
            return;
        }
        let mut job = jobs.shift_remove(id).expect("present");
        job.consumed = true;
        jobs.insert(id.to_owned(), job);
        let completed: Vec<String> =
            jobs.iter().filter(|(_, j)| j.consumed && j.info.notification_id.is_none()).map(|(id, _)| id.clone()).collect();
        let excess = completed.len().saturating_sub(COMPLETED_LIMIT);
        for id in completed.into_iter().take(excess) {
            jobs.shift_remove(&id);
        }
    }

    /// `settle`: records how the work of one generation ended.
    fn settle(&self, app: &App, id: &str, generation: u64, exit: Exit) -> Option<Info> {
        let mut jobs = self.state.lock().unwrap();
        let job = jobs.get_mut(id)?;
        if job.generation != generation {
            return None;
        }
        if job.info.status != Status::Running {
            return Some(job.info.clone());
        }
        job.blocking.clear();
        job.info.completed_at = Some(crate::id::now());
        match exit {
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
        job.done.send_replace(Some(info.clone()));
        job.task = None;
        Some(info)
    }

    /// `Job.get`.
    pub fn get(&self, id: &str) -> Option<Info> {
        self.state.lock().unwrap().get(id).map(|job| job.info.clone())
    }

    /// `Job.start`: registers the job and starts its work (a running job of the same ID is returned as is).
    pub fn start(&self, app: &Arc<App>, input: StartInput) -> Info {
        let generation = self.generations.fetch_add(1, Ordering::SeqCst) + 1;
        let info = {
            let mut jobs = self.state.lock().unwrap();
            if let Some(existing) = jobs.get(&input.id)
                && existing.info.status == Status::Running
            {
                return existing.info.clone();
            }
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
                done: watch::Sender::new(None),
                backgrounded: watch::Sender::new(None),
                task: None,
                blocking: HashMap::new(),
                is_backgrounded: false,
                consumed: false,
                recovery: input.recovery,
            };
            // `new Map(jobs).set(id, job)`: a restarted ID keeps its place.
            jobs.insert(input.id.clone(), job);
            info
        };
        let app = app.clone();
        let id = input.id.clone();
        let run = input.run;
        let task = tokio::spawn(async move {
            let exit = run.await;
            jobs().settle(&app, &id, generation, exit);
        });
        if let Some(job) = self.state.lock().unwrap().get_mut(&input.id)
            && job.generation == generation
            && job.info.status == Status::Running
        {
            job.task = Some(task.abort_handle());
        }
        info
    }

    /// `Job.wait`: the job's Info once it has finished (or the timeout passed; `true` then).
    pub async fn wait(&self, id: &str, timeout: Option<Duration>) -> (Option<Info>, bool) {
        let (snapshot, mut done, generation, recoverable) = {
            let jobs = self.state.lock().unwrap();
            let Some(job) = jobs.get(id) else { return (None, false) };
            (job.info.clone(), job.done.subscribe(), job.generation, job.recovery.is_some())
        };
        let result = if snapshot.status != Status::Running {
            (Some(snapshot), false)
        } else {
            let wait = async {
                match done.wait_for(|v| v.is_some()).await {
                    Ok(info) => info.clone(),
                    Err(_) => None,
                }
            };
            match timeout {
                None => (wait.await.or(Some(snapshot)), false),
                Some(limit) if limit.is_zero() => (Some(snapshot), true),
                Some(limit) => match tokio::time::timeout(limit, wait).await {
                    Ok(info) => (info.or(Some(snapshot)), false),
                    Err(_) => (Some(snapshot), true),
                },
            }
        };
        // A recoverable job can still be handed to the background after it finished.
        let running = result.0.as_ref().is_some_and(|info| info.status == Status::Running);
        if !running && !recoverable {
            self.consume(id, generation);
        }
        result
    }

    /// `Job.block`: waits for the job on behalf of a session until it finishes or is backgrounded.
    pub async fn block(&self, id: &str, session: &str) -> Option<Blocked> {
        enum Start {
            Finished(Info, u64),
            Backgrounded(Info),
            Wait(watch::Receiver<Option<Info>>, watch::Receiver<Option<Info>>, u64),
        }
        let start = {
            let mut jobs = self.state.lock().unwrap();
            let job = jobs.get_mut(id)?;
            if job.info.status != Status::Running {
                Start::Finished(job.info.clone(), job.generation)
            } else if job.is_backgrounded {
                Start::Backgrounded(job.info.clone())
            } else {
                *job.blocking.entry(session.to_owned()).or_insert(0) += 1;
                Start::Wait(job.done.subscribe(), job.backgrounded.subscribe(), job.generation)
            }
        };
        let (mut done, mut backgrounded, generation) = match start {
            Start::Finished(info, generation) => {
                self.consume(id, generation);
                return Some(Blocked::Finished(info));
            }
            Start::Backgrounded(info) => return Some(Blocked::Backgrounded(info)),
            Start::Wait(done, backgrounded, generation) => (done, backgrounded, generation),
        };
        // `Effect.ensuring(removeBlock)`, even when the caller stops waiting.
        struct Unblock<'a> {
            jobs: &'a Jobs,
            id: &'a str,
            session: &'a str,
        }
        impl Drop for Unblock<'_> {
            fn drop(&mut self) {
                let mut jobs = self.jobs.state.lock().unwrap();
                let Some(job) = jobs.get_mut(self.id) else { return };
                if job.info.status != Status::Running || job.is_backgrounded {
                    return;
                }
                if let Some(count) = job.blocking.get_mut(self.session) {
                    if *count <= 1 {
                        job.blocking.remove(self.session);
                    } else {
                        *count -= 1;
                    }
                }
            }
        }
        let _unblock = Unblock { jobs: self, id, session };
        let outcome = tokio::select! {
            info = done.wait_for(|v| v.is_some()) => info.ok().and_then(|i| i.clone()).map(Blocked::Finished),
            info = backgrounded.wait_for(|v| v.is_some()) => info.ok().and_then(|i| i.clone()).map(Blocked::Backgrounded),
        };
        if let Some(Blocked::Finished(_)) = &outcome {
            self.consume(id, generation);
        }
        outcome
    }

    /// `markBackground`: the job no longer blocks anyone; a recoverable one gets its notification ID and is persisted.
    fn mark_background(app: &App, job: &mut Active) {
        job.is_backgrounded = true;
        job.blocking.clear();
        if job.recovery.is_some() && job.info.notification_id.is_none() {
            job.info.notification_id = Some(crate::session::message::create_id());
        }
        persist(app, job);
    }

    /// `Job.background`.
    pub fn background(&self, app: &App, id: &str) -> Option<Info> {
        let mut jobs = self.state.lock().unwrap();
        let job = jobs.get_mut(id)?;
        // Recoverable work may finish before the caller backgrounds it.
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

    /// `Job.backgroundAll`: every running job a session is blocked on (of one type, when given).
    pub fn background_all(&self, app: &App, session: &str, ty: Option<&str>) -> Vec<Info> {
        let mut jobs = self.state.lock().unwrap();
        let mut moved = Vec::new();
        for job in jobs.values_mut() {
            if job.info.status != Status::Running || job.is_backgrounded {
                continue;
            }
            if ty.is_some_and(|ty| job.info.ty != ty) || !job.blocking.contains_key(session) {
                continue;
            }
            Self::mark_background(app, job);
            job.backgrounded.send_replace(Some(job.info.clone()));
            moved.push(job.info.clone());
        }
        moved
    }

    fn cancel_internal(&self, app: &App, id: &str, keep: bool) -> Option<Info> {
        let (info, generation, task) = {
            let mut jobs = self.state.lock().unwrap();
            let job = jobs.get_mut(id)?;
            if job.info.status != Status::Running {
                let result = (job.info.clone(), job.generation);
                drop(jobs);
                self.consume(id, result.1);
                return Some(result.0);
            }
            job.blocking.clear();
            job.info.status = Status::Cancelled;
            job.info.completed_at = Some(crate::id::now());
            if keep {
                persist(app, job);
            }
            job.done.send_replace(Some(job.info.clone()));
            (job.info.clone(), job.generation, job.task.take())
        };
        // Closing the job's scope interrupts its work.
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

    /// `Job.cancelSession`: a removed session's jobs are cancelled and forgotten.
    pub fn cancel_session(&self, app: &App, session: &str) -> Vec<Info> {
        let ids: Vec<String> = self
            .state
            .lock()
            .unwrap()
            .values()
            .filter(|job| job.info.status == Status::Running && job.matches_session(session))
            .map(|job| job.info.id.clone())
            .collect();
        let cancelled: Vec<Info> = ids.iter().filter_map(|id| self.cancel_internal(app, id, false)).collect();
        if let Err(error) = app
            .db
            .lock()
            .unwrap()
            .execute("DELETE FROM job WHERE session_id = ?1 OR child_session_id = ?1", params![session])
        {
            eprintln!("job removal failed: {error}");
        }
        self.state.lock().unwrap().retain(|_, job| !job.matches_session(session));
        cancelled
    }

    /// `Job.pendingBackground`: the persisted background jobs, oldest first.
    pub fn pending_background(&self, app: &App) -> Vec<Background> {
        rows(app)
            .into_iter()
            .filter_map(|row| {
                Some(Background {
                    id: row.id,
                    notification_id: row.notification_id,
                    recovery: Recovery::from_json(&json::parse(&row.recovery)?)?,
                    status: Status::parse(&row.status)?,
                    output: row.output,
                    error: row.error,
                })
            })
            .collect()
    }

    /// `Job.runningBackground`: what the running background jobs are.
    pub fn running_background(&self) -> Vec<Recovery> {
        self.state
            .lock()
            .unwrap()
            .values()
            .filter(|job| job.is_backgrounded && job.info.status == Status::Running)
            .filter_map(|job| job.recovery.clone())
            .collect()
    }

    /// `Job.completeBackground`: a job's notice was admitted; its record goes.
    pub fn complete_background(&self, app: &App, notification_id: &str) {
        let mut jobs = self.state.lock().unwrap();
        if let Err(error) =
            app.db.lock().unwrap().execute("DELETE FROM job WHERE notification_id = ?1", params![notification_id])
        {
            eprintln!("job removal failed: {error}");
        }
        let entry = jobs.iter().find(|(_, job)| job.info.notification_id.as_deref() == Some(notification_id));
        if let Some((id, job)) = entry
            && job.info.status != Status::Running
        {
            let id = id.clone();
            jobs.shift_remove(&id);
        }
    }

    /// `JobList.read` (`Job.backgroundJobs`): the persisted background jobs, then the live ones not persisted, as
    /// `Job.Info` items.
    pub fn background_jobs(&self, app: &App) -> Vec<Item> {
        let rows = rows(app);
        let jobs = self.state.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut items = Vec::new();
        for row in rows {
            let Some(recovery) = json::parse(&row.recovery).and_then(|r| Recovery::from_json(&r)) else { continue };
            seen.insert(row.notification_id.clone());
            let live = jobs.get(&row.id).filter(|live| live.info.notification_id.as_deref() == Some(&row.notification_id));
            let stored = Status::parse(&row.status).unwrap_or(Status::Running);
            let status = live.map_or(stored, |l| l.info.status);
            let completed = match live {
                Some(live) => live.info.completed_at,
                None => (stored != Status::Running).then_some(row.time_updated),
            };
            let title = live.and_then(|l| l.info.title.clone()).or(row.title);
            items.push(Item { id: row.id, status, title, started: row.time_created, completed, recovery });
        }
        for live in jobs.values() {
            let Some(recovery) = live.recovery.clone().filter(|_| live.is_backgrounded) else { continue };
            if live.info.notification_id.as_ref().is_some_and(|n| seen.contains(n)) {
                continue;
            }
            items.push(Item {
                id: live.info.id.clone(),
                status: live.info.status,
                title: live.info.title.clone(),
                started: live.info.started_at,
                completed: live.info.completed_at,
                recovery,
            });
        }
        items
    }
}

/// One background job as `job.list` reports it (`Job.Info` of packages/schema/src/job.ts).
#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub status: Status,
    pub title: Option<String>,
    pub started: i64,
    pub completed: Option<i64>,
    pub recovery: Recovery,
}

impl Item {
    pub fn kind(&self) -> &'static str {
        self.recovery.kind()
    }

    /// The session the job belongs to (the parent, for a subagent).
    pub fn session(&self) -> &str {
        self.recovery.owner()
    }

    pub fn to_json(&self) -> Json {
        let mut time = Obj::new();
        time.int("started", self.started);
        if let Some(completed) = self.completed {
            time.int("completed", completed);
        }
        let mut o = Obj::new();
        o.str("kind", self.kind()).str("id", &self.id).str("sessionID", self.session()).str("status", self.status.as_str());
        o.opt_str("title", self.title.as_deref());
        o.set("time", time.done());
        match &self.recovery {
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
}

struct Row {
    notification_id: String,
    id: String,
    title: Option<String>,
    status: String,
    output: Option<String>,
    error: Option<String>,
    recovery: String,
    time_created: i64,
    time_updated: i64,
}

fn rows(app: &App) -> Vec<Row> {
    let db = app.db.lock().unwrap();
    let query = || -> rusqlite::Result<Vec<Row>> {
        let mut statement = db.prepare(
            "SELECT notification_id, id, title, status, output, error, recovery, time_created, time_updated FROM job \
             ORDER BY time_created ASC, notification_id ASC",
        )?;
        statement
            .query_map([], |r| {
                Ok(Row {
                    notification_id: r.get(0)?,
                    id: r.get(1)?,
                    title: r.get(2)?,
                    status: r.get(3)?,
                    output: r.get(4)?,
                    error: r.get(5)?,
                    recovery: r.get(6)?,
                    time_created: r.get(7)?,
                    time_updated: r.get(8)?,
                })
            })?
            .collect()
    };
    query().unwrap_or_else(|error| {
        eprintln!("job read failed: {error}");
        Vec::new()
    })
}
