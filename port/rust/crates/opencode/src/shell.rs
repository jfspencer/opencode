//! A location's shell commands (packages/core/src/shell.ts): each `create` runs one command line with a shell, streams
//! its combined output to `<data>/shell/<project>/<id>.out`, and keeps the command observable by ID until it is removed
//! (or, once 25 newer commands have ended, evicted). Callers (the shell tool, a user's `!command`, the HTTP API) own
//! what the command means; this service only runs it.
//!
//! Lifecycle, as in TS:
//! - the process leads its own process group; stdout and stderr share one pipe, pumped to the output file, counting
//!   bytes and newlines as they stream;
//! - the exit and the timeout race to decide how it ended (`exited` with an exit code or a signal, or `timeout`); a
//!   timeout sends SIGTERM to the group, then SIGKILL after 3 s;
//! - once the process has exited, output still open after 1 s (descendants holding the pipe) is abandoned and drained;
//! - the terminal state is recorded only once the output file is complete, then `shell.exited` is announced; a command
//!   that ended with a non-zero code has its remaining process group terminated;
//! - removing a running command ends it as `killed` (announced) and terminates its process group.
//!
//! Events are ephemeral (`shell.created`, `shell.exited`, `shell.deleted`) and carry the location. Output files older
//! than 7 days are swept hourly (`Shell.cleanup`).

pub mod select;
pub mod spawn;

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use indexmap::IndexMap;
use opencode_protocol::Json;
use tokio::sync::watch;

use crate::app::App;
use crate::bus::{LocationRef, PublishOptions};
use crate::json::{self, Obj};

/// Exited commands kept observable (`EXITED_LIMIT`).
const EXITED_LIMIT: usize = 25;
/// Output files older than this are removed (`Shell.RETENTION`).
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// `Shell.DIRECTORY`, under the data directory.
pub const DIRECTORY: &str = "shell";
/// `forceKillAfter`: SIGKILL follows SIGTERM after this long.
const FORCE_KILL_AFTER: Duration = Duration::from_secs(3);
/// Output still open this long after the process exited is abandoned.
const OUTPUT_DEADLINE: Duration = Duration::from_secs(1);
/// What one read from the output pipe takes at most.
const CHUNK: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Exited,
    Timeout,
    Killed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Exited => "exited",
            Status::Timeout => "timeout",
            Status::Killed => "killed",
        }
    }

    fn result(self) -> opencode_shell::result::Status {
        use opencode_shell::result::Status as S;
        match self {
            Status::Running => S::Running,
            Status::Exited => S::Exited,
            Status::Timeout => S::Timeout,
            Status::Killed => S::Killed,
        }
    }
}

/// `Shell.Info`.
#[derive(Clone, Debug)]
pub struct Info {
    pub id: String,
    pub status: Status,
    pub command: String,
    pub cwd: String,
    pub shell: String,
    pub file: String,
    pub pid: Option<u32>,
    pub exit: Option<i64>,
    pub signal: Option<String>,
    /// The creator's tags (an object).
    pub metadata: Json,
    pub started: i64,
    pub completed: Option<i64>,
}

impl Info {
    /// The wire form, in the schema's key order.
    pub fn to_json(&self) -> Json {
        let mut time = Obj::new();
        time.int("started", self.started);
        if let Some(completed) = self.completed {
            time.int("completed", completed);
        }
        let mut o = Obj::new();
        o.str("id", &self.id).str("status", self.status.as_str()).str("command", &self.command);
        o.str("cwd", &self.cwd).str("shell", &self.shell).str("file", &self.file);
        if let Some(pid) = self.pid {
            o.int("pid", pid as i64);
        }
        if let Some(exit) = self.exit {
            o.int("exit", exit);
        }
        o.opt_str("signal", self.signal.as_deref());
        o.set("metadata", self.metadata.clone()).set("time", time.done());
        o.done()
    }

    /// What `ShellResult` reads of a terminal Info.
    pub fn result_info(&self) -> opencode_shell::result::Info {
        opencode_shell::result::Info { status: self.status.result(), exit: self.exit, signal: self.signal.clone() }
    }
}

/// `Shell.NotFoundError`.
#[derive(Debug, Clone)]
pub struct NotFound(pub String);

