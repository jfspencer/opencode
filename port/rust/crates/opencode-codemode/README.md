# opencode-codemode

This crate runs code mode, the confined JavaScript-like language behind opencode's `execute` tool. It is a port of
`packages/codemode` with the default engine (`"legacy"`). The experimental bytecode VM (`src/vm`,
`experimental.codemode_vm`) is not ported.

The crate covers:

- parsing (an acorn 8.15 port) and the checks that run before execution
- the interpreter and its standard library
- the data boundary
- the tool runtime: tree, signatures, `search`, limits and hooks
- host extensions
- output bounding
- `scanTools`
- the text core's `execute` tool gives the model

It has no server dependencies. Its futures are single-threaded and can be driven by a current-thread tokio
runtime.

Fidelity is checked against the TS package, never against hand-written expectations. With `harness/check.sh` (see
harness/README.md), the 2,009 execution cases and the `scanTools` pass over every corpus program (2,091) show
**0 differences**. Only text that JavaScriptCore itself writes, such as `JSON.parse` diagnostics, is masked as
`<engine message>`. Everything the interpreter writes matches byte for byte, including JS number-to-string
conversion.

## API

```rust
use opencode_codemode::{Execution, ExecuteOptions, Hooks, Json, Limits, Runtime, ToolDef, ToolOutcome, ToolOutput};

// Once per tool inventory (TS `CodeMode.make`): builds the tool tree, signatures, and search index.
let runtime = Runtime::new(&[ToolDef::new("files.read", "Read a file", input_schema, ToolOutput::NullOrString)])?;
runtime.catalog(); // [(path, description, signature)], TS `runtime.catalog`

// Once per program (TS `runtime.execute(code, bindings)`).
let execution = runtime
    .execute(code, ExecuteOptions {
        limits: Limits::default(), // timeout_ms, max_tool_calls, max_output_bytes; Limits::validate gives TS's errors
        bindings: vec![],          // read-only JSON globals
        handler,                   // runs each admitted tool call
        hooks: Hooks::default(),   // observers around tool and extension calls
        extensions: vec![],        // host globals such as `fetch`
    })
    .await;
match execution {
    Execution::Done(result) => {
        result.to_json();       // CodeMode.Result, keys in TS's order
        result.format_output(); // the model's text (core's formatResult)
        result.ok();            // core sets `error: true` when this is false
    }
    // A handler returned ToolOutcome::Interrupted. TS fails the effect with an interrupt.
    Execution::Interrupted => {}
}
```

