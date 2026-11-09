//! Terminals (handlers/pty.ts, persistent-pty.ts, pty-socket.ts): a location's `pty.*` terminals and a session's
//! persistent ones (`persistentPty.*`), over the API and their WebSockets.
//!
//! A WebSocket connect checks the origin and a ticket (when it presents one) before anything else, answering a bare
//! 403; then a `pty.*` terminal must exist (404) and a persistent cursor must be a non-negative integer (400) before
//! the upgrade. Once upgraded, a terminal that cannot be attached closes the socket with 4404 (`session exited`,
//! `session not found`, `terminal unavailable`); its end closes it with 1000.

use std::sync::{Arc, Mutex};

use opencode_protocol::Json;
use tokio::sync::mpsc;

use crate::api::Decoded;
use crate::app::App;
use crate::http::websocket::{self, Message, Outbound, Socket};
use crate::http::{Failure, Request, Response, Result};
use crate::json::{self, Obj};
use crate::location::Location;
use crate::persistent_pty::{self, Event};
use crate::pty::{self, ticket};

pub async fn handle(app: &App, request: &Request, decoded: Decoded, located: Option<Arc<Location>>) -> Result<Response> {
    match request.route.operation {
        "pty.list" | "pty.create" | "pty.get" | "pty.update" | "pty.remove" | "pty.connect.token" => {
            terminals(request, &decoded, &located.expect("located endpoint"))
        }
        "pty.connect" => connect(app, request).await,
        operation if operation.starts_with("experimental.persistentPty.") => persistent(request, &decoded).await,
        _ => super::fallback(app, request),
    }
}

fn empty(status: u16) -> Response {
    Response { status, headers: vec![], body: crate::http::Body::Empty }
}

fn not_found(id: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "PtyNotFoundError").str("ptyID", id).str("message", &format!("PTY session not found: {id}"));
    Failure::new(404, o.done())
}

fn forbidden(message: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "ForbiddenError").str("message", message);
    Failure::new(403, o.done())
}

/// `isAllowedRequestOrigin`: no origin, the server's own host, or an allowed CORS origin.
fn allowed_origin(request: &Request) -> bool {
    let Some(origin) = request.header("origin") else { return true };
    if origin.is_empty() {
        return true;
    }
    if let Some(host) = request.header("host").filter(|h| !h.is_empty())
        && let Ok(url) = url::Url::parse(origin)
        && let Some(name) = url.host_str()
    {
        let origin_host = match url.port() {
            Some(port) => format!("{name}:{port}"),
            None => name.to_owned(),
        };
        if origin_host == host {
            return true;
        }
    }
    crate::cors::allowed(Some(origin))
}

/// The header that forces a CORS preflight (`x-opencode-ticket: 1`) and an allowed origin.
fn ticket_request(request: &Request) -> bool {
    request.header("x-opencode-ticket") == Some("1") && allowed_origin(request)
}

/// `Number(text)` as an integer, if it is a safe one.
fn safe_integer(text: &str) -> Option<i64> {
    let number = opencode_protocol::wire::number::js_string_to_number(text);
    opencode_protocol::wire::number::is_safe_integer(number).then_some(number as i64)
}

/// `server.pty` (handlers/pty.ts).
fn terminals(request: &Request, decoded: &Decoded, location: &Arc<Location>) -> Result<Response> {
    let service = &location.pty;
    let id = request.param("ptyID");
    let payload = &decoded.payload;
    match request.route.operation {
        "pty.list" => {
            let items = service.list().iter().map(pty::Info::to_json).collect();
            Ok(super::located_data(location, Json::Array(items)))
        }
        "pty.create" => {
            let strings = |key: &str| {
                json::get(payload, key).and_then(json::array).map(|items| items.iter().filter_map(json::as_str).map(str::to_owned).collect())
            };
            let env = match json::get(payload, "env") {
                Some(Json::Object(env)) => env.iter().filter_map(|(k, v)| json::as_str(v).map(|v| (k.clone(), v.to_owned()))).collect(),
                _ => vec![],
            };
            let input = pty::CreateInput {
                command: json::get_str(payload, "command").map(str::to_owned),
                args: strings("args").unwrap_or_default(),
                cwd: json::get_str(payload, "cwd").filter(|c| !c.is_empty()).map(str::to_owned),
                title: json::get_str(payload, "title").map(str::to_owned),
                env,
            };
            let inventory = location.inventory();
            let shell = || crate::shell::select::resolve(crate::shell::select::Priority::Config, &inventory);
            let info = service.create(input, &location.directory, shell).map_err(crate::app::defect)?;
            Ok(super::located_data(location, info.to_json()))
        }
        "pty.get" => {
            let info = service.get(id).map_err(|_| not_found(id))?;
            Ok(super::located_data(location, info.to_json()))
        }
        "pty.update" => {
            let size = json::get(payload, "size").and_then(|size| {
                Some((json::get_num(size, "rows")? as u16, json::get_num(size, "cols")? as u16))
            });
            let info = service.update(id, json::get_str(payload, "title"), size).map_err(|_| not_found(id))?;
            Ok(super::located_data(location, info.to_json()))
        }
        "pty.remove" => {
            service.remove(id).map_err(|_| not_found(id))?;
            Ok(Response::no_content())
        }
        "pty.connect.token" => {
            // The custom header forces a CORS preflight, so cross-origin pages cannot mint tickets past the origin
            // policy.
            if !ticket_request(request) {
                return Err(forbidden("Invalid PTY connect token request"));
            }
            service.get(id).map_err(|_| not_found(id))?;
            let scope = ticket::Scope::pty(id, Some(location.directory.clone()), location.workspace_id.clone());
            Ok(super::located_data(location, ticket::issue(scope)))
        }
        _ => unreachable!("pty operation"),
    }
}

