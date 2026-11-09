//! The driver's deterministic environment, as harness/ts/env.ts makes it for TS: a virtual clock (tokio's paused
//! clock behind `Date.now()`), a seeded random source, and a scripted transport whose requests the case answers.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::task::Waker;
use std::time::Duration;

use bytes::Bytes;
use opencode_protocol::Json;
use opencode_tui_sync::abort::Reason;
use opencode_tui_sync::runtime::{self, Deferred};
use opencode_tui_sync::transport::{LocalBoxFuture, Request, Response, ResponseBody, Transport};

pub const BASE_TIME: f64 = 1_700_000_000_000.0;

thread_local! {
    static START: Cell<Option<tokio::time::Instant>> = const { Cell::new(None) };
    static RANDOM: Cell<u32> = const { Cell::new(SEED) };
}

/// The TS driver's xorshift seed (harness/ts/env.ts).
const SEED: u32 = 0x9e3779b9;

pub fn install() {
    START.with(|s| s.set(Some(tokio::time::Instant::now())));
    runtime::set_clock(Some(Rc::new(|| {
        let start = START.with(|s| s.get()).expect("clock installed");
        BASE_TIME + (tokio::time::Instant::now() - start).as_millis() as f64
    })));
    runtime::set_random(Some(Rc::new(|bytes: &mut [u8]| {
        RANDOM.with(|state| {
            for byte in bytes.iter_mut() {
                let mut s = state.get();
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                state.set(s);
                *byte = (s & 0xff) as u8;
            }
        })
    })));
}

/// Restarts the clock at the base time and reseeds the random source.
pub fn reset() {
    START.with(|s| s.set(Some(tokio::time::Instant::now())));
    RANDOM.with(|state| state.set(SEED));
}

pub fn clock() -> f64 {
    runtime::now()
}

/// Polls a future once: its output if it is ready (a settled promise's `.then` has run).
pub async fn poll_once<F: std::future::Future + Unpin>(future: &mut F) -> Option<F::Output> {
    std::future::poll_fn(|cx| std::task::Poll::Ready(match std::pin::Pin::new(&mut *future).poll(cx) {
        std::task::Poll::Ready(output) => Some(output),
        std::task::Poll::Pending => None,
    }))
    .await
}

/// Lets every runnable task run.
pub async fn drain() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

/// Runs the timers due within `ms`, in deadline order, draining after each.
pub async fn advance(ms: f64) {
    let target = tokio::time::Instant::now() + Duration::from_millis(ms as u64);
    loop {
        let Some(next) = runtime::next_deadline() else { break };
        if next > target {
            break;
        }
        let now = tokio::time::Instant::now();
        if next > now {
            tokio::time::advance(next - now).await;
        }
        drain().await;
    }
    let now = tokio::time::Instant::now();
    if target > now {
        tokio::time::advance(target - now).await;
    }
    drain().await;
}

// ---- scripted transport ----------------------------------------------------------------------------------

pub struct Body {
    chunks: VecDeque<Bytes>,
    /// `Some(None)`: done; `Some(Some(error))`: failed.
    end: Option<Option<Reason>>,
    waker: Option<Waker>,
}

pub struct Pending {
    pub recorded: Request,
    head: Deferred<Result<(u16, Vec<(String, String)>), Reason>>,
    pub settled: Cell<bool>,
    pub closed: Cell<bool>,
    pub finished: Cell<bool>,
    body: RefCell<Body>,
}

#[derive(Default)]
pub struct Script {
    pub requests: RefCell<Vec<Rc<Pending>>>,
}

pub fn chunk(text: &str) -> Bytes {
    match text.strip_prefix("hex:") {
        Some(hex) => Bytes::from(
            (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0)).collect::<Vec<u8>>(),
        ),
        None => Bytes::from(text.as_bytes().to_vec()),
    }
}

pub fn reason(value: &Json) -> Reason {
    Reason { name: get_str(value, "name").unwrap_or("Error").into(), message: get_str(value, "message").unwrap_or("").into() }
}

impl Script {
    pub fn reset(&self) {
        self.requests.borrow_mut().clear();
    }

    pub fn get(&self, index: usize) -> Option<Rc<Pending>> {
        self.requests.borrow().get(index).cloned()
    }