impl NotFound {
    pub fn message(&self) -> String {
        format!("Shell command not found: {}", self.0)
    }
}

/// `Shell.CreateInput` (plus the shell to run it with).
#[derive(Default)]
pub struct CreateInput {
    pub command: String,
    pub cwd: Option<String>,
    pub timeout: Option<u64>,
    pub shell: Option<String>,
    pub metadata: Option<Json>,
}

/// `ShellCreateBefore`: what a `before` step may adjust before the command starts.
pub struct Invocation {
    pub command: String,
    pub cwd: String,
    pub timeout: u64,
    pub shell: String,
    /// The command's whole environment, in JS property order.
    pub env: IndexMap<String, String>,
}

impl Invocation {
    pub fn set_env(&mut self, key: &str, value: &str) {
        self.env.insert(key.to_owned(), value.to_owned());
    }
}

/// `AppProcess.AppProcessError` of a command that could not start.
#[derive(Debug, Clone)]
pub struct StartError {
    pub command: String,
    pub cause: spawn::Error,
}

impl StartError {
    pub fn message(&self) -> String {
        format!("Command failed: {}: {}", self.command, self.cause.message)
    }
}

pub enum CreateError<E> {
    Before(E),
    Start(StartError),
}

/// `Shell.Output`: one page of a command's output.
pub use opencode_shell::result::Page;

/// `ShellResult.Result`: a command's terminal Info and its bounded output tail (absent once the command is gone).
pub struct Outcome {
    pub info: Info,
    pub capture: Option<opencode_shell::result::Capture>,
}

/// How the process ended, as the first of the exit and the timeout recorded it.
#[derive(Clone)]
struct Ended {
    status: Status,
    exit: Option<i64>,
    signal: Option<String>,
    completed: i64,
}

struct Command {
    info: Mutex<Info>,
    pid: u32,
    size: AtomicU64,
    newlines: AtomicU64,
    /// Resolved with the terminal Info, or `Err` once the command is removed before it ended.
    done: watch::Sender<Option<Result<Info, ()>>>,
    ended: Mutex<Option<Ended>>,
    /// The timeout timer.
    timer: Mutex<Option<tokio::task::AbortHandle>>,
    /// Whether the timeout can still be changed (cleared once the command's history is data only).
    retimable: AtomicBool,
    /// The process has been reaped.
    exited: watch::Sender<bool>,
    /// The output pipe reached its end (every writer closed it).
    eof: watch::Sender<bool>,
    /// Capture stopped (1 s after the exit with the pipe still open).
    abandoned: watch::Sender<bool>,
    /// The output file holds everything captured.
    output_done: watch::Sender<bool>,
}

impl Command {
    /// Records how the command ended; the first outcome wins.
    fn end(&self, status: Status, exit: Option<i64>, signal: Option<String>) -> bool {
        let mut ended = self.ended.lock().unwrap();
        if ended.is_some() {
            return false;
        }
        *ended = Some(Ended { status, exit, signal, completed: crate::id::now() });
        true
    }

    fn stop_timer(&self) {
        if let Some(timer) = self.timer.lock().unwrap().take() {
            timer.abort();
        }
    }

    /// The process exited and every writer closed its output (the `close` event).
    async fn closed(&self) {
        let mut exited = self.exited.subscribe();
        let _ = exited.wait_for(|v| *v).await;
        let mut eof = self.eof.subscribe();
        let _ = eof.wait_for(|v| *v).await;
    }

    /// `handle.kill({ forceKillAfter: 3 s })`: SIGTERM to the process group, SIGKILL if it has not closed 3 s later.
    async fn stop(self: Arc<Self>) {
        spawn::kill(self.pid, libc::SIGTERM, *self.exited.borrow());
        if tokio::time::timeout(FORCE_KILL_AFTER, self.closed()).await.is_err() {
            spawn::kill(self.pid, libc::SIGKILL, *self.exited.borrow());
        }
    }
}

#[derive(Default)]
struct State {
    commands: IndexMap<String, Arc<Command>>,
    exit_order: VecDeque<String>,
}

/// A location's shell commands (`Shell.Service`).
pub struct Service {
    app: Weak<App>,
    location: LocationRef,
    output_dir: String,
    state: Mutex<State>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service").field("output_dir", &self.output_dir).finish_non_exhaustive()
    }
}

