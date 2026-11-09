//! The execution boundary (codemode.ts `make`/`execute`, interpreter/execute.ts `executeProgram`): one program run
//! over prepared tools, producing the result exactly as the TS tool layer receives it.

use crate::interp::errors::{Diagnostic, DiagnosticKind, format_location, normalize_error};
use crate::interp::interp::Interp;
use crate::interp::{Loc, boxed};
use crate::json::{Json, JsonObj};
use crate::jsstr::{JsStr, JsStrBuf};
use crate::tools::{Hooks, Prepared, ToolDef, ToolHandler, ToolSession};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// Resource budgets enforced on each execution. Absent means unlimited.
#[derive(Clone, Copy, Debug, Default)]
pub struct Limits {
    /// Wall-clock milliseconds before interruption.
    pub timeout_ms: Option<u64>,
    /// Admitted tool calls (`search` included).
    pub max_tool_calls: Option<u64>,
    /// UTF-8 bytes retained from the result and logs; warnings get a separate equal budget.
    pub max_output_bytes: Option<u64>,
}

impl Limits {
    /// The TS validation: each limit must be a safe integer at or above its minimum.
    pub fn validate(&self) -> Result<(), String> {
        let check = |name: &str, value: Option<u64>, minimum: u64| match value {
            Some(v) if v < minimum || v > 9007199254740991 => {
                Err(format!("{name} must be a safe integer greater than or equal to {minimum}."))
            }
            _ => Ok(()),
        };
        check("timeoutMs", self.timeout_ms, 1)?;
        check("maxToolCalls", self.max_tool_calls, 0)?;
        check("maxOutputBytes", self.max_output_bytes, 0)
    }
}

/// The result of a program, as `CodeMode.Result`.
#[derive(Clone, Debug, PartialEq)]
pub struct CodeResult {
    /// The JSON-safe value on success, or the failure diagnostic.
    pub outcome: Result<Json, Diagnostic>,
    pub warnings: Vec<Diagnostic>,
    pub logs: Vec<JsStr>,
    pub truncated: bool,
    /// Names of the admitted tool calls, in order.
    pub tool_calls: Vec<String>,
}

/// How a run ended: a result, or interrupted by host work interrupted on its own (TS propagates the interruption).
#[derive(Debug)]
pub enum Execution {
    Done(CodeResult),
    Interrupted,
}

/// Tools prepared once per inventory, shared by every execution against it.
pub struct Runtime {
    prepared: Rc<Prepared>,
}

pub struct ExecuteOptions {
    pub limits: Limits,
    /// Read-only JSON values bound in the program's outer global scope.
    pub bindings: Vec<(String, Json)>,
    /// Runs each admitted tool call.
    pub handler: ToolHandler,
    /// Host globals, after the bindings (see `Extension::validate`).
    pub extensions: Vec<crate::extensions::Extension>,
    pub hooks: Hooks,
}

impl Runtime {
    pub fn new(tools: &[ToolDef]) -> Result<Runtime, String> {
        Ok(Runtime { prepared: Rc::new(Prepared::new(tools)?) })
    }

    /// `new`, with namespace registrations `(path, description)` placed before the tools (see
    /// `Prepared::with_namespaces`): a namespace with no visible tools is still a key of `tools`.
    pub fn with_namespaces(namespaces: &[(String, String)], tools: &[ToolDef]) -> Result<Runtime, String> {
        Ok(Runtime { prepared: Rc::new(Prepared::with_namespaces(namespaces, tools)?) })
    }

    /// Structured descriptions of every visible tool: `(path, description, signature)`.
    pub fn catalog(&self) -> Vec<(String, String, String)> {
        self.prepared
            .catalog()
            .into_iter()
            .map(|d| (d.path.clone(), d.description.clone(), d.signature().to_string()))
            .collect()
    }

    /// Runs one program. The future is single-threaded (`!Send`): await it inside a `tokio::task::LocalSet`
    /// (`spawn_local`) or a `block_on` (README.md, "Gluing it to the execute tool"). Dropping it cancels the run.
    pub async fn execute(&self, code: &str, options: ExecuteOptions) -> Execution {
        execute(self.prepared.clone(), code, options).await
    }
}

