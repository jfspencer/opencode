//! A tool call's input as TS decodes it before running the tool: first repaired (plugin/tool-input-repair.ts, the
//! `execute.before` hook), then decoded against the tool's input schema (tool/runtime.ts `decodeInput`), whose
//! failure is the model-facing "Invalid arguments" error.
//!
//! The checks follow the recorded JSON schema of the tool's definition, worded as Effect's Standard Schema formatter
//! words them (`errors: "all"`, no reported input); a field whose check carries its own message in TS names it in
//! `messages`. Covers the flat object schemas of the file tools: string, boolean, integer and number fields with
//! `minimum` / `exclusiveMinimum` / `maximum` / `minLength` / `enum`.
//!
//! TODO(port): nested and array fields, `$ref`s and compositions (no file tool has them); the repair's alias table
//! (`bash` → `shell`) belongs to the registry, not to a tool.

use opencode_protocol::Json;

use super::{Definition, Failure, ToolError};
use crate::json;

/// `repair` of tool-input-repair.ts for a root object schema.
pub fn repair(value: &Json, schema: &Json) -> Json {
    repair_value(value, schema, 0)
}

const MAX_DEPTH: usize = 6;

fn repair_value(value: &Json, schema: &Json, depth: usize) -> Json {
    if depth > MAX_DEPTH {
        return value.clone();
    }
    match json::get_str(schema, "type") {
        Some("number") | Some("integer") => {
            let Json::String(text) = value else { return value.clone() };
            if crate::text::trim(text).is_empty() {
                return value.clone();
            }
            match js_number(text) {
                Some(parsed)
                    if parsed.is_finite() && (json::get_str(schema, "type") != Some("integer") || is_safe_integer(parsed)) =>
                {
                    Json::Number(parsed)
                }
                _ => value.clone(),
            }
        }
        Some("boolean") => match value {
            Json::String(text) if text == "true" => Json::Bool(true),
            Json::String(text) if text == "false" => Json::Bool(false),
            _ => value.clone(),
        },
        Some("object") => repair_object(value, schema, depth),
        _ => value.clone(),
    }
}

fn repair_object(value: &Json, schema: &Json, depth: usize) -> Json {
    let parsed = match value {
        Json::String(text) => match json::parse(text) {
            Some(parsed) => parsed,
            None => return value.clone(),
        },
        other => other.clone(),
    };
    let Json::Object(fields) = &parsed else { return value.clone() };
    let empty = json::object();
    let properties = json::get(schema, "properties").filter(|p| matches!(p, Json::Object(_))).unwrap_or(&empty);
    let required: Vec<&str> = json::array(json::get(schema, "required").unwrap_or(&Json::Null))
        .map(|items| items.iter().filter_map(json::as_str).collect())
        .unwrap_or_default();
    let patterned = matches!(json::get(schema, "patternProperties"), Some(Json::Object(_)));
    let composed = ["allOf", "anyOf", "oneOf"].iter().any(|key| matches!(json::get(schema, key), Some(Json::Array(_))));
    let closed = matches!(json::get(schema, "additionalProperties"), Some(Json::Bool(false)));
    let mut result = parsed.clone();
    for (key, current) in fields {
        let declared = json::get(properties, key);
        if declared.is_none() && closed && !patterned && !composed {
            json::remove(&mut result, key);
            continue;
        }
        let property = match declared {
            Some(property) => property,
            None if !patterned => match json::get(schema, "additionalProperties") {
                Some(property) => property,
                None => continue,
            },
            None => continue,
        };
        if !matches!(property, Json::Object(_)) {
            continue;
        }
        let plain = json::get_str(property, "type").is_some_and(|t| t != "null")
            && !composed
            && ["anyOf", "oneOf", "allOf", "enum", "const", "$ref", "nullable"].iter().all(|k| json::get(property, k).is_none());
        let placeholder = matches!(current, Json::Object(o) if o.is_empty()) && json::get_str(property, "type") != Some("object");
        if declared.is_some() && !required.contains(&key.as_str()) && plain && (matches!(current, Json::Null) || placeholder) {
            json::remove(&mut result, key);
            continue;
        }
        let repaired = repair_value(current, property, depth + 1);
        if &repaired != current {
            json::set(&mut result, key, repaired);
        }
    }
    result
}

