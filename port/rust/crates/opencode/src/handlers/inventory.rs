//! What a location's config and plugins register: agents, skills, commands, config, providers, models, and the
//! dropped features' endpoints (plugins, MCP, integrations, credentials), which answer as the TS server does with
//! nothing configured (handlers/agent.ts, skill.ts, command.ts, config.ts, provider.ts, model.ts, ...).

use std::sync::Arc;

use opencode_protocol::Json;

use crate::api::Decoded;
use crate::app::App;
use crate::http::{Request, Response, Result};
use crate::location::Location;

pub async fn handle(app: &App, request: &Request, _decoded: Decoded, located: Option<Arc<Location>>) -> Result<Response> {
    match request.route.operation {
        "agent.list" | "skill.list" => {
            // TODO(port): agents and skills (crate::inventory). The location is booted (the middleware did it).
            let location = located.expect("located endpoint");
            Ok(super::located_data(&location, Json::Array(vec![])))
        }
        _ => super::fallback(app, request),
    }
}
