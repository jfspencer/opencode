//! `opencode acp` (packages/cli/src/commands/handlers/acp.ts, acp/*.ts): the Agent Client Protocol over stdio,
//! newline-delimited JSON-RPC 2.0, against a private server. Sessions (new, load with its history replayed,
//! resume, fork, list, close, delete, additional directories), prompts (text, resources, images, slash commands,
//! compaction), the turn's events translated to `session/update` notifications (with child sessions' updates,
//! retries, compactions, completed-edit diffs and usage), permission requests and forms asked of the client,
//! cancellation, and config options (model, effort, mode) that follow the server's catalog and selections.
//!
//! Out of scope: the MCP servers a client names are checked but not registered (`experimental.mcp.add`), since MCP
//! is dropped from the port; the SDK's schema validation of request parameters (zod's error format) is not
//! reproduced.

mod elicitation;
mod permission;
mod prompt;
mod replay;
mod tool;
mod translate;
mod turn;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use opencode_protocol::Json;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::client::{Client, ClientError};
use super::connection;
use super::out::Result;
use super::parse::Input;
use crate::json::{self, Obj, get, get_num, get_str};

const AUTH_METHOD: &str = "opencode-login";
const DIRECTORIES_KEY: &str = "opencode.acp.additionalDirectories";

/// A JSON-RPC error (`RequestError`).
#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
    data: Option<Json>,
}

impl RpcError {
    fn invalid_params(data: Json, message: &str) -> Self {
        RpcError { code: -32602, message: format!("Invalid params: {message}"), data: Some(data) }
    }
    fn internal(data: Json, message: &str) -> Self {
        RpcError { code: -32603, message: format!("Internal error: {message}"), data: Some(data) }
    }
    fn method_not_found(method: &str) -> Self {
        let mut data = Obj::new();
        data.str("method", method);
        RpcError { code: -32601, message: format!("\"Method not found\": {method}"), data: Some(data.done()) }
    }
    fn session_not_found(id: &str) -> Self {
        let mut data = Obj::new();
        data.str("sessionId", id);
        Self::invalid_params(data.done(), &format!("session not found: {id}"))
    }
    fn invalid_request(message: &str, field: Option<&str>) -> Self {
        let mut data = Obj::new();
        data.opt_str("field", field);
        Self::invalid_params(data.done(), message)
    }
    fn unavailable() -> Self {
        let mut data = Obj::new();
        data.str("errorName", "ServerUnavailable");
        Self::internal(data.done(), "OpenCode server is unavailable")
    }
    fn service(message: &str, service: Option<&str>, name: Option<&str>) -> Self {
        let mut data = Obj::new();
        data.opt_str("service", service).opt_str("errorName", name);
        Self::internal(data.done(), message)
    }
    fn invalid(key: &str, value: &str, message: String) -> Self {
        let mut data = Obj::new();
        data.str(key, value);
        Self::invalid_params(data.done(), &message)
    }
    /// The `ACPInvalidModelError`, `ACPInvalidModeError` and `ACPInvalidEffortError` a catalog reload may fix.
    fn reloadable(&self) -> bool {
        self.code == -32602 && ["model not found: ", "mode not found: ", "effort not found: "].iter().any(|p| self.message.starts_with(&format!("Invalid params: {p}")))
    }
}

/// `ACPClient.classify` for a failed server call.
fn classify(error: ClientError) -> RpcError {
    match &error {
        ClientError::Transport(_) | ClientError::MalformedResponse => RpcError::unavailable(),
        ClientError::Declared { body, .. } => match get_str(body, "_tag") {
            Some("SessionNotFoundError") => RpcError::session_not_found(get_str(body, "sessionID").unwrap_or("")),
            Some("InvalidRequestError") => RpcError::invalid_request(&error.message(), get_str(body, "field")),
            Some("InvalidCursorError") => RpcError::invalid_request(&error.message(), Some("cursor")),
            other => RpcError::service("Internal service failure", None, other.or(Some("Error"))),
        },
        _ => RpcError::service("Internal service failure", None, Some("ClientError")),
    }
}

type Reply = std::result::Result<Json, RpcError>;

/// The connection to the client: one message per line on stdout, and the requests the agent sends the client.
struct Connection {
    write: Mutex<()>,
    next: AtomicI64,
    /// Requests awaiting the client's answer; a withdrawn one stays (as `None`) until its late answer arrives.
    pending: Mutex<HashMap<i64, Option<oneshot::Sender<std::result::Result<Json, Json>>>>>,
    /// The client's requests that `$/cancel_request` can cancel, by their ID's JSON text.
    incoming: Mutex<HashMap<String, watch::Sender<bool>>>,
}

impl Connection {
    fn send(&self, message: &Json) {
        let _guard = self.write.lock().unwrap();
        super::out::stdout(&format!("{}\n", json::text(message)));
    }
    fn notify(&self, method: &str, params: Json) {
        let mut o = Obj::new();
        o.str("jsonrpc", "2.0").str("method", method).set("params", params);
        self.send(&o.done());
    }
    fn update(&self, session: &str, update: Json) {
        let mut params = Obj::new();
        params.str("sessionId", session).set("update", update);
        self.notify("session/update", params.done());
    }
    fn respond(&self, id: &Json, reply: Reply) {
        let mut o = Obj::new();
        o.str("jsonrpc", "2.0").set("id", id.clone());
        match reply {
            Ok(result) => {
                o.set("result", result);
            }
            Err(error) => {
                let mut e = Obj::new();
                e.int("code", error.code).str("message", &error.message).opt("data", error.data);
                o.set("error", e.done());
            }
        }
        self.send(&o.done());
    }

    /// A request to the client; its answer's result or error. Dropped before the answer, it is withdrawn.
    async fn request(self: &Arc<Self>, method: &str, params: Json) -> std::result::Result<Json, Json> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, Some(tx));
        let mut withdraw = Withdraw { conn: self.clone(), id, answered: false };
        let mut o = Obj::new();
        o.str("jsonrpc", "2.0").int("id", id).str("method", method).set("params", params);
        self.send(&o.done());
        let answer = rx.await.unwrap_or(Err(Json::Null));
        withdraw.answered = true;
        answer
    }

    /// An answer from the client to one of the agent's requests.
    fn answered(&self, message: &Json) {
        let id = match get(message, "id") {
            Some(Json::Number(n)) => *n as i64,
            Some(other) => {
                super::out::stderr(&format!("Got response to unknown request {}\n", json::text(other)));
                return;
            }
            None => return,
        };
        let entry = self.pending.lock().unwrap().remove(&id);
        match entry {
            Some(Some(tx)) => {
                let answer = match (get(message, "result"), get(message, "error")) {
                    (Some(result), _) => Ok(result.clone()),
                    (None, Some(error)) => Err(error.clone()),
                    (None, None) => Err(Json::Null),
                };
                let _ = tx.send(answer);
            }
            Some(None) => {}
            None => super::out::stderr(&format!("Got response to unknown request {id}\n")),
        }
    }
}

