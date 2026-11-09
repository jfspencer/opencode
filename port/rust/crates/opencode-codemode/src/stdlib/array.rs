//! `Array` and `Array.prototype` (array.ts), with host array semantics for holes.

use crate::interp::callback::{Apply, Hint, apply_collection_callback, invoke, preserve_consumer_error, with_primitives};
use crate::interp::coerce::{describe_value, reject_circular_insertion, to_integer, to_number_plain, to_string_plain};
use crate::interp::cursor::{Cursor, HostIter, IterKind};
use crate::interp::errors::{invalid_data, range_error, type_error};
use crate::interp::limits::{MAX_ARRAY_LENGTH, check_array_length, check_string_length};
use crate::interp::native::{NativeOptions, prototype_from, receiver};
use crate::interp::object::{define, get, reject_addition};
use crate::interp::value::*;
use crate::interp::{Ctx, Fut, R, boxed};
use crate::jsstr::{JsStr, JsStrBuf};
use std::rc::Rc;

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}

fn opt_number(value: &Value) -> Option<f64> {
    if value.is_undefined() { None } else { Some(to_integer(value)) }
}

/// A relative index as the host array methods resolve one (`slice`, `fill`, `at` ...).
fn relative(value: Option<f64>, len: usize, default: f64) -> usize {
    let n = value.unwrap_or(default);
    let len_f = len as f64;
    if n < 0.0 { (len_f + n).max(0.0) as usize } else { n.min(len_f) as usize }
}

fn compare_text(a: &JsStr, b: &JsStr) -> std::cmp::Ordering {
    a.cmp(b)
}

pub async fn sort_array(ctx: &Ctx, target: Vec<Option<Value>>, comparator: Value, name: &str) -> R<Vec<Value>> {
    let len = target.len();
    if comparator.is_undefined() {
        let mut defined: Vec<(JsStr, Value)> = target
            .into_iter()
            .flatten()
            .filter(|item| !item.is_undefined())
            .map(|item| (to_string_plain(&item), item))
            .collect();
        defined.sort_by(|a, b| compare_text(&a.0, &b.0));
        let mut out: Vec<Value> = defined.into_iter().map(|(_, item)| item).collect();
        out.resize(len, Value::Undefined);
        return Ok(out);
    }
    let apply = apply_collection_callback(ctx, &comparator, name)?;
    let defined: Vec<Value> = target.into_iter().flatten().filter(|item| !item.is_undefined()).collect();
    let mut sorted = merge_sort(&apply, defined).await?;
    sorted.resize(len, Value::Undefined);
    Ok(sorted)
}

fn merge_sort(apply: &Apply, items: Vec<Value>) -> crate::interp::eval::LFut<'_, R<Vec<Value>>> {
    Box::pin(async move {
        if items.len() <= 1 {
            return Ok(items);
        }
        let midpoint = items.len() / 2;
        let right_half = items[midpoint..].to_vec();
        let mut left_half = items;
        left_half.truncate(midpoint);
        let left = merge_sort(apply, left_half).await?;
        let right = merge_sort(apply, right_half).await?;
        let mut merged = Vec::with_capacity(left.len() + right.len());
        let (mut l, mut r) = (0, 0);
        while l < left.len() && r < right.len() {
            // Treat a NaN comparator result as equal to preserve stable ordering.
            let order = to_number_plain(&apply(vec![left[l].clone(), right[r].clone()], Value::Undefined).await?);
            if order.is_nan() || order <= 0.0 {
                merged.push(left[l].clone());
                l += 1;
            } else {
                merged.push(right[r].clone());
                r += 1;
            }
        }
        merged.extend_from_slice(&left[l..]);
        merged.extend_from_slice(&right[r..]);
        Ok(merged)
    })
}

fn self_array(this: &Value, name: &str) -> R<Obj> {
    receiver(this, |obj| obj.is_array(), &format!("Array.prototype.{name}"))
}

