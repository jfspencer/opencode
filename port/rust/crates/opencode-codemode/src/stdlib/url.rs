//! `URL`, `URLSearchParams` (WHATWG, over the `url` crate), and the URI helpers (url.ts).

use crate::interp::callback::{apply_collection_callback, preserve_consumer_error};
use crate::interp::coerce::{is_runtime_reference, to_string_plain};
use crate::interp::cursor::{Cursor, HostIter, IterKind};
use crate::interp::errors::{type_error, uri_error};
use crate::interp::native::{NativeOptions, prototype_from, receiver, requires_new};
use crate::interp::object::{define, define_accessor, entries, get_data};
use crate::interp::value::*;
use crate::interp::{Ctx, R, boxed};
use crate::jsstr::JsStr;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// A USVString: lone surrogates become U+FFFD.
fn usv(text: &JsStr) -> String {
    text.to_rust_lossy()
}

pub struct UrlState {
    pub url: url::Url,
    pub params: Rc<RefCell<SearchParams>>,
}

impl UrlState {
    pub fn href(&self) -> JsStr {
        JsStr::from(self.url.as_str())
    }
    /// After the query changed, the linked params read it again.
    fn refresh_params(&self) {
        let list = parse_query(self.url.query().unwrap_or(""));
        self.params.borrow_mut().list = list;
    }
}

/// A URLSearchParams list, linked to its URL when it is a URL's `searchParams`.
#[derive(Default)]
pub struct SearchParams {
    pub list: Vec<(JsStr, JsStr)>,
    pub url: Option<Weak<RefCell<UrlState>>>,
}

fn parse_query(query: &str) -> Vec<(JsStr, JsStr)> {
    let query = query.strip_prefix('?').unwrap_or(query);
    url::form_urlencoded::parse(query.as_bytes()).map(|(k, v)| (JsStr::from(k.as_ref()), JsStr::from(v.as_ref()))).collect()
}

impl SearchParams {
    pub fn serialize(&self) -> JsStr {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in &self.list {
            serializer.append_pair(&usv(name), &usv(value));
        }
        JsStr::from(serializer.finish())
    }
    pub fn pair(&self, index: usize) -> Option<(JsStr, JsStr)> {
        self.list.get(index).cloned()
    }
    /// The URL update steps after a change.
    fn update(&self) {
        let Some(url) = self.url.as_ref().and_then(|weak| weak.upgrade()) else { return };
        let serialized = self.serialize();
        let mut state = url.borrow_mut();
        if serialized.is_empty() {
            state.url.set_query(None);
        } else {
            state.url.set_query(Some(&serialized.to_rust_lossy()));
        }
    }
}

const URI_RESERVED: &str = ";/?:@&=+$,";
const URI_UNESCAPED: &str = "-_.!~*'()";

fn encode(text: &JsStr, keep: &str) -> Result<JsStr, ()> {
    let units = text.units();
    let mut out = String::new();
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if unit < 0x80 {
            let ch = unit as u8 as char;
            if ch.is_ascii_alphanumeric() || URI_UNESCAPED.contains(ch) || keep.contains(ch) {
                out.push(ch);
                index += 1;
                continue;
            }
        }
        let point = if (0xD800..0xDC00).contains(&unit) {
            match units.get(index + 1) {
                Some(&next) if (0xDC00..0xE000).contains(&next) => {
                    index += 1;
                    0x10000 + (((unit as u32) - 0xD800) << 10) + ((next as u32) - 0xDC00)
                }
                _ => return Err(()),
            }
        } else if (0xDC00..0xE000).contains(&unit) {
            return Err(());
        } else {
            unit as u32
        };
        let mut buffer = [0u8; 4];
        for byte in char::from_u32(point).unwrap().encode_utf8(&mut buffer).bytes() {
            out.push_str(&format!("%{byte:02X}"));
        }
        index += 1;
    }
    Ok(JsStr::from(out))
}

