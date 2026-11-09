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
        .or_else(|| request.header("x-opencode-directory").map(crate::http::percent_decode))
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
        // without a session). TODO(port): `session.form.list` of a session without live capabilities answers
        // `{data: []}` without booting its location.
        Middleware::Form if request.param("sessionID") == "global" => {
            Some(app.locations.get(app, &request_location(app, request)).await?)
        }
        Middleware::Session | Middleware::Form => {
            let id = request.param("sessionID");
            if !id.starts_with("ses") {
                return Err(Failure::invalid("Invalid session ID", Some("sessionID"), None));
            }
            let stored = session::require(&app.db.lock().unwrap(), id)?;
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
    let payload = match route.payload.first() {
        Some(body) if body.kind == BodyKind::Json => {
            let text = std::str::from_utf8(&request.body).unwrap_or("");
            // An empty body decodes as `undefined`, which reports as `null` does; a body that is not JSON is
            // HttpApiBuilder's own rejection. TODO(port): confirm its `kind` against the TS server.
            let value = if text.trim().is_empty() {
                Json::Null
            } else {
                json::parse(text).ok_or_else(|| Failure::invalid("Expected a valid JSON body", None, Some("Payload")))?
            };
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