/// `Global.Path.data`.
fn data_directory() -> String {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    format!("{base}/opencode")
}

/// `Shell.cleanup`: removes command output files (`sh_<12 hex>….out`) older than the retention.
fn cleanup() {
    let root = format!("{}/{DIRECTORY}", data_directory());
    let Ok(projects) = std::fs::read_dir(&root) else { return };
    let cutoff = std::time::SystemTime::now() - RETENTION;
    for project in projects.flatten() {
        if !project.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(project.path()) else { continue };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let matches = name.len() >= 15
                && name.starts_with("sh_")
                && name.as_bytes()[3..15].iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
                && name.ends_with(".out");
            if !matches || !file.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let old = file.metadata().and_then(|m| m.modified()).is_ok_and(|mtime| mtime < cutoff);
            if old {
                let _ = std::fs::remove_file(file.path());
            }
        }
    }
}

/// Starts the hourly output sweep once per process (the global `shell-output-cleanup` node).
fn start_cleanup() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        tokio::spawn(async {
            loop {
                let _ = tokio::task::spawn_blocking(cleanup).await;
                tokio::time::sleep(Duration::from_secs(60 * 60)).await;
            }
        });
    });
}

fn count_newlines(bytes: &[u8]) -> u64 {
    bytes.iter().filter(|b| **b == b'\n').count() as u64
}

/// Reads `length` bytes of `file` from `start` (fewer at its end, none on an error), off the runtime's thread.
async fn read_range(file: String, start: u64, length: u64) -> Vec<u8> {
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut handle) = std::fs::File::open(&file) else { return Vec::new() };
        if handle.seek(SeekFrom::Start(start)).is_err() {
            return Vec::new();
        }
        let mut bytes = Vec::with_capacity(length.min(16 * 1024 * 1024) as usize);
        match handle.take(length).read_to_end(&mut bytes) {
            Ok(_) => bytes,
            Err(_) => Vec::new(),
        }
    })
    .await
    .unwrap_or_default()
}

impl Service {
    pub fn new(app: &App, location: LocationRef, project_id: &str) -> Arc<Self> {
        let output_dir = format!("{}/{DIRECTORY}/{project_id}", data_directory());
        let _ = std::fs::create_dir_all(&output_dir);
        start_cleanup();
        Arc::new(Self { app: app.me.clone(), location, output_dir, state: Mutex::new(State::default()) })
    }

    fn emit(&self, ty: &'static str, data: Json) {
        if let Some(app) = self.app.upgrade() {
            app.bus.emit(ty, data, PublishOptions { id: None, location: Some(self.location.clone()) });
        }
    }

    fn require(&self, id: &str) -> Result<Arc<Command>, NotFound> {
        self.state.lock().unwrap().commands.get(id).cloned().ok_or_else(|| NotFound(id.to_owned()))
    }