fn decode(text: &JsStr, reserved: &str) -> Result<JsStr, ()> {
    let units = text.units();
    let mut out: Vec<u16> = Vec::with_capacity(units.len());
    let hex = |unit: u16| char::from_u32(unit as u32).and_then(|c| c.to_digit(16));
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if unit != b'%' as u16 {
            out.push(unit);
            index += 1;
            continue;
        }
        let byte_at = |at: usize| -> Result<u8, ()> {
            if at + 2 >= units.len() {
                return Err(());
            }
            if units.get(at) != Some(&(b'%' as u16)) {
                return Err(());
            }
            let high = units.get(at + 1).and_then(|u| hex(*u)).ok_or(())?;
            let low = units.get(at + 2).and_then(|u| hex(*u)).ok_or(())?;
            Ok((high * 16 + low) as u8)
        };
        let first = byte_at(index)?;
        if first < 0x80 {
            let ch = first as char;
            if reserved.contains(ch) {
                out.extend_from_slice(&units[index..index + 3]);
            } else {
                out.push(first as u16);
            }
            index += 3;
            continue;
        }
        let count = if first & 0xE0 == 0xC0 {
            2
        } else if first & 0xF0 == 0xE0 {
            3
        } else if first & 0xF8 == 0xF0 {
            4
        } else {
            return Err(());
        };
        let mut bytes = vec![first];
        for k in 1..count {
            let byte = byte_at(index + 3 * k)?;
            if byte & 0xC0 != 0x80 {
                return Err(());
            }
            bytes.push(byte);
        }
        let decoded = std::str::from_utf8(&bytes).map_err(|_| ())?;
        for ch in decoded.chars() {
            let mut buffer = [0u16; 2];
            out.extend_from_slice(ch.encode_utf16(&mut buffer));
        }
        index += 3 * count;
    }
    Ok(JsStr::from_units(out))
}

pub fn uri_global(ctx: &Ctx, name: &'static str) -> Value {
    Value::Obj(ctx.native_fn(name, 1, move |_, _, args, _| {
        let value = to_string_plain(&args.first().cloned().unwrap_or_default());
        let result = match name {
            "encodeURI" => encode(&value, &format!("{URI_RESERVED}#")),
            "encodeURIComponent" => encode(&value, ""),
            "decodeURI" => decode(&value, &format!("{URI_RESERVED}#")),
            _ => decode(&value, ""),
        };
        result.map(Value::Str).map_err(|_| uri_error(format!("{name} received malformed URI data: URI error")))
    }))
}

fn url_argument(value: &Value) -> String {
    if let Value::Obj(obj) = value
        && let Kind::URL(state, _) = obj.kind()
    {
        return state.borrow().url.as_str().to_string();
    }
    usv(&to_string_plain(value))
}

fn parse_url(input: &str, base: Option<&str>) -> Option<url::Url> {
    let url = match base {
        None => url::Url::parse(input).ok()?,
        Some(base) => url::Url::parse(base).ok()?.join(input).ok()?,
    };
    (url.cannot_be_a_base() || url.is_special() || opaque_authority(input)).then_some(url)
}

/// Two WHATWG rules the `url` crate does not apply to a non-special URL's authority, which ends at `/`, `?` or `#`
/// (a backslash is part of it): credentials need a host (`foo://@`, `sc://:@/`), and a port is digits only
/// (`foo://-:\x`). The same check as the server's form formats (`URL.canParse`).
fn opaque_authority(text: &str) -> bool {
    let input: String =
        text.trim_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let Some(authority) = input.split_once(':').and_then(|(_, rest)| rest.strip_prefix("//")) else { return true };
    let authority = &authority[..authority.find(['/', '?', '#']).unwrap_or(authority.len())];
    let (credentials, host_port) = match authority.rfind('@') {
        Some(at) => (true, &authority[at + 1..]),
        None => (false, authority),
    };
    if credentials && host_port.is_empty() {
        return false;
    }
    let port = match host_port.rfind(']') {
        Some(end) => host_port[end + 1..].strip_prefix(':'),
        None => host_port.rfind(':').map(|colon| &host_port[colon + 1..]),
    };
    port.is_none_or(|port| port.chars().all(|c| c.is_ascii_digit()))
}

