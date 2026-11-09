//! `JSON` (json.ts): `parse` with revivers, `stringify` with function and array replacers and indentation.

use crate::interp::callback::{Hint, apply_collection_callback, with_primitives};
use crate::interp::coerce::{is_callable_value, to_string_plain};
use crate::interp::data::{Replacer, from_json, to_json};
use crate::interp::errors::syntax_error;
use crate::interp::limits::check_string_length;
use crate::interp::object::{define, get, keys, remove, set};
use crate::interp::value::*;
use crate::interp::{Ctx, R, boxed};
use crate::jsstr::{JsStr, JsStrBuf};
use std::rc::Rc;

pub fn json_global(ctx: &Ctx) -> Value {
    let json = ctx.new_object(Some(ctx.builtins.object.clone()), Kind::Namespace("JSON"));
    ctx.method_work(&json, "parse", 2, |ctx, _, args, _| {
        let text = args.first().cloned().unwrap_or_default();
        let reviver = args.get(1).cloned().unwrap_or_default();
        // ToString on the text runs a program object's own toString, as in JS.
        with_primitives(ctx, &[Hint::String], vec![text], move |ctx, values| {
            parse_text(ctx, to_string_plain(&values[0]), reviver)
        })
    });
    ctx.method_work(&json, "stringify", 3, |ctx, _, args, _| {
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move { stringify(&ctx, args).await })))
    });
    Value::Obj(json)
}

fn parse_text(ctx: &Ctx, text: JsStr, reviver: Value) -> R<NOut> {
    let parsed = match crate::json::parse(text.units()) {
        Ok(json) => from_json(ctx, &json),
        Err(message) => return Err(syntax_error(format!("JSON.parse received invalid JSON: {message}"))),
    };
    if !is_callable_value(&reviver) {
        return Ok(NOut::Val(parsed));
    }
    let apply = apply_collection_callback(ctx, &reviver, "JSON.parse")?;
    let ctx = ctx.clone();
    Ok(NOut::Work(boxed(async move {
        let root = ctx.new_plain();
        define(&root, PropKey::from_str(""), parsed, DATA);
        visit(&ctx, &apply, root, PropKey::from_str("")).await
    })))
}

fn visit<'a>(
    ctx: &'a Ctx,
    apply: &'a crate::interp::callback::Apply,
    holder: Obj,
    key: PropKey,
) -> crate::interp::eval::LFut<'a, R<Value>> {
    Box::pin(async move {
        let value = get(ctx, &holder, &key)?;
        if let Value::Obj(obj) = &value {
            for name in keys(obj) {
                let revived = visit(ctx, apply, obj.clone(), name.clone()).await?;
                if revived.is_undefined() {
                    remove(obj, &name);
                } else {
                    set(ctx, obj, &name, revived)?;
                }
            }
        }
        apply(vec![Value::Str(key.to_js()), value], Value::Obj(holder)).await
    })
}

async fn stringify(ctx: &Ctx, args: Vec<Value>) -> R<Value> {
    let space = args.get(2).cloned().unwrap_or_default();
    let replacer = args.get(1).cloned().unwrap_or_default();
    let properties: Option<Vec<JsStr>> = match &replacer {
        Value::Obj(obj) if obj.is_array() => {
            let data = obj.array().unwrap();
            let mut list: Vec<JsStr> = Vec::new();
            for index in 0..data.len() {
                // .filter skips holes; strings and numbers are names.
                let Some(item) = data.get(index) else { continue };
                let name = match item {
                    Value::Str(text) => text,
                    Value::Num(n) => JsStr::from(crate::numconv::to_string(n)),
                    _ => continue,
                };
                if !list.contains(&name) {
                    list.push(name);
                }
            }
            Some(list)
        }
        _ => None,
    };
    let callback: Option<Replacer> = if is_callable_value(&replacer) {
        let apply = apply_collection_callback(ctx, &replacer, "JSON.stringify")?;
        Some(Rc::new(move |args, holder| apply(args, Value::Obj(holder))))
    } else {
        None
    };
    let value = args.first().cloned().unwrap_or_default();
    let Some(json) = to_json(ctx, value, callback).await? else { return Ok(Value::Undefined) };
    let gap: Vec<u16> = match space {
        Value::Num(n) => {
            let count = crate::interp::coerce::integer_of(n).clamp(0.0, 10.0) as usize;
            vec![b' ' as u16; count]
        }
        Value::Str(text) => text.units()[..text.len().min(10)].to_vec(),
        _ => Vec::new(),
    };
    let text = crate::json::stringify(&json, properties.as_deref(), &gap);
    check_string_length(text.len() as f64)?;
    Ok(Value::Str(text))
}

pub fn _quote(text: &JsStr) -> JsStr {
    let mut out = JsStrBuf::new();
    crate::json::quote(text, &mut out);
    out.finish()
}
