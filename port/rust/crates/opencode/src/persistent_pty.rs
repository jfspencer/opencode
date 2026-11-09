//! Persistent terminals (packages/core/src/persistent-pty/index.ts): a session's terminals, which live in the
//! `opencode-pty` daemon the server owns ([`daemon`]) rather than in the server, so they can outlive a server restart
//! through an explicit handoff (`OPENCODE_PTY_HANDOFF`, claimed at startup).
//!
//! Terminals are `pty_persistent_<n>` (the daemon's number) and belong to a session (the daemon's group); deleting
//! the session removes them, and a terminal that exits while a client is attached is removed. A controller's
//! activity (a resize, framed control or input, attaching as controller) makes a terminal its session's current one,
//! which `read` shows. Events are ephemeral and carry no location: `persistent-pty.added`, `persistent-pty.updated`,
//! `persistent-pty.removed`.

pub mod daemon;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use base64::Engine;
use opencode_protocol::Json;
use tokio::sync::mpsc;

use crate::app::App;
use crate::bus::PublishOptions;
use crate::json::{self, Obj};
use daemon::{DaemonError, Handoff, Transport};

/// Why a terminal operation failed.
#[derive(Debug, Clone)]
pub enum Error {
    /// `PersistentPty.UnavailableError`: the daemon could not be reached or answered unexpectedly.
    Unavailable(String),
    /// `PersistentPty.NotFoundError`.
    NotFound(String),
    /// `PersistentPty.InvalidCwdError` (its message).
    InvalidCwd(String),
    /// `PersistentPty.UnsupportedError` (the feature).
    Unsupported(String),
}

impl From<DaemonError> for Error {
    fn from(error: DaemonError) -> Self {
        Error::Unavailable(error.message)
    }
}

type Result<T> = std::result::Result<T, Error>;

/// `PersistentPty.Info`, with what the service reads of it.
#[derive(Debug, Clone)]
pub struct Info {
    pub json: Json,
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub cwd: String,
    pub foreground_process: Json,
    pub cols: u16,
    pub rows: u16,
}

fn number(value: &Json, key: &str) -> Json {
    Json::Number(json::get_num(value, key).unwrap_or(0.0))
}

fn integer_text(value: f64) -> String {
    json::text(&Json::Number(value))
}

