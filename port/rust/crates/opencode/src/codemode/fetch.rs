//! Code Mode's `fetch` (packages/core/src/codemode/fetch.ts): a host global that sends an HTTP request for the
//! program under the webfetch tool's permission rules, follows redirects itself (each hop checked against the deny
//! rules, a browser's method and credential rules applied), and hands back a `Response`-like object.
//!
//! The request is what Bun's `fetch` sends for the same `init`: header names lowercased, a body converted as Bun
//! converts it (text, bytes, form data, an array's parts, anything else as its string), and the body's default
//! `content-type` when none is given. One 30 s timeout covers every hop and the body.
//!
//! TODO(port): Bun's wording for network failures (connection refused, DNS, TLS) is approximated.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Poll, Waker};
use std::time::Duration;

use opencode_codemode::{self as cm, HostError, HostFunction, HostReturn, HostValue, JsStr};
use opencode_protocol::Json;

use crate::json::Obj;
use crate::llm::http;
use crate::permission::{self, Ask};
use crate::tool;

const TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_REDIRECTS: usize = 20;
const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

/// The `fetch` extension of one `execute` call, acting for its session.
pub fn extension(context: tool::Context) -> cm::Extension {
    let context = Rc::new(context);
    let order = Rc::new(Order::default());
    let call = move |args: Vec<HostValue>| {
        let (context, order) = (context.clone(), order.clone());
        HostReturn::Async(Box::pin(async move { fetch(&context, &order, args).await }))
    };
    cm::Extension {
        name: "fetch".into(),
        globals: vec![("fetch".into(), function("fetch", 1, true, call))],
    }
}

/// The requests of one execution leave in the order they were made: each waits for the one before it to have been
/// written (as Bun's fetch, which queues them on one thread, sends them), then they run concurrently.
#[derive(Default)]
struct Order {
    next: Cell<u64>,
    serving: Cell<u64>,
    /// Tickets given up before their turn (a timed-out request), skipped when they come up.
    abandoned: RefCell<Vec<u64>>,
    waiting: RefCell<Vec<Waker>>,
}

/// A request's place in the order, given up once it has been written (or has failed, or was dropped).
struct Turn<'a> {
    order: &'a Order,
    ticket: u64,
}

impl Order {
    fn turn(&self) -> Turn<'_> {
        let ticket = self.next.get();
        self.next.set(ticket + 1);
        Turn { order: self, ticket }
    }

    fn advance(&self) {
        let mut serving = self.serving.get() + 1;
        let mut abandoned = self.abandoned.borrow_mut();
        while let Some(index) = abandoned.iter().position(|ticket| *ticket == serving) {
            abandoned.remove(index);
            serving += 1;
        }
        self.serving.set(serving);
        drop(abandoned);
        for waker in self.waiting.take() {
            waker.wake();
        }
    }
}

impl Turn<'_> {
    async fn wait(&self) {
        std::future::poll_fn(|cx| {
            if self.order.serving.get() == self.ticket {
                return Poll::Ready(());
            }
            self.order.waiting.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        if self.order.serving.get() == self.ticket {
            self.order.advance();
        } else {
            self.order.abandoned.borrow_mut().push(self.ticket);
        }
    }
}

fn function(name: &str, length: u32, asynchronous: bool, call: impl Fn(Vec<HostValue>) -> HostReturn + 'static) -> HostValue {
    HostValue::Function(HostFunction { name: name.into(), length, asynchronous, call: Rc::new(call) })
}

fn error(name: &str, message: impl Into<String>) -> HostValue {
    HostValue::Error(HostError { name: name.into(), message: JsStr::from(message.into()), cause: None, fields: Vec::new() })
}

fn text(value: impl Into<String>) -> HostValue {
    HostValue::Str(JsStr::from(value.into()))
}

/// `CodeModeFetch.display`: a fetch call's method and URL, as its progress row and permission request show it.
pub fn display(args: &[HostValue]) -> Json {
    let method = match args.get(1) {
        Some(HostValue::Object(fields)) => match field(fields, "method") {
            Some(HostValue::Str(method)) => method.to_rust_lossy().to_uppercase(),
            _ => "GET".into(),
        },
        _ => "GET".into(),
    };
    let mut o = Obj::new();
    o.str("method", &method).str("url", &js_string(args.first().unwrap_or(&HostValue::Undefined)));
    o.done()
}

fn field<'a>(fields: &'a [(JsStr, HostValue)], name: &str) -> Option<&'a HostValue> {
    fields.iter().rev().find(|(key, _)| key.eq_str(name)).map(|(_, value)| value)
}

