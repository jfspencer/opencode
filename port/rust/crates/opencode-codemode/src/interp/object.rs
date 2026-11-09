//! Property operations on program objects (objects.ts): [[Get]], [[Set]], [[DefineOwnProperty]], deletion, and key
//! enumeration in JS order, with arrays and Uint8Arrays keeping their indexes as live elements.

use crate::interp::errors::type_error;
use crate::interp::limits::check_array_length;
use crate::interp::value::*;
use crate::interp::{Ctx, R};
use crate::jsstr::JsStr;

fn indexed(target: &Obj) -> bool {
    matches!(target.kind(), Kind::Array(_) | Kind::Bytes(_))
}

/// The own property under `key`, including an array's live indexes and `length`.
pub fn own(target: &Obj, key: &PropKey) -> Option<Slot> {
    match (target.kind(), key) {
        (Kind::Array(data), PropKey::Index(index)) => {
            let value = data.get(*index as usize)?;
            return Some(Slot::data(value, data.elements.get()));
        }
        (Kind::Bytes(data), PropKey::Index(index)) => {
            let value = data.get(*index as usize)?;
            return Some(Slot::data(Value::Num(value as f64), DATA));
        }
        (Kind::Array(data), PropKey::Str(name)) if name.eq_str("length") => {
            return Some(Slot::Data {
                value: Value::Num(data.len() as f64),
                writable: data.elements.get().writable,
                enumerable: false,
                configurable: false,
            });
        }
        _ => {}
    }
    target.0.props.borrow().get(key).cloned()
}

pub fn has_own(target: &Obj, key: &PropKey) -> bool {
    match (target.kind(), key) {
        (Kind::Array(data), PropKey::Index(index)) => data.has(*index as usize),
        (Kind::Bytes(data), PropKey::Index(index)) => (*index as usize) < data.length,
        (Kind::Array(_), PropKey::Str(name)) if name.eq_str("length") => true,
        _ => target.0.props.borrow().contains_key(key),
    }
}

fn read(ctx: &Ctx, slot: Slot, receiver: &Value) -> R<Value> {
    match slot {
        Slot::Data { value, .. } => Ok(value),
        Slot::Accessor { get: None, .. } => Ok(Value::Undefined),
        Slot::Accessor { get: Some(get), .. } => get(ctx, receiver),
    }
}

pub fn get_own(ctx: &Ctx, target: &Obj, key: &PropKey) -> R<Value> {
    match own(target, key) {
        None => Ok(Value::Undefined),
        Some(slot) => read(ctx, slot, &Value::Obj(target.clone())),
    }
}

/// [[Get]]: walks the prototype chain; accessors see `receiver`.
pub fn get_with(ctx: &Ctx, target: &Obj, key: &PropKey, receiver: &Value) -> R<Value> {
    let mut current = Some(target.clone());
    while let Some(obj) = current {
        if let Some(slot) = own(&obj, key) {
            return read(ctx, slot, receiver);
        }
        current = obj.proto();
    }
    Ok(Value::Undefined)
}

pub fn get(ctx: &Ctx, target: &Obj, key: &PropKey) -> R<Value> {
    get_with(ctx, target, key, &Value::Obj(target.clone()))
}

/// [[Get]] for data properties only (built-in code that knows no accessor is involved).
pub fn get_data(target: &Obj, key: &PropKey) -> Value {
    let mut current = Some(target.clone());
    while let Some(obj) = current {
        if let Some(slot) = own(&obj, key) {
            return match slot {
                Slot::Data { value, .. } => value,
                Slot::Accessor { .. } => Value::Undefined,
            };
        }
        current = obj.proto();
    }
    Value::Undefined
}

pub fn get_str(ctx: &Ctx, target: &Obj, key: &str) -> R<Value> {
    get(ctx, target, &PropKey::from_str(key))
}

pub fn has(target: &Obj, key: &PropKey) -> bool {
    let mut current = Some(target.clone());
    while let Some(obj) = current {
        if has_own(&obj, key) {
            return true;
        }
        current = obj.proto();
    }
    false
}

