//! Code mode: the confined JavaScript-like interpreter behind opencode's `execute` tool, ported from
//! packages/codemode (the default AST interpreter, `src/interpreter`, and its standard library). See README.md.

pub mod api;
pub mod extensions;
pub mod interp;
pub mod jsdate;
pub mod jsregex;
pub mod json;
pub mod jsstr;
pub mod numconv;
pub mod parser;
pub mod scan;
pub mod stdlib;
pub mod tools;

pub use extensions::{Extension, ExtensionEndHook, ExtensionHook, HostError, HostFunction, HostOutcome, HostReturn, HostValue};
pub use api::{CodeResult, ExecuteOptions, Execution, Limits, Runtime};
pub use interp::errors::{Diagnostic, DiagnosticKind};
pub use json::{Json, JsonObj};
pub use jsstr::JsStr;
pub use tools::{CallId, CallStatus, Hooks, ToolDef, ToolHandler, ToolOutcome, ToolOutput};
