//! Sessions and their messages (handlers/session.ts, message.ts).

use std::sync::Arc;

use bytes::Bytes;
use opencode_protocol::Json;
use opencode_protocol::Wire;
use opencode_protocol::types as t;

use super::data;
use crate::api::{Decoded, event_stream, not_implemented};
use crate::app::{App, fatal};
use crate::bus::LocationRef;
use crate::http::{Failure, Request, Response, Result};
use crate::json::{self, Obj};
use crate::location::Location;
use crate::session::{self, CreateInput, ListFilter};

pub async fn handle(app: &App, request: &Request, decoded: Decoded, _located: Option<Arc<Location>>) -> Result<Response> {
    let operation = request.route.operation;
    match operation {
        "session.list" => session_list(app, &decoded),
        "session.create" => session_create(app, &decoded).await,
        "session.get" => {
            let stored = session::require(&app.db.lock().unwrap(), request.param("sessionID"))?;
            Ok(data(stored.wire()))
        }
        "session.update" => session_update(app, request, &decoded).await,
        "session.remove" => session::remove(app, request.param("sessionID")).map(|_| Response::no_content()),
        "session.active" => {
            // TODO(port): execution; nothing runs yet.
            Ok(data(Obj::new().done()))
        }
        "session.view" => {
            let idle = match json::get(&decoded.payload, "idle") {
                Some(Json::Number(n)) => *n as i64,
                _ => 0,
            };
            session::view(app, request.param("sessionID"), idle).map(|_| Response::no_content())
        }
        "experimental.session.wait" => {
            // TODO(port): execution; a session is always idle.
            session::require(&app.db.lock().unwrap(), request.param("sessionID"))?;
            Ok(Response::no_content())
        }
        "experimental.session.log" => session_log(app, request, &decoded),
        "session.revert.stage" => {
            let session_id = request.param("sessionID");
            let message_id = json::get_str(&decoded.payload, "messageID").unwrap_or_default();
            if !session::has_message(&app.db.lock().unwrap(), session_id, message_id).map_err(fatal)? {
                let mut o = Obj::new();
                o.str("_tag", "MessageNotFoundError").str("sessionID", session_id).str("messageID", message_id);
                o.str("message", &format!("Message not found: {message_id}"));
                return Err(Failure::new(404, o.done()));
            }
            // TODO(port): staging a revert (snapshots).
            Err(not_implemented(operation))
        }
        _ => super::fallback(app, request),
    }
}

/// `GET /api/experimental/session/:sessionID/log`: the durable log after `after` (empty: payloads are not retained),
/// then the `log.synced` marker; with `follow`, the stream stays open.
fn session_log(app: &App, request: &Request, decoded: &Decoded) -> Result<Response> {
    let id = request.param("sessionID").to_owned();
    let latest = {
        let db = app.db.lock().unwrap();
        session::require(&db, &id)?;
        crate::bus::latest_sequence(&db, &id).map_err(fatal)?
    };
    let follow = matches!(json::get(&decoded.query, "follow"), Some(Json::Bool(true)));
    let mut marker = Obj::new();
    marker.str("type", "log.synced").str("aggregateID", &id);
    if latest >= 0 {
        marker.int("seq", latest);
    }
    let frame = format!("data: {}\n\n", json::text(&marker.done()));
    let (sender, response) = event_stream();
    tokio::spawn(async move {
        if sender.send(Bytes::from(frame)).await.is_err() || !follow {
            return;
        }
        sender.closed().await;
    });
    Ok(response)
}

fn relative_directory(field: &str) -> Failure {
    Failure::invalid(format!("{field} must be an absolute path"), Some(field), None)
}