// A mutating method fails where its first element write, delete, or `length` write would on a frozen, sealed, or
// non-extensible array.
fn mutable(target: &Obj, growth: Option<i64>) -> R<()> {
    let data = target.array().unwrap();
    let length = data.len();
    let attrs = data.elements.get();
    if growth.unwrap_or(0) > 0 {
        reject_addition(target, &length)?;
    }
    if growth.unwrap_or(0) < 0 && length > 0 && !attrs.configurable {
        return Err(type_error(format!("Cannot delete property '{}'.", length - 1)));
    }
    if !attrs.writable && (length > 0 || growth.is_some()) {
        let key = if length > 0 { "0".to_string() } else { "length".to_string() };
        return Err(type_error(format!("Cannot assign to read only property '{key}'.")));
    }
    Ok(())
}

async fn array_from(ctx: Ctx, args: Vec<Value>) -> R<Value> {
    let source = arg(&args, 0);
    let apply = if args.len() < 2 || args[1].is_undefined() {
        None
    } else {
        Some(apply_collection_callback(&ctx, &args[1], "Array.from")?)
    };
    let this_arg = arg(&args, 2);
    let Some(cursor) = ctx.iterate(&source, None).await? else {
        if matches!(&source, Value::Obj(obj) if obj.generator().is_some()) {
            return Err(type_error(format!(
                "Array.from expects a synchronous iterable or array-like value, received {}.",
                describe_value(&source)
            )));
        }
        // JS would treat a promise as an empty array-like; that would hide a missing `await`.
        let Value::Obj(obj) = &source else {
            return Err(invalid_data(format!(
                "Array.from expects an iterable or array-like value, received {}.",
                describe_value(&source)
            )));
        };
        if obj.is_promise() {
            return Err(invalid_data(format!(
                "Array.from expects an iterable or array-like value, received {}.",
                describe_value(&source)
            )));
        }
        let length = to_integer(&get(&ctx, obj, &PropKey::from_str("length"))?).max(0.0);
        check_array_length(length)?;
        let mut values = Vec::with_capacity(length as usize);
        for index in 0..length as u32 {
            let item = get(&ctx, obj, &PropKey::Index(index))?;
            values.push(match &apply {
                None => item,
                Some(apply) => apply(vec![item, Value::Num(index as f64)], this_arg.clone()).await?,
            });
        }
        return Ok(Value::Obj(ctx.new_array(values)));
    };
    let mut values = Vec::new();
    let mut index = 0;
    loop {
        let step = cursor.next_step(&ctx).await?;
        if step.done {
            return Ok(Value::Obj(ctx.new_array(values)));
        }
        values.push(match &apply {
            None => step.value,
            Some(apply) => {
                let result = apply(vec![step.value, Value::Num(index as f64)], this_arg.clone()).await;
                let close = {
                    let (ctx, cursor) = (ctx.clone(), cursor.clone());
                    async move { cursor.close_now(&ctx).await }
                };
                preserve_consumer_error(close, result).await?
            }
        });
        index += 1;
    }
}

fn construct(ctx: &Ctx, args: &[Value], into: Obj) -> R<Value> {
    if args.len() != 1 {
        return Ok(Value::Obj(ctx.new_array_with_proto(into, args.iter().cloned().map(Some).collect())));
    }
    let Value::Num(first) = args[0] else {
        return Ok(Value::Obj(ctx.new_array_with_proto(into, vec![Some(args[0].clone())])));
    };
    if first.fract() != 0.0 || !first.is_finite() || first < 0.0 || first > MAX_ARRAY_LENGTH as f64 {
        return Err(range_error("Invalid array length."));
    }
    // Sparse like JS: Array(3) has holes.
    let array = ctx.new_array_with_proto(into, Vec::new());
    array.array().unwrap().set_len(first as usize);
    Ok(Value::Obj(array))
}

fn host_iter(ctx: &Ctx, target: Obj, kind: IterKind) -> Value {
    let iter = match target.kind() {
        Kind::Bytes(_) => HostIter::Bytes { target, index: 0, kind },
        _ => HostIter::Array { target, index: 0, kind },
    };
    Value::Obj(ctx.new_object(Some(ctx.builtins.iterator.clone()), Kind::Iterator(Cursor::host(iter))))
}

fn index_of(items: &[Option<Value>], search: &Value, from: Option<f64>, strict: bool) -> f64 {
    let len = items.len();
    let start = relative(from, len, 0.0);
    for (index, item) in items.iter().enumerate().skip(start) {
        match item {
            Some(item) if strict && item.strict_eq(search) => return index as f64,
            Some(item) if !strict && item.same_value_zero(search) => return index as f64,
            None if !strict && search.is_undefined() => return index as f64,
            _ => {}
        }
    }
    -1.0
}

