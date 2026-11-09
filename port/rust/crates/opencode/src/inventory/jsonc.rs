//! JSON with comments, as `jsonc-parser` 3.3.1 reads it for config files (`parse(text, errors, { allowTrailingComma:
//! true })`, packages/core/src/config.ts): `//` and `/* */` comments, trailing commas in objects and arrays. opencode
//! rejects a document with any parse error, so [`parse`] answers `None` wherever jsonc-parser reports one.
//!
//! The value is built as jsonc-parser builds it: a repeated key keeps its first position and its last value, and
//! object keys are then iterated in JS property order (array-index keys first), as `Object.entries` does. A
//! `__proto__` key sets the prototype in JS instead of a property, so it is dropped.
//!
//! [`set_root_property`] is `applyEdits(text, modify(text, [key], value, { formattingOptions: { tabSize: 2,
//! insertSpaces: true } }))` for a property of the root object (the only edit the server makes, `Config.update`).
//! TODO(port): jsonc-parser's formatter reformats the edited range; the edit here writes the same JSON value with
//! the formatter's layout for the common cases (an empty object, appending a property, replacing a value), which only
//! differs in whitespace elsewhere.

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

/// The parsed value, or `None` when jsonc-parser reports any error (an empty document included).
pub fn parse(text: &str) -> Option<Json> {
    let mut p = Parser { s: text.as_bytes(), text, i: 0, root: Vec::new(), depth: 0 };
    p.skip()?;
    if p.i >= p.s.len() {
        return None;
    }
    let value = p.value()?;
    p.skip()?;
    (p.i == p.s.len()).then_some(value)
}

/// A property of the root object: the span of the whole property and of its value, in bytes.
#[derive(Debug, Clone)]
struct Property {
    key: String,
    start: usize,
    value_start: usize,
    end: usize,
}

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
    /// The root object's properties, in order (for edits).
    root: Vec<Property>,
    depth: usize,
}

