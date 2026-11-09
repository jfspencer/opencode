//! Failures and diagnostics (model.ts, errors.ts): what interpreter work throws, how a handler sees it, and how an
//! uncaught one is reported.

use crate::interp::object::{define_str, get};
use crate::interp::value::*;
use crate::interp::{Ctx, Loc};
use crate::jsstr::{JsStr, JsStrBuf};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorType {
    Error,
    TypeError,
    RangeError,
    SyntaxError,
    ReferenceError,
    EvalError,
    URIError,
    AggregateError,
}

pub const ERROR_TYPES: [ErrorType; 8] = [
    ErrorType::Error,
    ErrorType::TypeError,
    ErrorType::RangeError,
    ErrorType::SyntaxError,
    ErrorType::ReferenceError,
    ErrorType::EvalError,
    ErrorType::URIError,
    ErrorType::AggregateError,
];

impl ErrorType {
    pub fn name(self) -> &'static str {
        match self {
            ErrorType::Error => "Error",
            ErrorType::TypeError => "TypeError",
            ErrorType::RangeError => "RangeError",
            ErrorType::SyntaxError => "SyntaxError",
            ErrorType::ReferenceError => "ReferenceError",
            ErrorType::EvalError => "EvalError",
            ErrorType::URIError => "URIError",
            ErrorType::AggregateError => "AggregateError",
        }
    }
    pub fn from_name(name: &str) -> Option<ErrorType> {
        ERROR_TYPES.iter().copied().find(|t| t.name() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    ParseError,
    UnsupportedSyntax,
    UnknownTool,
    InvalidToolInput,
    InvalidToolOutput,
    InvalidDataValue,
    ToolCallLimitExceeded,
    TimeoutExceeded,
    ToolFailure,
    ExecutionFailure,
    Truncated,
}

impl DiagnosticKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticKind::ParseError => "ParseError",
            DiagnosticKind::UnsupportedSyntax => "UnsupportedSyntax",
            DiagnosticKind::UnknownTool => "UnknownTool",
            DiagnosticKind::InvalidToolInput => "InvalidToolInput",
            DiagnosticKind::InvalidToolOutput => "InvalidToolOutput",
            DiagnosticKind::InvalidDataValue => "InvalidDataValue",
            DiagnosticKind::ToolCallLimitExceeded => "ToolCallLimitExceeded",
            DiagnosticKind::TimeoutExceeded => "TimeoutExceeded",
            DiagnosticKind::ToolFailure => "ToolFailure",
            DiagnosticKind::ExecutionFailure => "ExecutionFailure",
            DiagnosticKind::Truncated => "Truncated",
        }
    }
}

/// A normalized program diagnostic, as the tool layer receives it.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    pub message: JsStr,
    pub location: Option<Loc>,
    pub suggestions: Option<Vec<JsStr>>,
}

impl Diagnostic {
    pub fn new(kind: DiagnosticKind, message: impl Into<JsStr>) -> Diagnostic {
        Diagnostic { kind, message: message.into(), location: None, suggestions: None }
    }
}

/// A failure raised by the interpreter or a built-in. It becomes one program Error object the first time a handler
/// observes it, so every observer of the same failure sees the same value.
pub struct PendingThrow {
    pub ty: ErrorType,
    pub message: JsStr,
    pub loc: Cell<Option<Loc>>,
    pub kind: DiagnosticKind,
    pub suggestions: Option<Vec<JsStr>>,
    pub value: RefCell<Option<Obj>>,
    /// The name a DOMException has (`InvalidCharacterError`).
    pub name: Option<&'static str>,
}

/// A tool-layer failure (`ToolRuntimeError`): unknown tools, invalid input/output, the call limit.
#[derive(Debug)]
pub struct ToolRuntimeError {
    pub kind: DiagnosticKind,
    pub message: JsStr,
    pub suggestions: Vec<JsStr>,
}

/// What interpreter work throws.
#[derive(Clone)]
pub enum Fail {
    /// A program `throw` of any value.
    Throw(Value),
    Pending(Rc<PendingThrow>),
    /// A generator's `return()` unwinding through `finally`.
    GenReturn(Value),
    /// Host work interrupted on its own: settles like a rejection that program code cannot catch.
    Interrupt,
    ToolRuntime(Rc<ToolRuntimeError>),
    /// A tool refused or failed (`ToolError`), reported as `ToolFailure`.
    Tool(JsStr),
    /// A host error that is not one of the above (an extension's thrown Error): its name and message.
    Host(Rc<(JsStr, JsStr)>),
}