fn join(ctx: &Ctx, parts: Vec<Value>, separator: Value) -> R<NOut> {
    let mut values = vec![separator];
    values.extend(parts);
    with_primitives(ctx, &[Hint::String], values, |_, values| {
        let separator = to_string_plain(&values[0]);
        let mut out = JsStrBuf::new();
        for (index, item) in values[1..].iter().enumerate() {
            if index > 0 {
                out.push_js(&separator);
            }
            out.push_js(&to_string_plain(item));
            check_string_length(out.len() as f64)?;
        }
        Ok(NOut::Val(Value::Str(out.finish())))
    })
}

fn join_parts(data: &ArrData) -> Vec<Value> {
    data.values().into_iter().map(|item| if item.is_nullish() { Value::str("") } else { item }).collect()
}

pub fn array_global(ctx: &Ctx) -> Value {
    let builtins = &ctx.builtins;
    let proto = builtins.array.clone();
    let array = ctx.constructor(
        &proto,
        NativeOptions {
            name: "Array".into(),
            length: 1,
            call: Rc::new(|ctx, _, args, _| construct(ctx, &args, ctx.builtins.array.clone()).map(NOut::Val)),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                construct(ctx, &args, prototype_from(&new_target, &ctx.builtins.array)).map(NOut::Val)
            })),
            callback: true,
        },
    );
    ctx.method(&array, "isArray", 1, |_, _, args, _| Ok(Value::Bool(matches!(args.first(), Some(Value::Obj(o)) if o.is_array()))));
    ctx.method(&array, "of", 0, |ctx, _, args, _| Ok(Value::Obj(ctx.new_array(args))));
    ctx.method_work(&array, "from", 1, |ctx, _, args, _| Ok(NOut::Work(boxed(array_from(ctx.clone(), args)))));

    ctx.method_work(&proto, "join", 1, |ctx, this, args, _| {
        let target = self_array(&this, "join")?;
        let parts = join_parts(target.array().unwrap());
        let separator = match arg(&args, 0) {
            Value::Undefined => Value::str(","),
            other => other,
        };
        join(ctx, parts, separator)
    });
    ctx.method_work(&proto, "toString", 0, |ctx, this, _, _| {
        let target = self_array(&this, "toString")?;
        join(ctx, join_parts(target.array().unwrap()), Value::str(","))
    });
    ctx.method(&proto, "includes", 1, |_, this, args, _| {
        let target = self_array(&this, "includes")?;
        let items = target.array().unwrap().snapshot();
        Ok(Value::Bool(index_of(&items, &arg(&args, 0), opt_number(&arg(&args, 1)), false) >= 0.0))
    });
    ctx.method(&proto, "indexOf", 1, |_, this, args, _| {
        let target = self_array(&this, "indexOf")?;
        let items = target.array().unwrap().snapshot();
        Ok(Value::Num(index_of(&items, &arg(&args, 0), opt_number(&arg(&args, 1)), true)))
    });
    ctx.method(&proto, "lastIndexOf", 1, |_, this, args, _| {
        let target = self_array(&this, "lastIndexOf")?;
        let items = target.array().unwrap().snapshot();
        let len = items.len() as f64;
        // An explicit undefined is a fromIndex of 0, unlike omitting it.
        let from = if args.len() < 2 { len - 1.0 } else { opt_number(&args[1]).unwrap_or(0.0) };
        let start = if from < 0.0 { len + from } else { from.min(len - 1.0) };
        let search = arg(&args, 0);
        let mut index = start;
        while index >= 0.0 {
            if let Some(Some(item)) = items.get(index as usize)
                && item.strict_eq(&search)
            {
                return Ok(Value::Num(index));
            }
            index -= 1.0;
        }
        Ok(Value::Num(-1.0))
    });
    ctx.method(&proto, "at", 1, |_, this, args, _| {
        let target = self_array(&this, "at")?;
        let data = target.array().unwrap();
        let n = opt_number(&arg(&args, 0)).unwrap_or(0.0);
        let len = data.len() as f64;
        let index = if n < 0.0 { len + n } else { n };
        if index < 0.0 || index >= len {
            return Ok(Value::Undefined);
        }
        Ok(data.at(index as usize))
    });
    ctx.method(&proto, "slice", 2, |ctx, this, args, _| {
        let target = self_array(&this, "slice")?;
        let items = target.array().unwrap().snapshot();
        let len = items.len();
        let start = relative(opt_number(&arg(&args, 0)), len, 0.0);
        let end = relative(opt_number(&arg(&args, 1)), len, len as f64);
        let slice = if start < end { items[start..end].to_vec() } else { Vec::new() };
        Ok(Value::Obj(ctx.new_array_holes(slice)))
    });
    ctx.method(&proto, "concat", 1, |ctx, this, args, _| {
        let target = self_array(&this, "concat")?;
        let mut joined = target.array().unwrap().snapshot();
        for item in &args {
            match item {
                Value::Obj(obj) if obj.is_array() => joined.extend(obj.array().unwrap().snapshot()),
                other => joined.push(Some(other.clone())),
            }
            check_array_length(joined.len() as f64)?;
        }
        Ok(Value::Obj(ctx.new_array_holes(joined)))
    });
    ctx.method(&proto, "flat", 0, |ctx, this, args, _| {
        fn flatten(items: Vec<Option<Value>>, depth: f64, out: &mut Vec<Value>) -> R<()> {
            for item in items.into_iter().flatten() {
                match &item {
                    Value::Obj(obj) if obj.is_array() && depth > 0.0 => {
                        flatten(obj.array().unwrap().snapshot(), depth - 1.0, out)?
                    }
                    _ => out.push(item),
                }
                check_array_length(out.len() as f64)?;
            }
            Ok(())
        }
        let target = self_array(&this, "flat")?;
        let depth = opt_number(&arg(&args, 0)).unwrap_or(1.0);
        let mut out = Vec::new();
        flatten(target.array().unwrap().snapshot(), depth, &mut out)?;
        Ok(Value::Obj(ctx.new_array(out)))
    });
    ctx.method(&proto, "reverse", 0, |_, this, _, _| {
        let target = self_array(&this, "reverse")?;
        let data = target.array().unwrap();
        // Fewer than two elements means no writes at all, so a frozen one-element array reverses fine.
        if data.len() > 1 {
            mutable(&target, None)?;
        }
        let mut items = data.snapshot();
        items.reverse();
        data.replace(items);
        Ok(Value::Obj(target))
    });
    ctx.method_work(&proto, "sort", 1, |ctx, this, args, _| {
        let target = self_array(&this, "sort")?;
        mutable(&target, None)?;
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let data = target.array().unwrap();
            let items = data.snapshot();
            let length = items.len();
            let holes = items.iter().filter(|item| item.is_none()).count();
            let count = length - holes;
            let sorted = sort_array(&ctx, items, arg(&args, 0), "Array.sort").await?;
            let mut written: Vec<Option<Value>> = sorted.into_iter().take(count).map(Some).collect();
            written.resize(length, None);
            data.replace(written);
            Ok(Value::Obj(target))
        })))
    });
    ctx.method_work(&proto, "toSorted", 1, |ctx, this, args, _| {
        let target = self_array(&this, "toSorted")?;
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let sorted = sort_array(&ctx, target.array().unwrap().snapshot(), arg(&args, 0), "Array.toSorted").await?;
            Ok(Value::Obj(ctx.new_array(sorted)))
        })))
    });
    ctx.method(&proto, "toReversed", 0, |ctx, this, _, _| {
        let target = self_array(&this, "toReversed")?;
        let mut items = target.array().unwrap().values();
        items.reverse();
        Ok(Value::Obj(ctx.new_array(items)))
    });
    ctx.method(&proto, "with", 2, |ctx, this, args, _| {
        let target = self_array(&this, "with")?;
        let mut items = target.array().unwrap().values();
        let index = opt_number(&arg(&args, 0)).unwrap_or(0.0);
        let resolved = if index < 0.0 { items.len() as f64 + index } else { index };
        if resolved < 0.0 || resolved >= items.len() as f64 {
            return Err(range_error("Array.with index is out of range."));
        }
        items[resolved as usize] = arg(&args, 1);
        Ok(Value::Obj(ctx.new_array(items)))
    });
    ctx.method(&proto, "push", 1, |_, this, args, _| {
        let target = self_array(&this, "push")?;
        mutable(&target, Some(args.len() as i64))?;
        // Validate all insertions before mutating to avoid partial cyclic updates.
        for item in &args {
            reject_circular_insertion(&target, item, "Array.push result")?;
        }
        let data = target.array().unwrap();
        for item in args {
            data.push(item);
        }
        Ok(Value::Num(data.len() as f64))
    });
    ctx.method(&proto, "unshift", 1, |_, this, args, _| {
        let target = self_array(&this, "unshift")?;
        mutable(&target, Some(args.len() as i64))?;
        for item in &args {
            reject_circular_insertion(&target, item, "Array.unshift result")?;
        }
        let data = target.array().unwrap();
        let mut items: Vec<Option<Value>> = args.into_iter().map(Some).collect();
        items.extend(data.snapshot());
        data.replace(items);
        Ok(Value::Num(data.len() as f64))
    });
    ctx.method(&proto, "pop", 0, |_, this, _, _| {
        let target = self_array(&this, "pop")?;
        mutable(&target, Some(-1))?;
        let data = target.array().unwrap();
        let len = data.len();
        if len == 0 {
            return Ok(Value::Undefined);
        }
        let item = data.at(len - 1);
        data.set_len(len - 1);
        Ok(item)
    });
    ctx.method(&proto, "shift", 0, |_, this, _, _| {
        let target = self_array(&this, "shift")?;
        mutable(&target, Some(-1))?;
        let data = target.array().unwrap();
        let mut items = data.snapshot();
        if items.is_empty() {
            return Ok(Value::Undefined);
        }
        let first = items.remove(0).unwrap_or_default();
        data.replace(items);
        Ok(first)
    });
    ctx.method(&proto, "splice", 2, |ctx, this, args, _| {
        let target = self_array(&this, "splice")?;
        let data = target.array().unwrap();
        let length = data.len();
        let start = opt_number(&arg(&args, 0)).unwrap_or(0.0);
        let from = if start < 0.0 { (length as f64 + start).max(0.0) } else { start.min(length as f64) } as usize;
        let delete_count = if args.len() == 1 {
            length - from
        } else {
            opt_number(&arg(&args, 1)).unwrap_or(0.0).max(0.0).min((length - from) as f64) as usize
        };
        let inserted: Vec<Value> = args.get(2..).map(|r| r.to_vec()).unwrap_or_default();
        for item in &inserted {
            reject_circular_insertion(&target, item, "Array.splice result")?;
        }
        mutable(&target, Some(inserted.len() as i64 - delete_count as i64))?;
        let mut items = data.snapshot();
        let removed: Vec<Option<Value>> = items.splice(from..from + delete_count, inserted.into_iter().map(Some)).collect();
        data.replace(items);
        Ok(Value::Obj(ctx.new_array_holes(removed)))
    });
    ctx.method(&proto, "toSpliced", 2, |ctx, this, args, _| {
        let target = self_array(&this, "toSpliced")?;
        let mut copied = target.array().unwrap().values();
        if args.is_empty() {
            return Ok(Value::Obj(ctx.new_array(copied)));
        }
        let len = copied.len();
        let start = relative(opt_number(&args[0]), len, 0.0);
        if args.len() == 1 {
            copied.truncate(start);
        } else {
            let count = opt_number(&args[1]).unwrap_or(0.0).max(0.0).min((len - start) as f64) as usize;
            copied.splice(start..start + count, args[2..].iter().cloned());
        }
        Ok(Value::Obj(ctx.new_array(copied)))
    });
    ctx.method(&proto, "fill", 1, |_, this, args, _| {
        let target = self_array(&this, "fill")?;
        mutable(&target, None)?;
        let value = arg(&args, 0);
        reject_circular_insertion(&target, &value, "Array.fill result")?;
        let data = target.array().unwrap();
        let len = data.len();
        let start = relative(opt_number(&arg(&args, 1)), len, 0.0);
        let end = relative(opt_number(&arg(&args, 2)), len, len as f64);
        for index in start..end.max(start) {
            data.set(index, value.clone());
        }
        Ok(Value::Obj(target))
    });
    ctx.method(&proto, "copyWithin", 2, |_, this, args, _| {
        let target = self_array(&this, "copyWithin")?;
        mutable(&target, None)?;
        let data = target.array().unwrap();
        let mut items = data.snapshot();
        let len = items.len();
        let to = relative(Some(opt_number(&arg(&args, 0)).unwrap_or(0.0)), len, 0.0);
        let from = relative(Some(opt_number(&arg(&args, 1)).unwrap_or(0.0)), len, 0.0);
        let end = relative(opt_number(&arg(&args, 2)), len, len as f64);
        let count = end.saturating_sub(from).min(len - to);
        let copied: Vec<Option<Value>> = items[from..from + count].to_vec();
        for (offset, item) in copied.into_iter().enumerate() {
            items[to + offset] = item;
        }
        data.replace(items);
        Ok(Value::Obj(target))
    });
    ctx.method_work(&proto, "toLocaleString", 0, |ctx, this, _, _| {
        let target = self_array(&this, "toLocaleString")?;
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let data = target.array().unwrap();
            let mut out = JsStrBuf::new();
            let length = data.len();
            for index in 0..length {
                if index > 0 {
                    out.push_unit(b',' as u16);
                }
                let item = data.at(index);
                if item.is_nullish() {
                    continue;
                }
                let shown = invoke(&ctx, item, "toLocaleString", "Array.prototype.toLocaleString")?.await?;
                out.push_js(&to_string_plain(&shown));
            }
            Ok(Value::Str(out.finish()))
        })))
    });
    for (name, kind) in [("keys", IterKind::Keys), ("values", IterKind::Values), ("entries", IterKind::Entries)] {
        ctx.method(&proto, name, 0, move |ctx, this, _, _| Ok(host_iter(ctx, self_array(&this, name)?, kind)));
    }
    ctx.method_work(&proto, "flatMap", 1, |ctx, this, args, _| {
        let target = self_array(&this, "flatMap")?;
        let apply = apply_collection_callback(ctx, &arg(&args, 0), "Array.flatMap")?;
        let this_arg = arg(&args, 1);
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let data = target.array().unwrap();
            let length = data.len();
            let mut values = Vec::new();
            for index in 0..length {
                let Some(item) = data.get(index) else { continue };
                let mapped =
                    apply(vec![item, Value::Num(index as f64), Value::Obj(target.clone())], this_arg.clone()).await?;
                match &mapped {
                    Value::Obj(obj) if obj.is_array() => values.extend(obj.array().unwrap().snapshot().into_iter().flatten()),
                    _ => values.push(mapped),
                }
            }
            Ok(Value::Obj(ctx.new_array(values)))
        })))
    });
    install_callback_methods(ctx, &proto, "Array", Rc::new(|this, name| self_array(this, name)), Rc::new(|ctx, values| {
        Value::Obj(ctx.new_array_holes(values))
    }));
    let values = crate::interp::object::get_data(&proto, &PropKey::from_str("values"));
    define(&proto, PropKey::Sym(Sym::Iterator), values, HIDDEN);
    Value::Obj(array)
}

