//! The `opencode-pty` daemon's client (packages/core/src/persistent-pty/daemon.ts), protocol 7: four-byte big-endian
//! length frames of JSON (`{token, request}`) over a Unix socket, the daemon's raw output frames tagged `0x00`.
//!
//! The server owns one daemon, in the runtime directory `<root>/<id>` (a fresh UUID per server), started detached on
//! first use as `opencode-pty daemon --runtime-dir <root> --name <id>` (`OPENCODE_PTY_BIN`, else `opencode-pty` on
//! `PATH`) and found through its registration (`service.json`). The server claims it over one owner connection that
//! stays open for the server's life: the daemon stops when that connection goes, unless a handoff was prepared on it,
//! whose ticket lets a replacement server claim the same daemon. Every other request is one connection, one answer;
//! a subscription keeps its connection for the stream.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use opencode_protocol::Json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

use crate::json::{self, Obj};

const PROTOCOL_VERSION: f64 = 7.0;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Connect,
    Response,
    Registration,
    Protocol,
    Spawn,
}

/// `PersistentPty.DaemonError`.
#[derive(Debug, Clone)]
pub struct DaemonError {
    pub kind: Kind,
    pub message: String,
}

fn error(kind: Kind, message: impl Into<String>) -> DaemonError {
    DaemonError { kind, message: message.into() }
}

/// `responseError`: a rejected token means the registration is stale.
fn response_error(message: &str) -> DaemonError {
    error(if message == "authentication failed" { Kind::Registration } else { Kind::Protocol }, message)
}

/// A daemon's `service.json`, checked against its `pong`.
#[derive(Debug)]
pub struct Registration {
    pub instance_id: String,
    pub pid: f64,
    pub socket: String,
    pub token: String,
    pub features: Vec<String>,
}

/// `PersistentPty.Handoff`.
#[derive(Debug, Clone)]
pub struct Handoff {
    pub directory: String,
    pub instance_id: String,
    pub ticket: String,
    pub expires_at: f64,
}

impl Handoff {
    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        o.str("directory", &self.directory).str("instanceID", &self.instance_id).str("ticket", &self.ticket);
        o.set("expiresAt", Json::Number(self.expires_at));
        o.done()
    }

    /// `Schema.decodeUnknown(fromJsonString(Handoff))`.
    pub fn parse(text: &str) -> Option<Self> {
        let value = json::parse(text)?;
        Some(Self {
            directory: json::get_str(&value, "directory")?.to_owned(),
            instance_id: json::get_str(&value, "instanceID")?.to_owned(),
            ticket: json::get_str(&value, "ticket")?.to_owned(),
            expires_at: json::get_num(&value, "expiresAt")?,
        })
    }
}

fn encode(token: &str, request: Json) -> Result<Vec<u8>, DaemonError> {
    let mut envelope = Obj::new();
    envelope.str("token", token).set("request", request);
    let payload = json::text(&envelope.done());
    if payload.len() > MAX_FRAME_BYTES {
        return Err(error(Kind::Protocol, "opencode-pty frame too large"));
    }
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload.as_bytes());
    Ok(out)
}

/// The next frame (`None` at a clean end).
pub async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut head = [0u8; 4];
    let mut filled = 0;
    while filled < 4 {
        match reader.read(&mut head[filled..]).await {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err("opencode-pty truncated frame".into()),
            Ok(n) => filled += n,
            Err(e) => return Err(e.to_string()),
        }
    }
    let length = u32::from_be_bytes(head) as usize;
    if length > MAX_FRAME_BYTES {
        return Err("opencode-pty frame too large".into());
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload).await.map_err(|_| "opencode-pty truncated frame".to_owned())?;
    Ok(Some(payload))
}

pub fn decode(payload: &[u8]) -> Result<Json, DaemonError> {
    let text = std::str::from_utf8(payload).map_err(|e| error(Kind::Protocol, e.to_string()))?;
    let value = json::parse(text).ok_or_else(|| error(Kind::Protocol, "invalid opencode-pty response"))?;
    if json::get_str(&value, "type").is_none() {
        return Err(error(Kind::Protocol, "invalid opencode-pty response"));
    }
    Ok(value)
}

pub fn kind_of(response: &Json) -> &str {
    json::get_str(response, "type").unwrap_or("")
}