impl Diagnostic {
    /// The diagnostic as TS writes it: `kind`, `message`, then `location` and `suggestions` when present.
    pub fn to_json(&self) -> Json {
        diagnostic_json(self)
    }
}

fn diagnostic_json(diagnostic: &Diagnostic) -> Json {
    let mut out = JsonObj::new();
    out.insert_str("kind", Json::str(diagnostic.kind.as_str()));
    out.insert_str("message", Json::Str(diagnostic.message.clone()));
    if let Some(loc) = diagnostic.location {
        let mut location = JsonObj::new();
        location.insert_str("line", Json::Num(loc.line as f64));
        location.insert_str("column", Json::Num(loc.column as f64));
        out.insert_str("location", Json::Obj(location));
    }
    if let Some(suggestions) = &diagnostic.suggestions {
        out.insert_str("suggestions", Json::Arr(suggestions.iter().map(|s| Json::Str(s.clone())).collect()));
    }
    Json::Obj(out)
}

impl CodeResult {
    fn failure(diagnostic: Diagnostic) -> CodeResult {
        CodeResult { outcome: Err(diagnostic), warnings: Vec::new(), logs: Vec::new(), truncated: false, tool_calls: Vec::new() }
    }

    pub fn ok(&self) -> bool {
        self.outcome.is_ok()
    }

    /// The result object exactly as TS builds it (key order included).
    pub fn to_json(&self) -> Json {
        let mut out = JsonObj::new();
        out.insert_str("ok", Json::Bool(self.outcome.is_ok()));
        match &self.outcome {
            Ok(value) => {
                out.insert_str("value", value.clone());
                if !self.warnings.is_empty() {
                    out.insert_str("warnings", Json::Arr(self.warnings.iter().map(diagnostic_json).collect()));
                }
            }
            Err(error) => out.insert_str("error", diagnostic_json(error)),
        }
        if !self.logs.is_empty() {
            out.insert_str("logs", Json::Arr(self.logs.iter().map(|l| Json::Str(l.clone())).collect()));
        }
        if self.truncated {
            out.insert_str("truncated", Json::Bool(true));
        }
        let calls = self
            .tool_calls
            .iter()
            .map(|name| {
                let mut call = JsonObj::new();
                call.insert_str("name", Json::str(name));
                Json::Obj(call)
            })
            .collect();
        out.insert_str("toolCalls", Json::Arr(calls));
        Json::Obj(out)
    }

    /// The text core's `execute` tool hands to the model (core/src/codemode/tool.ts `formatResult`).
    pub fn format_output(&self) -> JsStr {
        let output = match &self.outcome {
            Ok(Json::Str(text)) => text.clone(),
            Ok(value) => crate::json::stringify(value, None, &[b' ' as u16, b' ' as u16]),
            Err(error) => {
                let mut parts: Vec<JsStr> = vec![error.message.clone()];
                for hint in error.suggestions.iter().flatten() {
                    if error.message.index_of(hint.units(), 0).is_none() {
                        parts.push(hint.clone());
                    }
                }
                let mut joined = JsStrBuf::new();
                for (index, part) in parts.iter().enumerate() {
                    if index > 0 {
                        joined.push_unit(b'\n' as u16);
                    }
                    joined.push_js(part);
                }
                js_trim(&joined.finish())
            }
        };
        let mut sections: Vec<JsStr> = Vec::new();
        if !output.is_empty() {
            sections.push(output);
        }
        if self.outcome.is_ok() && !self.warnings.is_empty() {
            let mut text = JsStrBuf::new();
            text.push_str("Warnings:");
            for warning in &self.warnings {
                text.push_str(&format!("\n- [{}] ", warning.kind.as_str()));
                text.push_js(&warning.message);
            }
            sections.push(text.finish());
        }
        if !self.logs.is_empty() {
            let mut text = JsStrBuf::new();
            text.push_str("Logs:");
            for line in &self.logs {
                text.push_unit(b'\n' as u16);
                text.push_js(line);
            }
            sections.push(text.finish());
        }
        let mut out = JsStrBuf::new();
        for (index, section) in sections.iter().enumerate() {
            if index > 0 {
                out.push_str("\n\n");
            }
            out.push_js(section);
        }
        out.finish()
    }
}