fn session_list(app: &App, decoded: &Decoded) -> Result<Response> {
    let query = t::SessionsQuery::decode(&decoded.query).map_err(|issue| Failure::schema("Query", &issue))?;
    if let Some(directory) = &query.directory {
        if !directory.0.starts_with('/') {
            return Err(relative_directory("directory"));
        }
    }
    let filter = match &query.cursor {
        Some(cursor) => ListFilter::parse_cursor(&cursor.0).ok_or_else(|| {
            let mut o = Obj::new();
            o.str("_tag", "InvalidCursorError").str("message", "Invalid cursor");
            Failure::new(400, o.done())
        })?,
        None => ListFilter {
            order: query.order.as_ref().map(|o| match o {
                t::SessionsQueryOrder0::Asc => "asc".to_owned(),
                t::SessionsQueryOrder0::Desc => "desc".to_owned(),
            }),
            search: query.search.clone(),
            parent_id: query.parent_id.as_ref().map(session::query_parent),
            archived: query.archived.as_ref().map(|a| matches!(a, t::SessionsQueryArchived0::True2)),
            directory: query.directory.as_ref().map(|d| d.0.clone()),
            project: query.project.as_ref().map(|p| p.0.clone()),
            subpath: query.subpath.as_ref().map(|p| p.0.clone()),
            anchor: None,
        },
    };
    let page = session::list(app, filter, query.limit.as_ref().map(|l| l.0))?;
    // The handler returns both keys; the JSON codec writes an undefined optional value as null.
    let text = |cursor: Option<String>| cursor.map(Json::String).unwrap_or(Json::Null);
    let mut cursor = Obj::new();
    cursor.set("previous", text(page.previous)).set("next", text(page.next));
    let mut o = Obj::new();
    o.set("data", Json::Array(page.data.iter().map(|s| s.wire()).collect()));
    o.set("cursor", cursor.done());
    Ok(Response::value(200, &o.done()))
}

/// Built-in agents (packages/core/src/agent.ts). TODO(port): agents from config and markdown files.
const BUILT_IN_AGENTS: &[&str] = &["build", "general", "explore", "compaction", "title", "summary", "plan"];

async fn session_create(app: &App, decoded: &Decoded) -> Result<Response> {
    let payload = &decoded.payload;
    let present = |key: &str| json::get(payload, key).filter(|v| !matches!(v, Json::Null)).cloned();
    let location = present("location");
    let directory = location.as_ref().and_then(|l| json::get_str(l, "directory")).map(str::to_owned);
    if directory.as_ref().is_some_and(|d| !d.starts_with('/')) {
        return Err(relative_directory("location.directory"));
    }
    let parent_id = json::get_str(payload, "parentID").map(str::to_owned);
    let location = LocationRef::directory(directory.unwrap_or_else(|| app.cwd.clone()));
    if let Some(agent) = json::get_str(payload, "agent") {
        // Agents are registered by the location's plugins.
        let target = match &parent_id {
            None => crate::location::canonical(&location),
            Some(parent) => session::require(&app.db.lock().unwrap(), parent)?.location,
        };
        app.locations.get(app, &target).await?;
        if !BUILT_IN_AGENTS.contains(&agent) {
            let mut o = Obj::new();
            o.str("_tag", "AgentNotFoundError").str("agentID", agent).str("message", &format!("Agent not found: {agent}"));
            return Err(Failure::new(404, o.done()));
        }
    }
    let stored = session::create(
        app,
        CreateInput {
            id: json::get_str(payload, "id").map(str::to_owned),
            title: json::get_str(payload, "title").map(str::to_owned),
            agent: json::get_str(payload, "agent").map(str::to_owned),
            model: present("model"),
            metadata: present("metadata"),
            permissions: present("permissions"),
            location: if parent_id.is_none() { Some(location) } else { None },
            parent_id,
        },
    )?;
    Ok(data(stored.wire()))
}

async fn session_update(app: &App, request: &Request, decoded: &Decoded) -> Result<Response> {
    let id = request.param("sessionID");
    let info = session::require(&app.db.lock().unwrap(), id)?;
    let payload = &decoded.payload;
    let present = |key: &str| json::get(payload, key).filter(|v| !matches!(v, Json::Null)).cloned();
    if let Some(Json::String(title)) = present("title") {
        if title.is_empty() {
            // The title is generated from the conversation once the location's plugins (the title agent) are up.
            app.locations.get(app, &info.location).await?;
            // TODO(port): title generation; a session without messages generates nothing.
        } else {
            session::rename(app, id, &title)?;
        }
    }
    if let Some(metadata) = present("metadata") {
        session::set_metadata(app, id, metadata)?;
    }
    if let Some(permissions) = present("permissions") {
        session::set_permissions(app, id, permissions)?;
    }
    if let Some(Json::Bool(archived)) = present("archived") {
        session::archive(app, id, archived)?;
    }
    Ok(Response::no_content())
}
