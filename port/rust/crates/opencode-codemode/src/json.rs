//! Host JSON values as JS holds them: object keys in JS property order (array-index keys first, ascending; then
//! insertion order), strings as UTF-16, numbers as f64. `stringify` and `parse` are the host `JSON.stringify` and
//! `JSON.parse` on these trees.

use crate::jsstr::{JsStr, JsStrBuf};
use crate::numconv;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(JsStr),
    Arr(Vec<Json>),
    Obj(JsonObj),
}

/// A JSON object with JS property order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsonObj {
    entries: Vec<(JsStr, Json)>,
    /// How many leading entries are array-index keys.
    indexes: usize,
}

fn index_of(key: &JsStr) -> Option<u32> {
    crate::interp::value::parse_array_index(key.units())
}

impl JsonObj {
    pub fn new() -> JsonObj {
        JsonObj::default()
    }
    /// Defines `key` as an own data property: an existing key keeps its position.
    pub fn insert(&mut self, key: JsStr, value: Json) {
        if let Some(entry) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            entry.1 = value;
            return;
        }
        match index_of(&key) {
            Some(index) => {
                let at = self.entries[..self.indexes]
                    .iter()
                    .position(|(k, _)| index_of(k).is_some_and(|other| other > index))
                    .unwrap_or(self.indexes);
                self.entries.insert(at, (key, value));
                self.indexes += 1;
            }
            None => self.entries.push((key, value)),
        }
    }
    pub fn insert_str(&mut self, key: &str, value: Json) {
        self.insert(JsStr::from(key), value)
    }
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.entries.iter().find(|(k, _)| k.eq_str(key)).map(|(_, v)| v)
    }
    pub fn get_js(&self, key: &JsStr) -> Option<&Json> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn iter(&self) -> impl Iterator<Item = &(JsStr, Json)> {
        self.entries.iter()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Json {
    pub fn str(text: &str) -> Json {
        Json::Str(JsStr::from(text))
    }
    pub fn as_str(&self) -> Option<&JsStr> {
        match self {
            Json::Str(text) => Some(text),
            _ => None,
        }
    }
    pub fn as_obj(&self) -> Option<&JsonObj> {
        match self {
            Json::Obj(obj) => Some(obj),
            _ => None,
        }
    }
    /// Compact `JSON.stringify(value)`.
    pub fn to_js_string(&self) -> JsStr {
        stringify(self, None, &[])
    }
    /// Compact `JSON.stringify(value)`, lossy for lone surrogates only where Rust strings cannot hold them (they are
    /// escaped, so the text is exact).
    pub fn to_json_string(&self) -> String {
        self.to_js_string().to_rust_lossy()
    }
}

/// QuoteJSONString (well-formed: lone surrogates escape as `\udXXX`).
pub fn quote(text: &JsStr, out: &mut JsStrBuf) {
    out.push_unit(b'"' as u16);
    let units = text.units();
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        match unit {
            0x22 => out.push_str("\\\""),
            0x5C => out.push_str("\\\\"),
            0x08 => out.push_str("\\b"),
            0x0C => out.push_str("\\f"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0..=0x1F => out.push_str(&format!("\\u{unit:04x}")),
            0xD800..=0xDBFF => {
                if index + 1 < units.len() && (0xDC00..=0xDFFF).contains(&units[index + 1]) {
                    out.push_unit(unit);
                    out.push_unit(units[index + 1]);
                    index += 1;
                } else {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            0xDC00..=0xDFFF => out.push_str(&format!("\\u{unit:04x}")),
            _ => out.push_unit(unit),
        }
        index += 1;
    }
    out.push_unit(b'"' as u16);
}

/// The host `JSON.stringify(value, propertyList, gap)` on a JSON tree.
pub fn stringify(value: &Json, properties: Option<&[JsStr]>, gap: &[u16]) -> JsStr {
    let mut out = JsStrBuf::new();
    write(value, properties, gap, &mut Vec::new(), &mut out);
    out.finish()
}

fn write(value: &Json, properties: Option<&[JsStr]>, gap: &[u16], indent: &mut Vec<u16>, out: &mut JsStrBuf) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => {
            if n.is_finite() {
                out.push_str(&numconv::to_string(*n))
            } else {
                out.push_str("null")
            }
        }
        Json::Str(text) => quote(text, out),
        Json::Arr(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            let stepback = indent.len();
            indent.extend_from_slice(gap);
            out.push_unit(b'[' as u16);
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_unit(b',' as u16);
                }
                if !gap.is_empty() {
                    out.push_unit(b'\n' as u16);
                    out.push_units(indent);
                }
                write(item, properties, gap, indent, out);
            }
            if !gap.is_empty() {
                out.push_unit(b'\n' as u16);
                out.push_units(&indent[..stepback]);
            }
            indent.truncate(stepback);
            out.push_unit(b']' as u16);
        }
        Json::Obj(obj) => {
            let selected: Vec<(&JsStr, &Json)> = match properties {
                Some(list) => list.iter().filter_map(|key| obj.get_js(key).map(|value| (key, value))).collect(),
                None => obj.iter().map(|(k, v)| (k, v)).collect(),
            };
            if selected.is_empty() {
                out.push_str("{}");
                return;
            }
            let stepback = indent.len();
            indent.extend_from_slice(gap);
            out.push_unit(b'{' as u16);
            for (index, (key, item)) in selected.iter().enumerate() {
                if index > 0 {
                    out.push_unit(b',' as u16);
                }
                if !gap.is_empty() {
                    out.push_unit(b'\n' as u16);
                    out.push_units(indent);
                }
                quote(key, out);
                out.push_unit(b':' as u16);
                if !gap.is_empty() {
                    out.push_unit(b' ' as u16);
                }
                write(item, properties, gap, indent, out);
            }
            if !gap.is_empty() {
                out.push_unit(b'\n' as u16);
                out.push_units(&indent[..stepback]);
            }
            indent.truncate(stepback);
            out.push_unit(b'}' as u16);
        }
    }
}

