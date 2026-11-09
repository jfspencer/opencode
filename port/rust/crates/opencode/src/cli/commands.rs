//! The small server-backed commands: `models`, `reload`, `debug paths|agents|config`, `session list|delete|export|
//! import` and `api` (packages/cli/src/commands/handlers/*.ts).

use opencode_protocol::Json;

use super::client::{Client, encode_component, fetch, form_encode};
use super::connection::{self, Args, Resolved};
use super::out::{Fail, Result, pretty, stderr, stdout};
use super::parse::{Globals, Input};
use crate::json::{self, Obj};

fn cwd() -> String {
    crate::project::absolutize(".")
}

fn location(directory: &str) -> Json {
    let mut inner = Obj::new();
    inner.str("directory", directory);
    let mut o = Obj::new();
    o.set("location", inner.done());
    o.done()
}

/// Runs `body` against the resolved server, then ends a private server.
async fn with_server<F, Fut>(globals: &Globals, args: Args, body: F) -> Result
where
    F: FnOnce(Client) -> Fut,
    Fut: std::future::Future<Output = Result>,
{
    let server: Resolved = connection::resolve(globals, args).await?;
    let result = body(server.client()).await;
    server.close().await;
    result
}

fn data(value: &Json) -> &[Json] {
    json::get(value, "data").and_then(json::array).unwrap_or(&[])
}

pub async fn models(globals: &Globals) -> Result {
    with_server(globals, Args::default(), |client| async move {
        let response = client.call("model.list", &[], Some(location(&cwd())), None).await?;
        let mut models: Vec<String> = data(&response)
            .iter()
            .map(|m| format!("{}/{}", json::get_str(m, "providerID").unwrap_or(""), json::get_str(m, "id").unwrap_or("")))
            .collect();
        models.sort_by(|a, b| crate::collate::compare(a, b));
        if !models.is_empty() {
            stdout(&format!("{}\n", models.join("\n")));
        }
        Ok(0)
    })
    .await
}

pub async fn reload(globals: &Globals) -> Result {
    with_server(globals, Args::default(), |client| async move {
        client.call("location.reload", &[], None, None).await?;
        stdout("Configuration reloaded\n");
        Ok(0)
    })
    .await
}

pub fn paths(input: &Input) -> Result {
    let all = super::paths::all();
    match input.str("name") {
        Some(name) => {
            let value = all.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone()).unwrap_or_default();
            stdout(&format!("{value}\n"));
        }
        None => {
            let text: String = all.iter().map(|(key, value)| format!("{key:<10} {value}\n")).collect();
            stdout(&text);
        }
    }
    Ok(0)
}

pub async fn agents(globals: &Globals) -> Result {
    with_server(globals, Args::default(), |client| async move {
        let response = client.call("agent.list", &[], Some(location(&cwd())), None).await?;
        let mut agents = data(&response).to_vec();
        agents.sort_by(|a, b| crate::collate::compare(json::get_str(a, "id").unwrap_or(""), json::get_str(b, "id").unwrap_or("")));
        stdout(&format!("{}\n", pretty(&Json::Array(agents))));
        Ok(0)
    })
    .await
}

/// `secretName` of debug/redact.ts: `/(?:api.?key|secret|password|token$|authorization$|cookie$|credential|private.?key)/i`.
fn secret_name(key: &str) -> bool {
    let key = key.to_lowercase();
    let chars: Vec<char> = key.chars().collect();
    let gap = |prefix: &str, suffix: &str| {
        let p: Vec<char> = prefix.chars().collect();
        let s: Vec<char> = suffix.chars().collect();
        (0..chars.len()).any(|i| {
            chars[i..].starts_with(&p)
                && (chars[i + p.len()..].starts_with(&s)
                    || (chars.len() > i + p.len() && chars[i + p.len() + 1..].starts_with(&s)))
        })
    };
    gap("api", "key")
        || key.contains("secret")
        || key.contains("password")
        || key.ends_with("token")
        || key.ends_with("authorization")
        || key.ends_with("cookie")
        || key.contains("credential")
        || gap("private", "key")
}

fn url_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return false;
    }
    let rest = &text[text.find("://").map(|i| i + 3).unwrap_or(0)..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return true;
    }
    let Some(query) = rest.split_once('?').map(|(_, q)| q.split('#').next().unwrap_or("")) else { return false };
    crate::http::parse_query(query).iter().any(|(name, _)| secret_name(name))
}

