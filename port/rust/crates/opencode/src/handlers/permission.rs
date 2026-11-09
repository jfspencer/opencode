//! Permission requests and saved rules (handlers/permission.ts): a location's pending requests (listed for the
//! location or a session, read, answered, or asked through the API), and the project's saved approvals
//! (permission/saved.ts: listed, edited, removed).

use std::sync::Arc;

use opencode_protocol::Json;

use super::{data, encoded, located};
use crate::api::Decoded;
use crate::app::{App, defect, fatal};
use crate::http::{Failure, Request, Response, Result};
use crate::json::{self, Obj};
use crate::location::Location;
use crate::permission::{self, Ask, AskError, Reply, Requester, Source};

pub async fn handle(app: &App, request: &Request, decoded: Decoded, located_in: Option<Arc<Location>>) -> Result<Response> {
    let location = || located_in.clone().expect("the endpoint's middleware loads its location");
    match request.route.operation {
        "permission.request.list" => {
            let location = location();
            Ok(located(request, &location, Json::Array(location.permission.list())))
        }
        "session.permission.create" => create(app, request, &decoded, &location()),
        "session.permission.list" => {
            let session_id = request.param("sessionID");
            let stored = crate::session::require(&app.db.lock().unwrap(), session_id)?;
            let location = app.locations.get(app, &stored.location).await?;
            Ok(encoded(request, &data_of(Json::Array(location.permission.for_session(session_id)))))
        }
        "session.permission.get" => {
            let location = location();
            let pending = owned(&location, request.param("sessionID"), request.param("requestID"))?;
            Ok(encoded(request, &data_of(pending)))
        }
        "session.permission.reply" => {
            let location = location();
            let request_id = request.param("requestID");
            owned(&location, request.param("sessionID"), request_id)?;
            let reply = json::get_str(&decoded.payload, "decision").and_then(Reply::parse).unwrap_or(Reply::Reject);
            let message = json::get_str(&decoded.payload, "message");
            location
                .permission
                .reply(app, &location, request_id, reply, message)
                .map_err(|error| permission::not_found(&error.request_id))?;
            Ok(Response::no_content())
        }
        "permission.saved.list" => {
            let location = location();
            let project_id = json::get_str(&decoded.query, "projectID").unwrap_or(&location.project.id).to_owned();
            let saved = saved_list(&app.db.lock().unwrap(), &project_id).map_err(fatal)?;
            Ok(encoded(request, &data_of(Json::Array(saved))))
        }
        "permission.saved.update" => {
            let action = json::get_str(&decoded.payload, "action");
            let resource = json::get_str(&decoded.payload, "resource");
            let updated = saved_update(&app.db.lock().unwrap(), request.param("id"), action, resource)?;
            Ok(encoded(request, &data_of(updated)))
        }
        "permission.saved.remove" => {
            app.db
                .lock()
                .unwrap()
                .execute("DELETE FROM permission WHERE id = ?1", [request.param("id")])
                .map_err(fatal)?;
            Ok(Response::no_content())
        }
        _ => super::fallback(app, request),
    }
}

fn data_of(value: Json) -> Json {
    let mut o = Obj::new();
    o.set("data", value);
    o.done()
}

/// `requireOwnedRequest`: a pending request of the session, else `PermissionNotFoundError`.
fn owned(location: &Location, session_id: &str, request_id: &str) -> Result<Json> {
    location
        .permission
        .get(request_id)
        .filter(|pending| json::get_str(pending, "sessionID") == Some(session_id))
        .ok_or_else(|| permission::not_found(request_id))
}

