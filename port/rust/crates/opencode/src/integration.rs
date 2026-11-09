//! Integration logins (packages/core/src/integration.ts, `oauth` and `command`): the stateful attempts a location keeps
//! while a user logs in, and how a settled attempt becomes a stored credential.
//!
//! An OAuth attempt is authorized by its method's implementation, which hands out the URL to open and settles the
//! attempt by itself (`mode: auto`: its callback receives the browser). A command attempt runs its method's command:
//! what the command prints on stderr is the attempt's message while it runs, and what it prints on stdout (trimmed) is
//! the credential, stored as a key. Attempts live ten minutes; settled ones are reported for a minute more, then
//! forgotten (a scrub every 30 seconds). Cancelling a pending attempt forgets it and releases what it holds (the
//! callback server, the command's process).
//!
//! The OAuth implementations are the built-in plugins'. Poe's browser login ([`poe`]) is the one here; the other
//! catalog providers' (GitHub Copilot, GitLab, OpenAI, xAI, Snowflake, DigitalOcean, the OpenCode Console) are out of
//! scope (catalog providers), and connecting one of their methods fails as a method without an implementation does.
//! None of the ones here authorizes by a pasted code (`mode: code`) or accepts a pasted redirect (`paste`), so
//! completing an attempt by hand always finds it completing already.

pub mod page;
pub mod poe;
pub mod wellknown;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use opencode_protocol::Json;
use tokio::io::AsyncReadExt;

use crate::app::App;
use crate::json::{self, Obj};

const ATTEMPT_LIFETIME: i64 = 10 * 60_000;
const TERMINAL_RETENTION: i64 = 60_000;
const SCRUB_INTERVAL: Duration = Duration::from_secs(30);

/// An OAuth implementation's authorization: what the user opens, and the callback that settles the attempt.
pub struct Authorization {
    pub url: String,
    pub instructions: String,
    pub expires_at: Option<i64>,
    /// Resolves with the credential value (`Credential.OAuth`), or why the authorization failed.
    pub callback: Pin<Box<dyn Future<Output = Result<Json, String>> + Send>>,
    /// What the attempt holds (its scope), released when it settles or is cancelled.
    pub resources: Guard,
}

/// Aborts the tasks it holds when dropped.
pub struct Guard(pub Vec<tokio::task::AbortHandle>);

impl Drop for Guard {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// Why an attempt operation failed.
pub enum Error {
    /// The TS service dies (an empty 500).
    Defect(String),
    /// `Integration.AuthorizationError`.
    Authorization(String),
    /// `Integration.AttemptNotFoundError`.
    NotFound,
}

#[derive(Clone, Copy)]
struct Time {
    created: i64,
    expires: i64,
}

impl Time {
    fn json(&self) -> Json {
        let mut o = Obj::new();
        o.set("created", Json::Number(self.created as f64)).set("expires", Json::Number(self.expires as f64));
        o.done()
    }
}

enum Entry {
    Pending {
        integration_id: String,
        /// The integration's name, the default label of the credential.
        name: String,
        label: Option<String>,
        /// A command's stderr so far.
        message: Option<String>,
        /// Its credential is being stored: it can no longer be cancelled, expired or settled again.
        persisting: bool,
        resources: Option<Guard>,
        time: Time,
    },
    Terminal {
        status: &'static str,
        integration_id: String,
        message: Option<String>,
        remove_at: i64,
        time: Time,
    },
}

impl Entry {
    fn integration(&self) -> &str {
        match self {
            Entry::Pending { integration_id, .. } | Entry::Terminal { integration_id, .. } => integration_id,
        }
    }

    fn failed(integration_id: String, message: String, time: Time) -> Entry {
        let remove_at = crate::id::now() + TERMINAL_RETENTION;
        Entry::Terminal { status: "failed", integration_id, message: Some(message), remove_at, time }
    }
}

type Map = Mutex<HashMap<String, Entry>>;

#[derive(Default)]
struct Inner {
    oauth: Map,
    command: Map,
    scrubbing: AtomicBool,
}

/// A location's login attempts (`Integration.Service`'s attempt state).
#[derive(Default)]
pub struct Attempts {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Attempts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Attempts")
    }
}

/// Which of an integration's methods has an OAuth implementation here.
fn implemented(integration_id: &str, method_id: &str) -> bool {
    (integration_id, method_id) == (poe::INTEGRATION, poe::METHOD)
}