/// `redactConfig`.
fn redact(value: &Json, headers: bool) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.iter().map(|i| redact(i, headers)).collect()),
        Json::Object(object) => {
            let mut out = Obj::new();
            for (key, item) in object {
                if let Json::String(text) = item {
                    if headers || secret_name(key) || url_secret(text) {
                        out.str(key, "***");
                        continue;
                    }
                }
                out.set(key, redact(item, headers || key.eq_ignore_ascii_case("headers")));
            }
            out.done()
        }
        other => other.clone(),
    }
}

/// `debug heap-dump`: only an already-running server (an explicit `--server`, or the service).
pub async fn heap_dump(globals: &Globals, input: &Input) -> Result {
    let directory = input.str("directory").map(|d| if d.starts_with('/') { d.to_owned() } else { crate::project::absolutize(d) });
    report(
        async {
            let endpoint = match &globals.server {
                Some(_) => connection::resolve(globals, Args::default()).await?.endpoint,
                None => {
                    let file = super::service::migrate_registrations();
                    super::service::discover(&file, None).await.ok_or_else(|| {
                        Fail::error("No background server is running. Pass --server <url> to capture a server started another way.")
                    })?
                }
            };
            let client = Client::new(&endpoint);
            let mut query = Obj::new();
            query.opt_str("directory", directory.as_deref());
            let snapshot = match client.call("debug.heapDump", &[], Some(query.done()), None).await {
                Ok(snapshot) => snapshot,
                Err(super::client::ClientError::UnexpectedStatus(404)) => {
                    return Err(Fail::error(format!(
                        "The server at {} does not support heap dumps. Update and restart it first.",
                        endpoint.url
                    )));
                }
                Err(error) => return Err(error.into()),
            };
            let path = json::get_str(&snapshot, "path").unwrap_or("").to_owned();
            if let Some(directory) = &directory {
                let name = path.rsplit('/').next().unwrap_or("");
                if format!("{}/{name}", directory.trim_end_matches('/')) != path {
                    stderr("The server ignored --directory. Update and restart it to choose where snapshots go.\n");
                }
            }
            stdout(&format!("{path}\n"));
            Ok(0)
        }
        .await,
    )
}

pub async fn config(globals: &Globals) -> Result {
    with_server(globals, Args::default(), |client| async move {
        let entries = client.call("config.get", &[], Some(location(&cwd())), None).await?;
        stdout(&format!("{}\n", pretty(&redact(&entries, false))));
        Ok(0)
    })
    .await
}

/// The session commands report failures as their bare message (`errorMessage`), exit 1.
fn report(result: Result) -> Result {
    match result {
        Err(fail) => {
            stderr(&format!("{}\n", fail.message));
            Ok(1)
        }
        ok => ok,
    }
}

/// `toLocaleString()` of a time in milliseconds, en-US style. TODO(port): the local time zone (UTC here).
fn locale_time(ms: f64) -> String {
    let seconds = (ms / 1000.0).floor() as i64;
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil(days);
    let (hour, minute, second) = (rest / 3600, rest % 3600 / 60, rest % 60);
    let (twelve, half) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{month}/{day}/{year}, {twelve}:{minute:02}:{second:02}\u{202f}{half}")
}

/// Days since 1970-01-01 to (year, month, day).
pub fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

