//! The Rust side of the differential check: runs every case of a corpus (JSON lines, see harness/README.md) and
//! prints one JSON line per case, `{"name", "result", "output"}`, byte for byte as harness/run-ts.ts prints them.
//!
//! usage: cargo run --example codemode_run -- corpus.jsonl > rust.jsonl

use opencode_codemode::json::{self, Json, JsonObj};
use opencode_codemode::{ExecuteOptions, Execution, Hooks, JsStr, Limits, Runtime, ToolDef, ToolOutcome, ToolOutput};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

fn text(value: Option<&Json>) -> String {
    value.and_then(|v| v.as_str()).map(|s| s.to_rust_lossy()).unwrap_or_default()
}

fn number(value: Option<&Json>) -> Option<u64> {
    match value {
        Some(Json::Num(n)) => Some(*n as u64),
        _ => None,
    }
}

#[derive(Clone)]
enum Behavior {
    Echo,
    Value(Json),
    Fail(String),
    Sequence(Vec<Result<Json, String>>),
}

struct ToolSpec {
    behavior: Behavior,
    yields: bool,
    calls: Cell<usize>,
}

fn behavior(spec: Option<&Json>) -> Behavior {
    let Some(Json::Obj(obj)) = spec else { return Behavior::Echo };
    match text(obj.get("kind")).as_str() {
        "value" => Behavior::Value(obj.get("value").cloned().unwrap_or(Json::Null)),
        "fail" | "die" => Behavior::Fail(text(obj.get("message"))),
        "sequence" => {
            let values = match obj.get("values") {
                Some(Json::Arr(items)) => items
                    .iter()
                    .map(|item| match item.as_obj() {
                        Some(step) if step.get("fail").is_some() => Err(text(step.get("fail"))),
                        Some(step) => Ok(step.get("ok").cloned().unwrap_or(Json::Null)),
                        None => Ok(Json::Null),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Behavior::Sequence(values)
        }
        _ => Behavior::Echo,
    }
}

/// Returns Pending once, then Ready: a host call that finishes after one scheduler turn.
struct YieldOnce(bool);

impl std::future::Future for YieldOnce {
    type Output = ();
    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        if self.0 {
            return std::task::Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

async fn run_case(case: &JsonObj) -> Json {
    let mut tools = Vec::new();
    let mut specs: HashMap<String, Rc<ToolSpec>> = HashMap::new();
    if let Some(Json::Arr(items)) = case.get("tools") {
        for item in items {
            let Some(tool) = item.as_obj() else { continue };
            let path = text(tool.get("path"));
            let input = tool.get("input").cloned().unwrap_or_else(|| {
                let mut schema = JsonObj::new();
                schema.insert_str("type", Json::str("object"));
                schema.insert_str("properties", Json::Obj(JsonObj::new()));
                Json::Obj(schema)
            });
            let output = match tool.get("output") {
                None => ToolOutput::Void,
                Some(Json::Str(s)) if s.eq_str("nullOrString") => ToolOutput::NullOrString,
                Some(schema) => ToolOutput::Schema(schema.clone()),
            };
            tools.push(ToolDef::new(path.clone(), text(tool.get("description")), input, output));
            let canonical = path.clone();
            specs.insert(
                canonical,
                Rc::new(ToolSpec {
                    behavior: behavior(tool.get("behavior")),
                    yields: text(tool.get("timing")) == "yield",
                    calls: Cell::new(0),
                }),
            );
        }
    }
    let runtime = match Runtime::new(&tools) {
        Ok(runtime) => runtime,
        Err(message) => return Json::str(&message),
    };
    let specs = Rc::new(specs);
    let handler: opencode_codemode::ToolHandler = Rc::new(move |name, input| {
        let spec = specs.get(&name).cloned();
        Box::pin(async move {
            let Some(spec) = spec else { return ToolOutcome::Err(format!("no tool {name}")) };
            if spec.yields {
                YieldOnce(false).await;
            }
            match &spec.behavior {
                Behavior::Echo => ToolOutcome::Ok(input),
                Behavior::Value(value) => ToolOutcome::Ok(Some(value.clone())),
                Behavior::Fail(message) => ToolOutcome::Err(message.clone()),
                Behavior::Sequence(values) => {
                    let index = spec.calls.get().min(values.len().saturating_sub(1));
                    spec.calls.set(spec.calls.get() + 1);
                    match values.get(index) {
                        None => ToolOutcome::Ok(Some(Json::Null)),
                        Some(Ok(value)) => ToolOutcome::Ok(Some(value.clone())),
                        Some(Err(message)) => ToolOutcome::Err(message.clone()),
                    }
                }
            }
        })
    });
    let limits = match case.get("limits") {
        Some(Json::Obj(limits)) => Limits {
            timeout_ms: number(limits.get("timeoutMs")),
            max_tool_calls: number(limits.get("maxToolCalls")),
            max_output_bytes: number(limits.get("maxOutputBytes")),
        },
        _ => Limits::default(),
    };
    let bindings = match case.get("bindings") {
        Some(Json::Obj(bindings)) => bindings.iter().map(|(k, v)| (k.to_rust_lossy(), v.clone())).collect(),
        _ => Vec::new(),
    };
    let code = text(case.get("code"));
    let options = ExecuteOptions { limits, bindings, handler, hooks: Hooks::default() };
    match runtime.execute(&code, options).await {
        Execution::Done(result) => {
            let mut line = JsonObj::new();
            line.insert_str("name", case.get("name").cloned().unwrap_or(Json::Null));
            line.insert_str("result", result.to_json());
            line.insert_str("output", Json::Str(result.format_output()));
            Json::Obj(line)
        }
        Execution::Interrupted => {
            let mut line = JsonObj::new();
            line.insert_str("name", case.get("name").cloned().unwrap_or(Json::Null));
            line.insert_str("interrupted", Json::Bool(true));
            Json::Obj(line)
        }
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: codemode_run corpus.jsonl");
    let source = std::fs::read_to_string(&path).expect("readable corpus");
    // CODEMODE_STACK sets the thread's stack (bytes); the default is a tokio worker's 2 MiB, which the interpreter
    // must survive on any program.
    let stack = std::env::var("CODEMODE_STACK").ok().and_then(|s| s.parse().ok()).unwrap_or(2 * 1024 * 1024);
    let child = std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let mut out = std::io::BufWriter::new(std::io::stdout());
            for line in source.lines().filter(|line| !line.trim().is_empty()) {
                let units: Vec<u16> = line.encode_utf16().collect();
                let case = match json::parse(&units) {
                    Ok(Json::Obj(case)) => case,
                    _ => continue,
                };
                let name = case.get("name").cloned().unwrap_or(Json::Null);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(run_case(&case))))
                    .unwrap_or_else(|_| {
                        let mut line = JsonObj::new();
                        line.insert_str("name", name);
                        line.insert_str("panic", Json::Bool(true));
                        Json::Obj(line)
                    });
                use std::io::Write;
                let text: JsStr = result.to_js_string();
                writeln!(out, "{}", text.to_rust_lossy()).unwrap();
                out.flush().unwrap();
            }
        })
        .unwrap();
    child.join().unwrap();
}
