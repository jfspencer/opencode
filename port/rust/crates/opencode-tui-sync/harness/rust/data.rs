//! The data-layer cases (harness/ts/data.ts): the crate's `Data` driven by the same steps, reporting the same
//! per-step results.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_sync::bus::Emitter;
use opencode_tui_sync::client::Error;
use opencode_tui_sync::connection::Status;
use opencode_tui_sync::data::{ConnectionState, CreateDataInput, CreateInput, Data, LoadMoreOptions, Promise, PromptInput, SessionStatus};
use opencode_tui_sync::event::Event;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::reactive::{Key, Reactivity};
use opencode_tui_sync::retention::{RetentionInput, SessionRetention};

use crate::tracker::Tracker;

use crate::env::{self, array, get, get_str, num, obj, Script};

/// A connection whose status the case sets.
#[derive(Default)]
struct Stub {
    status: Cell<Option<Status>>,
    listeners: RefCell<Vec<Box<dyn Fn(Status)>>>,
}

impl Stub {
    fn set(&self, status: Status) {
        if self.status.get() == Some(status) {
            return;
        }
        self.status.set(Some(status));
        for listener in self.listeners.borrow().iter() {
            listener(status);
        }
    }
}

impl ConnectionState for Stub {
    fn status(&self) -> Status {
        self.status.get().unwrap_or(Status::Connected)
    }
    fn on_status(&self, listener: Box<dyn Fn(Status)>) {
        self.listeners.borrow_mut().push(listener);
    }
}

fn parse_status(text: &str) -> Status {
    match text {
        "connected" => Status::Connected,
        "reconnecting" => Status::Reconnecting,
        _ => Status::Connecting,
    }
}

/// The request a respond step names: an index, "last", or the first unsettled one matching `METHOD path`.
pub fn request_index(script: &Script, target: Option<&Json>) -> Option<usize> {
    let requests = script.requests.borrow();
    match target? {
        Json::Number(n) => Some(*n as usize),
        Json::String(last) if last == "last" => requests.len().checked_sub(1),
        target => {
            let wanted = target.str("match")?;
            requests.iter().position(|pending| {
                if pending.settled.get() {
                    return false;
                }
                let url = url::Url::parse(&pending.recorded.url).ok();
                let path = url.map(|u| format!("{}{}", u.path(), u.query().map(|q| format!("?{q}")).unwrap_or_default())).unwrap_or_default();
                format!("{} {path}", pending.recorded.method).starts_with(wanted)
            })
        }
    }
}

fn records(items: &[Rc<Json>]) -> Json {
    Json::Array(items.iter().map(|item| (**item).clone()).collect())
}

fn value(json: Json) -> Json {
    obj(vec![("value", json)])
}

fn undefined() -> Json {
    obj(vec![("undefined", Json::Bool(true))])
}

fn optional(json: Option<Json>) -> Json {
    json.map(value).unwrap_or_else(undefined)
}

fn strings(items: impl IntoIterator<Item = String>) -> Json {
    Json::Array(items.into_iter().map(Json::String).collect())
}

/// The location argument of a call (`undefined` when absent or null).
fn location(args: &[Json], at: usize) -> Option<&Json> {
    args.get(at).filter(|a| matches!(a, Json::Object(_)))
}

enum Returned {
    Value(Json),
    Promise(Promise<Json>),
    Created(String, Promise<Json>),
}

fn unit(promise: Promise<()>) -> Returned {
    let mapped: Promise<Json> = opencode_tui_sync::runtime::Deferred::spawn(async move { promise.wait().await.map(|()| Json::Null) });
    Returned::Promise(mapped)
}

fn json_promise(promise: Promise<Json>) -> Returned {
    Returned::Promise(promise)
}

