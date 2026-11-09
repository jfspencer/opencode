//! `Map`, `Set`, `WeakMap`, `WeakSet`, and `groupBy` (collections.ts).

use crate::interp::callback::{apply_collection_callback, is_supported_callback, preserve_consumer_error, to_primitive_number, to_primitive_string};
use crate::interp::coerce::{describe_value, is_opaque, to_string_plain};
use crate::interp::cursor::{Cursor, HostIter, IterKind};
use crate::interp::errors::{invalid_data, type_error};
use crate::interp::native::{NativeOptions, prototype_from, receiver, requires_new};
use crate::interp::object::{define, define_accessor, get, get_data, get_own};
use crate::interp::value::*;
use crate::interp::{Ctx, R, boxed};
use std::rc::Rc;

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}

/// `Map.groupBy` and `Object.groupBy`: the same iteration, keyed into a Map or a null-prototype object.
pub fn group_by(ctx: &Ctx, namespace: &'static str) -> Obj {
    ctx.native_work("groupBy", 2, move |ctx, _, args, _| {
        let source = arg(&args, 0);
        if source.is_nullish() {
            return Err(type_error(format!("{namespace}.groupBy expects an iterable collection.")));
        }
        let apply = apply_collection_callback(ctx, &arg(&args, 1), &format!("{namespace}.groupBy"))?;
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let Some(cursor) = ctx.iterate(&source, None).await? else {
                return Err(type_error(format!("{namespace}.groupBy expects an iterable collection.")));
            };
            let close = || {
                let (ctx, cursor) = (ctx.clone(), cursor.clone());
                async move { cursor.close_now(&ctx).await }
            };
            let mut index = 0;
            if namespace == "Map" {
                let result = ctx.new_object(Some(ctx.builtins.map.clone()), Kind::Map(Default::default()));
                loop {
                    let step = cursor.next_step(&ctx).await?;
                    if step.done {
                        return Ok(Value::Obj(result));
                    }
                    let item = step.value;
                    let key = apply(vec![item.clone(), Value::Num(index as f64)], Value::Undefined).await;
                    let key = preserve_consumer_error(close(), key).await?;
                    let Kind::Map(map) = result.kind() else { unreachable!() };
                    let group = map.borrow().get(&key);
                    match group {
                        Some(Value::Obj(group)) => group.array().unwrap().push(item),
                        _ => map.borrow_mut().set(key, Value::Obj(ctx.new_array(vec![item]))),
                    }
                    index += 1;
                }
            }
            // Object.groupBy returns a null-prototype object, so group names never collide with inherited methods.
            let result = ctx.new_object(None, Kind::Ordinary);
            loop {
                let step = cursor.next_step(&ctx).await?;
                if step.done {
                    return Ok(Value::Obj(result));
                }
                let item = step.value;
                let key = async {
                    let value = apply(vec![item.clone(), Value::Num(index as f64)], Value::Undefined).await?;
                    if matches!(&value, Value::Obj(obj) if obj.is_promise()) {
                        return Ok(to_string_plain(&value));
                    }
                    if is_opaque(&value) {
                        return Err(invalid_data(format!(
                            "Object.groupBy callback must return a data value, received {}.",
                            describe_value(&value)
                        )));
                    }
                    to_primitive_string(&ctx, value).await
                }
                .await;
                let key = PropKey::from_js(preserve_consumer_error(close(), key).await?);
                match get_own(&ctx, &result, &key)? {
                    Value::Obj(group) if group.is_array() => group.array().unwrap().push(item),
                    _ => define(&result, key, Value::Obj(ctx.new_array(vec![item])), DATA),
                }
                index += 1;
            }
        })))
    })
}

fn iterator(ctx: &Ctx, iter: HostIter) -> Value {
    Value::Obj(ctx.new_object(Some(ctx.builtins.iterator.clone()), Kind::Iterator(Cursor::host(iter))))
}