pub fn new_url(ctx: &Ctx, proto: Obj, parsed: url::Url) -> Obj {
    let params = Rc::new(RefCell::new(SearchParams { list: parse_query(parsed.query().unwrap_or("")), url: None }));
    let state = Rc::new(RefCell::new(UrlState { url: parsed, params: params.clone() }));
    params.borrow_mut().url = Some(Rc::downgrade(&state));
    let search_params = ctx.new_object(Some(ctx.builtins.url_search_params.clone()), Kind::URLSearchParams(params));
    ctx.new_object(Some(proto), Kind::URL(state, search_params))
}

const PROPERTIES: [&str; 11] =
    ["href", "origin", "protocol", "username", "password", "host", "hostname", "port", "pathname", "search", "hash"];

fn self_url(this: &Value, name: &str) -> R<Obj> {
    receiver(this, |obj| matches!(obj.kind(), Kind::URL(..)), &format!("URL.prototype.{name}"))
}

fn read(url: &url::Url, name: &str) -> String {
    use url::quirks;
    match name {
        "href" => quirks::href(url).to_string(),
        "origin" => quirks::origin(url),
        "protocol" => quirks::protocol(url).to_string(),
        "username" => quirks::username(url).to_string(),
        "password" => quirks::password(url).to_string(),
        "host" => quirks::host(url).to_string(),
        "hostname" => quirks::hostname(url).to_string(),
        "port" => quirks::port(url).to_string(),
        "pathname" => quirks::pathname(url).to_string(),
        "search" => quirks::search(url).to_string(),
        _ => quirks::hash(url).to_string(),
    }
}

pub fn url_global(ctx: &Ctx) -> Value {
    let builtins = &ctx.builtins;
    let proto = builtins.url.clone();
    let url = ctx.constructor(
        &proto,
        NativeOptions {
            name: "URL".into(),
            length: 1,
            call: requires_new("URL"),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                if args.is_empty() {
                    return Err(type_error("new URL(...) requires a URL string and an optional base URL."));
                }
                let input = url_argument(&args[0]);
                let base = match args.get(1) {
                    None | Some(Value::Undefined) => None,
                    Some(value) => Some(url_argument(value)),
                };
                let into = prototype_from(&new_target, &ctx.builtins.url);
                match parse_url(&input, base.as_deref()) {
                    Some(parsed) => Ok(NOut::Val(Value::Obj(new_url(ctx, into, parsed)))),
                    None => Err(type_error(format!(
                        "new URL(...) received an invalid URL{}.",
                        if base.is_none() { "" } else { " or base URL" }
                    ))),
                }
            })),
            callback: true,
        },
    );
    for name in ["canParse", "parse"] {
        ctx.method(&url, name, 1, move |ctx, _, args, _| {
            if args.is_empty() {
                return Err(type_error(format!("URL.{name} requires a URL argument.")));
            }
            let input = url_argument(&args[0]);
            let base = match args.get(1) {
                None | Some(Value::Undefined) => None,
                Some(value) => Some(url_argument(value)),
            };
            let parsed = parse_url(&input, base.as_deref());
            Ok(match (name, parsed) {
                ("canParse", parsed) => Value::Bool(parsed.is_some()),
                (_, Some(parsed)) => Value::Obj(new_url(ctx, ctx.builtins.url.clone(), parsed)),
                (_, None) => Value::Null,
            })
        });
    }
    for name in PROPERTIES {
        let getter: Getter = Rc::new(move |_, this| {
            let obj = self_url(this, name)?;
            let Kind::URL(state, _) = obj.kind() else { unreachable!() };
            Ok(Value::Str(JsStr::from(read(&state.borrow().url, name))))
        });
        let setter: Option<Setter> = if name == "origin" {
            None
        } else {
            Some(Rc::new(move |_, this, value| {
                let obj = self_url(this, name)?;
                let Kind::URL(state, _) = obj.kind() else { unreachable!() };
                let text = usv(&to_string_plain(&value));
                let mut current = state.borrow_mut();
                let url = &mut current.url;
                use url::quirks;
                match name {
                    "href" => {
                        if quirks::set_href(url, &text).is_err() {
                            return Err(type_error(format!("URL.{name} received an invalid value.")));
                        }
                    }
                    "protocol" => drop(quirks::set_protocol(url, &text)),
                    "username" => drop(quirks::set_username(url, &text)),
                    "password" => drop(quirks::set_password(url, &text)),
                    "host" => drop(quirks::set_host(url, &text)),
                    "hostname" => drop(quirks::set_hostname(url, &text)),
                    "port" => drop(quirks::set_port(url, &text)),
                    "pathname" => quirks::set_pathname(url, &text),
                    "search" => quirks::set_search(url, &text),
                    _ => quirks::set_hash(url, &text),
                }
                if name == "href" || name == "search" {
                    current.refresh_params();
                }
                Ok(())
            }))
        };
        define_accessor(&proto, name, Some(getter), setter);
    }
    define_accessor(
        &proto,
        "searchParams",
        Some(Rc::new(|_, this| {
            let obj = self_url(this, "searchParams")?;
            let Kind::URL(_, params) = obj.kind() else { unreachable!() };
            Ok(Value::Obj(params.clone()))
        })),
        None,
    );
    for name in ["toString", "toJSON"] {
        ctx.method(&proto, name, 0, move |_, this, _, _| {
            let obj = self_url(&this, name)?;
            let Kind::URL(state, _) = obj.kind() else { unreachable!() };
            Ok(Value::Str(state.borrow().href()))
        });
    }
    Value::Obj(url)
}

