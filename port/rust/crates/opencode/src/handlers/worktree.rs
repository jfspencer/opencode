//! Worktrees (handlers/worktree.ts).

use std::sync::Arc;

use opencode_protocol::Json;
use opencode_protocol::Wire;
use opencode_protocol::types as t;

use crate::api::Decoded;
use crate::app::App;
use crate::http::{Failure, Request, Response, Result};
use crate::json::Obj;
use crate::location::Location;
use crate::worktree::{self, CreateInput};

pub async fn handle(app: &App, request: &Request, decoded: Decoded, _located: Option<Arc<Location>>) -> Result<Response> {
    match request.route.operation {
        "worktree.list" => {
            let query = t::WorktreeListQuery::decode(&decoded.query).map_err(|issue| Failure::schema("Query", &issue))?;
            let rows = worktree::list(app, &query.project_id.0).map_err(worktree::Error::failure)?;
            Ok(super::encoded(request, &Json::Array(rows.iter().map(|row| row.to_json()).collect())))
        }
        "worktree.create" => {
            let payload = t::WorktreeCreateInput::decode(&decoded.payload).map_err(|issue| Failure::schema("Payload", &issue))?;
            let directory = worktree::create(
                app,
                CreateInput {
                    project_id: payload.project_id.0,
                    name: payload.name,
                    directory: payload.directory.map(|d| d.0),
                    from: payload.from.map(|d| d.0),
                    branch: payload.branch,
                },
            )
            .await
            .map_err(worktree::Error::failure)?;
            let mut o = Obj::new();
            o.str("directory", &directory);
            Ok(super::encoded(request, &o.done()))
        }
        "worktree.remove" => {
            let payload = t::WorktreeRemoveInput::decode(&decoded.payload).map_err(|issue| Failure::schema("Payload", &issue))?;
            worktree::remove(app, &payload.project_id.0, &payload.directory.0, payload.force).await.map_err(worktree::Error::failure)?;
            Ok(Response::no_content())
        }
        "worktree.refresh" => {
            let payload = t::WorktreeRefreshPayload::decode(&decoded.payload).map_err(|issue| Failure::schema("Payload", &issue))?;
            worktree::refresh(app, &payload.project_id.0).await.map_err(worktree::Error::failure)?;
            Ok(Response::no_content())
        }
        _ => super::fallback(app, request),
    }
}