/// Calls the data method at a dotted path with JSON arguments.
fn dispatch(data: &Data, path: &str, args: &[Json]) -> Result<Returned, String> {
    let s = |i: usize| get_text(args, i);
    let session = &data.session;
    let location_api = &data.location;
    let resource_of = |name: &str| match name {
        "vcs" => Some(&location_api.vcs),
        "usage" => Some(&location_api.usage),
        "agent" => Some(&location_api.agent),
        "command" => Some(&location_api.command),
        "config" => Some(&location_api.config),
        "integration" => Some(&location_api.integration),
        "model" => Some(&location_api.model),
        "provider" => Some(&location_api.provider),
        "reference" => Some(&location_api.reference),
        "skill" => Some(&location_api.skill),
        "mcp.server" => Some(&location_api.mcp.server),
        "mcp.resource" => Some(&location_api.mcp.resource),
        _ => None,
    };
    Ok(match path {
        "session.list" => Returned::Value(value(records(&session.list()))),
        "session.get" => Returned::Value(optional(session.get(&s(0)).map(|r| (*r).clone()))),
        "session.missing" => Returned::Value(value(Json::Bool(session.missing(&s(0))))),
        "session.forget" => {
            session.forget(&s(0));
            Returned::Value(undefined())
        }
        "session.creating" => Returned::Value(value(Json::Bool(session.creating(&s(0))))),
        "session.remember" => {
            session.remember(args.first().cloned().unwrap_or_default());
            Returned::Value(undefined())
        }
        "session.setStatus" => {
            let status = if s(1) == "running" { SessionStatus::Running } else { SessionStatus::Idle };
            session.set_status(&s(0), status);
            Returned::Value(undefined())
        }
        "session.root" => Returned::Value(value(Json::String(session.root(&s(0))))),
        "session.family" => Returned::Value(value(strings(session.family(&s(0)).iter().cloned()))),
        "session.evict" => {
            session.evict(&s(0));
            Returned::Value(undefined())
        }
        "session.cost" => Returned::Value(value(Json::Number(session.cost(&s(0))))),
        "session.status" => Returned::Value(value(Json::String(session.status(&s(0)).as_str().into()))),
        "session.active" => Returned::Value(value(strings(session.active()))),
        "session.input.list" => Returned::Value(value(strings(session.input.list(&s(0))))),
        "session.input.has" => Returned::Value(value(Json::Bool(session.input.has(&s(0), &s(1))))),
        "session.pending.list" => Returned::Value(value(records(&session.pending.list(&s(0))))),
        "session.pending.status" => {
            Returned::Value(optional(session.pending.status(&s(0), &s(1)).map(|status| Json::String(status.as_str().into()))))
        }
        "session.pending.sync" => unit(session.pending.sync(&s(0))),
        "session.pending.invalidate" => {
            session.pending.invalidate(&s(0));
            Returned::Value(undefined())
        }
        "session.create" => {
            let input = args.first().cloned().unwrap_or_default();
            let text = |key: &str| input.str(key).map(str::to_owned);
            let created = session.create(CreateInput {
                id: text("id"),
                title: text("title"),
                agent: text("agent"),
                model: input.get("model").cloned(),
                location: input.get("location").cloned(),
                project_id: text("projectID"),
            });
            Returned::Created(created.id, created.request)
        }
        "session.compact" => {
            let input = args.first().cloned().unwrap_or_default();
            json_promise(session.compact(input.str("sessionID").unwrap_or(""), input.get("model").cloned()))
        }
        "session.prompt" => json_promise(session.prompt(PromptInput {
            request: args.first().cloned().unwrap_or_default(),
            gate: None,
            prepare: None,
        })),
        "session.sync" => {
            let children = args.get(1).and_then(|o| o.bool("children")).unwrap_or(false);
            unit(session.sync(&s(0), children))
        }
        "session.invalidate" => {
            session.invalidate(&s(0));
            Returned::Value(undefined())
        }
        "session.message.list" => Returned::Value(value(records(&session.message.list(&s(0))))),
        "session.message.get" => Returned::Value(optional(session.message.get(&s(0), &s(1)).map(|r| (*r).clone()))),
        "session.message.more" => Returned::Value(value(Json::Bool(session.message.more(&s(0))))),
        "session.message.loading" => Returned::Value(value(Json::Bool(session.message.loading(&s(0))))),
        "session.message.sync" => unit(session.message.sync(&s(0))),
        "session.message.loadMore" => {
            let all = args.get(1).and_then(|o| o.bool("all")).unwrap_or(false);
            unit(session.message.load_more(&s(0), LoadMoreOptions { all, ..Default::default() }))
        }
        "session.message.invalidate" => {
            session.message.invalidate(&s(0));
            Returned::Value(undefined())
        }
        "session.permission.list" => Returned::Value(optional(session.permission.list(&s(0)).map(|l| records(&l)))),
        "session.permission.sessions" => Returned::Value(value(strings(session.permission.sessions()))),
        "session.permission.sync" => unit(session.permission.sync(&s(0))),
        "session.permission.invalidate" => {
            session.permission.invalidate(&s(0));
            Returned::Value(undefined())
        }
        "session.permission.reply" => unit(session.permission.reply(args.first().cloned().unwrap_or_default())),
        "session.form.list" => Returned::Value(optional(session.form.list(&s(0), location(args, 1)).map(|l| records(&l)))),
        "session.form.sessions" => Returned::Value(value(strings(session.form.sessions()))),
        "session.form.sync" => unit(session.form.sync(&s(0), location(args, 1))),
        "session.form.invalidate" => {
            session.form.invalidate(&s(0), location(args, 1));
            Returned::Value(undefined())
        }
        "session.form.reply" => unit(session.form.reply(args.first().cloned().unwrap_or_default(), location(args, 1))),
        "session.form.cancel" => unit(session.form.cancel(args.first().cloned().unwrap_or_default(), location(args, 1))),
        "project.list" => Returned::Value(value(records(&data.project.list()))),
        "project.get" => Returned::Value(optional(data.project.get(&s(0)).map(|r| (*r).clone()))),
        "project.sync" => unit(data.project.sync()),
        "project.invalidate" => {
            data.project.invalidate();
            Returned::Value(undefined())
        }
        "project.permission.list" => Returned::Value(optional(data.project.permission.list(&s(0)).map(|r| (*r).clone()))),
        "project.permission.sync" => unit(data.project.permission.sync(&s(0))),
        "shell.list" => Returned::Value(value(Json::Array(data.shell.list(location(args, 0))))),
        "shell.listBySession" => Returned::Value(value(Json::Array(data.shell.list_by_session(&s(0))))),
        "shell.get" => Returned::Value(optional(data.shell.get(&s(0)))),
        "shell.sync" => unit(data.shell.sync(location(args, 0))),
        "shell.invalidate" => {
            data.shell.invalidate(location(args, 0));
            Returned::Value(undefined())
        }
        "location.info" => Returned::Value(optional(location_api.info(location(args, 0)).map(|r| (*r).clone()))),
        "location.default" => Returned::Value(value(location_api.default())),
        "location.syncInfo" => unit(location_api.sync_info(location(args, 0))),
        "location.sync" => unit(location_api.sync(location(args, 0))),
        "location.invalidate" => {
            location_api.invalidate(location(args, 0));
            Returned::Value(undefined())
        }
        "location.websearch.list" => Returned::Value(optional(location_api.websearch.list(location(args, 0)).map(|r| (*r).clone()))),
        "location.websearch.refresh" => unit(location_api.websearch.refresh(location(args, 0))),
        other => {
            let Some((name, method)) = other.strip_prefix("location.").and_then(|rest| rest.rsplit_once('.')) else {
                return Err(format!("no data method {other}"));
            };
            let Some(resource) = resource_of(name) else { return Err(format!("no data method {other}")) };
            match method {
                "list" | "info" | "limits" => Returned::Value(optional(resource.list(location(args, 0)).map(|r| (*r).clone()))),
                "sync" => unit(resource.sync(location(args, 0))),
                "invalidate" => {
                    resource.invalidate(location(args, 0));
                    Returned::Value(undefined())
                }
                _ => return Err(format!("no data method {other}")),
            }
        }
    })
}

