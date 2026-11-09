//! Projects, locations, server info and debug (handlers/project.ts, location.ts, server.ts, debug.ts; server/src
//! pairing.ts, server-info.ts and the session tokens of auth.ts).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opencode_protocol::Json;
use opencode_protocol::Wire;
use opencode_protocol::types as t;

use crate::api::Decoded;
use crate::app::{App, fatal};
use crate::http::{Body, Failure, Request, Response, Result};
use crate::json::Obj;
use crate::location::Location;

pub async fn handle(app: &App, request: &Request, decoded: Decoded, located: Option<Arc<Location>>) -> Result<Response> {
    match request.route.operation {
        "project.list" => {
            let query = t::ProjectListQuery::decode(&decoded.query).map_err(|issue| Failure::schema("Query", &issue))?;
            let archived = matches!(query.archived, Some(t::ProjectListQueryArchived0::True2));
            let list = crate::project::list(&app.db.lock().unwrap(), archived).map_err(fatal)?;
            Ok(super::encoded(request, &Json::Array(list)))
        }
        "project.update" => project_update(app, request, &decoded),
        "location.get" => Ok(super::encoded(request, &located.expect("located endpoint").info())),
        "location.reload" => {
            // TODO(port): the failure's text is Effect's `Cause.pretty` of the boot failure.
            app.locations.reload(app).await.map_err(|failure| {
                let message = crate::json::get_str(&failure.body, "message").unwrap_or("Location reload failed").to_owned();
                service_unavailable(&message, "location")
            })?;
            Ok(Response::no_content())
        }
        "server.info" => Ok(super::encoded(request, &server_info())),
        "server.pair" => Ok(super::encoded(request, &pairing_issue())),
        "server.connect" => Ok(server_connect(app, request)),
        "debug.heapDump" => heap_dump(request, &decoded),
        "debug.location.list" => {
            let keys = app.locations.keys().iter().map(|reference| reference.to_json()).collect();
            Ok(super::encoded(request, &Json::Array(keys)))
        }
        "debug.location.evict" => {
            // Resolved through requestRef so the key matches the shape the location middleware cached it under.
            app.locations.invalidate(app, &crate::api::request_location(app, request));
            Ok(Response::no_content())
        }
        _ => super::fallback(app, request),
    }
}

/// `ServiceUnavailableError` (503).
pub fn service_unavailable(message: &str, service: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "ServiceUnavailableError").str("message", message).str("service", service);
    Failure::new(503, o.done())
}

fn project_update(app: &App, request: &Request, decoded: &Decoded) -> Result<Response> {
    let payload = t::ProjectUpdatePayload::decode(&decoded.payload).map_err(|issue| Failure::schema("Payload", &issue))?;
    let id = request.param("projectID");
    // A relative canonical directory is rejected before the lookup (D220; upstream dies when it is stored).
    if payload.canonical.as_ref().is_some_and(|c| !c.0.starts_with('/')) {
        let mut o = Obj::new();
        o.str("_tag", "InvalidRequestError").str("message", "canonical must be an absolute path").str("field", "canonical");
        return Err(Failure::new(400, o.done()));
    }
    let input = crate::project::Update {
        canonical: payload.canonical.map(|c| c.0),
        name: payload.name,
        icon_override: payload.icon.as_ref().and_then(|icon| icon.r#override.clone()),
        icon_color: payload.icon.as_ref().and_then(|icon| icon.color.clone()),
        start: payload.commands.and_then(|commands| commands.start),
    };
    let updated = crate::project::update(&app.db.lock().unwrap(), &app.bus, id, input).map_err(fatal)?;
    match updated {
        Some(info) => Ok(super::encoded(request, &info)),
        None => {
            let mut o = Obj::new();
            o.str("_tag", "ProjectNotFoundError").str("projectID", id).str("message", &format!("Project not found: {id}"));
            Err(Failure::new(404, o.done()))
        }
    }
}

/// `Global.Path.tmp`: `<os.tmpdir()>/opencode`, created at startup and canonicalized.
fn global_tmp() -> String {
    let base = std::env::var("TMPDIR").ok().filter(|v| !v.is_empty()).map(|v| {
        let trimmed = v.trim_end_matches('/');
        if trimmed.is_empty() { "/".to_owned() } else { trimmed.to_owned() }
    });
    let tmp = crate::project::normalize(&format!("{}/opencode", base.unwrap_or_else(|| "/tmp".into())));
    let _ = std::fs::create_dir_all(&tmp);
    crate::project::resolve_path(&tmp)
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname").ok().map(|h| h.trim().to_owned()).filter(|h| !h.is_empty())
}

fn server_info() -> Json {
    let mut paths = Obj::new();
    paths.str("tmp", &global_tmp());
    // TODO(port): persistent PTYs (PersistentPty.available) are the shell slice's; the TS server on Linux has them.
    let mut capabilities = Obj::new();
    capabilities.set("persistentPty", Json::Bool(true));
    let mut o = Obj::new();
    o.str("version", crate::app::VERSION);
    o.int("pid", std::process::id() as i64);
    o.opt_str("hostname", hostname().as_deref());
    o.set("urls", Json::Array(crate::app::urls().into_iter().map(Json::String).collect()));
    o.set("paths", paths.done());
    o.set("capabilities", capabilities.done());
    o.done()
}

// --- pairing (server/src/pairing.ts) ---

const PAIRING_TTL: Duration = Duration::from_secs(5 * 60);
const PAIRING_CAPACITY: usize = 1_000;

fn codes() -> &'static Mutex<HashMap<String, Instant>> {
    static CODES: std::sync::OnceLock<Mutex<HashMap<String, Instant>>> = std::sync::OnceLock::new();
    CODES.get_or_init(Default::default)
}

