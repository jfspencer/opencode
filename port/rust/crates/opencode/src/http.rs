//! The HTTP server (packages/server/src/process.ts): authentication, routing over the generated route table, and
//! response bodies (JSON, empty, server-sent events).

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{HeaderMap, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use opencode_protocol::Json;
use opencode_protocol::routes::{ROUTES, Route};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::app::App;
use crate::json::Obj;

pub struct Request {
    /// The query string's pairs, percent-decoded, in order.
    pub query: Vec<(String, String)>,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub route: &'static Route,
    pub params: HashMap<&'static str, String>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn param(&self, name: &str) -> &str {
        self.params.get(name).map(String::as_str).unwrap_or("")
    }

    /// The query as Effect's string tree: a repeated key is an array of its values.
    pub fn query_tree(&self) -> Json {
        let mut object = opencode_protocol::wire::Object::new();
        for (key, value) in &self.query {
            match object.get_mut(key) {
                Some(Json::Array(items)) => items.push(Json::String(value.clone())),
                Some(existing) => {
                    let first = std::mem::take(existing);
                    *existing = Json::Array(vec![first, Json::String(value.clone())]);
                }
                None => {
                    object.insert(key.clone(), Json::String(value.clone()));
                }
            }
        }
        Json::Object(object)
    }

    pub fn query_value(&self, key: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

pub enum Body {
    Empty,
    Json(String),
    /// Server-sent events: each message is one already-framed chunk.
    Events(mpsc::Receiver<Bytes>),
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Body,
}

impl Response {
    pub fn json(status: u16, text: String) -> Self {
        Self { status, headers: vec![], body: Body::Json(text) }
    }

    pub fn value(status: u16, value: &Json) -> Self {
        Self::json(status, crate::json::text(value))
    }

    pub fn no_content() -> Self {
        Self { status: 204, headers: vec![], body: Body::Empty }
    }

    pub fn events(receiver: mpsc::Receiver<Bytes>) -> Self {
        Self {
            status: 200,
            headers: vec![
                ("cache-control", "no-cache, no-transform".into()),
                ("x-accel-buffering", "no".into()),
                ("x-content-type-options", "nosniff".into()),
            ],
            body: Body::Events(receiver),
        }
    }
}

/// A failed request: the declared error's status and its encoded body.
#[derive(Debug)]
pub struct Failure {
    pub status: u16,
    pub body: Json,
}

impl Failure {
    pub fn new(status: u16, body: Json) -> Self {
        Self { status, body }
    }

    /// `InvalidRequestError` (400) with a message and optional field/kind.
    pub fn invalid(message: impl Into<String>, field: Option<&str>, kind: Option<&str>) -> Self {
        let mut o = Obj::new();
        o.str("_tag", "InvalidRequestError");
        o.str("message", &message.into());
        o.opt_str("kind", kind);
        o.opt_str("field", field);
        Self::new(400, o.done())
    }

    /// A schema rejection of one part of the request (`kind`: Params, Query, Headers, Payload), as
    /// packages/server/src/middleware/schema-error.ts reports it.
    pub fn schema(kind: &str, issue: &opencode_protocol::wire::Issue) -> Self {
        Self::invalid(truncate_reason(&issue.to_string()), None, Some(kind))
    }

    pub fn into_response(self) -> Response {
        Response::value(self.status, &self.body)
    }
}

fn truncate_reason(reason: &str) -> String {
    const LIMIT: usize = 1024;
    let units: Vec<u16> = reason.encode_utf16().collect();
    if units.len() <= LIMIT {
        return reason.to_owned();
    }
    format!("{}... ({} more chars)", String::from_utf16_lossy(&units[..LIMIT]), units.len() - LIMIT)
}

pub type Result<T> = std::result::Result<T, Failure>;

type Out = hyper::Response<BoxBody<Bytes, Infallible>>;

pub async fn serve(app: Arc<App>, listener: TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let _ = stream.set_nodelay(true);
        let app = app.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let app = app.clone();
                async move { Ok::<_, Infallible>(handle(app, request).await) }
            });
            let _ = http1::Builder::new().keep_alive(true).serve_connection(TokioIo::new(stream), service).await;
        });
    }
}

