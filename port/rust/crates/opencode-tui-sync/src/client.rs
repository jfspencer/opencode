//! The promise client (packages/client/src/promise/generated/client.ts and client.ts): every operation by name
//! with the TS client's request descriptors (generated into `generated/ops.rs` from the TS client itself),
//! Basic credentials, declared error bodies thrown as their tagged errors, other failures as `ClientError`s, and
//! the server-sent event stream.
//!
//! An operation takes its input as the TS client does: one JSON object holding the path parameters, query,
//! header and body fields by their TS names (`{ sessionID, text, ... }`). [`Client::call`] starts the request at
//! once, as the TS `request()` calls `fetch` before its first `await`; the returned [`Call`] settles with the
//! response. `api.session.revert.stage(input)` is `client.api().session().revert().stage(&input, options)`.
//!
//! Divergences, all at the edges of what JS values can be:
//! - Inputs are JSON: no `undefined` (leave the key out) and no binary bodies (`experimental.file.write` sends a
//!   string payload's UTF-8 bytes).
//! - JSON text with lone surrogate escapes is rejected as malformed (a Rust string cannot hold it), and a
//!   `MalformedResponse` names this crate's parse error, not JavaScriptCore's (`JSON Parse error: ...`).

use std::rc::Rc;

use opencode_protocol::wire::Out;
use opencode_protocol::{Json, Wire};

use crate::abort::{AbortSignal, Reason};
use crate::js;
use crate::shared_events::SharedEvents;
use crate::transport::{LocalBoxFuture, Request, Response, ResponseBody, Transport};

pub use crate::generated::ops;
pub use crate::generated::ops::Api;

/// One segment of an operation's path template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Literal(&'static str),
    /// `${encodeURIComponent(input.<key>)}`.
    Param(&'static str),
    /// `${encodePath(input.<key>)}`: each `/`-separated part encoded.
    Path(&'static str),
}

/// What an operation sends as its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    None,
    /// `{ <key>: input[<input key>], ... }` as JSON.
    Fields(&'static [(&'static str, &'static str)]),
    /// `input[<key>]` as JSON.
    Input(&'static str),
    /// `input[<key>]` as raw bytes (`application/octet-stream`).
    Binary(&'static str),
}

/// A request descriptor of the TS client.
#[derive(Debug)]
pub struct Operation {
    pub name: &'static str,
    pub method: &'static str,
    pub path: &'static [Segment],
    /// `(query key, input key)`.
    pub query: &'static [(&'static str, &'static str)],
    pub headers: &'static [(&'static str, &'static str)],
    pub body: Body,
    pub success: u16,
    pub declared: &'static [u16],
    /// The success response has no content.
    pub empty: bool,
    /// The success response is bytes.
    pub binary: bool,
    /// A server-sent event stream (`sse()`).
    pub stream: bool,
    /// The result is the response's `data` (`.then((value) => value.data)`).
    pub data: bool,
}

/// `ClientOptions`.
#[derive(Clone)]
pub struct ClientOptions {
    pub base_url: String,
    /// Sent with every request (`options.headers`), e.g. the Basic credentials of [`basic_auth`].
    pub headers: Vec<(String, String)>,
    pub transport: Rc<dyn Transport>,
}

/// `RequestOptions`.
#[derive(Clone, Default)]
pub struct RequestOptions {
    pub signal: Option<AbortSignal>,
    pub headers: Vec<(String, String)>,
    /// Reports every chunk a streaming response receives, including keepalive comments that yield no event.
    pub on_activity: Option<Rc<dyn Fn()>>,
}

impl RequestOptions {
    pub fn signal(signal: &AbortSignal) -> Self {
        Self { signal: Some(signal.clone()), ..Default::default() }
    }
}

/// `Service.headers(endpoint)`: HTTP Basic credentials.
pub fn basic_auth(username: &str, password: &str) -> (String, String) {
    use base64::Engine;
    let token = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    ("authorization".into(), format!("Basic {token}"))
}

/// Why a `ClientError` was thrown (`ClientErrorReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientErrorReason {
    Transport,
    UnexpectedStatus,
    UnsupportedContentType,
    MalformedResponse,
    SseEventTooLarge,
}

impl ClientErrorReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "Transport",
            Self::UnexpectedStatus => "UnexpectedStatus",
            Self::UnsupportedContentType => "UnsupportedContentType",
            Self::MalformedResponse => "MalformedResponse",
            Self::SseEventTooLarge => "SseEventTooLarge",
        }
    }
}

