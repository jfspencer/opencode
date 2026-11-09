//! The context cases (harness/ts/context.ts): the crate's client, data, location and event contexts over the
//! scripted transport, reporting the TS driver's per-step results.

use std::cell::RefCell;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_sync::context::client::{ClientContext, ClientInput};
use opencode_tui_sync::context::data::DataContext;
use opencode_tui_sync::context::event::EventContext;
use opencode_tui_sync::context::location::LocationContext;
use opencode_tui_sync::data::Promise;
use opencode_tui_sync::runtime::{self, LogLevel};

use crate::env::{self, array, get, get_str, num, obj, Script};
use crate::tracker::Tracker;

fn level(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

type Settled = Rc<RefCell<Vec<(String, Json)>>>;

fn track<T: Clone + 'static>(settled: &Settled, id: String, promise: Promise<T>, value: impl Fn(T) -> Json + 'static) {
    let settled = settled.clone();
    runtime::spawn(async move {
        let outcome = match promise.wait().await {
            Ok(result) => obj(vec![("id", Json::String(id.clone())), ("ok", value(result))]),
            Err(error) => obj(vec![("id", Json::String(id.clone())), ("error", crate::failure(&error))]),
        };
        settled.borrow_mut().push((id, outcome));
    });
}

fn location_arg(step: &Json) -> Option<Json> {
    get(step, "location").filter(|l| matches!(l, Json::Object(_))).cloned()
}

pub async fn run(spec: &Json, script: &Rc<Script>) -> Json {
    let tracker = Rc::new(Tracker::default());
    let logs: Rc<RefCell<Vec<Json>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = logs.clone();
    let client = ClientContext::new(ClientInput {
        api: crate::make_client(&obj(vec![]), script),
        url: Some("http://127.0.0.1:4096".into()),
        headers: None,
        service: None,
        log: Some(Rc::new(move |l, message, tags| {
            sink.borrow_mut().push(Json::Array(vec![Json::String(level(l).into()), Json::String(message.into()), tags.clone()]))
        })),
        reactivity: tracker.clone(),
    });
    let data = DataContext::new(&client, get_str(spec, "directory").unwrap_or("/project"));
    let location = LocationContext::new(&client, &data);
    let events: Rc<RefCell<Vec<Json>>> = Rc::new(RefCell::new(Vec::new()));
    let seen_events = events.clone();
    let _subscription = EventContext::new(&client).subscribe(move |event, metadata| {
        seen_events
            .borrow_mut()
            .push(Json::String(format!("{}@{}", event.r#type(), metadata.directory.clone().unwrap_or_default())));
    });
    let settled: Settled = Rc::new(RefCell::new(Vec::new()));
    let mut results = Vec::new();
    let mut seen = 0usize;
    let mut calls = 0usize;
    let mut snapshot = |op: &str| {
        let requests: Vec<Json> = script.requests.borrow().iter().skip(seen).map(|p| env::recorded(&p.recorded)).collect();
        seen = script.requests.borrow().len();
        let mut done: Vec<(String, Json)> = settled.borrow_mut().drain(..).collect();
        done.sort_by(|a, b| a.0.cmp(&b.0));
        let connection = client.connection();
        let error = location.error().map(|e| {
            obj(vec![("location", e.location), ("cause", crate::failure(&e.cause))])
        });
        let titles: Vec<Json> =
            ["ses_a", "ses_b", "ses_c"].iter().filter(|id| data.session.title.pending(id)).map(|id| Json::String((*id).into())).collect();
        let sync: Json = match data.internal() {
            Json::Object(mut internal) => internal.shift_remove("sync").unwrap_or(Json::Null),
            _ => Json::Null,
        };
        results.push(obj(vec![
            ("op", Json::String(op.into())),
            ("requests", Json::Array(requests)),
            ("settled", Json::Array(done.into_iter().map(|(_, v)| v).collect())),
            (
                "connection",
                obj(vec![
                    ("status", Json::String(connection.status().as_str().into())),
                    ("attempt", Json::Number(connection.attempt() as f64)),
                    ("error", connection.error().map(Json::String).unwrap_or(Json::Null)),
                ]),
            ),
            (
                "location",
                obj(vec![
                    ("ref", location.r#ref().unwrap_or(Json::Null)),
                    ("current", location.current().map(|r| (*r).clone()).unwrap_or(Json::Null)),
                    ("error", error.unwrap_or(Json::Null)),
                ]),
            ),
            ("titles", Json::Array(titles)),
            ("events", Json::Array(events.borrow_mut().drain(..).collect())),
            ("logs", Json::Array(logs.borrow_mut().drain(..).collect())),
            ("store", data.snapshot()),
            ("sync", sync),
        ]));
    };
    env::drain().await;
    snapshot("mount");
    for step in array(spec, "steps") {
        let op = get_str(step, "op").unwrap_or("");
        let target = crate::data::request_index(script, get(step, "request"));
        let id = format!("c{calls}");
        match op {
            "respond" => {
                if let Some(index) = target {
                    script.respond(index, step);
                }
            }
            "chunk" => {
                if let Some(index) = target {
                    script.push(index, get_str(step, "data").unwrap_or(""));
                }
            }
            "end" => {
                if let Some(index) = target {
                    script.end(index, get(step, "error").map(env::reason));
                }
            }
            "fail" => {
                if let Some(index) = target {
                    script.fail(index, get(step, "error").map(env::reason).unwrap_or(opencode_tui_sync::abort::Reason::error("")));
                }
            }
            "advance" => env::advance(get(step, "ms").and_then(num).unwrap_or(0.0)).await,
            "location.set" => location.set(location_arg(step)),
            "data.location.sync" => {
                calls += 1;
                track(&settled, id, data.location.sync(location_arg(step).as_ref()), |()| Json::Null);
            }
            "title.generate" => {
                calls += 1;
                track(&settled, id, data.session.title.generate(get_str(step, "session").unwrap_or("")), |()| Json::Null);
            }
            "media.read" => {
                calls += 1;
                track(&settled, id, client.media().read(get_str(step, "uri").unwrap_or("")), |bytes: Rc<Vec<u8>>| {
                    obj(vec![("bytes", Json::Array(bytes.iter().map(|b| Json::Number(*b as f64)).collect()))])
                });
            }
            "data.session.sync" => {
                calls += 1;
                track(&settled, id, data.session.sync(get_str(step, "session").unwrap_or(""), false), |()| Json::Null);
            }
            "data.session.message.sync" => {
                calls += 1;
                track(&settled, id, data.session.message.sync(get_str(step, "session").unwrap_or("")), |()| Json::Null);
            }
            _ => {}
        }
        env::drain().await;
        snapshot(op);
    }
    location.dispose();
    data.dispose();
    client.dispose();
    obj(vec![("results", Json::Array(results))])
}
