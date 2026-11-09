//! Host globals (interpreter/extensions.ts): functions, or objects of functions, a program calls directly (core's
//! `fetch`). Every value crossing in either direction is converted, never shared: data and built-in wrappers are
//! copied, a host function becomes a program function whose calls cross the same way, and an asynchronous host call
//! becomes a program promise.

use crate::interp::coerce::{describe_value, is_opaque, to_string_plain};
use crate::interp::errors::{ErrorType, Fail, range_error, type_error};
use crate::interp::limits::{MAX_ARRAY_LENGTH, MAX_VALUE_DEPTH};
use crate::interp::native::NativeOptions;
use crate::interp::object::{define, get, has, keys};
use crate::interp::value::*;
use crate::interp::{Ctx, Fut, R, boxed};
use crate::jsstr::JsStr;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// A value on the host side of the boundary.
#[derive(Clone)]
pub enum HostValue {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(JsStr),
    /// `None` is a hole.
    Array(Vec<Option<HostValue>>),
    /// A plain object, keys in insertion order.
    Object(Vec<(JsStr, HostValue)>),
    Date(f64),
    RegExp { source: JsStr, flags: String },
    Bytes(Vec<u8>),
    Url(String),
    SearchParams(Vec<(JsStr, JsStr)>),
    Headers(Vec<(String, String)>),
    Map(Vec<(HostValue, HostValue)>),
    Set(Vec<HostValue>),
    Error(HostError),
    Function(HostFunction),
}

#[derive(Clone)]
pub struct HostError {
    /// The error's `name` (`TypeError`, ... or any other text, which reads as `Error`).
    pub name: String,
    pub message: JsStr,
    pub cause: Option<Box<HostValue>>,
    /// Own enumerable fields such as Node's `code`.
    pub fields: Vec<(JsStr, HostValue)>,
}

/// What a host function hands back: a value or a throw now, or later.
pub enum HostReturn {
    Value(Result<HostValue, HostValue>),
    Async(Fut<Result<HostValue, HostValue>>),
}

#[derive(Clone)]
pub struct HostFunction {
    pub name: String,
    pub length: u32,
    /// An `async` function (or one marked with `Extension.async`): a call returns a promise.
    pub asynchronous: bool,
    pub call: Rc<dyn Fn(Vec<HostValue>) -> HostReturn>,
}

/// Host functions and objects exposed as globals (`Extension.make({ name, globals })`).
#[derive(Clone)]
pub struct Extension {
    pub name: String,
    pub globals: Vec<(String, HostValue)>,
}

impl Extension {
    /// A global that shadows a built-in or another extension is an error (TS throws at `CodeMode.make`).
    pub fn validate(extensions: &[Extension]) -> Result<(), String> {
        let mut bound: Vec<String> = crate::stdlib::GLOBAL_NAMES.iter().map(|s| s.to_string()).collect();
        for extension in extensions {
            for (name, value) in &extension.globals {
                if !matches!(value, HostValue::Function(_) | HostValue::Object(_)) {
                    return Err(format!("Extension \"{}\" global \"{name}\" must be a function or an object.", extension.name));
                }
                if bound.contains(name) {
                    return Err(format!("Extension \"{}\" global \"{name}\" is already defined.", extension.name));
                }
                bound.push(name.clone());
            }
        }
        Ok(())
    }
}

/// Observers around calls to extension globals (and functions of global objects), with the arguments as they
/// crossed.
pub type ExtensionHook = Rc<dyn Fn(&str, &str, &[HostValue])>;

const UNCROSSED: [&str; 5] = ["stack", "message", "constructor", "toString", "__proto__"];

fn depth_error(label: &str) -> Fail {
    type_error(format!("{label} exceeds the maximum value depth of {MAX_VALUE_DEPTH}."))
}