    /// `respond(index, spec)`: `{ status, headers, chunks, end, error }`.
    pub fn respond(&self, index: usize, spec: &Json) {
        let Some(pending) = self.get(index) else { return };
        if pending.settled.get() {
            return;
        }
        pending.settled.set(true);
        {
            let mut body = pending.body.borrow_mut();
            for text in array(spec, "chunks") {
                if let Json::String(text) = text {
                    body.chunks.push_back(chunk(text));
                }
            }
            match get_str(spec, "end").unwrap_or("done") {
                "done" => body.end = Some(None),
                "error" => {
                    body.end = Some(Some(get(spec, "error").map(reason).unwrap_or(Reason {
                        name: "TypeError".into(),
                        message: "failed".into(),
                    })))
                }
                _ => {}
            }
        }
        let headers = array(spec, "headers")
            .iter()
            .filter_map(|pair| match pair {
                Json::Array(kv) => match (kv.first(), kv.get(1)) {
                    (Some(Json::String(k)), Some(Json::String(v))) => Some((k.to_ascii_lowercase(), v.clone())),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let status = get(spec, "status").and_then(num).unwrap_or(200.0) as u16;
        pending.head.resolve(Ok((status, headers)));
    }

    pub fn push(&self, index: usize, text: &str) {
        let Some(pending) = self.get(index) else { return };
        let mut body = pending.body.borrow_mut();
        if body.end.is_some() {
            return;
        }
        body.chunks.push_back(chunk(text));
        if let Some(waker) = body.waker.take() {
            waker.wake();
        }
    }

    pub fn end(&self, index: usize, error: Option<Reason>) {
        let Some(pending) = self.get(index) else { return };
        let mut body = pending.body.borrow_mut();
        if body.end.is_some() {
            return;
        }
        body.end = Some(error);
        if let Some(waker) = body.waker.take() {
            waker.wake();
        }
    }

    pub fn fail(&self, index: usize, error: Reason) {
        let Some(pending) = self.get(index) else { return };
        if pending.settled.get() {
            return;
        }
        pending.settled.set(true);
        pending.head.resolve(Err(error));
    }
}

pub struct ScriptTransport(pub Rc<Script>);

impl Transport for ScriptTransport {
    fn fetch(&self, request: Request) -> LocalBoxFuture<'static, Result<Response, Reason>> {
        let pending = Rc::new(Pending {
            recorded: request,
            head: Deferred::new(),
            settled: Cell::new(false),
            closed: Cell::new(false),
            finished: Cell::new(false),
            body: RefCell::new(Body { chunks: VecDeque::new(), end: None, waker: None }),
        });
        self.0.requests.borrow_mut().push(pending.clone());
        let guard = PendingGuard(Some(pending.clone()));
        Box::pin(async move {
            let head = pending.head.wait().await;
            let mut guard = guard;
            guard.0 = None;
            let (status, headers) = head?;
            Ok(Response { status, headers, body: Box::new(ScriptBody(pending)) })
        })
    }
}

/// Marks a request closed when its pending fetch is dropped (aborted) before the response, and settled: as the TS
/// script's abort listener does, so no later step answers it.
struct PendingGuard(Option<Rc<Pending>>);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Some(pending) = self.0.take() {
            pending.closed.set(true);
            pending.settled.set(true);
        }
    }
}

struct ScriptBody(Rc<Pending>);

impl ResponseBody for ScriptBody {
    fn next(&mut self) -> LocalBoxFuture<'_, Option<Result<Bytes, Reason>>> {
        Box::pin(std::future::poll_fn(move |cx| {
            let mut body = self.0.body.borrow_mut();
            if let Some(chunk) = body.chunks.pop_front() {
                return std::task::Poll::Ready(Some(Ok(chunk)));
            }
            match &body.end {
                Some(None) => {
                    self.0.finished.set(true);
                    std::task::Poll::Ready(None)
                }
                Some(Some(error)) => {
                    self.0.finished.set(true);
                    std::task::Poll::Ready(Some(Err(error.clone())))
                }
                None => {
                    body.waker = Some(cx.waker().clone());
                    std::task::Poll::Pending
                }
            }
        }))
    }
}

impl Drop for ScriptBody {
    /// A body dropped before it was read to its end was cancelled.
    fn drop(&mut self) {
        if !self.0.finished.get() {
            self.0.finished.set(true);
            // A cut counts while the server is still sending.
            let body = self.0.body.borrow();
            self.0.closed.set(!body.chunks.is_empty() || body.end.is_none());
        }
    }
}

/// A recorded request as the TS driver reports it: method, URL, sorted headers, body text.
pub fn recorded(request: &Request) -> Json {
    let mut headers = request.headers.clone();
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    obj(vec![
        ("method", Json::String(request.method.clone())),
        ("url", Json::String(request.url.clone())),
        (
            "headers",
            Json::Array(headers.into_iter().map(|(k, v)| Json::Array(vec![Json::String(k), Json::String(v)])).collect()),
        ),
        (
            "body",
            request.body.as_ref().map(|b| Json::String(String::from_utf8_lossy(b).into_owned())).unwrap_or(Json::Null),
        ),
    ])
}

// ---- JSON helpers ---------------------------------------------------------------------------------------

pub fn get<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    value.as_object().and_then(|o| o.get(key))
}

pub fn get_str<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    match get(value, key) {
        Some(Json::String(s)) => Some(s),
        _ => None,
    }
}

pub fn num(value: &Json) -> Option<f64> {
    match value {
        Json::Number(n) => Some(*n),
        _ => None,
    }
}

pub fn array<'a>(value: &'a Json, key: &str) -> &'a [Json] {
    match get(value, key) {
        Some(Json::Array(items)) => items,
        _ => &[],
    }
}

pub fn obj(entries: Vec<(&str, Json)>) -> Json {
    Json::Object(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