pub fn has_prototype(value: &Value, proto: &Obj) -> bool {
    let Value::Obj(obj) = value else { return false };
    let mut current = obj.proto();
    while let Some(level) = current {
        if level.ptr_eq(proto) {
            return true;
        }
        current = level.proto();
    }
    false
}

/// Creating a property on a non-extensible object is the one [[Set]] failure with its own message.
pub fn reject_addition(target: &Obj, key: &dyn std::fmt::Display) -> R<()> {
    if target.extensible() {
        return Ok(());
    }
    Err(type_error(format!("Cannot add property {key}, object is not extensible.")))
}

fn write_element(target: &Obj, key: &PropKey, value: &Value) -> R<Option<bool>> {
    match (target.kind(), key) {
        (Kind::Bytes(data), PropKey::Index(index)) => {
            let number = match value {
                Value::Num(n) => *n,
                other => crate::interp::coerce::to_number_plain(other),
            };
            data.set(*index as usize, crate::interp::coerce::to_uint8(number));
            Ok(Some(true))
        }
        (Kind::Array(data), PropKey::Index(index)) => {
            if !data.has(*index as usize) {
                reject_addition(target, index)?;
            }
            data.set(*index as usize, value.clone());
            Ok(Some(true))
        }
        (Kind::Array(data), PropKey::Str(name)) if name.eq_str("length") => {
            let length = match value {
                Value::Num(n) => *n,
                other => crate::interp::coerce::to_number_plain(other),
            };
            if length.fract() != 0.0 || !length.is_finite() || length < 0.0 {
                return Err(crate::interp::errors::range_error("Invalid array length"));
            }
            if (length as usize) < data.len() && !data.elements.get().configurable {
                return Ok(Some(false));
            }
            check_array_length(length)?;
            data.set_len(length as usize);
            Ok(Some(true))
        }
        _ => Ok(None),
    }
}

/// [[Set]]: an inherited setter or read-only property decides before an own data property is created.
pub fn set(ctx: &Ctx, target: &Obj, key: &PropKey, value: Value) -> R<bool> {
    let mut current = Some(target.clone());
    while let Some(obj) = current {
        let Some(slot) = own(&obj, key) else {
            current = obj.proto();
            continue;
        };
        match slot {
            Slot::Accessor { set: None, .. } => return Ok(false),
            Slot::Accessor { set: Some(setter), .. } => {
                setter(ctx, &Value::Obj(target.clone()), value)?;
                return Ok(true);
            }
            Slot::Data { writable, .. } => {
                if !writable {
                    return Ok(false);
                }
                if !obj.ptr_eq(target) {
                    break;
                }
                if indexed(target)
                    && let Some(written) = write_element(target, key, &value)?
                {
                    return Ok(written);
                }
                if let Some(Slot::Data { value: slot_value, .. }) = target.0.props.borrow_mut().get_mut(key) {
                    *slot_value = value;
                }
                return Ok(true);
            }
        }
    }
    if indexed(target)
        && let Some(written) = write_element(target, key, &value)?
    {
        return Ok(written);
    }
    reject_addition(target, &key.to_js())?;
    target.0.props.borrow_mut().insert(key.clone(), Slot::data(value, DATA));
    Ok(true)
}

/// [[DefineOwnProperty]] for a data property, ignoring the chain.
pub fn define(target: &Obj, key: PropKey, value: Value, attrs: Attrs) {
    if indexed(target) {
        match (target.kind(), &key) {
            (Kind::Array(data), PropKey::Index(index)) => {
                data.set(*index as usize, value);
                return;
            }
            (Kind::Bytes(data), PropKey::Index(index)) => {
                let number = match &value {
                    Value::Num(n) => *n,
                    other => crate::interp::coerce::to_number_plain(other),
                };
                data.set(*index as usize, crate::interp::coerce::to_uint8(number));
                return;
            }
            (Kind::Array(data), PropKey::Str(name)) if name.eq_str("length") => {
                if let Value::Num(n) = value
                    && n >= 0.0
                    && n.fract() == 0.0
                {
                    data.set_len(n as usize);
                }
                return;
            }
            _ => {}
        }
    }
    target.0.props.borrow_mut().insert(key, Slot::data(value, attrs));
}