struct Withdraw {
    conn: Arc<Connection>,
    id: i64,
    answered: bool,
}

impl Drop for Withdraw {
    fn drop(&mut self) {
        if self.answered {
            return;
        }
        if let Some(entry) = self.conn.pending.lock().unwrap().get_mut(&self.id) {
            *entry = None;
        }
        let mut params = Obj::new();
        params.int("requestId", self.id);
        self.conn.notify("$/cancel_request", params.done());
    }
}

/// `ACPCapabilities`: what the client said it supports.
#[derive(Clone, Copy, Default)]
struct Capabilities {
    child_updates: bool,
    form_elicitation: bool,
    compaction: bool,
}

/// The models, modes and commands of a directory (`ACPCatalog.Catalog`).
#[derive(Clone, PartialEq)]
struct Catalog {
    models: Vec<Json>,
    default_model: Json,
    modes: Vec<Json>,
    default_mode: String,
    commands: Vec<Json>,
}

#[derive(Clone, Default)]
struct Selection {
    model: Option<Json>,
    mode: Option<String>,
}

impl Selection {
    fn patched(&self, patch: &Selection) -> Selection {
        Selection { model: patch.model.clone().or_else(|| self.model.clone()), mode: patch.mode.clone().or_else(|| self.mode.clone()) }
    }
}

/// How an attached session follows its catalog and the server's selections (`ACPSessions.attach`'s fold): from
/// the response that attached it, until it is detached.
#[derive(Default)]
struct Follow {
    active: bool,
    catalog: Option<Catalog>,
    queued: Vec<Selection>,
}

struct Attached {
    id: String,
    cwd: String,
    selection: Mutex<Selection>,
    follow: Mutex<Follow>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Attached {
    fn stop(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

struct TurnEntry {
    token: u64,
    cancel: watch::Sender<bool>,
    done: watch::Receiver<bool>,
}

struct Agent {
    client: Client,
    conn: Arc<Connection>,
    catalogs: tokio::sync::Mutex<HashMap<String, Catalog>>,
    sessions: Mutex<HashMap<String, Arc<Attached>>>,
    turns: Mutex<HashMap<String, TurnEntry>>,
    next_turn: AtomicI64,
    unprompted: Mutex<Vec<String>>,
    capabilities: Mutex<Capabilities>,
    connected: watch::Receiver<bool>,
}

fn location(directory: &str) -> Json {
    let mut inner = Obj::new();
    inner.str("directory", directory);
    let mut o = Obj::new();
    o.set("location", inner.done());
    o.done()
}

fn data(value: &Json) -> Vec<Json> {
    get(value, "data").and_then(json::array).map(|a| a.to_vec()).unwrap_or_default()
}

fn find_model<'a>(models: &'a [Json], reference: &Json) -> Option<&'a Json> {
    models.iter().find(|m| get_str(m, "providerID") == get_str(reference, "providerID") && get_str(m, "id") == get_str(reference, "id"))
}

fn variants(model: Option<&Json>) -> Vec<String> {
    model
        .and_then(|m| get(m, "variants"))
        .and_then(json::array)
        .unwrap_or(&[])
        .iter()
        .filter_map(|v| get_str(v, "id").map(String::from))
        .collect()
}

fn advertised(model: &Json) -> String {
    format!("{}/{}", get_str(model, "providerID").unwrap_or(""), get_str(model, "id").unwrap_or(""))
}

impl Catalog {
    fn current(&self, selection: &Selection) -> Json {
        selection.model.clone().unwrap_or_else(|| self.default_model.clone())
    }

