//! The CLI's client of the server API (packages/client/src/promise/generated/client.ts): operations by name from
//! the generated route table, Basic credentials, declared error bodies thrown as their tagged errors, other
//! statuses as `ClientError`s, and the event stream as server-sent events. Transport failures carry the words
//! of Bun's `fetch`, which the TS messages quote.

use std::time::Duration;

use opencode_protocol::Json;
use opencode_protocol::routes::{BodyKind, ROUTES, Route};

use crate::llm::http::{self, FailureKind};

/// Where a server is and how to authenticate (`Endpoint`).
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub url: String,
    /// The `opencode` user's Basic password.
    pub password: Option<String>,
}

impl Endpoint {
    pub fn headers(&self) -> Vec<(String, String)> {
        use base64::Engine;
        match &self.password {
            Some(password) => vec![(
                "authorization".into(),
                format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))),
            )],
            None => vec![],
        }
    }
}

#[derive(Clone, Debug)]
pub enum ClientError {
    /// The request never got an answer; the cause's message.
    Transport(String),
    UnexpectedStatus(u16),
    UnsupportedContentType(Option<String>),
    MalformedResponse,
    /// A declared error body (`declared()`): an `Error` named after its `_tag`.
    Declared { status: u16, body: Json },
}

impl ClientError {
    pub fn name(&self) -> String {
        match self {
            ClientError::Declared { body, .. } => crate::json::get_str(body, "_tag").unwrap_or("Error").to_owned(),
            _ => "ClientError".into(),
        }
    }

    /// `errorMessage(error)`.
    pub fn message(&self) -> String {
        match self {
            ClientError::Transport(cause) => format!("Transport: {cause}"),
            ClientError::UnexpectedStatus(status) => format!("UnexpectedStatus: {status}"),
            ClientError::UnsupportedContentType(Some(kind)) => format!("UnsupportedContentType: {kind}"),
            ClientError::UnsupportedContentType(None) => "UnsupportedContentType".into(),
            ClientError::MalformedResponse => "MalformedResponse".into(),
            ClientError::Declared { body, .. } => crate::json::get_str(body, "message")
                .or_else(|| crate::json::get(body, "data").and_then(|d| crate::json::get_str(d, "message")))
                .unwrap_or("")
                .to_owned(),
        }
    }

    #[allow(dead_code)] // `run`, next
    pub fn tag(&self) -> Option<&str> {
        match self {
            ClientError::Declared { body, .. } => crate::json::get_str(body, "_tag"),
            _ => None,
        }
    }

    /// The transport cause's own message (`cause.cause`), for "Could not reach server".
    pub fn transport_cause(&self) -> Option<&str> {
        match self {
            ClientError::Transport(cause) => Some(cause),
            _ => None,
        }
    }
}

/// Bun's `fetch` error texts for a failure below HTTP.
pub fn fetch_message(failure: &http::Failure) -> String {
    match failure.kind {
        FailureKind::Refused => "Unable to connect. Is the computer able to access the url?".into(),
        FailureKind::Unresolved => "Was there a typo in the url or port?".into(),
        FailureKind::Closed => {
            "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()".into()
        }
        _ => failure.message.clone(),
    }
}

/// `encodeURIComponent`.
pub fn encode_component(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `URLSearchParams` serialization of one name or value.
pub fn form_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"*-._".contains(&byte) {
            out.push(byte as char);
        } else if byte == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
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
        Json::String(text) => pairs.push((key.into(), text.clone())),
        other => pairs.push((key.into(), crate::json::text(other))),
    }
}

pub fn query_string(query: &Json) -> String {
    let mut pairs = vec![];
    if let Json::Object(object) = query {
        for (key, value) in object {
            append_query(&mut pairs, key, value);
        }
    }
    pairs.iter().map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v))).collect::<Vec<_>>().join("&")
}

fn route(operation: &str) -> &'static Route {
    ROUTES.iter().find(|r| r.operation == operation).unwrap_or_else(|| panic!("no route {operation}"))
}

/// A plain HTTP answer.
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Answer {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// `response.statusText`.
    pub fn status_text(&self) -> &'static str {
        hyper::StatusCode::from_u16(self.status).ok().and_then(|s| s.canonical_reason()).unwrap_or("")
    }
}

fn is_json(answer: &Answer) -> bool {
    answer.header("content-type").is_some_and(|kind| {
        kind.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json") || kind.contains("+json")
    })
}

/// A raw request (`fetch`); `Err` carries the transport failure's Bun message.
pub async fn fetch(method: &str, url: &str, headers: &[(String, String)], body: Option<String>) -> Result<Answer, String> {
    let response = http::send(method, url, headers, body.unwrap_or_default()).await.map_err(|f| fetch_message(&f))?;
    let status = response.status;
    let headers = response.headers.clone();
    let body = response.text().await.map_err(|f| fetch_message(&f))?;
    Ok(Answer { status, headers, body })
}

#[derive(Clone)]
pub struct Client {
    pub endpoint: Endpoint,
}

impl Client {
    pub fn new(endpoint: &Endpoint) -> Self {
        Self { endpoint: endpoint.clone() }
    }

