//! Version control (handlers/vcs.ts).

use std::sync::Arc;

use opencode_protocol::Json;
use opencode_protocol::Wire;
use opencode_protocol::types as t;

use super::located_data;
use super::project::service_unavailable;
use crate::api::Decoded;
use crate::app::{App, fatal};
use crate::bus::PublishOptions;
use crate::http::{Failure, Request, Response, Result};
use crate::json::Obj;
use crate::location::Location;
use crate::vcs::{DiffError, DiffOptions, InitializeError, Mode, Untracked};

pub async fn handle(app: &App, request: &Request, decoded: Decoded, located: Option<Arc<Location>>) -> Result<Response> {
    let location = located.expect("located endpoint");
    let vcs = &location.version_control;
    match request.route.operation {
        "vcs.init" => init(app, &location, &decoded).await,
        "vcs.get" => Ok(located_data(&location, vcs.info().to_json())),
        "vcs.base" => {
            let base = vcs.base().await.map_err(review_error)?;
            Ok(located_data(&location, base.unwrap_or(Json::Null)))
        }
        "vcs.status" => {
            let rows = vcs.status().await;
            Ok(located_data(&location, Json::Array(rows.iter().map(|row| row.to_json()).collect())))
        }
        "vcs.branch.list" => {
            let query = t::VcsBranchListQuery::decode(&decoded.query).map_err(|issue| Failure::schema("Query", &issue))?;
            let limit = query.limit.map(|l| l.0).unwrap_or(50).min(100).max(0) as usize;
            let branches = vcs.branches(query.search.as_deref(), Some(limit)).await;
            Ok(located_data(&location, Json::Array(branches.into_iter().map(Json::String).collect())))
        }
        "vcs.diff" => {
            let query = t::VcsDiffQuery::decode(&decoded.query).map_err(|issue| Failure::schema("Query", &issue))?;
            let mode = match query.mode {
                t::VcsMode::Working => Mode::Working,
                t::VcsMode::Branch => Mode::Branch,
                t::VcsMode::Committed => Mode::Committed,
            };
            let options = DiffOptions {
                context: query.context.map(|c| c.0),
                base: query.base,
                untracked: query.untracked.map(|u| match u {
                    t::VcsUntracked::Files => Untracked::Files,
                    t::VcsUntracked::Folders => Untracked::Folders,
                }),
            };
            let rows = vcs.diff(mode, options).await.map_err(review_error)?;
            Ok(located_data(&location, Json::Array(rows.iter().map(|row| row.to_json()).collect())))
        }
        _ => super::fallback(app, request),
    }
}

fn review_error(error: DiffError) -> Failure {
    service_unavailable(&error.0, "vcs")
}

/// `POST /api/vcs/init`: a repository in the project directory, then the project resolved again and the location
/// rebooted as a checkout.
async fn init(app: &App, location: &Location, decoded: &Decoded) -> Result<Response> {
    let query = t::VcsInitQuery::decode(&decoded.query).map_err(|issue| Failure::schema("Query", &issue))?;
    let provider = query.provider.unwrap_or_else(|| "git".into());
    let directory = location.project.directory.clone();
    let failed = || service_unavailable("VCS initialization failed", &provider);
    location.version_control.initialize(&provider).await.map_err(|error| match error {
        InitializeError::Missing => Failure::invalid("Project directory does not exist", Some("location"), None),
        InitializeError::Conflict => {
            let mut o = Obj::new();
            o.str("_tag", "ConflictError").str("message", "Project already has version control").str("resource", &directory);
            Failure::new(409, o.done())
        }
        InitializeError::Unknown => Failure::invalid("Unknown VCS provider", Some("provider"), None),
        InitializeError::Unsupported => {
            let mut o = Obj::new();
            o.str("_tag", "VcsInitNotSupportedError").str("providerID", &provider);
            o.str("message", "VCS provider does not support initialization");
            Failure::new(400, o.done())
        }
        InitializeError::Failed => failed(),
    })?;
    let target = directory.clone();
    let resolved = tokio::task::spawn_blocking(move || crate::project::discover(&target)).await.expect("project discovery");
    let reference = location.reference();
    {
        let mut db = app.db.lock().unwrap();
        app.projects.persist_at(&mut db, &app.bus, &resolved, Some(reference.clone())).map_err(fatal)?;
        crate::project_icon::refresh(&db, &app.bus, &resolved.id, Some(reference.clone())).map_err(fatal)?;
    }
    if resolved.vcs.is_none() {
        return Err(failed());
    }
    app.locations.invalidate(app, &reference);
    let mut data = Obj::new();
    data.str("projectID", &resolved.id);
    app.bus.emit("worktree.updated", data.done(), PublishOptions { id: None, location: Some(reference) });
    Ok(Response::no_content())
}
