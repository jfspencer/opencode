//! `opencode run` (packages/cli/src/commands/handlers/run.ts, run/run.ts, run/noninteractive.ts, run/ui.ts,
//! session-target.ts): one prompt to completion through a server. The message comes from the arguments and
//! stdin, files are attached inline or as data URLs, the session is new, continued, chosen or forked, and the
//! server's events are rendered as they arrive (text on stdout, everything else on stderr, or one JSON event per
//! line). Permission requests are rejected (or approved with `--auto`), questions cancelled.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use opencode_protocol::Json;
use opencode_protocol::wire::number::js_trim;

use super::client::{Client, ClientError};
use super::connection::{self, Args, Resolved};
use super::out::{Result, stderr, stdout};
use super::parse::{Globals, Input};
use super::tool;
use crate::json::{self, Obj, get, get_str};

const TEXT_DIM: &str = "\x1b[90m";
const TEXT_NORMAL: &str = "\x1b[0m";
const TEXT_WARNING_BOLD: &str = "\x1b[93m\x1b[1m";
const TEXT_DANGER_BOLD: &str = "\x1b[91m\x1b[1m";

const QUESTION_CANCELLED_FEEDBACK: &str = "This non-interactive run cannot ask the user questions, so the question was cancelled. Continue without an answer; make reasonable assumptions and state them.";
const PERMISSION_REJECTED_FEEDBACK: &str =
    "This non-interactive run cannot ask the user for permission, so the request was rejected. Continue without this action.";

const ATTACH_FILE_MAX_BYTES: u64 = 10 * 1024 * 1024;

// --- UI (run/ui.ts) ---

static BLANK: AtomicBool = AtomicBool::new(false);

fn println(parts: &[&str]) {
    stderr(&format!("{}\n", parts.join(" ")));
}

/// `UI.empty`: one blank line, the first time only.
fn empty() {
    if BLANK.swap(true, Ordering::SeqCst) {
        return;
    }
    println(&[TEXT_NORMAL]);
}

fn ui_error(message: &str) {
    let message = message.strip_prefix("Error: ").unwrap_or(message);
    println(&[&format!("{TEXT_DANGER_BOLD}Error: {TEXT_NORMAL}{message}")]);
}

fn now() -> i64 {
    crate::id::now()
}

static EXIT: AtomicI32 = AtomicI32::new(0);

/// `reportRunError`.
fn report(json_format: bool, message: &str, session: Option<&str>) {
    EXIT.store(1, Ordering::SeqCst);
    if json_format {
        let mut error = Obj::new();
        error.str("type", "unknown").str("message", message);
        let mut o = Obj::new();
        o.str("type", "error").int("timestamp", now()).str("sessionID", session.unwrap_or("")).set("error", error.done());
        stdout(&format!("{}\n", json::text(&o.done())));
        return;
    }
    ui_error(message);
}

struct Options {
    message: Vec<String>,
    continue_: bool,
    session: Option<String>,
    fork: bool,
    worktree: Option<Option<String>>,
    model: Option<String>,
    agent: Option<String>,
    json: bool,
    files: Vec<String>,
    title: Option<String>,
    archived: bool,
    thinking: bool,
    auto: bool,
}

/// The handler: resolve a server without ever leaving a new service behind, then run until done or a signal.
pub async fn command(globals: &Globals, input: &Input) -> Result {
    let options = Options {
        message: input.list("message").to_vec(),
        continue_: input.flag("continue"),
        session: input.str("session").map(String::from),
        fork: input.flag("fork"),
        worktree: input.optional_value("worktree").map(|v| v.map(String::from)),
        model: input.str("model").map(String::from),
        agent: input.str("agent").map(String::from),
        json: input.str("format") == Some("json"),
        files: input.list("file").to_vec(),
        title: input.str("title").map(String::from),
        archived: input.flag("archived"),
        thinking: input.flag("thinking"),
        auto: input.flag("auto") || input.flag("yolo") || input.flag("dangerously-skip-permissions"),
    };
    let server = match connection::resolve(globals, Args { no_autostart: true, ..Args::default() }).await {
        Ok(server) => server,
        Err(fail) => {
            report(options.json, &fail.message, None);
            return Ok(1);
        }
    };
    let (abort, aborted) = tokio::sync::watch::channel(false);
    let signal = Arc::new(AtomicI32::new(0));
    let watcher = {
        let signal = signal.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal as listen};
            let mut terminate = listen(SignalKind::terminate()).expect("signal handler");
            let mut interrupt = listen(SignalKind::interrupt()).expect("signal handler");
            let code = tokio::select! {
                _ = terminate.recv() => 143,
                _ = interrupt.recv() => 130,
            };
            signal.store(code, Ordering::SeqCst);
            let _ = abort.send(true);
        })
    };
    if let Err(message) = run(&options, &server, aborted).await {
        report(options.json, &message, None);
    }
    watcher.abort();
    server.close().await;
    let code = signal.load(Ordering::SeqCst);
    Ok(if code != 0 { code } else { EXIT.load(Ordering::SeqCst) })
}

/// Reads stdin in full on a thread of its own; `grace` bounds the wait for the first data or the end when stdin
/// is neither a pipe nor a file (`readStdinIfReady`).
async fn read_stdin(grace: Option<std::time::Duration>) -> Option<String> {
    let (first, first_rx) = tokio::sync::oneshot::channel::<()>();
    let (done, done_rx) = tokio::sync::oneshot::channel::<String>();
    let wait = grace.is_some() && {
        // SAFETY: fstat fills a plain struct.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let kind = if unsafe { libc::fstat(0, &mut stat) } == 0 { stat.st_mode & libc::S_IFMT } else { 0 };
        kind != libc::S_IFIFO && kind != libc::S_IFREG
    };
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buffer = vec![];
        let mut chunk = [0u8; 8192];
        let mut first = Some(first);
        loop {
            match stdin.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buffer.extend_from_slice(&chunk[..n]);
                    if let Some(first) = first.take() {
                        let _ = first.send(());
                    }
                }
            }
        }
        let _ = done.send(String::from_utf8_lossy(&buffer).into_owned());
    });
    if wait {
        let grace = grace.unwrap_or_default();
        let mut done_rx = done_rx;
        tokio::select! {
            text = &mut done_rx => return text.ok(),
            started = first_rx => if started.is_err() { return done_rx.await.ok() },
            _ = tokio::time::sleep(grace) => return None,
        }
        return done_rx.await.ok();
    }
    done_rx.await.ok()
}

fn merge_input(message: Option<String>, piped: Option<String>) -> Option<String> {
    match (message, piped) {
        (None, piped) => piped.filter(|p| !p.is_empty()),
        (Some(message), None) => Some(message),
        (Some(message), Some(piped)) if piped.is_empty() => Some(message),
        (Some(message), Some(piped)) => Some(format!("{message}\n{piped}")),
    }
}

enum FilePart {
    Text(String),
    Attachment { uri: String, name: String },
}

fn is_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    if bytes.contains(&0) {
        return true;
    }
    let control = bytes.iter().filter(|b| **b < 9 || (**b > 13 && **b < 32)).count();
    control as f64 / bytes.len() as f64 > 0.3
}

