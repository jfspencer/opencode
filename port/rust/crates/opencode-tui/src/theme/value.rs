//! The JS values the theme code passes around: theme documents, definitions and their resolved trees.
//!
//! The TS works on plain objects; this is the same shape: records keep JS property order (array-index keys first,
//! ascending, then insertion order), and a resolved color is a [`Color`], which carries the identity the TS gets
//! from RGBA object identity (`ResolvedTheme.source`/`increase`/`decrease` only know the colors of the theme's own
//! hue scales).

use opencode_protocol::Json;

use super::js;
use super::rgba::RGBA;

/// A resolved theme color. `origin` is set for the colors of a resolved theme's hue scales (and for every token
/// that references one): it stands for the RGBA object the TS indexes in `compileHueSteps`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color {
    pub rgba: RGBA,
    pub(crate) origin: Option<Origin>,
}

/// Which hue scale step of which resolved theme a color is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Origin {
    /// The resolution that created it (one per `resolveExpandedTheme` call).
    pub theme: u32,
    /// Index of the hue in the theme's hue record.
    pub hue: u16,
    /// Position of the step in `HueStep.literals`.
    pub position: u8,
}

impl Color {
    /// A color with no hue identity (a hex literal, `transparent`, a computed color).
    pub fn plain(rgba: RGBA) -> Color {
        Color { rgba, origin: None }
    }
}

impl std::ops::Deref for Color {
    type Target = RGBA;
    fn deref(&self) -> &RGBA {
        &self.rgba
    }
}

impl From<Color> for RGBA {
    fn from(color: Color) -> RGBA {
        color.rgba
    }
}

/// A JS value as the theme code sees it.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Rec(Rec),
    Color(Color),
}

/// A plain object: entries in JS property order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Rec(Vec<(String, Value)>);

impl Rec {
    pub fn new() -> Rec {
        Rec(Vec::new())
    }

    /// Sets a property: an existing key keeps its position, a new one goes where JS would put it.
    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        let key = key.into();
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
            return;
        }
        if js::is_array_index(&key) {
            let n: u64 = key.parse().unwrap();
            let at = self
                .0
                .iter()
                .position(|(k, _)| !js::is_array_index(k) || k.parse::<u64>().unwrap() > n)
                .unwrap_or(self.0.len());
            self.0.insert(at, (key, value));
        } else {
            self.0.push((key, value));
        }
    }

    /// `delete o[key]`.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let at = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(at).1)
    }

    /// An own property (`Object.hasOwn`), `undefined` included.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// `o[key] !== undefined` for an own property.
    pub fn defined(&self, key: &str) -> Option<&Value> {
        self.get(key).filter(|v| !matches!(v, Value::Undefined))
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn from_entries(entries: impl IntoIterator<Item = (String, Value)>) -> Rec {
        let mut rec = Rec::new();
        for (k, v) in entries {
            rec.set(k, v);
        }
        rec
    }
}

impl Value {
    pub fn str(text: impl Into<String>) -> Value {
        Value::Str(text.into())
    }

    /// `isRecord` (expand.ts / resolve.ts): a plain object, not an array, not a color.
    pub fn as_rec(&self) -> Option<&Rec> {
        match self {
            Value::Rec(r) => Some(r),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }

    /// `o?.[key]` where `o` is this value: a record's own property, `undefined` otherwise.
    pub fn at(&self, key: &str) -> &Value {
        self.as_rec().and_then(|r| r.get(key)).unwrap_or(&Value::Undefined)
    }

    /// JS truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            _ => true,
        }
    }

    /// A JSON value as a JS value (`JSON.parse` output).
    pub fn from_json(json: &Json) -> Value {
        match json {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(n) => Value::Num(*n),
            Json::String(s) => Value::Str(s.clone()),
            Json::Array(items) => Value::Arr(items.iter().map(Value::from_json).collect()),
            Json::Object(object) => {
                Value::Rec(Rec(js::entries(object).into_iter().map(|(k, v)| (k.to_owned(), Value::from_json(v))).collect()))
            }
        }
    }

    /// The value as JSON (`undefined` properties dropped, colors as their hex), for tests and the harness.
    pub fn to_json(&self) -> Json {
        match self {
            Value::Undefined | Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::Num(n) => Json::Number(*n),
            Value::Str(s) => Json::String(s.clone()),
            Value::Arr(items) => Json::Array(items.iter().map(Value::to_json).collect()),
            Value::Rec(rec) => Json::Object(
                rec.entries().filter(|(_, v)| !v.is_undefined()).map(|(k, v)| (k.to_owned(), v.to_json())).collect(),
            ),
            Value::Color(c) => Json::String(c.to_hex()),
        }
    }
}

/// `mergeTheme(...values)` (expand.ts): a deep merge of records; `undefined` items are skipped, non-records replace.
pub fn merge_theme(values: &[&Value]) -> Rec {
    let mut result = Rec::new();
    for value in values {
        let Some(rec) = value.as_rec() else { continue };
        for (key, item) in rec.entries() {
            if item.is_undefined() {
                continue;
            }
            let merged = if item.as_rec().is_some() {
                Value::Rec(merge_theme(&[result.get(key).unwrap_or(&Value::Undefined), item]))
            } else {
                item.clone()
            };
            result.set(key, merged);
        }
    }
    result
}

/// Property names every plain object inherits from `Object.prototype`: a lookup of one of these that is not an
/// own property finds a function (or, for `__proto__`, `Object.prototype` itself) instead of `undefined`.
pub fn is_prototype_property(key: &str) -> bool {
    matches!(
        key,
        "constructor"
            | "__proto__"
            | "toString"
            | "toLocaleString"
            | "valueOf"
            | "hasOwnProperty"
            | "isPrototypeOf"
            | "propertyIsEnumerable"
            | "__defineGetter__"
            | "__defineSetter__"
            | "__lookupGetter__"
            | "__lookupSetter__"
    )
}
