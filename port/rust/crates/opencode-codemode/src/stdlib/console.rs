//! Captured console (console.ts): every method appends one formatted line to the run's logs.

use crate::interp::coerce::{contains_opaque_reference, to_string_plain};
use crate::interp::object::{entries, get, get_own};
use crate::interp::value::*;
use crate::interp::{Ctx, R};
use crate::json::quote;
use crate::jsstr::{JsStr, JsStrBuf};
use crate::numconv;

const METHODS: [&str; 7] = ["log", "info", "debug", "warn", "error", "dir", "table"];
const MAX_CONSOLE_DEPTH: usize = 32;

pub fn console_global(ctx: &Ctx) -> Value {
    let console = ctx.new_object(Some(ctx.builtins.object.clone()), Kind::Namespace("console"));
    for name in METHODS {
        ctx.method(&console, name, 0, move |ctx, _, args, _| {
            let line = format_message(ctx, name, &args)?;
            ctx.logs.borrow_mut().push(line);
            Ok(Value::Undefined)
        });
    }
    Value::Obj(console)
}

fn format_message(ctx: &Ctx, name: &str, args: &[Value]) -> R<JsStr> {
    if name == "dir" {
        return Ok(match args.first() {
            None => JsStr::from("undefined"),
            Some(value) => format_value(ctx, value),
        });
    }
    if name == "table" {
        return Ok(format_table(
            ctx,
            args.first().cloned().unwrap_or_default(),
            args.get(1).cloned().unwrap_or_default(),
        ));
    }
    let prefix = match name {
        "warn" => "[warn] ",
        "error" => "[error] ",
        "debug" => "[debug] ",
        _ => "",
    };
    let mut out = JsStrBuf::new();
    out.push_str(prefix);
    for (index, part) in format_arguments(ctx, args)?.iter().enumerate() {
        if index > 0 {
            out.push_unit(b' ' as u16);
        }
        out.push_js(part);
    }
    Ok(out.finish())
}

// A first string argument with more after it takes them in its format specifiers.
fn format_arguments(ctx: &Ctx, args: &[Value]) -> R<Vec<JsStr>> {
    let Some(Value::Str(first)) = args.first() else {
        return Ok(args.iter().map(|arg| format_value(ctx, arg)).collect());
    };
    if args.len() < 2 {
        return Ok(vec![format_value(ctx, &args[0])]);
    }
    let mut next = 1;
    let mut text = JsStrBuf::new();
    let units = first.units();
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if unit == b'%' as u16
            && let Some(&kind) = units.get(index + 1)
            && let Some(kind) = char::from_u32(kind as u32).filter(|k| "sdifoOc%".contains(*k))
        {
            index += 2;
            if kind == '%' {
                text.push_unit(b'%' as u16);
                continue;
            }
            if next >= args.len() {
                text.push_unit(b'%' as u16);
                text.push_char(kind);
                continue;
            }
            let arg = &args[next];
            next += 1;
            match kind {
                'c' => {}
                'd' | 'i' => {
                    let number = if let Value::Sym(_) = arg {
                        f64::NAN
                    } else {
                        numconv::parse_int(to_string_plain(arg).units(), 10.0)
                    };
                    text.push_js(&format_value(ctx, &Value::Num(number)));
                }
                'f' => {
                    let number =
                        if let Value::Sym(_) = arg { f64::NAN } else { numconv::parse_float(to_string_plain(arg).units()) };
                    text.push_js(&format_value(ctx, &Value::Num(number)));
                }
                _ => text.push_js(&format_value(ctx, arg)),
            }
            continue;
        }
        text.push_unit(unit);
        index += 1;
    }
    let mut out = vec![text.finish()];
    out.extend(args[next..].iter().map(|arg| format_value(ctx, arg)));
    Ok(out)
}

/// One value as `console.log` shows it.
pub fn format_value(ctx: &Ctx, value: &Value) -> JsStr {
    match value {
        Value::Undefined => JsStr::from("undefined"),
        Value::Str(text) => text.clone(),
        other => format_console_value(ctx, other, &mut Vec::new(), 0),
    }
}

fn format_console_value(ctx: &Ctx, value: &Value, seen: &mut Vec<usize>, depth: usize) -> JsStr {
    let obj = match value {
        Value::Undefined | Value::Null => return JsStr::from("null"),
        Value::Str(text) => {
            let mut out = JsStrBuf::new();
            quote(text, &mut out);
            return out.finish();
        }
        Value::Tool(_) => return JsStr::from("[opaque reference]"),
        Value::Obj(obj) => obj,
        other => return to_string_plain(other),
    };
    if depth > MAX_CONSOLE_DEPTH {
        return JsStr::from("...");
    }
    if seen.contains(&obj.addr()) {
        return JsStr::from("[Circular]");
    }
    seen.push(obj.addr());
    let shown = inspect(ctx, obj, &mut |item: &Value, seen: &mut Vec<usize>| format_console_value(ctx, item, seen, depth + 1), seen);
    seen.pop();
    shown
}