pub fn define_str(target: &Obj, key: &str, value: Value, attrs: Attrs) {
    define(target, PropKey::from_str(key), value, attrs)
}

pub fn define_accessor(target: &Obj, key: &str, get: Option<Getter>, set: Option<Setter>) {
    target
        .0
        .props
        .borrow_mut()
        .insert(PropKey::from_str(key), Slot::Accessor { get, set, enumerable: false, configurable: true });
}

pub fn remove(target: &Obj, key: &PropKey) -> bool {
    match (target.kind(), key) {
        (Kind::Bytes(data), PropKey::Index(index)) => return (*index as usize) >= data.length,
        (Kind::Array(data), PropKey::Index(index)) => {
            if data.has(*index as usize) && !data.elements.get().configurable {
                return false;
            }
            data.delete(*index as usize);
            return true;
        }
        (Kind::Array(_), PropKey::Str(name)) if name.eq_str("length") => return false,
        _ => {}
    }
    let mut props = target.0.props.borrow_mut();
    let Some(slot) = props.get(key) else { return true };
    if !slot.configurable() {
        return false;
    }
    props.shift_remove(key);
    true
}

/// JS order: array indexes, integer-like keys ascending, other strings, then symbols.
pub fn own_keys(target: &Obj) -> Vec<PropKey> {
    let mut out = Vec::new();
    match target.kind() {
        Kind::Array(data) => {
            let items = data.items.borrow();
            for (index, item) in items.iter().enumerate() {
                if item.is_some() {
                    out.push(PropKey::Index(index as u32));
                }
            }
            drop(items);
            out.push(PropKey::from_str("length"));
        }
        Kind::Bytes(data) => {
            for index in 0..data.length {
                out.push(PropKey::Index(index as u32));
            }
        }
        _ => {}
    }
    let props = target.0.props.borrow();
    let mut indexes: Vec<u32> =
        props.keys().filter_map(|key| if let PropKey::Index(i) = key { Some(*i) } else { None }).collect();
    indexes.sort_unstable();
    out.extend(indexes.into_iter().map(PropKey::Index));
    out.extend(props.keys().filter(|key| matches!(key, PropKey::Str(_))).cloned());
    out.extend(props.keys().filter(|key| matches!(key, PropKey::Sym(_))).cloned());
    out
}

fn enumerable(target: &Obj, key: &PropKey) -> bool {
    own(target, key).is_some_and(|slot| slot.enumerable())
}

/// Own enumerable keys, including the iterator symbols; what spread and `Object.assign` copy.
pub fn enumerable_keys(target: &Obj) -> Vec<PropKey> {
    own_keys(target).into_iter().filter(|key| enumerable(target, key)).collect()
}

/// Own enumerable string keys: `Object.keys`.
pub fn keys(target: &Obj) -> Vec<PropKey> {
    own_keys(target).into_iter().filter(|key| !key.is_symbol() && enumerable(target, key)).collect()
}

/// Own enumerable string entries: `Object.entries` and serialization.
pub fn entries(ctx: &Ctx, target: &Obj) -> R<Vec<(PropKey, Value)>> {
    let mut out = Vec::new();
    for key in keys(target) {
        let value = get_own(ctx, target, &key)?;
        out.push((key, value));
    }
    Ok(out)
}

pub fn assign(ctx: &Ctx, target: &Obj, source: &Obj, skip: Option<&[PropKey]>) -> R<()> {
    for key in enumerable_keys(source) {
        if skip.is_some_and(|skip| skip.contains(&key)) {
            continue;
        }
        let value = get_own(ctx, source, &key)?;
        set(ctx, target, &key, value)?;
    }
    Ok(())
}

pub fn key_js(key: &PropKey) -> JsStr {
    key.to_js()
}
