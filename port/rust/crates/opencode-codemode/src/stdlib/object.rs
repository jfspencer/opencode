//! `Object` and `Object.prototype` (object.ts).

use crate::interp::callback::{invoke, preserve_consumer_error};
use crate::interp::coerce::{CircularCheck, contains_opaque_reference, describe_value, to_string_plain};
use crate::interp::errors::{invalid_data, type_error};
use crate::interp::native::{NativeOptions, receiver};
use crate::interp::object::*;
use crate::interp::value::*;
use crate::interp::{Ctx, Loc, R, boxed};
use crate::jsstr::JsStr;
use std::rc::Rc;

// ToObject for enumeration.
pub fn enumerable_source(ctx: &Ctx, label: &str, value: &Value, loc: Option<Loc>) -> R<Obj> {
    match value {
        Value::Undefined | Value::Null => {
            Err(type_error(format!("{label} cannot convert {} to an object.", describe_value(value))).at(loc))
        }
        Value::Obj(obj) if obj.is_promise() => Err(invalid_data(format!(
            "{label} received an un-awaited Promise; await it before inspecting the result."
        ))
        .at(loc)),
        Value::Tool(_) => Err(invalid_data(format!(
            "{label} cannot read tool references: they are not plain data. Use Object.keys(tools) for names, or search({{ query }}) for signatures."
        ))
        .at(loc)),
        Value::Str(text) => {
            let chars: Vec<Value> = text.code_points().map(|(_, start, width)| Value::Str(text.slice(start, start + width))).collect();
            Ok(ctx.new_array(chars))
        }
        Value::Obj(obj) => Ok(obj.clone()),
        _ => Ok(ctx.new_plain()),
    }
}

fn object_assign(ctx: &Ctx, args: &[Value]) -> R<Value> {
    let Some(Value::Obj(target)) = args.first() else {
        let target = args.first().cloned().unwrap_or_default();
        return Err(type_error(format!(
            "Object.assign expects a data object or array target, received {}.",
            describe_value(&target)
        )));
    };
    let mut check = CircularCheck::new();
    for source in &args[1..] {
        if source.is_nullish() {
            continue;
        }
        let from = enumerable_source(ctx, "Object.assign(...)", source, None)?;
        for key in enumerable_keys(&from) {
            let value = get_own(ctx, &from, &key)?;
            check.check(target, &value, "Object.assign result")?;
            if !set(ctx, target, &key, value)? {
                let mut message = crate::jsstr::JsStrBuf::new();
                message.push_str("Cannot assign to read only property '");
                message.push_js(&key.to_js());
                message.push_str("'.");
                return Err(type_error(message.finish()));
            }
        }
    }
    Ok(Value::Obj(target.clone()))
}

async fn object_from_entries(ctx: Ctx, source: Value) -> R<Value> {
    let out = ctx.new_plain();
    let Some(cursor) = ctx.iterate(&source, None).await? else {
        return Err(type_error("Object.fromEntries expects a synchronous iterable of entries."));
    };
    loop {
        let step = cursor.next_step(&ctx).await?;
        if step.done {
            return Ok(Value::Obj(out));
        }
        let entry = step.value;
        let defined = (|| -> R<()> {
            let Value::Obj(entry) = &entry else {
                return Err(type_error("Object.fromEntries expects [key, value] entry objects."));
            };
            if contains_opaque_reference(&Value::Obj(entry.clone())) {
                return Err(type_error("Object.fromEntries expects [key, value] entry objects."));
            }
            let key = to_string_plain(&get_own(&ctx, entry, &PropKey::Index(0))?);
            let value = get_own(&ctx, entry, &PropKey::Index(1))?;
            define(&out, PropKey::from_js(key), value, DATA);
            Ok(())
        })();
        let close = {
            let (ctx, cursor) = (ctx.clone(), cursor.clone());
            async move { cursor.close_now(&ctx).await }
        };
        preserve_consumer_error(close, defined).await?;
    }
}

fn class_tag(value: &Value) -> &'static str {
    match value {
        Value::Null => "Null",
        Value::Undefined => "Undefined",
        Value::Obj(obj) => obj.tag(),
        Value::Str(_) => "String",
        Value::Num(_) => "Number",
        Value::Bool(_) => "Boolean",
        _ => "Object",
    }
}