    /// `configOptions`.
    fn options(&self, selection: &Selection) -> Json {
        let model = self.current(selection);
        let available = variants(find_model(&self.models, &model));
        let mut models = self.models.clone();
        models.sort_by(|a, b| {
            get_str(a, "providerID")
                .unwrap_or("")
                .cmp(get_str(b, "providerID").unwrap_or(""))
                .then_with(|| crate::collate::compare(get_str(a, "name").unwrap_or(""), get_str(b, "name").unwrap_or("")))
        });
        let mut list = vec![];
        let mut option = Obj::new();
        option
            .str("id", "model")
            .str("name", "Model")
            .str("category", "model")
            .str("type", "select")
            .str("currentValue", &advertised(&model))
            .set(
                "options",
                Json::Array(
                    models
                        .iter()
                        .map(|m| {
                            let mut o = Obj::new();
                            o.str("value", &advertised(m))
                                .str("name", &format!("{}/{}", get_str(m, "providerID").unwrap_or(""), get_str(m, "name").unwrap_or("")));
                            o.done()
                        })
                        .collect(),
                ),
            );
        list.push(option.done());
        if !available.is_empty() {
            let mut values: Vec<String> = vec![];
            for v in available.iter().cloned().chain(std::iter::once("default".to_owned())) {
                if !values.contains(&v) {
                    values.push(v);
                }
            }
            let current = select_variant(get_str(&model, "variant"), &available);
            let mut option = Obj::new();
            option
                .str("id", "effort")
                .str("name", "Effort")
                .str("description", "Available effort levels for this model")
                .str("category", "thought_level")
                .str("type", "select")
                .str("currentValue", &current)
                .set(
                    "options",
                    Json::Array(
                        values
                            .iter()
                            .map(|v| {
                                let name = v
                                    .split(['_', '-'])
                                    .map(|part| {
                                        let mut chars = part.chars();
                                        match chars.next() {
                                            Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                                            None => String::new(),
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                let mut o = Obj::new();
                                o.str("value", v).str("name", &name);
                                o.done()
                            })
                            .collect(),
                    ),
                );
            list.push(option.done());
        }
        let mut option = Obj::new();
        option
            .str("id", "mode")
            .str("name", "Session Mode")
            .str("category", "mode")
            .str("type", "select")
            .str("currentValue", selection.mode.as_deref().unwrap_or(&self.default_mode))
            .set(
                "options",
                Json::Array(
                    self.modes
                        .iter()
                        .map(|mode| {
                            let mut o = Obj::new();
                            o.opt("value", get(mode, "id").cloned()).opt("name", get(mode, "name").cloned());
                            if let Some(description) = get_str(mode, "description").filter(|d| !d.is_empty()) {
                                o.str("description", description);
                            }
                            o.done()
                        })
                        .collect(),
                ),
            );
        list.push(option.done());
        Json::Array(list)
    }

    fn available_commands(&self) -> Json {
        let mut list: Vec<Json> = self
            .commands
            .iter()
            .map(|c| {
                let mut o = Obj::new();
                o.opt("name", get(c, "name").cloned()).str("description", get_str(c, "description").unwrap_or(""));
                o.done()
            })
            .collect();
        let mut compact = Obj::new();
        compact.str("name", "compact").str("description", "Compact the session");
        list.push(compact.done());
        Json::Array(list)
    }
}

fn select_variant(variant: Option<&str>, variants: &[String]) -> String {
    match variant {
        None | Some("default") | Some("") => "default".into(),
        Some(v) if variants.iter().any(|x| x == v) => v.into(),
        _ if variants.iter().any(|x| x == "default") => "default".into(),
        _ => variants.first().cloned().unwrap_or_else(|| "default".into()),
    }
}

fn parse_cwd(cwd: &str) -> std::result::Result<(), RpcError> {
    if !cwd.starts_with('/') {
        return Err(RpcError::invalid_request(&format!("cwd must be an absolute path: {cwd}"), Some("cwd")));
    }
    Ok(())
}

/// `ACPDirectories.parse`: the additional directories, absolute and without glob characters, other than the cwd.
fn parse_directories(cwd: &str, params: &Json) -> std::result::Result<Vec<String>, RpcError> {
    parse_cwd(cwd)?;
    let directories: Vec<&str> = get(params, "additionalDirectories").and_then(json::array).unwrap_or(&[]).iter().map(|d| json::as_str(d).unwrap_or("")).collect();
    if let Some(invalid) = directories.iter().find(|d| !d.starts_with('/') || d.contains(['*', '?'])) {
        let mut data = Obj::new();
        data.str("additionalDirectory", invalid);
        return Err(RpcError::invalid_params(data.done(), &format!("additional directory must be an absolute path without glob characters: {invalid}")));
    }
    let root = crate::project::resolve_path(cwd);
    let mut out: Vec<String> = vec![];
    for directory in directories {
        let resolved = crate::project::absolutize(directory);
        if !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    Ok(out.into_iter().filter(|d| crate::project::resolve_path(d) != root).collect())
}

/// `rules`: tools compare the written path without following symlinks, so both spellings of a root are granted.
fn directory_rules(directories: &[String]) -> Vec<Json> {
    let mut roots: Vec<String> = vec![];
    for directory in directories {
        for root in [directory.clone(), crate::project::resolve_path(directory)] {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
    }
    roots
        .iter()
        .map(|d| {
            let mut o = Obj::new();
            o.str("action", "external_directory").str("resource", &crate::project::normalize(&format!("{d}/*"))).str("effect", "allow");
            o.done()
        })
        .collect()
}

fn directory_list(directories: &[String]) -> Json {
    Json::Array(directories.iter().map(|d| Json::String(d.clone())).collect())
}

/// `ACPDirectories.grant`.
fn grant(directories: &[String], body: &mut Obj) {
    if directories.is_empty() {
        return;
    }
    let mut metadata = Obj::new();
    metadata.set(DIRECTORIES_KEY, directory_list(directories));
    body.set("permissions", Json::Array(directory_rules(directories))).set("metadata", metadata.done());
}

/// `ACPDirectories.list`: the directories a session was granted.
fn granted(session: &Json) -> Vec<String> {
    match get(session, "metadata").and_then(|m| get(m, DIRECTORIES_KEY)) {
        Some(Json::Array(items)) if items.iter().all(|i| matches!(i, Json::String(_))) => items.iter().filter_map(json::as_str).map(String::from).collect(),
        _ => vec![],
    }
}

fn check_mcp(params: &Json) -> std::result::Result<(), RpcError> {
    let servers = get(params, "mcpServers").and_then(json::array).unwrap_or(&[]);
    if servers.iter().any(|s| get_str(s, "type").is_some_and(|t| t != "http")) {
        return Err(RpcError::invalid_request("Only stdio and HTTP MCP servers are supported", Some("mcpServers")));
    }
    Ok(())
}

impl Agent {
    fn capabilities(&self) -> Capabilities {
        *self.capabilities.lock().unwrap()
    }

    /// Waits for the agent's own event stream, so no selection or catalog event is missed.
    async fn connected(&self) {
        let mut connected = self.connected.clone();
        let _ = connected.wait_for(|c| *c).await;
    }

    async fn catalog(&self, cwd: &str) -> std::result::Result<Catalog, RpcError> {
        let key = crate::project::resolve_path(cwd);
        let mut catalogs = self.catalogs.lock().await;
        if let Some(found) = catalogs.get(&key) {
            return Ok(found.clone());
        }
        self.connected().await;
        let loaded = self.load_catalog(cwd).await?;
        catalogs.insert(key, loaded.clone());
        Ok(loaded)
    }

    /// `ACPCatalog.reload`: the directory's catalog read again; attached sessions follow the change.
    async fn reload(&self, cwd: &str) {
        let key = crate::project::resolve_path(cwd);
        let next = {
            let mut catalogs = self.catalogs.lock().await;
            if !catalogs.contains_key(&key) {
                return;
            }
            let Ok(next) = self.load_catalog(cwd).await else { return };
            catalogs.insert(key.clone(), next.clone());
            next
        };
        let attached: Vec<Arc<Attached>> =
            self.sessions.lock().unwrap().values().filter(|a| crate::project::resolve_path(&a.cwd) == key).cloned().collect();
        for session in attached {
            let previous = {
                let mut follow = session.follow.lock().unwrap();
                if !follow.active {
                    continue;
                }
                follow.catalog.replace(next.clone())
            };
            if let Some(previous) = previous {
                self.changed(&session, &previous, &next, &Selection::default());
            }
        }
    }

    async fn load_catalog(&self, cwd: &str) -> std::result::Result<Catalog, RpcError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match self.client.call("plugin.list", &[], Some(location(cwd)), None).await {
                Ok(plugins) if !data(&plugins).is_empty() => break,
                _ if std::time::Instant::now() >= deadline => break,
                _ => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
            }
        }
        loop {
            let models = self.client.call("model.list", &[], Some(location(cwd)), None).await.map_err(classify)?;
            let preferred = self.client.call("model.default", &[], Some(location(cwd)), None).await.map_err(classify)?;
            let agents = self.client.call("agent.list", &[], Some(location(cwd)), None).await.map_err(classify)?;
            let commands = self.client.call("command.list", &[], Some(location(cwd)), None).await.map_err(classify)?;
            let models: Vec<Json> = data(&models).into_iter().filter(|m| json::get_bool(m, "enabled") == Some(true)).collect();
            let preferred = get(&preferred, "data").filter(|d| !matches!(d, Json::Null));
            let default = match preferred {
                Some(reference) => find_model(&models, reference).cloned(),
                None => models.first().cloned(),
            };
            let modes: Vec<Json> = data(&agents)
                .into_iter()
                .filter(|a| get_str(a, "mode") != Some("subagent") && json::get_bool(a, "hidden") != Some(true))
                .map(|a| {
                    let mut o = Obj::new();
                    o.opt("id", get(&a, "id").cloned()).opt("name", get(&a, "name").cloned()).opt("description", get(&a, "description").cloned());
                    o.done()
                })
                .collect();
            let not_ready = match (&default, modes.first()) {
                (None, _) => Some("No models are available"),
                (_, None) => Some("No primary agents are available"),
                _ => None,
            };
            if let Some(message) = not_ready {
                if std::time::Instant::now() >= deadline {
                    return Err(RpcError::service(message, None, Some("CatalogNotReady")));
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                continue;
            }
            let default = default.unwrap();
            let mut reference = Obj::new();
            reference.opt("providerID", get(&default, "providerID").cloned()).opt("id", get(&default, "id").cloned());
            if variants(Some(&default)).iter().any(|v| v == "default") {
                reference.str("variant", "default");
            }
            return Ok(Catalog {
                default_mode: get_str(&modes[0], "id").unwrap_or("").to_owned(),
                models,
                default_model: reference.done(),
                modes,
                commands: data(&commands).into_iter().filter(|c| get_str(c, "name") != Some("compact")).collect(),
            });
        }
    }

    fn attached(&self, id: &str) -> std::result::Result<Arc<Attached>, RpcError> {
        self.sessions.lock().unwrap().get(id).cloned().ok_or_else(|| RpcError::session_not_found(id))
    }

    fn current(&self, attached: &Arc<Attached>) -> bool {
        self.sessions.lock().unwrap().get(&attached.id).is_some_and(|a| Arc::ptr_eq(a, attached))
    }

    /// `ACPSessions.attach`: the session's selection; after the response, its commands and the changes it follows.
    async fn attach(&self, session: &Json, cwd: &str) -> std::result::Result<(Arc<Attached>, Catalog, Json), RpcError> {
        self.connected().await;
        let catalog = self.catalog(cwd).await?;
        let id = get_str(session, "id").unwrap_or("").to_owned();
        let selection = Selection { model: get(session, "model").cloned(), mode: get_str(session, "agent").map(String::from) };
        let options = catalog.options(&selection);
        let attached = Arc::new(Attached {
            id: id.clone(),
            cwd: cwd.to_owned(),
            selection: Mutex::new(selection),
            follow: Mutex::new(Follow::default()),
            tasks: Mutex::new(vec![]),
        });
        let replaced = self.sessions.lock().unwrap().insert(id, attached.clone());
        if let Some(replaced) = replaced {
            replaced.stop();
        }
        Ok((attached, catalog, options))
    }

    /// After a successful response that attached a session: its commands, then the changes since attaching.
    async fn activate(&self, attached: &Arc<Attached>, catalog: Catalog) {
        if !self.current(attached) {
            return;
        }
        self.conn.update(&attached.id, commands_update(catalog.available_commands()));
        let latest = self.catalog(&attached.cwd).await.unwrap_or_else(|_| catalog.clone());
        let queued = {
            let mut follow = attached.follow.lock().unwrap();
            follow.active = true;
            follow.catalog = Some(latest.clone());
            std::mem::take(&mut follow.queued)
        };
        if latest != catalog {
            self.changed(attached, &catalog, &latest, &Selection::default());
        }
        for patch in queued {
            self.changed(attached, &latest, &latest, &patch);
        }
    }

    /// `changed`: a selection or catalog change the client is told about when it shows.
    fn changed(&self, attached: &Attached, previous: &Catalog, next: &Catalog, patch: &Selection) {
        let selection = {
            let mut current = attached.selection.lock().unwrap();
            let before = current.clone();
            *current = before.patched(patch);
            before
        };
        let options = next.options(&selection.patched(patch));
        if options != previous.options(&selection) {
            let mut update = Obj::new();
            update.str("sessionUpdate", "config_option_update").set("configOptions", options);
            self.conn.update(&attached.id, update.done());
        }
        if next.commands != previous.commands {
            self.conn.update(&attached.id, commands_update(next.available_commands()));
        }
    }

    /// A selection the server reports for an attached session (`session.model.selected`, `session.agent.selected`).
    fn selected(&self, session: &str, patch: Selection) {
        let Some(attached) = self.sessions.lock().unwrap().get(session).cloned() else { return };
        let catalog = {
            let mut follow = attached.follow.lock().unwrap();
            if !follow.active {
                follow.queued.push(patch);
                return;
            }
            follow.catalog.clone()
        };
        if let Some(catalog) = catalog {
            self.changed(&attached, &catalog, &catalog, &patch);
        }
    }

    fn detach(&self, id: &str) {
        if let Some(attached) = self.sessions.lock().unwrap().remove(id) {
            attached.stop();
        }
    }

    /// `ACPSessions.release`: only this attachment, not one that replaced it.
    fn release(&self, attached: &Arc<Attached>) {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.get(&attached.id).is_some_and(|a| Arc::ptr_eq(a, attached)) {
            sessions.remove(&attached.id);
            attached.stop();
        }
    }

    /// `ACPSessions.fork`: work that lives as long as the attachment.
    fn fork(&self, attached: &Arc<Attached>, work: impl std::future::Future<Output = ()> + Send + 'static) {
        if !self.current(attached) {
            return;
        }
        attached.tasks.lock().unwrap().push(tokio::spawn(work));
    }

    async fn get_session(&self, id: &str, cwd: &str) -> std::result::Result<Json, RpcError> {
        if !id.starts_with("ses") {
            return Err(RpcError::invalid_request("Invalid session ID", Some("sessionID")));
        }
        let found = self.client.call("session.get", &[("sessionID", id)], None, None).await.map_err(classify)?;
        let session = get(&found, "data").cloned().unwrap_or(Json::Null);
        let directory = get(&session, "location").and_then(|l| get_str(l, "directory")).unwrap_or("");
        if crate::project::resolve_path(cwd) != crate::project::resolve_path(directory) {
            let mut data = Obj::new();
            data.str("sessionId", id).str("cwd", cwd);
            return Err(RpcError::invalid_params(data.done(), &format!("session {id} does not belong to cwd: {cwd}")));
        }
        Ok(session)
    }

    /// `ACPDirectories.activate`: the request's directories replace the ones the session was granted before.
    async fn activate_directories(&self, session: &Json, directories: &[String]) -> std::result::Result<(), RpcError> {
        let previous = granted(session);
        let owned = directory_rules(&previous);
        let current: Vec<Json> = get(session, "permissions").and_then(json::array).map(<[Json]>::to_vec).unwrap_or_default();
        let permissions: Vec<Json> = directory_rules(directories).into_iter().chain(current.iter().filter(|rule| !owned.contains(rule)).cloned()).collect();
        let permissions_changed = permissions != current;
        let metadata_changed = previous != directories;
        if !permissions_changed && !metadata_changed {
            return Ok(());
        }
        let mut body = Obj::new();
        if permissions_changed {
            body.set("permissions", Json::Array(permissions));
        }
        if metadata_changed {
            let mut metadata = Obj::new();
            if let Some(Json::Object(entries)) = get(session, "metadata") {
                for (key, value) in entries.iter().filter(|(key, _)| key.as_str() != DIRECTORIES_KEY) {
                    metadata.set(key, value.clone());
                }
            }
            if !directories.is_empty() {
                metadata.set(DIRECTORIES_KEY, directory_list(directories));
            }
            body.set("metadata", metadata.done());
        }
        let id = get_str(session, "id").unwrap_or("");
        self.client.call("session.update", &[("sessionID", id)], None, Some(body.done())).await.map_err(classify)?;
        Ok(())
    }

    /// One request; `After` work runs once its successful response is written.
    async fn request(self: &Arc<Self>, method: &str, params: &Json, cancelled: watch::Receiver<bool>) -> (Reply, Option<After>) {
        let cwd = get_str(params, "cwd").unwrap_or("").to_owned();
        let session_id = get_str(params, "sessionId").unwrap_or("").to_owned();
        let mut after = None;
        let reply: Reply = async {
            match method {
                "initialize" => {
                    let client = get(params, "clientCapabilities");
                    let meta = client.and_then(|c| get(c, "_meta"));
                    let present = |value: Option<&Json>| value.is_some_and(|v| !matches!(v, Json::Null));
                    *self.capabilities.lock().unwrap() = Capabilities {
                        child_updates: meta.and_then(|m| json::get_bool(m, translate_child_capability())) == Some(true),
                        form_elicitation: present(client.and_then(|c| get(c, "elicitation")).and_then(|e| get(e, "form"))),
                        compaction: present(client.and_then(|c| get(c, "session")).and_then(|s| get(s, "compaction"))),
                    };
                    Ok(initialize(params))
                }
                "authenticate" => {
                    let method = get_str(params, "methodId").unwrap_or("");
                    if method != AUTH_METHOD {
                        let mut data = Obj::new();
                        data.str("methodId", method);
                        return Err(RpcError::invalid_params(data.done(), &format!("unknown auth method: {method}")));
                    }
                    Ok(json::object())
                }
                "session/new" => {
                    let directories = parse_directories(&cwd, params)?;
                    check_mcp(params)?;
                    // Load first so a catalog failure leaves no session.
                    self.catalog(&cwd).await?;
                    let mut body = Obj::new();
                    let mut at = Obj::new();
                    at.str("directory", &cwd);
                    body.set("location", at.done());
                    grant(&directories, &mut body);
                    let created = self.client.call("session.create", &[], None, Some(body.done())).await.map_err(classify)?;
                    let session = get(&created, "data").cloned().unwrap_or(Json::Null);
                    let (attached, catalog, options) = self.attach(&session, &cwd).await?;
                    self.unprompted.lock().unwrap().push(attached.id.clone());
                    let mut o = Obj::new();
                    o.str("sessionId", &attached.id).set("configOptions", options);
                    after = Some(After::Activate(attached, catalog));
                    Ok(o.done())
                }
                "session/load" | "session/resume" => {
                    let directories = parse_directories(&cwd, params)?;
                    check_mcp(params)?;
                    let session = self.get_session(&session_id, &cwd).await?;
                    self.activate_directories(&session, &directories).await?;
                    let directory = get(&session, "location").and_then(|l| get_str(l, "directory")).unwrap_or(&cwd).to_owned();
                    let (attached, catalog, options) = self.attach(&session, &directory).await?;
                    after = Some(After::Activate(attached.clone(), catalog));
                    let mut o = Obj::new();
                    if method == "session/resume" {
                        o.set("configOptions", options);
                        return Ok(o.done());
                    }
                    if let Err(error) = replay::history(self, &attached.id, &attached.cwd, self.capabilities().compaction).await {
                        self.release(&attached);
                        return Err(error);
                    }
                    let catalog = self.catalog(&attached.cwd).await?;
                    let selection = attached.selection.lock().unwrap().clone();
                    o.set("configOptions", catalog.options(&selection));
                    Ok(o.done())
                }
                "session/fork" => {
                    let directories = parse_directories(&cwd, params)?;
                    check_mcp(params)?;
                    let session = self.get_session(&session_id, &cwd).await?;
                    let id = get_str(&session, "id").unwrap_or("").to_owned();
                    let forked = self.client.call("session.fork", &[("sessionID", &id)], None, Some(json::object())).await.map_err(classify)?;
                    let forked = get(&forked, "data").cloned().unwrap_or(Json::Null);
                    // Forks inherit the source's grants; replace them with this request's.
                    self.activate_directories(&forked, &directories).await?;
                    let directory = get(&forked, "location").and_then(|l| get_str(l, "directory")).unwrap_or(&cwd).to_owned();
                    let (attached, catalog, options) = self.attach(&forked, &directory).await?;
                    let mut o = Obj::new();
                    o.str("sessionId", &attached.id).set("configOptions", options);
                    after = Some(After::Activate(attached, catalog));
                    Ok(o.done())
                }
                "session/list" => self.list(params).await,
                "session/delete" => {
                    self.unprompted.lock().unwrap().retain(|s| *s != session_id);
                    self.cancel(&session_id).await;
                    if session_id.starts_with("ses") {
                        match self.client.call("session.remove", &[("sessionID", &session_id)], None, None).await {
                            Ok(_) => {}
                            Err(error) if error.tag() == Some("SessionNotFoundError") => {}
                            Err(error) => return Err(classify(error)),
                        }
                    }
                    self.detach(&session_id);
                    Ok(json::object())
                }
                "session/close" => {
                    if !self.cancel(&session_id).await && session_id.starts_with("ses") {
                        match self.client.call("session.interrupt", &[("sessionID", &session_id)], None, None).await {
                            Ok(_) => {}
                            Err(error) if error.tag() == Some("SessionNotFoundError") => {}
                            Err(error) => return Err(classify(error)),
                        }
                    }
                    self.detach(&session_id);
                    self.discard_unprompted(&session_id).await;
                    Ok(json::object())
                }
                "session/set_config_option" | "session/set_mode" => {
                    let attached = self.attached(&session_id)?;
                    let (config, value) = if method == "session/set_mode" {
                        ("mode".to_owned(), get_str(params, "modeId").map(String::from))
                    } else {
                        (get_str(params, "configId").unwrap_or("").to_owned(), get_str(params, "value").map(String::from))
                    };
                    let Some(value) = value else {
                        let mut data = Obj::new();
                        data.str("configId", &config);
                        return Err(RpcError::invalid_params(data.done(), &format!("unknown config option: {config}")));
                    };
                    // A catalog reload may know the model, mode or effort.
                    match self.select(&attached, &config, &value).await {
                        Err(error) if error.reloadable() => {
                            self.reload(&attached.cwd).await;
                            self.select(&attached, &config, &value).await?;
                        }
                        other => other?,
                    }
                    if method == "session/set_mode" {
                        return Ok(json::object());
                    }
                    let catalog = self.catalog(&attached.cwd).await?;
                    let selection = attached.selection.lock().unwrap().clone();
                    let mut o = Obj::new();
                    o.set("configOptions", catalog.options(&selection));
                    Ok(o.done())
                }
                "session/prompt" => {
                    self.unprompted.lock().unwrap().retain(|s| *s != session_id);
                    self.prompt(params, cancelled).await
                }
                _ => Err(RpcError::method_not_found(method)),
            }
        }
        .await;
        (reply, after)
    }

    /// `discardUnprompted`: clients often open a session and close it without prompting; closing removes those.
    async fn discard_unprompted(&self, session_id: &str) {
        let had = {
            let mut list = self.unprompted.lock().unwrap();
            let had = list.iter().any(|s| s == session_id);
            list.retain(|s| s != session_id);
            had
        };
        if !had || !session_id.starts_with("ses") {
            return;
        }
        let mut query = Obj::new();
        query.int("limit", 1);
        // Another client of the same server may have used it.
        if let Ok(messages) = self.client.call("session.message.list", &[("sessionID", session_id)], Some(query.done()), None).await {
            if data(&messages).is_empty() {
                let _ = self.client.call("session.remove", &[("sessionID", session_id)], None, None).await;
            }
        }
    }

    async fn list(&self, params: &Json) -> Reply {
        let mut query = Obj::new();
        if let Some(cwd) = get_str(params, "cwd") {
            parse_cwd(cwd)?;
            query.str("directory", cwd);
        }
        query.str("order", "desc").int("limit", 100);
        if let Some(cursor) = get_str(params, "cursor").filter(|c| !c.is_empty()) {
            query.str("cursor", cursor);
        }
        let page = self.client.call("session.list", &[], Some(query.done()), None).await.map_err(classify)?;
        let sessions: Vec<Json> = data(&page)
            .iter()
            .map(|session| {
                let time = get(session, "time");
                let created = time.and_then(|t| get_num(t, "created")).unwrap_or(0.0);
                let updated = time.and_then(|t| get_num(t, "updated")).unwrap_or(0.0);
                let title = get_str(session, "title").map(String::from).unwrap_or_else(|| {
                    format!("{} - {}", if get(session, "parentID").is_some() { "Child session" } else { "New session" }, iso(created))
                });
                let mut o = Obj::new();
                o.opt("sessionId", get(session, "id").cloned())
                    .opt("cwd", get(session, "location").and_then(|l| get(l, "directory")).cloned());
                let extra = granted(session);
                if !extra.is_empty() {
                    o.set("additionalDirectories", directory_list(&extra));
                }
                o.str("title", &title).str("updatedAt", &iso(updated));
                o.done()
            })
            .collect();
        let mut o = Obj::new();
        o.set("sessions", Json::Array(sessions));
        if let Some(next) = get(&page, "cursor").and_then(|c| get_str(c, "next")).filter(|n| !n.is_empty()) {
            o.str("nextCursor", next);
        }
        Ok(o.done())
    }

    async fn select(&self, attached: &Attached, config: &str, value: &str) -> std::result::Result<(), RpcError> {
        let catalog = self.catalog(&attached.cwd).await?;
        let current = catalog.current(&attached.selection.lock().unwrap());
        let change = match config {
            "model" => Selection { model: Some(require_model(&catalog, value, &current)?), mode: None },
            "effort" => {
                let model = find_model(&catalog.models, &current);
                if model.is_none() || (value != "default" && !variants(model).iter().any(|v| v == value)) {
                    return Err(RpcError::invalid("effort", value, format!("effort not found: {value}")));
                }
                let mut next = current.clone();
                json::set(&mut next, "variant", Json::String(value.into()));
                Selection { model: Some(next), mode: None }
            }
            "mode" => {
                if !catalog.modes.iter().any(|m| get_str(m, "id") == Some(value)) {
                    return Err(RpcError::invalid("mode", value, format!("mode not found: {value}")));
                }
                Selection { model: None, mode: Some(value.into()) }
            }
            other => return Err(RpcError::invalid("configId", other, format!("unknown config option: {other}"))),
        };
        // The selection changes before the switch, so the server's echo of it changes nothing.
        {
            let mut selection = attached.selection.lock().unwrap();
            *selection = selection.patched(&change);
        }
        let mut body = Obj::new();
        let operation = match (&change.model, &change.mode) {
            (Some(model), _) => {
                body.set("model", model.clone());
                "session.switchModel"
            }
            (_, Some(mode)) => {
                body.str("agent", mode);
                "session.switchAgent"
            }
            _ => return Ok(()),
        };
        self.client.call(operation, &[("sessionID", &attached.id)], None, Some(body.done())).await.map_err(classify)?;
        Ok(())
    }

    /// Ends a session's running prompt and waits until it has answered; whether there was one.
    async fn cancel(&self, session_id: &str) -> bool {
        let entry = self.turns.lock().unwrap().remove(session_id);
        let Some(entry) = entry else { return false };
        let _ = entry.cancel.send(true);
        let mut done = entry.done;
        let _ = done.wait_for(|d| *d).await;
        true
    }

    /// `ACPTurn.prompt`: one prompt per session at a time; a cancel of its request cancels its turn.
    async fn prompt(self: &Arc<Self>, params: &Json, mut aborted: watch::Receiver<bool>) -> Reply {
        let session_id = get_str(params, "sessionId").unwrap_or("").to_owned();
        let (cancel, cancelled) = watch::channel(false);
        let (done_tx, done) = watch::channel(false);
        let token = self.next_turn.fetch_add(1, Ordering::SeqCst) as u64;
        {
            let mut turns = self.turns.lock().unwrap();
            if turns.contains_key(&session_id) {
                return Err(RpcError::service(&format!("Session already has an active ACP prompt: {session_id}"), Some("session"), None));
            }
            turns.insert(session_id.clone(), TurnEntry { token, cancel: cancel.clone(), done });
        }
        let forward = tokio::spawn(async move {
            if aborted.wait_for(|a| *a).await.is_ok() {
                let _ = cancel.send(true);
            }
        });
        let reply = turn::run(self, params, cancelled).await;
        forward.abort();
        {
            let mut turns = self.turns.lock().unwrap();
            if turns.get(&session_id).is_some_and(|t| t.token == token) {
                turns.remove(&session_id);
            }
        }
        let _ = done_tx.send(true);
        reply
    }
}

fn translate_child_capability() -> &'static str {
    "opencode/child-session-updates"
}

/// Work after a successful response.
enum After {
    Activate(Arc<Attached>, Catalog),
}

fn commands_update(commands: Json) -> Json {
    let mut o = Obj::new();
    o.str("sessionUpdate", "available_commands_update").set("availableCommands", commands);
    o.done()
}

/// `parseModelSelection` and `requireModel`: an advertised model, optionally with `/<effort>`.
fn require_model(catalog: &Catalog, value: &str, current: &Json) -> std::result::Result<Json, RpcError> {
    let models = &catalog.models;
    let has_variant = |model: &Json, variant: &str| variants(Some(model)).iter().any(|v| v == variant);
    let (provider, id, variant) = if let Some(exact) = models.iter().find(|m| advertised(m) == value) {
        (get_str(exact, "providerID").unwrap_or("").to_owned(), get_str(exact, "id").unwrap_or("").to_owned(), None)
    } else {
        // `lastIndexOf("/")`; without one, JS's `slice(0, -1)` drops the last character.
        let (base, variant) = match value.rfind('/') {
            Some(at) => (&value[..at], &value[at + 1..]),
            None => (value.get(..value.len().saturating_sub(value.chars().last().map_or(0, char::len_utf8))).unwrap_or(""), value),
        };
        match models.iter().find(|m| advertised(m) == base && has_variant(m, variant)) {
            Some(model) => (get_str(model, "providerID").unwrap_or("").to_owned(), get_str(model, "id").unwrap_or("").to_owned(), Some(variant.to_owned())),
            None => match value.split_once('/') {
                Some((provider, id)) => (provider.to_owned(), id.to_owned(), None),
                None => (value.to_owned(), String::new(), None),
            },
        }
    };
    let mut reference = Obj::new();
    reference.str("providerID", &provider).str("id", &id);
    let Some(model) = find_model(models, &reference.done()) else {
        let mut data = Obj::new();
        data.str("providerId", &provider).str("modelId", value);
        return Err(RpcError::invalid_params(data.done(), &format!("model not found: {value}")));
    };
    if let Some(variant) = &variant {
        if !has_variant(model, variant) {
            return Err(RpcError::invalid("effort", variant, format!("effort not found: {variant}")));
        }
    }
    let same = get_str(current, "providerID") == get_str(model, "providerID") && get_str(current, "id") == get_str(model, "id");
    let kept = get_str(current, "variant").filter(|v| same && (*v == "default" || has_variant(model, v))).map(String::from);
    let mut out = Obj::new();
    out.opt("providerID", get(model, "providerID").cloned()).opt("id", get(model, "id").cloned()).opt_str("variant", variant.or(kept).as_deref());
    Ok(out.done())
}

fn iso(ms: f64) -> String {
    let millis = ms.rem_euclid(1000.0) as i64;
    let seconds = (ms / 1000.0).floor() as i64;
    let (y, m, d) = super::commands::civil(seconds.div_euclid(86_400));
    let rest = seconds.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

fn initialize(params: &Json) -> Json {
    let capabilities = get(params, "clientCapabilities");
    let mut auth = Obj::new();
    auth.str("description", "Run `opencode auth login` in the terminal").str("name", "Login with opencode").str("id", AUTH_METHOD);
    if capabilities.and_then(|c| get(c, "auth")).and_then(|a| json::get_bool(a, "terminal")) == Some(true) {
        auth.str("type", "terminal").set("args", Json::Array(vec![Json::String("--login".into())]));
    }
    if capabilities.and_then(|c| get(c, "_meta")).and_then(|m| json::get_bool(m, "terminal-auth")) == Some(true) {
        let mut terminal = Obj::new();
        terminal
            .str("command", "opencode")
            .set("args", Json::Array(vec![Json::String("auth".into()), Json::String("login".into())]))
            .str("label", "OpenCode Login");
        let mut meta = Obj::new();
        meta.set("terminal-auth", terminal.done());
        auth.set("_meta", meta.done());
    }
    let parse = |text: &str| json::parse(text).expect("literal JSON");
    let mut o = Obj::new();
    o.int("protocolVersion", 1)
        .set(
            "agentCapabilities",
            parse(r#"{"loadSession":true,"mcpCapabilities":{"http":true,"sse":false},"promptCapabilities":{"embeddedContext":true,"image":true},"sessionCapabilities":{"additionalDirectories":{},"close":{},"delete":{},"fork":{},"list":{},"resume":{}},"_meta":{"opencode/child-session-updates":true}}"#),
        )
        .set("authMethods", Json::Array(vec![auth.done()]));
    let mut info = Obj::new();
    info.str("name", "OpenCode").str("version", crate::app::VERSION);
    o.set("agentInfo", info.done());
    o.done()
}

/// The agent's own event stream (`ACPSessions` and `ACPCatalog` subscribe before serving): selections the server
/// reports for attached sessions, and model, agent and command changes that reload a directory's catalog.
async fn follow_events(agent: Arc<Agent>, connected: watch::Sender<bool>) {
    let mut events = match agent.client.events().await {
        Ok(events) => events,
        Err(_) => {
            let _ = connected.send(true);
            return;
        }
    };
    while let Some(Ok(event)) = events.next().await {
        let kind = get_str(&event, "type").unwrap_or("");
        let data = get(&event, "data").cloned().unwrap_or(Json::Null);
        match kind {
            "server.connected" => {
                let _ = connected.send(true);
            }
            "session.model.selected" => agent.selected(get_str(&data, "sessionID").unwrap_or(""), Selection { model: get(&data, "model").cloned(), mode: None }),
            "session.agent.selected" => {
                agent.selected(get_str(&data, "sessionID").unwrap_or(""), Selection { model: None, mode: get_str(&data, "agent").map(String::from) })
            }
            "model.updated" | "agent.updated" | "command.updated" => {
                let directories: Vec<String> = match get(&event, "location").and_then(|l| get_str(l, "directory")) {
                    Some(directory) => vec![directory.to_owned()],
                    None => agent.catalogs.lock().await.keys().cloned().collect(),
                };
                for directory in directories {
                    let agent = agent.clone();
                    tokio::spawn(async move { agent.reload(&directory).await });
                }
            }
            _ => {}
        }
    }
    let _ = connected.send(true);
}

/// The handler: `--login` is `auth login`; otherwise serve ACP on stdio until stdin ends.
pub async fn command(globals: &super::parse::Globals, input: &Input) -> Result {
    if input.flag("login") {
        return super::auth::command("login", globals, &Input::default()).await;
    }
    // SAFETY: no other thread reads the environment yet.
    unsafe { std::env::set_var("OPENCODE_CLIENT", "acp") };
    let (endpoint, mut private) = connection::standalone().await?;
    let (connected_tx, connected) = watch::channel(false);
    let agent = Arc::new(Agent {
        client: Client::new(&endpoint),
        conn: Arc::new(Connection { write: Mutex::new(()), next: AtomicI64::new(0), pending: Mutex::new(HashMap::new()), incoming: Mutex::new(HashMap::new()) }),
        catalogs: tokio::sync::Mutex::new(HashMap::new()),
        sessions: Mutex::new(HashMap::new()),
        turns: Mutex::new(HashMap::new()),
        next_turn: AtomicI64::new(0),
        unprompted: Mutex::new(vec![]),
        capabilities: Mutex::new(Capabilities::default()),
        connected,
    });
    tokio::spawn(follow_events(agent.clone(), connected_tx));
    let (lines_tx, mut lines) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    let failure = loop {
        tokio::select! {
            line = lines.recv() => {
                let Some(line) = line else { break None };
                dispatch(&agent, &line);
            }
            exit = private.exited() => {
                let reason = match exit {
                    Ok(code) => format!("code {code}"),
                    Err(signal) => format!("signal {signal}"),
                };
                break Some(format!("server exited unexpectedly ({reason})"));
            }
        }
    };
    if let Some(failure) = &failure {
        super::out::stderr(&format!("opencode acp: {failure}\n"));
    }
    // Exit directly: the lease pipe ends the private server.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    std::process::exit(if failure.is_some() { 1 } else { 0 })
}

fn dispatch(agent: &Arc<Agent>, line: &str) {
    let Some(message) = json::parse(line) else {
        let mut o = Obj::new();
        let mut error = Obj::new();
        error.int("code", -32700).str("message", "Parse error");
        o.str("jsonrpc", "2.0").set("id", Json::Null).set("error", error.done());
        agent.conn.send(&o.done());
        return;
    };
    let Some(method) = get_str(&message, "method").map(String::from) else {
        if get(&message, "id").is_some() {
            agent.conn.answered(&message);
        }
        return;
    };
    let params = get(&message, "params").cloned().unwrap_or_else(json::object);
    let id = get(&message, "id").cloned();
    if id.is_none() {
        match method.as_str() {
            "$/cancel_request" => {
                let target = get(&params, "requestId").map(json::text);
                if let Some(cancel) = target.and_then(|t| agent.conn.incoming.lock().unwrap().get(&t).cloned()) {
                    let _ = cancel.send(true);
                }
            }
            "session/cancel" => {
                let agent = agent.clone();
                let session = get_str(&params, "sessionId").unwrap_or("").to_owned();
                tokio::spawn(async move { agent.cancel(&session).await });
            }
            _ => {}
        }
        return;
    }
    let id = id.unwrap();
    let key = json::text(&id);
    let (cancel, cancelled) = watch::channel(false);
    agent.conn.incoming.lock().unwrap().insert(key.clone(), cancel);
    let agent = agent.clone();
    tokio::spawn(async move {
        let (reply, after) = agent.request(&method, &params, cancelled).await;
        let succeeded = reply.is_ok();
        agent.conn.respond(&id, reply);
        agent.conn.incoming.lock().unwrap().remove(&key);
        match after {
            Some(After::Activate(attached, catalog)) if succeeded => agent.activate(&attached, catalog).await,
            _ => {}
        }
    });
}
