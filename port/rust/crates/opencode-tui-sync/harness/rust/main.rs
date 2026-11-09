//! The Rust side of the differential harness (see harness/ts/driver.ts): runs each case of a JSONL file against
//! this crate and prints one JSON result per case, in the TS driver's shape.
//!
//! usage: sync-driver <cases.jsonl>

mod context;
mod data;
mod tracker;
mod env;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use env::{array, get, get_str, num, obj, recorded, Script, ScriptTransport};
use opencode_protocol::wire::Out;
use opencode_protocol::Json;
use opencode_tui_sync::abort::{AbortController, Reason};
use opencode_tui_sync::client::{Client, ClientOptions, Error, RequestOptions, Value};
use opencode_tui_sync::connection::{ClientConnection, ConnectionOptions, Reconnect};
use opencode_tui_sync::event::Event;
use opencode_tui_sync::reactive::{Key, Reactivity};
use opencode_tui_sync::runtime::{Deferred, LogLevel};
use opencode_tui_sync::shared_events::SubscribeOptions;

fn main() {
    let path = std::env::args().nth(1).expect("usage: sync-driver <cases.jsonl>");
    let text = std::fs::read_to_string(&path).expect("cases file");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().start_paused(true).build().expect("runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        env::install();
        let script = Rc::new(Script::default());
        for line in text.lines().filter(|l| !l.is_empty()) {
            let spec = opencode_protocol::wire::parse(line).expect("case JSON");
            env::reset();
            script.reset();
            let kind = get_str(&spec, "kind").unwrap_or("");
            let result = match kind {
                "request" => run_request(&spec, &script).await,
                "sse" => run_sse(&spec, &script).await,
                "connection" => run_connection(&spec, &script).await,
                "data" => data::run(&spec, &script).await,
                "context" => context::run(&spec, &script).await,
                "pty" => run_pty(&spec, &script).await,
                other => obj(vec![("crashed", Json::String(format!("unknown case kind {other}")))]),
            };
            env::advance(120_000.0).await;
            let line = obj(vec![("name", get(&spec, "name").cloned().unwrap_or(Json::Null)), ("result", result)]);
            let mut out = Out::new();
            out.json(&line);
            println!("{}", out.finish());
        }
    });
}

pub fn make_client(spec: &Json, script: &Rc<Script>) -> Client {
    let headers = match get(spec, "headers") {
        Some(Json::Object(object)) => object
            .iter()
            .map(|(k, v)| (k.clone(), match v {
                Json::String(s) => s.clone(),
                other => opencode_tui_sync::js::string(other),
            }))
            .collect(),
        _ => Vec::new(),
    };
    Client::make(ClientOptions {
        base_url: get_str(spec, "baseUrl").unwrap_or("http://127.0.0.1:4096").to_owned(),
        headers,
        transport: Rc::new(ScriptTransport(script.clone())),
    })
}

fn requests_from(script: &Script, from: usize) -> Json {
    Json::Array(script.requests.borrow().iter().skip(from).map(|p| recorded(&p.recorded)).collect())
}

pub fn outcome(value: Value) -> Json {
    match value {
        Value::Undefined => obj(vec![("undefined", Json::Bool(true))]),
        Value::Json(json) => obj(vec![("value", json)]),
        Value::Bytes(bytes) => obj(vec![("bytes", Json::Array(bytes.into_iter().map(|b| Json::Number(b as f64)).collect()))]),
    }
}

pub fn failure(error: &Error) -> Json {
    let mut entries = vec![("name", Json::String(error.name())), ("message", Json::String(error.message()))];
    if let Some(tag) = error.tag() {
        entries.push(("tag", Json::String(tag.into())));
    }
    obj(entries)
}

