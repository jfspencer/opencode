//! Schedules and jobs (handlers/schedule.ts, job.ts): every schedule route needs `experimental.schedules` where the
//! Session lives; a schedule whose Session is gone, or whose location has the flag off, reads as missing or
//! forbidden like the rest.

use std::sync::Arc;

use opencode_protocol::Json;

use crate::api::Decoded;
use crate::app::App;
use crate::http::{Failure, Request, Response, Result};
use crate::json::{self, Obj};
use crate::location::Location;
use crate::schedule::{self, Error};

pub async fn handle(app: &App, request: &Request, decoded: Decoded, _located: Option<Arc<Location>>) -> Result<Response> {
    match request.route.operation {
        "experimental.schedule.create" => {
            let session_id = json::get_str(&decoded.payload, "sessionID").unwrap_or_default();
            // Stateless HTTP callers have no guaranteed UI listener to answer an interactive ask, so settle asks
            // unattended.
            let info = schedule::create(app, session_id, &decoded.payload, Some(vec![])).await.map_err(failure)?;
            Ok(data(request, info))
        }
        "experimental.schedule.list" => {
            let session_id = json::get_str(&decoded.query, "sessionID").unwrap_or_default();
            allowed(app, session_id).await?;
            let rows = schedule::list(app, Some(session_id)).map_err(failure)?;
            Ok(data(request, Json::Array(rows.iter().map(|row| row.info(None)).collect())))
        }
        "experimental.schedule.get" => {
            let row = find(app, request.param("scheduleID")).await?;
            Ok(data(request, row.info(None)))
        }
        "experimental.schedule.pause" => {
            let id = request.param("scheduleID");
            find(app, id).await?;
            let row = schedule::pause(app, id).map_err(failure)?;
            Ok(data(request, row.info(None)))
        }
        "experimental.schedule.resume" => {
            let id = request.param("scheduleID");
            find(app, id).await?;
            let row = schedule::resume(app, id).map_err(failure)?;
            Ok(data(request, row.info(None)))
        }
        "experimental.schedule.delete" => {
            let id = request.param("scheduleID");
            find(app, id).await?;
            schedule::remove(app, id).map_err(failure)?;
            Ok(Response::no_content())
        }
        "experimental.job.list" => {
            let jobs = crate::session::subagent::list_jobs(app, &decoded.query)?;
            let mut body = crate::json::Obj::new();
            body.set("data", opencode_protocol::Json::Array(jobs));
            Ok(super::encoded(request, &body.done()))
        }
        _ => super::fallback(app, request),
    }
}

fn data(request: &Request, value: Json) -> Response {
    let mut o = Obj::new();
    o.set("data", value);
    super::encoded(request, &o.done())
}

fn forbidden(message: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "ForbiddenError").str("message", message);
    Failure::new(403, o.done())
}

fn missing_schedule(id: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "ScheduleNotFoundError").str("scheduleID", id).str("message", &format!("Schedule not found: {id}"));
    Failure::new(404, o.done())
}

fn failure(error: Error) -> Failure {
    use crate::permission::Error as Permission;
    match error {
        Error::NotFound(id) => missing_schedule(&id),
        Error::Invalid(reason) => Failure::invalid(reason, None, Some("schedule")),
        Error::Disabled => forbidden(schedule::DISABLED),
        Error::Session(failure) => failure,
        Error::Permission(Permission::SessionNotFound(id)) => crate::session::missing(&id),
        Error::Permission(Permission::Declined) => crate::app::defect("Permission.DeclinedError"),
        Error::Permission(error) => forbidden(&error.message()),
    }
}

/// `allowed`: the Session exists and has schedules on.
async fn allowed(app: &App, session_id: &str) -> Result<()> {
    match schedule::enabled(app, session_id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(forbidden(schedule::DISABLED)),
        Err(error) => Err(failure(error)),
    }
}

/// `find`: the schedule, as missing when its Session is gone.
async fn find(app: &App, id: &str) -> Result<schedule::Row> {
    let row = schedule::get(app, id).map_err(failure)?;
    let Some(session_id) = row.session_id.clone() else { return Err(missing_schedule(id)) };
    match schedule::enabled(app, &session_id).await {
        Ok(true) => Ok(row),
        Ok(false) => Err(forbidden(schedule::DISABLED)),
        Err(Error::Session(_)) => Err(missing_schedule(id)),
        Err(error) => Err(failure(error)),
    }
}
