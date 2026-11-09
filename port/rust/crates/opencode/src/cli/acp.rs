//! `opencode acp` (packages/cli/src/commands/handlers/acp.ts, acp/*.ts): the Agent Client Protocol over stdio,
//! newline-delimited JSON-RPC 2.0, against a private server. Sessions, prompts (text, resources, images, slash
//! commands, compaction), the turn's events translated to `session/update` notifications, usage, cancellation,
//! config options (model, effort, mode), list, fork, resume, close and delete.
//!
//! TODO(port): permission requests and forms are answered on the client's behalf (rejected, cancelled) instead of
//! through `session/request_permission` and elicitation; `session/load` attaches without replaying history; child
//! session updates, MCP server registration, and completed-edit diffs are not sent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use opencode_protocol::Json;
use tokio::sync::mpsc;

use super::client::{Client, ClientError, Events};
use super::connection;
use super::out::Result;
use super::parse::Input;
use crate::json::{self, Obj, get, get_num, get_str};

const AUTH_METHOD: &str = "opencode-login";

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

/// Writes one message per line on stdout.
#[derive(Clone)]
struct Out(Arc<Mutex<()>>);

impl Out {
    fn send(&self, message: &Json) {
        let _guard = self.0.lock().unwrap();
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
}

/// The models, modes and commands of a directory (`ACPCatalog.Catalog`).
#[derive(Clone)]
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

struct Attached {
    cwd: String,
    selection: Mutex<Selection>,
}

struct Agent {
    client: Client,
    out: Out,
    catalogs: tokio::sync::Mutex<HashMap<String, Catalog>>,
    sessions: Mutex<HashMap<String, Arc<Attached>>>,
    turns: Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>,
    unprompted: Mutex<Vec<String>>,
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
        None | Some("default") => "default".into(),
        Some(v) if variants.iter().any(|x| x == v) => v.into(),
        _ if variants.iter().any(|x| x == "default") => "default".into(),
        _ => variants.first().cloned().unwrap_or_else(|| "default".into()),
    }
}

fn absolute(path: &str) -> bool {
    path.starts_with('/')
}

fn parse_cwd(cwd: &str) -> std::result::Result<(), RpcError> {
    if !absolute(cwd) {
        return Err(RpcError::invalid_request(&format!("cwd must be an absolute path: {cwd}"), Some("cwd")));
    }
    Ok(())
}

