//! The standard library (packages/codemode/src/stdlib) and the global bindings (interpreter/globals.ts).

pub mod array;
pub mod bytes;
pub mod collections;
pub mod console;
pub mod date;
pub mod errors;
pub mod function;
pub mod headers;
pub mod iterator;
pub mod json;
pub mod math;
pub mod number;
pub mod object;
pub mod regexp;
pub mod string;
pub mod url;
pub mod value;
pub mod web;

use crate::interp::Ctx;
use crate::interp::errors::ERROR_TYPES;
use crate::interp::native::NativeOptions;
use crate::interp::value::*;
use crate::jsstr::JsStr;
use std::rc::Rc;

/// Names bound in every program before extensions apply.
pub const GLOBAL_NAMES: [&str; 50] = [
    "tools",
    "search",
    "require",
    "undefined",
    "NaN",
    "Infinity",
    "Object",
    "Function",
    "Array",
    "Math",
    "JSON",
    "console",
    "Promise",
    "Symbol",
    "Iterator",
    "Number",
    "String",
    "Boolean",
    "parseInt",
    "parseFloat",
    "isFinite",
    "isNaN",
    "Date",
    "RegExp",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "URL",
    "URLSearchParams",
    "Headers",
    "Uint8Array",
    "TextEncoder",
    "TextDecoder",
    "encodeURI",
    "encodeURIComponent",
    "decodeURI",
    "decodeURIComponent",
    "atob",
    "btoa",
    "crypto",
    "structuredClone",
    "Error",
    "TypeError",
    "RangeError",
    "SyntaxError",
    "ReferenceError",
    "EvalError",
    "URIError",
    "AggregateError",
];

/// The immutable global bindings of every program, in declaration order.
pub fn globals(ctx: &Ctx) -> Vec<(JsStr, Value)> {
    crate::interp::generators::install(ctx);
    let mut out: Vec<(JsStr, Value)> = Vec::new();
    for name in GLOBAL_NAMES {
        let value = match name {
            "tools" => Value::Tool(ToolRef::root()),
            "search" => Value::Obj(ctx.new_native(NativeOptions {
                name: "search".into(),
                length: 0,
                call: Rc::new(|ctx, _, args, _| {
                    let ctx = ctx.clone();
                    Ok(NOut::Work(crate::interp::boxed(async move { ctx.tools.search_value(&ctx, args).await })))
                }),
                construct: None,
                callback: false,
            })),
            "require" => Value::Obj(ctx.native_fn("require", 1, |ctx, _, args, site| {
                let Some(Value::Str(specifier)) = args.first() else {
                    return Err(crate::interp::errors::type_error("require expects a module name string."));
                };
                ctx.module(specifier, site)
            })),
            "undefined" => Value::Undefined,
            "NaN" => Value::Num(f64::NAN),
            "Infinity" => Value::Num(f64::INFINITY),
            "Object" => object::object_global(ctx),
            "Function" => function::function_global(ctx),
            "Array" => array::array_global(ctx),
            "Math" => math::math_global(ctx),
            "JSON" => json::json_global(ctx),
            "console" => console::console_global(ctx),
            "Promise" => crate::interp::promises::promise_global(ctx),
            "Symbol" => function::symbol_global(ctx),
            "Iterator" => iterator::iterator_global(ctx),
            "Number" => number::number_global(ctx),
            "String" => string::string_global(ctx),
            "Boolean" => number::boolean_global(ctx),
            "parseInt" => Value::Obj(value::coercion(ctx, "parseInt", 2)),
            "parseFloat" => Value::Obj(value::coercion(ctx, "parseFloat", 1)),
            "isFinite" => Value::Obj(value::coercion(ctx, "isFinite", 1)),
            "isNaN" => Value::Obj(value::coercion(ctx, "isNaN", 1)),
            "Date" => date::date_global(ctx),
            "RegExp" => regexp::regexp_global(ctx),
            "Map" => collections::map_global(ctx),
            "Set" => collections::set_global(ctx),
            "WeakMap" => collections::weak_map_global(ctx),
            "WeakSet" => collections::weak_set_global(ctx),
            "URL" => url::url_global(ctx),
            "URLSearchParams" => url::url_search_params_global(ctx),
            "Headers" => headers::headers_global(ctx),
            "Uint8Array" => bytes::uint8_array_global(ctx),
            "TextEncoder" => bytes::text_encoder_global(ctx),
            "TextDecoder" => bytes::text_decoder_global(ctx),
            "encodeURI" | "encodeURIComponent" | "decodeURI" | "decodeURIComponent" => url::uri_global(ctx, name),
            "atob" | "btoa" => web::base64_global(ctx, name),
            "crypto" => web::crypto_global(ctx),
            "structuredClone" => web::structured_clone_global(ctx),
            other => {
                let ty = ERROR_TYPES.iter().copied().find(|t| t.name() == other).expect("an error type");
                errors::error_global(ctx, ty)
            }
        };
        out.push((JsStr::from(name), value));
    }
    out
}
