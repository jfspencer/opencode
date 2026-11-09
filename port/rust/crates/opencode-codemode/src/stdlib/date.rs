//! `Date` (date.ts) over `jsdate`.

use crate::interp::callback::{Hint, to_primitive, to_primitive_number};
use crate::interp::coerce::to_string_plain;
use crate::interp::errors::range_error;
use crate::interp::native::{NativeOptions, prototype_from, receiver};
use crate::interp::value::*;
use crate::interp::{Ctx, R, boxed};
use crate::jsdate::{self, Getter, Setter};
use std::cell::Cell;
use std::rc::Rc;

fn new_date(ctx: &Ctx, proto: Obj, time: f64) -> Value {
    Value::Obj(ctx.new_object(Some(proto), Kind::Date(Cell::new(time))))
}

// ToNumber on each value in turn, so a symbol throws before any later value's conversion runs program code.
async fn to_numbers(ctx: &Ctx, values: &[Value]) -> R<Vec<f64>> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        out.push(to_primitive_number(ctx, value.clone()).await?);
    }
    Ok(out)
}

async fn construct(ctx: Ctx, args: Vec<Value>, proto: Obj) -> R<Value> {
    if args.is_empty() {
        return Ok(new_date(&ctx, proto, jsdate::now()));
    }
    if args.len() == 1 {
        if let Value::Obj(obj) = &args[0]
            && let Some(time) = obj.date()
        {
            return Ok(new_date(&ctx, proto, time.get()));
        }
        let value = to_primitive(&ctx, args[0].clone(), Hint::Default).await?;
        let time = match &value {
            Value::Str(text) => jsdate::parse(text.units()),
            other => jsdate::time_clip(crate::interp::expr::prim_number(other)?),
        };
        return Ok(new_date(&ctx, proto, time));
    }
    // The spec converts at most seven components, in order, so extra arguments never run program code.
    let parts = to_numbers(&ctx, &args[..args.len().min(7)]).await?;
    Ok(new_date(&ctx, proto, jsdate::from_local_components(&parts)))
}

const GETTERS: [(&str, Getter); 17] = [
    ("getFullYear", Getter::FullYear),
    ("getMonth", Getter::Month),
    ("getDate", Getter::Date),
    ("getDay", Getter::Day),
    ("getHours", Getter::Hours),
    ("getMinutes", Getter::Minutes),
    ("getSeconds", Getter::Seconds),
    ("getMilliseconds", Getter::Milliseconds),
    ("getUTCFullYear", Getter::UtcFullYear),
    ("getUTCMonth", Getter::UtcMonth),
    ("getUTCDate", Getter::UtcDate),
    ("getUTCDay", Getter::UtcDay),
    ("getUTCHours", Getter::UtcHours),
    ("getUTCMinutes", Getter::UtcMinutes),
    ("getUTCSeconds", Getter::UtcSeconds),
    ("getUTCMilliseconds", Getter::UtcMilliseconds),
    ("getTimezoneOffset", Getter::TimezoneOffset),
];

const SETTERS: [(&str, u32, Setter); 15] = [
    ("setTime", 1, Setter::Time),
    ("setMilliseconds", 1, Setter::Milliseconds),
    ("setUTCMilliseconds", 1, Setter::UtcMilliseconds),
    ("setSeconds", 2, Setter::Seconds),
    ("setUTCSeconds", 2, Setter::UtcSeconds),
    ("setMinutes", 3, Setter::Minutes),
    ("setUTCMinutes", 3, Setter::UtcMinutes),
    ("setHours", 4, Setter::Hours),
    ("setUTCHours", 4, Setter::UtcHours),
    ("setDate", 1, Setter::Date),
    ("setUTCDate", 1, Setter::UtcDate),
    ("setMonth", 2, Setter::Month),
    ("setUTCMonth", 2, Setter::UtcMonth),
    ("setFullYear", 3, Setter::FullYear),
    ("setUTCFullYear", 3, Setter::UtcFullYear),
];

fn self_date(this: &Value, name: &str) -> R<Obj> {
    receiver(this, |obj| obj.date().is_some(), &format!("Date.prototype.{name}"))
}

