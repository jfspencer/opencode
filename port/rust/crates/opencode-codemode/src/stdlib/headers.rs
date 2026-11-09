//! `Headers` (headers.ts) with the WHATWG header list: names fold to lowercase, values are normalized and combined,
//! iteration is sorted by name with `set-cookie` values kept apart.

use crate::interp::callback::apply_collection_callback;
use crate::interp::coerce::{is_runtime_reference, to_string_plain};
use crate::interp::cursor::{Cursor, HostIter, IterKind};
use crate::interp::errors::type_error;
use crate::interp::native::{NativeOptions, prototype_from, receiver, requires_new};
use crate::interp::object::{define, entries, get_data};
use crate::interp::value::*;
use crate::interp::{Ctx, R, boxed};
use crate::jsstr::JsStr;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default, Clone)]
pub struct HeaderList {
    list: Vec<(String, String)>,
}

fn is_token(name: &[u16]) -> bool {
    !name.is_empty() && name.iter().all(|&u| u < 0x80 && ((u as u8).is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&(u as u8))))
}

fn normalize(value: &[u16]) -> &[u16] {
    let ws = |u: &u16| matches!(*u, 0x09 | 0x0A | 0x0D | 0x20);
    let start = value.iter().position(|u| !ws(u)).unwrap_or(value.len());
    let end = value.iter().rposition(|u| !ws(u)).map(|i| i + 1).unwrap_or(start);
    &value[start..end.max(start)]
}

fn latin1_string(units: &[u16]) -> String {
    units.iter().map(|&u| char::from_u32(u as u32).unwrap_or('\u{fffd}')).collect()
}

fn message(parts: &[&[u16]]) -> crate::jsstr::JsStr {
    let mut out = crate::jsstr::JsStrBuf::new();
    for part in parts {
        out.push_units(part);
    }
    out.finish()
}

fn check_name(name: &JsStr, has: bool) -> R<String> {
    if !is_token(name.units()) {
        let open: Vec<u16> = "Invalid header name: '".encode_utf16().collect();
        // bun's own wording; `has` closes the quote with a double quote.
        let close: Vec<u16> = if has { vec![b'"' as u16] } else { vec![b'\'' as u16] };
        return Err(type_error(message(&[&open, name.units(), &close])));
    }
    Ok(latin1_string(name.units()).to_ascii_lowercase())
}

fn check_value(name: &JsStr, value: &JsStr) -> R<String> {
    let normalized = normalize(value.units());
    if normalized.iter().any(|&u| matches!(u, 0 | 0x0A | 0x0D) || u > 0xFF) {
        let a: Vec<u16> = "Header '".encode_utf16().collect();
        let b: Vec<u16> = "' has invalid value: '".encode_utf16().collect();
        let c: Vec<u16> = vec![b'\'' as u16];
        return Err(type_error(message(&[&a, name.units(), &b, normalized, &c])));
    }
    Ok(latin1_string(normalized))
}

