//! Just enough JSON for the harness: strings decode to UTF-16 (so `\ud800` survives) and print as `JSON.stringify`.

use opencode_shell::js::{self, JsString};

#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn str(&self, key: &str) -> JsString {
        match self.get(key) {
            Some(Value::String(s)) => s.clone(),
            other => panic!("expected a string at {key}, got {other:?}"),
        }
    }

    pub fn text(&self, key: &str) -> String {
        js::text(&self.str(key))
    }

    pub fn opt_text(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(Value::String(s)) => Some(js::text(s)),
            _ => None,
        }
    }

    pub fn num(&self, key: &str) -> f64 {
        match self.get(key) {
            Some(Value::Number(n)) => *n,
            other => panic!("expected a number at {key}, got {other:?}"),
        }
    }

    pub fn opt_num(&self, key: &str) -> Option<f64> {
        match self.get(key) {
            Some(Value::Number(n)) => Some(*n),
            _ => None,
        }
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn array(&self, key: &str) -> &[Value] {
        match self.get(key) {
            Some(Value::Array(items)) => items,
            other => panic!("expected an array at {key}, got {other:?}"),
        }
    }

    pub fn as_str(&self) -> JsString {
        match self {
            Value::String(s) => s.clone(),
            other => panic!("expected a string, got {other:?}"),
        }
    }
}

pub fn parse(text: &str) -> Value {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut parser = Parser { s: &units, i: 0 };
    let value = parser.value();
    parser.ws();
    assert_eq!(parser.i, units.len(), "trailing text in {text}");
    value
}

struct Parser<'a> {
    s: &'a [u16],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], 0x20 | 0x09 | 0x0a | 0x0d) {
            self.i += 1;
        }
    }

    fn peek(&self) -> u16 {
        self.s[self.i]
    }

    fn literal(&mut self, word: &str) {
        assert!(js::starts_with_ascii(self.s, word, self.i as isize), "expected {word}");
        self.i += word.len();
    }

    fn value(&mut self) -> Value {
        self.ws();
        match self.peek() {
            0x7b => {
                self.i += 1;
                let mut entries = Vec::new();
                self.ws();
                if self.peek() == b'}' as u16 {
                    self.i += 1;
                    return Value::Object(entries);
                }
                loop {
                    self.ws();
                    let key = js::text(&self.string());
                    self.ws();
                    assert_eq!(self.peek(), b':' as u16);
                    self.i += 1;
                    let value = self.value();
                    entries.push((key, value));
                    self.ws();
                    let c = self.peek();
                    self.i += 1;
                    if c == b'}' as u16 {
                        return Value::Object(entries);
                    }
                    assert_eq!(c, b',' as u16);
                }
            }
            0x5b => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.peek() == b']' as u16 {
                    self.i += 1;
                    return Value::Array(items);
                }
                loop {
                    items.push(self.value());
                    self.ws();
                    let c = self.peek();
                    self.i += 1;
                    if c == b']' as u16 {
                        return Value::Array(items);
                    }
                    assert_eq!(c, b',' as u16);
                }
            }
            0x22 => Value::String(self.string()),
            0x74 => {
                self.literal("true");
                Value::Bool(true)
            }
            0x66 => {
                self.literal("false");
                Value::Bool(false)
            }
            0x6e => {
                self.literal("null");
                Value::Null
            }
            _ => {
                let start = self.i;
                while self.i < self.s.len() && js::set_has("-+.eE0123456789", self.s[self.i]) {
                    self.i += 1;
                }
                Value::Number(js::text(&self.s[start..self.i]).parse().expect("a number"))
            }
        }
    }

    fn string(&mut self) -> JsString {
        assert_eq!(self.peek(), b'"' as u16);
        self.i += 1;
        let mut out = JsString::new();
        loop {
            let c = self.s[self.i];
            self.i += 1;
            match c {
                0x22 => return out,
                0x5c => {
                    let e = self.s[self.i];
                    self.i += 1;
                    match e {
                        0x62 => out.push(0x08),
                        0x66 => out.push(0x0c),
                        0x6e => out.push(0x0a),
                        0x72 => out.push(0x0d),
                        0x74 => out.push(0x09),
                        0x75 => {
                            let hex = js::text(&self.s[self.i..self.i + 4]);
                            out.push(u16::from_str_radix(&hex, 16).expect("hex"));
                            self.i += 4;
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
    }
}

pub fn string(s: &[u16]) -> String {
    let mut out = String::new();
    js::json_string(s, &mut out);
    out
}

pub fn text(s: &str) -> String {
    string(&js::units(s))
}

pub fn strings(items: &[JsString]) -> String {
    format!("[{}]", items.iter().map(|s| string(s)).collect::<Vec<_>>().join(","))
}

/// An object from already-encoded values, in order; `None` values are left out like `undefined`.
pub fn object(entries: &[(&str, Option<String>)]) -> String {
    let fields: Vec<String> =
        entries.iter().filter_map(|(k, v)| v.as_ref().map(|v| format!("{}:{v}", text(k)))).collect();
    format!("{{{}}}", fields.join(","))
}
