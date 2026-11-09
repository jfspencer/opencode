//! Request dispatch: the HttpApi pipeline of packages/server/src/routes.ts. Middleware first (each endpoint's, from the
//! generated route table: the session and form location middleware validate and load the session and boot its
//! location; the location middleware boots the request's location), then
//! the request is decoded against the endpoint's schemas (path parameters, query, payload; a failure is a 400
//! `InvalidRequestError` naming the part), then the handler runs.

use bytes::Bytes;
use opencode_protocol::Json;
use opencode_protocol::roots;
use opencode_protocol::routes::{BodyKind, Middleware};
use tokio::sync::mpsc;

use crate::app::App;
use crate::bus::LocationRef;
use crate::http::{Failure, Request, Response, Result};
use crate::json::{self, Obj};
use crate::session;

/// What the pipeline decoded: canonical JSON of each request part.
pub struct Decoded {
    #[allow(dead_code)]
    pub params: Json,
    pub query: Json,
    pub payload: Json,
}

/// The location a request is placed in (`requestRef`): `location[directory]` in the query, the
/// `x-opencode-directory` header (URI-encoded), or the server's directory.
pub fn request_location(app: &App, request: &Request) -> LocationRef {
    let directory = request
        .query_value("location[directory]")
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .or_else(|| request.header("x-opencode-directory").map(|h| crate::text::decode_uri_component(h).unwrap_or_else(|| h.to_owned())))
        .unwrap_or_else(|| app.cwd.clone());
    LocationRef::directory(directory)
}

pub async fn dispatch(app: &App, request: Request) -> Response {
    match run(app, &request).await {
        Ok(response) => response,
        Err(failure) => failure.into_response(),
    }
}

async fn run(app: &App, request: &Request) -> Result<Response> {
    let route = request.route;
    let located = match route.middleware {
        Middleware::None => None,
        Middleware::Location => Some(app.locations.get(app, &request_location(app, request)).await?),
        // The form middleware's undocumented `global` session stands for the request's location (MCP elicitations
        // without a session).
        Middleware::Form if request.param("sessionID") == "global" => {
            Some(app.locations.get(app, &request_location(app, request)).await?)
        }
        Middleware::Session | Middleware::Form => {
            let id = request.param("sessionID");
            if !id.starts_with("ses") {
                return Err(Failure::invalid("Invalid session ID", Some("sessionID"), None));
            }
            let stored = session::require(&app.db.lock().unwrap(), id)?;
            // Forms live in memory: a session whose location is not loaded has none pending, and listing them must
            // not boot it.
            if route.operation == "session.form.list" && app.locations.cached(&stored.location).is_none() {
                return Ok(crate::handlers::data(Json::Array(vec![])));
            }
            Some(app.locations.get(app, &stored.location).await?)
        }
    };
    let decoded = decode(request)?;
    crate::handlers::handle(app, request, decoded, located).await
}

fn decode_part(kind: &str, ty: Option<&str>, value: Json) -> Result<Json> {
    let Some(ty) = ty else { return Ok(value) };
    match roots::roundtrip(ty, &value) {
        Some(Ok(text)) => Ok(json::parse(&text).unwrap_or(Json::Null)),
        Some(Err(issue)) => Err(Failure::schema(kind, &issue)),
        None => Ok(value),
    }
}

fn decode(request: &Request) -> Result<Decoded> {
    let route = request.route;
    let mut params = Obj::new();
    for (name, value) in &request.params {
        params.str(name, value);
    }
    let params = decode_part("Params", route.params, params.done())?;
    let query = decode_part("Query", route.query, request.query_tree())?;
    // HttpApiBuilder `decodePayload`: the payload schema is chosen by the request's media type (JSON when it has
    // none); a body of any other type is refused.
    if !route.payload.is_empty() {
        let has_body = !matches!(route.method, "GET" | "HEAD" | "OPTIONS" | "TRACE");
        let content_type = if has_body {
            let raw = request.header("content-type").unwrap_or("application/json").to_lowercase();
            let raw = raw.trim();
            raw.split(';').next().unwrap_or(raw).trim().to_owned()
        } else {
            "application/x-www-form-urlencoded".to_owned()
        };
        if !route.payload.iter().any(|body| body.content_type.is_some_and(|ct| ct == content_type)) {
            return Err(Failure::text(415, format!("Unsupported content-type: {content_type}")));
        }
    }
    let payload = match route.payload.first() {
        Some(body) if body.kind == BodyKind::Json => {
            // The request's text as Bun decodes it (invalid UTF-8 replaced, one leading BOM dropped). Only an empty
            // text is no body: it decodes as `undefined`, which reports as `null` does. A text JSON.parse refuses
            // (white space only included) is HttpApiBuilder's own rejection, of kind `Payload` as for a schema issue.
            let text = String::from_utf8_lossy(&request.body);
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            let mut value = if text.is_empty() {
                Json::Null
            } else {
                json::parse(text).ok_or_else(|| Failure::invalid("Expected a valid JSON body", None, Some("Payload")))?
            };
            // `ImportedMessages`: an import's messages are plain JSON on the wire, upgraded from the names they had
            // before v2.1 (`SessionLegacy.messages`) and only then decoded as current messages.
            if route.operation == "experimental.session.import" {
                if let Some(Json::Array(messages)) = json::get_mut(&mut value, "messages") {
                    *messages = crate::session::transfer::legacy_messages(messages);
                }
            }
            decode_part("Payload", body.ty, value)?
        }
        Some(body) if body.kind == BodyKind::Query => decode_part("Payload", body.ty, request.query_tree())?,
        _ => Json::Null,
    };
    Ok(Decoded { params, query, payload })
}

/// Sends `chunks` as a server-sent event stream until the client goes away.
pub fn event_stream() -> (mpsc::Sender<Bytes>, Response) {
    let (sender, receiver) = mpsc::channel(64);
    (sender, Response::events(receiver))
}

pub fn not_implemented(operation: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "UnknownError");
    o.str("message", &format!("Not implemented by the Rust server yet: {operation}"));
    Failure::new(501, o.done())
}
