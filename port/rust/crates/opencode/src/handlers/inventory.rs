//! What a location's config and plugins register: agents, skills, commands, config, providers, models, and the
//! dropped features' endpoints (plugins, MCP, integrations, credentials, web search, usage limits, the v1 migration),
//! which answer as the TS server does with nothing configured (packages/server/src/handlers/agent.ts, skill.ts,
//! command.ts, config.ts, provider.ts, model.ts, plugin.ts, mcp.ts, integration.ts, credential.ts, websearch.ts,
//! reference.ts, usage.ts, migration.ts).
//!
//! The plugin list and the integration catalog are those of the TS reference's built-in plugins (recorded from it,
//! `inventory/builtin/plugins.json` and `integrations.json`); config providers add their integrations.
//!
//! TODO(port): writes to dropped features (adding MCP servers, connecting integrations, storing credentials) answer
//! 501.

use std::sync::Arc;

use opencode_protocol::Json;

use super::{data, located_data};
use crate::api::{Decoded, not_implemented};
use crate::app::App;
use crate::http::{Failure, Request, Response, Result};
use crate::inventory::{self, list};
use crate::json::{self, Obj};
use crate::location::Location;

const PLUGINS: &str = include_str!("../inventory/builtin/plugins.json");
const INTEGRATIONS: &str = include_str!("../inventory/builtin/integrations.json");
const WEBSEARCH: &str = include_str!("../inventory/builtin/websearch.json");

fn not_found(tag: &str, key: &str, id: &str, message: String) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", tag).str(key, id).str("message", &message);
    Failure::new(404, o.done())
}