/// JS `String(value)` of a value that crossed to the host.
pub fn js_string(value: &HostValue) -> String {
    match value {
        HostValue::Undefined => "undefined".into(),
        HostValue::Null => "null".into(),
        HostValue::Bool(b) => b.to_string(),
        HostValue::Num(n) => cm::numconv::to_string(*n),
        HostValue::Str(s) => s.to_rust_lossy(),
        HostValue::Array(items) => items
            .iter()
            .map(|item| match item {
                None | Some(HostValue::Undefined | HostValue::Null) => String::new(),
                Some(item) => js_string(item),
            })
            .collect::<Vec<_>>()
            .join(","),
        HostValue::Object(_) => "[object Object]".into(),
        HostValue::Date(t) => {
            if t.is_finite() {
                format!("{} {}", cm::jsdate::to_date_string(*t), cm::jsdate::to_time_string(*t))
            } else {
                "Invalid Date".into()
            }
        }
        HostValue::RegExp { source, flags } => format!("/{}/{flags}", source.to_rust_lossy()),
        HostValue::Bytes(bytes) => bytes.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(","),
        HostValue::Url(href) => href.clone(),
        HostValue::SearchParams(pairs) => form(pairs),
        HostValue::Headers(_) => "[object Headers]".into(),
        HostValue::Map(_) => "[object Map]".into(),
        HostValue::Set(_) => "[object Set]".into(),
        HostValue::Error(error) => {
            let message = error.message.to_rust_lossy();
            match (error.name.as_str(), message.as_str()) {
                ("", message) => message.to_owned(),
                (name, "") => name.to_owned(),
                (name, message) => format!("{name}: {message}"),
            }
        }
        HostValue::Function(function) => format!("function {}() {{\n    [native code]\n}}", function.name),
    }
}

