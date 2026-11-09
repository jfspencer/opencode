//! Sessions and their messages (handlers/session.ts, message.ts).

use std::sync::Arc;

use bytes::Bytes;
use opencode_protocol::Json;
use opencode_protocol::Wire;
use opencode_protocol::types as t;

use super::data;
use crate::api::{Decoded, event_stream};
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
        "session.remove" => session::remove(app, request.param("sessionID")).await.map(|_| Response::no_content()),
        "session.background" => {
            session::subagent::background_session(app, request.param("sessionID")).await?;
            Ok(Response::no_content())
        }
        "session.active" => {
            let mut active = Obj::new();
            for id in app.sessions.executions.active() {
                let mut running = Obj::new();
                running.str("type", "running");
                active.set(&id, running.done());
            }
            Ok(data(active.done()))
        }
        "session.prompt" => {
            let admitted = session::prompt::submit(app, request.param("sessionID"), &decoded.payload).await?;
            Ok(data(wire_inbox(&admitted)))
        }
        "session.command" => {
            session::command::execute(app, request.param("sessionID"), &decoded.payload).await?;
            Ok(Response::no_content())
        }
        "session.synthetic" => synthetic(app, request, &decoded),
        "session.switchAgent" => {
            let session_id = request.param("sessionID");
            let stored = session::require(&app.db.lock().unwrap(), session_id)?;
            let agent = json::get_str(&decoded.payload, "agent").unwrap_or_default().to_owned();
            // Agents are registered by the location's plugins.
            let location = app.locations.get(app, &stored.location).await?;
            if location.inventory().agent(&agent).is_none() {
                let mut o = Obj::new();
                o.str("_tag", "AgentNotFoundError").str("agentID", &agent).str("message", &format!("Agent not found: {agent}"));
                return Err(Failure::new(404, o.done()));
            }
            session::prompt::switch_agent(app, session_id, &agent, None)?;
            Ok(Response::no_content())
        }
        "session.switchModel" => {
            let model = json::get(&decoded.payload, "model").cloned().unwrap_or(Json::Null);
            session::prompt::switch_model(app, request.param("sessionID"), model, None)?;
            Ok(Response::no_content())
        }
        "session.compact" => {
            let session_id = request.param("sessionID");
            let stored = session::require(&app.db.lock().unwrap(), session_id)?;
            session::revert::commit(app, &stored, None)?;
            let input_id = json::get_str(&decoded.payload, "id").map(str::to_owned).unwrap_or_else(session::message::create_id);
            let delivery = json::get_str(&decoded.payload, "delivery").unwrap_or("steer");
            match session::inbox::admit_compaction(app, &input_id, session_id, delivery)? {
                Ok(admitted) => {
                    app.sessions.executions.wake(app, session_id, session::inbox::Promotable::Input);
                    Ok(data(wire_inbox(&admitted)))
                }
                Err(_) => Err(session::prompt::conflict(
                    format!("Compaction input ID conflicts with an existing durable record: {input_id}"),
                    &input_id,
                )),
            }
        }
        "session.message.list" => messages(app, request, &decoded),
        "session.message.get" => {
            let session_id = request.param("sessionID");
            let message_id = request.param("messageID");
            let db = app.db.lock().unwrap();
            session::require(&db, session_id)?;
            match session::message::get(&db, message_id).map_err(fatal)? {
                Some((owner, message)) if owner == session_id => Ok(data(session::message::wire(&message))),
                _ => {
                    let mut o = Obj::new();
                    o.str("_tag", "MessageNotFoundError").str("sessionID", session_id).str("messageID", message_id);
                    o.str("message", &format!("Message not found: {message_id}"));
                    Err(Failure::new(404, o.done()))
                }
            }
        }
        "session.interrupt" => {
            let session_id = request.param("sessionID");
            session::require(&app.db.lock().unwrap(), session_id)?;
            let resume = matches!(json::get(&decoded.query, "resume"), Some(Json::Bool(true)))
                || json::get_str(&decoded.query, "resume") == Some("true");
            let interrupted = app.sessions.executions.interrupt(session_id, "user");
            if resume {
                // Resume steering input and between-turn controls of the interrupted intent; queued prompts stay parked.
                let next = session::inbox::next_promotable(&app.db.lock().unwrap(), session_id, session::inbox::Promotable::Input)
                    .map_err(fatal)?;
                if next.is_some_and(|n| n.delivery == "steer" || n.control()) {
                    app.sessions.executions.wake(app, session_id, session::inbox::Promotable::Steer);
                }
            }
            let mut o = Obj::new();
            o.set("interrupted", Json::Bool(interrupted));
            Ok(Response::value(200, &o.done()))
        }
        "session.inbox.list" => {
            let session_id = request.param("sessionID");
            let db = app.db.lock().unwrap();
            session::require(&db, session_id)?;
            let items = session::inbox::list(&db, session_id).map_err(fatal)?;
            Ok(data(Json::Array(items.iter().map(wire_inbox).collect())))
        }
        "session.inbox.cancel" => {
            let session_id = request.param("sessionID");
            session::require(&app.db.lock().unwrap(), session_id)?;
            let _ = session::inbox::mutate(app, session_id, request.param("inboxID"), "session.inbox.cancelled", None);
            Ok(Response::no_content())
        }
        "session.inbox.update" => {
            let session_id = request.param("sessionID");
            let inbox_id = request.param("inboxID");
            let delivery = json::get_str(&decoded.payload, "delivery").unwrap_or("steer").to_owned();
            let result = session::inbox::mutate(app, session_id, inbox_id, "session.inbox.delivery.changed", Some(&delivery));
            if result.is_err() {
                session::require(&app.db.lock().unwrap(), session_id)?;
                return Err(session::prompt::conflict(format!("Pending input cannot change to {delivery}: {inbox_id}"), inbox_id));
            }
            if delivery == "steer" {
                app.sessions.executions.wake(app, session_id, session::inbox::Promotable::Input);
            }
            Ok(Response::no_content())
        }
        "session.view" => {
            let idle = match json::get(&decoded.payload, "idle") {
                Some(Json::Number(n)) => *n as i64,
                _ => 0,
            };
            session::view(app, request.param("sessionID"), idle).map(|_| Response::no_content())
        }
        "experimental.session.wait" => {
            let session_id = request.param("sessionID");
            session::require(&app.db.lock().unwrap(), session_id)?;
            app.sessions.executions.await_idle(session_id).await;
            Ok(Response::no_content())
        }
        "experimental.session.log" => session_log(app, request, &decoded),
        "session.generate" => {
            let prompt = json::get_str(&decoded.payload, "prompt").unwrap_or_default().to_owned();
            Ok(data(crate::generate::session(app, request.param("sessionID"), &prompt).await?))
        }
        "session.move" => {
            session::moves::request(app, request.param("sessionID"), &decoded.payload).await?;
            Ok(Response::no_content())
        }
        "session.context" => {
            let db = app.db.lock().unwrap();
            let session_id = request.param("sessionID");
            session::require(&db, session_id)?;
            // `SessionHistory.load(.., "latest")`. TODO(port): from the latest completed compaction on.
            let history = session::message::history(&db, session_id).map_err(fatal)?;
            Ok(data(Json::Array(history.iter().map(session::message::wire).collect())))
        }
        "experimental.session.instructions.entry.list" => {
            let db = app.db.lock().unwrap();
            let session_id = request.param("sessionID");
            session::require(&db, session_id)?;
            Ok(data(session::entries::list(&db, session_id).map_err(fatal)?))
        }
        "experimental.session.instructions.entry.put" => {
            let session_id = request.param("sessionID");
            session::require(&app.db.lock().unwrap(), session_id)?;
            let value = json::get(&decoded.payload, "value").cloned().unwrap_or(Json::Null);
            session::entries::put(app, session_id, request.param("key"), &value)?;
            Ok(Response::no_content())
        }
        "experimental.session.instructions.entry.remove" => {
            let session_id = request.param("sessionID");
            session::require(&app.db.lock().unwrap(), session_id)?;
            session::entries::remove(app, session_id, request.param("key"))?;
            Ok(Response::no_content())
        }
        "experimental.session.skill" => {
            session::skill::activate(app, request.param("sessionID"), &decoded.payload).await?;
            Ok(Response::no_content())
        }
        "session.revert.stage" => {
            let payload = &decoded.payload;
            let message_id = json::get_str(payload, "messageID").unwrap_or_default();
            let files = json::get_bool(payload, "files");
            Ok(data(session::revert::stage(app, request.param("sessionID"), message_id, files).await?))
        }
        "session.revert.clear" => {
            session::revert::clear(app, request.param("sessionID")).await?;
            Ok(Response::no_content())
        }
        "session.revert.commit" => {
            session::revert::commit_staged(app, request.param("sessionID"))?;
            Ok(Response::no_content())
        }
        "session.diff" => Ok(data(session::diff::turn(app, request.param("sessionID"), &decoded.query).await?)),
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
        let located = app.locations.get(app, &target).await?;
        if located.inventory().agent(agent).is_none() {
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
            session::title::generate(&app.arc(), id).await;
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

/// An inbox item through the response schema.
fn wire_inbox(item: &Json) -> Json {
    match t::SessionInboxInfo::decode(item) {
        Ok(info) => json::of(&info),
        Err(_) => item.clone(),
    }
}

/// `Session.synthetic`: synthetic input admitted like a prompt (wakes unless `resume` is `false` or a revert is
/// staged).
fn synthetic(app: &App, request: &Request, decoded: &Decoded) -> Result<Response> {
    let session_id = request.param("sessionID");
    let stored = session::require(&app.db.lock().unwrap(), session_id)?;
    let payload = &decoded.payload;
    let present = |key: &str| json::get(payload, key).filter(|v| !matches!(v, Json::Null)).cloned();
    let input_id = json::get_str(payload, "id").map(str::to_owned).unwrap_or_else(session::message::create_id);
    let mut item_payload = Obj::new();
    item_payload.str("text", json::get_str(payload, "text").unwrap_or_default());
    item_payload.opt("description", present("description"));
    item_payload.opt("metadata", present("metadata"));
    let mut item = Obj::new();
    item.str("type", "synthetic").set("payload", item_payload.done());
    item.str("delivery", json::get_str(payload, "delivery").unwrap_or("steer"));
    // Synthetic input is admitted outside the session's location services, so its events carry no location.
    let _ = &stored;
    let admitted = match session::inbox::admit(app, &input_id, session_id, item.done(), None)? {
        Ok(admitted) => admitted,
        Err(_) => {
            return Err(session::prompt::conflict(
                format!("Synthetic input ID conflicts with an existing durable record: {input_id}"),
                &input_id,
            ));
        }
    };
    let reverted =
        session::get(&app.db.lock().unwrap(), session_id).map_err(fatal)?.is_some_and(|s| json::get(&s.info, "revert").is_some());
    if json::get_bool(payload, "resume") != Some(false) && !reverted {
        app.sessions.executions.wake(app, session_id, session::inbox::Promotable::Input);
    }
    Ok(data(wire_inbox(&admitted)))
}

const DEFAULT_MESSAGES_LIMIT: i64 = 50;

fn invalid_cursor(message: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "InvalidCursorError").str("message", message);
    Failure::new(400, o.done())
}