pub async fn session_list(globals: &Globals, input: &Input) -> Result {
    let table = input.str("format") != Some("json");
    let limit = input.int("max-count");
    report(
        with_server(globals, Args::default(), |client| async move {
            let located = client.call("location.get", &[], Some(location(&cwd())), None).await?;
            let project = json::get(&located, "project").and_then(|p| json::get_str(p, "id")).unwrap_or("").to_owned();
            let mut query = Obj::new();
            query
                .str("project", &project)
                .set("parentID", Json::Null)
                .set("archived", Json::Bool(false))
                .str("order", "desc")
                .int("limit", limit.unwrap_or(100));
            let page = client.call("session.list", &[], Some(query.done()), None).await?;
            let sessions = data(&page);
            if table && sessions.is_empty() {
                return Ok(0);
            }
            let output = if table {
                sessions
                    .iter()
                    .map(|s| {
                        let title = json::get_str(s, "title").unwrap_or("Untitled session").replace(['\r', '\n', '\t'], " ");
                        let updated = json::get(s, "time").and_then(|t| json::get_num(t, "updated")).unwrap_or(0.0);
                        format!("{}\t{title}\t{}", json::get_str(s, "id").unwrap_or(""), locale_time(updated))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                let items = sessions
                    .iter()
                    .map(|s| {
                        let time = json::get(s, "time");
                        let mut o = Obj::new();
                        o.opt("id", json::get(s, "id").cloned())
                            .opt("title", json::get(s, "title").cloned())
                            .opt("updated", time.and_then(|t| json::get(t, "updated")).cloned())
                            .opt("created", time.and_then(|t| json::get(t, "created")).cloned())
                            .opt("projectId", json::get(s, "projectID").cloned())
                            .opt("directory", json::get(s, "location").and_then(|l| json::get(l, "directory")).cloned());
                        o.done()
                    })
                    .collect();
                pretty(&Json::Array(items))
            };
            // TODO(port): on a terminal without --max-count, the table goes through `less -R -S`.
            stdout(&format!("{output}\n"));
            Ok(0)
        })
        .await,
    )
}

pub async fn session_delete(globals: &Globals, input: &Input) -> Result {
    let id = input.str("sessionID").unwrap_or("").to_owned();
    report(
        with_server(globals, Args::default(), |client| async move {
            client.call("session.remove", &[("sessionID", &id)], None, None).await?;
            stdout(&format!("Session {id} deleted\n"));
            Ok(0)
        })
        .await,
    )
}

pub async fn session_export(globals: &Globals, input: &Input) -> Result {
    let requested = input.str("session").map(String::from);
    let sanitize = input.flag("sanitize");
    if requested.is_none() && !super::stdin_tty() {
        return report(Err(Fail::error("Pass a session ID when running without an interactive terminal")));
    }
    report(
        with_server(globals, Args::default(), |client| async move {
            // TODO(port): without a session ID on a terminal, a picker of the project's sessions.
            let Some(id) = requested else { return Ok(0) };
            let mut query = Obj::new();
            query.set("sanitize", Json::Bool(sanitize));
            let exported = client.call("experimental.session.export", &[("sessionID", &id)], Some(query.done()), None).await?;
            stdout(&format!("{}\n", pretty(json::get(&exported, "data").unwrap_or(&Json::Null))));
            Ok(0)
        })
        .await,
    )
}

pub async fn session_import(globals: &Globals, input: &Input) -> Result {
    let file = input.str("file").unwrap_or("").to_owned();
    let text = if file.starts_with("http://") || file.starts_with("https://") {
        match fetch("GET", &file, &[], None).await {
            Ok(answer) if (200..300).contains(&answer.status) => answer.body,
            Ok(answer) => {
                return Err(Fail::error(format!(
                    "Failed to read session data: Failed to fetch session data: {}",
                    answer.status_text()
                )));
            }
            Err(message) => return Err(Fail::error(format!("Failed to read session data: {message}"))),
        }
    } else {
        match std::fs::read(&file) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                let reason = match error.kind() {
                    std::io::ErrorKind::NotFound => format!("ENOENT: no such file or directory, open '{file}'"),
                    std::io::ErrorKind::PermissionDenied => format!("EACCES: permission denied, open '{file}'"),
                    _ => error.to_string(),
                };
                return Err(Fail::error(format!("Failed to read session data: {reason}")));
            }
        }
    };
    let Some(data) = json::parse(&text) else {
        return Err(Fail::named("SchemaError", "Expected a valid JSON string"));
    };
    let Json::Object(_) = data else { return Err(Fail::named("SchemaError", "Expected object")) };
    let directory = input.str("directory").map(|d| crate::project::absolutize(d)).unwrap_or_else(cwd);
    let server = connection::resolve(globals, Args::default()).await?;
    let result = async {
        let client = server.client();
        let located = client.call("location.get", &[], Some(location(&directory)), None).await?;
        let mut body = data.clone();
        let mut loc = Obj::new();
        loc.opt("directory", json::get(&located, "directory").cloned());
        json::set(&mut body, "location", loc.done());
        let mut headers = server.endpoint.headers();
        headers.push(("content-type".into(), "application/json".into()));
        let url = format!("{}/api/experimental/session/import", server.endpoint.url.trim_end_matches('/'));
        let answer = fetch("POST", &url, &headers, Some(json::text(&body))).await.map_err(|cause| {
            Fail::named("ClientError", format!("Transport: {cause}"))
        })?;
        if answer.status == 409 {
            stderr("Session already exists\n");
            return Ok(1);
        }
        if !(200..300).contains(&answer.status) {
            return Err(Fail::error(format!("Failed to import session: {}", answer.status_text())));
        }
        let id = json::parse(&answer.body)
            .and_then(|v| json::get(&v, "data").and_then(|d| json::get_str(d, "id")).map(String::from))
            .ok_or_else(|| Fail::named("SchemaError", "Expected a valid JSON string"))?;
        stdout(&format!("Imported session: {id}\n"));
        Ok(0)
    }
    .await;
    server.close().await;
    result
}