fn prepare_file(input: &str, root: &str) -> std::result::Result<FilePart, String> {
    use base64::Engine;
    let file = if input.starts_with('/') { input.to_owned() } else { format!("{root}/{input}") };
    let file = crate::project::absolutize(&file);
    let handle = std::fs::File::open(&file).map_err(|_| format!("File not found: {input}"))?;
    let meta = handle.metadata().map_err(|_| format!("File not found: {input}"))?;
    if !meta.is_file() || meta.len() > ATTACH_FILE_MAX_BYTES {
        return Err(format!("Cannot attach a directory, special file, or file larger than 10 MiB: {input}"));
    }
    let bytes = std::fs::read(&file).map_err(|_| format!("File not found: {input}"))?;
    let detected = crate::fs::mime_type(&file);
    let text = String::from_utf8(bytes.clone()).ok();
    let mime = if detected.starts_with("image/") || detected == "application/pdf" {
        detected
    } else if !is_binary(&bytes) && text.is_some() {
        "text/plain"
    } else {
        detected
    };
    let name = file.rsplit('/').next().unwrap_or(&file).to_owned();
    if mime == "text/plain" {
        return Ok(FilePart::Text(format!("<file name=\"{name}\">\n{}\n</file>", text.unwrap_or_default())));
    }
    Ok(FilePart::Attachment {
        uri: format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)),
        name,
    })
}

/// `Model.Ref.parse`.
fn parse_model(value: &str) -> std::result::Result<Json, String> {
    let invalid = || format!("Invalid model reference: {value}");
    let end = value.find('/').filter(|i| *i > 0).ok_or_else(invalid)?;
    let provider = &value[..end];
    let rest = &value[end + 1..];
    let (id, variant) = match rest.find('#') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    if id.is_empty() || provider.contains('#') || variant.is_some_and(|v| v.is_empty() || v.contains('#')) {
        return Err(invalid());
    }
    let mut o = Obj::new();
    o.str("providerID", provider).str("id", id).opt_str("variant", variant);
    Ok(o.done())
}

fn location_query(directory: &str) -> Json {
    let mut inner = Obj::new();
    inner.str("directory", directory);
    let mut o = Obj::new();
    o.set("location", inner.done());
    o.done()
}

fn message_of(error: ClientError) -> String {
    error.message()
}

async fn run(options: &Options, server: &Resolved, aborted: tokio::sync::watch::Receiver<bool>) -> std::result::Result<(), String> {
    if options.fork && !options.continue_ && options.session.is_none() {
        return Err("--fork requires --continue or --session".into());
    }
    if options.worktree.is_some() && (options.continue_ || options.session.is_some()) {
        return Err("--worktree starts a new session and cannot be combined with --continue or --session".into());
    }
    let root = std::env::var("PWD").ok().filter(|p| !p.is_empty()).unwrap_or_else(super::startup_directory);
    if std::env::set_current_dir(&root).is_err() {
        return Err(format!("Failed to change directory to {root}"));
    }
    let directory = crate::project::absolutize(".");
    let argument = Some(options.message.join(" ")).filter(|m| !m.is_empty());
    let piped = if super::stdin_tty() {
        None
    } else if argument.is_some() {
        read_stdin(Some(std::time::Duration::from_millis(1000))).await
    } else {
        read_stdin(None).await
    };
    let message = merge_input(argument, piped).filter(|m| !js_trim(m).is_empty()).ok_or("You must provide a message")?;
    let mut files = vec![];
    for file in &options.files {
        files.push(prepare_file(file, &root)?);
    }
    execute(options, server, &directory, &message, files, aborted).await
}

async fn execute(
    options: &Options,
    server: &Resolved,
    directory: &str,
    message: &str,
    files: Vec<FilePart>,
    aborted: tokio::sync::watch::Receiver<bool>,
) -> std::result::Result<(), String> {
    let client = server.client();
    let explicit = options.model.as_deref().map(parse_model).transpose()?;
    let mut location = directory.to_owned();
    if let Some(name) = &options.worktree {
        let (root, inner) = create_worktree(&client, directory, name.as_deref()).await?;
        println(&[&format!("{TEXT_DIM}worktree {root}{TEXT_NORMAL}")]);
        location = inner;
    }
    let target = match resolve_target(&client, options, &location, explicit.as_ref(), server.service).await {
        Ok(target) => target,
        Err(TargetError::Run(message, session)) => {
            report(options.json, &message, session.as_deref());
            return Ok(());
        }
        Err(TargetError::Other(message)) => return Err(message),
    };
    let session_id = get_str(&target.session, "id").unwrap_or("").to_owned();
    let title = match (&options.title, target.resume) {
        (Some(title), false) => Some(if title.is_empty() {
            let units: Vec<u16> = message.encode_utf16().collect();
            let head = String::from_utf16_lossy(&units[..units.len().min(50)]);
            format!("{head}{}", if units.len() > 50 { "..." } else { "" })
        } else {
            title.clone()
        }),
        _ => None,
    };
    let archived = options.archived && (!target.resume || options.fork);
    if title.is_some() || archived {
        let mut body = Obj::new();
        body.opt_str("title", title.as_deref());
        if archived {
            body.set("archived", Json::Bool(true));
        }
        client.call("session.update", &[("sessionID", &session_id)], None, Some(body.done())).await.map_err(message_of)?;
    }
    let mut text = message.to_owned();
    let mut attachments = vec![];
    for file in files {
        match file {
            FilePart::Text(content) => {
                text.push_str("\n\n");
                text.push_str(&content);
            }
            FilePart::Attachment { uri, name } => {
                let mut o = Obj::new();
                o.str("uri", &uri).str("name", &name);
                attachments.push(o.done());
            }
        }
    }
    let prompt = Prompt {
        client: client.clone(),
        session_id: session_id.clone(),
        directory: get_str(&target.location, "directory").unwrap_or(directory).to_owned(),
        message: text,
        files: attachments,
        agent: target.agent.clone(),
        model: target.model.clone(),
        thinking: options.thinking,
        json: options.json,
        auto: options.auto,
        standalone: server.private.is_some(),
    };
    if let Err(message) = prompt.run(aborted).await {
        report(options.json, &message, Some(&session_id));
    }
    Ok(())
}

// --- session target (session-target.ts) ---

struct Target {
    session: Json,
    location: Json,
    model: Option<Json>,
    agent: Option<String>,
    resume: bool,
}

enum TargetError {
    /// `RunTargetError`: reported with the session it concerns.
    Run(String, Option<String>),
    Other(String),
}

impl From<ClientError> for TargetError {
    fn from(error: ClientError) -> Self {
        TargetError::Other(error.message())
    }
}

async fn location_get(client: &Client, directory: Option<&str>) -> std::result::Result<Json, ClientError> {
    client.call("location.get", &[], directory.map(location_query), None).await
}

async fn find_session(client: &Client, id: &str) -> std::result::Result<Option<Json>, ClientError> {
    match client.call("session.get", &[("sessionID", id)], None, None).await {
        Ok(found) => Ok(get(&found, "data").cloned()),
        Err(error) if error.tag() == Some("SessionNotFoundError") => Ok(None),
        Err(error) => Err(error),
    }
}