fn time_of(this: &Value, name: &str) -> R<f64> {
    Ok(self_date(this, name)?.date().unwrap().get())
}

fn iso(time: f64) -> R<String> {
    jsdate::to_iso_string(time).ok_or_else(|| range_error("Invalid time value."))
}

pub fn date_global(ctx: &Ctx) -> Value {
    let proto = ctx.builtins.date.clone();
    let date = ctx.constructor(
        &proto,
        NativeOptions {
            name: "Date".into(),
            length: 7,
            // ISO instead of the host's locale string: date strings are deterministic.
            call: Rc::new(|_, _, _, _| Ok(NOut::Val(Value::str(&jsdate::to_iso_string(jsdate::now()).unwrap_or_default())))),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                let proto = prototype_from(&new_target, &ctx.builtins.date);
                Ok(NOut::Work(boxed(construct(ctx.clone(), args, proto))))
            })),
            callback: true,
        },
    );
    ctx.method(&date, "now", 0, |_, _, _, _| Ok(Value::Num(jsdate::now())));
    ctx.method(&date, "parse", 1, |_, _, args, _| {
        Ok(Value::Num(jsdate::parse(to_string_plain(&args.first().cloned().unwrap_or_default()).units())))
    });
    ctx.method_work(&date, "UTC", 7, |ctx, _, args, _| {
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let parts = to_numbers(&ctx, &args[..args.len().min(7)]).await?;
            Ok(Value::Num(jsdate::utc(&parts)))
        })))
    });
    ctx.method(&proto, "getTime", 0, |_, this, _, _| Ok(Value::Num(time_of(&this, "getTime")?)));
    ctx.method(&proto, "valueOf", 0, |_, this, _, _| Ok(Value::Num(time_of(&this, "valueOf")?)));
    ctx.method(&proto, "toISOString", 0, |_, this, _, _| Ok(Value::str(&iso(time_of(&this, "toISOString")?)?)));
    ctx.method(&proto, "toJSON", 1, |_, this, _, _| {
        let time = time_of(&this, "toJSON")?;
        if !time.is_finite() {
            return Ok(Value::Null);
        }
        Ok(Value::str(&iso(time)?))
    });
    ctx.method(&proto, "toString", 0, |_, this, _, _| {
        Ok(Value::Str(crate::interp::coerce::obj_to_string(&self_date(&this, "toString")?)))
    });
    let formats: [(&str, fn(f64) -> String); 7] = [
        ("toDateString", jsdate::to_date_string),
        ("toTimeString", jsdate::to_time_string),
        ("toUTCString", jsdate::to_utc_string),
        ("toGMTString", jsdate::to_utc_string),
        ("toLocaleString", jsdate::to_locale_string),
        ("toLocaleDateString", jsdate::to_locale_date_string),
        ("toLocaleTimeString", jsdate::to_locale_time_string),
    ];
    for (name, format) in formats {
        ctx.method(&proto, name, 0, move |_, this, _, _| Ok(Value::str(&format(time_of(&this, name)?))));
    }
    for (name, getter) in GETTERS {
        ctx.method(&proto, name, 0, move |_, this, _, _| Ok(Value::Num(jsdate::get(time_of(&this, name)?, getter))));
    }
    for (name, length, setter) in SETTERS {
        ctx.method_work(&proto, name, length, move |ctx, this, args, _| {
            let target = self_date(&this, name)?;
            // Native setters read the current time before argument coercion, whose callbacks may mutate the Date.
            let hosted = target.date().unwrap().get();
            let ctx = ctx.clone();
            Ok(NOut::Work(boxed(async move {
                let values = to_numbers(&ctx, &args[..args.len().min(length as usize)]).await?;
                // Every setter but setTime and setFullYear leaves an invalid Date untouched and answers NaN.
                if hosted.is_nan() && !matches!(setter, Setter::Time | Setter::FullYear | Setter::UtcFullYear) {
                    return Ok(Value::Num(f64::NAN));
                }
                let time = jsdate::set(hosted, setter, &values);
                target.date().unwrap().set(time);
                Ok(Value::Num(time))
            })))
        });
    }
    Value::Obj(date)
}