/// A message page cursor (`{id, order, direction}` as base64url JSON).
fn message_cursor(id: &str, order: &str, direction: &str) -> String {
    use base64::Engine;
    let mut o = Obj::new();
    o.str("id", id).str("order", order).str("direction", direction);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json::text(&o.done()))
}

fn parse_message_cursor(text: &str) -> Option<(String, String, String)> {
    use base64::Engine;
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let bytes = LENIENT.decode(text).ok()?;
    let value = json::parse(&String::from_utf8_lossy(&bytes))?;
    let id = json::get_str(&value, "id").filter(|id| id.starts_with("msg_"))?.to_owned();
    let order = json::get_str(&value, "order").filter(|o| *o == "asc" || *o == "desc")?.to_owned();
    let direction = json::get_str(&value, "direction").filter(|d| *d == "previous" || *d == "next")?.to_owned();
    Some((id, order, direction))
}

/// `GET /api/session/:id/message`: one page of messages in order, with cursors to move through the timeline.
fn messages(app: &App, request: &Request, decoded: &Decoded) -> Result<Response> {
    let session_id = request.param("sessionID");
    let query = &decoded.query;
    let cursor = json::get_str(query, "cursor").filter(|c| !c.is_empty());
    let requested_order = json::get_str(query, "order");
    if cursor.is_some() && requested_order.is_some() {
        return Err(invalid_cursor("Cursor cannot be combined with order"));
    }
    let decoded_cursor = match cursor {
        Some(text) => Some(parse_message_cursor(text).ok_or_else(|| invalid_cursor("Invalid cursor"))?),
        None => None,
    };
    let order =
        decoded_cursor.as_ref().map(|c| c.1.clone()).or(requested_order.map(str::to_owned)).unwrap_or_else(|| "desc".into());
    let limit = match json::get(query, "limit") {
        Some(Json::String(s)) => s.parse::<f64>().ok().map(|n| n as i64),
        Some(Json::Number(n)) => Some(*n as i64),
        _ => None,
    }
    .unwrap_or(DEFAULT_MESSAGES_LIMIT);
    let db = app.db.lock().unwrap();
    session::require(&db, session_id)?;
    let input = session::message::ListInput {
        session: session_id.to_owned(),
        limit: Some(limit),
        order: order.clone(),
        ty: json::get_str(query, "type").map(str::to_owned),
        cursor: decoded_cursor.map(|(id, _, direction)| (id, direction)),
    };
    let rows = session::message::list(&db, &input).map_err(fatal)?;
    drop(db);
    let id = |m: &Json| json::get_str(m, "id").unwrap_or_default().to_owned();
    let mut cursors = Obj::new();
    cursors.set("previous", rows.first().map(|m| Json::String(message_cursor(&id(m), &order, "previous"))).unwrap_or(Json::Null));
    cursors.set("next", rows.last().map(|m| Json::String(message_cursor(&id(m), &order, "next"))).unwrap_or(Json::Null));
    let mut o = Obj::new();
    o.set("data", Json::Array(rows.iter().map(session::message::wire).collect()));
    o.set("cursor", cursors.done());
    Ok(Response::value(200, &o.done()))
}