impl HeaderList {
    pub fn append(&mut self, name: &JsStr, value: &JsStr) -> R<()> {
        let lower = check_name(name, false)?;
        let value = check_value(name, value)?;
        self.list.push((lower, value));
        Ok(())
    }
    pub fn set(&mut self, name: &JsStr, value: &JsStr) -> R<()> {
        let lower = check_name(name, false)?;
        let value = check_value(name, value)?;
        match self.list.iter().position(|(n, _)| *n == lower) {
            Some(first) => {
                self.list[first].1 = value;
                let mut index = 0;
                self.list.retain(|(n, _)| {
                    let keep = index <= first || *n != lower;
                    index += 1;
                    keep
                });
            }
            None => self.list.push((lower, value)),
        }
        Ok(())
    }
    pub fn delete(&mut self, name: &JsStr) -> R<()> {
        let lower = check_name(name, false)?;
        self.list.retain(|(n, _)| *n != lower);
        Ok(())
    }
    pub fn get(&self, name: &JsStr) -> R<Option<String>> {
        let lower = check_name(name, false)?;
        let values: Vec<&str> = self.list.iter().filter(|(n, _)| *n == lower).map(|(_, v)| v.as_str()).collect();
        Ok(if values.is_empty() { None } else { Some(values.join(", ")) })
    }
    pub fn has(&self, name: &JsStr) -> R<bool> {
        let lower = check_name(name, true)?;
        Ok(self.list.iter().any(|(n, _)| *n == lower))
    }
    pub fn set_cookies(&self) -> Vec<String> {
        self.list.iter().filter(|(n, _)| n == "set-cookie").map(|(_, v)| v.clone()).collect()
    }
    /// Sort and combine: names in byte order, values joined, `set-cookie` values kept apart.
    pub fn sorted_pairs(&self) -> Vec<(JsStr, JsStr)> {
        let mut names: Vec<&str> = self.list.iter().map(|(n, _)| n.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        let mut out = Vec::new();
        for name in names {
            if name == "set-cookie" {
                for value in self.set_cookies() {
                    out.push((JsStr::from(name), JsStr::from(value)));
                }
                continue;
            }
            let values: Vec<&str> = self.list.iter().filter(|(n, _)| n == name).map(|(_, v)| v.as_str()).collect();
            out.push((JsStr::from(name), JsStr::from(values.join(", "))));
        }
        out
    }
    pub fn sorted_pair(&self, index: usize) -> Option<(JsStr, JsStr)> {
        self.sorted_pairs().into_iter().nth(index)
    }
}

fn latin1(text: &str) -> JsStr {
    JsStr::from_units(text.chars().map(|c| c as u32 as u16).collect())
}

async fn construct(ctx: Ctx, init: Value, proto: Obj) -> R<Value> {
    let mut list = HeaderList::default();
    if !init.is_undefined() {
        let pairs = match &init {
            Value::Obj(_) => crate::stdlib::url::read_pairs(&ctx, &init, "new Headers(...)").await?,
            _ => None,
        };
        match pairs {
            Some(pairs) => {
                for (name, value) in pairs {
                    list.append(&name, &value)?;
                }
            }
            None => {
                let Value::Obj(obj) = &init else {
                    return Err(type_error(
                        "new Headers(...) expects a record of names to values, iterable [name, value] pairs, or Headers.",
                    ));
                };
                if is_runtime_reference(&init) {
                    return Err(type_error(
                        "new Headers(...) expects a record of names to values, iterable [name, value] pairs, or Headers.",
                    ));
                }
                let mut record = crate::json::JsonObj::new();
                for (key, value) in entries(&ctx, obj)? {
                    record.insert(key.to_js(), crate::json::Json::Str(to_string_plain(&value)));
                }
                for (name, value) in record.iter() {
                    list.append(name, value.as_str().unwrap())?;
                }
            }
        }
    }
    Ok(Value::Obj(ctx.new_object(Some(proto), Kind::Headers(RefCell::new(list)))))
}

fn self_headers(this: &Value, name: &str) -> R<Obj> {
    receiver(this, |obj| matches!(obj.kind(), Kind::Headers(_)), &format!("Headers.prototype.{name}"))
}

fn with_list<T>(obj: &Obj, f: impl FnOnce(&mut HeaderList) -> R<T>) -> R<T> {
    let Kind::Headers(list) = obj.kind() else { unreachable!() };
    f(&mut list.borrow_mut())
}

fn require_args(name: &str, args: &[Value], count: usize) -> R<()> {
    if args.len() < count {
        return Err(type_error(format!("Headers.{name} requires {count} argument{}.", if count == 1 { "" } else { "s" })));
    }
    Ok(())
}

fn text(args: &[Value], index: usize) -> JsStr {
    to_string_plain(&args.get(index).cloned().unwrap_or_default())
}

pub fn headers_global(ctx: &Ctx) -> Value {
    let proto = ctx.builtins.headers.clone();
    let headers = ctx.constructor(
        &proto,
        NativeOptions {
            name: "Headers".into(),
            length: 0,
            call: requires_new("Headers"),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                let proto = prototype_from(&new_target, &ctx.builtins.headers);
                Ok(NOut::Work(boxed(construct(ctx.clone(), args.into_iter().next().unwrap_or_default(), proto))))
            })),
            callback: true,
        },
    );
    ctx.method(&proto, "append", 2, |_, this, args, _| {
        require_args("append", &args, 2)?;
        with_list(&self_headers(&this, "append")?, |list| list.append(&text(&args, 0), &text(&args, 1)))?;
        Ok(Value::Undefined)
    });
    ctx.method(&proto, "delete", 1, |_, this, args, _| {
        require_args("delete", &args, 1)?;
        with_list(&self_headers(&this, "delete")?, |list| list.delete(&text(&args, 0)))?;
        Ok(Value::Undefined)
    });
    ctx.method(&proto, "get", 1, |_, this, args, _| {
        require_args("get", &args, 1)?;
        let found = with_list(&self_headers(&this, "get")?, |list| list.get(&text(&args, 0)))?;
        Ok(found.map(|v| Value::Str(latin1(&v))).unwrap_or(Value::Null))
    });
    ctx.method(&proto, "getSetCookie", 0, |ctx, this, _, _| {
        let cookies = with_list(&self_headers(&this, "getSetCookie")?, |list| Ok(list.set_cookies()))?;
        Ok(Value::Obj(ctx.new_array(cookies.iter().map(|c| Value::Str(latin1(c))).collect())))
    });
    ctx.method(&proto, "has", 1, |_, this, args, _| {
        require_args("has", &args, 1)?;
        Ok(Value::Bool(with_list(&self_headers(&this, "has")?, |list| list.has(&text(&args, 0)))?))
    });
    ctx.method(&proto, "set", 2, |_, this, args, _| {
        require_args("set", &args, 2)?;
        with_list(&self_headers(&this, "set")?, |list| list.set(&text(&args, 0), &text(&args, 1)))?;
        Ok(Value::Undefined)
    });
    for (name, kind) in [("keys", IterKind::Keys), ("values", IterKind::Values), ("entries", IterKind::Entries)] {
        ctx.method(&proto, name, 0, move |ctx, this, _, _| {
            let target = self_headers(&this, name)?;
            let cursor = Cursor::host(HostIter::Headers { target, index: 0, kind });
            Ok(Value::Obj(ctx.new_object(Some(ctx.builtins.iterator.clone()), Kind::Iterator(cursor))))
        });
    }
    ctx.method_work(&proto, "forEach", 1, |ctx, this, args, _| {
        require_args("forEach", &args, 1)?;
        let target = self_headers(&this, "forEach")?;
        let apply = apply_collection_callback(ctx, &args[0], "Headers.forEach")?;
        let this_arg = args.get(1).cloned().unwrap_or_default();
        let pairs = with_list(&target, |list| Ok(list.sorted_pairs()))?;
        Ok(NOut::Work(boxed(async move {
            for (key, value) in pairs {
                apply(vec![Value::Str(value), Value::Str(key), this.clone()], this_arg.clone()).await?;
            }
            Ok(Value::Undefined)
        })))
    });
    let entries = get_data(&proto, &PropKey::from_str("entries"));
    define(&proto, PropKey::Sym(Sym::Iterator), entries, HIDDEN);
    Value::Obj(headers)
}
