//! `Number` and `Boolean` (number.ts).

use crate::interp::callback::{Hint, with_primitives};
use crate::interp::coerce::to_number_checked;
use crate::interp::errors::{Fail, type_error};
use crate::interp::native::NativeOptions;
use crate::interp::object::define;
use crate::interp::value::*;
use crate::interp::{Ctx, R};
use crate::jsstr::JsStr;
use crate::numconv;
use crate::stdlib::value::coercion;
use std::rc::Rc;

fn host_range_error(message: &str) -> Fail {
    Fail::Host(Rc::new((JsStr::from("RangeError"), JsStr::from(message))))
}

fn this_number(this: &Value, name: &str) -> R<f64> {
    match this {
        Value::Num(n) => Ok(*n),
        _ => Err(type_error(format!("Number.prototype.{name} requires that 'this' be a Number."))),
    }
}

fn digits_in(digits: Option<f64>, min: f64, max: f64, message: &str) -> R<Option<u32>> {
    match digits {
        None => Ok(None),
        Some(d) => {
            let d = crate::interp::coerce::integer_of(d);
            if !(min..=max).contains(&d) {
                return Err(host_range_error(message));
            }
            Ok(Some(d as u32))
        }
    }
}

fn format(name: &str, value: f64, digits: Option<f64>) -> R<String> {
    match name {
        "toFixed" => {
            let d = digits_in(Some(digits.unwrap_or(0.0)), 0.0, 100.0, "toFixed() argument must be between 0 and 100")?;
            Ok(numconv::to_fixed(value, d.unwrap_or(0)))
        }
        "toExponential" => {
            let d = digits.map(crate::interp::coerce::integer_of);
            if !value.is_finite() {
                return Ok(numconv::to_string(value));
            }
            let d = digits_in(d, 0.0, 100.0, "toExponential() argument must be between 0 and 100")?;
            Ok(numconv::to_exponential(value, d))
        }
        "toPrecision" => {
            let Some(p) = digits else { return Ok(numconv::to_string(value)) };
            let p = crate::interp::coerce::integer_of(p);
            if !value.is_finite() {
                return Ok(numconv::to_string(value));
            }
            let p = digits_in(Some(p), 1.0, 100.0, "toPrecision() argument must be between 1 and 100")?;
            Ok(numconv::to_precision(value, p.unwrap()))
        }
        _ => {
            let radix = match digits {
                None => 10.0,
                Some(r) => crate::interp::coerce::integer_of(r),
            };
            if !(2.0..=36.0).contains(&radix) {
                return Err(host_range_error("toString() radix must be between 2 and 36"));
            }
            Ok(numconv::to_string_radix(value, radix as u32))
        }
    }
}

pub fn number_global(ctx: &Ctx) -> Value {
    let builtins = &ctx.builtins;
    let call = coercion(ctx, "Number", 1).native_fn().unwrap().call.borrow().clone();
    let number = ctx.constructor(
        &builtins.number,
        NativeOptions { name: "Number".into(), length: 1, call, construct: None, callback: true },
    );
    ctx.constants(
        &number,
        &[
            ("MAX_SAFE_INTEGER", Value::Num(9007199254740991.0)),
            ("MIN_SAFE_INTEGER", Value::Num(-9007199254740991.0)),
            ("MAX_VALUE", Value::Num(f64::MAX)),
            ("MIN_VALUE", Value::Num(5e-324)),
            ("EPSILON", Value::Num(f64::EPSILON)),
            ("NaN", Value::Num(f64::NAN)),
            ("POSITIVE_INFINITY", Value::Num(f64::INFINITY)),
            ("NEGATIVE_INFINITY", Value::Num(f64::NEG_INFINITY)),
        ],
    );
    let integer = |v: &Value| matches!(v, Value::Num(n) if n.is_finite() && n.fract() == 0.0);
    ctx.method(&number, "isInteger", 1, move |_, _, args, _| Ok(Value::Bool(args.first().is_some_and(integer))));
    ctx.method(&number, "isFinite", 1, |_, _, args, _| {
        Ok(Value::Bool(matches!(args.first(), Some(Value::Num(n)) if n.is_finite())))
    });
    ctx.method(&number, "isNaN", 1, |_, _, args, _| Ok(Value::Bool(matches!(args.first(), Some(Value::Num(n)) if n.is_nan()))));
    ctx.method(&number, "isSafeInteger", 1, move |_, _, args, _| {
        Ok(Value::Bool(args.first().is_some_and(|v| integer(v) && matches!(v, Value::Num(n) if n.abs() <= 9007199254740991.0))))
    });
    define(&number, PropKey::from_str("parseInt"), Value::Obj(coercion(ctx, "parseInt", 2)), HIDDEN);
    define(&number, PropKey::from_str("parseFloat"), Value::Obj(coercion(ctx, "parseFloat", 1)), HIDDEN);

    let proto = builtins.number.clone();
    // The receiver is checked first, then the one argument converts through ToPrimitive with the number hint.
    for name in ["toFixed", "toExponential", "toPrecision", "toString"] {
        ctx.method_work(&proto, name, 1, move |ctx, this, args, _| {
            let value = this_number(&this, name)?;
            let digits = args.first().cloned().unwrap_or_default();
            with_primitives(ctx, &[Hint::Number], vec![digits], move |_, values| {
                let digits = match &values[0] {
                    Value::Undefined => None,
                    other => Some(to_number_checked(other)?),
                };
                Ok(NOut::Val(Value::Str(JsStr::from(format(name, value, digits)?))))
            })
        });
    }
    ctx.method(&proto, "toLocaleString", 0, |_, this, _, _| {
        let value = this_number(&this, "toLocaleString")?;
        Ok(Value::Str(JsStr::from(numconv::to_locale_string_en_us(value))))
    });
    ctx.method(&proto, "valueOf", 0, |_, this, _, _| Ok(Value::Num(this_number(&this, "valueOf")?)));
    Value::Obj(number)
}

pub fn boolean_global(ctx: &Ctx) -> Value {
    let builtins = &ctx.builtins;
    let call = coercion(ctx, "Boolean", 1).native_fn().unwrap().call.borrow().clone();
    let boolean = ctx.constructor(
        &builtins.boolean,
        NativeOptions { name: "Boolean".into(), length: 1, call, construct: None, callback: true },
    );
    let this_bool = |this: &Value, name: &str| -> R<bool> {
        match this {
            Value::Bool(b) => Ok(*b),
            _ => Err(type_error(format!("Boolean.prototype.{name} requires that 'this' be a Boolean."))),
        }
    };
    ctx.method(&builtins.boolean, "toString", 0, move |_, this, _, _| {
        Ok(Value::str(if this_bool(&this, "toString")? { "true" } else { "false" }))
    });
    ctx.method(&builtins.boolean, "valueOf", 0, move |_, this, _, _| Ok(Value::Bool(this_bool(&this, "valueOf")?)));
    Value::Obj(boolean)
}