impl Parser<'_> {
    /// Skips whitespace (space, tab, CR, LF) and comments; an unterminated block comment is an error.
    fn skip(&mut self) -> Option<()> {
        loop {
            match self.s.get(self.i) {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.i += 1,
                Some(b'/') if self.s.get(self.i + 1) == Some(&b'/') => {
                    self.i += 2;
                    while let Some(&c) = self.s.get(self.i) {
                        if c == b'\n' || c == b'\r' {
                            break;
                        }
                        self.i += 1;
                    }
                }
                Some(b'/') if self.s.get(self.i + 1) == Some(&b'*') => {
                    self.i += 2;
                    loop {
                        if self.i + 1 >= self.s.len() {
                            return None;
                        }
                        if self.s[self.i] == b'*' && self.s[self.i + 1] == b'/' {
                            self.i += 2;
                            break;
                        }
                        self.i += 1;
                    }
                }
                _ => return Some(()),
            }
        }
    }

    fn value(&mut self) -> Option<Json> {
        match self.s.get(self.i)? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string().map(Json::String),
            b'-' | b'0'..=b'9' => self.number(),
            _ => {
                // A word: true, false, null, anything else is an invalid symbol.
                let start = self.i;
                while let Some(&c) = self.s.get(self.i) {
                    if matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'{' | b'}' | b'[' | b']' | b'"' | b':' | b',' | b'/') {
                        break;
                    }
                    self.i += 1;
                }
                match &self.text[start..self.i] {
                    "true" => Some(Json::Bool(true)),
                    "false" => Some(Json::Bool(false)),
                    "null" => Some(Json::Null),
                    _ => None,
                }
            }
        }
    }

    fn object(&mut self) -> Option<Json> {
        self.depth += 1;
        let root = self.depth == 1;
        self.i += 1;
        let mut map = Object::new();
        let mut needs_comma = false;
        loop {
            self.skip()?;
            match self.s.get(self.i)? {
                b'}' => {
                    self.i += 1;
                    break;
                }
                b',' => {
                    if !needs_comma {
                        return None;
                    }
                    self.i += 1;
                    self.skip()?;
                    if self.s.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        break;
                    }
                }
                _ if needs_comma => return None,
                _ => {}
            }
            let start = self.i;
            if self.s.get(self.i) != Some(&b'"') {
                return None;
            }
            let key = self.string()?;
            self.skip()?;
            if self.s.get(self.i) != Some(&b':') {
                return None;
            }
            self.i += 1;
            self.skip()?;
            let value_start = self.i;
            let value = self.value()?;
            if root {
                self.root.push(Property { key: key.clone(), start, value_start, end: self.i });
            }
            if key != "__proto__" {
                map.insert(key, value);
            }
            needs_comma = true;
        }
        self.depth -= 1;
        Some(Json::Object(js_order(map)))
    }

    fn array(&mut self) -> Option<Json> {
        self.depth += 1;
        self.i += 1;
        let mut items = Vec::new();
        let mut needs_comma = false;
        loop {
            self.skip()?;
            match self.s.get(self.i)? {
                b']' => {
                    self.i += 1;
                    break;
                }
                b',' => {
                    if !needs_comma {
                        return None;
                    }
                    self.i += 1;
                    self.skip()?;
                    if self.s.get(self.i) == Some(&b']') {
                        self.i += 1;
                        break;
                    }
                }
                _ if needs_comma => return None,
                _ => {}
            }
            items.push(self.value()?);
            needs_comma = true;
        }
        self.depth -= 1;
        Some(Json::Array(items))
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
            if !self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
                return None;
            }
        }
        let digits = |p: &mut Self| {
            while p.s.get(p.i).is_some_and(u8::is_ascii_digit) {
                p.i += 1;
            }
        };
        if self.s[self.i] == b'0' {
            self.i += 1;
        } else {
            digits(self);
        }
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            digits(self);
        }
        if let Some(b'e' | b'E') = self.s.get(self.i) {
            self.i += 1;
            if let Some(b'+' | b'-') = self.s.get(self.i) {
                self.i += 1;
            }
            if !self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            digits(self);
        }
        self.text[start..self.i].parse::<f64>().ok().map(Json::Number)
    }

    fn string(&mut self) -> Option<String> {
        self.i += 1;
        let mut out = String::new();
        let mut units: Vec<u16> = Vec::new();
        let flush = |units: &mut Vec<u16>, out: &mut String| {
            if !units.is_empty() {
                // Lone surrogates are valid in a JS string; a Rust string holds their replacement.
                out.push_str(&String::from_utf16_lossy(units));
                units.clear();
            }
        };
        loop {
            let start = self.i;
            while let Some(&c) = self.s.get(self.i) {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            if self.i > start {
                flush(&mut units, &mut out);
                out.push_str(&self.text[start..self.i]);
            }
            match *self.s.get(self.i)? {
                b'"' => {
                    self.i += 1;
                    flush(&mut units, &mut out);
                    return Some(out);
                }
                b'\\' => {
                    self.i += 1;
                    let c = *self.s.get(self.i)?;
                    self.i += 1;
                    let simple = match c {
                        b'"' => Some('"'),
                        b'\\' => Some('\\'),
                        b'/' => Some('/'),
                        b'b' => Some('\u{8}'),
                        b'f' => Some('\u{c}'),
                        b'n' => Some('\n'),
                        b'r' => Some('\r'),
                        b't' => Some('\t'),
                        b'u' => None,
                        _ => return None,
                    };
                    match simple {
                        Some(ch) => {
                            flush(&mut units, &mut out);
                            out.push(ch);
                        }
                        None => {
                            let hex = self.text.get(self.i..self.i + 4)?;
                            let unit = u16::from_str_radix(hex, 16).ok().filter(|_| hex.bytes().all(|b| b.is_ascii_hexdigit()))?;
                            self.i += 4;
                            units.push(unit);
                        }
                    }
                }
                // A control character (a line break ends the string): an error either way.
                _ => return None,
            }
        }
    }
}

/// Reorders an object's keys as JS iterates them: array-index keys first, ascending, then the rest in insertion order.
pub fn js_order(map: Object) -> Object {
    if !map.keys().any(|k| opencode_protocol::wire::out::array_index(k).is_some()) {
        return map;
    }
    let mut indexed: Vec<(u32, String, Json)> = Vec::new();
    let mut rest = Object::new();
    for (k, v) in map {
        match opencode_protocol::wire::out::array_index(&k) {
            Some(n) => indexed.push((n, k, v)),
            None => {
                rest.insert(k, v);
            }
        }
    }
    indexed.sort_by_key(|(n, _, _)| *n);
    let mut out = Object::new();
    for (_, k, v) in indexed {
        out.insert(k, v);
    }
    out.extend(rest);
    out
}

