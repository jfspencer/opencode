//! Reading and writing the JSON values the store holds, with JS object semantics: an assigned key keeps its place,
//! a new key goes last, `delete` keeps the order of the rest, and setting `undefined` is a delete.
//!
//! `message.get("time")?.num("created")` reads `message.time?.created`; [`obj!`](crate::obj) builds an object
//! literal (absent `Option`s are left out, as `JSON.stringify` drops `undefined`).

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

pub use opencode_protocol::Json as Value;

/// Field access on JSON values.
pub trait JsonExt {
    fn get(&self, key: &str) -> Option<&Json>;
    fn get_mut(&mut self, key: &str) -> Option<&mut Json>;
    /// The value at a dotted path (`"time.created"`).
    fn at(&self, path: &str) -> Option<&Json> {
        let mut value = Some(self.as_json());
        for key in path.split('.') {
            value = value.and_then(|v| v.get(key));
        }
        value
    }
    fn as_json(&self) -> &Json;
    fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Json::String(text)) => Some(text),
            _ => None,
        }
    }
    fn num(&self, key: &str) -> Option<f64> {
        match self.get(key) {
            Some(Json::Number(n)) => Some(*n),
            _ => None,
        }
    }
    fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(Json::Bool(b)) => Some(*b),
            _ => None,
        }
    }
    fn arr(&self, key: &str) -> Option<&Vec<Json>> {
        match self.get(key) {
            Some(Json::Array(items)) => Some(items),
            _ => None,
        }
    }
    /// `value.key = item` (an object only).
    fn set(&mut self, key: &str, item: Json);
    /// `value.key = item` for `Some`, `delete value.key` for `None` (Solid's store setter with `undefined`).
    fn put(&mut self, key: &str, item: Option<Json>) {
        match item {
            Some(item) => self.set(key, item),
            None => {
                self.remove(key);
            }
        }
    }
    /// `delete value.key`.
    fn remove(&mut self, key: &str) -> Option<Json>;
}

impl JsonExt for Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(object) => object.get(key),
            _ => None,
        }
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        match self {
            Json::Object(object) => object.get_mut(key),
            _ => None,
        }
    }

    fn as_json(&self) -> &Json {
        self
    }

    fn set(&mut self, key: &str, item: Json) {
        if let Json::Object(object) = self {
            match object.get_mut(key) {
                Some(slot) => *slot = item,
                None => {
                    object.insert(key.to_owned(), item);
                }
            }
        }
    }

    fn remove(&mut self, key: &str) -> Option<Json> {
        match self {
            Json::Object(object) => object.shift_remove(key),
            _ => None,
        }
    }
}

impl<T: JsonExt + ?Sized> JsonExt for std::rc::Rc<T>
where
    T: Clone,
{
    fn get(&self, key: &str) -> Option<&Json> {
        (**self).get(key)
    }
    fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        std::rc::Rc::make_mut(self).get_mut(key)
    }
    fn as_json(&self) -> &Json {
        (**self).as_json()
    }
    fn set(&mut self, key: &str, item: Json) {
        std::rc::Rc::make_mut(self).set(key, item)
    }
    fn remove(&mut self, key: &str) -> Option<Json> {
        std::rc::Rc::make_mut(self).remove(key)
    }
}

/// JS truthiness of a JSON value.
pub fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Number(n) => *n != 0.0 && !n.is_nan(),
        Json::String(s) => !s.is_empty(),
        _ => true,
    }
}

pub fn string(text: impl Into<String>) -> Json {
    Json::String(text.into())
}

pub fn number(n: f64) -> Json {
    Json::Number(n)
}

pub fn object() -> Json {
    Json::Object(Object::new())
}

/// Something an [`obj!`](crate::obj) entry accepts: a value, or an optional one (left out when `None`).
pub trait Entry {
    fn into_entry(self) -> Option<Json>;
}

impl Entry for Json {
    fn into_entry(self) -> Option<Json> {
        Some(self)
    }
}

impl Entry for Option<Json> {
    fn into_entry(self) -> Option<Json> {
        self
    }
}

impl Entry for &Json {
    fn into_entry(self) -> Option<Json> {
        Some(self.clone())
    }
}

impl Entry for Option<&Json> {
    fn into_entry(self) -> Option<Json> {
        self.cloned()
    }
}

impl Entry for &str {
    fn into_entry(self) -> Option<Json> {
        Some(Json::String(self.to_owned()))
    }
}

impl Entry for String {
    fn into_entry(self) -> Option<Json> {
        Some(Json::String(self))
    }
}

impl Entry for &String {
    fn into_entry(self) -> Option<Json> {
        Some(Json::String(self.clone()))
    }
}

impl Entry for Option<&str> {
    fn into_entry(self) -> Option<Json> {
        self.map(|s| Json::String(s.to_owned()))
    }
}

impl Entry for Option<String> {
    fn into_entry(self) -> Option<Json> {
        self.map(Json::String)
    }
}

impl Entry for f64 {
    fn into_entry(self) -> Option<Json> {
        Some(Json::Number(self))
    }
}

impl Entry for Option<f64> {
    fn into_entry(self) -> Option<Json> {
        self.map(Json::Number)
    }
}

impl Entry for bool {
    fn into_entry(self) -> Option<Json> {
        Some(Json::Bool(self))
    }
}

impl Entry for Vec<Json> {
    fn into_entry(self) -> Option<Json> {
        Some(Json::Array(self))
    }
}

/// Builds an object from entries, leaving out absent ones.
pub fn build(entries: Vec<(&str, Option<Json>)>) -> Json {
    let mut object = Object::new();
    for (key, value) in entries {
        if let Some(value) = value {
            object.insert(key.to_owned(), value);
        }
    }
    Json::Object(object)
}

/// A JS object literal: `obj! { "id" => id, "previous" => maybe }`; a `None` value leaves its key out.
#[macro_export]
macro_rules! obj {
    () => { $crate::json::object() };
    ($($key:expr => $value:expr),+ $(,)?) => {
        $crate::json::build(vec![$(($key, $crate::json::Entry::into_entry($value))),+])
    };
}