pub type SelfFn = Rc<dyn Fn(&Value, &str) -> R<Obj>>;
pub type WrapFn = Rc<dyn Fn(&Ctx, Vec<Option<Value>>) -> Value>;

fn element(target: &Obj, index: usize) -> Option<Value> {
    match target.kind() {
        Kind::Array(data) => data.get(index),
        Kind::Bytes(data) => data.get(index).map(|b| Value::Num(b as f64)),
        _ => None,
    }
}

fn length_of(target: &Obj) -> usize {
    match target.kind() {
        Kind::Array(data) => data.len(),
        Kind::Bytes(data) => data.length,
        _ => 0,
    }
}

/// The callback methods Array and Uint8Array share. They fix the iteration length while reading existing elements
/// live; `wrap` builds the collection `map` and `filter` return.
pub fn install_callback_methods(ctx: &Ctx, proto: &Obj, label: &'static str, self_fn: SelfFn, wrap: WrapFn) {
    let names = ["map", "filter", "find", "findIndex", "findLast", "findLastIndex", "some", "every", "forEach", "reduce", "reduceRight"];
    for name in names {
        let (self_fn, wrap) = (self_fn.clone(), wrap.clone());
        ctx.method_work(proto, name, 1, move |ctx, this, args, _| {
            let target = self_fn(&this, name)?;
            let call = apply_collection_callback(ctx, &arg(&args, 0), &format!("{label}.{name}"))?;
            // reduce and reduceRight take an initial value where the others take a thisArg.
            let this_arg = if name.starts_with("reduce") { Value::Undefined } else { arg(&args, 1) };
            let apply = move |callback_args: Vec<Value>| -> Fut<R<Value>> { call(callback_args, this_arg.clone()) };
            let (ctx, wrap) = (ctx.clone(), wrap.clone());
            Ok(NOut::Work(boxed(async move { run_callback(&ctx, name, label, target, apply, args, wrap).await })))
        });
    }
}