    fn url(&self, route: &Route, params: &[(&str, &str)], query: Option<&Json>) -> String {
        let mut path = String::new();
        for segment in route.path.split('/').skip(1) {
            path.push('/');
            match segment.strip_prefix(':') {
                Some(name) => {
                    let value = params.iter().find(|(n, _)| *n == name).map(|(_, v)| *v).unwrap_or("");
                    path.push_str(&encode_component(value));
                }
                None => path.push_str(segment),
            }
        }
        let mut url = format!("{}{path}", self.endpoint.url.trim_end_matches('/'));
        if let Some(query) = query {
            let text = query_string(query);
            if !text.is_empty() {
                url.push('?');
                url.push_str(&text);
            }
        }
        url
    }

    /// Calls an operation: path parameters, a query object and a JSON payload. Answers the success body (JSON, or
    /// `null` when the endpoint answers no content).
    pub async fn call(&self, operation: &str, params: &[(&str, &str)], query: Option<Json>, body: Option<Json>) -> Result<Json, ClientError> {
        self.call_with(operation, params, query, body, &[], None).await
    }

    pub async fn call_with(
        &self,
        operation: &str,
        params: &[(&str, &str)],
        query: Option<Json>,
        body: Option<Json>,
        extra: &[(String, String)],
        timeout: Option<Duration>,
    ) -> Result<Json, ClientError> {
        let route = route(operation);
        let url = self.url(route, params, query.as_ref());
        let mut headers = self.endpoint.headers();
        for (name, value) in extra {
            headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
            headers.push((name.clone(), value.clone()));
        }
        if body.is_some() && !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("content-type")) {
            headers.push(("content-type".into(), "application/json".into()));
        }
        let request = fetch(route.method, &url, &headers, body.map(|b| crate::json::text(&b)));
        let answer = match timeout {
            Some(limit) => match tokio::time::timeout(limit, request).await {
                Ok(answer) => answer,
                Err(_) => Err("The operation timed out.".to_owned()),
            },
            None => request.await,
        }
        .map_err(ClientError::Transport)?;
        let success = route.success.first();
        if Some(answer.status) != success.map(|s| s.status) {
            if route.errors.iter().any(|e| e.status == answer.status) {
                return Err(ClientError::Declared { status: answer.status, body: json(&answer)? });
            }
            return Err(ClientError::UnexpectedStatus(answer.status));
        }
        if success.is_some_and(|s| s.kind == BodyKind::None) {
            return Ok(Json::Null);
        }
        json(&answer)
    }

    /// `event.subscribe`: the server-sent event stream.
    #[allow(dead_code)] // `run`, next
    pub async fn events(&self) -> Result<Events, ClientError> {
        let url = format!("{}/api/event", self.endpoint.url.trim_end_matches('/'));
        let mut headers = self.endpoint.headers();
        headers.push(("accept".into(), "text/event-stream".into()));
        let response = http::send("GET", &url, &headers, String::new())
            .await
            .map_err(|f| ClientError::Transport(fetch_message(&f)))?;
        if response.status != 200 {
            return Err(ClientError::UnexpectedStatus(response.status));
        }
        Ok(Events { response, buffer: String::new(), done: false })
    }
}

fn json(answer: &Answer) -> Result<Json, ClientError> {
    if !is_json(answer) {
        return Err(ClientError::UnsupportedContentType(answer.header("content-type").map(String::from)));
    }
    if answer.body.is_empty() {
        return Err(ClientError::MalformedResponse);
    }
    crate::json::parse(&answer.body).ok_or(ClientError::MalformedResponse)
}

/// An open event stream (`sse()`): one JSON value per `data` block.
#[allow(dead_code)] // `run`, next
pub struct Events {
    response: http::Response,
    buffer: String,
    done: bool,
}

#[allow(dead_code)] // `run`, next
impl Events {
    /// The next event; `None` when the stream ends.
    pub async fn next(&mut self) -> Option<Result<Json, ClientError>> {
        loop {
            while let Some(boundary) = self.buffer.find("\n\n") {
                let block: String = self.buffer[..boundary].to_owned();
                self.buffer.drain(..boundary + 2);
                let data: Vec<&str> = block
                    .split('\n')
                    .filter_map(|line| line.strip_prefix("data:").map(|rest| rest.trim_start()))
                    .collect();
                let data = data.join("\n");
                if !data.is_empty() {
                    return Some(crate::json::parse(&data).ok_or(ClientError::MalformedResponse));
                }
            }
            if self.done {
                return None;
            }
            match self.response.body.next().await {
                Some(Ok(chunk)) => {
                    self.buffer.push_str(&String::from_utf8_lossy(&chunk));
                    self.buffer = self.buffer.replace("\r\n", "\n").replace('\r', "\n");
                }
                Some(Err(failure)) => {
                    self.done = true;
                    return Some(Err(ClientError::Transport(fetch_message(&failure))));
                }
                None => {
                    self.done = true;
                    if !self.buffer.is_empty() {
                        self.buffer.push_str("\n\n");
                    }
                }
            }
        }
    }
}
