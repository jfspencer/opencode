//! `String` and `String.prototype` (string.ts), on UTF-16 strings with host semantics.

use crate::interp::callback::{Hint, apply_collection_callback, is_supported_callback, to_primitive_string, with_primitives};
use crate::interp::coerce::{contains_opaque_reference, is_callable_value, to_integer, to_number_plain, to_string_plain};
use crate::interp::cursor::{Cursor, HostIter};
use crate::interp::errors::{Fail, invalid_data, range_error, type_error};
use crate::interp::limits::{check_array_length, check_string_length};
use crate::interp::native::NativeOptions;
use crate::interp::object::{define, get};
use crate::interp::value::*;
use crate::interp::{Ctx, R, boxed};
use crate::jsregex::JsRegExp;
use crate::jsstr::{JsStr, JsStrBuf};
use crate::stdlib::regexp::{match_to_value, to_host_regex};
use crate::stdlib::value::coercion;
use std::rc::Rc;

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}

fn require_data(name: &str, index: usize, value: &Value) -> R<()> {
    if contains_opaque_reference(value) {
        return Err(invalid_data(format!("String.{name} expects argument {} to be a data value.", index + 1)));
    }
    Ok(())
}

fn str_arg(name: &str, args: &[Value], index: usize) -> R<JsStr> {
    let value = arg(args, index);
    require_data(name, index, &value)?;
    Ok(to_string_plain(&value))
}

fn num_arg(name: &str, args: &[Value], index: usize) -> R<f64> {
    let value = arg(args, index);
    require_data(name, index, &value)?;
    crate::interp::coerce::to_number_checked(&value)
}

fn opt_num(name: &str, args: &[Value], index: usize) -> R<Option<f64>> {
    if arg(args, index).is_undefined() {
        return Ok(None);
    }
    num_arg(name, args, index).map(Some)
}

fn opt_str(name: &str, args: &[Value], index: usize) -> R<Option<JsStr>> {
    if arg(args, index).is_undefined() {
        return Ok(None);
    }
    str_arg(name, args, index).map(Some)
}

fn integer(n: f64) -> f64 {
    crate::interp::coerce::integer_of(n)
}

fn clamp_index(n: Option<f64>, len: usize, default: f64) -> usize {
    integer(n.unwrap_or(default)).clamp(0.0, len as f64) as usize
}

fn relative(n: Option<f64>, len: usize, default: f64) -> usize {
    let n = integer(n.unwrap_or(default));
    if n < 0.0 { (len as f64 + n).max(0.0) as usize } else { n.min(len as f64) as usize }
}

pub fn is_js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    )
}

fn trim(value: &JsStr, start: bool, end: bool) -> JsStr {
    let units = value.units();
    let mut from = 0;
    let mut to = units.len();
    if start {
        while from < to && is_js_whitespace(units[from]) {
            from += 1;
        }
    }
    if end {
        while to > from && is_js_whitespace(units[to - 1]) {
            to -= 1;
        }
    }
    value.slice(from, to)
}

/// Applies a transform to each well-formed run, keeping lone surrogates as they are.
fn map_well_formed(value: &JsStr, transform: impl Fn(&str) -> String) -> JsStr {
    let units = value.units();
    let mut out = JsStrBuf::new();
    let mut run: Vec<u16> = Vec::new();
    let flush = |run: &mut Vec<u16>, out: &mut JsStrBuf| {
        if !run.is_empty() {
            out.push_str(&transform(&String::from_utf16(run).unwrap()));
            run.clear();
        }
    };
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if (0xD800..0xDC00).contains(&unit) && units.get(index + 1).is_some_and(|n| (0xDC00..0xE000).contains(n)) {
            run.push(unit);
            run.push(units[index + 1]);
            index += 2;
            continue;
        }
        if (0xD800..0xE000).contains(&unit) {
            flush(&mut run, &mut out);
            out.push_unit(unit);
        } else {
            run.push(unit);
        }
        index += 1;
    }
    flush(&mut run, &mut out);
    out.finish()
}