/// `ACPDirectories.parse`: the additional directories, absolute and without glob characters.
fn parse_directories(cwd: &str, params: &Json) -> std::result::Result<Vec<String>, RpcError> {
    parse_cwd(cwd)?;
    let mut out: Vec<String> = vec![];
    for directory in get(params, "additionalDirectories").and_then(json::array).unwrap_or(&[]) {
        let directory = json::as_str(directory).unwrap_or("");
        if !absolute(directory) || directory.contains(['*', '?']) {
            let mut data = Obj::new();
            data.str("additionalDirectory", directory);
            return Err(RpcError::invalid_params(
                data.done(),
                &format!("additional directory must be an absolute path without glob characters: {directory}"),
            ));
        }
        let resolved = crate::project::absolutize(directory);
        if resolved != crate::project::absolutize(cwd) && !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    Ok(out)
}

fn grant(directories: &[String], body: &mut Obj) {
    if directories.is_empty() {
        return;
    }
    let rules: Vec<Json> = directories
        .iter()
        .map(|d| {
            let mut o = Obj::new();
            o.str("action", "external_directory").str("resource", &format!("{d}/*")).str("effect", "allow");
            o.done()
        })
        .collect();
    let mut metadata = Obj::new();
    metadata.set("opencode.acp.additionalDirectories", Json::Array(directories.iter().map(|d| Json::String(d.clone())).collect()));
    body.set("permissions", Json::Array(rules)).set("metadata", metadata.done());
}

fn check_mcp(params: &Json) -> std::result::Result<(), RpcError> {
    let servers = get(params, "mcpServers").and_then(json::array).unwrap_or(&[]);
    if servers.iter().any(|s| get_str(s, "type").is_some_and(|t| t != "http")) {
        return Err(RpcError::invalid_request("Only stdio and HTTP MCP servers are supported", Some("mcpServers")));
    }
    Ok(())
}

impl Agent {
    async fn catalog(&self, cwd: &str) -> std::result::Result<Catalog, RpcError> {
        let key = crate::project::absolutize(cwd);
        let mut catalogs = self.catalogs.lock().await;
        if let Some(found) = catalogs.get(&key) {
            return Ok(found.clone());
        }
        let loaded = self.load_catalog(cwd).await?;
        catalogs.insert(key, loaded.clone());
        Ok(loaded)
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

    /// `ACPSessions.attach`: the session's selection, and (after the response) its commands.
    async fn attach(&self, session: &Json, cwd: &str) -> std::result::Result<(String, Json, Json), RpcError> {
        let catalog = self.catalog(cwd).await?;
        let id = get_str(session, "id").unwrap_or("").to_owned();
        let selection = Selection { model: get(session, "model").cloned(), mode: get_str(session, "agent").map(String::from) };
        let options = catalog.options(&selection);
        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), Arc::new(Attached { cwd: cwd.to_owned(), selection: Mutex::new(selection) }));
        Ok((id, options, catalog.available_commands()))
    }

    async fn get_session(&self, id: &str, cwd: &str) -> std::result::Result<Json, RpcError> {
        if !id.starts_with("ses") {
            return Err(RpcError::invalid_request("Invalid session ID", Some("sessionID")));
        }
        let found = self.client.call("session.get", &[("sessionID", id)], None, None).await.map_err(classify)?;
        let session = get(&found, "data").cloned().unwrap_or(Json::Null);
        let directory = get(&session, "location").and_then(|l| get_str(l, "directory")).unwrap_or("");
        if crate::project::absolutize(cwd) != crate::project::absolutize(directory) {
            let mut data = Obj::new();
            data.str("sessionId", id).str("cwd", cwd);
            return Err(RpcError::invalid_params(data.done(), &format!("session {id} does not belong to cwd: {cwd}")));
        }
        Ok(session)
    }

    /// One request; the `after` notifications go out once the response is written.
    async fn request(self: &Arc<Self>, method: &str, params: &Json) -> (Reply, Vec<(String, Json)>) {
        let cwd = get_str(params, "cwd").unwrap_or("").to_owned();
        let session_id = get_str(params, "sessionId").unwrap_or("").to_owned();
        let mut after = vec![];
        let reply: Reply = async {
            match method {
                "initialize" => Ok(initialize(params)),
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
                    self.catalog(&cwd).await?;
                    let mut body = Obj::new();
                    let mut at = Obj::new();
                    at.str("directory", &cwd);
                    body.set("location", at.done());
                    grant(&directories, &mut body);
                    let created = self.client.call("session.create", &[], None, Some(body.done())).await.map_err(classify)?;
                    let session = get(&created, "data").cloned().unwrap_or(Json::Null);
                    let (id, options, commands) = self.attach(&session, &cwd).await?;
                    self.unprompted.lock().unwrap().push(id.clone());
                    after.push((id.clone(), commands_update(commands)));
                    let mut o = Obj::new();
                    o.str("sessionId", &id).set("configOptions", options);
                    Ok(o.done())
                }
                "session/load" | "session/resume" => {
                    parse_directories(&cwd, params)?;
                    check_mcp(params)?;
                    let session = self.get_session(&session_id, &cwd).await?;
                    let directory = get(&session, "location").and_then(|l| get_str(l, "directory")).unwrap_or(&cwd).to_owned();
                    let (id, options, commands) = self.attach(&session, &directory).await?;
                    after.push((id, commands_update(commands)));
                    let mut o = Obj::new();
                    o.set("configOptions", options);
                    Ok(o.done())
                }
                "session/fork" => {
                    parse_directories(&cwd, params)?;
                    check_mcp(params)?;
                    let session = self.get_session(&session_id, &cwd).await?;
                    let id = get_str(&session, "id").unwrap_or("").to_owned();
                    let forked = self.client.call("session.fork", &[("sessionID", &id)], None, Some(json::object())).await.map_err(classify)?;
                    let forked = get(&forked, "data").cloned().unwrap_or(Json::Null);
                    let directory = get(&forked, "location").and_then(|l| get_str(l, "directory")).unwrap_or(&cwd).to_owned();
                    let (id, options, commands) = self.attach(&forked, &directory).await?;
                    after.push((id.clone(), commands_update(commands)));
                    let mut o = Obj::new();
                    o.str("sessionId", &id).set("configOptions", options);
                    Ok(o.done())
                }
                "session/list" => self.list(params).await,
                "session/delete" => {
                    self.unprompted.lock().unwrap().retain(|s| *s != session_id);
                    self.cancel(&session_id);
                    if session_id.starts_with("ses") {
                        match self.client.call("session.remove", &[("sessionID", &session_id)], None, None).await {
                            Ok(_) => {}
                            Err(error) if error.tag() == Some("SessionNotFoundError") => {}
                            Err(error) => return Err(classify(error)),
                        }
                    }
                    self.sessions.lock().unwrap().remove(&session_id);
                    Ok(json::object())
                }
                "session/close" => {
                    let running = self.cancel(&session_id);
                    if !running && session_id.starts_with("ses") {
                        let _ = self.client.call("session.interrupt", &[("sessionID", &session_id)], None, None).await;
                    }
                    self.sessions.lock().unwrap().remove(&session_id);
                    let unprompted = {
                        let mut list = self.unprompted.lock().unwrap();
                        let had = list.contains(&session_id);
                        list.retain(|s| *s != session_id);
                        had
                    };
                    if unprompted {
                        let mut query = Obj::new();
                        query.int("limit", 1);
                        if let Ok(messages) = self.client.call("session.message.list", &[("sessionID", &session_id)], Some(query.done()), None).await {
                            if data(&messages).is_empty() {
                                let _ = self.client.call("session.remove", &[("sessionID", &session_id)], None, None).await;
                            }
                        }
                    }
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
                    self.select(&attached, &session_id, &config, &value).await?;
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
                    self.prompt(params).await
                }
                _ => Err(RpcError::method_not_found(method)),
            }
        }
        .await;
        (reply, after)
    }

    async fn list(&self, params: &Json) -> Reply {
        let mut query = Obj::new();
        if let Some(cwd) = get_str(params, "cwd") {
            parse_cwd(cwd)?;
            query.str("directory", cwd);
        }
        query.str("order", "desc").int("limit", 100);
        if let Some(cursor) = get_str(params, "cursor") {
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
                if let Some(Json::Array(extra)) = get(session, "metadata").and_then(|m| get(m, "opencode.acp.additionalDirectories")) {
                    if !extra.is_empty() {
                        o.set("additionalDirectories", Json::Array(extra.clone()));
                    }
                }
                o.str("title", &title).str("updatedAt", &iso(updated));
                o.done()
            })
            .collect();
        let mut o = Obj::new();
        o.set("sessions", Json::Array(sessions));
        if let Some(next) = get(&page, "cursor").and_then(|c| get_str(c, "next")) {
            o.str("nextCursor", next);
        }
        Ok(o.done())
    }

    async fn select(&self, attached: &Attached, session_id: &str, config: &str, value: &str) -> std::result::Result<(), RpcError> {
        let catalog = self.catalog(&attached.cwd).await?;
        let current = catalog.current(&attached.selection.lock().unwrap());
        let invalid = |key: &str, value: &str, message: String| {
            let mut data = Obj::new();
            data.str(key, value);
            RpcError::invalid_params(data.done(), &message)
        };
        let change = match config {
            "model" => {
                let model = catalog
                    .models
                    .iter()
                    .find(|m| advertised(m) == value)
                    .ok_or_else(|| {
                        let mut data = Obj::new();
                        data.str("providerId", value.split('/').next().unwrap_or("")).str("modelId", value);
                        RpcError::invalid_params(data.done(), &format!("model not found: {value}"))
                    })?
                    .clone();
                let mut reference = Obj::new();
                reference.opt("providerID", get(&model, "providerID").cloned()).opt("id", get(&model, "id").cloned());
                let same = get_str(&current, "providerID") == get_str(&model, "providerID") && get_str(&current, "id") == get_str(&model, "id");
                if let Some(variant) = get_str(&current, "variant").filter(|v| same && (*v == "default" || variants(Some(&model)).iter().any(|x| x == v))) {
                    reference.str("variant", variant);
                }
                Selection { model: Some(reference.done()), mode: None }
            }
            "effort" => {
                let model = find_model(&catalog.models, &current);
                if model.is_none() || (value != "default" && !variants(model).iter().any(|v| v == value)) {
                    return Err(invalid("effort", value, format!("effort not found: {value}")));
                }
                let mut next = current.clone();
                json::set(&mut next, "variant", Json::String(value.into()));
                Selection { model: Some(next), mode: None }
            }
            "mode" => {
                if !catalog.modes.iter().any(|m| get_str(m, "id") == Some(value)) {
                    return Err(invalid("mode", value, format!("mode not found: {value}")));
                }
                Selection { model: None, mode: Some(value.into()) }
            }
            other => return Err(invalid("configId", other, format!("unknown config option: {other}"))),
        };
        {
            let mut selection = attached.selection.lock().unwrap();
            if let Some(model) = &change.model {
                selection.model = Some(model.clone());
            }
            if let Some(mode) = &change.mode {
                selection.mode = Some(mode.clone());
            }
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
        self.client.call(operation, &[("sessionID", session_id)], None, Some(body.done())).await.map_err(classify)?;
        Ok(())
    }

    /// Ends a session's running prompt; whether there was one.
    fn cancel(&self, session_id: &str) -> bool {
        match self.turns.lock().unwrap().remove(session_id) {
            Some(cancel) => {
                let _ = cancel.send(true);
                true
            }
            None => false,
        }
    }

    async fn prompt(&self, params: &Json) -> Reply {
        let session_id = get_str(params, "sessionId").unwrap_or("").to_owned();
        let attached = self.attached(&session_id)?;
        let catalog = self.catalog(&attached.cwd).await?;
        let prepared = prepare(&catalog, get(params, "prompt").and_then(json::array).unwrap_or(&[]))?;
        let (cancel, cancelled) = tokio::sync::watch::channel(false);
        {
            let mut turns = self.turns.lock().unwrap();
            if turns.contains_key(&session_id) {
                return Err(RpcError::service(&format!("Session already has an active ACP prompt: {session_id}"), Some("session"), None));
            }
            turns.insert(session_id.clone(), cancel);
        }
        let result = self.turn(&session_id, &attached, &catalog, &prepared, cancelled).await;
        self.turns.lock().unwrap().remove(&session_id);
        result
    }

    async fn submit(&self, session_id: &str, prepared: &Prepared) -> std::result::Result<(), RpcError> {
        if !prepared.synthetic.is_empty() {
            let mut body = Obj::new();
            body.str("text", &prepared.synthetic.join("\n\n"))
                .str("description", "ACP embedded context")
                .str("delivery", "steer")
                .set("resume", Json::Bool(false));
            self.client.call("session.synthetic", &[("sessionID", session_id)], None, Some(body.done())).await.map_err(classify)?;
        }
        if prepared.compaction {
            let mut body = Obj::new();
            body.str("id", &prepared.id);
            self.client.call("session.compact", &[("sessionID", session_id)], None, Some(body.done())).await.map_err(classify)?;
            return Ok(());
        }
        if let Some((name, args)) = prepared.command.as_ref() {
            let mut body = Obj::new();
            body.str("name", name).str("text", args).set("files", Json::Array(prepared.files.clone())).str("delivery", "steer");
            self.client.call("session.command", &[("sessionID", session_id)], None, Some(body.done())).await.map_err(classify)?;
            return Ok(());
        }
        let mut body = Obj::new();
        body.str("id", &prepared.id).str("text", &prepared.text).set("files", Json::Array(prepared.files.clone())).str("delivery", "steer");
        self.client.call("session.prompt", &[("sessionID", session_id)], None, Some(body.done())).await.map_err(classify)?;
        Ok(())
    }

    async fn turn(
        &self,
        session_id: &str,
        attached: &Attached,
        catalog: &Catalog,
        prepared: &Prepared,
        mut cancelled: tokio::sync::watch::Receiver<bool>,
    ) -> Reply {
        let mut events = self.client.events().await.map_err(classify)?;
        match events.next().await {
            Some(Ok(event)) if get_str(&event, "type") == Some("server.connected") => {}
            _ => return Err(RpcError::unavailable()),
        }
        let state = Arc::new(Mutex::new(Turn { start: prepared.id.clone(), ..Turn::default() }));
        let (done_tx, mut done_rx) = mpsc::channel::<std::result::Result<&'static str, RpcError>>(1);
        let consumer = {
            let state = state.clone();
            let out = self.out.clone();
            let client = self.client.clone();
            let session = session_id.to_owned();
            let cwd = attached.cwd.clone();
            tokio::spawn(async move {
                let result = consume(&mut events, &state, &out, &client, &session, &cwd).await;
                let _ = done_tx.send(result).await;
            })
        };
        self.submit(session_id, prepared).await?;
        let terminal = if prepared.command.is_some() {
            consumer.abort();
            Ok("succeeded")
        } else {
            tokio::select! {
                result = done_rx.recv() => result.unwrap_or(Err(RpcError::unavailable())),
                _ = async { let _ = cancelled.wait_for(|c| *c).await; } => {
                    let _ = self.client.call("session.interrupt", &[("sessionID", session_id)], None, None).await;
                    if state.lock().unwrap().started {
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), done_rx.recv()).await;
                    }
                    consumer.abort();
                    Ok("interrupted")
                }
            }
        };
        let terminal = terminal?;
        let current = std::mem::take(&mut *state.lock().unwrap());
        if terminal != "interrupted" {
            if let Some(error) = current.step_error.as_ref().or(current.execution_error.as_ref()) {
                let kind = get_str(error, "type").unwrap_or("");
                if kind == "provider.auth" {
                    return Err(RpcError { code: -32000, message: "Authentication required: provider authentication required".into(), data: Some(json::object()) });
                }
                if kind != "aborted" && kind != "provider.content-filter" {
                    let message = get_str(error, "message").filter(|m| !m.is_empty()).unwrap_or("OpenCode prompt failed");
                    return Err(RpcError::service(message, Some("session"), Some(kind)));
                }
            }
        }
        self.usage_update(session_id, attached, catalog, &current).await;
        Ok(response(&current, session_id, terminal))
    }

    /// `sendUsageUpdate`: the last step's tokens against the model's context.
    async fn usage_update(&self, session_id: &str, attached: &Attached, catalog: &Catalog, state: &Turn) {
        let used = state.last.as_ref().map(total).unwrap_or(0.0);
        if used == 0.0 {
            return;
        }
        let selection = attached.selection.lock().unwrap().clone();
        let Some(model) = find_model(&catalog.models, &catalog.current(&selection)) else { return };
        let Some(size) = get(model, "limit").and_then(|l| get_num(l, "context")).filter(|c| *c > 0.0) else { return };
        let Ok(info) = self.client.call("session.get", &[("sessionID", session_id)], None, None).await else { return };
        let mut cost = Obj::new();
        cost.set("amount", get(&info, "data").and_then(|d| get(d, "cost")).cloned().unwrap_or(Json::Number(0.0))).str("currency", "USD");
        let mut update = Obj::new();
        update.str("sessionUpdate", "usage_update").set("used", Json::Number(used)).set("size", Json::Number(size)).set("cost", cost.done());
        self.out.update(session_id, update.done());
    }
}