pub(crate) fn js_trim(text: &JsStr) -> JsStr {
    let units = text.units();
    let ws = crate::stdlib::string::is_js_whitespace;
    let start = units.iter().position(|u| !ws(*u)).unwrap_or(units.len());
    let end = units.iter().rposition(|u| !ws(*u)).map(|i| i + 1).unwrap_or(start);
    text.slice(start, end.max(start))
}

fn parse_failure(message: &str, line: u32, column: u32) -> Diagnostic {
    let location = Loc { line, column: column + 1 };
    // Acorn appends ` (line:column)` to its messages.
    let reason = match message.rfind(" (") {
        Some(at) if message.ends_with(')') && message[at + 2..message.len() - 1].split(':').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) && message[at + 2..message.len() - 1].contains(':') => &message[..at],
        _ => message,
    };
    let text = format!(
        "{}{} Code must be plain JavaScript written as a program body.",
        reason,
        if reason.ends_with('.') { "" } else { "." }
    );
    Diagnostic {
        kind: DiagnosticKind::ParseError,
        message: JsStr::from(format!("SyntaxError: {text}{}", format_location(Some(location)))),
        location: Some(location),
        suggestions: None,
    }
}

pub(crate) const PARSER_STACK: usize = 8 * 1024 * 1024;

/// Frees a tree: one as deep as the parser allows is freed recursively, so it gets the parser's stack too.
pub(crate) fn drop_program<T>(program: T) {
    stacker::grow(4 * PARSER_STACK, move || drop(program));
}

/// Frees its value as `drop_program` does, also when the execution holding it is dropped early.
struct OnGrownStack<T>(Option<T>);

impl<T> Drop for OnGrownStack<T> {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            drop_program(value);
        }
    }
}

/// acorn's parse plus the export check (parseProgram).
pub fn parse_program(code: &str) -> Result<crate::parser::ast::Program, Diagnostic> {
    let units: Vec<u16> = code.encode_utf16().collect();
    // acorn gives up at about 3,900 nested parentheses under bun; 8 MiB of parser stack reaches about as deep.
    let parsed = stacker::grow(PARSER_STACK + 1024 * 1024, || crate::parser::parse_with_stack_limit(&units, PARSER_STACK));
    let program = parsed.map_err(|error| {
        // acorn exhausting the JS stack is a RangeError, not a SyntaxError: it fails the execution as a defect.
        if error.message == crate::parser::STACK_OVERFLOW_MESSAGE {
            return Diagnostic::new(DiagnosticKind::ExecutionFailure, "Execution exceeded the maximum nesting depth.");
        }
        parse_failure(&error.message, error.loc.line, error.loc.column)
    })?;
    use crate::parser::ast::Stmt;
    if let Some(exported) =
        program.body.iter().find(|s| matches!(s, Stmt::ExportNamed(_) | Stmt::ExportDefault(_) | Stmt::ExportAll(_)))
    {
        let location = Loc::of(exported.span());
        drop_program(program);
        return Err(Diagnostic {
            kind: DiagnosticKind::ParseError,
            message: JsStr::from(format!(
                "SyntaxError: Exports aren't available. Return the result instead.{}",
                format_location(Some(location))
            )),
            location: Some(location),
            suggestions: None,
        });
    }
    Ok(program)
}

fn utf8_len(text: &JsStr) -> usize {
    text.to_rust_lossy().len()
}

// Drop a replacement character produced by truncating inside a UTF-8 sequence.
fn utf8_truncate(text: &JsStr, max_bytes: usize) -> JsStr {
    let rust = text.to_rust_lossy();
    let bytes = rust.as_bytes();
    if bytes.len() <= max_bytes {
        return text.clone();
    }
    let decoded = String::from_utf8_lossy(&bytes[..max_bytes]).into_owned();
    let decoded = decoded.strip_suffix('\u{FFFD}').map(String::from).unwrap_or(decoded);
    JsStr::from(decoded)
}

