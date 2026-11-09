//! A location's terminals (packages/core/src/pty.ts, pty/protocol.ts): programs in pseudo-terminals, kept by ID until
//! they are removed (or, once 25 newer terminals have exited, evicted), each with its output retained for replay.
//!
//! - The default program is the configured shell, as a login shell (`-l` before the caller's arguments, D223), in the
//!   location's directory, with the server's environment plus the caller's, `TERM=xterm-256color` and
//!   `OPENCODE_TERMINAL=1`.
//! - Output is counted in UTF-16 units (JS string length): the cursor is the total so far, and the last 2 Mi units
//!   are retained.
//! - An attachment replays the retained output from a cursor (from the start; `-1` from the end), then a binary
//!   control frame (`0x00` + `{"cursor":N}`), then live output as text frames; the end of the program (or the
//!   terminal's removal) closes it normally.
//! - Events are ephemeral and carry the location: `pty.created`, `pty.updated`, `pty.exited`, `pty.deleted`.

pub mod terminal;
pub mod ticket;

use std::collections::VecDeque;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use bytes::Bytes;
use indexmap::IndexMap;
use opencode_protocol::Json;
use tokio::io::unix::AsyncFd;
use tokio::sync::{mpsc, oneshot};

use crate::app::App;
use crate::bus::{LocationRef, PublishOptions};
use crate::http::websocket::Outbound;
use crate::json::Obj;

/// `BUFFER_LIMIT`, in UTF-16 units.
const BUFFER_LIMIT: usize = 2 * 1024 * 1024;
/// How far the retained text may grow past the limit before it is cut back (the replay still starts at the limit).
const BUFFER_SLACK: usize = 256 * 1024;
/// `EXITED_LIMIT`: exited terminals kept observable.
const EXITED_LIMIT: usize = 25;
/// `PtyProtocol.REPLAY_CHUNK`: replay frames hold at most this many UTF-16 units.
const REPLAY_CHUNK: usize = 64 * 1024;

/// `Pty.Info`.
#[derive(Clone, Debug)]
pub struct Info {
    pub id: String,
    pub title: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub running: bool,
    pub pid: u32,
    pub exit_code: Option<i64>,
}

impl Info {
    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        o.str("id", &self.id).str("title", &self.title).str("command", &self.command);
        o.set("args", Json::Array(self.args.iter().map(|a| Json::String(a.clone())).collect()));
        o.str("cwd", &self.cwd).str("status", if self.running { "running" } else { "exited" });
        o.int("pid", self.pid as i64);
        if let Some(code) = self.exit_code {
            o.int("exitCode", code);
        }
        o.done()
    }
}

/// `Pty.NotFoundError` and `Pty.ExitedError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Exited,
}

/// `Pty.CreateInput`, with the location's defaults applied by the caller.
pub struct CreateInput {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub env: Vec<(String, String)>,
}

pub struct Service {
    app: Weak<App>,
    location: LocationRef,
    state: Mutex<State>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("pty::Service").field("location", &self.location).finish_non_exhaustive()
    }
}

#[derive(Default)]
struct State {
    sessions: IndexMap<String, Arc<Session>>,
    exit_order: VecDeque<String>,
}

struct Session {
    info: Mutex<Info>,
    output: Mutex<Output>,
    /// Input for the program, written in order by its I/O task.
    input: mpsc::UnboundedSender<Vec<u8>>,
    /// The master side, for resizes (dropped at teardown).
    master: Mutex<Option<Arc<OwnedFd>>>,
    /// Ends the I/O task (killing a program still running).
    stop: Mutex<Option<oneshot::Sender<()>>>,
}

#[derive(Default)]
struct Output {
    /// Retained output ending at `cursor`: at least the last `BUFFER_LIMIT` units, up to `BUFFER_SLACK` more.
    buffer: String,
    /// `buffer`'s length in UTF-16 units.
    units: usize,
    /// Total output so far, in UTF-16 units.
    cursor: u64,
    subscribers: Vec<(u64, mpsc::UnboundedSender<Outbound>)>,
    /// Torn down: the program's output and exit no longer matter.
    disposed: bool,
}