fn commands_update(commands: Json) -> Json {
    let mut o = Obj::new();
    o.str("sessionUpdate", "available_commands_update").set("availableCommands", commands);
    o.done()
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

struct Prepared {
    id: String,
    text: String,
    files: Vec<Json>,
    synthetic: Vec<String>,
    command: Option<(String, String)>,
    compaction: bool,
}

fn file_name(uri: &str) -> Option<String> {
    let path = uri.split(['?', '#']).next().unwrap_or(uri);
    path.rsplit('/').next().filter(|n| !n.is_empty()).map(|n| crate::http::percent_decode(n))
}

/// `ACPPrompt.prepare` over `promptContentToParts`.
fn prepare(catalog: &Catalog, content: &[Json]) -> std::result::Result<Prepared, RpcError> {
    let mut visible_text = vec![];
    let mut files = vec![];
    let mut synthetic = vec![];
    for block in content {
        match get_str(block, "type") {
            Some("text") => {
                let text = get_str(block, "text").unwrap_or("").to_owned();
                let audience: Vec<&str> =
                    get(block, "annotations").and_then(|a| get(a, "audience")).and_then(json::array).unwrap_or(&[]).iter().filter_map(json::as_str).collect();
                match audience.as_slice() {
                    ["assistant"] => synthetic.push(text),
                    ["user"] => {}
                    _ => visible_text.push(text),
                }
            }
            Some("image") => {
                let data = get_str(block, "data").filter(|d| !d.is_empty());
                let uri = get_str(block, "uri").filter(|u| !u.is_empty());
                if data.is_none() && uri.is_none() {
                    return Err(RpcError::invalid_request("image content has no data or uri", Some("prompt")));
                }
                let mime = get_str(block, "mimeType").unwrap_or("");
                let url = data.map(|d| format!("data:{mime};base64,{d}")).or(uri.map(String::from)).unwrap_or_default();
                let mut file = Obj::new();
                file.str("uri", &url).str("name", &uri.and_then(file_name).unwrap_or_else(|| "image".into()));
                files.push(file.done());
            }
            Some("resource_link") => {
                let uri = get_str(block, "uri").unwrap_or("");
                let mut file = Obj::new();
                file.str("uri", uri).str("name", get_str(block, "name").unwrap_or(""));
                files.push(file.done());
            }
            Some("resource") => {
                let resource = get(block, "resource").cloned().unwrap_or(Json::Null);
                let uri = get_str(&resource, "uri").unwrap_or("");
                if let Some(text) = get_str(&resource, "text") {
                    let label = match uri.strip_prefix("file://") {
                        Some(path) => {
                            let (path, hash) = path.split_once('#').unwrap_or((path, ""));
                            let line = hash.strip_prefix('L').map(|l| l.chars().take_while(|c| c.is_ascii_digit()).collect::<String>()).filter(|l| !l.is_empty());
                            let path = crate::http::percent_decode(path);
                            match line {
                                Some(line) => format!("{path}:{line}"),
                                None => path,
                            }
                        }
                        None => uri.to_owned(),
                    };
                    visible_text.push(format!("[{label}]\n{text}"));
                } else if let Some(mime) = get_str(&resource, "mimeType") {
                    let url = if uri.starts_with("data:") {
                        uri.to_owned()
                    } else {
                        format!("data:{mime};base64,{}", get_str(&resource, "blob").unwrap_or(""))
                    };
                    let mut file = Obj::new();
                    file.str("uri", &url).str("name", &file_name(uri).unwrap_or_else(|| "file".into()));
                    files.push(file.done());
                }
            }
            _ => {}
        }
    }
    let text = visible_text.join("\n");
    let trimmed = text.trim();
    let slash = trimmed.strip_prefix('/').and_then(|rest| {
        let mut words = rest.split_whitespace();
        let name = words.next()?.to_owned();
        Some((name, words.collect::<Vec<_>>().join(" ")))
    });
    let compaction = slash.as_ref().is_some_and(|(name, _)| name == "compact");
    let command = slash.filter(|(name, _)| catalog.commands.iter().any(|c| get_str(c, "name") == Some(name.as_str())));
    Ok(Prepared { id: format!("msg_{}", crate::id::ascending()), text, files, synthetic, command, compaction })
}

/// `ACPTranslate.TurnState`, for the root session.
#[derive(Default)]
struct Turn {
    start: String,
    started: bool,
    tools: HashMap<String, (String, Json)>,
    finish: Option<String>,
    usage: Option<Json>,
    last: Option<Json>,
    step_error: Option<Json>,
    execution_error: Option<Json>,
}

fn total(tokens: &Json) -> f64 {
    let cache = get(tokens, "cache");
    ["input", "output", "reasoning"].iter().map(|k| get_num(tokens, k).unwrap_or(0.0)).sum::<f64>()
        + cache.map_or(0.0, |c| get_num(c, "read").unwrap_or(0.0) + get_num(c, "write").unwrap_or(0.0))
}

fn add_tokens(a: &Json, b: &Json) -> Json {
    let n = |v: &Json, k: &str| get_num(v, k).unwrap_or(0.0);
    let c = |v: &Json, k: &str| get(v, "cache").and_then(|c| get_num(c, k)).unwrap_or(0.0);
    let mut cache = Obj::new();
    cache.set("read", Json::Number(c(a, "read") + c(b, "read"))).set("write", Json::Number(c(a, "write") + c(b, "write")));
    let mut o = Obj::new();
    o.set("input", Json::Number(n(a, "input") + n(b, "input")))
        .set("output", Json::Number(n(a, "output") + n(b, "output")))
        .set("reasoning", Json::Number(n(a, "reasoning") + n(b, "reasoning")))
        .set("cache", cache.done());
    o.done()
}

fn response(state: &Turn, _session: &str, terminal: &str) -> Json {
    let mut o = Obj::new();
    let error = state.step_error.as_ref().or(state.execution_error.as_ref()).and_then(|e| get_str(e, "type"));
    let stop = if terminal == "interrupted" || error == Some("aborted") {
        "cancelled"
    } else if state.finish.as_deref() == Some("length") {
        "max_tokens"
    } else if state.finish.as_deref() == Some("content-filter") || error == Some("provider.content-filter") {
        "refusal"
    } else {
        "end_turn"
    };
    o.str("stopReason", stop);
    if let Some(tokens) = &state.usage {
        let n = |k: &str| get_num(tokens, k).unwrap_or(0.0);
        let c = |k: &str| get(tokens, "cache").and_then(|c| get_num(c, k)).unwrap_or(0.0);
        let mut usage = Obj::new();
        usage.set("inputTokens", Json::Number(n("input"))).set("outputTokens", Json::Number(n("output"))).set("totalTokens", Json::Number(total(tokens)));
        if n("reasoning") > 0.0 {
            usage.set("thoughtTokens", Json::Number(n("reasoning")));
        }
        if c("read") > 0.0 {
            usage.set("cachedReadTokens", Json::Number(c("read")));
        }
        if c("write") > 0.0 {
            usage.set("cachedWriteTokens", Json::Number(c("write")));
        }
        o.set("usage", usage.done());
    }
    o.set("_meta", json::object());
    o.done()
}

fn canonical(name: &str) -> String {
    match name.to_lowercase().as_str() {
        "bash" => "shell".into(),
        "task" => "subagent".into(),
        "apply_patch" => "patch".into(),
        other => other.to_owned(),
    }
}

fn tool_kind(name: &str) -> &'static str {
    match canonical(name).as_str() {
        "shell" => "execute",
        "webfetch" => "fetch",
        "edit" | "patch" | "write" => "edit",
        "grep" | "glob" | "context" | "context7_resolve_library_id" | "context7_get_library_docs" => "search",
        "read" => "read",
        "subagent" => "think",
        _ => "other",
    }
}