fn self_map(this: &Value, name: &str) -> R<Obj> {
    receiver(this, |obj| matches!(obj.kind(), Kind::Map(_)), &format!("Map.prototype.{name}"))
}

fn map_of(obj: &Obj) -> &std::cell::RefCell<crate::interp::collections::OrderedMap> {
    let Kind::Map(map) = obj.kind() else { unreachable!() };
    map
}

pub fn map_global(ctx: &Ctx) -> Value {
    let proto = ctx.builtins.map.clone();
    let map = ctx.constructor(
        &proto,
        NativeOptions {
            name: "Map".into(),
            length: 0,
            call: requires_new("Map"),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                let target =
                    ctx.new_object(Some(prototype_from(&new_target, &ctx.builtins.map)), Kind::Map(Default::default()));
                let init = arg(&args, 0);
                if init.is_nullish() {
                    return Ok(NOut::Val(Value::Obj(target)));
                }
                let ctx = ctx.clone();
                Ok(NOut::Work(boxed(async move {
                    let Some(cursor) = ctx.iterate(&init, None).await? else {
                        return Err(type_error(format!(
                            "new Map(...) expects an iterable of [key, value] pairs, received {}.",
                            describe_value(&init)
                        )));
                    };
                    loop {
                        let step = cursor.next_step(&ctx).await?;
                        if step.done {
                            return Ok(Value::Obj(target));
                        }
                        let added = (|| -> R<()> {
                            let Value::Obj(entry) = &step.value else {
                                return Err(type_error("new Map(...) expects [key, value] pairs as entry objects."));
                            };
                            let key = get_own(&ctx, entry, &PropKey::Index(0))?;
                            let value = get_own(&ctx, entry, &PropKey::Index(1))?;
                            map_of(&target).borrow_mut().set(key, value);
                            Ok(())
                        })();
                        let close = {
                            let (ctx, cursor) = (ctx.clone(), cursor.clone());
                            async move { cursor.close_now(&ctx).await }
                        };
                        preserve_consumer_error(close, added).await?;
                    }
                })))
            })),
            callback: true,
        },
    );
    define(&map, PropKey::from_str("groupBy"), Value::Obj(group_by(ctx, "Map")), HIDDEN);
    define_accessor(
        &proto,
        "size",
        Some(Rc::new(|_, this| {
            let target = receiver(this, |obj| matches!(obj.kind(), Kind::Map(_)), "Map.prototype.size")?;
            Ok(Value::Num(map_of(&target).borrow().len() as f64))
        })),
        None,
    );
    ctx.method(&proto, "get", 1, |_, this, args, _| Ok(map_of(&self_map(&this, "get")?).borrow().get(&arg(&args, 0)).unwrap_or_default()));
    ctx.method(&proto, "has", 1, |_, this, args, _| Ok(Value::Bool(map_of(&self_map(&this, "has")?).borrow().has(&arg(&args, 0)))));
    ctx.method(&proto, "set", 2, |_, this, args, _| {
        let target = self_map(&this, "set")?;
        map_of(&target).borrow_mut().set(arg(&args, 0), arg(&args, 1));
        Ok(Value::Obj(target))
    });
    ctx.method(&proto, "getOrInsert", 2, |_, this, args, _| {
        let target = self_map(&this, "getOrInsert")?;
        let mut map = map_of(&target).borrow_mut();
        if !map.has(&arg(&args, 0)) {
            map.set(arg(&args, 0), arg(&args, 1));
        }
        Ok(map.get(&arg(&args, 0)).unwrap_or_default())
    });
    ctx.method_work(&proto, "getOrInsertComputed", 2, |ctx, this, args, _| {
        let target = self_map(&this, "getOrInsertComputed")?;
        let apply = apply_collection_callback(ctx, &arg(&args, 1), "Map.getOrInsertComputed")?;
        let key = arg(&args, 0);
        if let Some(found) = map_of(&target).borrow().get(&key)
            && map_of(&target).borrow().has(&key)
        {
            return Ok(NOut::Val(found));
        }
        Ok(NOut::Work(boxed(async move {
            // The callback sees the stored key (-0 is +0) and its result wins over anything it inserted itself.
            let stored = match &key {
                Value::Num(n) if *n == 0.0 => Value::Num(0.0),
                other => other.clone(),
            };
            let value = apply(vec![stored], Value::Undefined).await?;
            map_of(&target).borrow_mut().set(key, value.clone());
            Ok(value)
        })))
    });
    ctx.method(&proto, "delete", 1, |_, this, args, _| Ok(Value::Bool(map_of(&self_map(&this, "delete")?).borrow_mut().delete(&arg(&args, 0)))));
    ctx.method(&proto, "clear", 0, |_, this, _, _| {
        map_of(&self_map(&this, "clear")?).borrow_mut().clear();
        Ok(Value::Undefined)
    });
    for (name, kind) in [("keys", IterKind::Keys), ("values", IterKind::Values), ("entries", IterKind::Entries)] {
        ctx.method(&proto, name, 0, move |ctx, this, _, _| {
            Ok(iterator(ctx, HostIter::Map { target: self_map(&this, name)?, position: 0, kind }))
        });
    }
    ctx.method_work(&proto, "forEach", 1, |ctx, this, args, _| {
        let target = self_map(&this, "forEach")?;
        let apply = apply_collection_callback(ctx, &arg(&args, 0), "Map.forEach")?;
        let this_arg = arg(&args, 1);
        Ok(NOut::Work(boxed(async move {
            let mut position = 0;
            loop {
                let next = map_of(&target).borrow().entry_from(position);
                let Some((after, key, value)) = next else { return Ok(Value::Undefined) };
                position = after;
                apply(vec![value, key, Value::Obj(target.clone())], this_arg.clone()).await?;
            }
        })))
    });
    let entries = get_data(&proto, &PropKey::from_str("entries"));
    define(&proto, PropKey::Sym(Sym::Iterator), entries, HIDDEN);
    Value::Obj(map)
}

