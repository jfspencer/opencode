//! Host work interrupted on its own (core ends a declined call this way): no `catch`, `finally` or rejection handler
//! sees it, and reaching the program's body it interrupts the execution. harness/corpus/interrupts.jsonl compares the
//! cases with TS.

use opencode_codemode::json::{Json, JsonObj};
use opencode_codemode::{
    ExecuteOptions, Execution, Extension, HostFunction, HostOutcome, HostReturn, HostValue, Hooks, Limits, Runtime,
    ToolDef, ToolHandler, ToolOutcome, ToolOutput,
};
use std::cell::RefCell;
use std::rc::Rc;

/// Runs `code` over a `mark` tool that records its `at`, a `stop` tool and a `halt` global that end interrupted.
async fn run(code: &str) -> (Execution, Vec<String>, Vec<String>) {
    let mut schema = JsonObj::new();
    schema.insert_str("type", Json::str("object"));
    let tool = |path: &str| ToolDef::new(path, "", Json::Obj(schema.clone()), ToolOutput::Void);
    let runtime = Runtime::new(&[tool("mark"), tool("stop")]).unwrap();
    let marks: Rc<RefCell<Vec<String>>> = Default::default();
    let recorded = marks.clone();
    let handler: ToolHandler = Rc::new(move |path, input| {
        let recorded = recorded.clone();
        Box::pin(async move {
            if path == "stop" {
                return ToolOutcome::Interrupted;
            }
            let at = input.as_ref().and_then(|input| input.as_obj()?.get("at")?.as_str().map(|s| s.to_rust_lossy()));
            recorded.borrow_mut().push(at.unwrap_or_default());
            ToolOutcome::Ok(None)
        })
    });
    let ends: Rc<RefCell<Vec<String>>> = Default::default();
    let (tool_ends, extension_ends) = (ends.clone(), ends.clone());
    let hooks = Hooks {
        tool_end: Some(Rc::new(move |_, name, status| tool_ends.borrow_mut().push(format!("{name} {status:?}")))),
        extension_end: Some(Rc::new(move |_, _, name, status| extension_ends.borrow_mut().push(format!("{name} {status:?}")))),
        ..Hooks::default()
    };
    let halt = HostValue::Function(HostFunction {
        name: "halt".into(),
        length: 0,
        asynchronous: true,
        call: Rc::new(|_| HostReturn::Async(Box::pin(async { HostOutcome::Interrupted }))),
    });
    let options = ExecuteOptions {
        limits: Limits::default(),
        bindings: Vec::new(),
        handler,
        hooks,
        extensions: vec![Extension { name: "test".into(), globals: vec![("halt".into(), halt)] }],
    };
    let execution = runtime.execute(code, options).await;
    let (marks, ends) = (marks.borrow().clone(), ends.borrow().clone());
    (execution, marks, ends)
}

const GUARDED: &str = r#"try { await CALL } catch (e) { await tools.mark({ at: "catch" }) } finally { await tools.mark({ at: "finally" }) }
return await tools.mark({ at: "after" })"#;

#[tokio::test(flavor = "current_thread")]
async fn an_interrupted_call_ends_the_program_uncaught() {
    for (call, ended) in [("tools.stop()", "stop Interrupted"), ("halt()", "halt Interrupted")] {
        let (execution, marks, ends) = run(&GUARDED.replace("CALL", call)).await;
        assert!(matches!(execution, Execution::Interrupted), "{call}");
        assert_eq!(marks, Vec::<String>::new(), "{call}");
        assert_eq!(ends, [ended], "{call}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_settled_promise_combinator_does_not_observe_an_interrupt() {
    let (execution, marks, _) =
        run("const settled = await Promise.allSettled([halt()])\nawait tools.mark({ at: settled[0].status })").await;
    assert!(matches!(execution, Execution::Interrupted));
    assert_eq!(marks, Vec::<String>::new());
}

#[tokio::test(flavor = "current_thread")]
async fn an_unobserved_interrupt_is_not_reported() {
    let (execution, marks, _) = run("halt()\ntools.stop()\nawait tools.mark({ at: \"after\" })\nreturn 1").await;
    let Execution::Done(result) = execution else { panic!("the program returns") };
    assert_eq!(result.outcome, Ok(Json::Num(1.0)));
    assert!(result.warnings.is_empty());
    assert_eq!(marks, ["after"]);
}