fn locations(name: &str, input: &Json, cwd: &str) -> Json {
    let value = |key: &str| get_str(input, key).map(String::from);
    let paths: Vec<String> = match canonical(name).as_str() {
        "shell" => vec![value("workdir").or_else(|| value("cwd")).unwrap_or_else(|| cwd.to_owned())],
        "read" | "edit" | "write" => value("path").or_else(|| value("filePath")).into_iter().collect(),
        "external_directory" => value("filepath").into_iter().collect(),
        "grep" | "glob" | "context" | "context7_resolve_library_id" | "context7_get_library_docs" => value("path").into_iter().collect(),
        _ => vec![],
    };
    let mut out: Vec<String> = vec![];
    for path in paths.into_iter().filter(|p| !p.is_empty()) {
        let absolute = if path.starts_with('/') { path } else { crate::project::absolutize(&format!("{cwd}/{path}")) };
        if !out.contains(&absolute) {
            out.push(absolute);
        }
    }
    Json::Array(
        out.into_iter()
            .map(|p| {
                let mut o = Obj::new();
                o.str("path", &p);
                o.done()
            })
            .collect(),
    )
}

fn title(name: &str, input: &Json) -> String {
    if canonical(name) == "shell" {
        return get_str(input, "command").or_else(|| get_str(input, "cmd")).unwrap_or(name).to_owned();
    }
    name.to_owned()
}