// Warnings have a separate budget so result data cannot starve diagnostics.
fn bound_output(result: CodeResult, max: usize) -> CodeResult {
    let mut truncated = false;
    let mut value = Json::Null;
    let mut value_bytes = 0;
    if let Ok(original) = &result.outcome {
        let serialized = original.to_js_string();
        let bytes = utf8_len(&serialized);
        if bytes > max {
            truncated = true;
            let mut text = JsStrBuf::new();
            text.push_js(&utf8_truncate(&serialized, max));
            text.push_str(&format!(
                " [result truncated: {bytes} bytes exceeds the {max}-byte output limit; return a smaller value]"
            ));
            value = Json::Str(text.finish());
            value_bytes = max;
        } else {
            value = original.clone();
            value_bytes = bytes;
        }
    }
    let warnings = if result.outcome.is_ok() { result.warnings.clone() } else { Vec::new() };
    let mut kept_warnings = Vec::new();
    let mut warning_bytes = 0;
    for warning in &warnings {
        let bytes = utf8_len(&diagnostic_json(warning).to_js_string()) + 1;
        if warning_bytes + bytes > max {
            break;
        }
        warning_bytes += bytes;
        kept_warnings.push(warning.clone());
    }
    if kept_warnings.len() < warnings.len() {
        truncated = true;
        kept_warnings.push(Diagnostic::new(
            DiagnosticKind::Truncated,
            format!("{} additional warnings omitted by the output limit.", warnings.len() - kept_warnings.len()),
        ));
    }
    let mut kept_logs = Vec::new();
    let budget = max.saturating_sub(value_bytes);
    let mut log_bytes = 0;
    for line in &result.logs {
        let bytes = utf8_len(line) + 1;
        if log_bytes + bytes > budget {
            break;
        }
        log_bytes += bytes;
        kept_logs.push(line.clone());
    }
    if kept_logs.len() < result.logs.len() {
        truncated = true;
        kept_logs.push(JsStr::from(format!("[logs truncated: showing {} of {} lines]", kept_logs.len(), result.logs.len())));
    }
    if !truncated {
        return result;
    }
    CodeResult {
        outcome: match result.outcome {
            Ok(_) => Ok(value),
            Err(error) => Err(error),
        },
        warnings: if result_ok(&warnings) { kept_warnings } else { kept_warnings },
        logs: kept_logs,
        truncated: true,
        tool_calls: result.tool_calls,
    }
}

fn result_ok(_: &[Diagnostic]) -> bool {
    true
}

pub async fn execute(prepared: Rc<Prepared>, code: &str, options: ExecuteOptions) -> Execution {
    if js_trim(&JsStr::from(code)).is_empty() {
        return Execution::Done(CodeResult::failure(Diagnostic::new(DiagnosticKind::ParseError, "Code cannot be empty.")));
    }
    let result = match parse_program(code) {
        Err(diagnostic) => CodeResult::failure(diagnostic),
        Ok(program) => {
            let program = OnGrownStack(Some(Rc::new(program)));
            let outcome = run_program(prepared, program.0.clone().unwrap(), &options).await;
            drop(program);
            match outcome {
                None => return Execution::Interrupted,
                Some(result) => result,
            }
        }
    };
    Execution::Done(match options.limits.max_output_bytes {
        None => result,
        Some(max) => bound_output(result, max as usize),
    })
}