/// How `console.log` shows an object; `item` formats a child with cycle and depth tracking.
fn inspect(
    ctx: &Ctx,
    obj: &Obj,
    item: &mut dyn FnMut(&Value, &mut Vec<usize>) -> JsStr,
    seen: &mut Vec<usize>,
) -> JsStr {
    let mut out = JsStrBuf::new();
    match obj.kind() {
        Kind::Array(data) => {
            out.push_unit(b'[' as u16);
            for index in 0..data.len() {
                if index > 0 {
                    out.push_unit(b',' as u16);
                }
                if let Some(value) = data.get(index) {
                    out.push_js(&item(&value, seen));
                }
            }
            out.push_unit(b']' as u16);
        }
        Kind::Error(_) => return crate::interp::coerce::obj_to_string(obj),
        Kind::Promise(_) => return JsStr::from("[Promise (await it to get its value)]"),
        Kind::Function(_) | Kind::Generator(_) | Kind::Iterator(_) => return JsStr::from("[opaque reference]"),
        Kind::Map(map) => {
            let pairs = map.borrow().entries();
            out.push_str(&format!("Map({}) [", pairs.len()));
            for (index, (key, value)) in pairs.iter().enumerate() {
                if index > 0 {
                    out.push_unit(b',' as u16);
                }
                out.push_unit(b'[' as u16);
                out.push_js(&item(key, seen));
                out.push_unit(b',' as u16);
                out.push_js(&item(value, seen));
                out.push_unit(b']' as u16);
            }
            out.push_unit(b']' as u16);
        }
        Kind::Set(set) => {
            let values = set.borrow().values();
            out.push_str(&format!("Set({}) [", values.len()));
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push_unit(b',' as u16);
                }
                out.push_js(&item(value, seen));
            }
            out.push_unit(b']' as u16);
        }
        Kind::WeakMap(_) => return JsStr::from("WeakMap { <items unknown> }"),
        Kind::WeakSet(_) => return JsStr::from("WeakSet { <items unknown> }"),
        Kind::Headers(headers) => {
            let mut object = crate::json::JsonObj::new();
            for (name, value) in headers.borrow().sorted_pairs() {
                object.insert(name, crate::json::Json::Str(value));
            }
            out.push_str("Headers ");
            out.push_js(&crate::json::Json::Obj(object).to_js_string());
        }
        Kind::Bytes(data) => {
            let bytes: Vec<String> = data.to_vec().iter().map(|b| b.to_string()).collect();
            out.push_str(&format!("Uint8Array({}) [{}]", bytes.len(), bytes.join(",")));
        }
        Kind::Date(_) | Kind::RegExp(_) | Kind::URL(..) | Kind::URLSearchParams(_) => {
            return crate::interp::coerce::obj_to_string(obj);
        }
        _ => {
            out.push_unit(b'{' as u16);
            let pairs = entries(ctx, obj).unwrap_or_default();
            for (index, (key, value)) in pairs.iter().enumerate() {
                if index > 0 {
                    out.push_unit(b',' as u16);
                }
                quote(&key.to_js(), &mut out);
                out.push_unit(b':' as u16);
                out.push_js(&item(value, seen));
            }
            out.push_unit(b'}' as u16);
        }
    }
    out.finish()
}

struct Row {
    index: Option<JsStr>,
    values: Vec<(JsStr, Value)>,
}

fn format_table(ctx: &Ctx, value: Value, columns_argument: Value) -> JsStr {
    if value.is_undefined() {
        return JsStr::from("undefined");
    }
    if contains_opaque_reference(&value) {
        return JsStr::from("[opaque reference]");
    }
    let columns: Option<Vec<JsStr>> = match &columns_argument {
        Value::Obj(obj) => obj.array().map(|data| data.values().iter().map(to_string_plain).collect()),
        _ => None,
    };
    let row_values = |item: &Value| -> Vec<(JsStr, Value)> {
        match item {
            Value::Obj(obj) if !obj.is_array() => match &columns {
                Some(columns) => columns
                    .iter()
                    .map(|column| (column.clone(), get(ctx, obj, &PropKey::from_js(column.clone())).unwrap_or_default()))
                    .collect(),
                None => entries(ctx, obj).unwrap_or_default().into_iter().map(|(k, v)| (k.to_js(), v)).collect(),
            },
            other => vec![(JsStr::from("Value"), other.clone())],
        }
    };
    let rows: Vec<Row> = match &value {
        Value::Obj(obj) if obj.is_array() => {
            let data = obj.array().unwrap();
            (0..data.len())
                .map(|index| match data.get(index) {
                    Some(item) => Row { index: Some(JsStr::from(index.to_string())), values: row_values(&item) },
                    None => Row { index: None, values: Vec::new() },
                })
                .collect()
        }
        Value::Obj(obj) => entries(ctx, obj)
            .unwrap_or_default()
            .into_iter()
            .map(|(key, item)| Row { index: Some(key.to_js()), values: row_values(&item) })
            .collect(),
        other => vec![Row { index: Some(JsStr::from("0")), values: vec![(JsStr::from("Value"), other.clone())] }],
    };
    let keys: Vec<JsStr> = match &columns {
        Some(columns) => columns.clone(),
        None => {
            let mut keys: Vec<JsStr> = Vec::new();
            for row in &rows {
                let mut ordered = crate::json::JsonObj::new();
                for (key, _) in &row.values {
                    ordered.insert(key.clone(), crate::json::Json::Null);
                }
                for (key, _) in ordered.iter() {
                    if !keys.contains(key) {
                        keys.push(key.clone());
                    }
                }
            }
            keys
        }
    };
    let mut out = JsStrBuf::new();
    out.push_str("(index)");
    for key in &keys {
        out.push_unit(b'\t' as u16);
        out.push_js(key);
    }
    for row in &rows {
        out.push_unit(b'\n' as u16);
        let Some(index) = &row.index else { continue };
        out.push_js(index);
        for key in &keys {
            out.push_unit(b'\t' as u16);
            let cell = row.values.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.clone());
            match cell {
                None | Some(Value::Undefined) => {}
                Some(Value::Str(text)) => out.push_js(&text),
                Some(other) => out.push_js(&format_console_value(ctx, &other, &mut Vec::new(), 0)),
            }
        }
    }
    out.finish()
}

pub fn _unused(ctx: &Ctx, obj: &Obj) -> R<Value> {
    get_own(ctx, obj, &PropKey::from_str("x"))
}