/// `toInfo`: the daemon's terminal as the API's.
fn to_info(wire: &Json) -> Info {
    let id = format!("pty_persistent_{}", integer_text(json::get_num(wire, "id").unwrap_or(0.0)));
    let lifecycle = json::get(wire, "lifecycle").cloned().unwrap_or(Json::Null);
    let status = json::get_str(&lifecycle, "status").unwrap_or("");
    let command: Vec<String> = json::get(wire, "command")
        .and_then(json::array)
        .map(|items| items.iter().filter_map(json::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    let title = json::get_str(wire, "title").unwrap_or_default().to_owned();
    let cwd = json::get_str(wire, "cwd").unwrap_or_default().to_owned();
    let session_id = json::get_str(wire, "group_id").unwrap_or_default().to_owned();
    let foreground_process = json::get(wire, "foreground_process").cloned().unwrap_or(Json::Null);
    let (cols, rows) = (json::get_num(wire, "cols").unwrap_or(0.0), json::get_num(wire, "rows").unwrap_or(0.0));
    let mut o = Obj::new();
    o.str("id", &id).str("title", &title);
    o.str("command", command.first().map(String::as_str).unwrap_or(""));
    o.set("args", Json::Array(command.iter().skip(1).map(|a| Json::String(a.clone())).collect()));
    o.str("cwd", &cwd).str("status", if status == "running" { "running" } else { "exited" });
    o.set("pid", match json::get(wire, "pid") {
        Some(Json::Number(pid)) => Json::Number(*pid),
        _ => Json::Number(0.0),
    });
    if status == "exited"
        && let Some(Json::Number(code)) = json::get(&lifecycle, "exit_code")
    {
        o.set("exitCode", Json::Number(*code));
    }
    o.str("sessionID", &session_id);
    o.set("userTitle", match json::get(wire, "user_title") {
        Some(Json::String(title)) => Json::String(title.clone()),
        _ => Json::Null,
    });
    o.set("foregroundProcess", foreground_process.clone());
    let mut size = Obj::new();
    size.set("cols", Json::Number(cols)).set("rows", Json::Number(rows));
    o.set("size", size.done());
    let mut output = Obj::new();
    output.set("head", number(wire, "output_head")).set("tail", number(wire, "output_tail"));
    o.set("output", output.done());
    Info { json: o.done(), id, session_id, title, cwd, foreground_process, cols: cols as u16, rows: rows as u16 }
}

/// `fromID`: the daemon's number of a terminal (only IDs the daemon listed reach it).
fn from_id(id: &str) -> Json {
    let number = id
        .strip_prefix("pty_persistent_")
        .map(opencode_protocol::wire::number::js_string_to_number)
        .filter(|n| opencode_protocol::wire::number::is_safe_integer(*n) && *n >= 1.0)
        .unwrap_or(0.0);
    Json::Number(number)
}

fn unexpected(response: &Json) -> Error {
    Error::Unavailable(format!("unexpected opencode-pty response: {}", daemon::kind_of(response)))
}

/// `PersistentPty.create`'s input.
pub struct CreateInput {
    pub command: Option<String>,
    pub args: Json,
    pub cwd: Option<String>,
    pub title: String,
    pub env: Json,
    pub cols: Option<f64>,
    pub rows: Option<f64>,
}

/// What a socket gets when it attaches (`PersistentPty.Attachment`, before activation).
pub struct Attached {
    pub info: Info,
    pub role: String,
    pub generation: f64,
    /// `{requestedOffset, availableOffset, endOffset, truncated}`.
    pub replay: Json,
    pub end_offset: f64,
    pub data: Vec<u8>,
    stream: tokio::net::UnixStream,
}

/// A terminal's stream events (`StreamEvent`).
pub enum Event {
    Output(Vec<u8>),
    Resized { cols: f64, rows: f64, generation: f64, checkpoint: String },
    Exited { exit_code: Option<f64>, final_offset: f64 },
    ControllerChanged { attachment_id: Option<String>, generation: f64 },
    TitleChanged(String),
    ForegroundProcessChanged(Json),
}

/// An active attachment: dropping it does not detach; [`Activation::detach`] does.
pub struct Activation {
    detached: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Activation {
    pub fn detach(&self) {
        self.detached.store(true, Ordering::SeqCst);
        self.task.abort();
    }
}

pub struct Service {
    app: Weak<App>,
    daemon: Transport,
    removing: Mutex<HashSet<String>>,
    /// Each session's current terminal.
    current: Mutex<HashMap<String, String>>,
}

static SERVICE: OnceLock<Service> = OnceLock::new();

/// The server's persistent terminals ([`start`] made them).
pub fn service() -> &'static Service {
    SERVICE.get().expect("persistent terminals are started with the server")
}

/// Starts the service before the server serves: a handoff's daemon is claimed first, then deleted sessions'
/// terminals are removed as they go. `root` is `<state>/pty`.
pub async fn start(app: &Arc<App>, handoff: Option<String>, root: String) {
    let service = SERVICE.get_or_init(|| Service {
        app: Arc::downgrade(app),
        daemon: Transport::new(root),
        removing: Mutex::new(HashSet::new()),
        current: Mutex::new(HashMap::new()),
    });
    // An invalid descriptor is ignored: persistent terminals start fresh.
    if let Some(handoff) = handoff.as_deref().and_then(Handoff::parse) {
        service.daemon.inherit(&handoff).await;
    }
    let mut events = app.bus.subscribe();
    let (deleted, mut sessions) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) if event.ty == "session.deleted" => {
                    if let Some(id) = json::get_str(&event.data, "sessionID") {
                        let _ = deleted.send(id.to_owned());
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    tokio::spawn(async move {
        while let Some(session) = sessions.recv().await {
            service.remove_session(&session).await;
        }
    });
}

impl Service {
    fn emit(&self, ty: &'static str, data: Json) {
        if let Some(app) = self.app.upgrade() {
            app.bus.emit(ty, data, PublishOptions::default());
        }
    }

    fn select(&self, session_id: &str, id: &str) {
        self.current.lock().unwrap().insert(session_id.to_owned(), id.to_owned());
    }

    fn deselect(&self, session_id: &str, id: &str) {
        let mut current = self.current.lock().unwrap();
        if current.get(session_id).is_some_and(|c| c == id) {
            current.remove(session_id);
        }
    }

    pub async fn list(&'static self, session_id: Option<&str>) -> Result<Vec<Info>> {
        let mut request = Obj::new();
        request.str("op", "list");
        let Some(response) = self.daemon.request_if_running(request.done()).await? else { return Ok(vec![]) };
        if daemon::kind_of(&response) != "terminals" {
            return Err(unexpected(&response));
        }
        let terminals = json::get(&response, "terminals").and_then(json::array).unwrap_or(&[]);
        Ok(terminals.iter().map(to_info).filter(|t| session_id.is_none_or(|s| t.session_id == s)).collect())
    }

    pub async fn get(&'static self, id: &str) -> Result<Info> {
        self.list(None).await?.into_iter().find(|t| t.id == id).ok_or_else(|| Error::NotFound(id.to_owned()))
    }

    pub async fn create(&'static self, session_id: &str, input: CreateInput) -> Result<Info> {
        let cwd = crate::project::absolutize(input.cwd.as_deref().unwrap_or("/"));
        let target = cwd.clone();
        // opencode-pty's PTY library silently falls back to $HOME when the cwd is not a directory.
        let directory =
            tokio::task::spawn_blocking(move || std::fs::metadata(&target).is_ok_and(|m| m.is_dir())).await.unwrap_or(false);
        if !directory {
            return Err(Error::InvalidCwd(format!("cwd is not a directory: {cwd}")));
        }
        let program = input.command.unwrap_or_else(crate::shell::select::environment);
        let mut request = Obj::new();
        request.str("op", "create").str("program", &program).set("args", input.args).str("cwd", &cwd);
        request.str("title", &input.title).str("group_id", session_id).set("env", input.env);
        request.set("cols", Json::Number(input.cols.unwrap_or(80.0))).set("rows", Json::Number(input.rows.unwrap_or(24.0)));
        let response = self.daemon.request(request.done(), true).await?;
        if daemon::kind_of(&response) != "created" {
            return Err(unexpected(&response));
        }
        let terminal = to_info(json::get(&response, "terminal").unwrap_or(&Json::Null));
        let mut data = Obj::new();
        data.str("sessionID", session_id).set("terminal", terminal.json.clone());
        self.emit("persistent-pty.added", data.done());
        Ok(terminal)
    }

    /// A terminal command answered `ok`; `select` makes the terminal its session's current one.
    async fn command(&'static self, id: &str, op: &str, body: Vec<(&str, Json)>, select: bool) -> Result<()> {
        let terminal = self.get(id).await?;
        let mut request = Obj::new();
        request.str("op", op).set("id", from_id(id));
        for (key, value) in body {
            request.set(key, value);
        }
        let response = self.daemon.request(request.done(), false).await?;
        if daemon::kind_of(&response) != "ok" {
            return Err(unexpected(&response));
        }
        if select {
            self.select(&terminal.session_id, id);
        }
        Ok(())
    }

    pub async fn resize(&'static self, id: &str, cols: f64, rows: f64, attachment_id: Option<&str>) -> Result<()> {
        let attachment = attachment_id.map(json::string).unwrap_or(Json::Null);
        let body = vec![("attachment_id", attachment), ("cols", Json::Number(cols)), ("rows", Json::Number(rows))];
        self.command(id, "resize", body, true).await
    }

    pub async fn control(&'static self, id: &str, attachment_id: &str, cols: u16, rows: u16) -> Result<()> {
        let body = vec![
            ("attachment_id", json::string(attachment_id)),
            ("cols", Json::Number(cols as f64)),
            ("rows", Json::Number(rows as f64)),
        ];
        self.command(id, "control", body, true).await
    }

    pub async fn input(&'static self, id: &str, attachment_id: &str, cols: u16, rows: u16, data: &[u8]) -> Result<()> {
        let body = vec![
            ("attachment_id", json::string(attachment_id)),
            ("cols", Json::Number(cols as f64)),
            ("rows", Json::Number(rows as f64)),
            ("data_base64", json::string(&base64::engine::general_purpose::STANDARD.encode(data))),
        ];
        self.command(id, "input", body, true).await
    }

    /// `rename`: a user title (`None` clears it, so the program's shows).
    pub async fn rename(&'static self, id: &str, title: Option<&str>) -> Result<Info> {
        let terminal = self.get(id).await?;
        // Daemons without the feature drop the connection on unknown requests.
        if !self.daemon.supports("rename").await? {
            return Err(Error::Unsupported("rename".into()));
        }
        let mut request = Obj::new();
        request.str("op", "rename").set("id", from_id(id)).set("title", title.map(json::string).unwrap_or(Json::Null));
        let response = self.daemon.request(request.done(), false).await?;
        if daemon::kind_of(&response) != "ok" {
            return Err(unexpected(&response));
        }
        let renamed = self.get(id).await?;
        let mut data = Obj::new();
        data.str("sessionID", &terminal.session_id).set("terminal", renamed.json.clone());
        self.emit("persistent-pty.updated", data.done());
        Ok(renamed)
    }

    /// `PersistentPty.Snapshot` as the API encodes it (the checkpoint in base64).
    pub async fn snapshot(&'static self, id: &str) -> Result<Json> {
        self.get(id).await?;
        let mut request = Obj::new();
        request.str("op", "snapshot").set("id", from_id(id));
        let response = self.daemon.request(request.done(), false).await?;
        if daemon::kind_of(&response) != "snapshot" {
            return Err(unexpected(&response));
        }
        let mut cursor = Obj::new();
        cursor.set("x", number(&response, "cursor_x")).set("y", number(&response, "cursor_y"));
        let mut o = Obj::new();
        o.set("info", to_info(json::get(&response, "terminal").unwrap_or(&Json::Null)).json);
        o.str("text", json::get_str(&response, "text").unwrap_or_default());
        o.str("checkpoint", &daemon::canonical_base64(json::get_str(&response, "checkpoint_base64").unwrap_or_default()));
        o.set("cursor", cursor.done());
        Ok(o.done())
    }

    /// `read`: the session's current terminal's screen (`null` without one); `lines` rows from its bottom, or the
    /// live height.
    pub async fn read(&'static self, session_id: &str, lines: Option<f64>) -> Result<Json> {
        let Some(id) = self.current.lock().unwrap().get(session_id).cloned() else { return Ok(Json::Null) };
        let terminal = match self.get(&id).await {
            Ok(terminal) => Some(terminal),
            Err(Error::NotFound(_)) => None,
            Err(error) => return Err(error),
        };
        if terminal.is_none_or(|t| t.session_id != session_id) {
            self.deselect(session_id, &id);
            return Ok(Json::Null);
        }
        let mut request = Obj::new();
        request.str("op", "read_rows").set("id", from_id(&id));
        request.opt("rows", lines.map(Json::Number));
        let response = self.daemon.request(request.done(), false).await?;
        if daemon::kind_of(&response) != "rows" {
            return Err(unexpected(&response));
        }
        let info = to_info(json::get(&response, "terminal").unwrap_or(&Json::Null));
        let text = json::get(&response, "lines")
            .and_then(json::array)
            .map(|lines| lines.iter().filter_map(json::as_str).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        let mut cursor = Obj::new();
        cursor.set("x", number(&response, "cursor_x")).set("y", number(&response, "cursor_y"));
        let mut screen = Obj::new();
        screen.str("text", &text).set("cols", Json::Number(info.cols as f64)).set("rows", Json::Number(info.rows as f64));
        screen.set("cursor", cursor.done());
        let mut o = Obj::new();
        o.str("ptyID", &info.id).str("title", &info.title).str("cwd", &info.cwd);
        o.set("foregroundProcess", info.foreground_process.clone()).set("screen", screen.done());
        Ok(o.done())
    }

    pub async fn remove(&'static self, id: &str) -> Result<()> {
        let terminal = self.get(id).await?;
        let mut request = Obj::new();
        request.str("op", "terminate").set("id", from_id(id));
        let response = self.daemon.request(request.done(), false).await?;
        if daemon::kind_of(&response) != "ok" {
            return Err(unexpected(&response));
        }
        self.deselect(&terminal.session_id, id);
        let mut data = Obj::new();
        data.str("sessionID", &terminal.session_id).str("ptyID", id);
        self.emit("persistent-pty.removed", data.done());
        Ok(())
    }

    pub async fn shutdown(&'static self) -> Result<()> {
        let response = self.daemon.shutdown().await;
        self.current.lock().unwrap().clear();
        match response? {
            Some(response) if daemon::kind_of(&response) != "ok" => Err(unexpected(&response)),
            _ => Ok(()),
        }
    }

    pub async fn handoff(&'static self) -> Result<Option<Handoff>> {
        Ok(self.daemon.handoff().await?)
    }

    /// A terminal that exits while a client watches is removed (once).
    fn remove_visible_exit(&'static self, id: &str) {
        if !self.removing.lock().unwrap().insert(id.to_owned()) {
            return;
        }
        let id = id.to_owned();
        tokio::spawn(async move {
            let _ = self.remove(&id).await;
            self.removing.lock().unwrap().remove(&id);
        });
    }

    /// A deleted session's terminals go with it.
    async fn remove_session(&'static self, session_id: &str) {
        if let Ok(terminals) = self.list(Some(session_id)).await {
            for terminal in terminals {
                let _ = self.remove(&terminal.id).await;
            }
        }
        self.current.lock().unwrap().remove(session_id);
    }

    /// `attach`: subscribes to the terminal's stream from `cursor`; a controller selects it.
    pub async fn attach(&'static self, id: &str, cursor: f64, attachment_id: &str, role: &str, takeover: bool) -> Result<Attached> {
        self.get(id).await?;
        let mut request = Obj::new();
        request.str("op", "subscribe").set("id", from_id(id)).set("offset", Json::Number(cursor));
        request.str("attachment_id", attachment_id).str("role", role).set("takeover", Json::Bool(takeover));
        let subscription = self.daemon.subscribe(request.done()).await?;
        let attached = subscription.attached;
        let info = to_info(json::get(&attached, "terminal").unwrap_or(&Json::Null));
        let role = json::get_str(&attached, "role").unwrap_or("observer").to_owned();
        if role == "controller" {
            self.select(&info.session_id, id);
        }
        let end_offset = json::get_num(&attached, "end_offset").unwrap_or(0.0);
        let mut replay = Obj::new();
        replay.set("requestedOffset", number(&attached, "requested_offset"));
        replay.set("availableOffset", number(&attached, "available_offset"));
        replay.set("endOffset", Json::Number(end_offset));
        replay.set("truncated", Json::Bool(json::get_bool(&attached, "truncated").unwrap_or(false)));
        let data = base64::engine::general_purpose::STANDARD
            .decode(json::get_str(&attached, "replay_base64").unwrap_or_default())
            .unwrap_or_default();
        Ok(Attached {
            info,
            role,
            generation: json::get_num(&attached, "generation").unwrap_or(0.0),
            replay: replay.done(),
            end_offset,
            data,
            stream: subscription.stream,
        })
    }

    /// `activate`: the live stream, event by event, until the terminal exits or the stream ends (then `on_end`, unless
    /// detached first). An exit removes the terminal.
    pub fn activate(
        &'static self,
        id: String,
        attached: Attached,
        mut on_event: impl FnMut(Event) + Send + 'static,
        on_end: impl FnOnce() + Send + 'static,
    ) -> Activation {
        let detached = Arc::new(AtomicBool::new(false));
        let flag = detached.clone();
        let mut stream = attached.stream;
        let task = tokio::spawn(async move {
            while let Ok(Some(frame)) = daemon::read_frame(&mut stream).await {
                if frame.first() == Some(&0) {
                    let Some(data) = daemon::output(&frame) else { break };
                    on_event(Event::Output(data.to_vec()));
                    continue;
                }
                let Ok(event) = daemon::decode(&frame) else { break };
                let num = |key: &str| json::get_num(&event, key).unwrap_or(0.0);
                match daemon::kind_of(&event) {
                    "resized" => on_event(Event::Resized {
                        cols: num("cols"),
                        rows: num("rows"),
                        generation: num("generation"),
                        checkpoint: daemon::canonical_base64(json::get_str(&event, "checkpoint_base64").unwrap_or_default()),
                    }),
                    "controller_changed" => on_event(Event::ControllerChanged {
                        attachment_id: json::get_str(&event, "attachment_id").map(str::to_owned),
                        generation: num("generation"),
                    }),
                    "title_changed" => on_event(Event::TitleChanged(json::get_str(&event, "title").unwrap_or_default().to_owned())),
                    "foreground_process_changed" => {
                        on_event(Event::ForegroundProcessChanged(json::get(&event, "process").cloned().unwrap_or(Json::Null)))
                    }
                    "exited" => {
                        self.remove_visible_exit(&id);
                        let exit_code = match json::get(&event, "exit_code") {
                            Some(Json::Number(code)) => Some(*code),
                            _ => None,
                        };
                        on_event(Event::Exited { exit_code, final_offset: num("final_offset") });
                        break;
                    }
                    _ => {}
                }
            }
            if !flag.load(Ordering::SeqCst) {
                on_end();
            }
        });
        Activation { detached, task }
    }
}