pub async fn handle(app: &App, request: &Request, decoded: Decoded, located: Option<Arc<Location>>) -> Result<Response> {
    let operation = request.route.operation;
    // Endpoints outside a location.
    match operation {
        "credential.list" => return Ok(data(Json::Array(vec![]))),
        "credential.activate" | "credential.remove" => return Ok(Response::no_content()),
        "experimental.migration.v1.status" => {
            let mut o = Obj::new();
            o.str("status", "completed");
            return Ok(Response::value(200, &o.done()));
        }
        _ => {}
    }
    let Some(location) = located else { return super::fallback(app, request) };
    let inventory = location.inventory();
    match operation {
        "agent.list" => Ok(located_data(&location, list(&inventory.agents))),
        "agent.get" => {
            let id = request.param("agentID");
            match inventory.agent(id) {
                Some(agent) => Ok(located_data(&location, json::of(agent))),
                None => Err(not_found("AgentNotFoundError", "agentID", id, format!("Agent not found: {id}"))),
            }
        }
        "config.get" => Ok(Response::value(200, &inventory::config::wire(&inventory.entries))),
        "config.shells" => Ok(Response::value(200, &shells())),
        "experimental.config.update" => {
            let shell = json::get_str(&decoded.payload, "shell");
            if let Err(error) = inventory::config::update_shell(shell) {
                // The TS server dies on a failed update (`Effect.orDie`).
                eprintln!("config update failed: {error}");
                let mut o = Obj::new();
                o.str("_tag", "UnknownError").str("message", "Internal server error");
                return Err(Failure::new(500, o.done()));
            }
            location.inventory.reload(&app.bus, &location.reference()).await;
            Ok(Response::no_content())
        }
        "provider.list" => {
            let providers: Vec<Json> = inventory.catalog.available().map(json::of).collect();
            Ok(located_data(&location, Json::Array(providers)))
        }
        "provider.get" => {
            let id = request.param("providerID");
            match inventory.provider(id) {
                Some(provider) => Ok(located_data(&location, json::of(provider))),
                None => Err(not_found("ProviderNotFoundError", "providerID", id, format!("Provider not found: {id}"))),
            }
        }
        "model.list" => {
            let types: Vec<String> = match json::get(&decoded.query, "types") {
                Some(Json::String(s)) => vec![s.clone()],
                Some(Json::Array(items)) => items
                    .iter()
                    .filter_map(|i| match i {
                        Json::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => vec![],
            };
            let chat: Vec<Json> = inventory.available_models().map(json::of).collect();
            let models = if types.is_empty() {
                chat
            } else {
                // Each requested type once, in order; only chat models exist without a catalog.
                let mut seen: Vec<&str> = vec![];
                let mut out = vec![];
                for ty in &types {
                    if seen.contains(&ty.as_str()) {
                        continue;
                    }
                    seen.push(ty);
                    if ty == "chat" {
                        out.extend(chat.iter().cloned());
                    }
                }
                out
            };
            Ok(located_data(&location, Json::Array(models)))
        }
        "model.default" => Ok(located_data(&location, inventory.default_model().map(json::of).unwrap_or(Json::Null))),
        "command.list" => {
            let commands: Vec<Json> = inventory.commands.iter().map(|c| json::of(&c.info())).collect();
            Ok(located_data(&location, Json::Array(commands)))
        }
        "skill.list" => Ok(located_data(&location, list(&inventory.skills))),
        "reference.list" => Ok(located_data(&location, list(&inventory.references))),
        "experimental.usage.limits" => Ok(located_data(&location, Json::Null)),
        "websearch.providers" => Ok(located_data(&location, json::parse(WEBSEARCH).unwrap_or_default())),
        "websearch.query" => {
            let mut o = Obj::new();
            o.str("_tag", "InvalidRequestError").str("message", "Web search is disabled").str("kind", "websearch_disabled");
            Err(Failure::new(400, o.done()))
        }
        "plugin.list" => Ok(located_data(&location, json::parse(PLUGINS).unwrap_or_default())),
        "plugin.gui" => {
            let id = json::get_str(&decoded.query, "id").unwrap_or_default();
            Err(not_found("PluginNotFoundError", "pluginID", id, format!("No active plugin with a ./gui-experimental bundle: {id}")))
        }
        "plugin.check" => match json::get_str(&decoded.payload, "target") {
            // Built-in plugins have no package targets.
            Some(target) => Err(Failure::invalid(
                format!("Plugin package is not in the current server inventory: {target}"),
                Some("target"),
                None,
            )),
            None => Ok(located_data(&location, json::parse(PLUGINS).unwrap_or_default())),
        },
        "plugin.update" => {
            let targets: Vec<&str> = match json::get(&decoded.payload, "targets") {
                Some(Json::Array(items)) => items.iter().filter_map(|t| match t {
                    Json::String(s) => Some(s.as_str()),
                    _ => None,
                }).collect(),
                _ => vec![],
            };
            if targets.is_empty() {
                return Ok(Response::no_content());
            }
            Err(Failure::invalid(
                format!("Plugin packages are not in the current server inventory: {}", targets.join(", ")),
                Some("targets"),
                None,
            ))
        }
        "mcp.list" => Ok(located_data(&location, Json::Array(vec![]))),
        "mcp.resource.catalog" => {
            let mut o = Obj::new();
            o.set("resources", Json::Array(vec![])).set("templates", Json::Array(vec![]));
            Ok(located_data(&location, o.done()))
        }
        "experimental.mcp.remove" | "experimental.mcp.connect" | "experimental.mcp.disconnect" => {
            let server = request.param("server");
            Err(not_found("McpServerNotFoundError", "server", server, format!("MCP server not found: {server}")))
        }
        "integration.list" => Ok(located_data(&location, Json::Array(integrations(&inventory)))),
        "integration.get" => {
            let id = request.param("integrationID");
            match integrations(&inventory).into_iter().find(|i| json::get_str(i, "id") == Some(id)) {
                Some(integration) => Ok(located_data(&location, integration)),
                None => Err(integration_not_found(id)),
            }
        }
        "integration.oauth.cancel" | "integration.command.cancel" => Ok(Response::no_content()),
        "integration.connect.key" | "integration.oauth.status" | "integration.oauth.complete" | "integration.command.status" => {
            let id = request.param("integrationID");
            if !integrations(&inventory).iter().any(|i| json::get_str(i, "id") == Some(id)) {
                return Err(integration_not_found(id));
            }
            // TODO(port): connecting an integration (stores a credential).
            Err(not_implemented(operation))
        }
        _ => super::fallback(app, request),
    }
}

fn integration_not_found(id: &str) -> Failure {
    not_found("IntegrationNotFoundError", "integrationID", id, format!("Integration not found: {id}"))
}

/// `Integration.list`: the built-in plugins' integrations and the configured providers', by name (`localeCompare`),
/// each with its environment connections (the env names that are set).
fn integrations(inventory: &inventory::Inventory) -> Vec<Json> {
    let mut items: Vec<Json> = match json::parse(INTEGRATIONS) {
        Some(Json::Array(items)) => items,
        _ => vec![],
    };
    for document in inventory::config::documents(&inventory.entries) {
        for (id, provider) in document.providers.iter().flat_map(|p| p.0.iter()) {
            let index = match items.iter().position(|i| json::get_str(i, "id") == Some(id)) {
                Some(i) => i,
                None => {
                    let mut method = Obj::new();
                    method.str("type", "key").str("label", "Manually enter API Key");
                    let mut o = Obj::new();
                    o.str("id", id).str("name", id).set("methods", Json::Array(vec![method.done()]));
                    o.set("connections", Json::Array(vec![]));
                    items.push(o.done());
                    items.len() - 1
                }
            };
            let Json::Object(item) = &mut items[index] else { continue };
            if let Some(name) = provider.name.as_ref().filter(|n| !n.is_empty()) {
                item.insert("name".into(), Json::String(name.clone()));
            }
            if let Some(env) = &provider.env {
                let mut method = Obj::new();
                method.str("type", "env").set("names", Json::Array(env.iter().map(|n| Json::String(n.clone())).collect()));
                if let Some(Json::Array(methods)) = item.get_mut("methods") {
                    match methods.iter_mut().find(|m| json::get_str(m, "type") == Some("env")) {
                        Some(existing) => *existing = method.done(),
                        None => methods.push(method.done()),
                    }
                }
            }
        }
    }
    for item in items.iter_mut() {
        let Json::Object(o) = item else { continue };
        let names: Vec<String> = match o.get("methods") {
            Some(Json::Array(methods)) => methods
                .iter()
                .filter(|m| json::get_str(m, "type") == Some("env"))
                .flat_map(|m| match json::get(m, "names") {
                    Some(Json::Array(names)) => names.iter().filter_map(|n| match n {
                        Json::String(s) => Some(s.clone()),
                        _ => None,
                    }).collect::<Vec<_>>(),
                    _ => vec![],
                })
                .collect(),
            _ => vec![],
        };
        let connections: Vec<Json> = names
            .into_iter()
            .filter(|n| std::env::var(n).is_ok_and(|v| !v.is_empty()))
            .map(|n| {
                let mut c = Obj::new();
                c.str("type", "env").str("name", &n);
                c.done()
            })
            .collect();
        o.insert("connections".into(), Json::Array(connections));
    }
    items.sort_by(|a, b| {
        let name = |v: &Json| json::get_str(v, "name").unwrap_or_default().to_owned();
        let (a, b) = (name(a), name(b));
        a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| b.cmp(&a))
    });
    items
}

/// `ShellSelect.list`: the shells of `/etc/shells` (or the defaults) that exist, each named by its basename when that
/// name resolves on `PATH` (and the cache's `bin`).
fn shells() -> Json {
    let text = std::fs::read_to_string("/etc/shells").unwrap_or_default();
    let mut shells: Vec<String> = vec![];
    if text.is_empty() {
        shells = ["/bin/bash", "/bin/zsh", "/bin/sh"].map(String::from).to_vec();
    } else {
        for line in text.split('\n') {
            if !line.trim().is_empty() && !line.starts_with('#') && !shells.iter().any(|s| s == line) {
                shells.push(line.to_owned());
            }
        }
    }
    let bin = format!("{}/bin", inventory::config::Options::get().cache);
    let items: Vec<Json> = shells
        .into_iter()
        .filter(|shell| executable(shell, &bin))
        .map(|shell| {
            let name = inventory::config::basename(&shell).to_lowercase();
            let mut o = Obj::new();
            o.str("path", &shell);
            o.str("name", if executable(&name, &bin) { &name } else { &shell });
            o.set("acceptable", Json::Bool(!["fish", "nu"].contains(&name.as_str())));
            o.done()
        })
        .collect();
    Json::Array(items)
}

/// A rooted path that is a file, or a name `which` finds on `PATH` (then `bin`).
fn executable(file: &str, bin: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if file.starts_with('/') {
        return std::fs::metadata(file).is_ok_and(|m| m.is_file());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let search = if path.is_empty() { bin.to_owned() } else { format!("{path}:{bin}") };
    search.split(':').filter(|d| !d.is_empty()).any(|dir| {
        std::fs::metadata(format!("{dir}/{file}")).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}