/// Program value to host value.
pub fn to_host(ctx: &Ctx, value: &Value, label: &str, depth: usize, seen: &mut Vec<usize>) -> R<HostValue> {
    if depth > MAX_VALUE_DEPTH {
        return Err(depth_error(label));
    }
    let obj = match value {
        Value::Undefined => return Ok(HostValue::Undefined),
        Value::Null => return Ok(HostValue::Null),
        Value::Bool(b) => return Ok(HostValue::Bool(*b)),
        Value::Num(n) => return Ok(HostValue::Num(*n)),
        Value::Str(s) => return Ok(HostValue::Str(s.clone())),
        Value::Obj(obj) if !obj.is_opaque() => obj,
        other => {
            return Err(type_error(format!(
                "{label} contains {}, which cannot be passed to an extension.",
                describe_value(other)
            )));
        }
    };
    if seen.contains(&obj.addr()) {
        return Err(type_error(format!("{label} contains a circular value.")));
    }
    seen.push(obj.addr());
    let result = (|| -> R<HostValue> {
        let next = |item: &Value, seen: &mut Vec<usize>| to_host(ctx, item, label, depth + 1, seen);
        Ok(match obj.kind() {
            Kind::Error(_) => {
                let name = to_string_plain(&get(ctx, obj, &PropKey::from_str("name"))?).to_rust_lossy();
                let message = match get(ctx, obj, &PropKey::from_str("message"))? {
                    Value::Undefined => JsStr::new(),
                    other => to_string_plain(&other),
                };
                let mut error = HostError { name, message, cause: None, fields: Vec::new() };
                let mut names: Vec<PropKey> = vec![PropKey::from_str("cause")];
                for key in keys(obj) {
                    if !names.contains(&key) {
                        names.push(key);
                    }
                }
                for key in names {
                    let text = key.to_js().to_rust_lossy();
                    if UNCROSSED.contains(&text.as_str()) || !has(obj, &key) {
                        continue;
                    }
                    // A field that cannot cross is left behind rather than replacing the error.
                    let item = match next(&get(ctx, obj, &key)?, seen) {
                        Ok(item) => item,
                        Err(Fail::Pending(_)) => continue,
                        Err(other) => return Err(other),
                    };
                    if text == "cause" {
                        error.cause = Some(Box::new(item));
                    } else {
                        error.fields.push((key.to_js(), item));
                    }
                }
                HostValue::Error(error)
            }
            Kind::Array(data) => {
                let mut items = Vec::with_capacity(data.len());
                for index in 0..data.len() {
                    items.push(match data.get(index) {
                        None => None,
                        Some(item) => Some(next(&item, seen)?),
                    });
                }
                HostValue::Array(items)
            }
            Kind::Date(time) => HostValue::Date(time.get()),
            Kind::RegExp(regex) => HostValue::RegExp { source: JsStr::from_units(regex.source()), flags: regex.flags() },
            Kind::Map(map) => {
                let pairs = map.borrow().entries();
                let mut out = Vec::new();
                for (key, item) in pairs {
                    out.push((next(&key, seen)?, next(&item, seen)?));
                }
                HostValue::Map(out)
            }
            Kind::Set(set) => {
                let items = set.borrow().values();
                let mut out = Vec::new();
                for item in items {
                    out.push(next(&item, seen)?);
                }
                HostValue::Set(out)
            }
            Kind::URLSearchParams(params) => HostValue::SearchParams(params.borrow().list.clone()),
            Kind::Headers(headers) => HostValue::Headers(
                headers.borrow().sorted_pairs().into_iter().map(|(k, v)| (k.to_rust_lossy(), v.to_rust_lossy())).collect(),
            ),
            Kind::URL(state, _) => HostValue::Url(state.borrow().url.as_str().to_string()),
            Kind::Bytes(data) => HostValue::Bytes(data.to_vec()),
            _ => {
                let mut fields = Vec::new();
                for (key, item) in crate::interp::object::entries(ctx, obj)? {
                    if key.eq_str("__proto__") {
                        continue;
                    }
                    fields.push((key.to_js(), next(&item, seen)?));
                }
                HostValue::Object(fields)
            }
        })
    })();
    seen.pop();
    result
}