pub fn object_property_key(value: &Value) -> PropKey {
    match value {
        Value::Sym(sym) => PropKey::Sym(*sym),
        other => PropKey::from_js(to_string_plain(other)),
    }
}

// SetIntegrityLevel: primitives pass through.
fn restrict(level: &str, value: &Value) -> R<Value> {
    let Value::Obj(obj) = value else { return Ok(value.clone()) };
    obj.0.extensible.set(false);
    if level == "preventExtensions" {
        return Ok(value.clone());
    }
    if let Some(data) = obj.bytes()
        && data.length > 0
    {
        return Err(type_error(format!("Cannot {level} array buffer views with elements.")));
    }
    for slot in obj.0.props.borrow_mut().values_mut() {
        match slot {
            Slot::Data { writable, configurable, .. } => {
                *configurable = false;
                if level == "freeze" {
                    *writable = false;
                }
            }
            Slot::Accessor { configurable, .. } => *configurable = false,
        }
    }
    if let Some(data) = obj.array() {
        data.elements.set(Attrs { writable: level == "seal", enumerable: true, configurable: false });
    }
    Ok(value.clone())
}

// TestIntegrityLevel.
fn integrity(value: &Value, frozen: bool) -> bool {
    let Value::Obj(obj) = value else { return true };
    if obj.extensible() {
        return false;
    }
    own_keys(obj).iter().all(|key| match own(obj, key) {
        Some(Slot::Data { writable, configurable, .. }) => !configurable && !(frozen && writable),
        Some(Slot::Accessor { configurable, .. }) => !configurable,
        None => false,
    })
}