async fn run_request(spec: &Json, script: &Rc<Script>) -> Json {
    let client = make_client(spec, script);
    let name = get_str(spec, "op").unwrap_or("");
    let Some(op) = Client::operation(name) else {
        return obj(vec![("crashed", Json::String(format!("no operation {name}")))]);
    };
    let controller = AbortController::new();
    let abort = get(spec, "abort");
    let when = abort.and_then(|a| get_str(a, "when"));
    let reason = abort.and_then(|a| get(a, "reason")).and_then(|r| get_str(r, "message")).map(Reason::error);
    let mut options = RequestOptions::default();
    if let Some(Json::Object(headers)) = get(spec, "options").and_then(|o| get(o, "headers")) {
        options.headers = headers.iter().map(|(k, v)| (k.clone(), opencode_tui_sync::js::string(v))).collect();
    }
    if abort.is_some() {
        options.signal = Some(controller.signal().clone());
    }
    if when == Some("before") {
        controller.abort(reason.clone());
    }
    let input = get(spec, "input").cloned().unwrap_or(Json::Null);
    let call = client.call(op, &input, options);
    let settled = Rc::new(RefCell::new(None));
    let slot = settled.clone();
    tokio::task::spawn_local(async move {
        let result = match call.await {
            Ok(value) => outcome(value),
            Err(error) => obj(vec![("error", failure(&error))]),
        };
        *slot.borrow_mut() = Some(result);
    });
    env::drain().await;
    let request = requests_from(script, 0);
    if when == Some("pending") {
        controller.abort(reason.clone());
    }
    if let Some(first) = script.get(0) {
        if !first.settled.get() && !controller.signal().aborted() {
            match get(spec, "response") {
                Some(response) if get_str(response, "type") == Some("respond") => script.respond(0, response),
                Some(response) if get_str(response, "type") == Some("fail") => {
                    script.fail(0, get(response, "error").map(env::reason).unwrap_or(Reason::error("")))
                }
                _ => {}
            }
        }
    }
    env::drain().await;
    if when == Some("body") {
        controller.abort(reason);
    }
    env::drain().await;
    let result = settled.borrow_mut().take().unwrap_or_else(|| obj(vec![("pending", Json::Bool(true))]));
    obj(vec![("request", request), ("result", result)])
}

async fn run_sse(spec: &Json, script: &Rc<Script>) -> Json {
    let client = make_client(spec, script);
    let controller = AbortController::new();
    let activity = Rc::new(Cell::new(0u32));
    let counter = activity.clone();
    let types = array(spec, "types").iter().filter_map(|t| if let Json::String(t) = t { Some(t.clone()) } else { None }).collect();
    let mut subscription = client.event().subscribe(SubscribeOptions {
        types,
        signal: Some(controller.signal().clone()),
        on_activity: Some(Rc::new(move || counter.set(counter.get() + 1))),
    });
    let response = get(spec, "response").cloned().unwrap_or(Json::Null);
    let abort_after = get(spec, "abortAfter").and_then(num).map(|n| n as usize);
    let reads = get(spec, "reads").and_then(num).unwrap_or(20.0) as usize;
    let mut results = Vec::new();
    for i in 0..reads {
        if abort_after == Some(i) {
            controller.abort(None);
        }
        let activity = activity.clone();
        let mut next = std::pin::pin!(async {
            let item = subscription.next().await;
            (item, activity.get())
        });
        let mut settled = env::poll_once(&mut next).await;
        env::drain().await;
        if i == 0 {
            if let Some(first) = script.get(0) {
                if !first.settled.get() {
                    script.respond(0, &response);
                }
            }
        }
        env::drain().await;
        if settled.is_none() {
            settled = env::poll_once(&mut next).await;
        }
        let answered = settled.is_some();
        if !answered {
            // A read the stream cannot answer yet stays pending: abort to end it.
            results.push(obj(vec![("pending", Json::Bool(true))]));
            controller.abort(None);
            env::drain().await;
            settled = env::poll_once(&mut next).await;
        }
        let result = match settled {
            None => obj(vec![("pending", Json::Bool(true))]),
            Some((None, _)) => obj(vec![("done", Json::Bool(true))]),
            Some((Some(Ok(event)), _)) => {
                obj(vec![("value", event.json().clone())])
            }
            Some((Some(Err(error)), _)) => {
                let error = error.reason();
                obj(vec![("error", obj(vec![("name", Json::String(error.name)), ("message", Json::String(error.message))]))])
            }
        };
        let stop = !answered || get(&result, "done").is_some() || get(&result, "error").is_some();
        results.push(result);
        if stop {
            break;
        }
    }
    subscription.close();
    drop(subscription);
    env::drain().await;
    obj(vec![
        ("request", requests_from(script, 0)),
        ("reads", Json::Array(results)),
        ("activity", Json::Number(activity.get() as f64)),
        ("closed", Json::Bool(script.get(0).is_some_and(|p| p.closed.get()))),
    ])
}

