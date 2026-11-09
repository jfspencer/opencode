//! `Uint8Array`, `TextEncoder`, and `TextDecoder` (bytes.ts).

use crate::interp::callback::apply_collection_callback;
use crate::interp::coerce::{describe_value, to_integer, to_number_plain, to_string_plain, to_uint8};
use crate::interp::cursor::{Cursor, HostIter, IterKind};
use crate::interp::errors::{range_error, syntax_error, type_error};
use crate::interp::limits::{check_array_length, check_string_length};
use crate::interp::native::{NativeOptions, prototype_from, receiver, requires_new};
use crate::interp::object::{define, define_accessor, get, get_data};
use crate::interp::value::*;
use crate::interp::{Ctx, R, boxed};
use crate::jsstr::JsStr;
use std::cell::RefCell;
use std::rc::Rc;

pub fn new_bytes(ctx: &Ctx, proto: Obj, bytes: Vec<u8>) -> Obj {
    let length = bytes.len();
    ctx.new_object(Some(proto), Kind::Bytes(BytesData { buffer: Rc::new(RefCell::new(bytes)), offset: 0, length }))
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}

/// The bytes a Uint8Array, array, or other iterable of numbers describes; each value clamps like a host write.
async fn collect_bytes(ctx: &Ctx, source: &Value, name: &str, mapper: Option<&Value>, this_arg: Value) -> R<Vec<u8>> {
    let apply = match mapper {
        None | Some(Value::Undefined) => None,
        Some(mapper) => Some(apply_collection_callback(ctx, mapper, name)?),
    };
    if let Value::Obj(obj) = source
        && let Some(data) = obj.bytes()
        && apply.is_none()
    {
        return Ok(data.to_vec());
    }
    let Some(cursor) = ctx.iterate(source, None).await? else {
        return Err(type_error(format!(
            "{name} expects a Uint8Array, an array, or an iterable of numbers, received {}.",
            describe_value(source)
        )));
    };
    let mut values = Vec::new();
    loop {
        let step = cursor.next_step(ctx).await?;
        if step.done {
            break;
        }
        values.push(step.value);
        check_array_length(values.len() as f64)?;
    }
    let Some(apply) = apply else { return Ok(values.iter().map(|v| to_uint8(to_number_plain(v))).collect()) };
    let mut mapped = Vec::with_capacity(values.len());
    for (index, value) in values.into_iter().enumerate() {
        mapped.push(apply(vec![value, Value::Num(index as f64)], this_arg.clone()).await?);
    }
    Ok(mapped.iter().map(|v| to_uint8(to_number_plain(v))).collect())
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(BASE64[(n >> 18) as usize & 63] as char);
        out.push(BASE64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { BASE64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { BASE64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Forgiving-base64 decode (atob and `Uint8Array.fromBase64`'s loose handling): ASCII whitespace is skipped.
pub fn base64_decode(text: &[u16]) -> Option<Vec<u8>> {
    let mut data: Vec<u8> = Vec::new();
    for &unit in text {
        if matches!(unit, 0x09 | 0x0A | 0x0C | 0x0D | 0x20) {
            continue;
        }
        if unit > 0x7F {
            return None;
        }
        data.push(unit as u8);
    }
    if data.len() % 4 == 0 {
        if data.ends_with(b"==") {
            data.truncate(data.len() - 2);
        } else if data.ends_with(b"=") {
            data.truncate(data.len() - 1);
        }
    }
    if data.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for byte in data {
        let value = BASE64.iter().position(|&c| c == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

fn hex_decode(text: &[u16]) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let digit = |u: u16| char::from_u32(u as u32).and_then(|c| c.to_digit(16));
    text.chunks(2).map(|pair| Some((digit(pair[0])? * 16 + digit(pair[1])?) as u8)).collect()
}

fn self_bytes(this: &Value, name: &str) -> R<Obj> {
    receiver(this, |obj| obj.bytes().is_some(), &format!("Uint8Array.prototype.{name}"))
}

fn opt_number(value: &Value) -> Option<f64> {
    if value.is_undefined() { None } else { Some(to_integer(value)) }
}

fn relative(value: Option<f64>, len: usize, default: f64) -> usize {
    let n = value.unwrap_or(default);
    if n < 0.0 { (len as f64 + n).max(0.0) as usize } else { n.min(len as f64) as usize }
}

pub fn uint8_array_global(ctx: &Ctx) -> Value {
    let builtins = &ctx.builtins;
    let proto = builtins.uint8_array.clone();
    let array = ctx.constructor(
        &proto,
        NativeOptions {
            name: "Uint8Array".into(),
            length: 3,
            call: requires_new("Uint8Array"),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                let proto = prototype_from(&new_target, &ctx.builtins.uint8_array);
                let source = arg(&args, 0);
                if let Value::Obj(_) = source {
                    let ctx = ctx.clone();
                    return Ok(NOut::Work(boxed(async move {
                        let bytes = collect_bytes(&ctx, &source, "new Uint8Array(...)", None, Value::Undefined).await?;
                        Ok(Value::Obj(new_bytes(&ctx, proto, bytes)))
                    })));
                }
                let length = if source.is_undefined() { 0.0 } else { to_number_plain(&source) };
                if length.fract() != 0.0 || !length.is_finite() || length < 0.0 {
                    return Err(range_error(format!("Invalid typed array length: {}", to_string_plain(&source))));
                }
                check_array_length(length)?;
                Ok(NOut::Val(Value::Obj(new_bytes(ctx, proto, vec![0; length as usize]))))
            })),
            callback: true,
        },
    );
    ctx.method_work(&array, "from", 1, |ctx, _, args, _| {
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let bytes = collect_bytes(&ctx, &arg(&args, 0), "Uint8Array.from", args.get(1), arg(&args, 2)).await?;
            Ok(Value::Obj(new_bytes(&ctx, ctx.builtins.uint8_array.clone(), bytes)))
        })))
    });
    ctx.method(&array, "of", 0, |ctx, _, args, _| {
        let bytes = args.iter().map(|v| to_uint8(to_number_plain(v))).collect();
        Ok(Value::Obj(new_bytes(ctx, ctx.builtins.uint8_array.clone(), bytes)))
    });
    for name in ["fromBase64", "fromHex"] {
        ctx.method(&array, name, 1, move |ctx, _, args, _| {
            let Some(Value::Str(text)) = args.first() else {
                return Err(type_error(format!("Uint8Array.{name} expects a string.")));
            };
            let decoded = if name == "fromHex" { hex_decode(text.units()) } else { base64_decode(text.units()) };
            match decoded {
                Some(bytes) => Ok(Value::Obj(new_bytes(ctx, ctx.builtins.uint8_array.clone(), bytes))),
                None => Err(syntax_error(format!(
                    "Uint8Array.{name}: the string is not valid {}.",
                    if name == "fromHex" { "hex" } else { "base64" }
                ))),
            }
        });
    }
    // Uint8Array cannot contain NaN, so non-number search values never match a byte.
    let search = |value: &Value| match value {
        Value::Num(n) => *n,
        _ => f64::NAN,
    };
    define_accessor(&proto, "length", Some(Rc::new(|_, this| Ok(Value::Num(self_bytes(this, "length")?.bytes().unwrap().length as f64)))), None);
    ctx.method(&proto, "at", 1, |_, this, args, _| {
        let target = self_bytes(&this, "at")?;
        let data = target.bytes().unwrap();
        let n = opt_number(&arg(&args, 0)).unwrap_or(0.0);
        let index = if n < 0.0 { data.length as f64 + n } else { n };
        if index < 0.0 || index >= data.length as f64 {
            return Ok(Value::Undefined);
        }
        Ok(Value::Num(data.get(index as usize).unwrap() as f64))
    });
    ctx.method(&proto, "slice", 2, |ctx, this, args, _| {
        let target = self_bytes(&this, "slice")?;
        let bytes = target.bytes().unwrap().to_vec();
        let start = relative(opt_number(&arg(&args, 0)), bytes.len(), 0.0);
        let end = relative(opt_number(&arg(&args, 1)), bytes.len(), bytes.len() as f64);
        let slice = if start < end { bytes[start..end].to_vec() } else { Vec::new() };
        Ok(Value::Obj(new_bytes(ctx, ctx.builtins.uint8_array.clone(), slice)))
    });
    // A view on the same bytes, as in JS: writes through one are visible through the other.
    ctx.method(&proto, "subarray", 2, |ctx, this, args, _| {
        let target = self_bytes(&this, "subarray")?;
        let data = target.bytes().unwrap();
        let start = relative(opt_number(&arg(&args, 0)), data.length, 0.0);
        let end = relative(opt_number(&arg(&args, 1)), data.length, data.length as f64);
        let length = end.saturating_sub(start);
        let view = BytesData { buffer: data.buffer.clone(), offset: data.offset + start, length };
        Ok(Value::Obj(ctx.new_object(Some(ctx.builtins.uint8_array.clone()), Kind::Bytes(view))))
    });
    ctx.method_work(&proto, "set", 1, |ctx, this, args, _| {
        let target = self_bytes(&this, "set")?;
        let offset = opt_number(&arg(&args, 1)).unwrap_or(0.0);
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let source = collect_bytes(&ctx, &arg(&args, 0), "Uint8Array.set", None, Value::Undefined).await?;
            let data = target.bytes().unwrap();
            if offset.fract() != 0.0 || offset < 0.0 || source.len() as f64 + offset > data.length as f64 {
                return Err(range_error("Uint8Array.set: the source does not fit at that offset."));
            }
            for (index, byte) in source.into_iter().enumerate() {
                data.set(offset as usize + index, byte);
            }
            Ok(Value::Undefined)
        })))
    });
    ctx.method(&proto, "fill", 1, |_, this, args, _| {
        let target = self_bytes(&this, "fill")?;
        let data = target.bytes().unwrap();
        let value = to_uint8(to_number_plain(&arg(&args, 0)));
        let start = relative(opt_number(&arg(&args, 1)), data.length, 0.0);
        let end = relative(opt_number(&arg(&args, 2)), data.length, data.length as f64);
        for index in start..end.max(start) {
            data.set(index, value);
        }
        Ok(Value::Obj(target))
    });
    ctx.method(&proto, "reverse", 0, |_, this, _, _| {
        let target = self_bytes(&this, "reverse")?;
        let data = target.bytes().unwrap();
        let mut bytes = data.to_vec();
        bytes.reverse();
        for (index, byte) in bytes.into_iter().enumerate() {
            data.set(index, byte);
        }
        Ok(Value::Obj(target))
    });
    ctx.method_work(&proto, "sort", 1, |ctx, this, args, _| {
        let target = self_bytes(&this, "sort")?;
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let data = target.bytes().unwrap();
            let mut bytes = data.to_vec();
            // Without a comparator, typed arrays sort numerically rather than by string.
            if arg(&args, 0).is_undefined() {
                bytes.sort_unstable();
            } else {
                let items = bytes.iter().map(|b| Some(Value::Num(*b as f64))).collect();
                let sorted = crate::stdlib::array::sort_array(&ctx, items, arg(&args, 0), "Uint8Array.sort").await?;
                bytes = sorted.iter().map(|v| to_uint8(to_number_plain(v))).collect();
            }
            for (index, byte) in bytes.into_iter().enumerate() {
                data.set(index, byte);
            }
            Ok(Value::Obj(target))
        })))
    });
    ctx.method(&proto, "indexOf", 1, move |_, this, args, _| {
        let target = self_bytes(&this, "indexOf")?;
        let bytes = target.bytes().unwrap().to_vec();
        let needle = search(&arg(&args, 0));
        let start = relative(opt_number(&arg(&args, 1)), bytes.len(), 0.0);
        Ok(Value::Num(bytes.iter().enumerate().skip(start).find(|(_, b)| **b as f64 == needle).map(|(i, _)| i as f64).unwrap_or(-1.0)))
    });
    ctx.method(&proto, "lastIndexOf", 1, move |_, this, args, _| {
        let target = self_bytes(&this, "lastIndexOf")?;
        let bytes = target.bytes().unwrap().to_vec();
        let needle = search(&arg(&args, 0));
        let len = bytes.len() as f64;
        let from = if args.len() < 2 { len - 1.0 } else { opt_number(&args[1]).unwrap_or(0.0) };
        let mut index = if from < 0.0 { len + from } else { from.min(len - 1.0) };
        while index >= 0.0 {
            if bytes[index as usize] as f64 == needle {
                return Ok(Value::Num(index));
            }
            index -= 1.0;
        }
        Ok(Value::Num(-1.0))
    });
    ctx.method(&proto, "includes", 1, move |_, this, args, _| {
        let target = self_bytes(&this, "includes")?;
        let bytes = target.bytes().unwrap().to_vec();
        let needle = search(&arg(&args, 0));
        let start = relative(opt_number(&arg(&args, 1)), bytes.len(), 0.0);
        Ok(Value::Bool(bytes.iter().skip(start).any(|b| *b as f64 == needle)))
    });
    ctx.method(&proto, "join", 1, |_, this, args, _| {
        let target = self_bytes(&this, "join")?;
        let separator = match arg(&args, 0) {
            Value::Undefined => JsStr::from(","),
            other => to_string_plain(&other),
        };
        let parts: Vec<String> = target.bytes().unwrap().to_vec().iter().map(|b| b.to_string()).collect();
        let joined = parts.join(&separator.to_rust_lossy());
        let joined = if separator.is_well_formed() {
            JsStr::from(joined)
        } else {
            let mut out = crate::jsstr::JsStrBuf::new();
            for (index, part) in parts.iter().enumerate() {
                if index > 0 {
                    out.push_js(&separator);
                }
                out.push_str(part);
            }
            out.finish()
        };
        check_string_length(joined.len() as f64)?;
        Ok(Value::Str(joined))
    });
    ctx.method(&proto, "toString", 0, |_, this, _, _| {
        let target = self_bytes(&this, "toString")?;
        let parts: Vec<String> = target.bytes().unwrap().to_vec().iter().map(|b| b.to_string()).collect();
        Ok(Value::str(&parts.join(",")))
    });
    ctx.method(&proto, "toBase64", 0, |_, this, _, _| Ok(Value::str(&base64_encode(&self_bytes(&this, "toBase64")?.bytes().unwrap().to_vec()))));
    ctx.method(&proto, "toHex", 0, |_, this, _, _| {
        let bytes = self_bytes(&this, "toHex")?.bytes().unwrap().to_vec();
        Ok(Value::str(&bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()))
    });
    for (name, kind) in [("keys", IterKind::Keys), ("values", IterKind::Values), ("entries", IterKind::Entries)] {
        ctx.method(&proto, name, 0, move |ctx, this, _, _| {
            let target = self_bytes(&this, name)?;
            let cursor = Cursor::host(HostIter::Bytes { target, index: 0, kind });
            Ok(Value::Obj(ctx.new_object(Some(ctx.builtins.iterator.clone()), Kind::Iterator(cursor))))
        });
    }
    crate::stdlib::array::install_callback_methods(
        ctx,
        &proto,
        "Uint8Array",
        Rc::new(|this, name| self_bytes(this, name)),
        Rc::new(|ctx, values| {
            let bytes = values.iter().map(|v| to_uint8(to_number_plain(&v.clone().unwrap_or_default()))).collect();
            Value::Obj(new_bytes(ctx, ctx.builtins.uint8_array.clone(), bytes))
        }),
    );
    let values = get_data(&proto, &PropKey::from_str("values"));
    define(&proto, PropKey::Sym(Sym::Iterator), values, HIDDEN);
    Value::Obj(array)
}