/// Reads a synchronous iterable of `[name, value]` pairs as strings; None when `init` is not iterable.
pub async fn read_pairs(ctx: &Ctx, init: &Value, label: &str) -> R<Option<Vec<(JsStr, JsStr)>>> {
    let Some(cursor) = ctx.iterate(init, None).await? else { return Ok(None) };
    let mut pairs: Vec<Vec<JsStr>> = Vec::new();
    loop {
        let step = cursor.next_step(ctx).await?;
        if step.done {
            if pairs.iter().any(|entry| entry.len() != 2) {
                return Err(type_error(format!("{label} expects iterable [name, value] pairs.")));
            }
            return Ok(Some(pairs.into_iter().map(|mut p| (p.remove(0), p.remove(0))).collect()));
        }
        let pair = read_pair(ctx, &step.value, label).await;
        let close = {
            let (ctx, cursor) = (ctx.clone(), cursor.clone());
            async move { cursor.close_now(&ctx).await }
        };
        pairs.push(preserve_consumer_error(close, pair).await?);
    }
}

async fn read_pair(ctx: &Ctx, value: &Value, label: &str) -> R<Vec<JsStr>> {
    let Some(cursor) = ctx.iterate(value, None).await? else {
        return Err(type_error(format!("{label} expects iterable [name, value] pairs.")));
    };
    let mut items = Vec::new();
    loop {
        let step = cursor.next_step(ctx).await?;
        if step.done {
            return Ok(items);
        }
        items.push(to_string_plain(&step.value));
    }
}

fn new_params(ctx: &Ctx, proto: Obj, list: Vec<(JsStr, JsStr)>) -> Value {
    Value::Obj(ctx.new_object(Some(proto), Kind::URLSearchParams(Rc::new(RefCell::new(SearchParams { list, url: None })))))
}

fn usv_list(list: Vec<(JsStr, JsStr)>) -> Vec<(JsStr, JsStr)> {
    list.into_iter().map(|(k, v)| (JsStr::from(usv(&k)), JsStr::from(usv(&v)))).collect()
}

