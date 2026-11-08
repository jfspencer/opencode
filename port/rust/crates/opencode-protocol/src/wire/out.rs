//! Writes JSON text byte-for-byte as `JSON.stringify` (without indentation) would.

use super::json::Json;
use super::number::write_js_number;

/// A JSON text being written. Objects and arrays are written with [`Out::object`] / [`Out::array`].
#[derive(Default)]
pub struct Out {
    buf: String,
}

impl Out {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn finish(self) -> String {
        self.buf
    }

    pub fn null(&mut self) {
        self.buf.push_str("null");
    }

    pub fn bool(&mut self, b: bool) {
        self.buf.push_str(if b { "true" } else { "false" });
    }

    /// A number as `JSON.stringify` writes it: non-finite numbers become `null`.
    pub fn number(&mut self, n: f64) {
        if n.is_finite() {
            write_js_number(&mut self.buf, n);
        } else {
            self.null();
        }
    }

    pub fn int(&mut self, n: i64) {
        use std::fmt::Write;
        write!(self.buf, "{n}").expect("write to string");
    }

    pub fn string(&mut self, s: &str) {
        write_json_string(&mut self.buf, s);
    }

    pub fn json(&mut self, v: &Json) {
        match v {
            Json::Null => self.null(),
            Json::Bool(b) => self.bool(*b),
            Json::Number(n) => self.number(*n),
            Json::String(s) => self.string(s),
            Json::Array(items) => {
                let mut a = self.array();
                for item in items {
                    a.item().json(item);
                }
            }
            Json::Object(o) => {
                let mut w = self.object();
                for key in js_key_order(o.keys().map(String::as_str)) {
                    w.key(key).json(&o[key]);
                }
            }
        }
    }

    pub fn array(&mut self) -> ArrayOut<'_> {
        self.buf.push('[');
        ArrayOut { out: self, first: true }
    }

    pub fn object(&mut self) -> ObjectOut<'_> {
        self.buf.push('{');
        ObjectOut { out: self, first: true }
    }

    /// Appends text that is already JSON.
    pub fn raw(&mut self, json: &str) {
        self.buf.push_str(json);
    }
}

pub struct ArrayOut<'a> {
    out: &'a mut Out,
    first: bool,
}

impl ArrayOut<'_> {
    pub fn item(&mut self) -> &mut Out {
        if !self.first {
            self.out.buf.push(',');
        }
        self.first = false;
        self.out
    }
}

impl Drop for ArrayOut<'_> {
    fn drop(&mut self) {
        self.out.buf.push(']');
    }
}

/// Entries must be given in JS property order; see [`js_key_order`].
pub struct ObjectOut<'a> {
    out: &'a mut Out,
    first: bool,
}

impl ObjectOut<'_> {
    pub fn key(&mut self, key: &str) -> &mut Out {
        if !self.first {
            self.out.buf.push(',');
        }
        self.first = false;
        write_json_string(&mut self.out.buf, key);
        self.out.buf.push(':');
        self.out
    }
}

impl Drop for ObjectOut<'_> {
    fn drop(&mut self) {
        self.out.buf.push('}');
    }
}

/// JS `QuoteJSONString`.
pub fn write_json_string(buf: &mut String, s: &str) {
    buf.push('"');
    let mut start = 0;
    for (i, c) in s.char_indices() {
        let escape = match c {
            '"' => "\\\"",
            '\\' => "\\\\",
            '\u{8}' => "\\b",
            '\u{c}' => "\\f",
            '\n' => "\\n",
            '\r' => "\\r",
            '\t' => "\\t",
            c if (c as u32) < 0x20 => "",
            _ => continue,
        };
        buf.push_str(&s[start..i]);
        if escape.is_empty() {
            use std::fmt::Write;
            write!(buf, "\\u{:04x}", c as u32).expect("write to string");
        } else {
            buf.push_str(escape);
        }
        start = i + c.len_utf8();
    }
    buf.push_str(&s[start..]);
    buf.push('"');
}

/// Whether `key` is an array index (a canonical decimal integer below 2^32 - 1). JS lists such keys first, in
/// ascending numeric order, ahead of the other keys in insertion order.
pub fn array_index(key: &str) -> Option<u32> {
    let b = key.as_bytes();
    if b.is_empty() || b.len() > 10 || (b.len() > 1 && b[0] == b'0') || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n: u64 = key.parse().ok()?;
    (n < u32::MAX as u64).then_some(n as u32)
}

/// Orders keys as a JS object enumerates them: array indices ascending, then the rest in the given order.
pub fn js_key_order<'a>(keys: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut indices = Vec::new();
    let mut others = Vec::new();
    for key in keys {
        match array_index(key) {
            Some(n) => indices.push((n, key)),
            None => others.push(key),
        }
    }
    if indices.is_empty() {
        return others;
    }
    indices.sort_by_key(|(n, _)| *n);
    indices.into_iter().map(|(_, k)| k).chain(others).collect()
}
