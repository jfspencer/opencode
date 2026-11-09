//! Coercions that never run program code (objects.ts `coerceTo*`, each kind's own `toString`), and the value
//! predicates of references.ts.

use crate::interp::errors::{Fail, invalid_data};
use crate::interp::object::{get_data, get_own, own_keys};
use crate::interp::value::*;
use crate::interp::{Ctx, R};
use crate::jsstr::{JsStr, JsStrBuf};
use crate::numconv;

/// ToString without consulting program-defined methods (each kind's `toString` in objects.ts).
pub fn to_string_plain(value: &Value) -> JsStr {
    match value {
        Value::Undefined => JsStr::from("undefined"),
        Value::Null => JsStr::from("null"),
        Value::Bool(b) => JsStr::from(if *b { "true" } else { "false" }),
        Value::Num(n) => JsStr::from(numconv::to_string(*n)),
        Value::Str(s) => s.clone(),
        Value::Sym(sym) => JsStr::from(format!("Symbol({})", sym.description())),
        // A host class instance without its own toString.
        Value::Tool(_) => JsStr::from("[object Object]"),
        Value::Obj(obj) => obj_to_string(obj),
    }
}

pub fn obj_to_string(obj: &Obj) -> JsStr {
    match obj.kind() {
        Kind::Array(data) => {
            let mut buf = JsStrBuf::new();
            let len = data.len();
            for index in 0..len {
                if index > 0 {
                    buf.push_unit(b',' as u16);
                }
                if let Some(item) = data.get(index)
                    && !item.is_nullish()
                {
                    buf.push_js(&to_string_plain(&item));
                }
            }
            buf.finish()
        }
        Kind::Error(_) => {
            let name = get_data(obj, &PropKey::from_str("name"));
            let message = get_data(obj, &PropKey::from_str("message"));
            let shown_name = match name {
                Value::Str(s) => s,
                _ => JsStr::from("Error"),
            };
            let shown_message = match message {
                Value::Str(s) => s,
                _ => JsStr::new(),
            };
            if shown_message.is_empty() {
                return shown_name;
            }
            if shown_name.is_empty() {
                return shown_message;
            }
            let mut buf = JsStrBuf::new();
            buf.push_js(&shown_name);
            buf.push_str(": ");
            buf.push_js(&shown_message);
            buf.finish()
        }
        Kind::Date(time) => JsStr::from(crate::jsdate::to_iso_string(time.get()).unwrap_or_else(|| "Invalid Date".into())),
        Kind::RegExp(regex) => {
            let mut buf = JsStrBuf::new();
            buf.push_unit(b'/' as u16);
            buf.push_units(&regex.source());
            buf.push_unit(b'/' as u16);
            buf.push_str(&regex.flags());
            buf.finish()
        }
        Kind::URLSearchParams(params) => params.borrow().serialize(),
        Kind::URL(state, _) => state.borrow().href(),
        Kind::Bytes(data) => {
            let text: Vec<String> = data.to_vec().iter().map(|b| b.to_string()).collect();
            JsStr::from(text.join(","))
        }
        _ => JsStr::from(format!("[object {}]", obj.tag())),
    }
}

/// ToPrimitive without program methods: only a Date answers the number hint differently.
pub fn to_primitive_plain(obj: &Obj, number_hint: bool) -> Value {
    if number_hint && let Kind::Date(time) = obj.kind() {
        return Value::Num(time.get());
    }
    Value::Str(obj_to_string(obj))
}

/// ToNumber without program methods; tool references are not numbers.
pub fn to_number_plain(value: &Value) -> f64 {
    match value {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Num(n) => *n,
        Value::Str(s) => numconv::string_to_number(s.units()),
        // Number(symbol) throws in JS; callers that can see a symbol check first.
        Value::Sym(_) => f64::NAN,
        Value::Tool(_) => f64::NAN,
        Value::Obj(obj) => match to_primitive_plain(obj, true) {
            Value::Num(n) => n,
            other => to_number_plain(&other),
        },
    }
}

/// ToNumber that throws for a symbol, as the host's `Number(symbol)` does.
pub fn to_number_checked(value: &Value) -> R<f64> {
    if let Value::Sym(_) = value {
        return Err(Fail::Host(std::rc::Rc::new((
            JsStr::from("TypeError"),
            JsStr::from("Cannot convert a symbol to a number"),
        ))));
    }
    Ok(to_number_plain(value))
}

/// ToIntegerOrInfinity: NaN is 0, fractions truncate.
pub fn to_integer(value: &Value) -> f64 {
    integer_of(to_number_plain(value))
}

pub fn integer_of(number: f64) -> f64 {
    if number.is_nan() { 0.0 } else { number.trunc() + 0.0 }
}