    /// `Shell.create`: runs `before` on the invocation (the shell tool's preparation), then starts the command.
    pub async fn create<E, F, Fut>(
        self: &Arc<Self>,
        input: CreateInput,
        base_env: Option<IndexMap<String, String>>,
        directory: &str,
        default_shell: impl FnOnce() -> String,
        before: F,
    ) -> Result<Info, CreateError<E>>
    where
        F: FnOnce(Invocation) -> Fut,
        Fut: Future<Output = Result<Invocation, E>>,
    {
        let mut env = base_env.unwrap_or_default();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("OPENCODE_TERMINAL".into(), "1".into());
        let invocation = Invocation {
            command: input.command,
            cwd: input.cwd.unwrap_or_else(|| directory.to_owned()),
            timeout: input.timeout.unwrap_or(0),
            shell: input.shell.unwrap_or_else(default_shell),
            env,
        };
        let invocation = before(invocation).await.map_err(CreateError::Before)?;
        let id = format!("sh_{}", crate::id::ascending());
        let args = opencode_shell::select::args(&invocation.shell, &invocation.command);
        let file = format!("{}/{id}.out", self.output_dir);
        let metadata = input.metadata.filter(|m| matches!(m, Json::Object(_))).unwrap_or_else(json::object);
        let env: Vec<(String, String)> = invocation.env.into_iter().collect();
        let spawned = spawn::spawn(&invocation.shell, &args, &invocation.cwd, &env)
            .map_err(|cause| CreateError::Start(StartError { command: invocation.command.clone(), cause }))?;
        let created = Info {
            id: id.clone(),
            status: Status::Running,
            command: invocation.command,
            cwd: invocation.cwd,
            shell: invocation.shell,
            file: file.clone(),
            pid: None,
            exit: None,
            signal: None,
            metadata,
            started: crate::id::now(),
            completed: None,
        };
        let info = Info { pid: Some(spawned.pid), ..created.clone() };
        let command = Arc::new(Command {
            info: Mutex::new(info.clone()),
            pid: spawned.pid,
            size: AtomicU64::new(0),
            newlines: AtomicU64::new(0),
            done: watch::Sender::new(None),
            ended: Mutex::new(None),
            timer: Mutex::new(None),
            retimable: AtomicBool::new(true),
            exited: watch::Sender::new(false),
            eof: watch::Sender::new(false),
            abandoned: watch::Sender::new(false),
            output_done: watch::Sender::new(false),
        });
        self.state.lock().unwrap().commands.insert(id.clone(), command.clone());
        // The output file is opened before the command is announced (`createWriteStream` and its "open").
        let path = file.clone();
        let output = tokio::task::spawn_blocking(move || std::fs::File::create(path).ok().map(Arc::new)).await.ok().flatten();
        tokio::spawn(pump(command.clone(), spawned.stdout, spawned.stderr, output));
        self.set_timeout(&command, invocation.timeout);
        tokio::spawn(lifecycle(self.clone(), command, spawned.child));
        let mut data = Obj::new();
        data.set("info", created.to_json());
        self.emit("shell.created", data.done());
        Ok(info)
    }