pub fn object_global(ctx: &Ctx) -> Value {
    let builtins = &ctx.builtins;
    let construct = |ctx: &Ctx, args: &[Value]| -> R<Value> {
        match args.first() {
            None | Some(Value::Undefined) | Some(Value::Null) => Ok(Value::Obj(ctx.new_plain())),
            Some(Value::Obj(obj)) => Ok(Value::Obj(obj.clone())),
            Some(other) => Err(type_error(format!(
                "Object({}) wrapper objects are not supported; use the primitive value directly.",
                crate::interp::coerce::typeof_value(other)
            ))),
        }
    };
    let object = ctx.constructor(
        &builtins.object,
        NativeOptions {
            name: "Object".into(),
            length: 1,
            call: Rc::new(move |ctx, _, args, _| construct(ctx, &args).map(NOut::Val)),
            construct: Some(Rc::new(move |ctx, args, _, _| construct(ctx, &args).map(NOut::Val))),
            callback: true,
        },
    );
    ctx.method(&object, "keys", 1, |ctx, _, args, _| {
        let first = args.first().cloned().unwrap_or_default();
        let names: Vec<Value> = match &first {
            Value::Tool(tool) => ctx.tools.keys(tool.path())?.into_iter().map(Value::Str).collect(),
            other => keys(&enumerable_source(ctx, "Object.keys(...)", other, None)?)
                .into_iter()
                .map(|key| Value::Str(key.to_js()))
                .collect(),
        };
        Ok(Value::Obj(ctx.new_array(names)))
    });
    ctx.method(&object, "values", 1, |ctx, _, args, _| {
        let source = enumerable_source(ctx, "Object.values(...)", &args.first().cloned().unwrap_or_default(), None)?;
        let values = entries(ctx, &source)?.into_iter().map(|(_, value)| value).collect();
        Ok(Value::Obj(ctx.new_array(values)))
    });
    ctx.method(&object, "entries", 1, |ctx, _, args, _| {
        let source = enumerable_source(ctx, "Object.entries(...)", &args.first().cloned().unwrap_or_default(), None)?;
        let pairs = entries(ctx, &source)?
            .into_iter()
            .map(|(key, value)| Value::Obj(ctx.new_array(vec![Value::Str(key.to_js()), value])))
            .collect();
        Ok(Value::Obj(ctx.new_array(pairs)))
    });
    ctx.method(&object, "hasOwn", 2, |ctx, _, args, _| {
        let source = enumerable_source(ctx, "Object.hasOwn(...)", &args.first().cloned().unwrap_or_default(), None)?;
        Ok(Value::Bool(has_own(&source, &object_property_key(&args.get(1).cloned().unwrap_or_default()))))
    });
    ctx.method(&object, "is", 2, |_, _, args, _| {
        let a = args.first().cloned().unwrap_or_default();
        let b = args.get(1).cloned().unwrap_or_default();
        Ok(Value::Bool(a.same_value(&b)))
    });
    ctx.method(&object, "assign", 2, |ctx, _, args, _| object_assign(ctx, &args));
    ctx.method_work(&object, "fromEntries", 1, |ctx, _, args, _| {
        Ok(NOut::Work(boxed(object_from_entries(ctx.clone(), args.into_iter().next().unwrap_or_default()))))
    });
    for level in ["freeze", "seal", "preventExtensions"] {
        ctx.method(&object, level, 1, move |_, _, args, _| restrict(level, &args.first().cloned().unwrap_or_default()));
    }
    ctx.method(&object, "isFrozen", 1, |_, _, args, _| Ok(Value::Bool(integrity(&args.first().cloned().unwrap_or_default(), true))));
    ctx.method(&object, "isSealed", 1, |_, _, args, _| Ok(Value::Bool(integrity(&args.first().cloned().unwrap_or_default(), false))));
    ctx.method(&object, "isExtensible", 1, |_, _, args, _| {
        Ok(Value::Bool(matches!(args.first(), Some(Value::Obj(obj)) if obj.extensible())))
    });
    ctx.method(&object, "getPrototypeOf", 1, |ctx, _, args, _| {
        let value = args.first().cloned().unwrap_or_default();
        if let Value::Obj(obj) = &value {
            return Ok(obj.proto().map(Value::Obj).unwrap_or(Value::Null));
        }
        match ctx.primitive_prototype(&value) {
            Some(proto) => Ok(Value::Obj(proto)),
            None => Err(type_error(format!(
                "Object.getPrototypeOf cannot convert {} to an object.",
                describe_value(&value)
            ))),
        }
    });
    ctx.method(&object, "create", 2, |ctx, _, args, _| {
        let proto = match args.first() {
            Some(Value::Null) => None,
            Some(Value::Obj(obj)) => Some(obj.clone()),
            _ => return Err(type_error("Object prototype may only be an Object or null.")),
        };
        if !matches!(args.get(1), None | Some(Value::Undefined)) {
            return Err(type_error(
                "Object.create property descriptors are not supported; assign the fields after creating the object.",
            ));
        }
        Ok(Value::Obj(ctx.new_object(proto, Kind::Ordinary)))
    });
    let group_by = crate::stdlib::collections::group_by(ctx, "Object");
    define(&object, PropKey::from_str("groupBy"), Value::Obj(group_by), HIDDEN);
    let proto = builtins.object.clone();
    ctx.method(&proto, "hasOwnProperty", 1, |_, this, args, _| {
        let target = receiver(&this, |_| true, "Object.prototype.hasOwnProperty")?;
        Ok(Value::Bool(has_own(&target, &object_property_key(&args.first().cloned().unwrap_or_default()))))
    });
    ctx.method(&proto, "isPrototypeOf", 1, |_, this, args, _| {
        let target = receiver(&this, |_| true, "Object.prototype.isPrototypeOf")?;
        Ok(Value::Bool(has_prototype(&args.first().cloned().unwrap_or_default(), &target)))
    });
    ctx.method(&proto, "propertyIsEnumerable", 1, |_, this, args, _| {
        let target = receiver(&this, |_| true, "Object.prototype.propertyIsEnumerable")?;
        let key = object_property_key(&args.first().cloned().unwrap_or_default());
        Ok(Value::Bool(own(&target, &key).is_some_and(|slot| slot.enumerable())))
    });
    ctx.method(&proto, "toString", 0, |_, this, _, _| Ok(Value::Str(JsStr::from(format!("[object {}]", class_tag(&this))))));
    ctx.method_work(&proto, "toLocaleString", 0, |ctx, this, _, _| {
        Ok(NOut::Work(invoke(ctx, this, "toString", "Object.prototype.toLocaleString")?))
    });
    ctx.method(&proto, "valueOf", 0, |_, this, _, _| {
        if this.is_nullish() {
            return Err(type_error("Object.prototype.valueOf called on null or undefined."));
        }
        Ok(this)
    });
    Value::Obj(object)
}
