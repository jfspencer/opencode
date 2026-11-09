//! Endpoint handlers (packages/server/src/handlers/*.ts), one module per area of the API.
//!
//! An operation is routed by its name (without the `experimental.` prefix): its first segment names the area,
//! and the session's sub-areas (permissions, forms, revert) have modules of their own. Each module answers the
//! operations it implements and passes the rest to [`fallback`].

mod event;
mod fs;
mod inventory;
mod misc;
mod permission;
mod form;
mod project;
mod schedule;
mod session;
mod shell;
mod vcs;
mod worktree;

use std::sync::Arc;

use crate::api::{Decoded, not_implemented};
use crate::app::App;
use crate::http::{Request, Response, Result};
use crate::location::Location;

pub async fn handle(app: &App, request: &Request, decoded: Decoded, located: Option<Arc<Location>>) -> Result<Response> {
    let operation = request.route.operation;
    let name = operation.strip_prefix("experimental.").unwrap_or(operation);
    let mut segments = name.split('.');
    let area = segments.next().unwrap_or(name);
    match (area, segments.next()) {
        ("event", _) => event::handle(app, request, decoded, located).await,
        ("session", Some("permission")) | ("permission", _) => permission::handle(app, request, decoded, located).await,
        ("session", Some("form")) | ("form", _) => form::handle(app, request, decoded, located).await,
        ("session", _) | ("message", _) => session::handle(app, request, decoded, located).await,
        (
            "agent" | "skill" | "command" | "config" | "provider" | "model" | "plugin" | "reference" | "websearch"
            | "integration" | "mcp" | "credential" | "usage" | "migration",
            _,
        ) => inventory::handle(app, request, decoded, located).await,
        ("project" | "location" | "server" | "debug", _) => project::handle(app, request, decoded, located).await,
        ("fs", _) => fs::handle(app, request, decoded, located).await,
        ("vcs", _) => vcs::handle(app, request, decoded, located).await,
        ("worktree", _) => worktree::handle(app, request, decoded, located).await,
        ("schedule" | "job", _) => schedule::handle(app, request, decoded, located).await,
        ("shell" | "pty" | "persistentPty", _) => shell::handle(app, request, decoded, located).await,
        _ => misc::handle(app, request, decoded, located).await,
    }
}

/// An operation the Rust server does not implement yet: the session middleware's checks still apply, then 501.
pub fn fallback(app: &App, request: &Request) -> Result<Response> {
    if request.route.path.contains(":sessionID") {
        crate::session::require(&app.db.lock().unwrap(), request.param("sessionID"))?;
    }
    Err(not_implemented(request.route.operation))
}

/// `{data}`: the envelope most endpoints answer with.
pub fn data(value: opencode_protocol::Json) -> Response {
    let mut o = crate::json::Obj::new();
    o.set("data", value);
    Response::value(200, &o.done())
}

/// `{location, data}`: the envelope of a located endpoint's answer (`response` in server/src/location.ts). Its
/// `Location.Info` is encoded as the public reference (`{directory, workspaceID?}`).
pub fn located_data(location: &Location, value: opencode_protocol::Json) -> Response {
    let mut o = crate::json::Obj::new();
    o.set("location", location.reference().to_json()).set("data", value);
    Response::value(200, &o.done())
}

/// A 200 answer encoded through the endpoint's success schema, as HttpApi encodes it: keys in schema order,
/// unknown keys dropped. A value the schema rejects is a server bug; it is sent as built (and logged).
pub fn encoded(request: &Request, value: &opencode_protocol::Json) -> Response {
    use opencode_protocol::routes::BodyKind;
    let ty = request.route.success.iter().find(|b| b.kind == BodyKind::Json).and_then(|b| b.ty);
    match ty.and_then(|ty| opencode_protocol::roots::roundtrip(ty, value)) {
        Some(Ok(text)) => Response::json(200, text),
        Some(Err(issue)) => {
            eprintln!("{} answer does not fit its schema: {issue}", request.route.operation);
            Response::value(200, value)
        }
        None => Response::value(200, value),
    }
}

/// [`located_data`] encoded through the endpoint's success schema.
pub fn located(request: &Request, location: &Location, value: opencode_protocol::Json) -> Response {
    let mut o = crate::json::Obj::new();
    o.set("location", location.reference().to_json()).set("data", value);
    encoded(request, &o.done())
}