/// The request's pending upgrade with its handshake answer, or `None` when it asks for no WebSocket.
fn upgrade(request: &Request) -> Option<(hyper::upgrade::OnUpgrade, Response)> {
    let accept = websocket::accept(&request.headers)?;
    let pending = request.upgrade.lock().unwrap().take()?;
    Some((pending, websocket::switching(accept)))
}

/// `pty.connect`: the terminal's output (replay, the cursor frame, then live) and its input.
async fn connect(app: &App, request: &Request) -> Result<Response> {
    if !allowed_origin(request) {
        return Ok(empty(403));
    }
    let reference = crate::location::canonical(&crate::api::request_location(app, request));
    let id = request.param("ptyID").to_owned();
    if let Some(presented) = request.query_value(ticket::QUERY).filter(|t| !t.is_empty()) {
        let scope = ticket::Scope::pty(&id, Some(reference.directory.clone()), reference.workspace_id.clone());
        if !ticket::consume(presented, &scope) {
            return Ok(empty(403));
        }
    }
    let location = app.locations.get(app, &reference).await?;
    if location.pty.get(&id).is_err() {
        return Ok(empty(404));
    }
    let cursor = request.query_value("cursor").and_then(safe_integer).filter(|c| *c >= -1);
    // `Effect.orDie(request.upgrade)`.
    let Some((pending, switching)) = upgrade(request) else { return Err(Failure::defect()) };
    let (outbox, messages) = mpsc::unbounded_channel();
    let attachment = match location.pty.attach(&id, cursor, outbox.clone()) {
        Ok(attachment) => Some(attachment),
        Err(error) => {
            let reason = if error == pty::Error::Exited { "session exited" } else { "session not found" };
            let _ = outbox.send(Outbound::Close(4404, reason.into()));
            None
        }
    };
    drop(outbox);
    // The socket keeps its location from the idle sweep while it stays open (`holdLocation`).
    let retain = crate::location_activity::Retain::new(location);
    tokio::spawn(async move {
        let _retain = retain;
        if let Some(socket) = Socket::open(pending).await {
            socket
                .run(messages, |message| {
                    // Text as it is; binary as UTF-8 (dropped when invalid).
                    let text = match message {
                        Message::Text(text) => Some(text),
                        Message::Binary(bytes) => String::from_utf8(bytes).ok(),
                    };
                    if let (Some(attachment), Some(text)) = (&attachment, text) {
                        attachment.write(&text);
                    }
                    std::future::ready(())
                })
                .await;
        }
        if let Some(attachment) = &attachment {
            attachment.detach();
        }
    });
    Ok(switching)
}

fn unavailable(message: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "ServiceUnavailableError").str("message", message).str("service", "opencode-pty");
    Failure::new(503, o.done())
}

/// `mapTerminalError` (and `mapUnavailable`, for operations without a terminal).
fn terminal_error(error: persistent_pty::Error) -> Failure {
    match error {
        persistent_pty::Error::NotFound(id) => not_found(&id),
        persistent_pty::Error::Unavailable(message) => unavailable(&message),
        persistent_pty::Error::InvalidCwd(message) => Failure::invalid(message, Some("cwd"), Some("invalid_cwd")),
        persistent_pty::Error::Unsupported(feature) => {
            let mut o = Obj::new();
            o.str("_tag", "PtyFeatureUnsupportedError").str("feature", &feature).str(
                "message",
                "Terminal renaming will be available after OpenCode's terminal service restarts",
            );
            Failure::new(501, o.done())
        }
    }
}