/// `answerError`: a form validates its answer; a method without one refuses a non-empty answer when it is named
/// (`Key`, `External`), while OAuth ignores it.
pub fn answer_error(form: Option<&Json>, answer: &Json, method: Option<&str>) -> Option<String> {
    if let Some(form) = form.and_then(json::array) {
        return crate::form::validate_fields(form).or_else(|| crate::form::validate_answer(form, answer));
    }
    let answered = matches!(answer, Json::Object(fields) if !fields.is_empty());
    method.filter(|_| answered).map(|method| format!("{method} method does not accept a form answer"))
}

impl Attempts {
    /// `oauth.connect`: authorizes the integration's OAuth method and starts the attempt.
    pub async fn oauth_connect(
        &self,
        app: &App,
        integration: Option<&Json>,
        integration_id: &str,
        method_id: &str,
        answer: Option<&Json>,
        label: Option<String>,
    ) -> Result<Json, Error> {
        let method = integration.and_then(|integration| {
            methods(integration).iter().find(|m| json::get_str(m, "type") == Some("oauth") && json::get_str(m, "id") == Some(method_id))
        });
        let (Some(method), true) = (method, implemented(integration_id, method_id)) else {
            return Err(Error::Defect(format!("OAuth method not found: {integration_id}/{method_id}")));
        };
        let answer = answer.cloned().unwrap_or_else(json::object);
        if let Some(invalid) = answer_error(json::get(method, "form"), &answer, None) {
            return Err(Error::Authorization(invalid));
        }
        let authorization = poe::authorize().await.map_err(Error::Authorization)?;
        let id = format!("con_{}", crate::id::ascending());
        let created = crate::id::now();
        let time = Time { created, expires: authorization.expires_at.unwrap_or(created + ATTEMPT_LIFETIME) };
        let name = integration.and_then(|i| json::get_str(i, "name")).unwrap_or(integration_id).to_owned();
        self.inner.oauth.lock().unwrap().insert(
            id.clone(),
            Entry::Pending {
                integration_id: integration_id.to_owned(),
                name,
                label,
                message: None,
                persisting: false,
                resources: Some(authorization.resources),
                time,
            },
        );
        self.scrub();
        let (inner, app_ref, attempt) = (Arc::downgrade(&self.inner), app.me.clone(), id.clone());
        let callback = authorization.callback;
        tokio::spawn(async move {
            let result = callback.await;
            if let (Some(inner), Some(app)) = (inner.upgrade(), app_ref.upgrade()) {
                settle(&app, &inner.oauth, &attempt, result);
            }
        });
        let mut o = Obj::new();
        o.str("attemptID", &id).str("url", &authorization.url).str("instructions", &authorization.instructions);
        o.str("mode", "auto").set("time", time.json());
        Ok(o.done())
    }

    /// `oauth.status`.
    pub fn oauth_status(&self, integration_id: &str, attempt_id: &str) -> Result<Json, Error> {
        status(&self.inner.oauth, integration_id, attempt_id, "Authorization failed")
    }

    /// `oauth.complete`: an attempt that settled already is done; a pending one settles through its own callback.
    pub fn oauth_complete(&self, integration_id: &str, attempt_id: &str) -> Result<(), Error> {
        let attempts = self.inner.oauth.lock().unwrap();
        match attempts.get(attempt_id).filter(|entry| entry.integration() == integration_id) {
            None => Err(Error::NotFound),
            Some(Entry::Terminal { .. }) => Ok(()),
            Some(Entry::Pending { .. }) => Err(Error::Defect(format!("OAuth attempt already completing: {attempt_id}"))),
        }
    }

    /// `oauth.cancel`.
    pub fn oauth_cancel(&self, integration_id: &str, attempt_id: &str) {
        cancel(&self.inner.oauth, integration_id, attempt_id);
    }