impl Fail {
    pub fn pending(ty: ErrorType, message: impl Into<JsStr>, loc: Option<Loc>, kind: DiagnosticKind) -> Fail {
        Fail::Pending(Rc::new(PendingThrow {
            ty,
            message: message.into(),
            loc: Cell::new(loc),
            kind,
            suggestions: None,
            value: RefCell::new(None),
            name: None,
        }))
    }
    pub fn at(self, loc: Option<Loc>) -> Fail {
        locate(self, loc)
    }
    pub fn is_interrupt(&self) -> bool {
        matches!(self, Fail::Interrupt)
    }
}

pub fn type_error(message: impl Into<JsStr>) -> Fail {
    Fail::pending(ErrorType::TypeError, message, None, DiagnosticKind::ExecutionFailure)
}
pub fn range_error(message: impl Into<JsStr>) -> Fail {
    Fail::pending(ErrorType::RangeError, message, None, DiagnosticKind::ExecutionFailure)
}
pub fn reference_error(message: impl Into<JsStr>) -> Fail {
    Fail::pending(ErrorType::ReferenceError, message, None, DiagnosticKind::ExecutionFailure)
}
pub fn syntax_error(message: impl Into<JsStr>) -> Fail {
    Fail::pending(ErrorType::SyntaxError, message, None, DiagnosticKind::ExecutionFailure)
}
pub fn uri_error(message: impl Into<JsStr>) -> Fail {
    Fail::pending(ErrorType::URIError, message, None, DiagnosticKind::ExecutionFailure)
}
pub fn plain_error(message: impl Into<JsStr>) -> Fail {
    Fail::pending(ErrorType::Error, message, None, DiagnosticKind::ExecutionFailure)
}
pub fn invalid_data(message: impl Into<JsStr>) -> Fail {
    Fail::pending(ErrorType::TypeError, message, None, DiagnosticKind::InvalidDataValue)
}
/// A DOMException such as `InvalidCharacterError`: an `Error` with that name.
pub fn dom_exception(name: &'static str, message: impl Into<JsStr>) -> Fail {
    Fail::Pending(Rc::new(PendingThrow {
        ty: ErrorType::Error,
        message: message.into(),
        loc: Cell::new(None),
        kind: DiagnosticKind::ExecutionFailure,
        suggestions: None,
        value: RefCell::new(None),
        name: Some(name),
    }))
}

pub const SUPPORTED_SYNTAX_MESSAGE: &str = "This is a restricted JavaScript-like language. Supported: plain and async functions, data literals, destructuring, standard control flow, await and Promise, and built-ins such as Array, Object, Math, JSON, Date, RegExp, Map, Set, and URL. Unsupported: classes, getters/setters, BigInt, and custom Symbols. Use plain functions and data objects instead.";

pub fn unsupported_syntax(kind: &str, loc: Loc) -> Fail {
    Fail::Pending(Rc::new(PendingThrow {
        ty: ErrorType::SyntaxError,
        message: JsStr::from(format!("Syntax '{kind}' is not supported. {SUPPORTED_SYNTAX_MESSAGE}")),
        loc: Cell::new(Some(loc)),
        kind: DiagnosticKind::UnsupportedSyntax,
        suggestions: Some(vec![JsStr::from(SUPPORTED_SYNTAX_MESSAGE)]),
        value: RefCell::new(None),
        name: None,
    }))
}

pub fn format_location(location: Option<Loc>) -> String {
    match location {
        Some(loc) => format!(" (line {}, col {})", loc.line, loc.column),
        None => String::new(),
    }
}

/// Gives a failure the location of the expression that raised it, keeping the first one attached. Host errors
/// become the equivalent program error.
pub fn locate(error: Fail, loc: Option<Loc>) -> Fail {
    match error {
        Fail::Pending(ref pending) => {
            if pending.loc.get().is_none() && loc.is_some() {
                pending.loc.set(loc);
            }
            error
        }
        Fail::Host(host) => {
            let ty = host.0.to_rust().as_deref().and_then(ErrorType::from_name).unwrap_or(ErrorType::Error);
            Fail::pending(ty, host.1.clone(), loc, DiagnosticKind::ExecutionFailure)
        }
        other => other,
    }
}

fn js_concat(parts: &[&JsStr]) -> JsStr {
    let mut buf = JsStrBuf::new();
    for part in parts {
        buf.push_js(part);
    }
    buf.finish()
}