/// The host `JSON.parse(text)` without a reviver. The error text is the engine's own (not ported).
pub fn parse(text: &[u16]) -> Result<Json, String> {
    let mut parser = Parser { text, pos: 0 };
    parser.skip_ws();
    let value = parser.value(0)?;
    parser.skip_ws();
    if parser.pos < text.len() {
        return Err(parser.unexpected());
    }
    Ok(value)
}

struct Parser<'a> {
    text: &'a [u16],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u16> {
        self.text.get(self.pos).copied()
    }
    fn skip_ws(&mut self) {
        while let Some(unit) = self.peek() {
            if matches!(unit, 0x20 | 0x09 | 0x0A | 0x0D) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }
    fn unexpected(&self) -> String {
        match self.peek() {
            None => "JSON Parse error: Unexpected EOF".into(),
            Some(unit) => format!(
                "JSON Parse error: Unexpected identifier \"{}\"",
                String::from_utf16_lossy(&[unit])
            ),
        }
    }
    fn expect(&mut self, unit: u8) -> Result<(), String> {
        if self.peek() == Some(unit as u16) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }
    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        for byte in word.bytes() {
            if self.peek() != Some(byte as u16) {
                return Err(self.unexpected());
            }
            self.pos += 1;
        }
        Ok(value)
    }
    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > 5000 {
            return Err("JSON Parse error: Exceeded maximum nesting depth".into());
        }
        match self.peek() {
            None => Err(self.unexpected()),
            Some(0x7B) => {
                self.pos += 1;
                let mut obj = JsonObj::new();
                self.skip_ws();
                if self.peek() == Some(0x7D) {
                    self.pos += 1;
                    return Ok(Json::Obj(obj));
                }
                loop {
                    self.skip_ws();
                    if self.peek() != Some(0x22) {
                        return Err(self.unexpected());
                    }
                    let key = self.string()?;
                    self.skip_ws();
                    self.expect(b':')?;
                    self.skip_ws();
                    let value = self.value(depth + 1)?;
                    obj.insert(key, value);
                    self.skip_ws();
                    match self.peek() {
                        Some(0x2C) => self.pos += 1,
                        Some(0x7D) => {
                            self.pos += 1;
                            return Ok(Json::Obj(obj));
                        }
                        _ => return Err(self.unexpected()),
                    }
                }
            }
            Some(0x5B) => {
                self.pos += 1;
                let mut items = Vec::new();
                self.skip_ws();
                if self.peek() == Some(0x5D) {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                loop {
                    self.skip_ws();
                    items.push(self.value(depth + 1)?);
                    self.skip_ws();
                    match self.peek() {
                        Some(0x2C) => self.pos += 1,
                        Some(0x5D) => {
                            self.pos += 1;
                            return Ok(Json::Arr(items));
                        }
                        _ => return Err(self.unexpected()),
                    }
                }
            }
            Some(0x22) => Ok(Json::Str(self.string()?)),
            Some(0x74) => self.literal("true", Json::Bool(true)),
            Some(0x66) => self.literal("false", Json::Bool(false)),
            Some(0x6E) => self.literal("null", Json::Null),
            Some(unit) if unit == b'-' as u16 || (b'0' as u16..=b'9' as u16).contains(&unit) => self.number(),
            Some(_) => Err(self.unexpected()),
        }
    }
    fn number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        let digit = |unit: Option<u16>| unit.is_some_and(|u| (b'0' as u16..=b'9' as u16).contains(&u));
        if self.peek() == Some(b'-' as u16) {
            self.pos += 1;
        }
        match self.peek() {
            Some(0x30) => self.pos += 1,
            unit if digit(unit) => {
                while digit(self.peek()) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.unexpected()),
        }
        if self.peek() == Some(b'.' as u16) {
            self.pos += 1;
            if !digit(self.peek()) {
                return Err(self.unexpected());
            }
            while digit(self.peek()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(0x65 | 0x45)) {
            self.pos += 1;
            if matches!(self.peek(), Some(0x2B | 0x2D)) {
                self.pos += 1;
            }
            if !digit(self.peek()) {
                return Err(self.unexpected());
            }
            while digit(self.peek()) {
                self.pos += 1;
            }
        }
        Ok(Json::Num(numconv::string_to_number(&self.text[start..self.pos])))
    }
    fn string(&mut self) -> Result<JsStr, String> {
        self.pos += 1;
        let mut out = JsStrBuf::new();
        loop {
            let Some(unit) = self.peek() else { return Err("JSON Parse error: Unterminated string".into()) };
            self.pos += 1;
            match unit {
                0x22 => return Ok(out.finish()),
                0x5C => {
                    let Some(escape) = self.peek() else { return Err("JSON Parse error: Unterminated string".into()) };
                    self.pos += 1;
                    match escape {
                        0x22 => out.push_unit(0x22),
                        0x5C => out.push_unit(0x5C),
                        0x2F => out.push_unit(0x2F),
                        0x62 => out.push_unit(0x08),
                        0x66 => out.push_unit(0x0C),
                        0x6E => out.push_unit(0x0A),
                        0x72 => out.push_unit(0x0D),
                        0x74 => out.push_unit(0x09),
                        0x75 => {
                            let mut code: u16 = 0;
                            for _ in 0..4 {
                                let Some(hex) = self.peek().and_then(|u| char::from_u32(u as u32)?.to_digit(16)) else {
                                    return Err("JSON Parse error: \"\\u\" must be followed by 4 hex digits".into());
                                };
                                code = code * 16 + hex as u16;
                                self.pos += 1;
                            }
                            out.push_unit(code);
                        }
                        _ => return Err("JSON Parse error: Invalid escape character".into()),
                    }
                }
                0..=0x1F => return Err("JSON Parse error: Unterminated string".into()),
                _ => out.push_unit(unit),
            }
        }
    }
}
