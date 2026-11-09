//! `RegExp` (regexp.ts) over `jsregex`.

use crate::interp::coerce::to_string_plain;
use crate::interp::errors::{syntax_error, type_error};
use crate::interp::native::{NativeOptions, prototype_from, receiver};
use crate::interp::object::{define, define_accessor};
use crate::interp::value::*;
use crate::interp::{Ctx, R};
use crate::jsregex::{JsRegExp, Match, RegExpError};
use crate::jsstr::JsStr;
use std::rc::Rc;

const ESCAPE_HINT: &str = "To match special characters like ( ) [ ] { } + * ? . literally, escape them with a backslash (e.g. \"\\\\(\") or test for them with String.includes instead.";

fn quote(text: &JsStr) -> String {
    crate::stdlib::json::_quote(text).to_rust_lossy()
}

fn reason(error: &RegExpError) -> String {
    let text = match error {
        RegExpError::Flags(text) | RegExpError::Pattern(text) => text,
    };
    let lower = text.to_ascii_lowercase();
    match lower.find("invalid regular expression:") {
        Some(0) => text["invalid regular expression:".len()..].trim_start().to_string(),
        _ => text.clone(),
    }
}

/// A host regex for String.match/matchAll/search: a RegExp as is, anything else as its pattern text.
pub fn to_host_regex(arg: &Value, method: &str, extra_flags: &str) -> R<Rc<JsRegExp>> {
    if let Value::Obj(obj) = arg
        && let Kind::RegExp(regex) = obj.kind()
    {
        return Ok(regex.clone());
    }
    let source = if arg.is_undefined() { JsStr::new() } else { to_string_plain(arg) };
    let flags: Vec<u16> = extra_flags.encode_utf16().collect();
    JsRegExp::new(source.units(), &flags).map(Rc::new).map_err(|error| {
        syntax_error(format!(
            "String.{method} received the string {}, which is not a valid regular expression pattern ({}). {ESCAPE_HINT}",
            quote(&source),
            reason(&error)
        ))
    })
}

pub fn match_to_value(ctx: &Ctx, found: &Match, input: &JsStr, indices: bool) -> Obj {
    let groups: Vec<Value> = found
        .captures
        .iter()
        .map(|capture| match capture {
            Some((s, e)) => Value::Str(input.slice(*s, *e)),
            None => Value::Undefined,
        })
        .collect();
    let result = ctx.new_array(groups);
    let index = found.captures[0].map(|(s, _)| s).unwrap_or(0);
    define(&result, PropKey::from_str("index"), Value::Num(index as f64), DATA);
    define(&result, PropKey::from_str("input"), Value::Str(input.clone()), DATA);
    let named = found.groups.as_ref().map(|groups| {
        let record = ctx.new_plain();
        for (name, range) in groups {
            let item = match range {
                Some((s, e)) => Value::Str(input.slice(*s, *e)),
                None => Value::Undefined,
            };
            define(&record, PropKey::from_js(JsStr::from_slice(name)), item, DATA);
        }
        record
    });
    if let Some(named) = named {
        define(&result, PropKey::from_str("groups"), Value::Obj(named), DATA);
    }
    if indices {
        let range = |pair: &Option<(usize, usize)>| match pair {
            Some((s, e)) => Value::Obj(ctx.new_array(vec![Value::Num(*s as f64), Value::Num(*e as f64)])),
            None => Value::Undefined,
        };
        let pairs = ctx.new_array(found.captures.iter().map(range).collect());
        let groups = found.groups.as_ref().map(|groups| {
            let record = ctx.new_plain();
            for (name, pair) in groups {
                define(&record, PropKey::from_js(JsStr::from_slice(name)), range(pair), DATA);
            }
            Value::Obj(record)
        });
        define(&pairs, PropKey::from_str("groups"), groups.unwrap_or(Value::Undefined), DATA);
        define(&result, PropKey::from_str("indices"), Value::Obj(pairs), DATA);
    }
    result
}

