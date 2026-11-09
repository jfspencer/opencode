//! Calling back into the program from built-ins (callback.ts): ToPrimitive through a value's own methods, the one
//! callback acceptance rule, and IteratorClose's error preference.

use crate::interp::coerce::{is_callable_value, is_opaque, to_number_plain, to_string_plain};
use crate::interp::errors::{Fail, type_error};
use crate::interp::object::get;
use crate::interp::value::*;
use crate::interp::{Ctx, Fut, R, boxed};
use crate::jsstr::JsStr;
use std::future::Future;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hint {
    Number,
    String,
    Default,
}

/// IteratorClose: a consumer failure closes the iterator and wins over any close failure, except that a generator's
/// return() is a return completion, so a failing close wins over it.
pub async fn preserve_consumer_error<T>(close: impl Future<Output = R<()>>, body: R<T>) -> R<T> {
    match body {
        Ok(value) => Ok(value),
        Err(error) => {
            if error.is_interrupt() {
                return Err(error);
            }
            if let Err(closed) = close.await
                && matches!(error, Fail::GenReturn(_))
            {
                return Err(closed);
            }
            Err(error)
        }
    }
}

/// ToPrimitive: calls `valueOf`/`toString` in hint order and returns the first primitive result. Dates treat the
/// default hint as "string". Opaque values pass through unchanged.
pub async fn to_primitive(ctx: &Ctx, value: Value, hint: Hint) -> R<Value> {
    let Value::Obj(obj) = &value else { return Ok(value) };
    if obj.is_opaque() {
        return Ok(value);
    }
    let as_string = hint == Hint::String || (hint == Hint::Default && obj.date().is_some());
    let order = if as_string { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
    for method in order {
        let callable = get(ctx, obj, &PropKey::from_str(method))?;
        if !matches!(&callable, Value::Obj(f) if f.is_callable()) {
            continue;
        }
        let result = ctx.call(callable, value.clone(), Vec::new(), None).await?;
        if !matches!(result, Value::Obj(_) | Value::Tool(_)) {
            return Ok(result);
        }
    }
    Err(type_error("Cannot convert object to primitive value."))
}

pub async fn to_primitive_string(ctx: &Ctx, value: Value) -> R<JsStr> {
    let primitive = to_primitive(ctx, value, Hint::String).await?;
    Ok(to_string_plain(&primitive))
}

pub async fn to_primitive_number(ctx: &Ctx, value: Value) -> R<f64> {
    let primitive = to_primitive(ctx, value, Hint::Number).await?;
    crate::interp::expr::prim_number(&primitive)
}

/// Invoke(value, name): calls the method the value would find through its prototype.
pub fn invoke(ctx: &Ctx, value: Value, name: &str, label: &str) -> R<Fut<R<Value>>> {
    let target = match &value {
        Value::Obj(obj) => obj.clone(),
        other => match ctx.primitive_prototype(other) {
            Some(proto) => proto,
            None => return Err(type_error(format!("{label} called on null or undefined."))),
        },
    };
    let method = crate::interp::object::get_with(ctx, &target, &PropKey::from_str(name), &value)?;
    Ok(ctx.call(method, value, Vec::new(), None))
}

/// Runs a body on its arguments after ToPrimitive, in order, one hint per position. Primitive arguments skip the
/// conversion entirely.
pub fn with_primitives(
    ctx: &Ctx,
    hints: &[Hint],
    values: Vec<Value>,
    body: impl FnOnce(&Ctx, Vec<Value>) -> R<NOut> + 'static,
) -> R<NOut> {
    if !values.iter().any(|value| matches!(value, Value::Obj(_))) {
        return body(ctx, values);
    }
    let hints = hints.to_vec();
    let ctx = ctx.clone();
    Ok(NOut::Work(boxed(async move {
        let mut primitives = Vec::with_capacity(values.len());
        for (index, value) in values.into_iter().enumerate() {
            let hint = hints.get(index).or(hints.last()).copied().unwrap_or(Hint::Default);
            primitives.push(to_primitive(&ctx, value, hint).await?);
        }
        match body(&ctx, primitives)? {
            NOut::Val(value) => Ok(value),
            NOut::Work(work) => work.await,
        }
    })))
}

/// The single acceptance list for callbacks: every callable except natives that opt out.
pub fn is_supported_callback(value: &Value) -> bool {
    match value {
        Value::Obj(obj) => match obj.callable() {
            Some(Callable::Program(_)) => true,
            Some(Callable::Native(native)) => native.callback,
            None => false,
        },
        _ => false,
    }
}

pub type Apply = Rc<dyn Fn(Vec<Value>, Value) -> Fut<R<Value>>>;

pub fn apply_collection_callback(ctx: &Ctx, callback: &Value, name: &str) -> R<Apply> {
    if !is_supported_callback(callback) {
        if is_callable_value(callback) {
            return Err(type_error(format!(
                "{name} cannot use this callable as a callback; wrap it in an arrow function, e.g. (value) => tools.ns.tool(value)."
            )));
        }
        return Err(type_error(format!("{name} expects a function callback.")));
    }
    let (ctx, callback) = (ctx.clone(), callback.clone());
    Ok(Rc::new(move |args, this| ctx.call(callback.clone(), this, args, None)))
}

pub fn opaque_passthrough(value: &Value) -> bool {
    is_opaque(value)
}

pub fn number_of(value: &Value) -> f64 {
    to_number_plain(value)
}

pub fn string_of(value: &Value) -> JsStr {
    to_string_plain(value)
}
