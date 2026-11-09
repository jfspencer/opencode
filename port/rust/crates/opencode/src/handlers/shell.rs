//! Shells, PTYs and jobs (handlers/shell.ts, shell-error.ts, pty.ts, persistent-pty.ts, job.ts), and the session
//! endpoints that run on them (`session.shell`, `session.environment`, `session.background` in handlers/session.ts).

use std::sync::Arc;

use opencode_protocol::Json;

use crate::api::Decoded;
use crate::app::App;
use crate::http::{Failure, Request, Response, Result};
use crate::json::{self, Obj};
use crate::location::Location;
use crate::session;
use crate::shell::{CreateError, CreateInput, select};

pub async fn handle(app: &App, request: &Request, decoded: Decoded, located: Option<Arc<Location>>) -> Result<Response> {
    match request.route.operation {
        "shell.list" | "shell.create" | "shell.get" | "shell.output" | "shell.remove" => {
            shell(request, &decoded, &located.expect("located endpoint")).await
        }
        "experimental.job.list" => job_list(app, request, &decoded),
        _ => super::fallback(app, request),
    }
}

fn not_found(id: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "ShellNotFoundError").str("id", id).str("message", &crate::shell::NotFound(id.to_owned()).message());
    Failure::new(404, o.done())
}

fn query_number(query: &Json, key: &str) -> Option<u64> {
    match json::get(query, key)? {
        Json::Number(n) => Some(n.max(0.0) as u64),
        Json::String(text) => text.trim().parse::<f64>().ok().map(|n| n.max(0.0) as u64),
        _ => None,
    }
}

/// `server.shell` (handlers/shell.ts).
async fn shell(request: &Request, decoded: &Decoded, location: &Arc<Location>) -> Result<Response> {
    let service = &location.shell;
    match request.route.operation {
        "shell.list" => {
            let items = service.list().iter().map(|info| info.to_json()).collect();
            Ok(super::located(request, location, Json::Array(items)))
        }
        "shell.create" => {
            let payload = &decoded.payload;
            let cwd = json::get_str(payload, "cwd").filter(|c| !c.is_empty()).unwrap_or(&location.directory).to_owned();
            let target = cwd.clone();
            let directory = tokio::task::spawn_blocking(move || std::fs::metadata(&target).is_ok_and(|m| m.is_dir()))
                .await
                .unwrap_or(false);
            if !directory {
                return Err(Failure::invalid(format!("cwd is not a directory: {cwd}"), Some("cwd"), Some("invalid_cwd")));
            }
            let metadata = json::get(payload, "metadata").filter(|m| matches!(m, Json::Object(_))).cloned();
            let base = session::shell::base_environment(location.workspace_id.is_some(), metadata.as_ref());
            let inventory = location.inventory();
            let input = CreateInput {
                command: json::get_str(payload, "command").unwrap_or_default().to_owned(),
                cwd: Some(cwd.clone()),
                timeout: json::get_num(payload, "timeout").map(|t| t.max(0.0) as u64),
                shell: None,
                metadata,
            };
            let default_shell = || select::resolve(select::Priority::Config, &inventory);
            let info = service
                .create(input, base, &location.directory, default_shell, |invocation| async { Ok::<_, ()>(invocation) })
                .await
                .map_err(|error| match error {
                    CreateError::Before(()) => Failure::defect(),
                    CreateError::Start(error) => session::shell::start_failure(&error, &cwd),
                })?;
            Ok(super::located(request, location, info.to_json()))
        }
        "shell.get" => {
            let id = request.param("id");
            let info = service.get(id).map_err(|_| not_found(id))?;
            Ok(super::located(request, location, info.to_json()))
        }
        "shell.output" => {
            let id = request.param("id");
            let cursor = query_number(&decoded.query, "cursor");
            let limit = query_number(&decoded.query, "limit");
            let page = service.output(id, cursor, limit).await.map_err(|_| not_found(id))?;
            Ok(super::located(request, location, session::shell::page_json(&page)))
        }
        "shell.remove" => {
            let _ = service.remove(request.param("id")).await;
            Ok(Response::no_content())
        }
        _ => unreachable!("shell operation"),
    }
}