- **Tools.** `ToolDef { path, description, input, output, decode_input, signature, namespace_description }`.
  - `path` is the dotted canonical path (core's `qualifiedName`).
  - `input` is a render-only JSON Schema.
  - `output` is `ToolOutput::Schema(json)` (render-only), `ToolOutput::NullOrString` (core's default
    `Schema.NullOr(Schema.String)`, which is validated), or `ToolOutput::Void`.
  - `decode_input` optionally validates or decodes the input before the call is admitted.
  - `namespace_description` is `(namespace path, text)`, from `Namespace.make`.

  The handler is `Rc<dyn Fn(path, Option<Json>) -> Fut<ToolOutcome>>`. It returns one of:
  - `ToolOutcome::Ok(value)`, where `None` is `undefined`.
  - `ToolOutcome::Err(message)`: the program catches it as the tool's failure with exactly that text.
  - `ToolOutcome::Interrupted`.

  Output decoding, the JSON round trip, `toolCalls`, and the call limit all work as in TS.
- **Hooks.** `Hooks { tool_start, tool_end, extension_start, extension_end }` correspond to `tool.before`/`after`
  and `extension.before`/`after`. Each receives a `CallId`, so an end finds its start the way TS hands both hooks
  the same call object. Every started call ends exactly once. A call still in flight when the execution ends, or
  when it is dropped, ends `Interrupted`. These hooks only observe: unlike TS's `before`, they cannot deny a call.
- **Extensions.** `Extension { name, globals: [(name, HostValue)] }` corresponds to `Extension.make`.
  - A global is a `HostValue::Function(HostFunction { name, length, asynchronous, call })` or an `Object` of them.
  - `asynchronous` is TS's `Extension.async`/`AsyncFunction`: a call always returns a promise.
  - `call` returns `HostReturn::Value(Ok | Err)` now, or `HostReturn::Async(future)`.

  Values cross as TS converts them: data, Date, RegExp, Uint8Array, URL, URLSearchParams, Headers, Map, Set,
  errors with their fields, and functions. Depth, circularity, and opaque-value errors carry TS's text.
  `Extension::validate` gives `CodeMode.make`'s shadowing errors.

  An async future is polled once at the call, as an Effect starts synchronously. If it is ready then, it settles
  before the program's next job. A JS Promise always settles at least one host turn later.
- **`scan::scan_tools(code)`** is `CodeMode.scanTools`, for schedule checks. It returns
  `Ok(paths in first-seen order)` or `Err(Diagnostic)`.
- **Helpers for instructions** (core/src/codemode/instructions.ts): `tools::SEARCH_SIGNATURE` (TS
  `searchSignature`) and `tools::tool_expression(path)` (TS `toolExpression`).
- **Values.** `json::Json` (JSON with JS property order) and `jsstr::JsStr` (UTF-16, lone surrogates allowed) are
  the boundary types. `Json::to_js_string()` is `JSON.stringify`, and `json::parse` is `JSON.parse`.

## Gluing it to the execute tool

This section maps core/src/codemode/tool.ts onto the crate:

- **Runtime per inventory.** Build one `Runtime` per tool inventory snapshot, as core caches one per `Inventory`.
  Each registration becomes a `ToolDef`:
  - path: `qualifiedName(registration)`
  - input: `inputSchema`
  - output: `outputSchema` as `ToolOutput::Schema`, otherwise `ToolOutput::NullOrString`
  - each namespace's description goes in `namespace_description`

  `runtime.catalog()` feeds `CodeModeTool.catalog` and the instructions. That module is in core, so it is not part
  of this crate.
- **Handler.** The handler runs the registered tool the way core's `executeTool` does:
  - It returns `ToolOutcome::Ok(Some(resultValue(executed)))`: the structured `output` if it has one, otherwise the
    text parts joined with `"\n"`, or `null` when that is empty.
  - A failure returns `ToolOutcome::Err(toSessionError(error).message)`.
  - The handler also collects output files in call order, as core's `files` ref does.
- **Hooks.** The progress rows (`progressHooks`) map to the hooks:
  - `tool_start` adds a `running` row with `displayInput(input)`.
  - `extension_start` adds a row only for `fetch`, with `CodeModeFetch.display(args)`.
  - Each end sets that call's row to `completed` (success) or `error`.
- **Extensions and result.** `fetch` (core/src/codemode/fetch.ts) belongs to the server. It becomes one
  `Extension { name: "fetch", globals: [("fetch", async HostFunction)] }`, whose call returns a `Response`-like
  `HostValue::Object` of functions. The tool's text is `result.format_output()`, and its `error: true` follows
  `!result.ok()`.
- **Schedule checks.** `CodeModeTool.check` is the same `execute` with the check's `limits` and `bindings`, over an
  inventory filtered to `scan_tools`' paths.
- **Threading.** `Runtime` and the execution future are `!Send` (`Rc` throughout). The server's `tokio::spawn`
  needs `Send`, so there are two options:
  - Run the server's current-thread runtime inside a `tokio::task::LocalSet` and start each execution with
    `spawn_local`. The handler forwards calls to the session's tool executor over a channel, and the result
    leaves the local task as `Send` data (the text, the rows, the files).
  - Give executions a dedicated thread with its own current-thread runtime.

  Both directions can carry JSON as text (`Json::to_js_string`, `json::parse`).
- **Cancellation and stack.** Dropping the execution future cancels the run: host work is dropped, the hooks see
  `Interrupted`, and the object graph is freed. The interpreter needs only a 2 MiB stack (a tokio worker's
  default), because deep recursion grows its stack on demand (`stacker`).

## Layout

| module | TS source | role |
| --- | --- | --- |
| `api.rs` | `codemode.ts`, `interpreter/execute.ts` | `Runtime`, limits, parsing checks, timeout, diagnostics, `boundOutput`, result JSON and text |
| `parser/` | acorn 8.15 | tokenizer, statements, expressions, patterns, regexp syntax validation, the ESTree tree (`ast.rs`) |
| `interp/` | `interpreter/*.ts` | the evaluator (see below) |
| `stdlib/` | `stdlib/*.ts` | built-ins: Object, Array, String, Number, Math, JSON, Date, RegExp, Map/Set/Weak*, iterators, Promise statics, URL, URLSearchParams, Headers, Uint8Array, TextEncoder/Decoder, structuredClone, console |
| `tools/` | `tool-runtime.ts`, `tool-schema.ts`, `tools.ts`, `namespace.ts` | the tool tree, path resolution with "Did you mean", JSON Schema rendered as TypeScript signatures, `search`, the tool session |
| `extensions.rs` | `interpreter/extensions.ts`, `extension.ts` | host globals and conversion across the boundary |
| `scan.rs` | `scan.ts` | `scanTools` |
| `numconv.rs` | JavaScriptCore | Number to string (shortest round trip, radix, `toFixed`/`toExponential`/`toPrecision`, en-US `toLocaleString`) and string to number |
| `jsregex/` | JavaScriptCore Yarr | the RegExp engine: syntax and errors, `u`/`v`/`i`/`s`/`m`/`y`/`d` flags, named groups, modifiers, backtracking VM with JSC's limits |
| `jsdate/` | JavaScriptCore + ICU | Date: time math, parsing, formatting, local time zones from the system tz database |
| `json.rs`, `jsstr.rs` | | JSON with JS key order and JSC's parse messages; UTF-16 strings |

The evaluator in `interp/`:

- Each activation (the program body, a function call, a promise reaction, a tool call's argument crossing) is a
  Rust future.
- An `await`, a `yield`, a blocking host call, or a time slice suspends the future with a `Signal`. The nearest
  boundary that can handle the signal takes it, at the points where TS splits its explicit activation stack.
- `runner.rs` is TS's `Runner`. It keeps the job queue, the inbox of host completions in completion order,
  stuck-await detection, and unhandled rejections.
- JavaScriptCore's host-stack overflow is emulated with budgets: call depth, native nesting, and expression
  nesting. Programs that overflow in TS fail with the same "maximum nesting depth" diagnostic.
- `dispose` breaks object cycles at the end of a run. Freeing an object does not recurse.

## Fidelity notes and known gaps

- **Host stack limits are approximate.** Some programs overflow the stack in TS: deep recursion, expressions
  nested about 24,000 `+` or `.b` deep (7,250 for chained calls), the tree walks for `arguments` and loop closures
  past about 15,000 levels, `scanTools` past about 9,100, and flattening more than about 12,000 nested arrays.
  Rust fails these the same way (with a RangeError the program can catch where TS's is catchable), but at
  thresholds calibrated from bun with deterministic budgets.
  TS's own thresholds move with JIT warm-up, by as much as 17,000 to 30,000 for `+`, so the corpus stays clear of
  them.
- **No build-dependent limits.** Chains the parser builds in a loop (`a + b + …`, `a.b.c()`) parse at any length,
  as acorn's do. Their trees are freed one link at a time. Statement evaluation, patterns, and every recursive
  walk grow their stack on demand, so a debug build on a 2 MiB stack gives the same results as release, and no
  program crashes the process.
- **Between about 2,500 and 5,000 nested levels** of brackets, statements, or patterns, acorn and the interpreter
  run out of JS stack in TS. Rust's parser limit (8 MiB of parser stack) is not exact there. A stress run of 136
  such programs found 10 that differ in that band, for example `((…1…))` nested 2,500 deep: TS overflowed after
  JIT warm-up, while Rust evaluates it. At 1,000 and at 20,000 levels the results match (`deep.jsonl`).
- **`localeCompare`** approximates ICU root collation: ASCII punctuation order, common expansions, and accents as
  secondary differences. It matches on every corpus case, but it is not the full collation algorithm.
- **Dates outside UTC** use the system tz database, which can be newer than bun's ICU. The conformance suite runs
  in UTC, where the Date check finds 0 differences.
- **URL** uses the `url` crate (WHATWG), adjusted to the WebKit behavior the corpus exercises. Rare edge cases can
  still differ from WebKit.
- **Cleanup timeout warning.** In TS, a program can return while host cleanup outlasts the timeout, which gives
  the warning "background work was still running at the timeout". Dropping a Rust future is instantaneous, so
  that warning never appears.
- **Hooks cannot deny calls**, and an end hook gets only the status, not the value or error. Core's hooks only
  record progress.
- **Effect Schema inputs are not interpreted.** Tools declared with Effect Schema decoders (`Tool.make` with a
  Schema) would need `decode_input`. Core passes JSON Schemas, which are render-only, and the default output
  `NullOr(String)` is ported.
- **Program source is `&str`**, so it cannot hold lone surrogates. String values can.
- **Upstream tests not replayed.** The 412 upstream executions with tools, extensions, or hooks are not replayed
  as recorded. Their behaviors are covered by the `tools`, `extensions`, and `hooks` corpora instead.

## TS behavior worth fixing upstream (matched here)

These are bugs in `scanTools`. Both make it miss calls the program then makes:

- **A `var` in a class static block is treated as program-scoped.** For
  `await tools.danger({}); x = class { static { var tools } }`, `scanTools` returns `[]`, but `tools.danger`
  runs. The class is rejected only after the call.
- **A body `var` is treated as visible to parameter defaults.** For
  `const f = function (a = tools.danger({})) { var tools }; await f()`, `scanTools` returns `[]` and the call
  runs.

Core limits the impact: a schedule check runs over only the scanned paths (`Tool.check` filters the inventory),
so a missed call fails as an unknown tool instead of reaching it.

`scanTools` also promises first-seen order, but a `switch` case's statements are visited before its test.
`switch (tools.a()) { case tools.b(): tools.c() }` gives `["a", "c", "b"]`.

## Memory

A value is two words (`Value` is 16 bytes, checked by a test), and strings are thin UTF-16 buffers. Running a
typical corpus program, the whole runner process peaks at about 6 MB (median per case). Programs written to hit
the 10,000,000-element array limit, such as `[Array(1e7).fill(0), [1]].flat()`, peak at about 160 MB, because
`concat` and `flat` check the result length before building it. All 1,483 upstream cases in one process peak at
190 MB.

Memory freed after a heavy run stays with the allocator. A long-lived server may want `malloc_trim` or a
different allocator.

## Dependencies

| crate | for |
| --- | --- |
| `indexmap` | property order |
| `ryu-js` | shortest number formatting, as JSC does it |
| `stacker` | growing the stack for deep programs |
| `triomphe` | thin string pointers |
| `unicode-normalization` | `String.prototype.normalize` |
| `url` | URL parsing |
| `tokio` | timeouts |
| `getrandom` | `Math.random`, `crypto.randomUUID`, `crypto.getRandomValues` |