    /// `command.connect`: runs the integration's command method.
    pub fn command_connect(
        &self,
        app: &App,
        integration: &Json,
        method: &Json,
        label: Option<String>,
    ) -> Result<Json, Error> {
        let integration_id = json::get_str(integration, "id").unwrap_or_default().to_owned();
        let command: Vec<String> = json::get(method, "command")
            .and_then(json::array)
            .unwrap_or_default()
            .iter()
            .filter_map(|part| json::as_str(part).map(str::to_owned))
            .collect();
        if command.first().is_none_or(|program| program.is_empty()) {
            let method_id = json::get_str(method, "id").unwrap_or_default();
            return Err(Error::Defect(format!("Command method not found: {integration_id}/{method_id}")));
        }
        let id = format!("con_{}", crate::id::ascending());
        let created = crate::id::now();
        let time = Time { created, expires: created + ATTEMPT_LIFETIME };
        let name = json::get_str(integration, "name").unwrap_or(&integration_id).to_owned();
        self.inner.command.lock().unwrap().insert(
            id.clone(),
            Entry::Pending { integration_id, name, label, message: None, persisting: false, resources: None, time },
        );
        self.scrub();
        let (inner, app_ref, attempt) = (Arc::downgrade(&self.inner), app.me.clone(), id.clone());
        let task = tokio::spawn(async move {
            let result = run(&command, &inner, &attempt).await;
            if let (Some(inner), Some(app)) = (inner.upgrade(), app_ref.upgrade()) {
                let value = result.map(|key| {
                    let mut o = Obj::new();
                    o.str("type", "key").str("key", &key);
                    o.done()
                });
                settle(&app, &inner.command, &attempt, value);
            }
        });
        if let Some(Entry::Pending { resources, .. }) = self.inner.command.lock().unwrap().get_mut(&id) {
            *resources = Some(Guard(vec![task.abort_handle()]));
        }
        let mut o = Obj::new();
        o.str("attemptID", &id).set("time", time.json());
        Ok(o.done())
    }

    /// `command.status`: a pending attempt reports the command's stderr so far.
    pub fn command_status(&self, integration_id: &str, attempt_id: &str) -> Result<Json, Error> {
        status(&self.inner.command, integration_id, attempt_id, "Authentication failed")
    }

    /// `command.cancel`.
    pub fn command_cancel(&self, integration_id: &str, attempt_id: &str) {
        cancel(&self.inner.command, integration_id, attempt_id);
    }

    /// Starts the location's scrub of expired and forgotten attempts (once).
    fn scrub(&self) {
        if self.inner.scrubbing.swap(true, Ordering::SeqCst) {
            return;
        }
        let inner: Weak<Inner> = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SCRUB_INTERVAL).await;
                let Some(inner) = inner.upgrade() else { return };
                for map in [&inner.oauth, &inner.command] {
                    expire(map);
                }
            }
        });
    }
}

fn methods(integration: &Json) -> &[Json] {
    json::get(integration, "methods").and_then(json::array).unwrap_or(&[])
}

/// `claim` and `settle`: a failed attempt records why; a successful one stores its credential (labelled as asked,
/// else after the integration) and records the outcome. Either way the attempt's resources are released.
fn settle(app: &App, map: &Map, attempt_id: &str, result: Result<Json, String>) {
    let claimed = {
        let mut attempts = map.lock().unwrap();
        let Some(Entry::Pending { integration_id, name, label, persisting, time, .. }) = attempts.get_mut(attempt_id) else {
            return;
        };
        if *persisting {
            return;
        }
        match result {
            Err(message) => {
                let entry = Entry::failed(integration_id.clone(), message, *time);
                attempts.insert(attempt_id.to_owned(), entry);
                return;
            }
            Ok(value) => {
                *persisting = true;
                (integration_id.clone(), name.clone(), label.clone(), *time, value)
            }
        }
    };
    let (integration_id, name, label, time, value) = claimed;
    let persisted = crate::credential::create_labeled(app, &integration_id, &name, value, label);
    let entry = match persisted {
        Ok(_) => {
            let remove_at = crate::id::now() + TERMINAL_RETENTION;
            Entry::Terminal { status: "complete", integration_id, message: None, remove_at, time }
        }
        Err(error) => Entry::failed(integration_id, error.to_string(), time),
    };
    map.lock().unwrap().insert(attempt_id.to_owned(), entry);
}

fn status(map: &Map, integration_id: &str, attempt_id: &str, failed: &str) -> Result<Json, Error> {
    let attempts = map.lock().unwrap();
    let entry = attempts.get(attempt_id).filter(|entry| entry.integration() == integration_id).ok_or(Error::NotFound)?;
    let mut o = Obj::new();
    match entry {
        Entry::Pending { message, time, .. } => {
            o.str("status", "pending");
            if let Some(message) = message.as_deref().filter(|m| !m.is_empty()) {
                o.str("message", message);
            }
            o.set("time", time.json());
        }
        Entry::Terminal { status, message, time, .. } => {
            o.str("status", status);
            if *status == "failed" {
                o.str("message", message.as_deref().unwrap_or(failed));
            }
            o.set("time", time.json());
        }
    }
    Ok(o.done())
}