/// `session.permission.create`: evaluates the request, publishing it when it asks.
fn create(app: &App, request: &Request, decoded: &Decoded, location: &Location) -> Result<Response> {
    let payload = &decoded.payload;
    let strings = |key: &str| {
        json::get(payload, key)
            .and_then(json::array)
            .map(|items| items.iter().filter_map(json::as_str).map(str::to_owned).collect::<Vec<_>>())
    };
    let source = json::get(payload, "source").map(|source| Source {
        message_id: json::get_str(source, "messageID").unwrap_or_default().to_owned(),
        id: json::get_str(source, "id").unwrap_or_default().to_owned(),
    });
    let requester = Requester {
        id: json::get_str(payload, "id").map(str::to_owned),
        session_id: request.param("sessionID").to_owned(),
        agent: json::get_str(payload, "agent").map(str::to_owned),
        source,
        unattended: None,
    };
    let ask = Ask {
        action: json::get_str(payload, "action").unwrap_or_default().to_owned(),
        resources: strings("resources").unwrap_or_default(),
        save: strings("save"),
        metadata: json::get(payload, "metadata").cloned(),
    };
    let result = location.permission.ask(app, location, &requester, &ask).map_err(|error| match error {
        AskError::SessionNotFound(id) => crate::session::missing(&id),
        AskError::Duplicate(id) => defect(format!("Duplicate pending permission ID: {id}")),
    })?;
    let mut answer = Obj::new();
    answer.str("id", &result.id).str("effect", result.effect);
    Ok(data(answer.done()))
}

/// `PermissionSaved.Info` of a row (`id, project_id, action, resource, time_created, time_updated`).
fn saved_info(row: &rusqlite::Row) -> rusqlite::Result<Json> {
    let mut time = Obj::new();
    time.int("created", row.get(4)?).int("updated", row.get(5)?);
    let mut o = Obj::new();
    o.str("id", &row.get::<_, String>(0)?).str("projectID", &row.get::<_, String>(1)?);
    o.str("action", &row.get::<_, String>(2)?).str("resource", &row.get::<_, String>(3)?);
    o.set("time", time.done());
    Ok(o.done())
}

const SAVED_COLUMNS: &str = "id, project_id, action, resource, time_created, time_updated";

/// `PermissionSaved.list({projectID})`, in the order SQLite returns them (the project's unique index: by action,
/// then resource), as drizzle's unordered select does.
fn saved_list(db: &rusqlite::Connection, project_id: &str) -> rusqlite::Result<Vec<Json>> {
    let mut statement = db.prepare_cached(&format!("SELECT {SAVED_COLUMNS} FROM permission WHERE project_id = ?1"))?;
    statement.query_map([project_id], saved_info)?.collect()
}

fn saved_not_found(id: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "PermissionSavedNotFoundError").str("id", id);
    o.str("message", &format!("Saved permission not found: {id}"));
    Failure::new(404, o.done())
}

/// `PermissionSaved.update`: a changed action or resource (`time_updated` set, as drizzle's `$onUpdate` does); a
/// duplicate of another saved approval of the project conflicts.
fn saved_update(db: &rusqlite::Connection, id: &str, action: Option<&str>, resource: Option<&str>) -> Result<Json> {
    use rusqlite::OptionalExtension;
    let current = db
        .query_row("SELECT action, resource FROM permission WHERE id = ?1", [id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()
        .map_err(fatal)?;
    let Some((current_action, current_resource)) = current else { return Err(saved_not_found(id)) };
    let action = action.map(str::to_owned).unwrap_or(current_action);
    let resource = resource.map(str::to_owned).unwrap_or(current_resource);
    let updated = db
        .query_row(
            &format!("UPDATE permission SET action = ?1, resource = ?2, time_updated = ?3 WHERE id = ?4 RETURNING {SAVED_COLUMNS}"),
            rusqlite::params![action, resource, crate::id::now(), id],
            saved_info,
        )
        .optional();
    match updated {
        Ok(Some(info)) => Ok(info),
        Ok(None) => Err(saved_not_found(id)),
        Err(rusqlite::Error::SqliteFailure(error, _)) if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE => {
            let mut o = Obj::new();
            o.str("_tag", "ConflictError");
            o.str("message", &format!("A saved permission already allows {action} for {resource}"));
            o.str("resource", id);
            Err(Failure::new(409, o.done()))
        }
        Err(error) => Err(fatal(error)),
    }
}