async fn fork(client: &Client, id: &str) -> std::result::Result<Json, TargetError> {
    let forked = client.call("session.fork", &[("sessionID", id)], None, Some(json::object())).await?;
    Ok(get(&forked, "data").cloned().unwrap_or(Json::Null))
}

async fn latest_session(client: &Client, location: &Json) -> std::result::Result<Option<Json>, ClientError> {
    let directory = get_str(location, "directory").unwrap_or("").to_owned();
    let mut cursor: Option<String> = None;
    loop {
        let mut query = Obj::new();
        query
            .str("directory", &directory)
            .set("parentID", Json::Null)
            .set("archived", Json::Bool(false))
            .int("limit", 50)
            .str("order", "desc");
        if let Some(cursor) = &cursor {
            query.str("cursor", cursor);
        }
        let page = client.call("session.list", &[], Some(query.done()), None).await?;
        let data = get(&page, "data").and_then(json::array).unwrap_or(&[]);
        if let Some(found) = data.iter().find(|s| get(s, "location").and_then(|l| get_str(l, "directory")) == Some(&directory)) {
            return Ok(Some(found.clone()));
        }
        let next = get(&page, "cursor").and_then(|c| get_str(c, "next")).map(String::from);
        match next {
            Some(next) if !data.is_empty() => cursor = Some(next),
            _ => return Ok(None),
        }
    }
}

async fn resolve_target(
    client: &Client,
    options: &Options,
    directory: &str,
    explicit: Option<&Json>,
    service: bool,
) -> std::result::Result<Target, TargetError> {
    // selectSession
    let mut selected: Option<Json> = None;
    let mut location: Option<Json> = None;
    if let Some(id) = &options.session {
        match find_session(client, id).await? {
            None if options.fork => return Err(TargetError::Other("Session not found".into())),
            None => {}
            Some(found) => {
                let found_id = get_str(&found, "id").unwrap_or("").to_owned();
                selected = Some(if options.fork { fork(client, &found_id).await? } else { found });
            }
        }
    } else if options.continue_ {
        let resolved = location_get(client, Some(directory)).await?;
        match latest_session(client, &resolved).await? {
            None => location = Some(resolved),
            Some(found) => {
                let found_id = get_str(&found, "id").unwrap_or("").to_owned();
                selected = Some(if options.fork { fork(client, &found_id).await? } else { found });
            }
        }
    }
    let location = match location {
        Some(location) => location,
        None => {
            let at = match &selected {
                Some(session) => get(session, "location").and_then(|l| get_str(l, "directory")).map(String::from),
                None => Some(directory.to_owned()),
            };
            location_get(client, at.as_deref()).await?
        }
    };
    let model = explicit.cloned().or_else(|| selected.as_ref().and_then(|s| get(s, "model")).cloned());
    let agent = options.agent.clone().or_else(|| selected.as_ref().and_then(|s| get_str(s, "agent")).map(String::from));
    // prepare (run.ts)
    if explicit.is_some_and(|m| get(m, "variant").is_some()) && model.is_none() {
        return Err(TargetError::Run(
            "Cannot select a variant before selecting a model".into(),
            selected.as_ref().and_then(|s| get_str(s, "id")).map(String::from),
        ));
    }
    let resume = selected.is_some();
    let session = match selected {
        Some(session) => session,
        None => {
            let mut body = Obj::new();
            let mut at = Obj::new();
            at.opt("directory", get(&location, "directory").cloned());
            body.opt_str("id", options.session.as_deref())
                .opt_str("agent", agent.as_deref())
                .opt("model", model.clone())
                .set("location", at.done());
            let created = client.call("session.create", &[], None, Some(body.done())).await?;
            get(&created, "data").cloned().unwrap_or(Json::Null)
        }
    };
    if service {
        let mut variables = Obj::new();
        for (key, value) in std::env::vars() {
            if key != "OPENCODE_PASSWORD" && key != "OPENCODE_SERVER_PASSWORD" {
                variables.str(&key, &value);
            }
        }
        let mut body = Obj::new();
        body.set("variables", variables.done());
        let id = get_str(&session, "id").unwrap_or("").to_owned();
        client.call("session.environment", &[("sessionID", &id)], None, Some(body.done())).await?;
    }
    let agent = agent.or_else(|| get_str(&session, "agent").map(String::from));
    Ok(Target { session, location, model, agent, resume })
}

/// `createWorktreeLocation`: a new worktree of the location's project, keeping the caller's subfolder.
async fn create_worktree(client: &Client, directory: &str, name: Option<&str>) -> std::result::Result<(String, String), String> {
    let base = location_get(client, Some(directory)).await.map_err(message_of)?;
    let project = get(&base, "project");
    let mut body = Obj::new();
    body.opt("projectID", project.and_then(|p| get(p, "id")).cloned())
        .opt("from", project.and_then(|p| get(p, "directory")).cloned())
        .opt_str("name", name);
    let created = client.call("worktree.create", &[], None, Some(body.done())).await.map_err(message_of)?;
    let root = get_str(&created, "directory").unwrap_or("").to_owned();
    let project_directory = project.and_then(|p| get_str(p, "directory")).unwrap_or("");
    let base_directory = get_str(&base, "directory").unwrap_or("");
    let subfolder = base_directory.strip_prefix(project_directory).map(|s| s.trim_start_matches('/')).unwrap_or("");
    if subfolder.is_empty() || !base_directory.starts_with(project_directory) {
        return Ok((root.clone(), root));
    }
    let inner = format!("{root}/{subfolder}");
    match location_get(client, Some(&inner)).await {
        Ok(_) => Ok((root, inner)),
        Err(_) => Ok((root.clone(), root)),
    }
}

// --- the prompt (run/noninteractive.ts) ---

struct Prompt {
    client: Client,
    session_id: String,
    directory: String,
    message: String,
    files: Vec<Json>,
    agent: Option<String>,
    model: Option<Json>,
    thinking: bool,
    json: bool,
    auto: bool,
    standalone: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Running,
    Settled,
    Cancelling,
}

#[derive(Clone)]
struct Outcome {
    id: String,
    outcome: &'static str,
    error: Option<Json>,
}

struct State {
    phase: Phase,
    promoted: bool,
    failure: Option<Json>,
    outcomes: Vec<Outcome>,
    job_sessions: Vec<String>,
    ended: u64,
    broken: Option<String>,
    owned: HashMap<String, bool>,
    render: Renderer,
}

type Shared = Arc<Mutex<State>>;

fn run_error(kind: &str, message: &str) -> Json {
    let mut o = Obj::new();
    o.str("type", kind).str("message", message);
    o.done()
}

fn terminal_outcome(event: &Json) -> Option<Outcome> {
    let id = get_str(event, "id").unwrap_or("").replacen("evt_", "msg_", 1);
    let data = get(event, "data");
    match get_str(event, "type")? {
        "session.execution.succeeded" => Some(Outcome { id, outcome: "succeeded", error: None }),
        "session.execution.failed" => Some(Outcome { id, outcome: "failed", error: data.and_then(|d| get(d, "error")).cloned() }),
        "session.execution.interrupted" => {
            let reason = data.and_then(|d| get(d, "reason")).map(|r| json::as_str(r).map(String::from).unwrap_or_else(|| json::text(r))).unwrap_or_default();
            Some(Outcome { id, outcome: "interrupted", error: Some(run_error("aborted", &format!("Session interrupted: {reason}"))) })
        }
        _ => None,
    }
}