/// A rejected operation.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// `ClientError`: its message is `${reason}: ${detail}`, or the reason alone.
    Client {
        reason: ClientErrorReason,
        detail: Option<String>,
        /// The status of an `UnexpectedStatus` (`cause.status`).
        status: Option<u16>,
        /// The `cause` when it was an error (a transport failure, an abort reason, a JSON syntax error).
        cause: Option<Reason>,
    },
    /// A declared error body thrown as an `Error` named after its `_tag`, with the body's fields on it.
    Declared { status: u16, body: Json },
    /// Any other rejection (the raw error of reading a binary body).
    Other(Reason),
}

impl Error {
    fn client(reason: ClientErrorReason, cause: Option<Reason>) -> Self {
        let detail = cause.as_ref().map(|cause| cause.message.clone());
        Error::Client { reason, detail, status: None, cause }
    }

    fn detail(reason: ClientErrorReason, detail: Option<String>) -> Self {
        Error::Client { reason, detail, status: None, cause: None }
    }

    /// A response the schema of its type rejects (see the crate docs).
    pub fn malformed(issue: impl std::fmt::Display) -> Self {
        Self::client(ClientErrorReason::MalformedResponse, Some(Reason::error(issue.to_string())))
    }

    /// `error.name`.
    pub fn name(&self) -> String {
        match self {
            Error::Client { .. } => "ClientError".into(),
            Error::Declared { body, .. } => match field(body, "_tag") {
                Some(Json::String(tag)) if !tag.is_empty() => tag.clone(),
                _ => match field(body, "name") {
                    Some(name) => js::string(name),
                    None => "Error".into(),
                },
            },
            Error::Other(reason) => reason.name.clone(),
        }
    }

    /// `error.message`.
    pub fn message(&self) -> String {
        match self {
            Error::Client { reason, detail: Some(detail), .. } if !detail.is_empty() => {
                format!("{}: {detail}", reason.as_str())
            }
            Error::Client { reason, .. } => reason.as_str().into(),
            Error::Declared { body, .. } => match field(body, "message") {
                Some(message) => js::string(message),
                None => match field(body, "data").and_then(|data| field(data, "message")) {
                    Some(message) => js::string(message),
                    None => String::new(),
                },
            },
            Error::Other(reason) => reason.message.clone(),
        }
    }

    /// The declared error's `_tag` (what the generated `is<Tag>` guards test).
    pub fn tag(&self) -> Option<&str> {
        match self {
            Error::Declared { body, .. } => match field(body, "_tag") {
                Some(Json::String(tag)) => Some(tag),
                _ => None,
            },
            _ => None,
        }
    }

    /// `is<Tag>(error)`.
    pub fn is(&self, tag: &str) -> bool {
        self.tag() == Some(tag)
    }

    /// A field of a declared error body (`error.sessionID`).
    pub fn field(&self, key: &str) -> Option<&Json> {
        match self {
            Error::Declared { body, .. } => field(body, key),
            _ => None,
        }
    }

    /// A string field of a declared error body.
    pub fn field_str(&self, key: &str) -> Option<&str> {
        match self.field(key) {
            Some(Json::String(text)) => Some(text),
            _ => None,
        }
    }

    pub fn reason(&self) -> Option<ClientErrorReason> {
        match self {
            Error::Client { reason, .. } => Some(*reason),
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name(), self.message())
    }
}

impl std::error::Error for Error {}

fn field<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    value.as_object().and_then(|object| object.get(key))
}

/// What a call resolves to.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `undefined`: an empty response, or a missing `data`.
    Undefined,
    Json(Json),
    /// A binary response (`Uint8Array`).
    Bytes(Vec<u8>),
}

