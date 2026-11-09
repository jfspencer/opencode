//! `atob`/`btoa`, `structuredClone`, and `crypto` (web.ts).

use crate::interp::coerce::{describe_value, is_runtime_reference, to_string_plain};
use crate::interp::errors::{ErrorType, dom_exception, type_error};
use crate::interp::object::{define, entries, get, get_own, has_own};
use crate::interp::value::*;
use crate::interp::{Ctx, R};
use crate::jsstr::JsStr;
use crate::stdlib::bytes::{base64_decode, base64_encode, new_bytes};
use std::rc::Rc;

pub fn base64_global(ctx: &Ctx, name: &'static str) -> Value {
    Value::Obj(ctx.native_fn(name, 1, move |_, _, args, _| {
        if args.is_empty() {
            return Err(type_error(format!("{name} requires 1 argument (a string)")));
        }
        let input = to_string_plain(&args[0]);
        let invalid = || dom_exception("InvalidCharacterError", "The string contains invalid characters.");
        if name == "atob" {
            let bytes = base64_decode(input.units()).ok_or_else(invalid)?;
            return Ok(Value::Str(JsStr::from_units(bytes.into_iter().map(|b| b as u16).collect())));
        }
        let mut bytes = Vec::with_capacity(input.len());
        for &unit in input.units() {
            if unit > 0xFF {
                return Err(invalid());
            }
            bytes.push(unit as u8);
        }
        Ok(Value::str(&base64_encode(&bytes)))
    }))
}

fn clone(ctx: &Ctx, value: &Value, memo: &mut Vec<(usize, Obj)>) -> R<Value> {
    let cannot = |value: &Value| type_error(format!("DataCloneError: {} could not be cloned.", describe_value(value)));
    let obj = match value {
        Value::Sym(_) | Value::Tool(_) => return Err(cannot(value)),
        Value::Obj(obj) => obj,
        other => return Ok(other.clone()),
    };
    if let Some((_, copy)) = memo.iter().find(|(addr, _)| *addr == obj.addr()) {
        return Ok(Value::Obj(copy.clone()));
    }
    let builtins = &ctx.builtins;
    let copy = match obj.kind() {
        Kind::Map(map) => {
            let copy = ctx.new_object(Some(builtins.map.clone()), Kind::Map(Default::default()));
            memo.push((obj.addr(), copy.clone()));
            for (key, item) in map.borrow().entries() {
                let (key, item) = (clone(ctx, &key, memo)?, clone(ctx, &item, memo)?);
                if let Kind::Map(target) = copy.kind() {
                    target.borrow_mut().set(key, item);
                }
            }
            return Ok(Value::Obj(copy));
        }
        Kind::Set(set) => {
            let copy = ctx.new_object(Some(builtins.set.clone()), Kind::Set(Default::default()));
            memo.push((obj.addr(), copy.clone()));
            for item in set.borrow().values() {
                let item = clone(ctx, &item, memo)?;
                if let Kind::Set(target) = copy.kind() {
                    target.borrow_mut().add(item);
                }
            }
            return Ok(Value::Obj(copy));
        }
        Kind::Date(time) => ctx.new_object(Some(builtins.date.clone()), Kind::Date(std::cell::Cell::new(time.get()))),
        Kind::RegExp(regex) => {
            let copy = regex.clone_regexp();
            ctx.new_object(Some(builtins.regexp.clone()), Kind::RegExp(Rc::new(copy)))
        }
        Kind::Bytes(data) => new_bytes(ctx, builtins.uint8_array.clone(), data.to_vec()),
        Kind::Error(_) => {
            let name = get(ctx, obj, &PropKey::from_str("name"))?;
            let message = get(ctx, obj, &PropKey::from_str("message"))?;
            let ty = match &name {
                Value::Str(text) => text.to_rust().as_deref().and_then(ErrorType::from_name).unwrap_or(ErrorType::Error),
                _ => ErrorType::Error,
            };
            let message = if message.is_undefined() { None } else { Some(to_string_plain(&message)) };
            let copy = ctx.create_error(ty, message);
            memo.push((obj.addr(), copy.clone()));
            if let Value::Str(stack) = get_own(ctx, obj, &PropKey::from_str("stack"))? {
                define(&copy, PropKey::from_str("stack"), Value::Str(stack), HIDDEN);
            }
            if has_own(obj, &PropKey::from_str("cause")) {
                let cause = clone(ctx, &get_own(ctx, obj, &PropKey::from_str("cause"))?, memo)?;
                define(&copy, PropKey::from_str("cause"), cause, HIDDEN);
            }
            return Ok(Value::Obj(copy));
        }
        _ => {
            if is_runtime_reference(value) || matches!(obj.kind(), Kind::WeakMap(_) | Kind::WeakSet(_)) {
                return Err(cannot(value));
            }
            let copy = match obj.array() {
                Some(data) => {
                    let copy = ctx.new_array(Vec::new());
                    copy.array().unwrap().set_len(data.len());
                    copy
                }
                None => ctx.new_plain(),
            };
            memo.push((obj.addr(), copy.clone()));
            for (key, item) in entries(ctx, obj)? {
                let item = clone(ctx, &item, memo)?;
                define(&copy, key, item, DATA);
            }
            return Ok(Value::Obj(copy));
        }
    };
    memo.push((obj.addr(), copy.clone()));
    Ok(Value::Obj(copy))
}

pub fn structured_clone_global(ctx: &Ctx) -> Value {
    Value::Obj(ctx.native_fn("structuredClone", 1, |ctx, _, args, _| {
        if args.is_empty() {
            return Err(type_error("structuredClone requires 1 argument"));
        }
        clone(ctx, &args[0], &mut Vec::new())
    }))
}

pub fn crypto_global(ctx: &Ctx) -> Value {
    let object = ctx.new_plain();
    ctx.method(&object, "randomUUID", 0, |_, _, _, _| {
        let mut bytes = [0u8; 16];
        let _ = getrandom::fill(&mut bytes);
        bytes[6] = (bytes[6] & 0x0F) | 0x40;
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let text = format!(
            "{}-{}-{}-{}-{}",
            hex[0..4].concat(),
            hex[4..6].concat(),
            hex[6..8].concat(),
            hex[8..10].concat(),
            hex[10..16].concat()
        );
        Ok(Value::str(&text))
    });
    ctx.method(&object, "getRandomValues", 1, |_, _, args, _| {
        let target = args.first().cloned().unwrap_or_default();
        let Value::Obj(obj) = &target else {
            return Err(type_error(format!("crypto.getRandomValues expects a Uint8Array, received {}.", describe_value(&target))));
        };
        let Some(data) = obj.bytes() else {
            return Err(type_error(format!("crypto.getRandomValues expects a Uint8Array, received {}.", describe_value(&target))));
        };
        let mut bytes = vec![0u8; data.length];
        let _ = getrandom::fill(&mut bytes);
        for (index, byte) in bytes.into_iter().enumerate() {
            data.set(index, byte);
        }
        Ok(target)
    });
    Value::Obj(object)
}