fn is_safe_integer(n: f64) -> bool {
    n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0
}

/// `Number(text)` for the strings models send (decimal, exponent, hex/octal/binary, `Infinity`).
fn js_number(text: &str) -> Option<f64> {
    let text = crate::text::trim(text);
    if text.is_empty() {
        return Some(0.0);
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
        if let Some(digits) = text.strip_prefix(prefix) {
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return None;
            }
            return Some(digits.chars().fold(0.0, |n, c| n * radix as f64 + c.to_digit(radix).unwrap_or(0) as f64));
        }
    }
    match text {
        "Infinity" | "+Infinity" => return Some(f64::INFINITY),
        "-Infinity" => return Some(f64::NEG_INFINITY),
        _ => {}
    }
    let valid = text.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
        && text.bytes().any(|b| b.is_ascii_digit());
    if !valid {
        return None;
    }
    text.parse::<f64>().ok()
}

/// A JS number as `String(n)` writes it in a schema message.
fn number_text(n: f64) -> String {
    json::text(&Json::Number(n))
}

/// How TS words a field's issues where the JSON schema does not say: a check's own message, or the identifier a
/// type error expects (a branded schema's, such as `RelativePath`).
#[derive(Debug, Clone, Copy)]
pub enum Wording {
    Message(&'static str),
    Expected(&'static str),
}

/// The issues of one field's value against its property schema, in Effect's check order.
fn check(value: &Json, property: &Json, wording: Option<Wording>) -> Vec<String> {
    let custom = match wording {
        Some(Wording::Message(message)) => Some(message),
        _ => None,
    };
    let ty = json::get_str(property, "type");
    if let Some(Json::Array(options)) = json::get(property, "enum") {
        if options.iter().any(|option| option == value) {
            return vec![];
        }
        return vec![format!("Expected {}", options.iter().map(json::text).collect::<Vec<_>>().join(" | "))];
    }
    let typed = match (ty, value) {
        (Some("string"), Json::String(_)) | (Some("boolean"), Json::Bool(_)) => true,
        (Some("number") | Some("integer"), Json::Number(_)) => true,
        (Some("object"), Json::Object(_)) | (Some("array"), Json::Array(_)) => true,
        (None, _) => true,
        _ => false,
    };
    if !typed {
        let expected = match (wording, ty) {
            (Some(Wording::Expected(expected)), _) => expected,
            (_, Some("integer")) => "number",
            (_, Some(other)) => other,
            (_, None) => "unknown",
        };
        return vec![format!("Expected {expected}")];
    }
    let mut issues = Vec::new();
    let message = |default: String| custom.map(str::to_owned).unwrap_or(default);
    match value {
        Json::Number(n) => {
            if ty == Some("integer") && !is_safe_integer(*n) {
                issues.push(message("Expected an integer".to_owned()));
            }
            let minimum = json::get_num(property, "minimum");
            let maximum = json::get_num(property, "maximum");
            if let (Some(minimum), Some(maximum)) = (minimum, maximum) {
                if *n < minimum || *n > maximum {
                    issues.push(message(format!("Expected a value between {} and {}", number_text(minimum), number_text(maximum))));
                }
            } else if let Some(minimum) = minimum {
                if *n < minimum {
                    issues.push(message(format!("Expected a value greater than or equal to {}", number_text(minimum))));
                }
            } else if let Some(maximum) = maximum {
                if *n > maximum {
                    issues.push(message(format!("Expected a value less than or equal to {}", number_text(maximum))));
                }
            }
            if let Some(minimum) = json::get_num(property, "exclusiveMinimum") {
                if *n <= minimum {
                    issues.push(message(format!("Expected a value greater than {}", number_text(minimum))));
                }
            }
        }
        Json::String(text) => {
            if let Some(minimum) = json::get_num(property, "minLength") {
                if (text.encode_utf16().count() as f64) < minimum {
                    issues.push(message(format!("Expected a value with a length of at least {}", number_text(minimum))));
                }
            }
        }
        _ => {}
    }
    issues
}

/// `formatInputIssues`.
fn invalid(tool: &str, issues: &[(String, String)], value: &Json) -> String {
    let mut details: Vec<String> = issues.iter().take(5).map(|(path, message)| format!("- {path}: {message}")).collect();
    if issues.len() > 5 {
        let more = issues.len() - 5;
        details.push(format!("- ...and {more} more {}", if more == 1 { "issue" } else { "issues" }));
    }
    let mut provided = String::new();
    pretty(value, 0, &mut provided);
    format!(
        "Invalid arguments for tool \"{tool}\":\n{}\n\nArguments provided:\n{provided}\n\nUpdate the arguments and call the tool again.",
        details.join("\n")
    )
}

/// `JSON.stringify(value, null, 2)`.
pub fn pretty(value: &Json, indent: usize, out: &mut String) {
    let pad = |n: usize| "  ".repeat(n);
    match value {
        Json::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                pretty(item, indent + 1, out);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(indent));
            out.push(']');
        }
        Json::Object(fields) if !fields.is_empty() => {
            out.push_str("{\n");
            for (i, (key, item)) in fields.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                out.push_str(&json::text(&json::string(key)));
                out.push_str(": ");
                pretty(item, indent + 1, out);
                out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(indent));
            out.push('}');
        }
        other => out.push_str(&json::text(other)),
    }
}