/// Host value to program value. `extension` is set while converting a global object, whose functions are entry
/// points like globals and fire the hooks.
pub fn from_host(ctx: &Ctx, value: &HostValue, label: &str, depth: usize, extension: Option<&Rc<ExtensionContext>>) -> R<Value> {
    if depth > MAX_VALUE_DEPTH {
        return Err(depth_error(label));
    }
    let builtins = &ctx.builtins;
    Ok(match value {
        HostValue::Undefined => Value::Undefined,
        HostValue::Null => Value::Null,
        HostValue::Bool(b) => Value::Bool(*b),
        HostValue::Num(n) => Value::Num(*n),
        HostValue::Str(s) => Value::Str(s.clone()),
        HostValue::Function(function) => Value::Obj(wrap(ctx, function, label, extension.cloned())),
        HostValue::Date(time) => Value::Obj(ctx.new_object(Some(builtins.date.clone()), Kind::Date(Cell::new(*time)))),
        HostValue::RegExp { source, flags } => {
            Value::Obj(crate::stdlib::regexp::construct_regexp(ctx, &[Value::Str(source.clone()), Value::str(flags)], None)?)
        }
        HostValue::Bytes(bytes) => Value::Obj(crate::stdlib::bytes::new_bytes(ctx, builtins.uint8_array.clone(), bytes.clone())),
        HostValue::Error(error) => {
            let ty = ErrorType::from_name(&error.name).unwrap_or(ErrorType::Error);
            let copied = ctx.create_error(ty, Some(error.message.clone()));
            let mut fields: Vec<(JsStr, &HostValue)> = Vec::new();
            if let Some(cause) = &error.cause {
                fields.push((JsStr::from("cause"), cause));
            }
            for (key, item) in &error.fields {
                fields.push((key.clone(), item));
            }
            for (key, item) in fields {
                let text = key.to_rust_lossy();
                if UNCROSSED.contains(&text.as_str()) || matches!(item, HostValue::Function(_)) {
                    continue;
                }
                let item = match from_host(ctx, item, &format!("{label}.{text}"), depth + 1, extension) {
                    Ok(item) => item,
                    Err(Fail::Pending(_)) => continue,
                    Err(other) => return Err(other),
                };
                define(&copied, PropKey::from_js(key), item, if text == "cause" { HIDDEN } else { DATA });
            }
            Value::Obj(copied)
        }
        HostValue::Url(href) => match url::Url::parse(href) {
            Ok(parsed) => Value::Obj(crate::stdlib::url::new_url(ctx, builtins.url.clone(), parsed)),
            Err(_) => return Err(type_error(format!("{label} produced an invalid URL, which the program cannot hold."))),
        },
        HostValue::SearchParams(list) => Value::Obj(ctx.new_object(
            Some(builtins.url_search_params.clone()),
            Kind::URLSearchParams(Rc::new(RefCell::new(crate::stdlib::url::SearchParams { list: list.clone(), url: None }))),
        )),
        HostValue::Headers(pairs) => {
            let mut list = crate::stdlib::headers::HeaderList::default();
            for (name, value) in pairs {
                list.append(&JsStr::from(name.as_str()), &JsStr::from(value.as_str()))?;
            }
            Value::Obj(ctx.new_object(Some(builtins.headers.clone()), Kind::Headers(RefCell::new(list))))
        }
        HostValue::Map(pairs) => {
            let target = ctx.new_object(Some(builtins.map.clone()), Kind::Map(Default::default()));
            for (key, item) in pairs {
                let (key, item) = (from_host(ctx, key, label, depth + 1, extension)?, from_host(ctx, item, label, depth + 1, extension)?);
                if let Kind::Map(map) = target.kind() {
                    map.borrow_mut().set(key, item);
                }
            }
            Value::Obj(target)
        }
        HostValue::Set(items) => {
            let target = ctx.new_object(Some(builtins.set.clone()), Kind::Set(Default::default()));
            for item in items {
                let item = from_host(ctx, item, label, depth + 1, extension)?;
                if let Kind::Set(set) = target.kind() {
                    set.borrow_mut().add(item);
                }
            }
            Value::Obj(target)
        }
        HostValue::Array(items) => {
            if items.len() > MAX_ARRAY_LENGTH {
                return Err(range_error(format!(
                    "{label} produced an array of {} items; at most {MAX_ARRAY_LENGTH} can cross.",
                    items.len()
                )));
            }
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                out.push(match item {
                    None => None,
                    Some(item) => Some(from_host(ctx, item, &format!("{label}[{index}]"), depth + 1, extension)?),
                });
            }
            Value::Obj(ctx.new_array_holes(out))
        }
        HostValue::Object(fields) => {
            let copied = ctx.new_plain();
            for (key, item) in fields {
                let path = format!("{label}.{}", key.to_rust_lossy());
                let item = from_host(ctx, item, &path, depth + 1, extension)?;
                define(&copied, PropKey::from_js(key.clone()), item, DATA);
            }
            Value::Obj(copied)
        }
    })
}

/// The hooks a call to an entry point (a global, or a global object's function) fires.
pub struct ExtensionContext {
    pub extension: String,
    pub start: Option<ExtensionHook>,
    pub end: Option<Rc<dyn Fn(&str, &str, crate::tools::CallStatus)>>,
}