/// `oneShot`: one connection, one request, one answer (5 s).
async fn one_shot(registration: &Registration, request: Json) -> Result<Json, DaemonError> {
    let payload = encode(&registration.token, request)?;
    let dispatched = std::sync::atomic::AtomicBool::new(false);
    let exchange = async {
        let mut socket = UnixStream::connect(&registration.socket).await.map_err(|e| error(Kind::Connect, e.to_string()))?;
        dispatched.store(true, Ordering::Relaxed);
        socket.write_all(&payload).await.map_err(|e| error(Kind::Response, e.to_string()))?;
        let frame = read_frame(&mut socket)
            .await
            .map_err(|e| error(Kind::Response, e))?
            .ok_or_else(|| error(Kind::Response, "opencode-pty closed without response"))?;
        let response = decode(&frame)?;
        if kind_of(&response) == "error" {
            return Err(response_error(json::get_str(&response, "message").unwrap_or_default()));
        }
        Ok(response)
    };
    match tokio::time::timeout(TIMEOUT, exchange).await {
        Ok(result) => result,
        Err(_) => Err(error(
            if dispatched.load(Ordering::Relaxed) { Kind::Response } else { Kind::Connect },
            "opencode-pty request timed out",
        )),
    }
}

/// The owner connection: requests on it are answered in order (`exchange`).
struct Owner {
    generation: u64,
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    frames: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // `socket.destroy()`: the daemon sees the connection end now.
        let fd = self.writer.get_mut().as_ref().as_raw_fd();
        // SAFETY: shutting down a socket this connection still holds open.
        unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
        self.reader.abort();
    }
}

impl Owner {
    async fn exchange(&self, token: &str, request: Json) -> Result<Json, String> {
        let payload = encode(token, request).map_err(|e| e.message)?;
        let mut frames = self.frames.lock().await;
        self.writer.lock().await.write_all(&payload).await.map_err(|e| e.to_string())?;
        let frame = frames.recv().await.ok_or_else(|| "PTY daemon closed its ownership connection".to_owned())?;
        let response = decode(&frame).map_err(|e| e.message)?;
        if kind_of(&response) == "error" {
            return Err(json::get_str(&response, "message").unwrap_or_default().to_owned());
        }
        Ok(response)
    }
}

/// An attachment's start (`DaemonAttachment` before activation): the `attached` answer and its stream.
pub struct Subscription {
    pub attached: Json,
    pub stream: UnixStream,
}

pub struct Transport {
    root: String,
    id: Mutex<String>,
    registration: Mutex<Option<Arc<Registration>>>,
    owner: Mutex<Option<Arc<Owner>>>,
    generations: AtomicU64,
    startup: tokio::sync::Mutex<()>,
}

impl Transport {
    pub fn new(root: String) -> Self {
        Self {
            root,
            id: Mutex::new(crate::id::uuid()),
            registration: Mutex::new(None),
            owner: Mutex::new(None),
            generations: AtomicU64::new(1),
            startup: tokio::sync::Mutex::new(()),
        }
    }

    fn directory(&self) -> String {
        format!("{}/{}", self.root, self.id.lock().unwrap())
    }

    fn cached(&self) -> Option<Arc<Registration>> {
        self.registration.lock().unwrap().clone()
    }

    /// `claim`: the owner connection, with a handoff's ticket when there is one (5 s).
    async fn claim(&'static self, current: Arc<Registration>, ticket: Option<&str>) -> Result<(), DaemonError> {
        let open = async {
            let socket = UnixStream::connect(&current.socket).await.map_err(|e| e.to_string())?;
            let (mut read, write) = socket.into_split();
            let generation = self.generations.fetch_add(1, Ordering::Relaxed);
            let (frames_tx, frames_rx) = mpsc::unbounded_channel();
            let reader = tokio::spawn(async move {
                while let Ok(Some(frame)) = read_frame(&mut read).await {
                    if frames_tx.send(frame).is_err() {
                        break;
                    }
                }
                // The connection closed: forget the daemon it owned (aborting this finishing reader is a no-op).
                let stale = {
                    let mut owner = self.owner.lock().unwrap();
                    if owner.as_ref().is_some_and(|o| o.generation == generation) {
                        *self.registration.lock().unwrap() = None;
                        owner.take()
                    } else {
                        None
                    }
                };
                drop(stale);
            });
            let owner = Owner {
                generation,
                writer: tokio::sync::Mutex::new(write),
                frames: tokio::sync::Mutex::new(frames_rx),
                reader,
            };
            let mut request = Obj::new();
            request.str("op", "own").str("instance_id", &current.instance_id);
            request.opt_str("ticket", ticket);
            let response = owner.exchange(&current.token, request.done()).await?;
            if kind_of(&response) != "owned" {
                return Err("PTY daemon does not support server ownership".to_owned());
            }
            Ok(owner)
        };
        let owner = match tokio::time::timeout(TIMEOUT, open).await {
            Ok(Ok(owner)) => owner,
            Ok(Err(message)) => return Err(error(Kind::Connect, message)),
            Err(_) => return Err(error(Kind::Connect, "PTY ownership claim timed out")),
        };
        *self.owner.lock().unwrap() = Some(Arc::new(owner));
        *self.registration.lock().unwrap() = Some(current);
        Ok(())
    }