/// Repairs and decodes a call's input against the tool's definition: the decoded (repaired) input, or the
/// "Invalid arguments" tool error. `wording` names the fields TS words differently.
pub fn decode(definition: &Definition, input: &Json, wording: &[(&str, Wording)]) -> Result<Json, Failure> {
    let schema = &definition.parameters;
    let value = repair(input, schema);
    let mut issues: Vec<(String, String)> = Vec::new();
    match &value {
        Json::Object(fields) => {
            let empty = json::object();
            let properties = json::get(schema, "properties").unwrap_or(&empty);
            let required: Vec<&str> = json::array(json::get(schema, "required").unwrap_or(&Json::Null))
                .map(|items| items.iter().filter_map(json::as_str).collect())
                .unwrap_or_default();
            if let Json::Object(properties) = properties {
                for (key, property) in properties {
                    match fields.get(key) {
                        None if required.contains(&key.as_str()) => issues.push((key.clone(), "Missing key".into())),
                        None => {}
                        Some(item) => {
                            let custom = wording.iter().find(|(field, _)| field == key).map(|(_, wording)| *wording);
                            issues.extend(check(item, property, custom).into_iter().map(|message| (key.clone(), message)));
                        }
                    }
                }
            }
        }
        _ => issues.push(("root".into(), "Expected object".into())),
    }
    if issues.is_empty() {
        return Ok(value);
    }
    Err(Failure::Error(ToolError::new(invalid(&definition.name, &issues, &value))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> Definition {
        super::super::definition("read", include_str!("definitions/read.json"))
    }

    fn message(result: Result<Json, Failure>) -> String {
        match result {
            Err(Failure::Error(error)) => error.message,
            _ => panic!("expected an error"),
        }
    }

    #[test]
    fn missing_and_closed() {
        let input = json::parse(r#"{"filePath":"README.md"}"#).unwrap();
        assert_eq!(
            message(decode(&definition(), &input, &[])),
            "Invalid arguments for tool \"read\":\n- path: Missing key\n\nArguments provided:\n{}\n\nUpdate the arguments and call the tool again."
        );
    }

    #[test]
    fn minimum() {
        let input = json::parse(r#"{"path":"many.txt","offset":-1}"#).unwrap();
        assert_eq!(
            message(decode(&definition(), &input, &[])),
            "Invalid arguments for tool \"read\":\n- offset: Expected a value greater than or equal to 0\n\nArguments provided:\n{\n  \"path\": \"many.txt\",\n  \"offset\": -1\n}\n\nUpdate the arguments and call the tool again."
        );
    }

    #[test]
    fn repairs() {
        let input = json::parse(r#"{"path":"a","offset":"2","limit":null}"#).unwrap();
        assert_eq!(json::text(&decode(&definition(), &input, &[]).ok().unwrap()), r#"{"path":"a","offset":2}"#);
        let stringified = json::string(r#"{"path":"a"}"#);
        assert_eq!(json::text(&decode(&definition(), &stringified, &[]).ok().unwrap()), r#"{"path":"a"}"#);
    }
}