async fn run_program(prepared: Rc<Prepared>, program: Rc<crate::parser::ast::Program>, options: &ExecuteOptions) -> Option<CodeResult> {
    let ids: Rc<std::cell::Cell<crate::tools::CallId>> = Default::default();
    let session = Rc::new(ToolSession::new(
        prepared,
        options.limits.max_tool_calls,
        options.handler.clone(),
        options.hooks.clone(),
        ids.clone(),
    ));
    let calls = session.calls.clone();
    let logs: Rc<RefCell<Vec<JsStr>>> = Default::default();
    let bindings = options.bindings.clone();
    let extensions = options.extensions.clone();
    let hooks = options.hooks.clone();
    let ctx = Interp::new(session, logs.clone(), move |ctx| {
        let mut globals: Vec<(JsStr, crate::interp::value::Value)> = bindings
            .iter()
            .map(|(name, value)| (JsStr::from(name.as_str()), crate::interp::data::from_json(ctx, value)))
            .collect();
        globals.extend(crate::extensions::extension_globals(ctx, &extensions, hooks.extension_start, hooks.extension_end, ids));
        globals
    });
    // Breaks the run's object cycles however it ends, including when the caller drops the execution early: its host
    // work is dropped too, so hooks see the calls in flight end as interrupted. What it frees can hold deep syntax
    // trees (function bodies), so it runs on the parser's stack size.
    struct Disposal(crate::interp::Ctx);
    impl Drop for Disposal {
        fn drop(&mut self) {
            let ctx = &self.0;
            stacker::grow(4 * PARSER_STACK, || ctx.dispose());
        }
    }
    let _disposal = Disposal(ctx.clone());
    let main = {
        let (ctx, program) = (ctx.clone(), program.clone());
        async move {
            let frame = crate::interp::eval::Frame::new(&ctx, vec![ctx.global_scope.clone()]);
            let value = frame.run_program(&program).await?;
            let json = crate::interp::data::to_boundary(&ctx, value).await?;
            Ok(crate::interp::value::Value::Obj({
                let holder = ctx.new_plain();
                // Carried out through a holder so the copied JSON survives the boundary settle.
                *ctx.main_result.borrow_mut() = Some(json.unwrap_or(Json::Null));
                holder
            }))
        }
    };
    let main = crate::interp::runner::activation(&ctx, boxed(main));
    let returned: Rc<RefCell<Option<Json>>> = Default::default();
    let run = {
        let (ctx, returned) = (ctx.clone(), returned.clone());
        async move {
            let outcome = ctx.runner.run(&ctx, main).await;
            match outcome {
                Err(error) => Err(error),
                Ok(Err(error)) => Ok(Err(error)),
                Ok(Ok(_)) => {
                    let value = ctx.main_result.borrow_mut().take().unwrap_or(Json::Null);
                    *returned.borrow_mut() = Some(value.clone());
                    // Host work the program left running is interrupted, not awaited.
                    ctx.runner.interrupt();
                    Ok(Ok(value))
                }
            }
        }
    };
    let logged = |logs: &Rc<RefCell<Vec<JsStr>>>| logs.borrow().clone();
    let outcome = match options.limits.timeout_ms {
        None => Some(run.await),
        Some(ms) => tokio::time::timeout(Duration::from_millis(ms), run).await.ok(),
    };
    let result = match outcome {
        None => {
            let ms = options.limits.timeout_ms.unwrap_or_default();
            ctx.runner.interrupt();
            let diagnostic = Diagnostic::new(DiagnosticKind::TimeoutExceeded, format!("Execution timed out after {ms}ms."));
            match returned.borrow().clone() {
                None => Some(CodeResult {
                    outcome: Err(diagnostic),
                    warnings: Vec::new(),
                    logs: logged(&logs),
                    truncated: false,
                    tool_calls: calls.borrow().clone(),
                }),
                Some(value) => {
                    let mut warnings = vec![Diagnostic::new(
                        DiagnosticKind::TimeoutExceeded,
                        format!("The program returned, but background work was still running at the {ms}ms timeout and was interrupted. Await all started promises."),
                    )];
                    warnings.extend(ctx.runner.diagnostics(&ctx));
                    Some(CodeResult { outcome: Ok(value), warnings, logs: logged(&logs), truncated: false, tool_calls: calls.borrow().clone() })
                }
            }
        }
        Some(Ok(Ok(value))) => {
            let warnings = ctx.runner.diagnostics(&ctx);
            Some(CodeResult { outcome: Ok(value), warnings, logs: logged(&logs), truncated: false, tool_calls: calls.borrow().clone() })
        }
        Some(Ok(Err(error))) | Some(Err(error)) => {
            ctx.runner.interrupt();
            if error.is_interrupt() {
                None
            } else {
                Some(CodeResult {
                    outcome: Err(normalize_error(&ctx, &error)),
                    warnings: Vec::new(),
                    logs: logged(&logs),
                    truncated: false,
                    tool_calls: calls.borrow().clone(),
                })
            }
        }
    };
    result
}