fn self_set(this: &Value, name: &str) -> R<Obj> {
    receiver(this, |obj| matches!(obj.kind(), Kind::Set(_)), &format!("Set.prototype.{name}"))
}

fn set_of(obj: &Obj) -> &std::cell::RefCell<crate::interp::collections::OrderedSet> {
    let Kind::Set(set) = obj.kind() else { unreachable!() };
    set
}

enum SetRecord {
    Set(Obj),
    Map(Obj),
    Like { source: Obj, size: f64, has: Value, keys: Value },
}

impl SetRecord {
    fn size(&self) -> f64 {
        match self {
            SetRecord::Set(obj) => set_of(obj).borrow().len() as f64,
            SetRecord::Map(obj) => map_of(obj).borrow().len() as f64,
            SetRecord::Like { size, .. } => *size,
        }
    }
    async fn has(&self, ctx: &Ctx, item: &Value) -> R<bool> {
        match self {
            SetRecord::Set(obj) => Ok(set_of(obj).borrow().has(item)),
            SetRecord::Map(obj) => Ok(map_of(obj).borrow().has(item)),
            SetRecord::Like { source, has, .. } => {
                Ok(ctx.call(has.clone(), Value::Obj(source.clone()), vec![item.clone()], None).await?.truthy())
            }
        }
    }
    async fn keys(&self, ctx: &Ctx) -> R<Vec<Value>> {
        match self {
            SetRecord::Set(obj) => Ok(set_of(obj).borrow().values()),
            SetRecord::Map(obj) => Ok(map_of(obj).borrow().keys()),
            SetRecord::Like { source, keys, .. } => {
                let result = ctx.call(keys.clone(), Value::Obj(source.clone()), Vec::new(), None).await?;
                if let Value::Obj(obj) = &result
                    && let Some(data) = obj.array()
                {
                    return Ok(data.values());
                }
                let cursor = ctx.iterate_direct(&result, None)?;
                let mut items = Vec::new();
                loop {
                    let step = cursor.next_step(ctx).await?;
                    if step.done {
                        return Ok(items);
                    }
                    items.push(step.value);
                }
            }
        }
    }
}

