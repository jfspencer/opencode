//! Schedules and jobs (handlers/schedule.ts, job.ts).

use std::sync::Arc;

use crate::api::Decoded;
use crate::app::App;
use crate::http::{Request, Response, Result};
use crate::location::Location;

pub async fn handle(app: &App, request: &Request, decoded: Decoded, _located: Option<Arc<Location>>) -> Result<Response> {
    match request.route.operation {
        "experimental.job.list" => {
            let jobs = crate::session::subagent::list_jobs(app, &decoded.query)?;
            let mut body = crate::json::Obj::new();
            body.set("data", opencode_protocol::Json::Array(jobs));
            Ok(super::encoded(request, &body.done()))
        }
        _ => super::fallback(app, request),
    }
}