fn index_of(value: &[u16], search: &[u16], from: usize) -> Option<usize> {
    if search.is_empty() {
        return Some(from.min(value.len()));
    }
    if search.len() > value.len() {
        return None;
    }
    (from..=value.len() - search.len()).find(|&i| &value[i..i + search.len()] == search)
}

fn last_index_of(value: &[u16], search: &[u16], from: usize) -> Option<usize> {
    if search.len() > value.len() {
        return None;
    }
    let start = from.min(value.len() - search.len());
    (0..=start).rev().find(|&i| &value[i..i + search.len()] == search)
}

/// Default (root) collation order approximating ICU for localeCompare.
pub fn locale_compare(a: &JsStr, b: &JsStr) -> f64 {
    fn key(text: &JsStr) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        let mut primary = Vec::new();
        let mut secondary = Vec::new();
        let mut tertiary = Vec::new();
        let decomposed: String = {
            use unicode_normalization::UnicodeNormalization;
            text.to_rust_lossy().nfd().collect()
        };
        for ch in decomposed.chars() {
            if unicode_normalization::char::is_combining_mark(ch) {
                secondary.push(ch as u32);
                continue;
            }
            let lower = ch.to_lowercase().next().unwrap_or(ch);
            // The root collation's primary order: whitespace, punctuation and symbols (ASCII ones in their
            // collation order), digits, then letters.
            const SYMBOLS: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";
            let (class, weight) = if ch.is_whitespace() {
                (1, ch as u32)
            } else if let Some(at) = SYMBOLS.find(ch) {
                (2, at as u32)
            } else if !ch.is_alphanumeric() {
                (2, 0x100 + ch as u32)
            } else if ch.is_numeric() {
                (3, lower.to_digit(10).unwrap_or(lower as u32))
            } else {
                (4, lower as u32)
            };
            primary.push((class << 24) | weight);
            secondary.push(0);
            tertiary.push(if ch.is_uppercase() { 1 } else { 0 });
        }
        (primary, secondary, tertiary)
    }
    let (ka, kb) = (key(a), key(b));
    let order = ka.0.cmp(&kb.0).then(ka.1.cmp(&kb.1)).then(ka.2.cmp(&kb.2));
    match order {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Greater => 1.0,
        std::cmp::Ordering::Equal => 0.0,
    }
}

fn split_string(value: &JsStr, separator: &JsStr, limit: Option<u32>) -> Vec<JsStr> {
    let limit = limit.unwrap_or(u32::MAX) as usize;
    let mut out = Vec::new();
    if limit == 0 {
        return out;
    }
    let units = value.units();
    let sep = separator.units();
    if sep.is_empty() {
        for index in 0..units.len().min(limit) {
            out.push(value.slice(index, index + 1));
        }
        return out;
    }
    if units.is_empty() {
        return vec![value.clone()];
    }
    let mut start = 0;
    while let Some(at) = index_of(units, sep, start) {
        if at + sep.len() > units.len() || (at == start && sep.is_empty()) {
            break;
        }
        out.push(value.slice(start, at));
        if out.len() == limit {
            return out;
        }
        start = at + sep.len();
    }
    out.push(value.slice(start, units.len()));
    out
}

type Op = Rc<dyn Fn(&Ctx, JsStr, Vec<Value>) -> R<NOut>>;

// ToPrimitive in spec order: the receiver, then the arguments the method consumes, one hint per position.
fn simple(ctx: &Ctx, proto: &Obj, name: &'static str, length: u32, hints: &'static [Hint], op: Op) {
    ctx.method_work(proto, name, length, move |ctx, this, args, _| {
        if this.is_nullish() {
            return Err(type_error(format!("String.prototype.{name} called on null or undefined.")));
        }
        let mut values = vec![this];
        let consumed = hints.len().min(args.len());
        values.extend(args[..consumed].iter().cloned());
        let rest: Vec<Value> = args[consumed..].to_vec();
        let mut all_hints = vec![Hint::String];
        all_hints.extend_from_slice(hints);
        let op = op.clone();
        with_primitives(ctx, &all_hints, values, move |ctx, primitives| {
            let value = to_string_plain(&primitives[0]);
            let mut operands: Vec<Value> = primitives[1..].to_vec();
            operands.extend(rest);
            op(ctx, value, operands)
        })
    });
}