async fn load_set_record(ctx: &Ctx, source: &Value, name: &str) -> R<SetRecord> {
    let Value::Obj(obj) = source else { return Err(type_error(format!("Set.{name} expects a Set-like object."))) };
    match obj.kind() {
        Kind::Set(_) => return Ok(SetRecord::Set(obj.clone())),
        Kind::Map(_) => return Ok(SetRecord::Map(obj.clone())),
        _ => {}
    }
    let size = to_primitive_number(ctx, get(ctx, obj, &PropKey::from_str("size"))?).await?;
    if size.is_nan() {
        return Err(type_error(format!("Set.{name} received a Set-like object with an invalid size.")));
    }
    let has = get(ctx, obj, &PropKey::from_str("has"))?;
    let keys = get(ctx, obj, &PropKey::from_str("keys"))?;
    if !is_supported_callback(&has) || !is_supported_callback(&keys) {
        return Err(type_error(format!("Set.{name} expects callable 'has' and 'keys' methods.")));
    }
    Ok(SetRecord::Like { source: obj.clone(), size: size.trunc().max(0.0), has, keys })
}

/// Visits a Set's values live, as a host `for (const item of set.values())` does: deleted values are skipped and
/// added ones visited.
async fn for_each_live<F, Fut2>(set: &Obj, mut visit: F) -> R<bool>
where
    F: FnMut(Value) -> Fut2,
    Fut2: std::future::Future<Output = R<bool>>,
{
    let mut position = 0;
    loop {
        let next = set_of(set).borrow().value_from(position);
        let Some((after, item)) = next else { return Ok(true) };
        position = after;
        if !visit(item).await? {
            return Ok(false);
        }
    }
}

