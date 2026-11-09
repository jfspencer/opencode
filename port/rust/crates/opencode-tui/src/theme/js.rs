//! JavaScript semantics the theme and config ports depend on: `Math.round`, number to string, property order of
//! objects (`Object.keys`: array-index keys first, ascending, then insertion order), and Effect 4.1's
//! `Formatter.format` of a JSON value (the `got <input>` of a schema issue).

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

/// `Math.round`: halves round toward +Infinity.
pub fn round(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let floor = value.floor();
    if value - floor >= 0.5 { floor + 1.0 } else { floor }
}

/// `Math.max(a, b)`: NaN if either is NaN.
pub fn max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
}

/// `Math.min(a, b)`: NaN if either is NaN.
pub fn min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}

/// `Number(text)`.
pub fn to_number(text: &str) -> f64 {
    opencode_protocol::wire::number::js_string_to_number(text)
}

/// `String(n)` for a number.
pub fn number(n: f64) -> String {
    opencode_protocol::wire::number::js_number_string(n)
}

/// Whether a property key is an array index (`"0"`..`"4294967294"`, canonical form), which JS orders first.
pub fn is_array_index(key: &str) -> bool {
    if key.is_empty() || key.len() > 10 || !key.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if key.len() > 1 && key.starts_with('0') {
        return false;
    }
    key.parse::<u64>().map(|n| n < 4_294_967_295).unwrap_or(false)
}

/// The keys of an object in JS property order.
pub fn keys(object: &Object) -> Vec<&str> {
    let mut indices: Vec<&str> = object.keys().map(String::as_str).filter(|k| is_array_index(k)).collect();
    indices.sort_by_key(|k| k.parse::<u64>().unwrap());
    indices.extend(object.keys().map(String::as_str).filter(|k| !is_array_index(k)));
    indices
}

/// The entries of an object in JS property order (`Object.entries`).
pub fn entries(object: &Object) -> Vec<(&str, &Json)> {
    keys(object).into_iter().map(|k| (k, &object[k])).collect()
}

/// `JSON.stringify` of a string.
pub fn quote(text: &str) -> String {
    let mut out = String::new();
    opencode_protocol::wire::out::write_json_string(&mut out, text);
    out
}

/// Effect 4.1 `Formatter.format(input)` (no indentation) of a JSON value: strings JSON-quoted, numbers as
/// `String(n)`, objects as `{"key":value}` in property order, arrays as `[a,b]`.
pub fn format(value: &Json) -> String {
    let mut out = String::new();
    format_into(&mut out, value);
    out
}

fn format_into(out: &mut String, value: &Json) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(n) => out.push_str(&number(*n)),
        Json::String(s) => out.push_str(&quote(s)),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                format_into(out, item);
            }
            out.push(']');
        }
        Json::Object(object) => {
            out.push('{');
            for (i, (key, item)) in entries(object).into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&quote(key));
                out.push(':');
                format_into(out, item);
            }
            out.push('}');
        }
    }
}

/// `/got ("[^"]*"|\S+)/.exec(message)?.[1]`.
pub fn got(message: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(found) = message[from..].find("got ") {
        let start = from + found + 4;
        let rest = &message[start..];
        if let Some(stripped) = rest.strip_prefix('"')
            && let Some(close) = stripped.find('"')
        {
            return Some(&rest[..close + 2]);
        }
        let end = rest.find(char::is_whitespace_js).unwrap_or(rest.len());
        if end > 0 {
            return Some(&rest[..end]);
        }
        from = start;
    }
    None
}

/// JS regex `\s`.
trait JsWhitespace {
    fn is_whitespace_js(self) -> bool;
}

impl JsWhitespace for char {
    fn is_whitespace_js(self) -> bool {
        opencode_protocol::wire::number::is_js_whitespace(self) || self == '\u{feff}'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding() {
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -2.0);
        assert_eq!(round(0.49999999999999994), 0.0);
    }

    #[test]
    fn got_values() {
        assert_eq!(got("Expected string, got \"a b\"\n  at x"), Some("\"a b\""));
        assert_eq!(got("Expected string, got {\"a\":\"x y\"}"), Some("{\"a\":\"x"));
        assert_eq!(got("Missing key\n  at [\"base\"]"), None);
        assert_eq!(got("Missing key\nExpected object, got 3"), Some("3"));
    }

    #[test]
    fn key_order() {
        let Json::Object(o) = opencode_protocol::wire::json::parse(r#"{"b":1,"10":2,"a":3,"2":4,"01":5}"#).unwrap()
        else {
            panic!()
        };
        assert_eq!(keys(&o), vec!["2", "10", "b", "a", "01"]);
    }
}