    /// `discover`: the registration in `directory` (the current runtime directory by default), checked live.
    async fn discover(&self, directory: Option<String>) -> Result<Registration, DaemonError> {
        let file = format!("{}/service.json", directory.unwrap_or_else(|| self.directory()));
        let text = tokio::task::spawn_blocking(move || std::fs::read_to_string(&file))
            .await
            .map_err(|e| error(Kind::Connect, e.to_string()))?
            .map_err(|e| error(Kind::Connect, e.to_string()))?;
        let value = json::parse(&text).ok_or_else(|| error(Kind::Protocol, "invalid opencode-pty registration"))?;
        let field = |name: &str| json::get_str(&value, name).map(str::to_owned);
        let (Some(instance_id), Some(pid), Some(protocol), Some(socket), Some(token)) = (
            field("instance_id"),
            json::get_num(&value, "pid"),
            json::get_num(&value, "protocol"),
            field("socket"),
            field("token"),
        ) else {
            return Err(error(Kind::Protocol, "invalid opencode-pty registration"));
        };
        if protocol != PROTOCOL_VERSION {
            return Err(error(
                Kind::Protocol,
                format!("opencode-pty protocol mismatch: daemon={}, client=7", crate::json::text(&Json::Number(protocol))),
            ));
        }
        let mut registration = Registration { instance_id, pid, socket, token, features: vec![] };
        let mut ping = Obj::new();
        ping.str("op", "ping");
        let response = one_shot(&registration, ping.done()).await?;
        if kind_of(&response) != "pong"
            || json::get_str(&response, "instance_id") != Some(registration.instance_id.as_str())
            || json::get_num(&response, "pid") != Some(registration.pid)
            || json::get_num(&response, "protocol") != Some(PROTOCOL_VERSION)
        {
            return Err(error(Kind::Protocol, "opencode-pty registration mismatch"));
        }
        // Daemons older than the features list omit it; the live daemon's answer wins over the file.
        registration.features = json::get(&response, "features")
            .and_then(json::array)
            .map(|items| items.iter().filter_map(json::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        Ok(registration)
    }

    /// `spawnDaemon`: starts a daemon in the runtime directory and claims it once it has registered (5 s).
    async fn spawn_daemon(&'static self) -> Result<Arc<Registration>, DaemonError> {
        let executable = std::env::var("OPENCODE_PTY_BIN").ok().filter(|b| !b.is_empty()).unwrap_or_else(|| "opencode-pty".into());
        let id = self.id.lock().unwrap().clone();
        let mut command = tokio::process::Command::new(&executable);
        command
            .args(["daemon", "--runtime-dir", &self.root, "--name", &id])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // `detached: true`: the daemon leads a session of its own and outlives the server's process group.
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|e| error(Kind::Spawn, format!("spawn {executable} {}", errno_code(&e))))?;
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        let mut last = None;
        while tokio::time::Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
            match self.discover(None).await {
                Ok(found) => {
                    let found = Arc::new(found);
                    if let Err(failure) = self.claim(found.clone(), None).await {
                        terminate(&child);
                        return Err(failure);
                    }
                    return Ok(found);
                }
                Err(failure) => last = Some(failure),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        terminate(&child);
        Err(last.unwrap_or_else(|| error(Kind::Connect, "opencode-pty did not become ready")))
    }

    /// `start`: an unreachable daemon can still hold the directory's lock, so a failure retries once in a fresh one.
    async fn start(&'static self) -> Result<Arc<Registration>, DaemonError> {
        match self.spawn_daemon().await {
            Ok(found) => Ok(found),
            Err(_) => {
                self.abandon();
                self.spawn_daemon().await
            }
        }
    }

    /// `abandon`: forgets the daemon (dropping its owner connection) and moves to a fresh runtime directory.
    pub fn abandon(&self) {
        self.owner.lock().unwrap().take();
        *self.registration.lock().unwrap() = None;
        *self.id.lock().unwrap() = crate::id::uuid();
    }

    /// `connect`: the live registration, discovering (and with `start`, starting) the daemon when there is none.
    async fn connect(&'static self, start: bool) -> Result<Arc<Registration>, DaemonError> {
        if let Some(registration) = self.cached() {
            return Ok(registration);
        }
        let _permit = self.startup.lock().await;
        if let Some(registration) = self.cached() {
            return Ok(registration);
        }
        let found = match self.discover(None).await {
            Ok(found) => Arc::new(found),
            Err(failure) if start && failure.kind == Kind::Connect => self.start().await?,
            Err(failure) => return Err(failure),
        };
        if self.owner.lock().unwrap().is_none() {
            self.claim(found.clone(), None).await?;
        }
        *self.registration.lock().unwrap() = Some(found.clone());
        Ok(found)
    }

    /// `withRegistration` around `oneShot`: a registration the daemon rejects as stale is forgotten.
    async fn attempt(&'static self, request: &Json, start: bool) -> Result<Json, DaemonError> {
        let current = self.connect(start).await?;
        let result = one_shot(&current, request.clone()).await;
        if let Err(failure) = &result
            && failure.kind == Kind::Registration
        {
            self.forget(&current);
        }
        result
    }

    fn forget(&self, current: &Arc<Registration>) {
        let mut registration = self.registration.lock().unwrap();
        if registration.as_ref().is_some_and(|r| Arc::ptr_eq(r, current)) {
            *registration = None;
        }
    }

    /// `request`: one request, retried once on a stale registration; on a lost daemon, with `start`, a new one.
    pub async fn request(&'static self, request: Json, start: bool) -> Result<Json, DaemonError> {
        match self.attempt(&request, start).await {
            Ok(response) => Ok(response),
            Err(failure) if failure.kind == Kind::Registration => self.attempt(&request, start).await,
            Err(failure) if failure.kind != Kind::Connect => Err(failure),
            Err(failure) => {
                *self.registration.lock().unwrap() = None;
                if !start {
                    return Err(failure);
                }
                self.attempt(&request, true).await
            }
        }
    }

    /// `requestIfRunning`: `None` when no daemon is reachable.
    pub async fn request_if_running(&'static self, request: Json) -> Result<Option<Json>, DaemonError> {
        match self.request(request, false).await {
            Ok(response) => Ok(Some(response)),
            Err(failure) if failure.kind == Kind::Connect => Ok(None),
            Err(failure) => Err(failure),
        }
    }

    pub async fn supports(&'static self, feature: &str) -> Result<bool, DaemonError> {
        Ok(self.connect(false).await?.features.iter().any(|f| f == feature))
    }

    /// `shutdown`: stops the daemon (when one runs) and waits until it is gone (5 s).
    pub async fn shutdown(&'static self) -> Result<Option<Json>, DaemonError> {
        let mut request = Obj::new();
        request.str("op", "shutdown");
        let response = self.request_if_running(request.done()).await?;
        *self.registration.lock().unwrap() = None;
        let Some(response) = response else { return Ok(None) };
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if self.discover(None).await.is_err() {
                return Ok(Some(response));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(error(Kind::Connect, "opencode-pty did not stop"))
    }

    /// `handoff`: a ticket a replacement server presents to claim this daemon (`None` when none is owned).
    pub async fn handoff(&'static self) -> Result<Option<Handoff>, DaemonError> {
        let _permit = self.startup.lock().await;
        let owner = self.owner.lock().unwrap().clone();
        let (Some(owner), Some(registered)) = (owner, self.cached()) else { return Ok(None) };
        let mut request = Obj::new();
        request.str("op", "prepare_handoff");
        let response = match tokio::time::timeout(TIMEOUT, owner.exchange(&registered.token, request.done())).await {
            Ok(Ok(response)) => response,
            Ok(Err(message)) => return Err(error(Kind::Response, message)),
            Err(_) => return Err(error(Kind::Response, "PTY handoff preparation timed out")),
        };
        if kind_of(&response) != "handoff" {
            return Err(error(Kind::Protocol, "Expected PTY handoff ticket"));
        }
        Ok(Some(Handoff {
            directory: self.directory(),
            instance_id: registered.instance_id.clone(),
            ticket: json::get_str(&response, "ticket").unwrap_or_default().to_owned(),
            expires_at: json::get_num(&response, "expires_at").unwrap_or(0.0),
        }))
    }

    /// `subscribe`: a terminal's stream (retried once on a stale registration).
    pub async fn subscribe(&'static self, request: Json) -> Result<Subscription, DaemonError> {
        match self.subscribe_once(&request).await {
            Err(failure) if failure.kind == Kind::Registration => self.subscribe_once(&request).await,
            result => result,
        }
    }

    async fn subscribe_once(&'static self, request: &Json) -> Result<Subscription, DaemonError> {
        let current = self.connect(false).await?;
        let result = async {
            let mut stream = UnixStream::connect(&current.socket).await.map_err(|e| error(Kind::Connect, e.to_string()))?;
            let payload = encode(&current.token, request.clone())?;
            stream.write_all(&payload).await.map_err(|e| error(Kind::Connect, e.to_string()))?;
            let frame = read_frame(&mut stream)
                .await
                .map_err(|e| error(Kind::Connect, e))?
                .ok_or_else(|| error(Kind::Connect, "opencode-pty closed before attachment"))?;
            let response = decode(&frame).map_err(|e| error(Kind::Connect, e.message))?;
            match kind_of(&response) {
                "error" => Err(response_error(json::get_str(&response, "message").unwrap_or_default())),
                "attached" => Ok(Subscription { attached: response, stream }),
                other => Err(error(Kind::Connect, format!("unexpected opencode-pty response: {other}"))),
            }
        }
        .await;
        if let Err(failure) = &result
            && failure.kind == Kind::Registration
        {
            self.forget(&current);
        }
        result
    }

    /// A replacement server claims its predecessor's daemon before it serves (`inherited`); any failure abandons that
    /// daemon, so the restart itself always succeeds and terminals start fresh.
    pub async fn inherit(&'static self, inherited: &Handoff) {
        let mut found: Option<Arc<Registration>> = None;
        let result: Result<(), DaemonError> = async {
            if inherited.expires_at <= crate::id::now() as f64 {
                return Err(error(Kind::Registration, "PTY restart handoff expired"));
            }
            // Older releases put the daemon directory outside the root; its directory name is still the ID.
            let id = std::path::Path::new(&inherited.directory)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            *self.id.lock().unwrap() = id.clone();
            let registration = Arc::new(self.discover(Some(inherited.directory.clone())).await?);
            found = Some(registration.clone());
            if registration.instance_id != inherited.instance_id {
                return Err(error(Kind::Registration, "PTY restart daemon changed"));
            }
            let target = format!("{}/{id}", self.root);
            if crate::project::absolutize(&inherited.directory) != crate::project::absolutize(&target) {
                migrate(&inherited.directory, &target).await?;
            }
            self.claim(registration, Some(&inherited.ticket)).await
        }
        .await;
        if result.is_err() {
            if let Some(found) = found {
                let mut request = Obj::new();
                request.str("op", "shutdown");
                let _ = one_shot(&found, request.done()).await;
            }
            self.abandon();
        }
    }
}

/// `migrate`: older releases kept registrations in purgeable temp directories; renaming keeps the lock file's inode,
/// so the running daemon keeps its lock in the new place.
async fn migrate(directory: &str, target: &str) -> Result<(), DaemonError> {
    let (directory, target) = (directory.to_owned(), target.to_owned());
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&target)?;
        std::fs::rename(format!("{directory}/service.lock"), format!("{target}/service.lock"))?;
        std::fs::rename(format!("{directory}/service.json"), format!("{target}/service.json"))
    })
    .await
    .map_err(|e| error(Kind::Registration, e.to_string()))?
    .map_err(|e| error(Kind::Registration, e.to_string()))
}

fn terminate(child: &tokio::process::Child) {
    if let Some(pid) = child.id() {
        // SAFETY: the child has not been reaped (`id` is set).
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
}

/// Node's spawn error code (`spawn opencode-pty ENOENT`).
fn errno_code(error: &std::io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::ENOENT) => "ENOENT",
        Some(libc::EACCES) => "EACCES",
        Some(libc::ENOTDIR) => "ENOTDIR",
        _ => "UNKNOWN",
    }
}

/// An output frame (`0x00`, start and end offsets as big-endian u64, then the bytes): its bytes, or `None` for a
/// frame too short to be one.
pub fn output(frame: &[u8]) -> Option<&[u8]> {
    (frame.len() >= 17).then(|| &frame[17..])
}

/// A base64 field re-encoded canonically (`Buffer.from(x, "base64").toString("base64")`).
pub fn canonical_base64(text: &str) -> String {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let bytes = LENIENT.decode(text.trim()).unwrap_or_default();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