/// `application/x-www-form-urlencoded` serialization of `URLSearchParams`.
fn form(pairs: &[(JsStr, JsStr)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in pairs {
        serializer.append_pair(&name.to_rust_lossy(), &value.to_rust_lossy());
    }
    serializer.finish()
}

/// What a request carries: its method as given (`init.method`), headers (`new Headers(init.headers)`) and body.
#[derive(Clone)]
struct Request {
    method: Option<String>,
    headers: Vec<(String, String)>,
    body: Option<Body>,
}

#[derive(Clone)]
struct Body {
    bytes: Vec<u8>,
    /// The type Bun gives the body when the headers name none.
    content_type: Option<&'static str>,
}

/// Bun's `init` conversion; a `TypeError` for headers it cannot take.
fn request(init: &HostValue) -> Result<Request, HostValue> {
    let HostValue::Object(fields) = init else { return Ok(Request { method: None, headers: Vec::new(), body: None }) };
    let method = match field(fields, "method") {
        None | Some(HostValue::Undefined) => None,
        Some(value) => Some(js_string(value)),
    };
    let headers = match field(fields, "headers") {
        None | Some(HostValue::Undefined) => Vec::new(),
        Some(value) => headers(value)?,
    };
    let body = match field(fields, "body") {
        None | Some(HostValue::Undefined | HostValue::Null) => None,
        Some(value) => Some(body(value)),
    };
    Ok(Request { method, headers, body })
}

/// `new Headers(init)`: names lowercased, values trimmed, sorted and combined as a `Headers` lists them.
fn headers(value: &HostValue) -> Result<Vec<(String, String)>, HostValue> {
    let pair = |item: &HostValue| -> Result<(String, String), HostValue> {
        match item {
            HostValue::Array(parts) if parts.len() == 2 => {
                let part = |index: usize| js_string(parts[index].as_ref().unwrap_or(&HostValue::Undefined));
                Ok((part(0), part(1)))
            }
            _ => Err(error("TypeError", "Expected a sequence of [name, value] pairs")),
        }
    };
    let pairs: Vec<(String, String)> = match value {
        HostValue::Headers(pairs) => pairs.clone(),
        HostValue::Object(fields) => fields.iter().map(|(name, value)| (name.to_rust_lossy(), js_string(value))).collect(),
        HostValue::Array(items) => {
            items.iter().map(|item| pair(item.as_ref().unwrap_or(&HostValue::Undefined))).collect::<Result<_, _>>()?
        }
        HostValue::Map(entries) => entries.iter().map(|(name, value)| (js_string(name), js_string(value))).collect(),
        HostValue::Set(items) => items.iter().map(pair).collect::<Result<_, _>>()?,
        _ => Vec::new(),
    };
    Ok(combine(pairs.into_iter().map(|(name, value)| (name.to_ascii_lowercase(), crate::text::trim(&value).to_owned()))))
}

/// A header list sorted by name with repeated names combined (`set-cookie` lines kept apart).
fn combine(pairs: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, value) in pairs {
        match out.iter_mut().find(|(existing, _)| *existing == name && name != "set-cookie") {
            Some((_, existing)) => {
                existing.push_str(", ");
                existing.push_str(&value);
            }
            None => out.push((name, value)),
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// A body as Bun sends it.
fn body(value: &HostValue) -> Body {
    match value {
        HostValue::Bytes(bytes) => Body { bytes: bytes.clone(), content_type: None },
        HostValue::SearchParams(pairs) => {
            Body { bytes: form(pairs).into_bytes(), content_type: Some("application/x-www-form-urlencoded;charset=UTF-8") }
        }
        // Blob parts.
        HostValue::Array(items) => {
            let mut bytes = Vec::new();
            for item in items.iter() {
                match item {
                    Some(HostValue::Bytes(part)) => bytes.extend_from_slice(part),
                    Some(item) => bytes.extend_from_slice(js_string(item).as_bytes()),
                    None => bytes.extend_from_slice(b"undefined"),
                }
            }
            Body { bytes, content_type: None }
        }
        HostValue::Str(text) => Body { bytes: text.to_rust_lossy().into_bytes(), content_type: Some("text/plain;charset=UTF-8") },
        // Anything else is sent as its string, with no type.
        other => Body { bytes: js_string(other).into_bytes(), content_type: None },
    }
}

/// The Fetch standard's method normalization.
fn normalize_method(method: &str) -> String {
    let upper = method.to_ascii_uppercase();
    if ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"].contains(&upper.as_str()) { upper } else { method.to_owned() }
}

/// `redirected`: the request a browser sends after a redirect. 303 (and 301/302 after a POST) becomes a GET without a
/// body; credentials are not sent to another origin.
fn redirected(request: &Request, status: u16, from: &url::Url, to: &url::Url) -> Request {
    let method = request.method.as_deref().map(str::to_uppercase).unwrap_or_else(|| "GET".into());
    let to_get = if status == 303 { method != "HEAD" } else { (status == 301 || status == 302) && method == "POST" };
    let mut headers = request.headers.clone();
    if to_get {
        let content = ["content-type", "content-length", "content-encoding", "content-language", "content-location"];
        headers.retain(|(name, _)| !content.contains(&name.as_str()));
    }
    if from.origin() != to.origin() {
        headers.retain(|(name, _)| !["authorization", "cookie", "proxy-authorization"].contains(&name.as_str()));
    }
    Request {
        method: if to_get { Some("GET".into()) } else { request.method.clone() },
        headers,
        body: if to_get { None } else { request.body.clone() },
    }
}

struct Fetched {
    response: http::Response,
    reason: String,
    url: String,
    redirects: usize,
}

async fn fetch(context: &tool::Context, order: &Order, args: Vec<HostValue>) -> Result<HostValue, HostValue> {
    let input = args.first().cloned().unwrap_or(HostValue::Undefined);
    let init = args.get(1).cloned().unwrap_or(HostValue::Undefined);
    // Rules match the URL as written; the fragment is never sent and would keep a rule for the exact URL from matching.
    let written = js_string(&input);
    let url = written.split('#').next().unwrap_or_default().to_owned();
    let parsed = url::Url::parse(&url).ok();
    if parsed.as_ref().is_some_and(|u| u.scheme() == "file") {
        return Err(error("Error", "fetch cannot be used for filesystem access."));
    }
    // Rules match the URL, so `https://user@evil.com` would slip past a rule for `https://evil.com/*`.
    if parsed.as_ref().is_some_and(|u| !u.username().is_empty() || u.password().is_some()) {
        return Err(error("TypeError", "fetch cannot be used with credentials in the URL."));
    }
    let requester = context.requester();
    // The webfetch tool's rules, so a URL denied there is denied here.
    let ask = Ask {
        action: "webfetch".into(),
        resources: vec![url.clone()],
        save: Some(vec!["*".into()]),
        metadata: Some(display(&[input.clone(), init.clone()])),
    };
    match context.location.permission.assert(&context.app, &context.location, &requester, &ask).await {
        Ok(()) => {}
        // The feedback is the user talking to the model, so it reaches the program as the error's message.
        Err(permission::Error::Corrected(feedback)) => {
            return Err(error("Error", format!("The user rejected fetching {url}: {feedback}")));
        }
        // A decline is a defect in TS; it crosses as an error without a message and the turn goes on (TODO(port): TS
        // bug, matched; see `codemode::failure_message`).
        Err(permission::Error::Declined) => return Err(error("Error", "")),
        Err(other) => return Err(error("Error", other.message())),
    }
    let request = request(&init)?;
    let run = async {
        let fetched = follow(context, order, &requester, &init, &url, request).await?;
        read(fetched).await
    };
    match tokio::time::timeout(TIMEOUT, run).await {
        Ok(result) => result,
        Err(_) => Err(error("TimeoutError", "The operation timed out.")),
    }
}

/// Sends the request and follows redirects, each new URL meeting the same deny rules as the webfetch tool's hops.
async fn follow(
    context: &tool::Context,
    order: &Order,
    requester: &permission::Requester,
    init: &HostValue,
    url: &str,
    mut request: Request,
) -> Result<Fetched, HostValue> {
    let mut target = url.to_owned();
    let mut redirects = 0;
    loop {
        let (response, reason) = send(order, &target, &request).await?;
        let location = response.headers.iter().find(|(name, _)| name == "location").map(|(_, value)| value.clone());
        let Some(location) = location.filter(|_| REDIRECT_STATUSES.contains(&response.status)) else {
            return Ok(Fetched { response, reason, url: target, redirects });
        };
        if redirects >= MAX_REDIRECTS {
            return Err(error("Error", format!("Too many redirects (more than {MAX_REDIRECTS})")));
        }
        let from = url::Url::parse(&target).map_err(|e| error("TypeError", e.to_string()))?;
        let mut next = from.join(&location).map_err(|e| error("TypeError", e.to_string()))?;
        if next.scheme() != "http" && next.scheme() != "https" {
            return Err(error("Error", format!("fetch cannot follow a redirect to {}: URLs.", next.scheme())));
        }
        if !next.username().is_empty() || next.password().is_some() {
            return Err(error("Error", "fetch cannot follow a redirect with credentials in the URL."));
        }
        next.set_fragment(None);
        let mut metadata = display(&[text(next.as_str()), init.clone()]);
        crate::json::set(&mut metadata, "redirectedFrom", Json::String(url.to_owned()));
        let ask = Ask { action: "webfetch".into(), resources: vec![next.to_string()], save: None, metadata: Some(metadata) };
        if let Err(denied) = context.location.permission.assert_not_denied(&context.app, &context.location, requester, &ask) {
            return Err(error("Error", denied.message()));
        }
        request = redirected(&request, response.status, &from, &next);
        drop(response);
        target = next.to_string();
        redirects += 1;
    }
}

/// One hop, without following redirects.
async fn send(order: &Order, target: &str, request: &Request) -> Result<(http::Response, String), HostValue> {
    let url = url::Url::parse(target).map_err(|_| error("TypeError", format!("fetch() URL is invalid: {target}")))?;
    let method = normalize_method(request.method.as_deref().unwrap_or("GET"));
    let mut headers = request.headers.clone();
    let body = match &request.body {
        Some(body) if method != "GET" && method != "HEAD" => {
            if let Some(content_type) = body.content_type.filter(|_| !headers.iter().any(|(name, _)| name == "content-type")) {
                headers.push(("content-type".into(), content_type.into()));
            }
            Some(body.bytes.clone())
        }
        _ => None,
    };
    let turn = order.turn();
    turn.wait().await;
    let mut turn = Some(turn);
    let (written, mut sent) = tokio::sync::oneshot::channel();
    let mut response = std::pin::pin!(http::request(&method, url.as_str(), &headers, body, Some(written)));
    let response = std::future::poll_fn(|cx| {
        if turn.is_some() && Pin::new(&mut sent).poll(cx).is_ready() {
            turn = None;
        }
        response.as_mut().poll(cx)
    })
    .await;
    drop(turn);
    response.map_err(|failure| {
        let message = match failure.kind {
            http::FailureKind::Refused | http::FailureKind::Unresolved => {
                "Unable to connect. Is the computer able to access the url?".to_owned()
            }
            _ => failure.message,
        };
        error("Error", message)
    })
}

/// `read`: the body read whole, and the `Response`-like object the program gets.
async fn read(fetched: Fetched) -> Result<HostValue, HostValue> {
    let Fetched { response, reason, url, redirects } = fetched;
    let status = response.status;
    let headers = combine(response.headers.iter().map(|(name, value)| (name.clone(), value.clone())));
    let mut body = response.body;
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|failure| error("Error", failure.message))?;
        bytes.extend_from_slice(&chunk);
    }
    let headers = Rc::new(headers);
    let bytes = Rc::new(bytes);
    let decoded = {
        let bytes = bytes.clone();
        move || {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            text.strip_prefix('\u{FEFF}').map(str::to_owned).unwrap_or(text)
        }
    };
    let (get_headers, has_headers, entry_headers) = (headers.clone(), headers.clone(), headers.clone());
    let header = |headers: &[(String, String)], args: &[HostValue]| -> Option<String> {
        let name = js_string(args.first().unwrap_or(&HostValue::Undefined)).to_lowercase();
        headers.iter().rev().find(|(key, _)| *key == name).map(|(_, value)| value.clone())
    };
    let get = function("get", 1, false, move |args| {
        HostReturn::Value(Ok(header(&get_headers, &args).map(text).unwrap_or(HostValue::Null)))
    });
    let has = function("has", 1, false, move |args| HostReturn::Value(Ok(HostValue::Bool(header(&has_headers, &args).is_some()))));
    let entries = function("entries", 0, false, move |_| {
        let mut seen: Vec<&str> = Vec::new();
        let mut pairs = Vec::new();
        // `Object.entries` of `Object.fromEntries(headers)`: one entry per name, the last value.
        for (name, _) in entry_headers.iter() {
            if seen.contains(&name.as_str()) {
                continue;
            }
            seen.push(name);
            let value = entry_headers.iter().rev().find(|(key, _)| key == name).map(|(_, value)| value.clone()).unwrap_or_default();
            pairs.push(Some(HostValue::Array(vec![Some(text(name.as_str())), Some(text(value))])));
        }
        HostReturn::Value(Ok(HostValue::Array(pairs)))
    });
    let text_decoded = decoded.clone();
    let read_text = function("text", 0, true, move |_| {
        let value = text(text_decoded());
        HostReturn::Async(Box::pin(async move { Ok(value) }))
    });
    let read_json = function("json", 0, true, move |_| {
        let units: Vec<u16> = decoded().encode_utf16().collect();
        let parsed = match cm::json::parse(&units) {
            Ok(value) => Ok(json_to_host(&value)),
            Err(message) => Err(error("SyntaxError", message)),
        };
        HostReturn::Async(Box::pin(async move { parsed }))
    });
    let read_bytes = function("bytes", 0, true, move |_| {
        let value = HostValue::Bytes((*bytes).clone());
        HostReturn::Async(Box::pin(async move { Ok(value) }))
    });
    let key = JsStr::from;
    Ok(HostValue::Object(vec![
        (key("url"), text(url)),
        (key("status"), HostValue::Num(status as f64)),
        (key("statusText"), text(reason)),
        (key("ok"), HostValue::Bool((200..300).contains(&status))),
        (key("redirected"), HostValue::Bool(redirects > 0)),
        (key("headers"), HostValue::Object(vec![(key("get"), get), (key("has"), has), (key("entries"), entries)])),
        (key("text"), read_text),
        (key("json"), read_json),
        (key("bytes"), read_bytes),
    ]))
}

fn json_to_host(value: &cm::Json) -> HostValue {
    match value {
        cm::Json::Null => HostValue::Null,
        cm::Json::Bool(b) => HostValue::Bool(*b),
        cm::Json::Num(n) => HostValue::Num(*n),
        cm::Json::Str(s) => HostValue::Str(s.clone()),
        cm::Json::Arr(items) => HostValue::Array(items.iter().map(|item| Some(json_to_host(item))).collect()),
        cm::Json::Obj(fields) => HostValue::Object(fields.iter().map(|(k, v)| (k.clone(), json_to_host(v))).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_and_bodies() {
        assert_eq!(js_string(&HostValue::Date(f64::NAN)), "Invalid Date");
        assert_eq!(js_string(&HostValue::RegExp { source: JsStr::from("re"), flags: "g".into() }), "/re/g");
        let array = HostValue::Array(vec![Some(HostValue::Num(1.0)), Some(HostValue::Num(2.0))]);
        assert_eq!(js_string(&array), "1,2");
        assert_eq!(body(&array).bytes, b"12");
        let params = HostValue::SearchParams(vec![(JsStr::from("a"), JsStr::from("1")), (JsStr::from("b"), JsStr::from("two words"))]);
        assert_eq!(body(&params).bytes, b"a=1&b=two+words");
        assert_eq!(normalize_method("post"), "POST");
        assert_eq!(normalize_method("patch"), "patch");
    }

    #[test]
    fn redirect_requests() {
        let request = Request {
            method: Some("post".into()),
            headers: vec![("authorization".into(), "x".into()), ("content-type".into(), "text/plain".into())],
            body: Some(Body { bytes: b"payload".to_vec(), content_type: None }),
        };
        let from = url::Url::parse("https://a.example.com/x").unwrap();
        let same = url::Url::parse("https://a.example.com/y").unwrap();
        let other = url::Url::parse("https://b.example.com/y").unwrap();
        let get = redirected(&request, 302, &from, &same);
        assert_eq!(get.method.as_deref(), Some("GET"));
        assert!(get.body.is_none());
        assert_eq!(get.headers, vec![("authorization".to_owned(), "x".to_owned())]);
        let kept = redirected(&request, 307, &from, &other);
        assert_eq!(kept.method.as_deref(), Some("post"));
        assert!(kept.body.is_some());
        assert_eq!(kept.headers, vec![("content-type".to_owned(), "text/plain".to_owned())]);
    }
}
