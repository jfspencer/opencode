//! Small helpers over the protocol crate's JSON value, which keeps JS insertion order.

use opencode_protocol::wire::{Object, Out};
use opencode_protocol::{Json, Wire};

/// Builds a JSON object in insertion order, like a JS object literal (absent values are skipped, as
/// `JSON.stringify` skips `undefined`).
#[derive(Default)]
pub struct Obj(Object);

impl Obj {
    pub fn new() -> Self {
        Self(Object::new())
    }

    pub fn set(&mut self, key: &str, value: Json) -> &mut Self {
        self.0.insert(key.to_owned(), value);
        self
    }

    pub fn opt(&mut self, key: &str, value: Option<Json>) -> &mut Self {
        if let Some(value) = value {
            self.0.insert(key.to_owned(), value);
        }
        self
    }

    pub fn str(&mut self, key: &str, value: &str) -> &mut Self {
        self.set(key, Json::String(value.to_owned()))
    }

    pub fn opt_str(&mut self, key: &str, value: Option<&str>) -> &mut Self {
        self.opt(key, value.map(|v| Json::String(v.to_owned())))
    }

    pub fn int(&mut self, key: &str, value: i64) -> &mut Self {
        self.set(key, Json::Number(value as f64))
    }

    pub fn done(&mut self) -> Json {
        Json::Object(std::mem::take(&mut self.0))
    }
}

pub fn get<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    value.as_object().and_then(|o| o.get(key))
}

pub fn get_str<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    match get(value, key) {
        Some(Json::String(s)) => Some(s),
        _ => None,
    }
}

/// The JSON value of a wire type (its canonical encoding).
pub fn of<T: Wire>(value: &T) -> Json {
    opencode_protocol::wire::parse(&opencode_protocol::to_string(value)).expect("wire output is valid JSON")
}

/// `JSON.stringify(value)`.
pub fn text(value: &Json) -> String {
    let mut out = Out::new();
    out.json(value);
    out.finish()
}

pub fn parse(text: &str) -> Option<Json> {
    opencode_protocol::wire::parse(text).ok()
}

pub fn array(value: &Json) -> Option<&[Json]> {
    match value {
        Json::Array(items) => Some(items),
        _ => None,
    }
}