/// `ServerPairing.issue`: a one-time code valid for five minutes.
fn pairing_issue() -> Json {
    use base64::Engine;
    let mut bytes = [0u8; 16];
    crate::id::random(&mut bytes);
    let code = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    {
        let mut codes = codes().lock().unwrap();
        codes.retain(|_, issued| issued.elapsed() < PAIRING_TTL);
        // The cache holds at most 1000 codes; the oldest goes first.
        if codes.len() >= PAIRING_CAPACITY {
            if let Some(oldest) = codes.iter().min_by_key(|(_, issued)| **issued).map(|(c, _)| c.clone()) {
                codes.remove(&oldest);
            }
        }
        codes.insert(code.clone(), Instant::now());
    }
    let mut o = Obj::new();
    o.str("code", &code).int("expires_in", PAIRING_TTL.as_secs() as i64);
    o.done()
}

/// `ServerPairing.consume`: whether the code was valid (it is used up).
fn pairing_consume(code: &str) -> bool {
    codes().lock().unwrap().remove(code).is_some_and(|issued| issued.elapsed() < PAIRING_TTL)
}

/// `GET /auth/connect/:code`: exchanges a pairing code for a session token (as JSON for API clients, as a cookie
/// and a redirect for browser navigations).
fn server_connect(app: &App, request: &Request) -> Response {
    let browser = request.header("accept").is_some_and(|accept| accept.contains("text/html"));
    let token = pairing_consume(request.param("code")).then(|| app.password.as_deref().map(crate::auth::issue_session)).flatten();
    let Some(token) = token else {
        if !browser {
            let mut o = Obj::new();
            o.str("_tag", "UnauthorizedError").str("message", "Pairing link expired or already used");
            return Response::value(401, &o.done());
        }
        return Response {
            status: 401,
            headers: vec![("content-type", "text/plain".into())],
            body: Body::Bytes(bytes::Bytes::from_static(
                b"This pairing link expired or was already used. Run `opencode pair` to get a new one.",
            )),
        };
    };
    if !browser {
        let mut o = Obj::new();
        o.str("token", &token);
        return super::encoded(request, &o.done());
    }
    let name = crate::auth::session_cookie_name(request.header("host"));
    let max_age = crate::auth::SESSION_TTL_SECONDS;
    Response {
        status: 302,
        headers: vec![
            ("location", "/".into()),
            ("set-cookie", format!("{name}={token}; Max-Age={max_age}; Path=/; HttpOnly; SameSite=Lax")),
        ],
        body: Body::Empty,
    }
}

// --- heap snapshots (util/src/heap-snapshot.ts) ---

fn heap_dump(request: &Request, decoded: &Decoded) -> Result<Response> {
    let query = t::DebugHeapDumpQuery::decode(&decoded.query).map_err(|issue| Failure::schema("Query", &issue))?;
    if query.directory.as_ref().is_some_and(|d| !d.starts_with('/')) {
        return Err(Failure::invalid("directory must be an absolute path", Some("directory"), None));
    }
    let directory = query.directory.unwrap_or_else(|| {
        let base = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
        format!("{base}/opencode/log")
    });
    let failed = |message: String| {
        let mut reference = [0u8; 4];
        crate::id::random(&mut reference);
        let reference: String = reference.iter().map(|b| format!("{b:02x}")).collect();
        eprintln!("failed to write heap snapshot (ref err_{reference}): {message}");
        let mut o = Obj::new();
        o.str("_tag", "UnknownError").str("message", &format!("Failed to write heap snapshot: {message}")).str("ref", &format!("err_{reference}"));
        Failure::new(500, o.done())
    };
    let time = crate::fs::utc_iso(crate::id::now()).replace([':', '.'], "");
    let file = crate::project::normalize(&format!("{directory}/heap-{}-{time}.heapsnapshot", std::process::id()));
    std::fs::create_dir_all(&directory).map_err(|e| failed(crate::fs::node_error(&e, "mkdir", &directory)))?;
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
            .map_err(|e| failed(crate::fs::node_error(&e, "open", &file)))?;
    }
    // TODO(port): there is no V8 heap to snapshot; the file is left empty and only the resident size is reported.
    let rss = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split_whitespace().nth(1).and_then(|pages| pages.parse::<f64>().ok()))
        .map(|pages| pages * 4096.0)
        .unwrap_or(0.0);
    let mut memory = Obj::new();
    memory.set("rss", Json::Number(rss));
    for key in ["heapTotal", "heapUsed", "external", "arrayBuffers"] {
        memory.set(key, Json::Number(0.0));
    }
    let mut o = Obj::new();
    o.str("path", &file).int("pid", std::process::id() as i64).set("memory", memory.done());
    Ok(super::encoded(request, &o.done()))
}