impl Value {
    /// The JSON value (`undefined` and bytes as `null`).
    pub fn json(self) -> Json {
        match self {
            Value::Json(json) => json,
            _ => Json::Null,
        }
    }

    /// Decodes the value through its schema type.
    pub fn decode<T: Wire>(&self) -> Result<T, Error> {
        match self {
            Value::Json(json) => T::decode(json).map_err(Error::malformed),
            _ => T::decode(&Json::Null).map_err(Error::malformed),
        }
    }

    pub fn bytes(self) -> Vec<u8> {
        match self {
            Value::Bytes(bytes) => bytes,
            _ => Vec::new(),
        }
    }
}

/// An operation's pending result (the TS promise).
pub type Call = LocalBoxFuture<'static, Result<Value, Error>>;

struct Inner {
    base_url: String,
    headers: Vec<(String, String)>,
    transport: Rc<dyn Transport>,
    events: std::cell::OnceCell<SharedEvents>,
}

/// `OpenCode.make(options)` (promise/client.ts): a client of one server, whose `event` is the shared event stream.
#[derive(Clone)]
pub struct Client {
    inner: Rc<Inner>,
    media_refs: bool,
}

const MEDIA_REF_OPERATIONS: [&str; 3] = ["message.list", "session.message.get", "session.inbox.list"];

impl Client {
    pub fn make(options: ClientOptions) -> Self {
        let inner = Rc::new(Inner {
            base_url: options.base_url,
            headers: options.headers,
            transport: options.transport,
            events: std::cell::OnceCell::new(),
        });
        let weak = Rc::downgrade(&inner);
        let connect: crate::shared_events::Connect = Rc::new(move |signal, on_activity| {
            let options = RequestOptions { signal: Some(signal), headers: Vec::new(), on_activity: Some(on_activity) };
            let source: Box<dyn crate::shared_events::Source> = match weak.upgrade() {
                Some(inner) => Box::new(StreamSource(
                    Some(Client { inner, media_refs: false }.stream(&ops::EVENT_SUBSCRIBE, &Json::Null, options)),
                )),
                None => Box::new(StreamSource::closed()),
            };
            source
        });
        let _ = inner.events.set(SharedEvents::make(connect));
        Self { inner, media_refs: false }
    }

    /// `withMediaRefs(api)` (promise/media.ts): message and inbox reads opt into `media:<hash>` refs.
    pub fn with_media_refs(&self) -> Self {
        Self { inner: self.inner.clone(), media_refs: true }
    }

    pub fn base_url(&self) -> &str {
        &self.inner.base_url
    }

    pub fn headers(&self) -> &[(String, String)] {
        &self.inner.headers
    }

    /// `api.event`: the shared event stream (`SharedEvents` over `event.subscribe`).
    pub fn event(&self) -> &SharedEvents {
        self.inner.events.get().expect("events are set at creation")
    }