async fn set_operation(ctx: &Ctx, target: Obj, name: &str, source: Value) -> R<Value> {
    let other = load_set_record(ctx, &source, name).await?;
    let new_set = || ctx.new_object(Some(ctx.builtins.set.clone()), Kind::Set(Default::default()));
    let copy = || {
        let result = new_set();
        let items = set_of(&target).borrow().values();
        for item in items {
            set_of(&result).borrow_mut().add(item);
        }
        result
    };
    let size = set_of(&target).borrow().len() as f64;
    let other = &other;
    match name {
        "union" => {
            let result = copy();
            for item in other.keys(ctx).await? {
                set_of(&result).borrow_mut().add(item);
            }
            Ok(Value::Obj(result))
        }
        "intersection" => {
            let result = new_set();
            if size <= other.size() {
                for_each_live(&target, |item| {
                    let result = result.clone();
                    async move {
                        if other.has(ctx, &item).await? {
                            set_of(&result).borrow_mut().add(item);
                        }
                        Ok(true)
                    }
                })
                .await?;
                return Ok(Value::Obj(result));
            }
            for item in other.keys(ctx).await? {
                if set_of(&target).borrow().has(&item) {
                    set_of(&result).borrow_mut().add(item);
                }
            }
            Ok(Value::Obj(result))
        }
        "difference" => {
            let result = copy();
            if size <= other.size() {
                let live = result.clone();
                for_each_live(&live, |item| {
                    let result = result.clone();
                    async move {
                        if other.has(ctx, &item).await? {
                            set_of(&result).borrow_mut().delete(&item);
                        }
                        Ok(true)
                    }
                })
                .await?;
                return Ok(Value::Obj(result));
            }
            for item in other.keys(ctx).await? {
                set_of(&result).borrow_mut().delete(&item);
            }
            Ok(Value::Obj(result))
        }
        "symmetricDifference" => {
            let result = copy();
            for item in other.keys(ctx).await? {
                if set_of(&target).borrow().has(&item) {
                    set_of(&result).borrow_mut().delete(&item);
                } else {
                    set_of(&result).borrow_mut().add(item);
                }
            }
            Ok(Value::Obj(result))
        }
        "isSubsetOf" => {
            if size > other.size() {
                return Ok(Value::Bool(false));
            }
            let all = for_each_live(&target, |item| async move { other.has(ctx, &item).await }).await?;
            Ok(Value::Bool(all))
        }
        "isSupersetOf" => {
            if size < other.size() {
                return Ok(Value::Bool(false));
            }
            for item in other.keys(ctx).await? {
                if !set_of(&target).borrow().has(&item) {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        _ => {
            if size <= other.size() {
                let none = for_each_live(&target, |item| async move { Ok(!other.has(ctx, &item).await?) }).await?;
                return Ok(Value::Bool(none));
            }
            for item in other.keys(ctx).await? {
                if set_of(&target).borrow().has(&item) {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
    }
}

pub fn set_global(ctx: &Ctx) -> Value {
    let proto = ctx.builtins.set.clone();
    let set = ctx.constructor(
        &proto,
        NativeOptions {
            name: "Set".into(),
            length: 0,
            call: requires_new("Set"),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                let target =
                    ctx.new_object(Some(prototype_from(&new_target, &ctx.builtins.set)), Kind::Set(Default::default()));
                let init = arg(&args, 0);
                if init.is_nullish() {
                    return Ok(NOut::Val(Value::Obj(target)));
                }
                let ctx = ctx.clone();
                Ok(NOut::Work(boxed(async move {
                    let Some(cursor) = ctx.iterate(&init, None).await? else {
                        return Err(type_error(format!(
                            "new Set(...) expects a synchronous iterable, received {}.",
                            describe_value(&init)
                        )));
                    };
                    loop {
                        let step = cursor.next_step(&ctx).await?;
                        if step.done {
                            return Ok(Value::Obj(target));
                        }
                        set_of(&target).borrow_mut().add(step.value);
                    }
                })))
            })),
            callback: true,
        },
    );
    define_accessor(
        &proto,
        "size",
        Some(Rc::new(|_, this| {
            let target = receiver(this, |obj| matches!(obj.kind(), Kind::Set(_)), "Set.prototype.size")?;
            Ok(Value::Num(set_of(&target).borrow().len() as f64))
        })),
        None,
    );
    ctx.method(&proto, "has", 1, |_, this, args, _| Ok(Value::Bool(set_of(&self_set(&this, "has")?).borrow().has(&arg(&args, 0)))));
    ctx.method(&proto, "add", 1, |_, this, args, _| {
        let target = self_set(&this, "add")?;
        set_of(&target).borrow_mut().add(arg(&args, 0));
        Ok(Value::Obj(target))
    });
    ctx.method(&proto, "delete", 1, |_, this, args, _| Ok(Value::Bool(set_of(&self_set(&this, "delete")?).borrow_mut().delete(&arg(&args, 0)))));
    ctx.method(&proto, "clear", 0, |_, this, _, _| {
        set_of(&self_set(&this, "clear")?).borrow_mut().clear();
        Ok(Value::Undefined)
    });
    for (name, kind) in [("keys", IterKind::Values), ("values", IterKind::Values), ("entries", IterKind::Entries)] {
        ctx.method(&proto, name, 0, move |ctx, this, _, _| {
            Ok(iterator(ctx, HostIter::Set { target: self_set(&this, name)?, position: 0, kind }))
        });
    }
    ctx.method_work(&proto, "forEach", 1, |ctx, this, args, _| {
        let target = self_set(&this, "forEach")?;
        let apply = apply_collection_callback(ctx, &arg(&args, 0), "Set.forEach")?;
        let this_arg = arg(&args, 1);
        Ok(NOut::Work(boxed(async move {
            let mut position = 0;
            loop {
                let next = set_of(&target).borrow().value_from(position);
                let Some((after, value)) = next else { return Ok(Value::Undefined) };
                position = after;
                apply(vec![value.clone(), value, Value::Obj(target.clone())], this_arg.clone()).await?;
            }
        })))
    });
    for name in ["union", "intersection", "difference", "symmetricDifference", "isSubsetOf", "isSupersetOf", "isDisjointFrom"] {
        ctx.method_work(&proto, name, 1, move |ctx, this, args, _| {
            let target = self_set(&this, name)?;
            let ctx = ctx.clone();
            Ok(NOut::Work(boxed(async move { set_operation(&ctx, target, name, arg(&args, 0)).await })))
        });
    }
    let values = get_data(&proto, &PropKey::from_str("values"));
    define(&proto, PropKey::Sym(Sym::Iterator), values, HIDDEN);
    Value::Obj(set)
}

// CanBeHeldWeakly: only program objects.
fn weak_key(value: &Value, label: &str) -> R<Obj> {
    match value {
        Value::Obj(obj) => Ok(obj.clone()),
        other => Err(type_error(format!("Invalid value used {label}: {} cannot be held weakly.", describe_value(other)))),
    }
}

fn table(obj: &Obj) -> &std::cell::RefCell<crate::interp::collections::WeakTable> {
    match obj.kind() {
        Kind::WeakMap(table) | Kind::WeakSet(table) => table,
        _ => unreachable!(),
    }
}

pub fn weak_map_global(ctx: &Ctx) -> Value {
    let proto = ctx.builtins.weak_map.clone();
    let weak_map = ctx.constructor(
        &proto,
        NativeOptions {
            name: "WeakMap".into(),
            length: 0,
            call: requires_new("WeakMap"),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                let target = ctx
                    .new_object(Some(prototype_from(&new_target, &ctx.builtins.weak_map)), Kind::WeakMap(Default::default()));
                let init = arg(&args, 0);
                if init.is_nullish() {
                    return Ok(NOut::Val(Value::Obj(target)));
                }
                let ctx = ctx.clone();
                Ok(NOut::Work(boxed(async move {
                    let Some(cursor) = ctx.iterate(&init, None).await? else {
                        return Err(type_error(format!(
                            "new WeakMap(...) expects an iterable of [key, value] pairs, received {}.",
                            describe_value(&init)
                        )));
                    };
                    loop {
                        let step = cursor.next_step(&ctx).await?;
                        if step.done {
                            return Ok(Value::Obj(target));
                        }
                        let added = (|| -> R<()> {
                            let Value::Obj(entry) = &step.value else {
                                return Err(type_error("new WeakMap(...) expects [key, value] pairs as entry objects."));
                            };
                            let key = weak_key(&get_own(&ctx, entry, &PropKey::Index(0))?, "as weak map key")?;
                            let value = get_own(&ctx, entry, &PropKey::Index(1))?;
                            table(&target).borrow_mut().set(&key, value);
                            Ok(())
                        })();
                        let close = {
                            let (ctx, cursor) = (ctx.clone(), cursor.clone());
                            async move { cursor.close_now(&ctx).await }
                        };
                        preserve_consumer_error(close, added).await?;
                    }
                })))
            })),
            callback: true,
        },
    );
    let self_table = |this: &Value, name: &str| {
        receiver(this, |obj| matches!(obj.kind(), Kind::WeakMap(_)), &format!("WeakMap.prototype.{name}"))
    };
    ctx.method(&proto, "get", 1, move |_, this, args, _| {
        let target = self_table(&this, "get")?;
        Ok(match args.first() {
            Some(Value::Obj(key)) => table(&target).borrow().get(key).unwrap_or_default(),
            _ => Value::Undefined,
        })
    });
    ctx.method(&proto, "has", 1, move |_, this, args, _| {
        let target = self_table(&this, "has")?;
        Ok(Value::Bool(matches!(args.first(), Some(Value::Obj(key)) if table(&target).borrow().has(key))))
    });
    ctx.method(&proto, "delete", 1, move |_, this, args, _| {
        let target = self_table(&this, "delete")?;
        Ok(Value::Bool(matches!(args.first(), Some(Value::Obj(key)) if table(&target).borrow_mut().delete(key))))
    });
    ctx.method(&proto, "set", 2, move |_, this, args, _| {
        let target = self_table(&this, "set")?;
        let key = weak_key(&arg(&args, 0), "as weak map key")?;
        table(&target).borrow_mut().set(&key, arg(&args, 1));
        Ok(this)
    });
    ctx.method(&proto, "getOrInsert", 2, move |_, this, args, _| {
        let target = self_table(&this, "getOrInsert")?;
        let key = weak_key(&arg(&args, 0), "as weak map key")?;
        if !table(&target).borrow().has(&key) {
            table(&target).borrow_mut().set(&key, arg(&args, 1));
        }
        Ok(table(&target).borrow().get(&key).unwrap_or_default())
    });
    ctx.method_work(&proto, "getOrInsertComputed", 2, move |ctx, this, args, _| {
        let target = self_table(&this, "getOrInsertComputed")?;
        let key = weak_key(&arg(&args, 0), "as weak map key")?;
        let apply = apply_collection_callback(ctx, &arg(&args, 1), "WeakMap.getOrInsertComputed")?;
        if table(&target).borrow().has(&key) {
            return Ok(NOut::Val(table(&target).borrow().get(&key).unwrap_or_default()));
        }
        Ok(NOut::Work(boxed(async move {
            let value = apply(vec![Value::Obj(key.clone())], Value::Undefined).await?;
            table(&target).borrow_mut().set(&key, value.clone());
            Ok(value)
        })))
    });
    Value::Obj(weak_map)
}