/// `server.experimental`'s persistent terminals (handlers/persistent-pty.ts).
async fn persistent(request: &Request, decoded: &Decoded) -> Result<Response> {
    let service = persistent_pty::service();
    let operation = request.route.operation.trim_start_matches("experimental.persistentPty.");
    let session_id = request.param("sessionID");
    let id = request.param("ptyID");
    let payload = &decoded.payload;
    match operation {
        "read" => {
            // `NumberFromString`: the decoded query keeps its string form.
            let lines = match json::get(&decoded.query, "lines") {
                Some(Json::Number(n)) => Some(*n),
                Some(Json::String(text)) => Some(opencode_protocol::wire::number::js_string_to_number(text)),
                _ => None,
            };
            Ok(super::data(service.read(session_id, lines).await.map_err(terminal_error)?))
        }
        "list" => {
            let terminals = service.list(Some(session_id)).await.map_err(terminal_error)?;
            Ok(super::data(Json::Array(terminals.into_iter().map(|t| t.json).collect())))
        }
        "create" => {
            let size = json::get(payload, "size");
            let input = persistent_pty::CreateInput {
                command: json::get_str(payload, "command").map(str::to_owned),
                args: json::get(payload, "args").cloned().unwrap_or(Json::Array(vec![])),
                cwd: json::get_str(payload, "cwd").map(str::to_owned),
                title: json::get_str(payload, "title").unwrap_or_default().to_owned(),
                env: json::get(payload, "env").cloned().unwrap_or_else(json::object),
                cols: size.and_then(|s| json::get_num(s, "cols")),
                rows: size.and_then(|s| json::get_num(s, "rows")),
            };
            Ok(super::data(service.create(session_id, input).await.map_err(terminal_error)?.json))
        }
        "shutdown" => {
            service.shutdown().await.map_err(terminal_error)?;
            Ok(Response::no_content())
        }
        "handoff" => {
            let handoff = service.handoff().await.map_err(terminal_error)?;
            let mut o = Obj::new();
            o.set("handoff", handoff.map(|h| h.to_json()).unwrap_or(Json::Null));
            Ok(Response::value(200, &o.done()))
        }
        "get" => Ok(super::data(service.get(id).await.map_err(terminal_error)?.json)),
        "update" => {
            if let Some(size) = json::get(payload, "size") {
                let (cols, rows) = (json::get_num(size, "cols").unwrap_or(0.0), json::get_num(size, "rows").unwrap_or(0.0));
                service.resize(id, cols, rows, json::get_str(payload, "attachmentID")).await.map_err(terminal_error)?;
            }
            let info = match json::get(payload, "title") {
                None => service.get(id).await,
                Some(title) => service.rename(id, json::as_str(title)).await,
            };
            Ok(super::data(info.map_err(terminal_error)?.json))
        }
        "snapshot" => Ok(super::data(service.snapshot(id).await.map_err(terminal_error)?)),
        "remove" => {
            service.remove(id).await.map_err(terminal_error)?;
            Ok(Response::no_content())
        }
        "connectToken" => {
            if !ticket_request(request) {
                return Err(forbidden("Invalid persistent PTY connect token request"));
            }
            service.get(id).await.map_err(terminal_error)?;
            Ok(super::data(ticket::issue(ticket::Scope::pty(id, None, None))))
        }
        "connect" => persistent_connect(request),
        _ => Err(crate::api::not_implemented(request.route.operation)),
    }
}