fn get_text(args: &[Json], index: usize) -> String {
    match args.get(index) {
        Some(Json::String(s)) => s.clone(),
        Some(other) => opencode_tui_sync::js::string(other),
        None => "undefined".into(),
    }
}

fn failure(error: &Error) -> Json {
    crate::failure(error)
}

pub async fn run(spec: &Json, script: &Rc<Script>) -> Json {
    let client = crate::make_client(&obj(vec![]), script).with_media_refs();
    let stub = Rc::new(Stub::default());
    stub.set(parse_status(get_str(spec, "status").unwrap_or("connected")));
    let emitter = Emitter::new();
    let errors: Rc<RefCell<Vec<Json>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = errors.clone();
    let limit = get(spec, "initialMessageLimit").and_then(num);
    let tracker = Rc::new(Tracker::default());
    let view: Rc<RefCell<(Option<String>, Vec<String>)>> = Rc::new(RefCell::new((None, Vec::new())));
    let runs = Rc::new(Cell::new(0u32));
    let data = Data::create(CreateDataInput {
        api: Rc::new(move || client.clone()),
        directory: get_str(spec, "directory").unwrap_or("/project").to_owned(),
        initial_message_limit: limit.map(|limit| Rc::new(move || limit) as Rc<dyn Fn() -> f64>),
        event: emitter.clone(),
        connection: Some(stub.clone()),
        on_error: get(spec, "onError")
            .is_some_and(|v| *v == Json::Bool(true))
            .then(|| Rc::new(move |error: &Error| sink.borrow_mut().push(Json::String(error.message()))) as Rc<dyn Fn(&Error)>),
        omit_tool_file_data: get(spec, "omitToolFileData").is_some_and(|v| *v == Json::Bool(true)),
        reactivity: tracker.clone(),
        log: None,
    });
    let settled: Rc<RefCell<Vec<(String, Json)>>> = Rc::new(RefCell::new(Vec::new()));
    let mut results = Vec::new();
    let mut seen = 0usize;
    let mut calls = 0usize;
    for step in array(spec, "steps") {
        let op = get_str(step, "op").unwrap_or("");
        let mut returned = None;
        match op {
            "events" => {
                // One flush of the connection: `batch(() => events.forEach(onEvent))`.
                opencode_tui_sync::reactive::batch(&*tracker, || {
                    for event in array(step, "events") {
                        emitter.emit(&Event::new(event.clone()));
                    }
                });
            }
            "call" => {
                let id = get_str(step, "id").map(str::to_owned).unwrap_or_else(|| {
                    let id = format!("c{calls}");
                    calls += 1;
                    id
                });
                let path = get_str(step, "path").unwrap_or("");
                let args = array(step, "args");
                returned = Some(match dispatch(&data, path, args) {
                    Err(message) => obj(vec![("crashed", Json::String(message))]),
                    Ok(Returned::Value(json)) => json,
                    Ok(Returned::Promise(promise)) => {
                        track(&settled, id.clone(), promise);
                        obj(vec![("promise", Json::String(id))])
                    }
                    Ok(Returned::Created(session, promise)) => {
                        track(&settled, id.clone(), promise);
                        obj(vec![("id", Json::String(session)), ("promise", Json::String(id))])
                    }
                });
            }
            "respond" => {
                if let Some(index) = request_index(script, get(step, "request")) {
                    script.respond(index, step);
                }
            }
            "fail" => {
                if let Some(index) = request_index(script, get(step, "request")) {
                    script.fail(index, get(step, "error").map(env::reason).unwrap_or(opencode_tui_sync::abort::Reason::error("")));
                }
            }
            "status" => stub.set(parse_status(get_str(step, "value").unwrap_or(""))),
            "retain" => {
                let (current_view, current_tracker) = (view.clone(), tracker.clone());
                let current_runs = runs.clone();
                let (keep_view, keep_tracker) = (view.clone(), tracker.clone());
                let retention = Rc::new(SessionRetention::new(RetentionInput {
                    data: data.clone(),
                    current: Rc::new(move || {
                        current_tracker.track(&Key::Custom("harness.view"));
                        current_runs.set(current_runs.get() + 1);
                        current_view.borrow().0.clone()
                    }),
                    keep: Rc::new(move || {
                        keep_tracker.track(&Key::Custom("harness.view"));
                        keep_view.borrow().1.clone()
                    }),
                    limit: get(step, "limit").and_then(num).unwrap_or(0.0) as usize,
                }));
                tracker.effect(move || retention.run());
            }
            "view" => {
                *view.borrow_mut() = (
                    get_str(step, "current").map(str::to_owned),
                    array(step, "keep").iter().filter_map(|k| match k { Json::String(s) => Some(s.clone()), _ => None }).collect(),
                );
                tracker.trigger(&Key::Custom("harness.view"));
            }
            "usage" => {
                let messages = data.session.message.list(get_str(step, "session").unwrap_or(""));
                let found = opencode_tui_sync::usage::last_assistant_with_usage(&messages, get_str(step, "boundary"));
                returned = Some(optional(found.map(|i| (*messages[i]).clone())));
            }
            "advance" => env::advance(get(step, "ms").and_then(num).unwrap_or(0.0)).await,
            "dispose" => data.dispose(),
            _ => {}
        }
        env::drain().await;
        let requests: Vec<Json> = script.requests.borrow().iter().skip(seen).map(|p| env::recorded(&p.recorded)).collect();
        seen = script.requests.borrow().len();
        let mut done: Vec<(String, Json)> = settled.borrow_mut().drain(..).collect();
        done.sort_by(|a, b| a.0.cmp(&b.0));
        let mut entries = vec![("op", Json::String(op.into()))];
        if let Some(returned) = returned {
            entries.push(("value", returned));
        }
        entries.push(("requests", Json::Array(requests)));
        entries.push(("settled", Json::Array(done.into_iter().map(|(_, v)| v).collect())));
        entries.push(("errors", Json::Array(errors.borrow_mut().drain(..).collect())));
        entries.push(("retentionRuns", Json::Number(runs.get() as f64)));
        entries.push(("store", data.snapshot()));
        entries.push(("internal", data.internal()));
        results.push(obj(entries));
    }
    data.dispose();
    obj(vec![("results", Json::Array(results))])
}

fn track(settled: &Rc<RefCell<Vec<(String, Json)>>>, id: String, promise: Promise<Json>) {
    let settled = settled.clone();
    opencode_tui_sync::runtime::spawn(async move {
        let outcome = match promise.wait().await {
            Ok(json) => obj(vec![("id", Json::String(id.clone())), ("ok", json)]),
            Err(error) => obj(vec![("id", Json::String(id.clone())), ("error", failure(&error))]),
        };
        settled.borrow_mut().push((id, outcome));
    });
}