fn units(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The byte offset in `text` of UTF-16 offset `unit`, and whether it falls inside a character (half of a surrogate
/// pair, which JS would slice into a lone surrogate that encodes as U+FFFD).
fn byte_offset(text: &str, unit: usize) -> (usize, bool) {
    let mut count = 0;
    for (index, c) in text.char_indices() {
        if count >= unit {
            return (index, count > unit);
        }
        count += c.len_utf16();
        if count > unit {
            return (index + c.len_utf8(), true);
        }
    }
    (text.len(), count > unit)
}

impl Output {
    fn push(&mut self, chunk: &str) {
        self.buffer.push_str(chunk);
        self.units += units(chunk);
        if self.units <= BUFFER_LIMIT + BUFFER_SLACK {
            return;
        }
        // Cut at the last character boundary that keeps at least the limit.
        let excess = self.units - BUFFER_LIMIT;
        let (mut cut, mut dropped) = (0, 0);
        for (index, c) in self.buffer.char_indices() {
            if dropped + c.len_utf16() > excess {
                cut = index;
                break;
            }
            dropped += c.len_utf16();
        }
        self.units -= dropped;
        self.buffer.drain(..cut);
    }

    /// The retained output from absolute cursor `from` (TS `buffer.slice(max(0, from - (end - buffer.length)))`).
    fn replay(&self, from: u64) -> String {
        let logical = self.units.min(BUFFER_LIMIT);
        let start = from.saturating_sub(self.cursor - logical as u64);
        if start >= logical as u64 {
            return String::new();
        }
        let (offset, inside) = byte_offset(&self.buffer, self.units - logical + start as usize);
        let mut out = String::with_capacity(self.buffer.len() - offset + 3);
        if inside {
            out.push('\u{FFFD}');
        }
        out.push_str(&self.buffer[offset..]);
        out
    }

    /// The subscribers' sockets close.
    fn end(&mut self, code: u16, reason: &str) {
        for (_, sink) in self.subscribers.drain(..) {
            let _ = sink.send(Outbound::Close(code, reason.to_owned()));
        }
    }
}

/// `PtyProtocol.metaFrame`: `0x00` and the cursor as JSON.
fn meta_frame(cursor: u64) -> Bytes {
    let mut frame = vec![0u8];
    frame.extend_from_slice(format!("{{\"cursor\":{cursor}}}").as_bytes());
    Bytes::from(frame)
}

/// `PtyProtocol.chunks`, kept to whole characters.
fn chunks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut count = 0;
    for c in text.chars() {
        if count + c.len_utf16() > REPLAY_CHUNK {
            out.push(std::mem::take(&mut current));
            count = 0;
        }
        current.push(c);
        count += c.len_utf16();
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

static TOKENS: AtomicU64 = AtomicU64::new(1);

/// A socket's attachment to a terminal (`Pty.Attachment`, already active).
pub struct Attachment {
    session: Arc<Session>,
    token: u64,
}

impl Attachment {
    pub fn write(&self, data: &str) {
        self.session.write(data.as_bytes());
    }

    pub fn detach(&self) {
        self.session.output.lock().unwrap().subscribers.retain(|(token, _)| *token != self.token);
    }
}

impl Session {
    fn write(&self, data: &[u8]) {
        if self.info.lock().unwrap().running {
            let _ = self.input.send(data.to_vec());
        }
    }

    fn on_data(&self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let mut output = self.output.lock().unwrap();
        if output.disposed {
            return;
        }
        output.cursor += units(chunk) as u64;
        output.subscribers.retain(|(_, sink)| sink.send(Outbound::Text(chunk.to_owned())).is_ok());
        output.push(chunk);
    }
}

impl Service {
    pub fn new(app: &App, location: LocationRef) -> Arc<Self> {
        Arc::new(Self { app: app.me.clone(), location, state: Mutex::new(State::default()) })
    }

    fn emit(&self, ty: &'static str, data: Json) {
        if let Some(app) = self.app.upgrade() {
            app.bus.emit(ty, data, PublishOptions { id: None, location: Some(self.location.clone()) });
        }
    }

    fn session(&self, id: &str) -> Result<Arc<Session>, Error> {
        self.state.lock().unwrap().sessions.get(id).cloned().ok_or(Error::NotFound)
    }

    pub fn list(&self) -> Vec<Info> {
        let sessions: Vec<Arc<Session>> = self.state.lock().unwrap().sessions.values().cloned().collect();
        sessions.iter().map(|s| s.info.lock().unwrap().clone()).collect()
    }

    pub fn get(&self, id: &str) -> Result<Info, Error> {
        Ok(self.session(id)?.info.lock().unwrap().clone())
    }

    /// `Pty.create`. `default_command` resolves the configured shell (only when no command is given); a program that
    /// cannot start is a defect, as bun-pty throws.
    pub fn create(self: &Arc<Self>, input: CreateInput, directory: &str, default_command: impl FnOnce() -> String) -> std::io::Result<Info> {
        let id = format!("pty_{}", crate::id::ascending());
        let command = input.command.filter(|c| !c.is_empty()).unwrap_or_else(default_command);
        let mut args = Vec::with_capacity(input.args.len() + 1);
        if opencode_shell::select::login(&command) {
            args.push("-l".to_owned());
        }
        args.extend(input.args);
        let cwd = input.cwd.filter(|c| !c.is_empty()).unwrap_or_else(|| directory.to_owned());
        let mut env: IndexMap<String, String> = std::env::vars().collect();
        env.extend(input.env);
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("OPENCODE_TERMINAL".into(), "1".into());
        let env: Vec<(String, String)> = env.into_iter().collect();
        let spawned = terminal::spawn(&command, &args, &cwd, &env, terminal::COLS, terminal::ROWS)?;
        let tail: String = {
            let chars: Vec<char> = id.chars().collect();
            chars[chars.len().saturating_sub(4)..].iter().collect()
        };
        let info = Info {
            id: id.clone(),
            title: input.title.filter(|t| !t.is_empty()).unwrap_or_else(|| format!("Terminal {tail}")),
            command,
            args,
            cwd,
            running: true,
            pid: spawned.pid,
            exit_code: None,
        };
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let (stop_tx, stop_rx) = oneshot::channel();
        let master = Arc::new(spawned.master);
        let session = Arc::new(Session {
            info: Mutex::new(info.clone()),
            output: Mutex::new(Output::default()),
            input: input_tx,
            master: Mutex::new(Some(master.clone())),
            stop: Mutex::new(Some(stop_tx)),
        });
        self.state.lock().unwrap().sessions.insert(id, session.clone());
        tokio::spawn(pump(Arc::downgrade(self), session, master, spawned.child, input_rx, stop_rx));
        let mut data = Obj::new();
        data.set("info", info.to_json());
        self.emit("pty.created", data.done());
        Ok(info)
    }

    /// `Pty.update`: a title (when not empty), a size (while running); announced either way.
    pub fn update(&self, id: &str, title: Option<&str>, size: Option<(u16, u16)>) -> Result<Info, Error> {
        let session = self.session(id)?;
        let info = {
            let mut info = session.info.lock().unwrap();
            if let Some(title) = title.filter(|t| !t.is_empty()) {
                info.title = title.to_owned();
            }
            if let Some((rows, cols)) = size
                && info.running
                && let Some(master) = session.master.lock().unwrap().as_ref()
            {
                terminal::resize(master, cols, rows);
            }
            info.clone()
        };
        let mut data = Obj::new();
        data.set("info", info.to_json());
        self.emit("pty.updated", data.done());
        Ok(info)
    }

    pub fn remove(&self, id: &str) -> Result<(), Error> {
        self.session(id)?;
        self.remove_session(id);
        Ok(())
    }

    fn remove_session(&self, id: &str) {
        let session = {
            let mut state = self.state.lock().unwrap();
            let Some(session) = state.sessions.shift_remove(id) else { return };
            state.exit_order.retain(|e| e != id);
            session
        };
        teardown(&session, 1000, "");
        let mut data = Obj::new();
        data.str("id", id);
        self.emit("pty.deleted", data.done());
    }

    /// `Pty.attach` and the socket's start (handlers/pty.ts): the replay from `cursor` in bounded text frames and
    /// the control frame go to `sink` first, then live output; the end of the program closes it (1000).
    pub fn attach(&self, id: &str, cursor: Option<i64>, sink: mpsc::UnboundedSender<Outbound>) -> Result<Attachment, Error> {
        let session = self.session(id)?;
        if !session.info.lock().unwrap().running {
            return Err(Error::Exited);
        }
        let token = TOKENS.fetch_add(1, Ordering::Relaxed);
        {
            let mut output = session.output.lock().unwrap();
            let end = output.cursor;
            let from = match cursor {
                Some(-1) => end,
                Some(cursor) => cursor.max(0) as u64,
                None => 0,
            };
            for chunk in chunks(&output.replay(from)) {
                let _ = sink.send(Outbound::Text(chunk));
            }
            let _ = sink.send(Outbound::Binary(meta_frame(end)));
            output.subscribers.push((token, sink));
        }
        Ok(Attachment { session, token })
    }

    fn exited(&self, session: &Arc<Session>, code: i64) {
        let id = {
            let mut info = session.info.lock().unwrap();
            if !info.running {
                return;
            }
            info.running = false;
            info.exit_code = Some(code);
            info.id.clone()
        };
        session.output.lock().unwrap().end(1000, "");
        self.state.lock().unwrap().exit_order.push_back(id.clone());
        let mut data = Obj::new();
        data.str("id", &id).int("exitCode", code);
        self.emit("pty.exited", data.done());
        loop {
            let oldest = {
                let state = self.state.lock().unwrap();
                if state.exit_order.len() <= EXITED_LIMIT {
                    break;
                }
                state.exit_order.front().cloned()
            };
            let Some(oldest) = oldest else { break };
            self.remove_session(&oldest);
        }
    }

    /// The location shuts down: every socket closes (1012, `holdLocation`) and every terminal ends.
    pub fn close(&self) {
        let sessions: Vec<Arc<Session>> = {
            let mut state = self.state.lock().unwrap();
            state.exit_order.clear();
            state.sessions.drain(..).map(|(_, s)| s).collect()
        };
        for session in sessions {
            teardown(&session, 1012, "location shut down");
        }
    }
}

/// `teardown`: the program's events stop mattering, a running program is killed, attached sockets close.
fn teardown(session: &Session, code: u16, reason: &str) {
    {
        let mut output = session.output.lock().unwrap();
        output.disposed = true;
        output.end(code, reason);
        output.buffer = String::new();
    }
    session.master.lock().unwrap().take();
    if let Some(stop) = session.stop.lock().unwrap().take() {
        let _ = stop.send(());
    }
}

/// A terminal's I/O: output to its subscribers and buffer, input to the program, then its exit.
async fn pump(
    service: Weak<Service>,
    session: Arc<Session>,
    master: Arc<OwnedFd>,
    mut child: tokio::process::Child,
    mut input: mpsc::UnboundedReceiver<Vec<u8>>,
    mut stop: oneshot::Receiver<()>,
) {
    // SAFETY: the descriptor is owned by the `Arc<OwnedFd>` the `AsyncFd` holds, so it stays open (and the same)
    // until the `AsyncFd` drops.
    let Ok(fd) = (unsafe { AsyncFd::register(master) }) else {
        terminal::kill(&mut child).await;
        return;
    };
    let mut decoder = terminal::Utf8Stream::default();
    let mut buffer = vec![0u8; terminal::CHUNK];
    let mut pending: Option<(Vec<u8>, usize)> = None;
    let mut eof = false;
    let status = loop {
        tokio::select! {
            _ = &mut stop => {
                terminal::kill(&mut child).await;
                return;
            }
            status = child.wait() => break status.ok(),
            ready = fd.readable(), if !eof => {
                let Ok(mut guard) = ready else { eof = true; continue };
                match guard.try_io(|fd| terminal::read(fd.as_raw_fd(), &mut buffer)) {
                    Ok(Ok(0)) => eof = true,
                    Ok(Ok(n)) => session.on_data(&decoder.decode(&buffer[..n])),
                    Ok(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Ok(Err(_)) => eof = true,
                    Err(_would_block) => {}
                }
            }
            ready = fd.writable(), if pending.is_some() => {
                let Ok(mut guard) = ready else { pending = None; continue };
                let (data, offset) = pending.as_mut().expect("pending input");
                match guard.try_io(|fd| terminal::write(fd.as_raw_fd(), &data[*offset..])) {
                    Ok(Ok(n)) => {
                        *offset += n;
                        if *offset >= data.len() {
                            pending = None;
                        }
                    }
                    Ok(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Ok(Err(_)) => pending = None,
                    Err(_would_block) => {}
                }
            }
            data = input.recv(), if pending.is_none() => match data {
                Some(data) if !data.is_empty() => pending = Some((data, 0)),
                _ => {}
            },
        }
    };
    // The program is gone: what it printed before it exited is still in the terminal.
    if !eof {
        loop {
            match terminal::read(fd.get_ref().as_raw_fd(), &mut buffer) {
                Ok(0) => break,
                Ok(n) => session.on_data(&decoder.decode(&buffer[..n])),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    }
    session.on_data(&decoder.finish());
    drop(fd);
    let code = status.map(terminal::exit_code).unwrap_or(1);
    if session.output.lock().unwrap().disposed {
        return;
    }
    if let Some(service) = service.upgrade() {
        service.exited(&session, code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_offsets() {
        let mut output = Output::default();
        output.cursor = 42;
        output.push("hello\r\ngot hello xterm-256color 1\r\n24 80\r\n");
        assert_eq!(output.replay(0).len(), 42);
        assert_eq!(output.replay(7), "got hello xterm-256color 1\r\n24 80\r\n");
        assert_eq!(output.replay(42), "");
        assert_eq!(output.replay(100), "");
        let mut astral = Output::default();
        astral.cursor = 3;
        astral.push("a😀");
        assert_eq!(astral.replay(2), "\u{FFFD}");
        assert_eq!(meta_frame(42).as_ref(), b"\x00{\"cursor\":42}");
    }
}