    /// Whether two handles are the same client (the same `OpenCode.make` result).
    pub fn same(&self, other: &Client) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }

    /// The operations by the TS client's namespaces.
    pub fn api(&self) -> Api<'_> {
        Api(self)
    }

    /// The operation of a dotted TS name (`session.revert.stage`).
    pub fn operation(name: &str) -> Option<&'static Operation> {
        ops::OPERATIONS.iter().copied().find(|op| op.name == name)
    }

    /// Calls an operation by its dotted TS name; an unknown name panics.
    pub fn call_named(&self, name: &str, input: &Json, options: RequestOptions) -> Call {
        let op = Self::operation(name).unwrap_or_else(|| panic!("no operation {name}"));
        self.call(op, input, options)
    }

    /// `request(descriptor, requestOptions)`: starts the request now.
    pub fn call(&self, op: &'static Operation, input: &Json, mut options: RequestOptions) -> Call {
        if self.media_refs && MEDIA_REF_OPERATIONS.contains(&op.name) {
            options.headers.retain(|(name, _)| !name.eq_ignore_ascii_case("x-opencode-capabilities"));
            options.headers.push(("x-opencode-capabilities".into(), "experimental-media-uri".into()));
        }
        let started = self.execute(op, input, &options);
        Box::pin(async move {
            let response = started.await?;
            if response.status != op.success {
                return Err(response_error(response, op).await);
            }
            if op.binary {
                return read_all(response.body).await.map(Value::Bytes).map_err(Error::Other);
            }
            if op.empty {
                drop(response);
                return Ok(Value::Undefined);
            }
            let value = json(response).await?;
            if !op.data {
                return Ok(Value::Json(value));
            }
            Ok(match value {
                Json::Object(mut object) => match object.shift_remove("data") {
                    Some(data) => Value::Json(data),
                    None => Value::Undefined,
                },
                Json::Null => {
                    return Err(Error::Other(Reason {
                        name: "TypeError".into(),
                        message: "null is not an object (evaluating 'value.data')".into(),
                    }));
                }
                _ => Value::Undefined,
            })
        })
    }

    /// `sse(descriptor, requestOptions)`: the request starts with the first `next()`.
    pub fn stream(&self, op: &'static Operation, input: &Json, options: RequestOptions) -> EventStream {
        EventStream { client: self.clone(), op, input: Some(input.clone()), options, state: StreamState::Idle }
    }

    /// `prepare(descriptor, requestOptions)`: the URL, headers and body of a request.
    pub fn prepare(&self, op: &Operation, input: &Json, options: &RequestOptions) -> Result<Request, Reason> {
        let mut base = url::Url::parse(&self.inner.base_url)
            .map_err(|_| Reason { name: "TypeError".into(), message: format!("\"{}\" cannot be parsed as a URL.", self.inner.base_url) })?;
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let path = path_of(op, input).unwrap_or_default();
        let mut url = base
            .join(path.strip_prefix('/').unwrap_or(&path))
            .map_err(|error| Reason { name: "TypeError".into(), message: error.to_string() })?;
        let mut pairs = Vec::new();
        for (key, input_key) in op.query {
            if let Some(value) = input_field(input, input_key) {
                append_query(&mut pairs, key, value);
            }
        }
        if !pairs.is_empty() {
            let mut query = url.query_pairs_mut();
            for (key, value) in &pairs {
                query.append_pair(key, value);
            }
        }
        let mut headers = Headers::default();
        for (name, value) in &self.inner.headers {
            headers.append(name, value)?;
        }
        for (name, input_key) in op.headers {
            match input_field(input, input_key) {
                None | Some(Json::Null) => {}
                Some(value) => headers.set(name, &js::string(value))?,
            }
        }
        // `new Headers(requestOptions.headers)` first, so a repeated name joins its values before replacing.
        let mut extra = Headers::default();
        for (name, value) in &options.headers {
            extra.append(name, value)?;
        }
        for (name, value) in extra.0 {
            headers.set(&name, &value)?;
        }
        let (body, binary) = match op.body {
            Body::None => (None, false),
            Body::Fields(fields) => {
                let mut object = opencode_protocol::wire::Object::new();
                for (key, input_key) in fields {
                    if let Some(value) = input_field(input, input_key) {
                        object.insert((*key).to_owned(), value.clone());
                    }
                }
                (Some(stringify(&Json::Object(object)).into_bytes()), false)
            }
            Body::Input(key) => (input_field(input, key).map(|value| stringify(value).into_bytes()), false),
            Body::Binary(key) => (
                input_field(input, key).map(|value| match value {
                    Json::String(text) => text.clone().into_bytes(),
                    other => stringify(other).into_bytes(),
                }),
                true,
            ),
        };
        if body.is_some() && !headers.has("content-type") {
            headers.set("content-type", if binary { "application/octet-stream" } else { "application/json" })?;
        }
        Ok(Request { method: op.method.into(), url: url.to_string(), headers: headers.0, body })
    }

    /// `execute(descriptor, requestOptions)`: `fetch`, with transport failures as `ClientError("Transport")`.
    fn execute(&self, op: &Operation, input: &Json, options: &RequestOptions) -> LocalBoxFuture<'static, Result<Response, Error>> {
        // The path is built before `request()` runs, so its failure is not a `ClientError`.
        if let Err(error) = path_of(op, input) {
            return Box::pin(async move { Err(Error::Other(error)) });
        }
        let request = match self.prepare(op, input, options) {
            Ok(request) => request,
            Err(cause) => return Box::pin(async move { Err(Error::client(ClientErrorReason::Transport, Some(cause))) }),
        };
        let fetched = fetch(&*self.inner.transport, request, options.signal.as_ref());
        let signal = options.signal.clone();
        Box::pin(async move {
            let mut response = fetched.await.map_err(|cause| Error::client(ClientErrorReason::Transport, Some(cause)))?;
            if let Some(signal) = signal {
                response.body = Box::new(SignalBody { body: response.body, signal });
            }
            Ok(response)
        })
    }
}