pub fn to_uint8(number: f64) -> u8 {
    if !number.is_finite() {
        return 0;
    }
    (number.trunc().rem_euclid(256.0)) as u8
}

pub fn to_int32(number: f64) -> i32 {
    numconv::to_int32(number)
}

pub fn to_uint32(number: f64) -> u32 {
    numconv::to_uint32(number)
}

pub fn typeof_value(value: &Value) -> &'static str {
    match value {
        Value::Undefined => "undefined",
        Value::Null => "object",
        Value::Bool(_) => "boolean",
        Value::Num(_) => "number",
        Value::Str(_) => "string",
        Value::Sym(_) => "symbol",
        Value::Tool(tool) => {
            if tool.path().is_empty() {
                "object"
            } else {
                "function"
            }
        }
        Value::Obj(obj) => {
            if obj.is_callable() {
                "function"
            } else {
                "object"
            }
        }
    }
}

pub fn is_callable_value(value: &Value) -> bool {
    typeof_value(value) == "function"
}

/// Interpreter machinery that is never data.
pub fn is_opaque(value: &Value) -> bool {
    match value {
        Value::Obj(obj) => obj.is_opaque(),
        Value::Tool(_) => true,
        _ => false,
    }
}

/// Values that cannot cross the data boundary: opaque machinery and host-backed wrappers.
pub fn is_runtime_reference(value: &Value) -> bool {
    match value {
        Value::Obj(obj) => obj.is_opaque() || obj.is_wrapper(),
        Value::Tool(_) => true,
        _ => false,
    }
}

// Depth-first search over a value tree (own property values, as getOwn reads them).
fn find(value: &Value, matches: &dyn Fn(&Value) -> bool, skip: &dyn Fn(&Value) -> bool) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut pending: Vec<Value> = vec![value.clone()];
    while let Some(current) = pending.pop() {
        if matches(&current) {
            return true;
        }
        let Value::Obj(obj) = &current else { continue };
        if skip(&current) || !seen.insert(obj.addr()) {
            continue;
        }
        // Children in reverse so the walk visits them in order.
        let mut children = Vec::new();
        for key in own_keys(obj) {
            children.push(own_value(obj, &key));
        }
        pending.extend(children.into_iter().rev());
    }
    false
}

fn own_value(obj: &Obj, key: &PropKey) -> Value {
    match crate::interp::object::own(obj, key) {
        Some(Slot::Data { value, .. }) => value,
        // Built-in accessors read host state; none of them hand back program objects that matter here.
        _ => Value::Undefined,
    }
}

pub fn contains_runtime_reference(value: &Value) -> bool {
    find(value, &is_runtime_reference, &|_| false)
}

pub fn contains_opaque_reference(value: &Value) -> bool {
    find(value, &is_opaque, &|current| is_runtime_reference(current) && !is_opaque(current))
}

/// Rejects cycles before mutation so later boundary walks remain safe.
pub fn reject_circular_insertion(container: &Obj, value: &Value, label: &str) -> R<()> {
    if find(value, &|current| matches!(current, Value::Obj(obj) if obj.ptr_eq(container)), &is_runtime_reference) {
        return Err(invalid_data(format!("{label} contains a circular value.")));
    }
    Ok(())
}

/// The same walk with a shared `seen` set across several values (Object.assign).
pub struct CircularCheck {
    seen: std::collections::HashSet<usize>,
}

impl CircularCheck {
    pub fn new() -> CircularCheck {
        CircularCheck { seen: Default::default() }
    }
    pub fn check(&mut self, container: &Obj, value: &Value, label: &str) -> R<()> {
        let mut pending = vec![value.clone()];
        while let Some(current) = pending.pop() {
            if let Value::Obj(obj) = &current {
                if obj.ptr_eq(container) {
                    return Err(invalid_data(format!("{label} contains a circular value.")));
                }
                if is_runtime_reference(&current) || !self.seen.insert(obj.addr()) {
                    continue;
                }
                let mut children = Vec::new();
                for key in own_keys(obj) {
                    children.push(own_value(obj, &key));
                }
                pending.extend(children.into_iter().rev());
            }
        }
        Ok(())
    }
}

impl Default for CircularCheck {
    fn default() -> Self {
        Self::new()
    }
}

pub fn describe_value(value: &Value) -> String {
    match value {
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Obj(obj) => obj.describe(),
        Value::Tool(_) => "a tool reference".into(),
        Value::Bool(_) => "a boolean".into(),
        Value::Num(_) => "a number".into(),
        Value::Str(_) => "a string".into(),
        Value::Sym(_) => "a symbol".into(),
    }
}

/// `getOwn` with a context (accessors allowed).
pub fn own_get(ctx: &Ctx, obj: &Obj, key: &PropKey) -> R<Value> {
    get_own(ctx, obj, key)
}
