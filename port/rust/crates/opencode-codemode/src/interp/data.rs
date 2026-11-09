//! The data boundary (data.ts): what `JSON.stringify` would serialize for a program value, the host boundary's
//! additions (a promise is awaited, a Set crosses as an array, URLSearchParams as text, a Uint8Array is rejected,
//! `__proto__` keys are dropped), and host JSON copied back in as program values.

use crate::interp::coerce::typeof_value;
use crate::interp::errors::{invalid_data, range_error, type_error};
use crate::interp::eval::LFut;
use crate::interp::limits::MAX_VALUE_DEPTH;
use crate::interp::object::{define, get, keys};
use crate::interp::value::*;
use crate::interp::{Ctx, Fut, R};
use crate::json::{Json, JsonObj};
use crate::jsstr::JsStr;
use std::rc::Rc;

/// A `JSON.stringify` function replacer: `(key, value)` with the holder as `this`.
pub type Replacer = Rc<dyn Fn(Vec<Value>, Obj) -> Fut<R<Value>>>;

/// `JSON.stringify` semantics (with the Error form and a promise rejection).
pub async fn to_json(ctx: &Ctx, value: Value, replacer: Option<Replacer>) -> R<Option<Json>> {
    walk(ctx, value, replacer, false).await
}

/// The host boundary.
pub async fn to_boundary(ctx: &Ctx, value: Value) -> R<Option<Json>> {
    walk(ctx, value, None, true).await
}

struct Walk {
    ctx: Ctx,
    replacer: Option<Replacer>,
    boundary: bool,
    stack: std::cell::RefCell<Vec<usize>>,
}

async fn walk(ctx: &Ctx, value: Value, replacer: Option<Replacer>, boundary: bool) -> R<Option<Json>> {
    let holder = ctx.new_plain();
    define(&holder, PropKey::from_str(""), value, DATA);
    let state = Walk { ctx: ctx.clone(), replacer, boundary, stack: Default::default() };
    state.visit(holder, PropKey::from_str(""), 0).await
}

impl Walk {
    fn visit(&self, holder: Obj, key: PropKey, depth: usize) -> LFut<'_, R<Option<Json>>> {
        Box::pin(async move {
            let ctx = &self.ctx;
            // Shared references expand into a tree, so the work is not bounded by the value's size or depth.
            if ctx.runner.tick() {
                crate::interp::runner::slice(ctx).await;
            }
            if depth > MAX_VALUE_DEPTH {
                return Err(range_error(format!("Value exceeds the maximum depth of {MAX_VALUE_DEPTH}.")));
            }
            let raw = get(ctx, &holder, &key)?;
            let is_promise = matches!(&raw, Value::Obj(obj) if obj.is_promise());
            if is_promise && !self.boundary {
                return Err(invalid_data(
                    "JSON.stringify received an un-awaited Promise; await it first - e.g. `const result = await tools.ns.tool(...)`.",
                ));
            }
            let settled = if is_promise { ctx.await_value(raw, None).await? } else { raw };
            let to_json = match &settled {
                Value::Obj(obj) => get(ctx, obj, &PropKey::from_str("toJSON"))?,
                _ => Value::Undefined,
            };
            let key_value = Value::Str(key.to_js());
            let own = if matches!(&to_json, Value::Obj(f) if f.is_callable()) {
                ctx.call(to_json, settled, vec![key_value.clone()], None).await?
            } else {
                settled
            };
            let value = match &self.replacer {
                Some(replacer) => replacer(vec![key_value, own], holder.clone()).await?,
                None => own,
            };
            if matches!(value, Value::Undefined | Value::Sym(_)) || typeof_value(&value) == "function" {
                return Ok(None);
            }
            let obj = match value {
                Value::Num(n) => return Ok(Some(if n.is_finite() { Json::Num(n) } else { Json::Null })),
                Value::Null => return Ok(Some(Json::Null)),
                Value::Str(text) => return Ok(Some(Json::Str(text))),
                Value::Bool(b) => return Ok(Some(Json::Bool(b))),
                Value::Obj(obj) => obj,
                _ => return Ok(Some(Json::Obj(JsonObj::new()))),
            };
            if self.boundary && obj.bytes().is_some() {
                return Err(invalid_data(
                    "A Uint8Array cannot cross to the host; pass text instead, e.g. `new TextDecoder().decode(bytes)` or `bytes.toBase64()`.",
                ));
            }
            if self.boundary
                && let Kind::URLSearchParams(params) = obj.kind()
            {
                return Ok(Some(Json::Str(params.borrow().serialize())));
            }
            if let Kind::Headers(headers) = obj.kind() {
                let mut copied = JsonObj::new();
                for (name, value) in headers.borrow().sorted_pairs() {
                    copied.insert(name, Json::Str(value));
                }
                return Ok(Some(Json::Obj(copied)));
            }
            let target = match obj.kind() {
                Kind::Set(set) if self.boundary => ctx.new_array(set.borrow().values()),
                _ => obj,
            };
            if self.stack.borrow().contains(&target.addr()) {
                return Err(type_error("Converting circular structure to JSON."));
            }
            self.stack.borrow_mut().push(target.addr());
            let result = self.visit_object(&target, depth).await;
            self.stack.borrow_mut().pop();
            result.map(Some)
        })
    }

    async fn visit_object(&self, target: &Obj, depth: usize) -> R<Json> {
        if let Some(data) = target.array() {
            let mut items = Vec::new();
            let mut index = 0;
            while index < data.len() {
                items.push(self.visit(target.clone(), PropKey::Index(index as u32), depth + 1).await?.unwrap_or(Json::Null));
                index += 1;
            }
            return Ok(Json::Arr(items));
        }
        let mut copied = JsonObj::new();
        // Errors serialize as { name, message, ...own }: both may be inherited, and neither is enumerable in JS.
        if target.is_error() {
            for name in ["name", "message"] {
                if let Some(item) = self.visit(target.clone(), PropKey::from_str(name), depth + 1).await? {
                    copied.insert(JsStr::from(name), item);
                }
            }
        }
        for key in keys(target) {
            if self.boundary && key.eq_str("__proto__") {
                continue;
            }
            if let Some(item) = self.visit(target.clone(), key.clone(), depth + 1).await? {
                copied.insert(key.to_js(), item);
            }
        }
        Ok(Json::Obj(copied))
    }
}

/// Host JSON as program values: objects and arrays are copied, primitives pass through.
pub fn from_json(ctx: &Ctx, value: &Json) -> Value {
    match value {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Num(n) => Value::Num(*n),
        Json::Str(text) => Value::Str(text.clone()),
        Json::Arr(items) => Value::Obj(ctx.new_array(items.iter().map(|item| from_json(ctx, item)).collect())),
        Json::Obj(obj) => {
            let copied = ctx.new_plain();
            for (key, item) in obj.iter() {
                define(&copied, PropKey::from_js(key.clone()), from_json(ctx, item), DATA);
            }
            Value::Obj(copied)
        }
    }
}

pub fn from_json_opt(ctx: &Ctx, value: &Option<Json>) -> Value {
    match value {
        None => Value::Undefined,
        Some(json) => from_json(ctx, json),
    }
}