pub fn construct_regexp(ctx: &Ctx, args: &[Value], proto: Option<Obj>) -> R<Obj> {
    let first = args.first().cloned().unwrap_or_default();
    let existing = match &first {
        Value::Obj(obj) => match obj.kind() {
            Kind::RegExp(regex) => Some(regex.clone()),
            _ => None,
        },
        _ => None,
    };
    let pattern = match (&existing, &first) {
        (Some(regex), _) => JsStr::from_units(regex.source()),
        (None, Value::Undefined) => JsStr::new(),
        (None, other) => to_string_plain(other),
    };
    let flags_arg = args.get(1).cloned().unwrap_or_default();
    let flags = match &flags_arg {
        Value::Undefined => match &existing {
            Some(regex) => JsStr::from(regex.flags()),
            None => JsStr::new(),
        },
        Value::Str(text) => text.clone(),
        other => {
            let kind = if matches!(other, Value::Null) { "null" } else { crate::interp::coerce::typeof_value(other) };
            return Err(syntax_error(format!(
                "RegExp flags must be a string of flag characters (e.g. \"g\", \"gi\"), not {kind}."
            )));
        }
    };
    let regex = JsRegExp::new(pattern.units(), flags.units()).map_err(|error| {
        let why = reason(&error);
        if why.to_ascii_lowercase().contains("flag") {
            syntax_error(format!(
                "new RegExp(...) received invalid flags {} ({why}). Valid flags are d, g, i, m, s, u, v, and y.",
                quote(&flags)
            ))
        } else {
            syntax_error(format!(
                "new RegExp(...) received {}, which is not a valid regular expression pattern ({why}). {ESCAPE_HINT}",
                quote(&pattern)
            ))
        }
    })?;
    let proto = proto.unwrap_or_else(|| ctx.builtins.regexp.clone());
    Ok(ctx.new_object(Some(proto), Kind::RegExp(Rc::new(regex))))
}

fn self_regexp(this: &Value, name: &str) -> R<Rc<JsRegExp>> {
    let obj = receiver(this, |obj| matches!(obj.kind(), Kind::RegExp(_)), &format!("RegExp.prototype.{name}"))?;
    let Kind::RegExp(regex) = obj.kind() else { unreachable!() };
    Ok(regex.clone())
}

pub fn regexp_global(ctx: &Ctx) -> Value {
    let builtins = &ctx.builtins;
    let proto = builtins.regexp.clone();
    let regexp = ctx.constructor(
        &proto,
        NativeOptions {
            name: "RegExp".into(),
            length: 2,
            call: Rc::new(|ctx, _, args, _| construct_regexp(ctx, &args, None).map(|o| NOut::Val(Value::Obj(o)))),
            construct: Some(Rc::new(|ctx, args, new_target, _| {
                let proto = prototype_from(&new_target, &ctx.builtins.regexp);
                construct_regexp(ctx, &args, Some(proto)).map(|o| NOut::Val(Value::Obj(o)))
            })),
            callback: true,
        },
    );
    ctx.method(&regexp, "escape", 1, |_, _, args, _| match args.first() {
        Some(Value::Str(text)) => Ok(Value::Str(JsStr::from_units(crate::jsregex::escape(text.units())))),
        _ => Err(type_error("RegExp.escape expects a string.")),
    });
    define_accessor(
        &proto,
        "source",
        Some(Rc::new(|_, this| Ok(Value::Str(JsStr::from_units(self_regexp(this, "source")?.source()))))),
        None,
    );
    define_accessor(&proto, "flags", Some(Rc::new(|_, this| Ok(Value::str(&self_regexp(this, "flags")?.flags())))), None);
    // The host regex holds the only lastIndex, so exec/test and the String methods share one counter.
    define_accessor(
        &proto,
        "lastIndex",
        Some(Rc::new(|_, this| Ok(Value::Num(self_regexp(this, "lastIndex")?.last_index())))),
        Some(Rc::new(|_, this, value| {
            let regex = self_regexp(this, "lastIndex")?;
            regex.set_last_index(crate::interp::coerce::to_number_plain(&value));
            Ok(())
        })),
    );
    let flags: [(&str, fn(&JsRegExp) -> bool); 8] = [
        ("hasIndices", JsRegExp::has_indices),
        ("global", JsRegExp::global),
        ("ignoreCase", JsRegExp::ignore_case),
        ("multiline", JsRegExp::multiline),
        ("sticky", JsRegExp::sticky),
        ("unicode", JsRegExp::unicode),
        ("unicodeSets", JsRegExp::unicode_sets),
        ("dotAll", JsRegExp::dot_all),
    ];
    for (name, read) in flags {
        define_accessor(&proto, name, Some(Rc::new(move |_, this| Ok(Value::Bool(read(self_regexp(this, name)?.as_ref()))))), None);
    }
    for name in ["exec", "test"] {
        ctx.method(&proto, name, 1, move |ctx, this, args, _| {
            let regex = self_regexp(&this, name)?;
            let input = to_string_plain(&args.first().cloned().unwrap_or_default());
            let found = regex.exec(input.units());
            if name == "test" {
                return Ok(Value::Bool(found.is_some()));
            }
            Ok(match found {
                None => Value::Null,
                Some(found) => Value::Obj(match_to_value(ctx, &found, &input, regex.has_indices())),
            })
        });
    }
    ctx.method(&proto, "toString", 0, |_, this, _, _| {
        let obj = receiver(&this, |obj| matches!(obj.kind(), Kind::RegExp(_)), "RegExp.prototype.toString")?;
        Ok(Value::Str(crate::interp::coerce::obj_to_string(&obj)))
    });
    Value::Obj(regexp)
}