/// Sets (or with `None` removes) a property of the root object, keeping the rest of the text. `None` when the edit
/// is impossible (jsonc-parser throws: deleting in an empty document, a root that is not an object).
pub fn set_root_property(text: &str, key: &str, value: Option<&Json>) -> Option<String> {
    let mut p = Parser { s: text.as_bytes(), text, i: 0, root: Vec::new(), depth: 0 };
    let parsed = p.skip().and_then(|_| if p.i < p.s.len() { p.value().map(|v| (v, p.i)) } else { None });
    let encoded = value.map(crate::json::text);
    let Some((root, _)) = parsed else {
        // An empty or unreadable document: the edit replaces it with the value.
        let encoded = encoded?;
        return Some(format!("{{\n  {}: {}\n}}", crate::json::text(&Json::String(key.to_owned())), encoded));
    };
    if !matches!(root, Json::Object(_)) {
        return None;
    }
    let root_start = text.as_bytes().iter().position(|&b| b == b'{').unwrap_or(0);
    let properties = p.root;
    let existing = properties.iter().rposition(|property| property.key == key);
    match (existing, encoded) {
        (Some(index), Some(encoded)) => {
            let property = &properties[index];
            Some(format!("{}{}{}", &text[..property.value_start], encoded, &text[property.end..]))
        }
        (Some(index), None) => {
            let property = &properties[index];
            let (begin, end) = if index > 0 {
                (properties[index - 1].end, property.end)
            } else if properties.len() > 1 {
                (root_start + 1, properties[1].start)
            } else {
                (root_start + 1, property.end)
            };
            let mut out = format!("{}{}", &text[..begin], &text[end..]);
            if properties.len() == 1 {
                // The formatter collapses the emptied object.
                let close = out[begin..].find('}').map(|i| begin + i)?;
                out = format!("{}{}", &out[..begin], &out[close..]);
            }
            Some(out)
        }
        (None, None) => Some(text.to_owned()),
        (None, Some(encoded)) => {
            let property = format!("{}: {}", crate::json::text(&Json::String(key.to_owned())), encoded);
            match properties.last() {
                Some(last) => Some(format!("{},\n  {}{}", &text[..last.end], property, &text[last.end..])),
                None => {
                    let close = text[root_start..].find('}').map(|i| root_start + i)?;
                    Some(format!("{}\n  {}\n{}", &text[..root_start + 1], property, &text[close..]))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(v: &Option<Json>) -> Option<String> {
        v.as_ref().map(crate::json::text)
    }

    #[test]
    fn accepts_comments_and_trailing_commas() {
        let doc = "// comments are allowed\n{\n  \"username\": \"from-jsonc\",\n  \"timezone\": \"Europe/Paris\", // trailing comment\n}\n";
        assert_eq!(text(&parse(doc)).as_deref(), Some(r#"{"username":"from-jsonc","timezone":"Europe/Paris"}"#));
        assert_eq!(text(&parse("{ \"a\": [1, 2,], /* x */ }")).as_deref(), Some(r#"{"a":[1,2]}"#));
        assert_eq!(text(&parse("{\"a\":1,\"a\":2,\"b\":3}")).as_deref(), Some(r#"{"a":2,"b":3}"#));
        assert_eq!(text(&parse("{\"b\":1,\"2\":2,\"1\":3}")).as_deref(), Some(r#"{"1":3,"2":2,"b":1}"#));
    }

    #[test]
    fn rejects_what_jsonc_parser_reports() {
        for doc in ["", "  ", "{ \"username\": \"broken\", ", "{,}", "[,]", "[1,,2]", "{\"a\" 1}", "01", "1.", "-", "tru", "{} x", "/* open", "{'a':1}", "\"a\nb\""] {
            assert!(parse(doc).is_none(), "{doc:?}");
        }
        assert!(parse("[1, 2, 3]").is_some());
        assert!(parse("\"just a string\"").is_some());
    }

    #[test]
    fn edits_root_properties() {
        let set = |text: &str, value: Option<&str>| set_root_property(text, "shell", value.map(|v| Json::String(v.into())).as_ref());
        assert_eq!(set("{}\n", Some("/bin/sh")).as_deref(), Some("{\n  \"shell\": \"/bin/sh\"\n}\n"));
        assert_eq!(set("{\n  \"shell\": \"/bin/sh\"\n}\n", Some("/bin/bash")).as_deref(), Some("{\n  \"shell\": \"/bin/bash\"\n}\n"));
        assert_eq!(set("{\n  \"shell\": \"/bin/sh\"\n}\n", None).as_deref(), Some("{}\n"));
        assert_eq!(set("{\"a\": 1}", Some("x")).as_deref(), Some("{\"a\": 1,\n  \"shell\": \"x\"}"));
        assert_eq!(set("{}\n", None).as_deref(), Some("{}\n"));
        assert_eq!(set("[1]", Some("x")), None);
    }
}