/// `fetch(request, { signal })`: rejects with the signal's reason when it aborts first.
fn fetch(transport: &dyn Transport, request: Request, signal: Option<&AbortSignal>) -> LocalBoxFuture<'static, Result<Response, Reason>> {
    if let Some(reason) = signal.and_then(AbortSignal::reason) {
        return Box::pin(async move { Err(reason) });
    }
    let pending = transport.fetch(request);
    let Some(signal) = signal.cloned() else { return pending };
    Box::pin(async move {
        tokio::select! {
            biased;
            reason = signal.wait() => Err(reason),
            response = pending => response,
        }
    })
}

/// A body whose reads reject with the signal's reason once it aborts.
struct SignalBody {
    body: Box<dyn ResponseBody>,
    signal: AbortSignal,
}

impl ResponseBody for SignalBody {
    fn next(&mut self) -> LocalBoxFuture<'_, Option<Result<bytes::Bytes, Reason>>> {
        Box::pin(async move {
            if let Some(reason) = self.signal.reason() {
                return Some(Err(reason));
            }
            let signal = self.signal.clone();
            tokio::select! {
                biased;
                reason = signal.wait() => Some(Err(reason)),
                chunk = self.body.next() => chunk,
            }
        })
    }
}

/// A `Headers` object: names lowercased, `set` replacing every value of a name in place of the first.
#[derive(Default)]
struct Headers(Vec<(String, String)>);

impl Headers {
    fn normalize(value: &str) -> String {
        value.trim_matches([' ', '\t', '\n', '\r']).to_owned()
    }

    /// A header value must be a byte string without NUL or line breaks (Bun's `TypeError`).
    fn validate(name: &str, value: &str) -> Result<(), Reason> {
        if value.chars().any(|c| c as u32 > 0xFF || matches!(c, '\0' | '\r' | '\n')) {
            return Err(Reason { name: "TypeError".into(), message: format!("Header '{name}' has invalid value: '{value}'") });
        }
        Ok(())
    }

    fn append(&mut self, name: &str, value: &str) -> Result<(), Reason> {
        let name = name.to_ascii_lowercase();
        let value = Self::normalize(value);
        Self::validate(&name, &value)?;
        match self.0.iter_mut().find(|(n, _)| *n == name) {
            Some((_, existing)) => {
                existing.push_str(", ");
                existing.push_str(&value);
            }
            None => self.0.push((name, value)),
        }
        Ok(())
    }

    fn set(&mut self, name: &str, value: &str) -> Result<(), Reason> {
        let name = name.to_ascii_lowercase();
        let value = Self::normalize(value);
        Self::validate(&name, &value)?;
        match self.0.iter_mut().find(|(n, _)| *n == name) {
            Some((_, existing)) => *existing = value,
            None => self.0.push((name, value)),
        }
        Ok(())
    }

    fn has(&self, name: &str) -> bool {
        self.0.iter().any(|(n, _)| n == name)
    }
}

/// `input?.[key]`, absent when the input is not an object or lacks the key.
fn input_field<'a>(input: &'a Json, key: &str) -> Option<&'a Json> {
    input.as_object().and_then(|object| object.get(key))
}