pub fn text_encoder_global(ctx: &Ctx) -> Value {
    let proto = ctx.builtins.text_encoder.clone();
    define_accessor(&proto, "encoding", Some(Rc::new(|_, _| Ok(Value::str("utf-8")))), None);
    ctx.method(&proto, "encode", 0, |ctx, _, args, _| {
        let text = match args.first() {
            None | Some(Value::Undefined) => JsStr::new(),
            Some(value) => to_string_plain(value),
        };
        Ok(Value::Obj(new_bytes(ctx, ctx.builtins.uint8_array.clone(), text.to_rust_lossy().into_bytes())))
    });
    Value::Obj(ctx.constructor(
        &proto,
        NativeOptions {
            name: "TextEncoder".into(),
            length: 0,
            call: requires_new("TextEncoder"),
            construct: Some(Rc::new(|ctx, _, new_target, _| {
                let proto = prototype_from(&new_target, &ctx.builtins.text_encoder);
                Ok(NOut::Val(Value::Obj(ctx.new_object(Some(proto), Kind::Ordinary))))
            })),
            callback: true,
        },
    ))
}

const UTF8_LABELS: [&str; 6] = ["unicode-1-1-utf-8", "unicode11utf8", "unicode20utf8", "utf-8", "utf8", "x-unicode20utf8"];