async fn run_pty(spec: &Json, script: &Rc<Script>) -> Json {
    use opencode_tui_sync::pty::{decode_persistent_pty_message, persistent_pty_frame, PersistentPtyClient, PersistentPtyConnectInput};
    let decoded: Vec<Json> = array(spec, "texts")
        .iter()
        .map(|text| match text {
            Json::String(text) => decode_persistent_pty_message(text).map(|m| m.to_json()).unwrap_or(Json::Null),
            _ => Json::Null,
        })
        .collect();
    let frames: Vec<Json> = array(spec, "frames")
        .iter()
        .map(|frame| {
            let data: Option<Vec<u8>> = match get(frame, "data") {
                Some(Json::Array(items)) => Some(items.iter().filter_map(num).map(|b| b as u8).collect()),
                _ => None,
            };
            let bytes = persistent_pty_frame(
                get(frame, "cols").and_then(num).unwrap_or(0.0),
                get(frame, "rows").and_then(num).unwrap_or(0.0),
                data.as_deref(),
            );
            Json::Array(bytes.into_iter().map(|b| Json::Number(b as f64)).collect())
        })
        .collect();
    let connect = get(spec, "connect").cloned().unwrap_or_default();
    let url = get_str(&connect, "url").unwrap_or("").to_owned();
    let client = make_client(&obj(vec![("baseUrl", Json::String(url.clone()))]), script);
    let pty = PersistentPtyClient::new(client, url);
    let pending = pty.connect_url(&PersistentPtyConnectInput {
        pty_id: get_str(&connect, "ptyID").unwrap_or("").to_owned(),
        cursor: get(&connect, "cursor").and_then(num).unwrap_or(0.0),
        attachment_id: get_str(&connect, "attachmentID").unwrap_or("").to_owned(),
        takeover: get(&connect, "takeover") == Some(&Json::Bool(true)),
    });
    env::drain().await;
    if script.get(0).is_some() {
        let ticket = get_str(&connect, "ticket").unwrap_or("").to_owned();
        let body = if ticket == "missing" {
            "{}".to_owned()
        } else {
            opencode_tui_sync::client::stringify(&obj(vec![("data", obj(vec![("ticket", Json::String(ticket))]))]))
        };
        script.respond(
            0,
            &obj(vec![
                ("status", Json::Number(200.0)),
                ("headers", Json::Array(vec![Json::Array(vec![Json::String("content-type".into()), Json::String("application/json".into())])])),
                ("chunks", Json::Array(vec![Json::String(body)])),
            ]),
        );
    }
    let (url, error) = match pending.wait().await {
        Ok(url) => (Json::String(url), Json::Null),
        Err(error) => (Json::Null, failure(&error)),
    };
    obj(vec![
        ("decoded", Json::Array(decoded)),
        ("frames", Json::Array(frames)),
        ("request", requests_from(script, 0)),
        ("url", url),
        ("error", error),
    ])
}

/// Groups delivered events by the batch that delivered them, as the TS driver does.
struct Batches {
    groups: RefCell<Vec<Vec<String>>>,
    open: Cell<bool>,
}

impl Reactivity for Batches {
    fn track(&self, _: &Key) {}
    fn trigger(&self, _: &Key) {}
    fn batch(&self, f: &mut dyn FnMut()) {
        let outer = !self.open.get();
        if outer {
            self.open.set(true);
            self.groups.borrow_mut().push(Vec::new());
        }
        f();
        if outer {
            self.open.set(false);
        }
    }
}