/// `job.list` (handlers/job.ts with `SessionBackground.list`): the background jobs, filtered by kind and status, and
/// by session (with its descendants when asked).
fn job_list(app: &App, request: &Request, decoded: &Decoded) -> Result<Response> {
    let query = &decoded.query;
    let session_id = json::get_str(query, "sessionID");
    if let Some(id) = session_id {
        session::require(&app.db.lock().unwrap(), id)?;
    }
    let kind = json::get_str(query, "kind");
    let status = json::get_str(query, "status");
    let descendants = matches!(json::get(query, "descendants"), Some(Json::Bool(true)))
        || json::get_str(query, "descendants") == Some("true");
    let items: Vec<crate::job::Item> = crate::job::jobs()
        .background_jobs(app)
        .into_iter()
        .filter(|item| kind.is_none_or(|k| item.kind() == k) && status.is_none_or(|s| item.status.as_str() == s))
        .collect();
    let items: Vec<Json> = match session_id {
        None => items.iter().map(|item| item.to_json()).collect(),
        Some(ancestor) if !descendants => {
            items.iter().filter(|item| item.session() == ancestor).map(|item| item.to_json()).collect()
        }
        Some(ancestor) => {
            let db = app.db.lock().unwrap();
            let mut parents: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
            items
                .iter()
                .filter(|item| descends(&db, &mut parents, item.session(), ancestor))
                .map(|item| item.to_json())
                .collect()
        }
    };
    let mut o = Obj::new();
    o.set("data", Json::Array(items));
    Ok(super::encoded(request, &o.done()))
}

/// `descends`: whether `from` is `ancestor` or one of its descendants.
fn descends(
    db: &rusqlite::Connection,
    parents: &mut std::collections::HashMap<String, Option<String>>,
    from: &str,
    ancestor: &str,
) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut current = Some(from.to_owned());
    while let Some(id) = current {
        if id == ancestor {
            return true;
        }
        if !seen.insert(id.clone()) {
            return false;
        }
        let parent = match parents.get(&id) {
            Some(cached) => cached.clone(),
            None => {
                let parent = session::get(db, &id).ok().flatten().and_then(|s| json::get_str(&s.info, "parentID").map(str::to_owned));
                parents.insert(id.clone(), parent.clone());
                parent
            }
        };
        current = parent;
    }
    false
}

/// The session endpoints built on shells and jobs (handlers/session.ts).
pub async fn session(app: &App, request: &Request, decoded: &Decoded) -> Result<Response> {
    let session_id = request.param("sessionID");
    match request.route.operation {
        "session.shell" => {
            let id = json::get_str(&decoded.payload, "id").map(str::to_owned);
            let command = json::get_str(&decoded.payload, "command").unwrap_or_default();
            session::shell::run(&app.arc(), session_id, id, command).await?;
            Ok(Response::no_content())
        }
        "session.environment" => {
            session::require(&app.db.lock().unwrap(), session_id)?;
            if let Some(Json::Object(variables)) = json::get(&decoded.payload, "variables") {
                let variables = variables
                    .iter()
                    .filter_map(|(key, value)| json::as_str(value).map(|v| (key.clone(), v.to_owned())))
                    .collect();
                session::shell::set_environment(session_id, variables);
            }
            Ok(Response::no_content())
        }
        "session.background" => {
            session::require(&app.db.lock().unwrap(), session_id)?;
            background(app, session_id)?;
            Ok(Response::no_content())
        }
        _ => super::fallback(app, request),
    }
}

/// `Session.background`: the session's blocking work moves to the background, and the model is told so.
fn background(app: &App, session_id: &str) -> Result<()> {
    let moved = crate::job::jobs().background_all(app, session_id, None);
    if moved.is_empty() {
        return Ok(());
    }
    let mut lines = vec![
        "User requested that active blocking work be moved to the background.".to_owned(),
        String::new(),
        "Backgrounded work:".to_owned(),
    ];
    for job in &moved {
        let title = job.title.as_deref().filter(|t| !t.is_empty()).unwrap_or(&job.id);
        lines.push(format!("- {}: {title}", job.ty));
    }
    lines.push(String::new());
    lines.push("The backgrounded work is still unfinished. Move on to other work if you can. If there is nothing else useful to do, finish your response. Do not wait, sleep, poll, or report the backgrounded work as complete until a later completion notification is added to the conversation.".to_owned());
    match session::shell::synthetic(app, None, session_id, None, &lines.join("\n"), None, None, true) {
        Ok(_) => Ok(()),
        Err(session::shell::SyntheticError::NotFound) => Err(session::missing(session_id)),
        Err(session::shell::SyntheticError::Conflict(_)) => Err(Failure::defect()),
    }
}
