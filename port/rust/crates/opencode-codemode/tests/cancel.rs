//! Dropping an execution early (the caller cancelling it, as a session abort does) ends the calls in flight: their
//! host work is dropped and the hooks see them end as interrupted, as TS's `after` hooks do when the fiber is
//! interrupted.

use opencode_codemode::json::{Json, JsonObj};
use opencode_codemode::{
    ExecuteOptions, Extension, HostFunction, HostReturn, HostValue, Hooks, Limits, Runtime, ToolDef,
    ToolHandler, ToolOutput,
};
use std::cell::RefCell;
use std::rc::Rc;

#[tokio::test(flavor = "current_thread")]
async fn dropping_an_execution_interrupts_its_calls() {
    let mut schema = JsonObj::new();
    schema.insert_str("type", Json::str("object"));
    let runtime = Runtime::new(&[ToolDef::new("wait", "", Json::Obj(schema), ToolOutput::Void)]).unwrap();
    let handler: ToolHandler = Rc::new(|_, _| Box::pin(std::future::pending()));
    let events: Rc<RefCell<Vec<String>>> = Default::default();
    let record = |events: &Rc<RefCell<Vec<String>>>| {
        let events = events.clone();
        move |text: String| events.borrow_mut().push(text)
    };
    let (start, end, ext_start, ext_end) = (record(&events), record(&events), record(&events), record(&events));
    let hooks = Hooks {
        tool_start: Some(Rc::new(move |id, name, _| start(format!("start {id} {name}")))),
        tool_end: Some(Rc::new(move |id, name, status| end(format!("end {id} {name} {status:?}")))),
        extension_start: Some(Rc::new(move |id, _, name, _| ext_start(format!("start {id} {name}")))),
        extension_end: Some(Rc::new(move |id, _, name, status| ext_end(format!("end {id} {name} {status:?}")))),
    };
    let never = HostValue::Function(HostFunction {
        name: "never".into(),
        length: 0,
        asynchronous: true,
        call: Rc::new(|_| HostReturn::Async(Box::pin(std::future::pending()))),
    });
    let options = ExecuteOptions {
        limits: Limits::default(),
        bindings: Vec::new(),
        handler,
        hooks,
        extensions: vec![Extension { name: "test".into(), globals: vec![("never".into(), never)] }],
    };
    let execution = runtime.execute("const a = never(); await tools.wait({}); await a", options);
    let finished = tokio::time::timeout(std::time::Duration::from_millis(20), execution).await;
    assert!(finished.is_err(), "the execution waits forever");
    let mut seen = events.borrow().clone();
    // Both calls end; the order they are dropped in is not specified.
    seen[2..].sort();
    assert_eq!(seen, ["start 0 never", "start 1 wait", "end 0 never Interrupted", "end 1 wait Interrupted"]);
}