/// `String(input.<key>)` in a path: a missing key is the text `undefined`.
fn input_string(input: &Json, key: &str) -> String {
    input_field(input, key).map(js::string).unwrap_or_else(|| "undefined".into())
}

/// The descriptor's path template filled in. `encodePath(value)` splits a string: any other value throws.
fn path_of(op: &Operation, input: &Json) -> Result<String, Reason> {
    let mut path = String::new();
    for segment in op.path {
        match segment {
            Segment::Literal(text) => path.push_str(text),
            Segment::Param(key) => path.push_str(&js::encode_uri_component(&input_string(input, key))),
            Segment::Path(key) => match input_field(input, key) {
                Some(Json::String(value)) => {
                    path.push_str(&value.split('/').map(js::encode_uri_component).collect::<Vec<_>>().join("/"))
                }
                other => {
                    let what = match other {
                        None => "undefined".to_owned(),
                        Some(Json::Null) => "null".to_owned(),
                        Some(_) => "value.split".to_owned(),
                    };
                    return Err(Reason { name: "TypeError".into(), message: format!("{what} is not an object (evaluating 'value.split')") });
                }
            },
        }
    }
    Ok(path)
}

/// `appendQuery`: objects become `key[child]`, arrays repeat the key, `null` is the text `null`.
fn append_query(pairs: &mut Vec<(String, String)>, key: &str, value: &Json) {
    match value {
        Json::Null => pairs.push((key.into(), "null".into())),
        Json::Array(items) => items.iter().for_each(|item| append_query(pairs, key, item)),
        Json::Object(object) => {
            for (child, item) in object {
                append_query(pairs, &format!("{key}[{child}]"), item);
            }
        }
        other => pairs.push((key.into(), js::string(other))),
    }
}

/// `JSON.stringify(value)`.
pub fn stringify(value: &Json) -> String {
    let mut out = Out::new();
    out.json(value);
    out.finish()
}

/// `isContentType(response, expected)`.
fn is_content_type(response: &Response, expected: &str) -> bool {
    response.header("content-type").is_some_and(|kind| {
        js::trim(kind.split(';').next().unwrap_or("")).to_lowercase() == expected
    })
}

/// `responseError(response, descriptor)`.
async fn response_error(response: Response, op: &Operation) -> Error {
    if op.declared.contains(&response.status) {
        let status = response.status;
        return match json(response).await {
            Ok(body) => Error::Declared { status, body },
            Err(error) => error,
        };
    }
    let status = response.status;
    drop(response);
    Error::Client {
        reason: ClientErrorReason::UnexpectedStatus,
        detail: Some(status.to_string()),
        status: Some(status),
        cause: None,
    }
}

/// `json(response)`.
async fn json(response: Response) -> Result<Json, Error> {
    let kind = response.header("content-type");
    if !is_content_type(&response, "application/json") && !kind.as_deref().is_some_and(|kind| kind.contains("+json")) {
        return Err(Error::detail(ClientErrorReason::UnsupportedContentType, kind));
    }
    let bytes = read_all(response.body).await.map_err(|cause| Error::client(ClientErrorReason::Transport, Some(cause)))?;
    let text = text(&bytes);
    if text.is_empty() {
        return Err(Error::client(ClientErrorReason::MalformedResponse, None));
    }
    opencode_protocol::wire::parse(&text).map_err(|error| {
        Error::client(ClientErrorReason::MalformedResponse, Some(Reason { name: "SyntaxError".into(), message: error.to_string() }))
    })
}

/// `response.text()`: UTF-8 with replacement, a leading byte order mark dropped.
fn text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