async fn construct_params(ctx: Ctx, init: Value, proto: Obj) -> R<Value> {
    match &init {
        Value::Undefined => return Ok(new_params(&ctx, proto, Vec::new())),
        Value::Obj(obj) if matches!(obj.kind(), Kind::URLSearchParams(_)) => {
            let Kind::URLSearchParams(params) = obj.kind() else { unreachable!() };
            let list = params.borrow().list.clone();
            return Ok(new_params(&ctx, proto, list));
        }
        Value::Str(text) => return Ok(new_params(&ctx, proto, parse_query(&usv(text)))),
        Value::Null | Value::Num(_) | Value::Bool(_) => {
            return Ok(new_params(&ctx, proto, parse_query(&usv(&to_string_plain(&init)))));
        }
        _ => {}
    }
    if let Some(pairs) = read_pairs(&ctx, &init, "new URLSearchParams(...)").await? {
        return Ok(new_params(&ctx, proto, usv_list(pairs)));
    }
    if is_runtime_reference(&init) {
        return Err(type_error("new URLSearchParams(...) expects a query string, data object, or synchronous iterable pairs."));
    }
    let Value::Obj(obj) = &init else {
        return Err(type_error(
            "new URLSearchParams(...) expects a query string, data object, iterable pairs, or URLSearchParams.",
        ));
    };
    let mut record = crate::json::JsonObj::new();
    for (key, value) in entries(&ctx, obj)? {
        record.insert(key.to_js(), crate::json::Json::Str(to_string_plain(&value)));
    }
    let list = record.iter().map(|(k, v)| (k.clone(), v.as_str().cloned().unwrap_or_default())).collect();
    Ok(new_params(&ctx, proto, usv_list(list)))
}

fn self_params(this: &Value, name: &str) -> R<Rc<RefCell<SearchParams>>> {
    let obj = receiver(this, |obj| matches!(obj.kind(), Kind::URLSearchParams(_)), &format!("URLSearchParams.prototype.{name}"))?;
    let Kind::URLSearchParams(params) = obj.kind() else { unreachable!() };
    Ok(params.clone())
}

fn require_args(name: &str, args: &[Value], count: usize) -> R<()> {
    if args.len() < count {
        return Err(type_error(format!("URLSearchParams.{name} requires {count} argument{}.", if count == 1 { "" } else { "s" })));
    }
    Ok(())
}

fn text_arg(args: &[Value], index: usize) -> JsStr {
    JsStr::from(usv(&to_string_plain(&args.get(index).cloned().unwrap_or_default())))
}

