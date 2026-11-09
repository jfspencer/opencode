//! The CLI config schema (`Info` of packages/cli/src/config/schema.ts: `$schema` plus `Config.Info` of
//! packages/tui/src/config/index.tsx) and Effect 4.1's decoding of it, as `Schema.decodeUnknownOption` observes it:
//! whether the value decodes, and the decoded value. Structs keep only their fields, in schema order (excess
//! properties are dropped); a struct with a rest record keeps the other properties after its fields; records with
//! literal keys keep those keys in literal order; a union decodes with its first member that accepts the value.

use std::sync::LazyLock;

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

use super::keybind::DEFINITIONS;

pub(crate) enum S {
    Str,
    Bool,
    /// `Schema.Int` (a safe integer), with `isGreaterThan(gt)`.
    Int { gt: f64 },
    /// `Schema.Finite`, with `isGreaterThanOrEqualTo(min)` and `isLessThanOrEqualTo(max)`.
    Finite { min: Option<f64>, max: Option<f64> },
    Literals(&'static [&'static str]),
    False,
    Any,
    /// Fields: name, schema, required.
    Struct(Vec<(&'static str, S, bool)>),
    /// A struct whose other properties are kept as they are (`Schema.StructWithRest(..., [Record(String, Unknown)])`).
    StructWithRest(Vec<(&'static str, S, bool)>),
    /// `Schema.Record(Schema.String, value)`.
    Record(Box<S>),
    /// `Schema.Record(Schema.Literals(keys), Schema.optionalKey(value))`.
    Keys(&'static [&'static str], Box<S>),
    Array(Box<S>),
    Union(Vec<S>),
}

fn is_object(value: &Json) -> Option<&Object> {
    match value {
        Json::Object(o) => Some(o),
        _ => None,
    }
}

fn fields(schema: &[(&'static str, S, bool)], input: &Object, out: &mut Object) -> Option<()> {
    for (name, field, required) in schema {
        match input.get(*name) {
            None => {
                if *required {
                    return None;
                }
            }
            Some(value) => {
                out.insert((*name).to_owned(), field.decode(value)?);
            }
        }
    }
    Some(())
}

impl S {
    pub fn decode(&self, input: &Json) -> Option<Json> {
        match self {
            S::Str => matches!(input, Json::String(_)).then(|| input.clone()),
            S::Bool => matches!(input, Json::Bool(_)).then(|| input.clone()),
            S::Int { gt } => match input {
                Json::Number(n) if n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0 && n > gt => {
                    Some(input.clone())
                }
                _ => None,
            },
            S::Finite { min, max } => match input {
                Json::Number(n) if n.is_finite() && min.is_none_or(|m| *n >= m) && max.is_none_or(|m| *n <= m) => {
                    Some(input.clone())
                }
                _ => None,
            },
            S::Literals(values) => match input {
                Json::String(s) if values.contains(&s.as_str()) => Some(input.clone()),
                _ => None,
            },
            S::False => matches!(input, Json::Bool(false)).then(|| input.clone()),
            S::Any => Some(input.clone()),
            S::Struct(schema) => {
                let object = is_object(input)?;
                let mut out = Object::new();
                fields(schema, object, &mut out)?;
                Some(Json::Object(out))
            }
            S::StructWithRest(schema) => {
                let object = is_object(input)?;
                let mut out = Object::new();
                fields(schema, object, &mut out)?;
                for key in crate::theme::js::keys(object) {
                    if !schema.iter().any(|(name, _, _)| *name == key) {
                        out.insert(key.to_owned(), object[key].clone());
                    }
                }
                Some(Json::Object(out))
            }
            S::Record(value) => {
                let object = is_object(input)?;
                let mut out = Object::new();
                for (key, item) in crate::theme::js::entries(object) {
                    out.insert(key.to_owned(), value.decode(item)?);
                }
                Some(Json::Object(out))
            }
            S::Keys(keys, value) => {
                let object = is_object(input)?;
                let mut out = Object::new();
                for key in *keys {
                    if let Some(item) = object.get(*key) {
                        out.insert((*key).to_owned(), value.decode(item)?);
                    }
                }
                Some(Json::Object(out))
            }
            S::Array(item) => match input {
                Json::Array(items) => Some(Json::Array(items.iter().map(|i| item.decode(i)).collect::<Option<_>>()?)),
                _ => None,
            },
            S::Union(members) => members.iter().find_map(|m| m.decode(input)),
        }
    }
}

const SHOW_HIDE: &[&str] = &["show", "hide"];

/// `KeyStroke` of keybind.ts.
fn key_stroke() -> S {
    S::Struct(vec![
        ("name", S::Str, true),
        ("ctrl", S::Bool, false),
        ("shift", S::Bool, false),
        ("meta", S::Bool, false),
        ("super", S::Bool, false),
        ("hyper", S::Bool, false),
    ])
}

/// `BindingItem`: a key string, a keystroke or a binding object.
fn binding_item() -> S {
    S::Union(vec![
        S::Str,
        key_stroke(),
        S::StructWithRest(vec![
            ("key", S::Union(vec![S::Str, key_stroke()]), true),
            ("event", S::Literals(&["press", "release"]), false),
            ("preventDefault", S::Bool, false),
            ("fallthrough", S::Bool, false),
        ]),
    ])
}

/// `BindingValueSchema`: `false`, `"none"`, a binding item or an array of them.
pub(crate) fn binding_value() -> S {
    S::Union(vec![S::False, S::Literals(&["none"]), binding_item(), S::Array(Box::new(binding_item()))])
}

pub(crate) static BINDING_VALUE: LazyLock<S> = LazyLock::new(binding_value);

/// The CLI config (`Info`).
pub(crate) static INFO: LazyLock<S> = LazyLock::new(|| {
    let lit = S::Literals;
    S::Struct(vec![
        ("$schema", S::Str, false),
        ("theme", S::Struct(vec![("name", S::Str, false), ("mode", lit(&["system", "dark", "light"]), false)]), false),
        ("keybinds", S::Struct(DEFINITIONS.iter().map(|(name, _, _)| (*name, binding_value(), false)).collect()), false),
        (
            "plugins",
            S::Array(Box::new(S::Union(vec![
                S::Str,
                S::Struct(vec![("package", S::Str, true), ("options", S::Record(Box::new(S::Any)), false)]),
            ]))),
            false,
        ),
        ("leader", S::Struct(vec![("timeout", S::Int { gt: 0.0 }, false)]), false),
        (
            "scroll",
            S::Struct(vec![("speed", S::Finite { min: Some(0.001), max: None }, false), ("acceleration", S::Bool, false)]),
            false,
        ),
        (
            "attention",
            S::Struct(vec![
                ("notifications", S::Bool, false),
                ("sound", S::Bool, false),
                ("volume", S::Finite { min: Some(0.0), max: Some(1.0) }, false),
                ("sound_pack", S::Str, false),
                (
                    "sounds",
                    S::Keys(&["default", "question", "permission", "error", "done", "subagent_done"], Box::new(S::Str)),
                    false,
                ),
            ]),
            false,
        ),
        (
            "diffs",
            S::Struct(vec![
                ("source", lit(&["working", "branch", "committed", "turn"]), false),
                ("wrap", lit(&["word", "none"]), false),
                ("tree", S::Bool, false),
                ("single", S::Bool, false),
                ("fullscreen", S::Bool, false),
                ("view", lit(&["auto", "split", "unified"]), false),
            ]),
            false,
        ),
        ("terminal", S::Struct(vec![("title", S::Bool, false), ("copy", lit(&["manual", "select"]), false)]), false),
        (
            "prompt",
            S::Struct(vec![
                ("editor", S::Bool, false),
                ("paste", lit(&["compact", "full"]), false),
                ("image_preview", S::Bool, false),
            ]),
            false,
        ),
        (
            "session",
            S::Struct(vec![
                ("sidebar", lit(&["auto", "hide"]), false),
                ("scrollbar", S::Bool, false),
                ("submit_scroll", lit(&["bottom", "keep"]), false),
                ("thinking", lit(SHOW_HIDE), false),
                ("grouping", lit(&["auto", "none"]), false),
                ("verbosity", lit(&["low", "medium", "high"]), false),
                ("image_preview", S::Bool, false),
                ("tps", S::Bool, false),
                ("markdown", lit(&["source", "rendered"]), false),
                ("new_location", lit(&["launch", "inherit"]), false),
                ("permissions", lit(&["prompt", "autoaccept"]), false),
            ]),
            false,
        ),
        (
            "tabs",
            S::Struct(vec![
                ("mode", lit(&["auto", "on", "off"]), false),
                ("enabled", S::Bool, false),
                ("scope", lit(&["global", "cwd"]), false),
                ("layout", lit(&["horizontal", "vertical"]), false),
                ("indicators", lit(&["status", "numbers"]), false),
            ]),
            false,
        ),
        (
            "mini",
            S::Struct(vec![
                ("thinking", lit(SHOW_HIDE), false),
                ("tools", lit(SHOW_HIDE), false),
                ("shell_output", lit(SHOW_HIDE), false),
                ("turn_summary", lit(SHOW_HIDE), false),
                ("footer", lit(SHOW_HIDE), false),
                ("splash", lit(SHOW_HIDE), false),
                (
                    "work_spinner",
                    lit(&[
                        "block-soft-slide",
                        "block-soft-sweep",
                        "block-low-comet",
                        "block-low-duet",
                        "block-shuttle",
                        "block-bridge",
                        "block-squeeze",
                        "small-toggle",
                        "square-toggle",
                        "grow-shrink",
                        "quadrant-orbit",
                        "crosshatch",
                        "density-wave",
                        "seed",
                    ]),
                    false,
                ),
                ("mono", S::Bool, false),
                ("replay", S::Bool, false),
                ("replay_limit", S::Int { gt: 0.0 }, false),
            ]),
            false,
        ),
        (
            "debug",
            S::Struct(vec![
                ("devtools", S::Bool, false),
                ("timing", S::Bool, false),
                ("turn_tokens", S::Union(vec![S::Bool, lit(&["verbose"])]), false),
            ]),
            false,
        ),
        ("experimental", S::Record(Box::new(S::Bool)), false),
        ("animations", S::Bool, false),
        ("highlights", S::Bool, false),
        ("mouse", S::Bool, false),
        (
            "cursor",
            S::Struct(vec![("style", lit(&["block", "underline", "line", "default"]), false), ("blinking", S::Bool, false)]),
            false,
        ),
    ])
});

/// `Schema.decodeUnknownOption(Info)`.
pub fn decode_info(input: &Json) -> Option<Json> {
    INFO.decode(input)
}

/// `Schema.decodeUnknownSync(BindingValueSchema)`.
pub fn decode_binding_value(input: &Json) -> Option<Json> {
    BINDING_VALUE.decode(input)
}