async fn read_all(mut body: Box<dyn ResponseBody>) -> Result<Vec<u8>, Reason> {
    let mut out = Vec::new();
    while let Some(chunk) = body.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

const MAX_SSE_EVENT_BYTES: usize = 16 * 1024 * 1024;

enum StreamState {
    Idle,
    Reading { body: Box<dyn ResponseBody>, decoder: js::TextDecoder, buffer: String, done: bool },
    Finished,
}

/// The server-sent events of a stream operation (`AsyncIterable` from `sse()`): one JSON value per `data` block.
/// Dropping it cancels the response body.
pub struct EventStream {
    client: Client,
    op: &'static Operation,
    input: Option<Json>,
    options: RequestOptions,
    state: StreamState,
}

impl EventStream {
    /// `iterator.next()`: the next event, `None` once the stream has ended, or the error that ended it.
    pub async fn next(&mut self) -> Option<Result<Json, Error>> {
        if let StreamState::Idle = self.state {
            let input = self.input.take().unwrap_or(Json::Null);
            match self.open(&input).await {
                Ok(body) => {
                    self.state =
                        StreamState::Reading { body, decoder: js::TextDecoder::default(), buffer: String::new(), done: false }
                }
                Err(error) => {
                    self.state = StreamState::Finished;
                    return Some(Err(error));
                }
            }
        }
        let result = self.read().await;
        if !matches!(result, Some(Ok(_))) {
            self.state = StreamState::Finished;
        }
        result
    }

    /// `iterator.return()`: cancels the body.
    pub fn close(&mut self) {
        self.state = StreamState::Finished;
    }

    async fn open(&self, input: &Json) -> Result<Box<dyn ResponseBody>, Error> {
        let response = self.client.execute(self.op, input, &self.options).await?;
        if response.status != self.op.success {
            return Err(response_error(response, self.op).await);
        }
        if !is_content_type(&response, "text/event-stream") {
            return Err(Error::detail(ClientErrorReason::UnsupportedContentType, response.header("content-type")));
        }
        Ok(response.body)
    }

    async fn read(&mut self) -> Option<Result<Json, Error>> {
        let StreamState::Reading { body, decoder, buffer, done } = &mut self.state else { return None };
        loop {
            if let Some(event) = take_event(buffer) {
                return Some(event);
            }
            if *done {
                return None;
            }
            let next = match body.next().await {
                Some(Ok(chunk)) => Some(chunk),
                Some(Err(cause)) => return Some(Err(Error::client(ClientErrorReason::Transport, Some(cause)))),
                None => None,
            };
            if next.is_some() {
                if let Some(on_activity) = &self.options.on_activity {
                    on_activity();
                }
            }
            *done = next.is_none();
            buffer.push_str(&decoder.decode(next.as_deref().unwrap_or(&[]), !*done));
            if buffer.len() > MAX_SSE_EVENT_BYTES && js::length(buffer) > MAX_SSE_EVENT_BYTES {
                return Some(Err(Error::client(ClientErrorReason::SseEventTooLarge, None)));
            }
            let trailing = !*done && buffer.ends_with('\r');
            if trailing {
                buffer.pop();
            }
            if buffer.contains('\r') {
                *buffer = buffer.replace("\r\n", "\n").replace('\r', "\n");
            }
            if trailing {
                buffer.push('\r');
            }
            if *done && !buffer.is_empty() {
                buffer.push_str("\n\n");
            }
        }
    }
}

/// The next `data` block of the buffer, removing it; blocks without data are skipped.
fn take_event(buffer: &mut String) -> Option<Result<Json, Error>> {
    while let Some(boundary) = buffer.find("\n\n") {
        let data = buffer[..boundary]
            .split('\n')
            .filter_map(|line| line.strip_prefix("data:").map(js::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        buffer.drain(..boundary + 2);
        if data.is_empty() {
            continue;
        }
        return Some(opencode_protocol::wire::parse(&data).map_err(|error| {
            Error::client(ClientErrorReason::MalformedResponse, Some(Reason { name: "SyntaxError".into(), message: error.to_string() }))
        }));
    }
    None
}

/// The shared stream's source: the event stream of `event.subscribe`, each event decoded.
struct StreamSource(Option<EventStream>);

impl StreamSource {
    fn closed() -> Self {
        Self(None)
    }
}

impl crate::shared_events::Source for StreamSource {
    fn next(&mut self) -> LocalBoxFuture<'_, Option<Result<crate::event::Event, Error>>> {
        Box::pin(async move {
            let stream = self.0.as_mut()?;
            stream.next().await.map(|item| item.map(crate::event::Event::new))
        })
    }
}