fn raw_input(name: &str, input: &Json, cwd: &str) -> Json {
    if canonical(name) != "shell" || get(input, "cwd").is_some() || get(input, "workdir").is_some() {
        return input.clone();
    }
    let mut out = input.clone();
    json::set(&mut out, "cwd", Json::String(cwd.into()));
    out
}

fn tool_content(content: &[Json]) -> Vec<Json> {
    content
        .iter()
        .filter_map(|part| {
            let inner = match get_str(part, "type") {
                Some("text") => {
                    let mut o = Obj::new();
                    o.str("type", "text").str("text", get_str(part, "text").unwrap_or(""));
                    o.done()
                }
                _ => {
                    let uri = get_str(part, "uri")?;
                    let rest = uri.strip_prefix("data:")?;
                    let (header, data) = rest.split_once(',')?;
                    let mime = header.split(';').next()?;
                    if !mime.starts_with("image/") || !header.ends_with(";base64") {
                        return None;
                    }
                    let mut o = Obj::new();
                    o.str("type", "image").str("mimeType", mime).str("data", data);
                    o.done()
                }
            };
            let mut o = Obj::new();
            o.str("type", "content").set("content", inner);
            Some(o.done())
        })
        .collect()
}

/// Folds the root session's events into updates until the turn ends (`ACPTranslate.fold`).
async fn consume(events: &mut Events, state: &Mutex<Turn>, out: &Out, client: &Client, session: &str, cwd: &str) -> std::result::Result<&'static str, RpcError> {
    loop {
        let event = match events.next().await {
            Some(Ok(event)) => event,
            _ => return Err(RpcError::unavailable()),
        };
        let kind = get_str(&event, "type").unwrap_or("").to_owned();
        let data = get(&event, "data").cloned().unwrap_or(Json::Null);
        // TODO(port): asks go to the client (`session/request_permission`, elicitation).
        if kind == "permission.asked" && get_str(&data, "sessionID") == Some(session) {
            let mut body = Obj::new();
            body.str("decision", "reject");
            let id = get_str(&data, "id").unwrap_or("").to_owned();
            let _ = client.call("session.permission.reply", &[("sessionID", session), ("requestID", &id)], None, Some(body.done())).await;
            continue;
        }
        if kind == "form.created" {
            let form = get(&data, "form").cloned().unwrap_or(Json::Null);
            if get_str(&form, "sessionID") == Some(session) {
                let id = get_str(&form, "id").unwrap_or("").to_owned();
                let _ = client.call("session.form.cancel", &[("sessionID", session), ("formID", &id)], Some(json::object()), None).await;
            }
            continue;
        }
        if get_str(&data, "sessionID") != Some(session) {
            continue;
        }
        let mut turn = state.lock().unwrap();
        if kind == "session.inbox.delivered" && get_str(&data, "inboxID") == Some(turn.start.as_str()) {
            turn.started = true;
            continue;
        }
        if !turn.started {
            continue;
        }
        let message = get_str(&data, "assistantMessageID").unwrap_or("").to_owned();
        let call = get_str(&data, "id").unwrap_or("").to_owned();
        let mut update = Obj::new();
        match kind.as_str() {
            "session.step.started" => {
                turn.step_error = None;
                continue;
            }
            "session.step.ended" | "session.step.failed" => {
                if let Some(tokens) = get(&data, "tokens") {
                    turn.usage = Some(match &turn.usage {
                        Some(previous) => add_tokens(previous, tokens),
                        None => tokens.clone(),
                    });
                    turn.last = Some(tokens.clone());
                }
                if kind == "session.step.ended" {
                    turn.finish = get_str(&data, "finish").map(String::from);
                } else {
                    turn.step_error = get(&data, "error").cloned();
                }
                continue;
            }
            "session.execution.succeeded" => return Ok("succeeded"),
            "session.execution.interrupted" => return Ok("interrupted"),
            "session.execution.failed" => {
                turn.execution_error = get(&data, "error").cloned();
                return Ok("failed");
            }
            "session.text.delta" | "session.reasoning.delta" => {
                let text = kind == "session.text.delta";
                let id = if text { message } else { format!("{message}:reasoning:{}", json::text(get(&data, "ordinal").unwrap_or(&Json::Number(0.0)))) };
                let mut content = Obj::new();
                content.str("type", "text").str("text", get_str(&data, "delta").unwrap_or(""));
                update
                    .str("sessionUpdate", if text { "agent_message_chunk" } else { "agent_thought_chunk" })
                    .str("messageId", &id)
                    .set("content", content.done());
            }
            "session.tool.input.started" => {
                let name = get_str(&data, "name").unwrap_or("tool").to_owned();
                turn.tools.insert(call.clone(), (name.clone(), json::object()));
                let input = json::object();
                update
                    .str("sessionUpdate", "tool_call")
                    .str("toolCallId", &call)
                    .str("name", &name)
                    .str("title", &title(&name, &input))
                    .str("kind", tool_kind(&name))
                    .str("status", "pending")
                    .set("locations", locations(&name, &input, cwd))
                    .set("rawInput", raw_input(&name, &input, cwd));
            }
            "session.tool.called" | "session.tool.progress" => {
                if kind == "session.tool.called" {
                    let name = turn.tools.get(&call).map(|t| t.0.clone()).unwrap_or_else(|| "tool".into());
                    turn.tools.insert(call.clone(), (name, get(&data, "input").cloned().unwrap_or_else(json::object)));
                }
                let Some((name, input)) = turn.tools.get(&call).cloned() else { continue };
                update
                    .str("sessionUpdate", "tool_call_update")
                    .str("toolCallId", &call)
                    .str("status", "in_progress")
                    .str("kind", tool_kind(&name))
                    .str("title", &title(&name, &input))
                    .set("locations", locations(&name, &input, cwd))
                    .set("rawInput", raw_input(&name, &input, cwd));
            }
            "session.tool.success" => {
                let (name, input) = turn.tools.remove(&call).unwrap_or_else(|| ("tool".into(), json::object()));
                let content = get(&data, "content").and_then(json::array).unwrap_or(&[]).to_vec();
                let mut primary = tool_content(&content);
                if canonical(&name) == "read" {
                    let first = content.iter().find(|c| get_str(c, "type") == Some("text")).and_then(|c| get_str(c, "text"));
                    if let Some(shown) = first.and_then(super::tool::read_display_text) {
                        {
                            let mut inner = Obj::new();
                            inner.str("type", "text").str("text", &shown);
                            let mut part = Obj::new();
                            part.str("type", "content").set("content", inner.done());
                            let images: Vec<Json> = primary.iter().filter(|p| get(p, "content").and_then(|c| get_str(c, "type")) == Some("image")).cloned().collect();
                            primary = std::iter::once(part.done()).chain(images).collect();
                        }
                    }
                }
                let mut raw = Obj::new();
                raw.opt("metadata", get(&data, "metadata").cloned());
                update
                    .str("sessionUpdate", "tool_call_update")
                    .str("toolCallId", &call)
                    .str("status", "completed")
                    .set("locations", locations(&name, &input, cwd))
                    .set("content", Json::Array(primary))
                    .set("rawOutput", raw.done());
            }
            "session.tool.failed" => {
                let (name, input) = turn.tools.remove(&call).unwrap_or_else(|| ("tool".into(), json::object()));
                let content = get(&data, "content").and_then(json::array).unwrap_or(&[]).to_vec();
                let error = get(&data, "error").and_then(|e| get_str(e, "message")).unwrap_or("").to_owned();
                let mut parts = tool_content(&content);
                let mut inner = Obj::new();
                inner.str("type", "text").str("text", &error);
                let mut part = Obj::new();
                part.str("type", "content").set("content", inner.done());
                parts.push(part.done());
                let mut raw = Obj::new();
                raw.opt("metadata", get(&data, "metadata").cloned()).str("error", &error);
                update
                    .str("sessionUpdate", "tool_call_update")
                    .str("toolCallId", &call)
                    .str("status", "failed")
                    .str("kind", tool_kind(&name))
                    .str("title", &title(&name, &input))
                    .set("locations", locations(&name, &input, cwd))
                    .set("rawInput", raw_input(&name, &input, cwd))
                    .set("content", Json::Array(parts))
                    .set("rawOutput", raw.done());
            }
            _ => continue,
        }
        drop(turn);
        out.update(session, update.done());
    }
}