async fn run_callback(
    ctx: &Ctx,
    name: &str,
    label: &str,
    target: Obj,
    apply: impl Fn(Vec<Value>) -> Fut<R<Value>>,
    args: Vec<Value>,
    wrap: WrapFn,
) -> R<Value> {
    let receiver = Value::Obj(target.clone());
    let length = length_of(&target);
    let at = |index: usize| element(&target, index).unwrap_or(Value::Undefined);
    let present = |index: usize| element(&target, index).is_some();
    let call = |item: Value, index: usize| apply(vec![item, Value::Num(index as f64), receiver.clone()]);
    match name {
        "map" => {
            let mut values: Vec<Option<Value>> = vec![None; length];
            for (index, slot) in values.iter_mut().enumerate() {
                if !present(index) {
                    continue;
                }
                *slot = Some(call(at(index), index).await?);
            }
            Ok(wrap(ctx, values))
        }
        "filter" => {
            let mut values = Vec::new();
            for index in 0..length {
                if !present(index) {
                    continue;
                }
                let item = at(index);
                if call(item.clone(), index).await?.truthy() {
                    values.push(Some(item));
                }
            }
            Ok(wrap(ctx, values))
        }
        "find" | "findIndex" => {
            for index in 0..length {
                let item = at(index);
                if call(item.clone(), index).await?.truthy() {
                    return Ok(if name == "find" { item } else { Value::Num(index as f64) });
                }
            }
            Ok(if name == "find" { Value::Undefined } else { Value::Num(-1.0) })
        }
        "findLast" | "findLastIndex" => {
            let mut index = length;
            while index > 0 {
                index -= 1;
                let item = at(index);
                if call(item.clone(), index).await?.truthy() {
                    return Ok(if name == "findLast" { item } else { Value::Num(index as f64) });
                }
            }
            Ok(if name == "findLast" { Value::Undefined } else { Value::Num(-1.0) })
        }
        "some" | "every" => {
            for index in 0..length {
                if !present(index) {
                    continue;
                }
                let result = call(at(index), index).await?.truthy();
                if name == "some" && result {
                    return Ok(Value::Bool(true));
                }
                if name == "every" && !result {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(name == "every"))
        }
        "forEach" => {
            for index in 0..length {
                if present(index) {
                    call(at(index), index).await?;
                }
            }
            Ok(Value::Undefined)
        }
        "reduce" => {
            let mut start = 0;
            let mut accumulator = arg(&args, 1);
            if args.len() < 2 {
                while start < length && !present(start) {
                    start += 1;
                }
                if start == length {
                    return Err(type_error(format!("{label}.reduce of an empty array with no initial value.")));
                }
                accumulator = at(start);
                start += 1;
            }
            for index in start..length {
                if !present(index) {
                    continue;
                }
                accumulator = apply(vec![accumulator, at(index), Value::Num(index as f64), receiver.clone()]).await?;
            }
            Ok(accumulator)
        }
        _ => {
            let mut start = length as i64 - 1;
            let mut accumulator = arg(&args, 1);
            if args.len() < 2 {
                while start >= 0 && !present(start as usize) {
                    start -= 1;
                }
                if start < 0 {
                    return Err(type_error(format!("{label}.reduceRight of an empty array with no initial value.")));
                }
                accumulator = at(start as usize);
                start -= 1;
            }
            let mut index = start;
            while index >= 0 {
                if present(index as usize) {
                    accumulator =
                        apply(vec![accumulator, at(index as usize), Value::Num(index as f64), receiver.clone()]).await?;
                }
                index -= 1;
            }
            Ok(accumulator)
        }
    }
}