async fn run_connection(spec: &Json, script: &Rc<Script>) -> Json {
    let api = make_client(spec, script);
    let batches = Rc::new(Batches { groups: RefCell::new(Vec::new()), open: Cell::new(false) });
    let logs: Rc<RefCell<Vec<Json>>> = Rc::new(RefCell::new(Vec::new()));
    let reconnects: Rc<RefCell<Vec<Deferred<Result<Client, Reason>>>>> = Rc::new(RefCell::new(Vec::new()));
    let reconnect: Option<Reconnect> = if get(spec, "reconnect").is_some_and(|v| *v == Json::Bool(true)) {
        let reconnects = reconnects.clone();
        let logs = logs.clone();
        Some(Rc::new(move |signal| {
            let deferred: Deferred<Result<Client, Reason>> = Deferred::new();
            reconnects.borrow_mut().push(deferred.clone());
            let logs = logs.clone();
            signal.add_listener(move || logs.borrow_mut().push(Json::String("reconnect signal aborted".into())));
            Box::pin(async move { deferred.wait().await })
        }))
    } else {
        None
    };
    let events = batches.clone();
    let sink = logs.clone();
    let connection = ClientConnection::create(
        api,
        ConnectionOptions {
            reconnect,
            on_event: Rc::new(move |event: &Event| {
                let mut groups = events.groups.borrow_mut();
                match groups.last_mut() {
                    Some(group) if events.open.get() => group.push(event.r#type().to_owned()),
                    _ => groups.push(vec![event.r#type().to_owned()]),
                }
            }),
            flush_interval: get(spec, "flushInterval").and_then(num),
            idle_timeout: get(spec, "idleTimeout").and_then(num),
            log: Some(Rc::new(move |level, message, data| {
                if level == LogLevel::Info {
                    sink.borrow_mut().push(Json::Array(vec![Json::String(message.into()), data.clone()]));
                }
            })),
            reactivity: batches.clone(),
        },
    );
    let mut snapshots = Vec::new();
    let mut seen = 0usize;
    let mut snapshot = |op: &str, connection: &ClientConnection| {
        let requests: Vec<Json> =
            script.requests.borrow().iter().skip(seen).map(|p| Json::String(p.recorded.url.clone())).collect();
        seen = script.requests.borrow().len();
        let groups: Vec<Json> = batches
            .groups
            .borrow_mut()
            .drain(..)
            .filter(|g| !g.is_empty())
            .map(|g| Json::Array(g.into_iter().map(Json::String).collect()))
            .collect();
        snapshots.push(obj(vec![
            ("op", Json::String(op.into())),
            ("now", Json::Number(env::clock() - env::BASE_TIME)),
            ("status", Json::String(connection.status().as_str().into())),
            ("attempt", Json::Number(connection.attempt() as f64)),
            ("error", connection.error().map(Json::String).unwrap_or(Json::Null)),
            ("requests", Json::Array(requests)),
            ("batches", Json::Array(groups)),
            ("pendingReconnects", Json::Number(reconnects.borrow().iter().filter(|d| !d.settled()).count() as f64)),
            ("logs", Json::Array(logs.borrow_mut().drain(..).collect())),
        ]));
    };
    env::drain().await;
    snapshot("start", &connection);
    let mut taken = 0usize;
    for step in array(spec, "steps") {
        let op = get_str(step, "op").unwrap_or("");
        let index = match get(step, "request") {
            Some(Json::String(last)) if last == "last" => script.requests.borrow().len().wrapping_sub(1),
            Some(Json::Number(n)) if *n >= 0.0 => *n as usize,
            _ => usize::MAX,
        };
        match op {
            "respond" => script.respond(index, step),
            "chunk" => script.push(index, get_str(step, "data").unwrap_or("")),
            "end" => script.end(index, get(step, "error").map(env::reason)),
            "fail" => script.fail(index, get(step, "error").map(env::reason).unwrap_or(Reason::error(""))),
            "advance" => env::advance(get(step, "ms").and_then(num).unwrap_or(0.0)).await,
            "resync" => connection.resync(get_str(step, "reason").unwrap_or("")),
            "reconnect" => {
                let pending = reconnects.borrow().get(taken).cloned();
                if let Some(pending) = pending {
                    taken += 1;
                    if get(step, "ok").is_some_and(|v| *v == Json::Bool(true)) {
                        pending.resolve(Ok(make_client(spec, script)));
                    } else {
                        pending.resolve(Err(Reason::error("resolution failed")));
                    }
                }
            }
            "dispose" => connection.dispose(),
            _ => {}
        }
        env::drain().await;
        snapshot(op, &connection);
    }
    connection.dispose();
    let history: Vec<Json> = connection
        .history()
        .into_iter()
        .map(|h| {
            let mut data = vec![("status", Json::String(h.status.into())), ("attempt", Json::Number(h.attempt as f64))];
            if let Some(error) = h.error {
                data.push(("error", Json::String(error)));
            }
            obj(vec![
                ("type", Json::String("client.connection".into())),
                ("created", Json::Number(h.created - env::BASE_TIME)),
                ("data", obj(data)),
            ])
        })
        .collect();
    snapshots.push(obj(vec![("history", Json::Array(history))]));
    obj(vec![("snapshots", Json::Array(snapshots))])
}