    fn set_timeout(&self, command: &Arc<Command>, duration: u64) {
        if command.ended.lock().unwrap().is_some() {
            return;
        }
        command.stop_timer();
        if duration == 0 {
            return;
        }
        let timed = command.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(duration)).await;
            // A recorded timeout always kills, even if this timer is replaced meanwhile.
            if timed.end(Status::Timeout, None, None) {
                tokio::spawn(timed.stop());
            }
        });
        *command.timer.lock().unwrap() = Some(timer.abort_handle());
    }

    /// `Shell.list`: the running commands, oldest first.
    pub fn list(&self) -> Vec<Info> {
        let state = self.state.lock().unwrap();
        state
            .commands
            .values()
            .map(|c| c.info.lock().unwrap().clone())
            .filter(|info| info.status == Status::Running)
            .collect()
    }

    /// `Shell.get`.
    pub fn get(&self, id: &str) -> Result<Info, NotFound> {
        Ok(self.require(id)?.info.lock().unwrap().clone())
    }

    /// `Shell.wait`: the terminal Info once the command has ended; `NotFound` if it is (or gets) removed first.
    pub async fn wait(&self, id: &str) -> Result<Info, NotFound> {
        let command = self.require(id)?;
        let mut done = command.done.subscribe();
        let outcome = match done.wait_for(|v| v.is_some()).await {
            Ok(value) => value.clone(),
            Err(_) => None,
        };
        match outcome {
            Some(Ok(info)) => Ok(info),
            _ => Err(NotFound(id.to_owned())),
        }
    }

    /// `Shell.timeout`: replaces a running command's timeout from now (zero clears it).
    pub fn timeout(&self, id: &str, duration: u64) -> Result<Info, NotFound> {
        let command = self.require(id)?;
        let info = command.info.lock().unwrap().clone();
        if info.status != Status::Running || !command.retimable.load(Ordering::SeqCst) {
            return Ok(info);
        }
        self.set_timeout(&command, duration);
        Ok(info)
    }

    /// `Shell.output`: one page of the output file by byte cursor.
    pub async fn output(&self, id: &str, cursor: Option<u64>, limit: Option<u64>) -> Result<Page, NotFound> {
        let command = self.require(id)?;
        let size = command.size.load(Ordering::SeqCst);
        let file = command.info.lock().unwrap().file.clone();
        Ok(page(&file, size, cursor, limit).await)
    }

    /// `Shell.result`: the command's terminal state and the bounded tail of its output, under the `tool_output`
    /// limits.
    pub async fn result(&self, started: &Info, max_lines: u64, max_bytes: u64) -> Outcome {
        let info = match self.wait(&started.id).await {
            Ok(info) => info,
            Err(_) => Info { status: Status::Killed, completed: Some(crate::id::now()), ..started.clone() },
        };
        let capture = async {
            let command = self.require(&info.id).ok()?;
            let size = command.size.load(Ordering::SeqCst);
            let cursor = size.saturating_sub(max_bytes);
            let tail = page(&info.file, size, Some(cursor), Some(max_bytes)).await;
            let newlines = self.require(&info.id).ok()?.newlines.load(Ordering::SeqCst);
            let bytes = tail.output.into_bytes();
            Some(opencode_shell::result::capture(&bytes, size, newlines, max_lines, max_bytes, &info.file))
        }
        .await;
        Outcome { info, capture }
    }

    /// `Shell.remove`.
    pub async fn remove(&self, id: &str) -> Result<(), NotFound> {
        self.require(id)?;
        self.remove_command(id).await;
        Ok(())
    }

    async fn remove_command(&self, id: &str) {
        let command = {
            let mut state = self.state.lock().unwrap();
            state.exit_order.retain(|other| other != id);
            state.commands.shift_remove(id)
        };
        let Some(command) = command else { return };
        command.stop_timer();
        // Unblock waiters of a command removed before it ended.
        command.done.send_if_modified(|done| {
            if done.is_some() {
                return false;
            }
            *done = Some(Err(()));
            true
        });
        let file = command.info.lock().unwrap().file.clone();
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_file(file)).await;
        // Every command reports exactly one exit; removing a running one ends it as killed.
        if command.info.lock().unwrap().status == Status::Running {
            let mut data = Obj::new();
            data.str("id", id).str("status", "killed");
            self.emit("shell.exited", data.done());
        }
        let mut data = Obj::new();
        data.str("id", id);
        self.emit("shell.deleted", data.done());
        // The process outlives its record only until its group is stopped.
        if !*command.exited.borrow() {
            tokio::spawn(command.stop());
        }
    }

    /// The location shuts down: pending waits end and running commands are stopped, without announcements.
    pub fn close(&self) {
        let commands: Vec<Arc<Command>> = {
            let mut state = self.state.lock().unwrap();
            state.exit_order.clear();
            state.commands.drain(..).map(|(_, c)| c).collect()
        };
        for command in commands {
            command.stop_timer();
            command.done.send_if_modified(|done| {
                if done.is_some() {
                    return false;
                }
                *done = Some(Err(()));
                true
            });
            if !*command.exited.borrow() {
                tokio::spawn(command.stop());
            }
        }
    }

    /// The terminal state, once the output is complete (the exit fiber's tail in TS).
    async fn finish(&self, command: &Arc<Command>) {
        let id = command.info.lock().unwrap().id.clone();
        let registered = self.state.lock().unwrap().commands.get(&id).is_some_and(|c| Arc::ptr_eq(c, command));
        // A removed command already failed its waiters and announced its exit and deletion.
        if !registered {
            return;
        }
        let Some(ended) = command.ended.lock().unwrap().clone() else { return };
        let info = {
            let mut info = command.info.lock().unwrap();
            info.status = ended.status;
            if ended.exit.is_some() {
                info.exit = ended.exit;
            }
            if ended.signal.is_some() {
                info.signal = ended.signal.clone();
            }
            info.completed = Some(ended.completed);
            info.clone()
        };
        // Waiters get the terminal Info before retention evicts anything.
        command.done.send_replace(Some(Ok(info.clone())));
        let mut data = Obj::new();
        data.str("id", &id);
        if let Some(exit) = ended.exit {
            data.int("exit", exit);
        }
        data.str("status", ended.status.as_str());
        self.emit("shell.exited", data.done());
        let evicted: Vec<String> = {
            let mut state = self.state.lock().unwrap();
            state.exit_order.push_back(id);
            let excess = state.exit_order.len().saturating_sub(EXITED_LIMIT);
            state.exit_order.iter().take(excess).cloned().collect()
        };
        for oldest in evicted {
            self.remove_command(&oldest).await;
        }
        // Exited history is data only.
        command.retimable.store(false, Ordering::SeqCst);
        command.stop_timer();
    }
}