/// The handler: `--login` is `auth login`; otherwise serve ACP on stdio until stdin ends.
pub async fn command(globals: &super::parse::Globals, input: &Input) -> Result {
    if input.flag("login") {
        return super::auth::command("login", globals, &Input::default()).await;
    }
    // SAFETY: no other thread reads the environment yet.
    unsafe { std::env::set_var("OPENCODE_CLIENT", "acp") };
    let (endpoint, mut private) = connection::standalone().await?;
    let agent = Arc::new(Agent {
        client: Client::new(&endpoint),
        out: Out(Arc::new(Mutex::new(()))),
        catalogs: tokio::sync::Mutex::new(HashMap::new()),
        sessions: Mutex::new(HashMap::new()),
        turns: Mutex::new(HashMap::new()),
        unprompted: Mutex::new(vec![]),
    });
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
        agent.out.send(&o.done());
        return;
    };
    let Some(method) = get_str(&message, "method").map(String::from) else { return };
    let params = get(&message, "params").cloned().unwrap_or_else(json::object);
    let id = get(&message, "id").cloned();
    if method == "session/cancel" {
        agent.cancel(get_str(&params, "sessionId").unwrap_or(""));
        return;
    }
    let Some(id) = id else { return };
    let agent = agent.clone();
    tokio::spawn(async move {
        let (reply, after) = agent.request(&method, &params).await;
        agent.out.respond(&id, reply);
        for (session, update) in after {
            agent.out.update(&session, update);
        }
    });
}
