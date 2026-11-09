//! `Function` (call/apply/bind; dynamic code is unsupported) and the confined `Symbol` (globals.ts).

use crate::interp::coerce::{to_integer, to_string_plain};
use crate::interp::errors::type_error;
use crate::interp::limits::check_array_length;
use crate::interp::native::{NativeOptions, receiver};
use crate::interp::object::get;
use crate::interp::value::*;
use crate::interp::{Ctx, R};
use crate::jsstr::JsStrBuf;
use std::rc::Rc;

fn target(this: &Value, method: &str) -> R<Obj> {
    receiver(this, |obj| obj.is_callable(), &format!("Function.prototype.{method}"))
}

// CreateListFromArrayLike: `apply` reads `length` and the indexed properties of any object.
fn list_from_array_like(ctx: &Ctx, value: &Value) -> R<Vec<Value>> {
    match value {
        Value::Undefined | Value::Null => Ok(Vec::new()),
        Value::Obj(obj) => {
            if let Some(data) = obj.array() {
                return Ok(data.values());
            }
            let length = to_integer(&get(ctx, obj, &PropKey::from_str("length"))?).max(0.0);
            check_array_length(length)?;
            let mut out = Vec::with_capacity(length as usize);
            for index in 0..length as u32 {
                out.push(get(ctx, obj, &PropKey::Index(index))?);
            }
            Ok(out)
        }
        _ => Err(type_error("Function.prototype.apply expects an array-like argument list.")),
    }
}

pub fn function_global(ctx: &Ctx) -> Value {
    let proto = ctx.builtins.function.clone();
    ctx.method_work(&proto, "call", 1, |ctx, this, args, site| {
        let function = target(&this, "call")?;
        let receiver = args.first().cloned().unwrap_or_default();
        let rest = args.get(1..).map(|r| r.to_vec()).unwrap_or_default();
        Ok(NOut::Work(ctx.call(Value::Obj(function), receiver, rest, site)))
    });
    ctx.method_work(&proto, "apply", 2, |ctx, this, args, site| {
        let function = target(&this, "apply")?;
        let receiver = args.first().cloned().unwrap_or_default();
        let list = list_from_array_like(ctx, &args.get(1).cloned().unwrap_or_default())?;
        Ok(NOut::Work(ctx.call(Value::Obj(function), receiver, list, site)))
    });
    ctx.method(&proto, "bind", 1, |ctx, this, args, _| {
        let function = target(&this, "bind")?;
        let bound: Vec<Value> = args.get(1..).map(|r| r.to_vec()).unwrap_or_default();
        let receiver = args.first().cloned().unwrap_or_default();
        let mut name = JsStrBuf::new();
        name.push_str("bound ");
        name.push_js(&to_string_plain(&crate::interp::object::get_data(&function, &PropKey::from_str("name"))));
        let length = match crate::interp::object::get_data(&function, &PropKey::from_str("length")) {
            Value::Num(n) => n,
            _ => 0.0,
        };
        let length = (length - bound.len() as f64).max(0.0);
        let options = NativeOptions {
            name: String::new(),
            length: length as u32,
            call: Rc::new(move |ctx, _, rest, site| {
                let mut all = bound.clone();
                all.extend(rest);
                Ok(NOut::Work(ctx.call(Value::Obj(function.clone()), receiver.clone(), all, site)))
            }),
            construct: None,
            callback: true,
        };
        let native = ctx.new_native(options);
        crate::interp::object::define(&native, PropKey::from_str("name"), Value::Str(name.finish()), READONLY);
        Ok(Value::Obj(native))
    });
    let reject = || -> NativeCall {
        Rc::new(|_, _, _, _| Err(type_error("The Function constructor is not supported; write the function inline.")))
    };
    let ctor = ctx.constructor(
        &proto,
        NativeOptions {
            name: "Function".into(),
            length: 1,
            call: reject(),
            construct: Some(Rc::new(|_, _, _, _| {
                Err(type_error("The Function constructor is not supported; write the function inline."))
            })),
            callback: true,
        },
    );
    Value::Obj(ctor)
}

pub fn symbol_global(ctx: &Ctx) -> Value {
    let symbol = ctx.new_native(NativeOptions {
        name: "Symbol".into(),
        length: 0,
        call: Rc::new(|_, _, _, _| {
            Err(type_error("Symbol is not callable; only Symbol.asyncIterator and Symbol.iterator are available."))
        }),
        construct: None,
        callback: false,
    });
    ctx.constants(&symbol, &[("asyncIterator", Value::Sym(Sym::AsyncIterator)), ("iterator", Value::Sym(Sym::Iterator))]);
    Value::Obj(symbol)
}