impl Prompt {
    async fn owns_session(&self, state: &Shared, session_id: &str) -> bool {
        if session_id == self.session_id {
            return true;
        }
        let mut current = session_id.to_owned();
        let mut chain = vec![];
        let result = loop {
            if current == self.session_id {
                break true;
            }
            if let Some(known) = state.lock().unwrap().owned.get(&current).copied() {
                break known;
            }
            chain.push(current.clone());
            match self.client.call("session.get", &[("sessionID", &current)], None, None).await {
                Ok(found) => match get(&found, "data").and_then(|s| get_str(s, "parentID")) {
                    Some(parent) => current = parent.to_owned(),
                    None => break false,
                },
                Err(_) => break false,
            }
        };
        let mut locked = state.lock().unwrap();
        for id in chain {
            locked.owned.insert(id, result);
        }
        result
    }

    async fn reply_permission(&self, request: &Json) {
        if !self.auto {
            let resources: Vec<&str> =
                get(request, "resources").and_then(json::array).unwrap_or(&[]).iter().filter_map(json::as_str).collect();
            println(&[
                &format!("{TEXT_WARNING_BOLD}!"),
                &format!(
                    "{TEXT_NORMAL}permission requested: {} ({}); auto-rejecting",
                    get_str(request, "action").unwrap_or(""),
                    resources.join(", ")
                ),
            ]);
        }
        let mut body = Obj::new();
        body.str("decision", if self.auto { "once" } else { "reject" });
        if !self.auto {
            body.str("message", PERMISSION_REJECTED_FEEDBACK);
        }
        let session = get_str(request, "sessionID").unwrap_or("");
        let id = get_str(request, "id").unwrap_or("");
        let _ = self
            .client
            .call("session.permission.reply", &[("sessionID", session), ("requestID", id)], None, Some(body.done()))
            .await;
    }

    async fn owns_form(&self, state: &Shared, form: &Json, location: Option<&Json>) -> bool {
        let session = get_str(form, "sessionID").unwrap_or("");
        if session == "global" {
            return self.standalone && location.and_then(|l| get_str(l, "directory")) == Some(&self.directory);
        }
        self.owns_session(state, session).await
    }

    async fn cancel_form(&self, state: &Shared, form: &Json) -> std::result::Result<(), String> {
        let question = get(form, "metadata").and_then(|m| get_str(m, "kind")) == Some("question");
        let session = get_str(form, "sessionID").unwrap_or("").to_owned();
        let id = get_str(form, "id").unwrap_or("").to_owned();
        let mut query = Obj::new();
        if question {
            query.str("message", QUESTION_CANCELLED_FEEDBACK);
        }
        let extra = if session == "global" {
            vec![("x-opencode-directory".to_owned(), super::client::encode_component(&self.directory))]
        } else {
            vec![]
        };
        let result = self
            .client
            .call_with("session.form.cancel", &[("sessionID", &session), ("formID", &id)], Some(query.done()), None, &extra, None)
            .await;
        if let Err(error) = result {
            if error.tag() != Some("FormAlreadySettledError") {
                return Err(error.message());
            }
        }
        if !question {
            let mut locked = state.lock().unwrap();
            if locked.failure.is_none() {
                let title = get_str(form, "title").unwrap_or("");
                locked.failure = Some(run_error("unknown", &format!("Cancelled a request for input this run cannot answer: {title}")));
            }
        }
        Ok(())
    }