pub fn weak_set_global(ctx: &Ctx) -> Value {
    let proto = ctx.builtins.weak_set.clone();
    let weak_set = ctx.constructor(
        &proto,
        NativeOptions {
            name: "WeakSet".into(),
            length: 0,
            call: requires_new("WeakSet"),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                let target = ctx
                    .new_object(Some(prototype_from(&new_target, &ctx.builtins.weak_set)), Kind::WeakSet(Default::default()));
                let init = arg(&args, 0);
                if init.is_nullish() {
                    return Ok(NOut::Val(Value::Obj(target)));
                }
                let ctx = ctx.clone();
                Ok(NOut::Work(boxed(async move {
                    let Some(cursor) = ctx.iterate(&init, None).await? else {
                        return Err(type_error(format!(
                            "new WeakSet(...) expects a synchronous iterable, received {}.",
                            describe_value(&init)
                        )));
                    };
                    loop {
                        let step = cursor.next_step(&ctx).await?;
                        if step.done {
                            return Ok(Value::Obj(target));
                        }
                        let added = weak_key(&step.value, "in weak set").map(|key| table(&target).borrow_mut().set(&key, Value::Undefined));
                        let close = {
                            let (ctx, cursor) = (ctx.clone(), cursor.clone());
                            async move { cursor.close_now(&ctx).await }
                        };
                        preserve_consumer_error(close, added).await?;
                    }
                })))
            })),
            callback: true,
        },
    );
    let self_table = |this: &Value, name: &str| {
        receiver(this, |obj| matches!(obj.kind(), Kind::WeakSet(_)), &format!("WeakSet.prototype.{name}"))
    };
    ctx.method(&proto, "has", 1, move |_, this, args, _| {
        let target = self_table(&this, "has")?;
        Ok(Value::Bool(matches!(args.first(), Some(Value::Obj(key)) if table(&target).borrow().has(key))))
    });
    ctx.method(&proto, "delete", 1, move |_, this, args, _| {
        let target = self_table(&this, "delete")?;
        Ok(Value::Bool(matches!(args.first(), Some(Value::Obj(key)) if table(&target).borrow_mut().delete(key))))
    });
    ctx.method(&proto, "add", 1, move |_, this, args, _| {
        let target = self_table(&this, "add")?;
        let key = weak_key(&arg(&args, 0), "in weak set")?;
        table(&target).borrow_mut().set(&key, Value::Undefined);
        Ok(this)
    });
    Value::Obj(weak_set)
}