fn thrown(ctx: &Ctx, reason: &HostValue, label: &str) -> Fail {
    match from_host(ctx, reason, label, 0, None) {
        Ok(value) => Fail::Throw(value),
        Err(error) => error,
    }
}

// A host function as a program function. Arguments cross in; for an entry point the hooks surround the call. The
// host's result, or what it threw, crosses out so the program catches a copy.
fn wrap(ctx: &Ctx, function: &HostFunction, label: &str, hooks: Option<Rc<ExtensionContext>>) -> Obj {
    let function = function.clone();
    let label = label.to_string();
    ctx.new_native(NativeOptions {
        name: function.name.clone(),
        length: function.length,
        call: Rc::new(move |ctx, _, values, site| {
            let mut args = Vec::with_capacity(values.len());
            for (index, item) in values.iter().enumerate() {
                args.push(to_host(ctx, item, &format!("Argument {} to {label}", index + 1), 0, &mut Vec::new())?);
            }
            if let Some(hooks) = &hooks
                && let Some(start) = &hooks.start
            {
                start(&hooks.extension, &label, &args);
            }
            let end = {
                let (hooks, label) = (hooks.clone(), label.clone());
                move |status| {
                    if let Some(hooks) = &hooks
                        && let Some(end) = &hooks.end
                    {
                        end(&hooks.extension, &label, status);
                    }
                }
            };
            // An asynchronous global settles its promise in a later job even when the host answers at once.
            let returned = match (function.call)(args) {
                HostReturn::Value(outcome) if function.asynchronous => HostReturn::Async(boxed(async move { outcome })),
                other => other,
            };
            match returned {
                HostReturn::Value(Ok(value)) => {
                    end(crate::tools::CallStatus::Success);
                    from_host(ctx, &value, &label, 0, None).map(NOut::Val)
                }
                HostReturn::Value(Err(reason)) => {
                    end(crate::tools::CallStatus::Failure);
                    Err(thrown(ctx, &reason, &label))
                }
                HostReturn::Async(work) => {
                    let promise = ctx.runner.new_promise(ctx)?;
                    let resolving = ctx.runner.resolving(&promise, site);
                    let label = label.clone();
                    let host_end = end.clone();
                    // The host's outcome is carried as is and converted when it settles, in a job.
                    let outcome: Rc<RefCell<Option<Result<HostValue, HostValue>>>> = Rc::new(RefCell::new(None));
                    let written = outcome.clone();
                    ctx.runner.host(
                        ctx,
                        boxed(async move {
                            let result = work.await;
                            host_end(if result.is_ok() {
                                crate::tools::CallStatus::Success
                            } else {
                                crate::tools::CallStatus::Failure
                            });
                            *written.borrow_mut() = Some(result);
                            Ok(Value::Undefined)
                        }),
                        move |ctx, _| match outcome.borrow_mut().take() {
                            Some(Ok(value)) => match from_host(ctx, &value, &label, 0, None) {
                                Ok(value) => resolving.resolve(ctx, value),
                                Err(error) => resolving.reject(ctx, error),
                            },
                            Some(Err(reason)) => resolving.reject(ctx, thrown(ctx, &reason, &label)),
                            None => resolving.reject(ctx, Fail::Interrupt),
                        },
                    );
                    Ok(NOut::Val(Value::Obj(promise)))
                }
            }
        }),
        construct: None,
        callback: true,
    })
}

/// The global bindings of a run's extensions.
pub fn extension_globals(
    ctx: &Ctx,
    extensions: &[Extension],
    start: Option<ExtensionHook>,
    end: Option<Rc<dyn Fn(&str, &str, crate::tools::CallStatus)>>,
) -> Vec<(JsStr, Value)> {
    let mut out = Vec::new();
    for extension in extensions {
        let hooks = Rc::new(ExtensionContext { extension: extension.name.clone(), start: start.clone(), end: end.clone() });
        for (name, value) in &extension.globals {
            let converted = match value {
                HostValue::Function(function) => Value::Obj(wrap(ctx, function, name, Some(hooks.clone()))),
                other => from_host(ctx, other, name, 0, Some(&hooks)).unwrap_or(Value::Undefined),
            };
            out.push((JsStr::from(name.as_str()), converted));
        }
    }
    out
}

pub fn _is_opaque(value: &Value) -> bool {
    is_opaque(value)
}