pub fn normalize_error(ctx: &Ctx, error: &Fail) -> Diagnostic {
    match error {
        Fail::Pending(pending) => {
            let location = pending.loc.get();
            let mut buf = JsStrBuf::new();
            buf.push_str(pending.name.unwrap_or(pending.ty.name()));
            buf.push_str(": ");
            buf.push_js(&pending.message);
            buf.push_str(&format_location(location));
            Diagnostic {
                kind: pending.kind,
                message: buf.finish(),
                location,
                suggestions: pending.suggestions.clone(),
            }
        }
        Fail::ToolRuntime(runtime) => Diagnostic {
            kind: runtime.kind,
            message: runtime.message.clone(),
            location: None,
            suggestions: if runtime.suggestions.is_empty() { None } else { Some(runtime.suggestions.clone()) },
        },
        Fail::Tool(message) => Diagnostic::new(DiagnosticKind::ToolFailure, message.clone()),
        Fail::Throw(value) => {
            if let Value::Obj(obj) = value
                && let Kind::Error(host) = obj.kind()
            {
                let host = host.borrow().clone();
                return match host {
                    Some(host) => normalize_error(ctx, &host),
                    None => Diagnostic::new(DiagnosticKind::ExecutionFailure, error_to_string(ctx, obj)),
                };
            }
            let message = if crate::interp::coerce::contains_runtime_reference(value) {
                JsStr::from("a non-data value")
            } else if let Value::Str(text) = value {
                text.clone()
            } else {
                crate::stdlib::console::format_value(ctx, value)
            };
            Diagnostic::new(DiagnosticKind::ExecutionFailure, js_concat(&[&JsStr::from("Uncaught: "), &message]))
        }
        Fail::Host(host) => {
            let message = host.1.to_rust_lossy().to_lowercase();
            if host.0.eq_str("RangeError") && (message.contains("call stack") || message.contains("recursion")) {
                return Diagnostic::new(DiagnosticKind::ExecutionFailure, "Execution exceeded the maximum nesting depth.");
            }
            Diagnostic::new(
                if host.0.eq_str("SyntaxError") { DiagnosticKind::ParseError } else { DiagnosticKind::ExecutionFailure },
                host.1.clone(),
            )
        }
        // Not Errors in TS: `String(error)` of a plain object.
        Fail::GenReturn(_) | Fail::Interrupt => Diagnostic::new(DiagnosticKind::ExecutionFailure, "[object Object]"),
    }
}

/// Error.prototype.toString: `name: message`, omitting whichever side is empty.
pub fn error_to_string(ctx: &Ctx, target: &Obj) -> JsStr {
    let name = get(ctx, target, &PropKey::from_str("name")).unwrap_or(Value::Undefined);
    let message = get(ctx, target, &PropKey::from_str("message")).unwrap_or(Value::Undefined);
    let shown_name = match name {
        Value::Undefined => JsStr::from("Error"),
        other => crate::interp::coerce::to_string_plain(&other),
    };
    let shown_message = match message {
        Value::Undefined => JsStr::new(),
        other => crate::interp::coerce::to_string_plain(&other),
    };
    if shown_message.is_empty() {
        return shown_name;
    }
    if shown_name.is_empty() {
        return shown_message;
    }
    js_concat(&[&shown_name, &JsStr::from(": "), &shown_message])
}

/// The program value a handler receives for a failure; one failure always yields the same value.
pub fn materialize(ctx: &Ctx, thrown: &Fail) -> Value {
    match thrown {
        Fail::Throw(value) => value.clone(),
        Fail::Pending(pending) => {
            if let Some(value) = pending.value.borrow().clone() {
                return Value::Obj(value);
            }
            let created = ctx.create_error(pending.ty, Some(pending.message.clone()));
            if let Some(name) = pending.name {
                define_str(&created, "name", Value::str(name), HIDDEN);
            }
            if let Kind::Error(host) = created.kind() {
                *host.borrow_mut() = Some(thrown.clone());
            }
            *pending.value.borrow_mut() = Some(created.clone());
            Value::Obj(created)
        }
        Fail::Host(host) => {
            let ty = host.0.to_rust().as_deref().and_then(ErrorType::from_name).unwrap_or(ErrorType::Error);
            Value::Obj(ctx.create_error(ty, Some(normalize_error(ctx, thrown).message)))
        }
        other => Value::Obj(ctx.create_error(ErrorType::Error, Some(normalize_error(ctx, other).message))),
    }
}