pub fn url_search_params_global(ctx: &Ctx) -> Value {
    let builtins = &ctx.builtins;
    let proto = builtins.url_search_params.clone();
    let params = ctx.constructor(
        &proto,
        NativeOptions {
            name: "URLSearchParams".into(),
            length: 0,
            call: requires_new("URLSearchParams"),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                let init = args.into_iter().next().unwrap_or_default();
                let proto = prototype_from(&new_target, &ctx.builtins.url_search_params);
                Ok(NOut::Work(boxed(construct_params(ctx.clone(), init, proto))))
            })),
            callback: true,
        },
    );
    define_accessor(&proto, "size", Some(Rc::new(|_, this| Ok(Value::Num(self_params(this, "size")?.borrow().list.len() as f64)))), None);
    ctx.method(&proto, "append", 2, |_, this, args, _| {
        require_args("append", &args, 2)?;
        let params = self_params(&this, "append")?;
        params.borrow_mut().list.push((text_arg(&args, 0), text_arg(&args, 1)));
        params.borrow().update();
        Ok(Value::Undefined)
    });
    ctx.method(&proto, "delete", 1, |_, this, args, _| {
        require_args("delete", &args, 1)?;
        let params = self_params(&this, "delete")?;
        let name = text_arg(&args, 0);
        let value = if matches!(args.get(1), None | Some(Value::Undefined)) { None } else { Some(text_arg(&args, 1)) };
        params.borrow_mut().list.retain(|(n, v)| !(n == &name && value.as_ref().is_none_or(|value| v == value)));
        params.borrow().update();
        Ok(Value::Undefined)
    });
    ctx.method(&proto, "get", 1, |_, this, args, _| {
        require_args("get", &args, 1)?;
        let params = self_params(&this, "get")?;
        let name = text_arg(&args, 0);
        let found = params.borrow().list.iter().find(|(n, _)| n == &name).map(|(_, v)| v.clone());
        Ok(found.map(Value::Str).unwrap_or(Value::Null))
    });
    ctx.method(&proto, "getAll", 1, |ctx, this, args, _| {
        require_args("getAll", &args, 1)?;
        let params = self_params(&this, "getAll")?;
        let name = text_arg(&args, 0);
        let all: Vec<Value> = params.borrow().list.iter().filter(|(n, _)| n == &name).map(|(_, v)| Value::Str(v.clone())).collect();
        Ok(Value::Obj(ctx.new_array(all)))
    });
    ctx.method(&proto, "has", 1, |_, this, args, _| {
        require_args("has", &args, 1)?;
        let params = self_params(&this, "has")?;
        let name = text_arg(&args, 0);
        let value = if matches!(args.get(1), None | Some(Value::Undefined)) { None } else { Some(text_arg(&args, 1)) };
        let found = params.borrow().list.iter().any(|(n, v)| n == &name && value.as_ref().is_none_or(|value| v == value));
        Ok(Value::Bool(found))
    });
    ctx.method(&proto, "set", 2, |_, this, args, _| {
        require_args("set", &args, 2)?;
        let params = self_params(&this, "set")?;
        let (name, value) = (text_arg(&args, 0), text_arg(&args, 1));
        {
            let mut state = params.borrow_mut();
            match state.list.iter().position(|(n, _)| n == &name) {
                Some(first) => {
                    state.list[first].1 = value;
                    let mut index = 0;
                    state.list.retain(|(n, _)| {
                        let keep = index <= first || n != &name;
                        index += 1;
                        keep
                    });
                }
                None => state.list.push((name, value)),
            }
        }
        params.borrow().update();
        Ok(Value::Undefined)
    });
    ctx.method(&proto, "sort", 0, |_, this, _, _| {
        let params = self_params(&this, "sort")?;
        params.borrow_mut().list.sort_by(|a, b| a.0.cmp(&b.0));
        params.borrow().update();
        Ok(Value::Undefined)
    });
    for (name, kind) in [("keys", IterKind::Keys), ("values", IterKind::Values), ("entries", IterKind::Entries)] {
        ctx.method(&proto, name, 0, move |ctx, this, _, _| {
            self_params(&this, name)?;
            let Value::Obj(target) = this else { unreachable!() };
            let cursor = Cursor::host(HostIter::SearchParams { target, index: 0, kind });
            Ok(Value::Obj(ctx.new_object(Some(ctx.builtins.iterator.clone()), Kind::Iterator(cursor))))
        });
    }
    ctx.method(&proto, "toString", 0, |_, this, _, _| Ok(Value::Str(self_params(&this, "toString")?.borrow().serialize())));
    ctx.method_work(&proto, "forEach", 1, |ctx, this, args, _| {
        require_args("forEach", &args, 1)?;
        let params = self_params(&this, "forEach")?;
        let apply = apply_collection_callback(ctx, &args[0], "URLSearchParams.forEach")?;
        let this_arg = args.get(1).cloned().unwrap_or_default();
        let list = params.borrow().list.clone();
        Ok(NOut::Work(boxed(async move {
            for (key, value) in list {
                apply(vec![Value::Str(value), Value::Str(key), this.clone()], this_arg.clone()).await?;
            }
            Ok(Value::Undefined)
        })))
    });
    let entries = get_data(&proto, &PropKey::from_str("entries"));
    define(&proto, PropKey::Sym(Sym::Iterator), entries, HIDDEN);
    Value::Obj(params)
}