fn self_decoder(this: &Value, name: &str) -> R<(bool, bool)> {
    let obj = receiver(this, |obj| matches!(obj.kind(), Kind::TextDecoder { .. }), &format!("TextDecoder.prototype.{name}"))?;
    let Kind::TextDecoder { fatal, ignore_bom } = obj.kind() else { unreachable!() };
    Ok((*fatal, *ignore_bom))
}

pub fn text_decoder_global(ctx: &Ctx) -> Value {
    let proto = ctx.builtins.text_decoder.clone();
    define_accessor(&proto, "encoding", Some(Rc::new(|_, this| {
        self_decoder(this, "encoding")?;
        Ok(Value::str("utf-8"))
    })), None);
    define_accessor(&proto, "fatal", Some(Rc::new(|_, this| Ok(Value::Bool(self_decoder(this, "fatal")?.0)))), None);
    define_accessor(&proto, "ignoreBOM", Some(Rc::new(|_, this| Ok(Value::Bool(self_decoder(this, "ignoreBOM")?.1)))), None);
    ctx.method(&proto, "decode", 0, |_, this, args, _| {
        let (fatal, ignore_bom) = self_decoder(&this, "decode")?;
        let input = arg(&args, 0);
        if input.is_undefined() {
            return Ok(Value::str(""));
        }
        let bytes = match &input {
            Value::Obj(obj) if obj.bytes().is_some() => obj.bytes().unwrap().to_vec(),
            other => {
                return Err(type_error(format!("TextDecoder.decode expects a Uint8Array, received {}.", describe_value(other))));
            }
        };
        let mut slice: &[u8] = &bytes;
        if !ignore_bom && slice.starts_with(&[0xEF, 0xBB, 0xBF]) {
            slice = &slice[3..];
        }
        if fatal && std::str::from_utf8(slice).is_err() {
            return Err(type_error("TextDecoder.decode: the input is not valid utf-8."));
        }
        Ok(Value::str(&String::from_utf8_lossy(slice)))
    });
    Value::Obj(ctx.constructor(
        &proto,
        NativeOptions {
            name: "TextDecoder".into(),
            length: 0,
            call: requires_new("TextDecoder"),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                let label = match args.first() {
                    None | Some(Value::Undefined) => "utf-8".to_string(),
                    Some(value) => to_string_plain(value).to_rust_lossy().trim().to_lowercase(),
                };
                if !UTF8_LABELS.contains(&label.as_str()) {
                    return Err(range_error(format!("The \"{label}\" encoding is not supported; only UTF-8 is.")));
                }
                let flag = |name: &str| -> R<bool> {
                    match args.get(1) {
                        Some(Value::Obj(options)) => Ok(get(ctx, options, &PropKey::from_str(name))?.truthy()),
                        _ => Ok(false),
                    }
                };
                let (fatal, ignore_bom) = (flag("fatal")?, flag("ignoreBOM")?);
                let proto = prototype_from(&new_target, &ctx.builtins.text_decoder);
                Ok(NOut::Val(Value::Obj(ctx.new_object(Some(proto), Kind::TextDecoder { fatal, ignore_bom }))))
            })),
            callback: true,
        },
    ))
}