/// Forgets a pending attempt of the integration (one whose credential is being stored stays).
fn cancel(map: &Map, integration_id: &str, attempt_id: &str) {
    let mut attempts = map.lock().unwrap();
    let cancellable = matches!(
        attempts.get(attempt_id),
        Some(Entry::Pending { integration_id: owner, persisting: false, .. }) if owner == integration_id
    );
    if cancellable {
        attempts.remove(attempt_id);
    }
}

/// `scrub`: pending attempts past their expiry expire; settled ones past their retention are forgotten.
fn expire(map: &Map) {
    let now = crate::id::now();
    let mut attempts = map.lock().unwrap();
    attempts.retain(|_, entry| !matches!(entry, Entry::Terminal { remove_at, .. } if *remove_at <= now));
    for entry in attempts.values_mut() {
        if let Entry::Pending { integration_id, persisting: false, time, .. } = entry {
            if time.expires <= now {
                let integration_id = integration_id.clone();
                let time = *time;
                *entry = Entry::Terminal { status: "expired", integration_id, message: None, remove_at: now + TERMINAL_RETENTION, time };
            }
        }
    }
}

/// Runs a login command: its stderr accumulates as the attempt's message; a failed exit fails with it (or the exit
/// code), and a successful one yields stdout, trimmed.
async fn run(command: &[String], inner: &Weak<Inner>, attempt_id: &str) -> Result<String, String> {
    use std::process::Stdio;
    let description = command.join(" ");
    let mut child = tokio::process::Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| platform_error(&error, "spawn", &description))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let collect = async {
        let mut out = vec![];
        let _ = stdout.read_to_end(&mut out).await;
        out
    };
    let drain = async {
        let mut decoder = Utf8Stream::default();
        let mut buffer = [0u8; 8192];
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => append(inner, attempt_id, &decoder.decode(&buffer[..read])),
            }
        }
        append(inner, attempt_id, &decoder.finish());
    };
    let (output, ()) = tokio::join!(collect, drain);
    let status = child.wait().await.map_err(|error| platform_error(&error, "exitCode", &description))?;
    let Some(code) = status.code() else {
        return Err(format!("Unknown: ChildProcess.exitCode ({description})"));
    };
    if code != 0 {
        let message = inner.upgrade().and_then(|inner| match inner.command.lock().unwrap().get(attempt_id) {
            Some(Entry::Pending { message, .. }) => message.as_deref().map(|m| js_trim(m).to_owned()),
            _ => None,
        });
        return Err(message.filter(|m| !m.is_empty()).unwrap_or_else(|| format!("Authentication command exited {code}")));
    }
    let credential = js_trim(&String::from_utf8_lossy(&output)).to_owned();
    if credential.is_empty() {
        return Err("Authentication command returned no credential".into());
    }
    Ok(credential)
}

/// Adds to a pending attempt's message.
fn append(inner: &Weak<Inner>, attempt_id: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    let Some(inner) = inner.upgrade() else { return };
    if let Some(Entry::Pending { message, .. }) = inner.command.lock().unwrap().get_mut(attempt_id) {
        message.get_or_insert_with(String::new).push_str(text);
    }
}

/// An Effect `PlatformError` of the child process module, as its message reads.
fn platform_error(error: &std::io::Error, method: &str, description: &str) -> String {
    let reason = match error.kind() {
        std::io::ErrorKind::NotFound => "NotFound",
        std::io::ErrorKind::PermissionDenied => "PermissionDenied",
        _ => "Unknown",
    };
    format!("{reason}: ChildProcess.{method} ({description})")
}

/// `TextDecoder` over a stream: sequences split across chunks are joined, invalid bytes become U+FFFD.
#[derive(Default)]
struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    fn decode(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let mut out = String::new();
        let mut rest: &[u8] = &self.pending;
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.push_str(text);
                    rest = &[];
                    break;
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match error.error_len() {
                        Some(len) => {
                            out.push('\u{fffd}');
                            rest = &after[len..];
                        }
                        None => {
                            rest = after;
                            break;
                        }
                    }
                }
            }
        }
        self.pending = rest.to_vec();
        out
    }

    fn finish(&mut self) -> String {
        let out = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        out
    }
}

/// `String.prototype.trim`.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}