    /// The event consumer: answers asks, records outcomes and renders this prompt's events.
    async fn consume(self: Arc<Self>, state: Shared, mut events: super::client::Events, message_id: String) {
        loop {
            let event = match events.next().await {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    state.lock().unwrap().broken = Some(error.message());
                    return;
                }
                None => {
                    state.lock().unwrap().broken = Some("Event stream disconnected during prompt execution".into());
                    return;
                }
            };
            let kind = get_str(&event, "type").unwrap_or("").to_owned();
            let data = get(&event, "data").cloned().unwrap_or(Json::Null);
            if kind == "permission.asked" {
                if self.owns_session(&state, get_str(&data, "sessionID").unwrap_or("")).await {
                    self.reply_permission(&data).await;
                }
                continue;
            }
            if kind == "form.created" {
                let form = get(&data, "form").cloned().unwrap_or(Json::Null);
                if self.owns_form(&state, &form, get(&event, "location")).await {
                    if let Err(message) = self.cancel_form(&state, &form).await {
                        state.lock().unwrap().broken = Some(message);
                        return;
                    }
                }
                continue;
            }
            let Some(session_id) = get_str(&data, "sessionID").map(String::from) else { continue };
            let terminal = terminal_outcome(&event);
            let mut locked = state.lock().unwrap();
            if locked.job_sessions.contains(&session_id) {
                if kind == "session.execution.started" && locked.phase == Phase::Cancelling {
                    let client = self.client.clone();
                    let id = session_id.clone();
                    tokio::spawn(async move {
                        let _ = client.call("session.interrupt", &[("sessionID", &id)], None, None).await;
                    });
                }
                if terminal.is_some() {
                    locked.ended += 1;
                }
            }
            if session_id != self.session_id {
                continue;
            }
            if kind == "session.inbox.delivered" && get_str(&data, "inboxID") == Some(&message_id) {
                locked.promoted = true;
            }
            if let Some(terminal) = &terminal {
                if !locked.promoted {
                    if let Some(error) = &terminal.error {
                        if locked.failure.is_none() {
                            locked.failure = Some(error.clone());
                        }
                    }
                } else {
                    locked.outcomes.push(terminal.clone());
                }
            }
            if locked.promoted && locked.phase == Phase::Running {
                locked.render.event(&event);
            }
        }
    }

    /// This prompt's jobs (`[]` while one of their wakes is queued or running), or `None` when nothing is left.
    async fn background(&self, state: &Shared, since: f64) -> std::result::Result<Option<Vec<Json>>, String> {
        let mut query = Obj::new();
        query.str("sessionID", &self.session_id).set("descendants", Json::Bool(true));
        let jobs = match self.client.call("experimental.job.list", &[], Some(query.done()), None).await {
            Ok(list) => get(&list, "data").and_then(json::array).map(|a| a.to_vec()).unwrap_or_default(),
            Err(ClientError::UnsupportedContentType(_)) => vec![],
            Err(error) => return Err(error.message()),
        };
        let running: Vec<Json> = jobs
            .into_iter()
            .filter(|job| get(job, "time").and_then(|t| json::get_num(t, "started")).is_some_and(|s| s >= since))
            .collect();
        {
            let mut locked = state.lock().unwrap();
            for job in &running {
                for key in ["sessionID", "childSessionID"] {
                    if key == "childSessionID" && get_str(job, "kind") != Some("subagent") {
                        continue;
                    }
                    if let Some(id) = get_str(job, key) {
                        if !locked.job_sessions.iter().any(|s| s == id) {
                            locked.job_sessions.push(id.to_owned());
                        }
                    }
                }
            }
        }
        if !running.is_empty() {
            return Ok(Some(running));
        }
        let sessions = state.lock().unwrap().job_sessions.clone();
        for session in &sessions {
            let inbox = self.client.call("session.inbox.list", &[("sessionID", session)], None, None).await.map_err(message_of)?;
            if get(&inbox, "data").and_then(json::array).is_some_and(|items| !items.is_empty()) {
                return Ok(Some(running));
            }
        }
        let active = self.client.call("session.active", &[], None, None).await.map_err(message_of)?;
        if let Some(Json::Object(active)) = get(&active, "data") {
            for session in active.keys() {
                if self.owns_session(state, session).await {
                    return Ok(Some(running));
                }
            }
        }
        Ok(None)
    }

    async fn projected(&self, message_id: &str) -> std::result::Result<(bool, Vec<Json>), String> {
        let mut messages = vec![];
        let mut cursor: Option<String> = None;
        loop {
            let mut query = Obj::new();
            query.int("limit", 200);
            match &cursor {
                Some(cursor) => {
                    query.str("cursor", cursor);
                }
                None => {
                    query.str("order", "desc");
                }
            }
            let page = self
                .client
                .call("session.message.list", &[("sessionID", &self.session_id)], Some(query.done()), None)
                .await
                .map_err(message_of)?;
            for message in get(&page, "data").and_then(json::array).unwrap_or(&[]) {
                if get_str(message, "id") == Some(message_id) {
                    messages.reverse();
                    return Ok((true, messages));
                }
                messages.push(message.clone());
            }
            cursor = get(&page, "cursor").and_then(|c| get_str(c, "next")).map(String::from);
            if cursor.is_none() {
                return Ok((false, vec![]));
            }
        }
    }

    async fn wait_ended(state: &Shared, seen: u64) -> std::result::Result<(), String> {
        loop {
            {
                let locked = state.lock().unwrap();
                if let Some(message) = &locked.broken {
                    return Err(message.clone());
                }
                if locked.ended > seen {
                    return Ok(());
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    async fn wait(&self) -> std::result::Result<(), String> {
        self.client
            .call("experimental.session.wait", &[("sessionID", &self.session_id)], None, None)
            .await
            .map(|_| ())
            .map_err(message_of)
    }

    /// admitting → running → settling → waiting for this prompt's jobs.
    async fn lifecycle(
        self: &Arc<Self>,
        state: &Shared,
        message_id: &str,
        admission: &Arc<Mutex<Option<f64>>>,
        events: super::client::Events,
        aborted: &tokio::sync::watch::Receiver<bool>,
    ) -> std::result::Result<Option<(bool, Vec<Json>, bool)>, String> {
        let id = self.session_id.clone();
        if let Some(agent) = &self.agent {
            let mut body = Obj::new();
            body.str("agent", agent);
            self.client.call("session.switchAgent", &[("sessionID", &id)], None, Some(body.done())).await.map_err(message_of)?;
        }
        if let Some(model) = &self.model {
            let mut body = Obj::new();
            body.set("model", model.clone());
            self.client.call("session.switchModel", &[("sessionID", &id)], None, Some(body.done())).await.map_err(message_of)?;
        }
        if *aborted.borrow() {
            return Ok(None);
        }
        tokio::spawn(self.clone().consume(state.clone(), events, message_id.to_owned()));
        let mut body = Obj::new();
        body.str("id", message_id).str("text", &self.message).set("files", Json::Array(self.files.clone())).str("delivery", "steer");
        let admitted = self.client.call("session.prompt", &[("sessionID", &id)], None, Some(body.done())).await.map_err(message_of)?;
        let since = get(&admitted, "data").and_then(|d| get(d, "time")).and_then(|t| json::get_num(t, "created")).unwrap_or(0.0);
        *admission.lock().unwrap() = Some(since);
        let permissions = self.client.call("session.permission.list", &[("sessionID", &id)], None, None).await;
        let forms = self.client.call("session.form.list", &[("sessionID", &id)], None, None).await;
        let globals = if self.standalone {
            self.client.call("form.list", &[], Some(location_query(&self.directory)), None).await.ok()
        } else {
            None
        };
        for request in permissions.ok().as_ref().and_then(|p| get(p, "data")).and_then(json::array).unwrap_or(&[]) {
            self.reply_permission(request).await;
        }
        for form in forms.ok().as_ref().and_then(|f| get(f, "data")).and_then(json::array).unwrap_or(&[]) {
            self.cancel_form(state, form).await?;
        }
        if let Some(globals) = &globals {
            let location = get(globals, "location");
            for form in get(globals, "data").and_then(json::array).unwrap_or(&[]) {
                if self.owns_form(state, form, location).await {
                    self.cancel_form(state, form).await?;
                }
            }
        }
        let mut waited = false;
        let mut shown = 0;
        loop {
            let seen = state.lock().unwrap().ended;
            self.wait().await?;
            if *aborted.borrow() {
                return Ok(None);
            }
            let (found, messages) = self.projected(message_id).await?;
            let (failed, finished) = {
                let locked = state.lock().unwrap();
                (locked.failure.is_some(), !locked.outcomes.is_empty())
            };
            if failed || finished || messages.iter().any(|m| get_str(m, "type") == Some("idle")) {
                let pending = if failed { None } else { self.background(state, since).await? };
                let Some(pending) = pending else { return Ok(Some((found, messages, waited))) };
                if !pending.is_empty() && pending.len() != shown {
                    state.lock().unwrap().render.waiting(&pending);
                }
                if !pending.is_empty() {
                    shown = pending.len();
                }
                waited = true;
            }
            Self::wait_ended(state, seen).await?;
        }
    }

    /// cancelling: interrupt this prompt's session and jobs until the tree is quiet.
    async fn cancel(&self, state: &Shared, message_id: &str, admission: &Arc<Mutex<Option<f64>>>) {
        state.lock().unwrap().phase = Phase::Cancelling;
        let Some(since) = *admission.lock().unwrap() else { return };
        if !state.lock().unwrap().promoted {
            let _ = self.client.call("session.inbox.cancel", &[("sessionID", &self.session_id), ("inboxID", message_id)], None, None).await;
        }
        let running = self.background(state, since).await.ok().flatten().unwrap_or_default();
        let _ = self.client.call("session.interrupt", &[("sessionID", &self.session_id)], None, None).await;
        for job in &running {
            match get_str(job, "kind") {
                Some("shell") => {
                    let shell = get_str(job, "shellID").unwrap_or("");
                    let _ = self.client.call("shell.remove", &[("id", shell)], Some(location_query(&self.directory)), None).await;
                }
                Some("subagent") => {
                    let child = get_str(job, "childSessionID").unwrap_or("");
                    let _ = self.client.call("session.interrupt", &[("sessionID", child)], None, None).await;
                }
                _ => {}
            }
        }
        loop {
            let seen = state.lock().unwrap().ended;
            if self.wait().await.is_err() {
                return;
            }
            let Ok(pending) = self.background(state, since).await else { return };
            let Some(pending) = pending else { return };
            if !pending.is_empty() && pending.iter().all(|job| get_str(job, "kind") == Some("image")) {
                return;
            }
            if Self::wait_ended(state, seen).await.is_err() {
                return;
            }
        }
    }

    async fn run(self, aborted: tokio::sync::watch::Receiver<bool>) -> std::result::Result<(), String> {
        let prompt = Arc::new(self);
        let mut events = prompt.client.events().await.map_err(message_of)?;
        match events.next().await {
            None => return Err("Event stream disconnected before prompt admission".into()),
            Some(Err(error)) => return Err(error.message()),
            Some(Ok(_)) => {}
        }
        let message_id = format!("msg_{}", crate::id::ascending());
        let state: Shared = Arc::new(Mutex::new(State {
            phase: Phase::Running,
            promoted: false,
            failure: None,
            outcomes: vec![],
            job_sessions: vec![prompt.session_id.clone()],
            ended: 0,
            broken: None,
            owned: HashMap::new(),
            render: Renderer::new(&prompt),
        }));
        let admission = Arc::new(Mutex::new(None));
        let mut watch = aborted.clone();
        let settled = tokio::select! {
            result = prompt.lifecycle(&state, &message_id, &admission, events, &aborted) => result?,
            _ = watch.wait_for(|a| *a) => None,
        };
        let Some((found, messages, background)) = settled else {
            prompt.cancel(&state, &message_id, &admission).await;
            return Ok(());
        };
        let mut locked = state.lock().unwrap();
        locked.phase = Phase::Settled;
        locked.render.reconcile(&messages);
        if let Some(error) = final_error(found, &messages, background, &locked) {
            EXIT.store(1, Ordering::SeqCst);
            locked.render.error(&error);
        }
        Ok(())
    }
}

/// `finalError`: the run's exit status comes from the first stored idle marker after this prompt.
fn final_error(found: bool, messages: &[Json], background: bool, state: &State) -> Option<Json> {
    if let Some(failure) = &state.failure {
        return Some(failure.clone());
    }
    if !found {
        return Some(run_error("unknown", "Prompt was not promoted"));
    }
    let idles: Vec<Outcome> = messages
        .iter()
        .filter(|m| get_str(m, "type") == Some("idle"))
        .map(|m| Outcome {
            id: get_str(m, "id").unwrap_or("").to_owned(),
            outcome: match get_str(m, "outcome") {
                Some("succeeded") => "succeeded",
                Some("failed") => "failed",
                Some("interrupted") => "interrupted",
                _ => "unknown",
            },
            error: get(m, "error").cloned(),
        })
        .collect();
    let recorded = if idles.is_empty() { state.outcomes.clone() } else { idles };
    let turns: Vec<Outcome> = if background { recorded } else { recorded.into_iter().take(1).collect() };
    let last = turns.iter().find(|t| t.outcome != "succeeded").or(turns.first())?;
    if last.outcome == "succeeded" {
        return None;
    }
    let fallback = run_error(if last.outcome == "failed" { "unknown" } else { "aborted" }, &format!("Session {}", last.outcome));
    Some(
        last.error
            .clone()
            .or_else(|| state.outcomes.iter().find(|o| o.id == last.id).and_then(|o| o.error.clone()))
            .unwrap_or(fallback),
    )
}

// --- the renderer ---

struct ToolEntry {
    id: String,
    timestamp: f64,
    name: String,
    input: Json,
    raw: Option<String>,
    call: Option<(Json, Option<Json>)>,
    metadata: Json,
}

struct Renderer {
    session_id: String,
    directory: String,
    json: bool,
    thinking: bool,
    starts: HashMap<String, (String, f64)>,
    tools: HashMap<String, ToolEntry>,
    steps: HashMap<String, bool>,
    printed: HashMap<String, String>,
}

fn key(parts: &[&str]) -> String {
    parts.join("\u{0}")
}

fn part_id(event_id: &str) -> String {
    format!("prt_{}", event_id.strip_prefix("evt_").unwrap_or(event_id))
}

fn projected_part_id(message_id: &str, part: &str) -> String {
    format!("prt_{}_{part}", message_id.strip_prefix("msg_").unwrap_or(message_id))
}

fn number(value: f64) -> Json {
    Json::Number(value)
}

impl Renderer {
    fn new(prompt: &Prompt) -> Self {
        Renderer {
            session_id: prompt.session_id.clone(),
            directory: prompt.directory.clone(),
            json: prompt.json,
            thinking: prompt.thinking,
            starts: HashMap::new(),
            tools: HashMap::new(),
            steps: HashMap::new(),
            printed: HashMap::new(),
        }
    }

    fn emit(&self, kind: &str, timestamp: f64, data: Vec<(&str, Json)>) -> bool {
        if !self.json {
            return false;
        }
        let mut o = Obj::new();
        o.str("type", kind).set("timestamp", number(timestamp)).str("sessionID", &self.session_id);
        for (key, value) in data {
            o.set(key, value);
        }
        stdout(&format!("{}\n", json::text(&o.done())));
        true
    }

    fn block(&self, line: &str) {
        empty();
        println(&[line]);
        empty();
    }

    #[allow(clippy::too_many_arguments)]
    fn text(&mut self, kind: &str, id: &str, message_id: &str, ordinal: i64, text: &str, metadata: Option<Json>, start: f64, end: f64) {
        if kind == "reasoning" && !self.thinking {
            return;
        }
        let printed_key = key(&[kind, message_id, &ordinal.to_string()]);
        let previous = self.printed.get(&printed_key).cloned();
        if let Some(previous) = &previous {
            if previous == text || !text.starts_with(previous.as_str()) {
                return;
            }
        }
        self.printed.insert(printed_key, text.to_owned());
        let value = &text[previous.as_ref().map_or(0, |p| p.len())..];
        let mut part = Obj::new();
        part.str("id", id).str("sessionID", &self.session_id).str("messageID", message_id).str("type", kind).str("text", value);
        if kind == "reasoning" {
            part.opt("metadata", metadata);
        }
        let mut time = Obj::new();
        time.set("start", number(start)).set("end", number(end));
        part.set("time", time.done());
        if self.emit(kind, end, vec![("part", part.done())]) {
            return;
        }
        let trimmed = js_trim(value);
        if trimmed.is_empty() {
            return;
        }
        let line = if kind == "text" { trimmed.to_owned() } else { format!("Thinking: {trimmed}") };
        if !super::stdout_tty() {
            stdout(&format!("{line}\n"));
            return;
        }
        if kind == "text" {
            self.block(&line);
        } else {
            self.block(&format!("{TEXT_DIM}\u{1b}[3m{line}\u{1b}[0m{TEXT_NORMAL}"));
        }
    }

    fn render_tool(&self, name: &str, state: &Json) {
        let info = tool::inline(name, state, &self.directory);
        if info.block {
            empty();
            println(&[&format!("{TEXT_NORMAL}{}", info.icon), &format!("{TEXT_NORMAL}{}", info.title)]);
            if let Some(body) = info.body.filter(|b| !js_trim(b).is_empty()) {
                println(&[&body]);
            }
            empty();
            return;
        }
        let description = info.description.map(|d| format!("{TEXT_DIM}{d}{TEXT_NORMAL}")).unwrap_or_default();
        println(&[&format!("{TEXT_NORMAL}{}", info.icon), &format!("{TEXT_NORMAL}{}", info.title), &description]);
    }

    /// One finished tool: `state` is `{status, input, metadata, content, error?}`; `time` its start and end.
    #[allow(clippy::too_many_arguments)]
    fn tool(&mut self, part_id: &str, message_id: &str, call_id: &str, name: &str, state: &Json, metadata: Json, start: f64, end: f64, timestamp: f64) {
        let printed_key = key(&[message_id, call_id]);
        if self.printed.contains_key(&printed_key) {
            return;
        }
        self.printed.insert(printed_key, String::new());
        let completed = get_str(state, "status") == Some("completed");
        let mut time = Obj::new();
        time.set("start", number(start)).set("end", number(end));
        let mut shown = Obj::new();
        if completed {
            shown
                .str("status", "completed")
                .opt("input", get(state, "input").cloned())
                .str("output", &tool::output_text(name, get(state, "content")))
                .str("title", name)
                .set("metadata", metadata)
                .set("time", time.done());
        } else {
            let message = get(state, "error").and_then(|e| get_str(e, "message")).unwrap_or("");
            shown
                .str("status", "error")
                .opt("input", get(state, "input").cloned())
                .str("error", message)
                .set("metadata", metadata)
                .set("time", time.done());
        }
        let mut part = Obj::new();
        part.str("partID", part_id)
            .str("sessionID", &self.session_id)
            .str("messageID", message_id)
            .str("type", "tool")
            .str("id", call_id)
            .str("tool", name)
            .set("state", shown.done());
        if self.emit("tool_use", timestamp, vec![("part", part.done())]) {
            return;
        }
        if completed {
            self.render_tool(name, state);
            return;
        }
        if !js_trim(&tool::output_text(name, get(state, "content"))).is_empty() {
            let mut as_completed = state.clone();
            json::set(&mut as_completed, "status", Json::String("completed".into()));
            json::remove(&mut as_completed, "error");
            self.render_tool(name, &as_completed);
        }
        let info = tool::inline(name, state, &self.directory);
        println(&[&format!("{TEXT_NORMAL}✗"), &format!("{TEXT_NORMAL}{} failed", info.title)]);
        ui_error(get(state, "error").and_then(|e| get_str(e, "message")).unwrap_or(""));
    }

    fn step(&mut self, kind: &str, timestamp: f64, id: &str, message_id: &str, rest: Vec<(&str, Json)>) -> bool {
        self.steps.insert(message_id.to_owned(), kind == "step-start");
        let mut part = Obj::new();
        part.str("id", id).str("sessionID", &self.session_id).str("messageID", message_id).str("type", kind);
        for (key, value) in rest {
            part.set(key, value);
        }
        self.emit(if kind == "step-start" { "step_start" } else { "step_finish" }, timestamp, vec![("part", part.done())])
    }

    fn error(&self, error: &Json) {
        if !self.emit("error", now() as f64, vec![("error", error.clone())]) {
            ui_error(get_str(error, "message").unwrap_or(""));
        }
    }

    fn waiting(&self, jobs: &[Json]) {
        let list: Vec<Json> = jobs
            .iter()
            .map(|job| {
                let mut o = Obj::new();
                o.opt("id", get(job, "id").cloned()).opt("kind", get(job, "kind").cloned()).opt("title", get(job, "title").cloned());
                o.done()
            })
            .collect();
        if self.emit("background_wait", now() as f64, vec![("jobs", Json::Array(list))]) {
            return;
        }
        let n = jobs.len();
        println(&[&format!("{TEXT_DIM}waiting for {n} background job{}{TEXT_NORMAL}", if n == 1 { "" } else { "s" })]);
    }

    fn event(&mut self, event: &Json) {
        let kind = get_str(event, "type").unwrap_or("");
        let time = json::get_num(event, "created").unwrap_or_else(|| now() as f64);
        let event_id = get_str(event, "id").unwrap_or("");
        let empty_data = Json::Null;
        let data = get(event, "data").unwrap_or(&empty_data);
        let message_id = get_str(data, "assistantMessageID").unwrap_or("").to_owned();
        let data_num = |key: &str| json::get_num(data, key).unwrap_or(0.0) as i64;
        match kind {
            "session.retry.scheduled" => {
                let mut retry = vec![("messageID", Json::String(message_id.clone()))];
                retry.push(("attempt", get(data, "attempt").cloned().unwrap_or(Json::Null)));
                if let Some(at) = get(data, "at") {
                    retry.push(("nextRetryAt", at.clone()));
                }
                retry.push(("error", get(data, "error").cloned().unwrap_or(Json::Null)));
                let attempt = get(data, "attempt").map(json::text).unwrap_or_default();
                let message = get(data, "error").and_then(|e| get_str(e, "message")).unwrap_or("").to_owned();
                if self.emit("retry", time, retry) {
                    return;
                }
                println(&[&format!("{TEXT_WARNING_BOLD}!"), &format!("{TEXT_NORMAL}retrying (attempt {attempt}): {message}")]);
            }
            "session.step.started" if !self.steps.contains_key(&message_id) => {
                let mut rest = vec![];
                if let Some(snapshot) = get(data, "snapshot") {
                    rest.push(("snapshot", snapshot.clone()));
                }
                if !self.step("step-start", time, &part_id(event_id), &message_id, rest) {
                    let agent = get_str(data, "agent").unwrap_or("");
                    let model = get(data, "model").and_then(|m| get_str(m, "id")).unwrap_or("");
                    self.block(&format!("> {agent} · {model}"));
                }
            }
            "session.step.ended" => {
                let mut rest = vec![];
                for (from, to) in [("finish", "reason"), ("snapshot", "snapshot"), ("cost", "cost"), ("tokens", "tokens")] {
                    if let Some(value) = get(data, from) {
                        rest.push((to, value.clone()));
                    }
                }
                self.step("step-finish", time, &part_id(event_id), &message_id, rest);
            }
            "session.text.started" | "session.reasoning.started" => {
                let part = if kind == "session.text.started" { "text" } else { "reasoning" };
                self.starts.insert(key(&[part, &message_id, &data_num("ordinal").to_string()]), (part_id(event_id), time));
            }
            "session.text.ended" | "session.reasoning.ended" => {
                let part = if kind == "session.text.ended" { "text" } else { "reasoning" };
                let ordinal = data_num("ordinal");
                let started = self.starts.get(&key(&[part, &message_id, &ordinal.to_string()])).cloned();
                let metadata = if part == "reasoning" { get(data, "native").cloned() } else { None };
                let text = get_str(data, "text").unwrap_or("").to_owned();
                let (id, start) = started.unwrap_or_else(|| (part_id(event_id), time));
                self.text(part, &id, &message_id, ordinal, &text, metadata, start, time);
            }
            "session.tool.input.started" | "session.tool.called" => {
                let call = get_str(data, "id").unwrap_or("").to_owned();
                let entry_key = key(&[&message_id, &call]);
                let current = self.tools.remove(&entry_key);
                let started = kind == "session.tool.input.started";
                let entry = ToolEntry {
                    id: current.as_ref().map(|c| c.id.clone()).unwrap_or_else(|| part_id(event_id)),
                    timestamp: current.as_ref().map_or(time, |c| c.timestamp),
                    name: if started {
                        get_str(data, "name").unwrap_or("").to_owned()
                    } else {
                        current.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| "tool".into())
                    },
                    input: if started { json::object() } else { get(data, "input").cloned().unwrap_or_else(json::object) },
                    raw: current.as_ref().and_then(|c| c.raw.clone()),
                    call: (!started).then(|| (get(data, "executed").cloned().unwrap_or(Json::Bool(false)), get(data, "native").cloned())),
                    metadata: json::object(),
                };
                self.tools.insert(entry_key, entry);
            }
            "session.tool.input.delta" | "session.tool.input.ended" | "session.tool.progress" => {
                let call = get_str(data, "id").unwrap_or("").to_owned();
                let Some(current) = self.tools.get_mut(&key(&[&message_id, &call])) else { return };
                match kind {
                    "session.tool.input.delta" => {
                        let mut raw = current.raw.take().unwrap_or_default();
                        raw.push_str(get_str(data, "delta").unwrap_or(""));
                        current.raw = Some(raw);
                    }
                    "session.tool.input.ended" => current.raw = get_str(data, "text").map(String::from),
                    _ => current.metadata = get(data, "metadata").cloned().unwrap_or_else(json::object),
                }
            }
            "session.tool.success" | "session.tool.failed" => {
                let call = get_str(data, "id").unwrap_or("").to_owned();
                let current = self.tools.get(&key(&[&message_id, &call]));
                let (id, timestamp, name, input, raw, call_state, progress) = match current {
                    Some(c) => (c.id.clone(), c.timestamp, c.name.clone(), c.input.clone(), c.raw.clone(), c.call.clone(), c.metadata.clone()),
                    None => (part_id(event_id), time, "tool".to_owned(), json::object(), None, None, json::object()),
                };
                let success = kind == "session.tool.success";
                let mut call_info = Obj::new();
                if let Some((executed, native)) = &call_state {
                    let mut provider = Obj::new();
                    provider.set("executed", executed.clone()).opt("state", native.clone());
                    call_info.set("providerCall", provider.done());
                }
                let mut result = Obj::new();
                result.opt("executed", get(data, "executed").cloned()).opt("state", get(data, "native").cloned());
                call_info.set("providerResult", result.done());
                call_info.opt_str("rawInput", raw.as_deref());
                let call_json = call_info.done();
                let metadata = if success {
                    let mut o = Obj::new();
                    o.opt("metadata", get(data, "metadata").cloned()).opt("content", get(data, "content").cloned());
                    if let Json::Object(rest) = &call_json {
                        for (k, v) in rest {
                            o.set(k, v.clone());
                        }
                    }
                    o.done()
                } else {
                    call_json
                };
                let mut state = Obj::new();
                if success {
                    state
                        .str("status", "completed")
                        .set("input", input)
                        .opt("metadata", get(data, "metadata").cloned())
                        .opt("content", get(data, "content").cloned());
                } else {
                    state
                        .str("status", "error")
                        .set("input", input)
                        .set("metadata", get(data, "metadata").cloned().unwrap_or(progress))
                        .opt("content", get(data, "content").cloned())
                        .opt("error", get(data, "error").cloned());
                }
                self.tool(&id, &message_id, &call, &name, &state.done(), metadata, timestamp, time, time);
            }
            _ => {}
        }
    }

    fn reconcile(&mut self, messages: &[Json]) {
        for message in messages {
            if get_str(message, "type") != Some("assistant") {
                continue;
            }
            let id = get_str(message, "id").unwrap_or("").to_owned();
            let time = get(message, "time");
            let created = time.and_then(|t| json::get_num(t, "created")).unwrap_or(0.0);
            let timestamp = time.and_then(|t| json::get_num(t, "completed")).unwrap_or(created);
            let seen = self.steps.get(&id).copied();
            let snapshot = get(message, "snapshot");
            if seen.is_none() {
                let mut rest = vec![];
                if let Some(start) = snapshot.and_then(|s| get(s, "start")) {
                    rest.push(("snapshot", start.clone()));
                }
                self.step("step-start", created, &projected_part_id(&id, "step-start"), &id, rest);
            }
            let mut ordinals: HashMap<&str, i64> = HashMap::new();
            for item in get(message, "content").and_then(json::array).unwrap_or(&[]) {
                let item_type = get_str(item, "type").unwrap_or("");
                if item_type == "tool" {
                    let state = get(item, "state").cloned().unwrap_or(Json::Null);
                    let status = get_str(&state, "status").unwrap_or("");
                    if status == "streaming" || status == "running" {
                        continue;
                    }
                    let call = get_str(item, "id").unwrap_or("").to_owned();
                    let name = get_str(item, "name").unwrap_or("").to_owned();
                    let mut metadata = Obj::new();
                    metadata.opt("metadata", get(&state, "metadata").cloned()).opt("content", get(&state, "content").cloned());
                    let item_time = get(item, "time");
                    let item_created = item_time.and_then(|t| json::get_num(t, "created")).unwrap_or(0.0);
                    let start = item_time.and_then(|t| json::get_num(t, "ran")).unwrap_or(item_created);
                    let end = item_time.and_then(|t| json::get_num(t, "completed")).unwrap_or(timestamp);
                    self.tool(&projected_part_id(&id, &format!("tool-{call}")), &id, &call, &name, &state, metadata.done(), start, end, timestamp);
                    continue;
                }
                if item_type != "text" && item_type != "reasoning" {
                    continue;
                }
                let kind = if item_type == "text" { "text" } else { "reasoning" };
                let counter = ordinals.entry(kind).or_insert(0);
                let ordinal = *counter;
                *counter += 1;
                let text = get_str(item, "text").unwrap_or("").to_owned();
                if text.is_empty() && !self.printed.contains_key(&key(&[kind, &id, &ordinal.to_string()])) {
                    continue;
                }
                let metadata = if kind == "reasoning" { get(item, "native").cloned() } else { None };
                self.text(kind, &projected_part_id(&id, &format!("{kind}-{ordinal}")), &id, ordinal, &text, metadata, created, timestamp);
            }
            let finish = get(message, "finish");
            let tokens = get(message, "tokens");
            if seen != Some(false) {
                if let (Some(finish), Some(tokens)) = (finish, tokens) {
                    let mut rest = vec![("reason", finish.clone())];
                    if let Some(end) = snapshot.and_then(|s| get(s, "end")) {
                        rest.push(("snapshot", end.clone()));
                    }
                    rest.push(("cost", get(message, "cost").cloned().unwrap_or(Json::Number(0.0))));
                    rest.push(("tokens", tokens.clone()));
                    self.step("step-finish", timestamp, &projected_part_id(&id, "step-finish"), &id, rest);
                }
            }
        }
    }
}