pub fn local_address(listener: &TcpListener) -> Option<SocketAddr> {
    listener.local_addr().ok()
}

async fn handle(app: Arc<App>, request: hyper::Request<Incoming>) -> Out {
    let (parts, body) = request.into_parts();
    let method = parts.method.as_str().to_owned();
    let raw_path = parts.uri.path().to_owned();
    let query = parts.uri.query().map(parse_query).unwrap_or_default();
    let headers = parts.headers;

    if !crate::auth::authorized(&app, &headers, &query) {
        return finish(crate::auth::unauthorized(&headers));
    }

    let Some((route, params)) = find(&method, &raw_path) else {
        return finish(Response { status: 404, headers: vec![], body: Body::Empty });
    };
    let body = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => Bytes::new(),
    };
    let request = Request { query, headers, body, route, params };
    let response = crate::api::dispatch(&app, request).await;
    finish(response)
}

fn finish(response: Response) -> Out {
    let mut builder = hyper::Response::builder().status(response.status);
    for (name, value) in &response.headers {
        if let Ok(value) = HeaderValue::from_str(value) {
            builder = builder.header(*name, value);
        }
    }
    let body: BoxBody<Bytes, Infallible> = match response.body {
        Body::Empty => Full::new(Bytes::new()).boxed(),
        Body::Json(text) => {
            builder = builder.header("content-type", "application/json");
            Full::new(Bytes::from(text)).boxed()
        }
        Body::Events(receiver) => {
            builder = builder.header("content-type", "text/event-stream");
            BodyExt::boxed(futures_stream(receiver))
        }
    };
    builder.body(body).unwrap()
}

fn futures_stream(receiver: mpsc::Receiver<Bytes>) -> impl hyper::body::Body<Data = Bytes, Error = Infallible> + Send + 'static {
    struct Events(mpsc::Receiver<Bytes>);
    impl hyper::body::Body for Events {
        type Data = Bytes;
        type Error = Infallible;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<Frame<Bytes>, Infallible>>> {
            self.0.poll_recv(cx).map(|chunk| chunk.map(|bytes| Ok(Frame::data(bytes))))
        }
    }
    Events(receiver)
}

/// `application/x-www-form-urlencoded` pairs (`URLSearchParams`): `+` is a space, invalid escapes are kept.
pub fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (form_decode(key), form_decode(value))
        })
        .collect()
}

fn form_decode(text: &str) -> String {
    percent_decode(&text.replace('+', " "))
}

/// Percent-decoding as `decodeURIComponent` without failing: invalid sequences stay as they are.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// The route for a request: static segments win over parameters, parameters over a trailing wildcard (the
/// router's precedence).
fn find(method: &str, path: &str) -> Option<(&'static Route, HashMap<&'static str, String>)> {
    let segments: Vec<&str> = path.split('/').collect();
    let mut best: Option<(Vec<u8>, &'static Route, HashMap<&'static str, String>)> = None;
    for route in ROUTES {
        if route.method != method {
            continue;
        }
        let Some((rank, params)) = matches(route.path, &segments) else { continue };
        if best.as_ref().is_none_or(|(r, _, _)| rank < *r) {
            best = Some((rank, route, params));
        }
    }
    best.map(|(_, route, params)| (route, params))
}

fn matches(pattern: &'static str, segments: &[&str]) -> Option<(Vec<u8>, HashMap<&'static str, String>)> {
    let parts: Vec<&'static str> = pattern.split('/').collect();
    let mut rank = Vec::with_capacity(parts.len());
    let mut params = HashMap::new();
    for (index, part) in parts.iter().enumerate() {
        if *part == "*" {
            rank.push(2);
            params.insert("*", segments.get(index..).map(|rest| rest.join("/")).unwrap_or_default());
            return Some((rank, params));
        }
        let segment = segments.get(index)?;
        if let Some(name) = part.strip_prefix(':') {
            if segment.is_empty() {
                return None;
            }
            rank.push(1);
            params.insert(name, percent_decode(segment));
        } else if part != segment {
            return None;
        } else {
            rank.push(0);
        }
    }
    (parts.len() == segments.len()).then_some((rank, params))
}