fn val(value: Value) -> R<NOut> {
    Ok(NOut::Val(value))
}

fn string(value: JsStr) -> R<NOut> {
    Ok(NOut::Val(Value::Str(value)))
}

fn regexp_of(value: &Value) -> Option<Rc<JsRegExp>> {
    match value {
        Value::Obj(obj) => match obj.kind() {
            Kind::RegExp(regex) => Some(regex.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn replace_all_needs_global(regex: &JsRegExp) -> R<()> {
    if !regex.global() {
        let source = String::from_utf16_lossy(&regex.source());
        let flags = regex.flags();
        return Err(type_error(format!(
            "String.replaceAll requires a regular expression with the global (g) flag: write /{source}/{flags}g, or use String.replace to replace only the first match."
        )));
    }
    Ok(())
}

async fn replace_with_callback(ctx: Ctx, value: JsStr, name: &'static str, pattern: Value, replacer: Value) -> R<Value> {
    let apply = apply_collection_callback(&ctx, &replacer, &format!("String.{name}"))?;
    let units = value.units();
    // Each match is (offset, length, callback arguments), collected before any callback runs.
    let mut matches: Vec<(usize, usize, Vec<Value>)> = Vec::new();
    if let Some(regex) = regexp_of(&pattern) {
        if name == "replaceAll" {
            replace_all_needs_global(&regex)?;
        }
        for found in crate::jsregex::replace_matches(&regex, units) {
            let (start, end) = found.captures[0].expect("a match");
            let mut args: Vec<Value> = found
                .captures
                .iter()
                .map(|capture| match capture {
                    Some((s, e)) => Value::Str(value.slice(*s, *e)),
                    None => Value::Undefined,
                })
                .collect();
            args.push(Value::Num(start as f64));
            args.push(Value::Str(value.clone()));
            if let Some(groups) = &found.groups {
                let record = ctx.new_plain();
                for (group, range) in groups {
                    let item = match range {
                        Some((s, e)) => Value::Str(value.slice(*s, *e)),
                        None => Value::Undefined,
                    };
                    define(&record, PropKey::from_js(JsStr::from_slice(group)), item, DATA);
                }
                args.push(Value::Obj(record));
            }
            matches.push((start, end - start, args));
        }
    } else {
        require_data(name, 0, &pattern)?;
        let search = to_string_plain(&pattern);
        for at in crate::jsregex::string_replace_positions(units, search.units(), name == "replaceAll") {
            matches.push((at, search.len(), vec![Value::Str(search.clone()), Value::Num(at as f64), Value::Str(value.clone())]));
        }
    }
    let mut output = JsStrBuf::new();
    let mut end = 0;
    for (offset, length, args) in matches {
        let replacement = apply(args, Value::Undefined).await?;
        output.push_units(&units[end.min(offset)..offset]);
        output.push_js(&to_string_plain(&replacement));
        end = offset + length;
    }
    output.push_units(&units[end.min(units.len())..]);
    Ok(Value::Str(output.finish()))
}

pub fn string_global(ctx: &Ctx) -> Value {
    let builtins = &ctx.builtins;
    let call = coercion(ctx, "String", 1).native_fn().unwrap().call.borrow().clone();
    let string_ctor =
        ctx.constructor(&builtins.string, NativeOptions { name: "String".into(), length: 1, call, construct: None, callback: true });
    ctx.method(&string_ctor, "fromCharCode", 1, |_, _, args, _| {
        let mut out = JsStrBuf::new();
        for value in &args {
            out.push_unit(crate::numconv::to_uint32(to_number_plain(value)) as u16);
        }
        Ok(Value::Str(out.finish()))
    });
    ctx.method(&string_ctor, "fromCodePoint", 1, |_, _, args, _| {
        let mut out = JsStrBuf::new();
        for value in &args {
            let point = to_number_plain(value);
            if point.fract() != 0.0 || !(0.0..=1114111.0).contains(&point) {
                return Err(Fail::Host(Rc::new((
                    JsStr::from("RangeError"),
                    JsStr::from(format!("Invalid code point {}", crate::numconv::to_string(point))),
                ))));
            }
            out.push_code_point(point as u32);
        }
        Ok(Value::Str(out.finish()))
    });
    ctx.method_work(&string_ctor, "raw", 1, |ctx, _, args, _| {
        let template = arg(&args, 0);
        let raw = match &template {
            Value::Obj(obj) => get(ctx, obj, &PropKey::from_str("raw"))?,
            _ => Value::Undefined,
        };
        if raw.is_nullish() {
            return Err(type_error("String.raw expects a template object with a raw array."));
        }
        let count = match &raw {
            Value::Str(text) => text.len() as f64,
            Value::Obj(obj) => to_integer(&get(ctx, obj, &PropKey::from_str("length"))?).max(0.0),
            _ => 0.0,
        };
        check_array_length(count)?;
        let count = count as usize;
        let mut parts = Vec::new();
        for index in 0..count {
            let literal = match &raw {
                Value::Str(text) => Value::Str(text.slice(index, index + 1)),
                Value::Obj(obj) => get(ctx, obj, &PropKey::Index(index as u32))?,
                _ => Value::Undefined,
            };
            parts.push(literal);
            if index + 1 < count && index + 1 < args.len() {
                parts.push(args[index + 1].clone());
            }
        }
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let mut out = JsStrBuf::new();
            for part in parts {
                out.push_js(&to_primitive_string(&ctx, part).await?);
            }
            check_string_length(out.len() as f64)?;
            Ok(Value::Str(out.finish()))
        })))
    });

    let proto = builtins.string.clone();
    let p = &proto;
    simple(ctx, p, "toString", 0, &[], Rc::new(|_, value, _| string(value)));
    simple(ctx, p, "valueOf", 0, &[], Rc::new(|_, value, _| string(value)));
    for (name, upper) in [("toLowerCase", false), ("toUpperCase", true), ("toLocaleLowerCase", false), ("toLocaleUpperCase", true)] {
        simple(
            ctx,
            p,
            name,
            0,
            &[],
            Rc::new(move |_, value, _| {
                string(map_well_formed(&value, |text| if upper { text.to_uppercase() } else { text.to_lowercase() }))
            }),
        );
    }
    for (name, start, end) in [
        ("trim", true, true),
        ("trimStart", true, false),
        ("trimLeft", true, false),
        ("trimEnd", false, true),
        ("trimRight", false, true),
    ] {
        simple(ctx, p, name, 0, &[], Rc::new(move |_, value, _| string(trim(&value, start, end))));
    }
    simple(
        ctx,
        p,
        "localeCompare",
        1,
        &[Hint::String],
        Rc::new(|_, value, args| val(Value::Num(locale_compare(&value, &str_arg("localeCompare", &args, 0)?)))),
    );
    simple(
        ctx,
        p,
        "normalize",
        0,
        &[Hint::String],
        Rc::new(|_, value, args| {
            use unicode_normalization::UnicodeNormalization;
            let form = opt_str("normalize", &args, 0)?;
            let name = form.as_ref().map(|f| f.to_rust_lossy()).unwrap_or_else(|| "NFC".into());
            let transform: fn(&str) -> String = match name.as_str() {
                "NFC" => |t| t.nfc().collect(),
                "NFD" => |t| t.nfd().collect(),
                "NFKC" => |t| t.nfkc().collect(),
                "NFKD" => |t| t.nfkd().collect(),
                _ => {
                    let shown = form.map(|f| crate::stdlib::json::_quote(&f).to_rust_lossy()).unwrap_or_default();
                    return Err(range_error(format!(
                        "String.normalize expects the form \"NFC\", \"NFD\", \"NFKC\", or \"NFKD\" (got {shown})."
                    )));
                }
            };
            string(map_well_formed(&value, transform))
        }),
    );
    ctx.method_work(p, "split", 2, |ctx, this, args, _| {
        if this.is_nullish() {
            return Err(type_error("String.prototype.split called on null or undefined."));
        }
        with_primitives(ctx, &[Hint::String], vec![this], move |ctx, primitives| {
            let value = to_string_plain(&primitives[0]);
            let separator = arg(&args, 0);
            let regex = regexp_of(&separator);
            // A RegExp separator is used as is; the limit converts before a plain separator does.
            let pattern = if regex.is_some() { Value::Undefined } else { separator.clone() };
            let limit_arg = arg(&args, 1);
            with_primitives(ctx, &[Hint::Number, Hint::String], vec![limit_arg.clone(), pattern], move |ctx, values| {
                let requested =
                    if limit_arg.is_undefined() { None } else { Some(num_arg("split", &[values[1].clone(), values[0].clone()], 1)?) };
                let wrap = |parts: Vec<Value>| -> R<NOut> {
                    check_array_length(parts.len() as f64)?;
                    Ok(NOut::Val(Value::Obj(ctx.new_array(parts))))
                };
                // Native: an undefined separator returns the whole string, unless the limit truncates to zero.
                if separator.is_undefined() {
                    return wrap(match requested {
                        Some(limit) if crate::numconv::to_uint32(limit) == 0 => Vec::new(),
                        _ => vec![Value::Str(value)],
                    });
                }
                let limit = requested.map(crate::numconv::to_uint32);
                if let Some(regex) = &regex {
                    let parts = crate::jsregex::split(regex, value.units(), limit);
                    return wrap(
                        parts
                            .into_iter()
                            .map(|part| part.map(|units| Value::Str(JsStr::from_units(units))).unwrap_or(Value::Undefined))
                            .collect(),
                    );
                }
                let separator = str_arg("split", &values[1..], 0)?;
                wrap(split_string(&value, &separator, limit).into_iter().map(Value::Str).collect())
            })
        })
    });
    simple(
        ctx,
        p,
        "slice",
        2,
        &[Hint::Number, Hint::Number],
        Rc::new(|_, value, args| {
            let len = value.len();
            let start = relative(opt_num("slice", &args, 0)?, len, 0.0);
            let end = relative(opt_num("slice", &args, 1)?, len, len as f64);
            string(if start < end { value.slice(start, end) } else { JsStr::new() })
        }),
    );
    for name in ["includes", "startsWith", "endsWith"] {
        simple(
            ctx,
            p,
            name,
            1,
            &[],
            Rc::new(move |ctx, value, args| {
                if regexp_of(&arg(&args, 0)).is_some() {
                    return Err(type_error(format!(
                        "String.{name} cannot take a regular expression; use regex.test(string) or String.search instead."
                    )));
                }
                let consumed: Vec<Value> = args.iter().take(2).cloned().collect();
                with_primitives(ctx, &[Hint::String, Hint::Number], consumed, move |_, primitives| {
                    let search = str_arg(name, &primitives, 0)?;
                    let position = opt_num(name, &primitives, 1)?;
                    let units = value.units();
                    let len = units.len();
                    let found = match name {
                        "includes" => index_of(units, search.units(), clamp_index(position, len, 0.0)).is_some(),
                        "startsWith" => {
                            let start = clamp_index(position, len, 0.0);
                            units.len() >= start + search.len() && &units[start..start + search.len()] == search.units()
                        }
                        _ => {
                            let end = clamp_index(position, len, len as f64);
                            end >= search.len() && &units[end - search.len()..end] == search.units()
                        }
                    };
                    val(Value::Bool(found))
                })
            }),
        );
    }
    simple(
        ctx,
        p,
        "indexOf",
        1,
        &[Hint::String, Hint::Number],
        Rc::new(|_, value, args| {
            let search = str_arg("indexOf", &args, 0)?;
            let from = clamp_index(opt_num("indexOf", &args, 1)?, value.len(), 0.0);
            val(Value::Num(index_of(value.units(), search.units(), from).map(|i| i as f64).unwrap_or(-1.0)))
        }),
    );
    simple(
        ctx,
        p,
        "lastIndexOf",
        1,
        &[Hint::String, Hint::Number],
        Rc::new(|_, value, args| {
            let search = str_arg("lastIndexOf", &args, 0)?;
            let position = opt_num("lastIndexOf", &args, 1)?;
            let from = match position {
                Some(n) if !n.is_nan() => integer(n).clamp(0.0, value.len() as f64) as usize,
                _ => value.len(),
            };
            val(Value::Num(last_index_of(value.units(), search.units(), from).map(|i| i as f64).unwrap_or(-1.0)))
        }),
    );
    for name in ["replace", "replaceAll"] {
        simple(
            ctx,
            p,
            name,
            2,
            &[],
            Rc::new(move |ctx, value, args| {
                let pattern = arg(&args, 0);
                let replacer = arg(&args, 1);
                let regex = regexp_of(&pattern);
                let search = if regex.is_some() { Value::Undefined } else { pattern.clone() };
                let replacement = if is_supported_callback(&replacer) { Value::Undefined } else { replacer.clone() };
                with_primitives(ctx, &[Hint::String], vec![search, replacement], move |ctx, primitives| {
                    if is_supported_callback(&replacer) {
                        let pattern = if regex.is_some() { pattern } else { primitives[0].clone() };
                        return Ok(NOut::Work(boxed(replace_with_callback(ctx.clone(), value, name, pattern, replacer))));
                    }
                    if is_callable_value(&replacer) {
                        return Err(type_error(format!(
                            "String.{name} cannot use this callable as a replacer; wrap it in an arrow function, e.g. (match) => tools.ns.tool(match)."
                        )));
                    }
                    if let Some(regex) = &regex {
                        let text = str_arg(name, &primitives, 1)?;
                        if name == "replaceAll" {
                            replace_all_needs_global(regex)?;
                        }
                        return string(JsStr::from_units(crate::jsregex::replace(regex, value.units(), text.units())));
                    }
                    let search = str_arg(name, &primitives, 0)?;
                    let text = str_arg(name, &primitives, 1)?;
                    string(JsStr::from_units(crate::jsregex::string_replace(
                        value.units(),
                        search.units(),
                        text.units(),
                        name == "replaceAll",
                    )))
                })
            }),
        );
    }
    // match, matchAll, and search read a RegExp as is and convert anything else to its pattern text.
    for name in ["match", "matchAll", "search"] {
        simple(
            ctx,
            p,
            name,
            1,
            &[],
            Rc::new(move |ctx, value, args| {
                let pattern = arg(&args, 0);
                let run = move |ctx: &Ctx, pattern: Value| -> R<NOut> {
                    let units = value.units();
                    match name {
                        "match" => {
                            let regex = to_host_regex(&pattern, "match", "")?;
                            if regex.global() {
                                return val(match crate::jsregex::match_global(&regex, units) {
                                    None => Value::Null,
                                    Some(all) => Value::Obj(
                                        ctx.new_array(all.into_iter().map(|m| Value::Str(JsStr::from_units(m))).collect()),
                                    ),
                                });
                            }
                            val(match regex.exec(units) {
                                None => Value::Null,
                                Some(found) => Value::Obj(match_to_value(ctx, &found, &value, regex.has_indices())),
                            })
                        }
                        "matchAll" => {
                            let regex = to_host_regex(&pattern, "matchAll", "g")?;
                            if !regex.global() {
                                let source = String::from_utf16_lossy(&regex.source());
                                return Err(type_error(format!(
                                    "String.matchAll requires a regular expression with the global (g) flag: write /{source}/{}g, or use String.match for a single match.",
                                    regex.flags()
                                )));
                            }
                            let mut matches = Vec::new();
                            for found in crate::jsregex::match_all(&regex, units) {
                                check_array_length((matches.len() + 1) as f64)?;
                                matches.push(Value::Obj(match_to_value(ctx, &found, &value, regex.has_indices())));
                            }
                            val(Value::Obj(ctx.new_array(matches)))
                        }
                        _ => {
                            let regex = to_host_regex(&pattern, "search", "")?;
                            val(Value::Num(crate::jsregex::search(&regex, units) as f64))
                        }
                    }
                };
                if regexp_of(&pattern).is_some() {
                    return run(ctx, pattern);
                }
                with_primitives(ctx, &[Hint::String], vec![pattern], move |ctx, values| run(ctx, values[0].clone()))
            }),
        );
    }
    simple(
        ctx,
        p,
        "repeat",
        1,
        &[Hint::Number],
        Rc::new(|_, value, args| {
            let count = num_arg("repeat", &args, 0)?;
            if !count.is_finite() || count < 0.0 {
                return Err(range_error("String.repeat expects a finite non-negative count."));
            }
            check_string_length(value.len() as f64 * count)?;
            let count = integer(count) as usize;
            let mut out = Vec::with_capacity(value.len() * count);
            for _ in 0..count {
                out.extend_from_slice(value.units());
            }
            string(JsStr::from_units(out))
        }),
    );
    for (name, at_start) in [("padStart", true), ("padEnd", false)] {
        simple(
            ctx,
            p,
            name,
            1,
            &[Hint::Number, Hint::String],
            Rc::new(move |_, value, args| {
                let length = num_arg(name, &args, 0)?;
                check_string_length(length)?;
                let fill = opt_str(name, &args, 1)?.unwrap_or_else(|| JsStr::from(" "));
                let target = integer(length).max(0.0) as usize;
                if target <= value.len() || fill.is_empty() {
                    return string(value);
                }
                let needed = target - value.len();
                let mut padding = Vec::with_capacity(needed);
                while padding.len() < needed {
                    let take = (needed - padding.len()).min(fill.len());
                    padding.extend_from_slice(&fill.units()[..take]);
                }
                let mut out = Vec::with_capacity(target);
                if at_start {
                    out.extend(padding);
                    out.extend_from_slice(value.units());
                } else {
                    out.extend_from_slice(value.units());
                    out.extend(padding);
                }
                string(JsStr::from_units(out))
            }),
        );
    }
    simple(
        ctx,
        p,
        "charAt",
        1,
        &[Hint::Number],
        Rc::new(|_, value, args| {
            let index = integer(opt_num("charAt", &args, 0)?.unwrap_or(0.0));
            if index < 0.0 || index >= value.len() as f64 {
                return string(JsStr::new());
            }
            string(value.slice(index as usize, index as usize + 1))
        }),
    );
    simple(
        ctx,
        p,
        "at",
        1,
        &[Hint::Number],
        Rc::new(|_, value, args| {
            let n = integer(opt_num("at", &args, 0)?.unwrap_or(0.0));
            let index = if n < 0.0 { value.len() as f64 + n } else { n };
            if index < 0.0 || index >= value.len() as f64 {
                return val(Value::Undefined);
            }
            string(value.slice(index as usize, index as usize + 1))
        }),
    );
    simple(
        ctx,
        p,
        "substring",
        2,
        &[Hint::Number, Hint::Number],
        Rc::new(|_, value, args| {
            let len = value.len();
            let start = clamp_index(Some(opt_num("substring", &args, 0)?.unwrap_or(0.0)), len, 0.0);
            let end = clamp_index(opt_num("substring", &args, 1)?, len, len as f64);
            let (from, to) = (start.min(end), start.max(end));
            string(value.slice(from, to))
        }),
    );
    simple(
        ctx,
        p,
        "substr",
        2,
        &[Hint::Number, Hint::Number],
        Rc::new(|_, value, args| {
            let len = value.len();
            let start = relative(Some(opt_num("substr", &args, 0)?.unwrap_or(0.0)), len, 0.0);
            let length = match opt_num("substr", &args, 1)? {
                None => len as f64,
                Some(n) => integer(n),
            };
            let end = (start as f64 + length.max(0.0)).min(len as f64) as usize;
            string(if start < end { value.slice(start, end) } else { JsStr::new() })
        }),
    );
    simple(ctx, p, "isWellFormed", 0, &[], Rc::new(|_, value, _| val(Value::Bool(value.is_well_formed()))));
    simple(
        ctx,
        p,
        "toWellFormed",
        0,
        &[],
        Rc::new(|_, value, _| {
            let units = value.units();
            let mut out = Vec::with_capacity(units.len());
            let mut index = 0;
            while index < units.len() {
                let unit = units[index];
                if (0xD800..0xDC00).contains(&unit) && units.get(index + 1).is_some_and(|n| (0xDC00..0xE000).contains(n)) {
                    out.push(unit);
                    out.push(units[index + 1]);
                    index += 2;
                    continue;
                }
                out.push(if (0xD800..0xE000).contains(&unit) { 0xFFFD } else { unit });
                index += 1;
            }
            string(JsStr::from_units(out))
        }),
    );
    simple(
        ctx,
        p,
        "charCodeAt",
        1,
        &[Hint::Number],
        Rc::new(|_, value, args| {
            let index = integer(opt_num("charCodeAt", &args, 0)?.unwrap_or(0.0));
            if index < 0.0 || index >= value.len() as f64 {
                return val(Value::Num(f64::NAN));
            }
            val(Value::Num(value.units()[index as usize] as f64))
        }),
    );
    simple(
        ctx,
        p,
        "codePointAt",
        1,
        &[Hint::Number],
        Rc::new(|_, value, args| {
            let index = integer(opt_num("codePointAt", &args, 0)?.unwrap_or(0.0));
            if index < 0.0 || index >= value.len() as f64 {
                return val(Value::Undefined);
            }
            let units = value.units();
            let at = index as usize;
            let first = units[at];
            if (0xD800..0xDC00).contains(&first)
                && let Some(&second) = units.get(at + 1)
                && (0xDC00..0xE000).contains(&second)
            {
                let point = 0x10000 + (((first as u32) - 0xD800) << 10) + ((second as u32) - 0xDC00);
                return val(Value::Num(point as f64));
            }
            val(Value::Num(first as f64))
        }),
    );
    simple(
        ctx,
        p,
        "concat",
        1,
        &[],
        Rc::new(|ctx, value, args| {
            with_primitives(ctx, &[Hint::String], args, move |_, parts| {
                let mut out = JsStrBuf::new();
                out.push_js(&value);
                for index in 0..parts.len() {
                    out.push_js(&str_arg("concat", &parts, index)?);
                }
                check_string_length(out.len() as f64)?;
                string(out.finish())
            })
        }),
    );
    let iterator = ctx.native_fn("[Symbol.iterator]", 0, |ctx, this, _, _| {
        let text = match &this {
            Value::Str(text) => text.clone(),
            Value::Undefined | Value::Null => {
                return Err(type_error("String.prototype.[Symbol.iterator] called on null or undefined."));
            }
            other => to_string_plain(other),
        };
        let cursor = Cursor::host(HostIter::String { text, index: 0 });
        Ok(Value::Obj(ctx.new_object(Some(ctx.builtins.iterator.clone()), Kind::Iterator(cursor))))
    });
    define(&proto, PropKey::Sym(Sym::Iterator), Value::Obj(iterator), HIDDEN);
    Value::Obj(string_ctor)
}