const METHODS: &[&str] = &["delete", "get", "head", "options", "patch", "post", "put"];

/// `opencode api`.
pub async fn api(globals: &Globals, input: &Input) -> Result {
    let request = input.list("operation | method path").to_vec();
    let params: Vec<(String, String)> = input.pairs("param").map(|p| p.to_vec()).unwrap_or_default();
    let server = connection::resolve(globals, Args::default()).await?;
    let result = async {
        let (method, path) = resolve_request(&request, &params)?;
        let mut headers = server.endpoint.headers();
        for header in input.list("header") {
            let index = header.find(':').filter(|i| *i >= 1);
            let Some(index) = index else {
                return Err(Fail::error(format!("Invalid header, expected name:value: {header}")));
            };
            let name = header[..index].trim().to_ascii_lowercase();
            headers.retain(|(n, _)| !n.eq_ignore_ascii_case(&name));
            headers.push((name, header[index + 1..].trim().to_owned()));
        }
        let body = input.str("data").map(String::from);
        if body.is_some() && !headers.iter().any(|(n, _)| n == "content-type") {
            headers.push(("content-type".into(), "application/json".into()));
        }
        let url = format!("{}{path}", server.endpoint.url.trim_end_matches('/'));
        let answer = fetch(&method, &url, &headers, body).await.map_err(|cause| Fail::named("TypeError", cause))?;
        if !answer.body.is_empty() {
            let newline = if answer.body.ends_with('\n') { "" } else { "\n" };
            stdout(&format!("{}{newline}", answer.body));
        }
        if !(200..300).contains(&answer.status) {
            stderr(&format!("HTTP {} {}\n", answer.status, answer.status_text()));
            return Ok(1);
        }
        Ok(0)
    }
    .await;
    server.close().await;
    result
}

/// `rawRequest`, else the operation from the route table (the TS CLI reads it from the server's
/// `/openapi.json`; the Rust build knows its own API).
fn resolve_request(request: &[String], params: &[(String, String)]) -> Result<(String, String)> {
    if request.len() == 2 && METHODS.contains(&request[0].to_ascii_lowercase().as_str()) && request[1].starts_with('/') {
        return Ok((request[0].to_ascii_uppercase(), request[1].clone()));
    }
    if request.len() != 1 {
        return Err(Fail::error("Expected an operation name or an HTTP method and path"));
    }
    let operation = &request[0];
    let route = opencode_protocol::routes::ROUTES
        .iter()
        .find(|r| r.operation == operation.as_str())
        .ok_or_else(|| Fail::error(format!("Operation not found: {operation}")))?;
    let mut used = vec![];
    let mut path = String::new();
    for segment in route.path.split('/').skip(1) {
        path.push('/');
        match segment.strip_prefix(':') {
            Some(name) => {
                let value = params
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v)
                    .ok_or_else(|| Fail::error(format!("Missing path parameter: {name}")))?;
                used.push(name.to_owned());
                path.push_str(&encode_component(value));
            }
            None => path.push_str(segment),
        }
    }
    let query: Vec<String> = params
        .iter()
        .filter(|(n, _)| !used.contains(n))
        .map(|(n, v)| format!("{}={}", form_encode(n), form_encode(v)))
        .collect();
    if !query.is_empty() {
        path.push('?');
        path.push_str(&query.join("&"));
    }
    Ok((route.method.to_owned(), path))
}