/// One page of an output file of `size` bytes (`Shell.output`).
async fn page(file: &str, size: u64, cursor: Option<u64>, limit: Option<u64>) -> Page {
    let start = cursor.unwrap_or(0);
    let length = limit.unwrap_or(opencode_shell::result::DEFAULT_PAGE_LIMIT);
    let bytes = if start >= size || length == 0 {
        Vec::new()
    } else {
        read_range(file.to_owned(), start, length.min(size - start)).await
    };
    opencode_shell::result::page(size, cursor, limit, move |_, _| bytes)
}

/// Streams the command's output into its file until both pipes end or capture is abandoned (stdout first when both
/// have output ready, as `Stream.merge` takes them); abandoned output is still drained (and discarded), so
/// descendants writing to it are not blocked.
async fn pump(
    command: Arc<Command>,
    mut stdout: tokio::net::unix::pipe::Receiver,
    mut stderr: tokio::net::unix::pipe::Receiver,
    mut file: Option<Arc<std::fs::File>>,
) {
    use tokio::io::AsyncReadExt;
    let mut out_buffer = vec![0u8; CHUNK];
    let mut err_buffer = vec![0u8; CHUNK];
    let mut abandoned = command.abandoned.subscribe();
    let (mut out_open, mut err_open) = (true, true);
    loop {
        if *abandoned.borrow_and_update() || !(out_open || err_open) {
            break;
        }
        let chunk = tokio::select! {
            biased;
            _ = abandoned.changed() => continue,
            read = stdout.read(&mut out_buffer), if out_open => match read {
                Ok(n) if n > 0 => out_buffer[..n].to_vec(),
                _ => {
                    out_open = false;
                    continue;
                }
            },
            read = stderr.read(&mut err_buffer), if err_open => match read {
                Ok(n) if n > 0 => err_buffer[..n].to_vec(),
                _ => {
                    err_open = false;
                    continue;
                }
            },
        };
        command.size.fetch_add(chunk.len() as u64, Ordering::SeqCst);
        command.newlines.fetch_add(count_newlines(&chunk), Ordering::SeqCst);
        if let Some(handle) = file.clone() {
            let written = tokio::task::spawn_blocking(move || {
                use std::io::Write;
                (&*handle).write_all(&chunk)
            })
            .await;
            if !matches!(written, Ok(Ok(()))) {
                file = None;
            }
        }
    }
    drop(file);
    command.output_done.send_replace(true);
    while out_open || err_open {
        tokio::select! {
            read = stdout.read(&mut out_buffer), if out_open => out_open = matches!(read, Ok(n) if n > 0),
            read = stderr.read(&mut err_buffer), if err_open => err_open = matches!(read, Ok(n) if n > 0),
        }
    }
    command.eof.send_replace(true);
}

/// Waits for the process, records how it ended once its output is complete, and stops what is left of its process
/// group after a failure.
async fn lifecycle(service: Arc<Service>, command: Arc<Command>, mut child: tokio::process::Child) {
    use std::os::unix::process::ExitStatusExt;
    let status = child.wait().await;
    command.exited.send_replace(true);
    // Output still open a second after the exit belongs to descendants: stop capturing it.
    let mut eof = command.eof.subscribe();
    if tokio::time::timeout(OUTPUT_DEADLINE, eof.wait_for(|v| *v)).await.is_err() {
        command.abandoned.send_replace(true);
    }
    let (exit, signal) = match &status {
        Ok(status) => match status.code() {
            Some(code) => (Some(code as i64), None),
            None => (None, status.signal().map(spawn::signal_name)),
        },
        Err(_) => (None, None),
    };
    command.end(Status::Exited, exit, signal);
    // The terminal status is reported only once the output file is complete.
    let mut output_done = command.output_done.subscribe();
    let _ = output_done.wait_for(|v| *v).await;
    service.finish(&command).await;
    // Releasing the process: after a failure, what is left of its group is stopped.
    if exit.is_some_and(|code| code != 0) {
        command.stop().await;
    }
}