/// `persistentPty.connect`: the terminal's stream as JSON control messages (text) and output (binary), and its input.
fn persistent_connect(request: &Request) -> Result<Response> {
    if !allowed_origin(request) {
        return Ok(empty(403));
    }
    let id = request.param("ptyID").to_owned();
    if let Some(presented) = request.query_value(ticket::QUERY).filter(|t| !t.is_empty())
        && !ticket::consume(presented, &ticket::Scope::pty(&id, None, None))
    {
        return Ok(empty(403));
    }
    let cursor = opencode_protocol::wire::number::js_string_to_number(request.query_value("cursor").unwrap_or("0"));
    let role = if request.query_value("role") == Some("observer") { "observer" } else { "controller" };
    let framed = request.query_value("input_protocol") == Some("1");
    let attachment_id = request.query_value("attachment_id").map(str::to_owned).unwrap_or_else(crate::id::uuid);
    let takeover = request.query_value("takeover") == Some("true");
    if !opencode_protocol::wire::number::is_safe_integer(cursor) || cursor < 0.0 {
        return Ok(empty(400));
    }
    let Some((pending, switching)) = upgrade(request) else { return Err(Failure::defect()) };
    tokio::spawn(async move {
        let Some(socket) = Socket::open(pending).await else { return };
        let service = persistent_pty::service();
        let (outbox, messages) = mpsc::unbounded_channel();
        // A client without framed input never reports its size: its input carries the latest the socket saw (D224).
        let size: Arc<Mutex<Option<(u16, u16)>>> = Arc::new(Mutex::new(None));
        // The upgrade comes first; the daemon is asked once the socket is open.
        let activation = match service.attach(&id, cursor, &attachment_id, role, takeover).await {
            Err(_) => {
                let _ = outbox.send(Outbound::Close(4404, "terminal unavailable".into()));
                None
            }
            Ok(mut attached) => {
                *size.lock().unwrap() = Some((attached.info.cols, attached.info.rows));
                let mut message = Obj::new();
                message.str("type", "attached").str("attachmentID", &attachment_id);
                message.set("inputProtocol", Json::Number(if framed { 1.0 } else { 0.0 }));
                message.set("info", attached.info.json.clone()).str("role", &attached.role);
                message.set("generation", Json::Number(attached.generation)).set("replay", attached.replay.clone());
                let _ = outbox.send(Outbound::Text(json::text(&message.done())));
                if !attached.data.is_empty() {
                    let _ = outbox.send(Outbound::Binary(std::mem::take(&mut attached.data).into()));
                }
                let mut complete = Obj::new();
                complete.str("type", "replay_complete").set("endOffset", Json::Number(attached.end_offset));
                let _ = outbox.send(Outbound::Text(json::text(&complete.done())));
                let (events, ended, sizes) = (outbox.clone(), outbox.clone(), size.clone());
                let on_event = move |event: Event| {
                    let _ = events.send(stream_message(event, &sizes));
                };
                let on_end = move || {
                    let _ = ended.send(Outbound::Close(1000, String::new()));
                };
                Some(service.activate(id.clone(), attached, on_event, on_end))
            }
        };
        drop(outbox);
        let attached = activation.is_some();
        socket
            .run(messages, |message| {
                let data = match message {
                    Message::Text(text) => text.into_bytes(),
                    Message::Binary(bytes) => bytes,
                };
                let (id, attachment_id, size) = (id.clone(), attachment_id.clone(), size.clone());
                async move {
                    if !attached {
                        return;
                    }
                    if !framed {
                        let Some((cols, rows)) = *size.lock().unwrap() else { return };
                        let _ = service.input(&id, &attachment_id, cols, rows, &data).await;
                        return;
                    }
                    if data.len() < 5 {
                        return;
                    }
                    let kind = data[0];
                    let cols = u16::from_be_bytes([data[1], data[2]]);
                    let rows = u16::from_be_bytes([data[3], data[4]]);
                    if (kind != 0 && kind != 1) || cols == 0 || rows == 0 {
                        return;
                    }
                    if kind == 0 {
                        let _ = service.control(&id, &attachment_id, cols, rows).await;
                    } else {
                        let _ = service.input(&id, &attachment_id, cols, rows, &data[5..]).await;
                    }
                }
            })
            .await;
        if let Some(activation) = activation {
            activation.detach();
        }
    });
    Ok(switching)
}

/// A stream event as the socket sends it (`resized` with its checkpoint in base64, and the size it reports kept).
fn stream_message(event: Event, size: &Mutex<Option<(u16, u16)>>) -> Outbound {
    let mut o = Obj::new();
    match event {
        Event::Output(data) => return Outbound::Binary(data.into()),
        Event::Resized { cols, rows, generation, checkpoint } => {
            *size.lock().unwrap() = Some((cols as u16, rows as u16));
            o.str("type", "resized").set("cols", Json::Number(cols)).set("rows", Json::Number(rows));
            o.set("generation", Json::Number(generation)).str("checkpoint", &checkpoint);
        }
        Event::Exited { exit_code, final_offset } => {
            o.str("type", "exited").opt("exitCode", exit_code.map(Json::Number)).set("finalOffset", Json::Number(final_offset));
        }
        Event::ControllerChanged { attachment_id, generation } => {
            o.str("type", "controller_changed").opt_str("attachmentID", attachment_id.as_deref());
            o.set("generation", Json::Number(generation));
        }
        Event::TitleChanged(title) => {
            o.str("type", "title_changed").str("title", &title);
        }
        Event::ForegroundProcessChanged(process) => {
            o.str("type", "foreground_process_changed").set("process", process);
        }
    }
    Outbound::Text(json::text(&o.done()))
}
