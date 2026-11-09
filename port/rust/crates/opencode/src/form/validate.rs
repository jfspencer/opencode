//! `validateFields` and `validateAnswer` (packages/core/src/form.ts): a form's definition is checked when it is
//! created (unique keys; each `when` condition names an earlier field and fits its type and options), an answer when
//! it is given (known keys, acknowledged external fields, required and active fields, each value against its field).
//! Both work on the canonical JSON the schemas decoded (`Form.Field`, `Form.Answer`).

use std::collections::{HashMap, HashSet};

use opencode_protocol::Json;

use super::formats;
use crate::json;

fn get<'a>(field: &'a Json, key: &str) -> Option<&'a Json> {
    json::get(field, key)
}

fn ty(field: &Json) -> &str {
    json::get_str(field, "type").unwrap_or_default()
}

fn key(field: &Json) -> &str {
    json::get_str(field, "key").unwrap_or_default()
}

fn flag(field: &Json, name: &str) -> bool {
    json::get_bool(field, name) == Some(true)
}

fn options(field: &Json) -> Option<&[Json]> {
    get(field, "options").and_then(json::array)
}

fn has_option(field: &Json, value: &Json) -> bool {
    options(field).is_some_and(|options| options.iter().any(|option| get(option, "value").is_some_and(|v| strict_eq(v, value))))
}

fn conditions(field: &Json) -> &[Json] {
    get(field, "when").and_then(json::array).unwrap_or_default()
}

/// JS `===` between JSON values (primitives compare by value, `NaN` is never equal; arrays and objects by identity,
/// which distinct decoded values never share).
fn strict_eq(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::String(a), Json::String(b)) => a == b,
        (Json::Number(a), Json::Number(b)) => a == b,
        (Json::Bool(a), Json::Bool(b)) => a == b,
        (Json::Null, Json::Null) => true,
        _ => false,
    }
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `validateFields`: the first problem of a form definition, if any.
pub fn validate_fields(fields: &[Json]) -> Option<String> {
    if fields.is_empty() {
        return Some("Form must have at least one field".into());
    }
    let mut earlier: HashMap<&str, &Json> = HashMap::new();
    let mut keys: HashSet<&str> = HashSet::new();
    for field in fields {
        let name = key(field);
        if !keys.insert(name) {
            return Some(format!("Duplicate form field key: {name}"));
        }
        if ty(field) == "external" {
            continue;
        }
        for when in conditions(field) {
            let target_key = json::get_str(when, "key").unwrap_or_default();
            let Some(target) = earlier.get(target_key) else {
                return Some(format!("Form field condition must reference an earlier field: {name} -> {target_key}"));
            };
            if let Some(invalid) = validate_when(when, target) {
                return Some(format!("{invalid}: {name} -> {target_key}"));
            }
        }
        earlier.insert(name, field);
    }
    None
}

fn validate_when(when: &Json, target: &Json) -> Option<&'static str> {
    let value = get(when, "value").unwrap_or(&Json::Null);
    match ty(target) {
        "boolean" => (!matches!(value, Json::Bool(_))).then_some("Form field condition value must be a boolean"),
        "number" | "integer" => (!matches!(value, Json::Number(_))).then_some("Form field condition value must be a number"),
        target_type => {
            // string and multiselect targets both compare against string values
            if !matches!(value, Json::String(_)) {
                return Some("Form field condition value must be a string");
            }
            let closed = if target_type == "multiselect" {
                !flag(target, "custom")
            } else {
                options(target).is_some() && !flag(target, "custom")
            };
            (closed && !has_option(target, value)).then_some("Form field condition value must be one of the field's options")
        }
    }
}

/// `validateAnswer`: the first problem of an answer to the form's fields, if any.
pub fn validate_answer(fields: &[Json], answer: &Json) -> Option<String> {
    let empty = Default::default();
    let answer = answer.as_object().unwrap_or(&empty);
    let known: HashSet<&str> = fields.iter().map(key).collect();
    if let Some(unknown) = answer.keys().find(|k| !known.contains(k.as_str())) {
        return Some(format!("Unknown form field: {unknown}"));
    }
    for field in fields {
        let name = key(field);
        let value = answer.get(name);
        if ty(field) == "external" {
            if value != Some(&Json::Bool(true)) {
                return Some(format!("External form field must be acknowledged: {name}"));
            }
            continue;
        }
        let active = conditions(field).iter().all(|when| matches_when(when, answer.get(json::get_str(when, "key").unwrap_or_default())));
        let Some(value) = value else {
            if flag(field, "required") && active {
                return Some(format!("Missing required form field: {name}"));
            }
            continue;
        };
        if !active {
            return Some(format!("Form field is not active: {name}"));
        }
        if let Some(invalid) = validate_field(field, value) {
            return Some(invalid);
        }
    }
    None
}

/// `matches`: an unanswered referenced field makes the condition false for both ops.
fn matches_when(when: &Json, value: Option<&Json>) -> bool {
    let Some(value) = value else { return false };
    let target = get(when, "value").unwrap_or(&Json::Null);
    let hit = match value {
        Json::Array(items) => items.iter().any(|item| strict_eq(item, target)),
        value => strict_eq(value, target),
    };
    if json::get_str(when, "op") == Some("eq") { hit } else { !hit }
}

fn number(field: &Json, name: &str) -> Option<f64> {
    json::get_num(field, name)
}

fn validate_field(field: &Json, value: &Json) -> Option<String> {
    let name = key(field);
    match ty(field) {
        "string" => {
            let Json::String(text) = value else { return Some(format!("Expected string for form field: {name}")) };
            let length = utf16_len(text) as f64;
            if flag(field, "required") && text.is_empty() {
                return Some(format!("Missing required form field: {name}"));
            }
            if number(field, "minLength").is_some_and(|min| length < min) {
                return Some(format!("Form field is too short: {name}"));
            }
            if number(field, "maxLength").is_some_and(|max| length > max) {
                return Some(format!("Form field is too long: {name}"));
            }
            if let Some(pattern) = json::get_str(field, "pattern") {
                match formats::js_regex(pattern) {
                    None => return Some(format!("Form field has invalid pattern: {name}")),
                    Some(regex) if !regex.is_match(text) => return Some(format!("Form field does not match pattern: {name}")),
                    Some(_) => {}
                }
            }
            let invalid = match json::get_str(field, "format") {
                Some("email") if !formats::email(text) => Some("email"),
                Some("uri") if !formats::url_can_parse(text) => Some("URI"),
                Some("date") if !formats::date(text) => Some("date"),
                Some("date-time") if !formats::date_time(text) => Some("date-time"),
                _ => None,
            };
            if let Some(format) = invalid {
                return Some(format!("Expected {format} for form field: {name}"));
            }
            if options(field).is_some() && !flag(field, "custom") && !has_option(field, value) {
                return Some(format!("Invalid option for form field: {name}"));
            }
            None
        }
        "number" | "integer" => {
            let n = match value {
                Json::Number(n) if n.is_finite() => *n,
                _ => return Some(format!("Expected number for form field: {name}")),
            };
            if ty(field) == "integer" && n.fract() != 0.0 {
                return Some(format!("Expected integer for form field: {name}"));
            }
            if number(field, "minimum").is_some_and(|min| n < min) {
                return Some(format!("Form field is too small: {name}"));
            }
            if number(field, "maximum").is_some_and(|max| n > max) {
                return Some(format!("Form field is too large: {name}"));
            }
            None
        }
        "boolean" => (!matches!(value, Json::Bool(_))).then(|| format!("Expected boolean for form field: {name}")),
        "multiselect" => {
            let items = match value {
                Json::Array(items) if items.iter().all(|item| matches!(item, Json::String(_))) => items,
                _ => return Some(format!("Expected string array for form field: {name}")),
            };
            let count = items.len() as f64;
            if flag(field, "required") && items.is_empty() {
                return Some(format!("Missing required form field: {name}"));
            }
            if number(field, "minItems").is_some_and(|min| count < min) {
                return Some(format!("Too few selections for form field: {name}"));
            }
            if number(field, "maxItems").is_some_and(|max| count > max) {
                return Some(format!("Too many selections for form field: {name}"));
            }
            if !flag(field, "custom") && items.iter().any(|item| !has_option(field, item)) {
                return Some(format!("Invalid option for form field: {name}"));
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(text: &str) -> Vec<Json> {
        json::array(&json::parse(text).unwrap()).unwrap().to_vec()
    }

    #[test]
    fn definitions() {
        assert_eq!(validate_fields(&fields(r#"[{"key":"a","type":"boolean"},{"key":"a","type":"string"}]"#)).unwrap(), "Duplicate form field key: a");
        assert_eq!(
            validate_fields(&fields(r#"[{"key":"b","type":"string","when":[{"key":"later","op":"eq","value":"x"}]},{"key":"later","type":"string"}]"#)).unwrap(),
            "Form field condition must reference an earlier field: b -> later"
        );
        assert_eq!(
            validate_fields(&fields(r#"[{"key":"pick","type":"string","options":[{"value":"a","label":"A"}]},{"key":"x","type":"string","when":[{"key":"pick","op":"eq","value":"z"}]}]"#)).unwrap(),
            "Form field condition value must be one of the field's options: x -> pick"
        );
        assert_eq!(validate_fields(&fields(r#"[{"key":"x","type":"boolean"}]"#)), None);
    }

    #[test]
    fn answers() {
        let form = fields(
            r#"[{"key":"env","type":"string","options":[{"value":"prod","label":"P"}],"required":true},
                {"key":"replicas","type":"integer","minimum":1,"maximum":5,"when":[{"key":"env","op":"eq","value":"prod"}]},
                {"key":"confirm","type":"boolean","required":true}]"#,
        );
        let check = |answer: &str| validate_answer(&form, &json::parse(answer).unwrap());
        assert_eq!(check(r#"{"env":"prod"}"#).unwrap(), "Missing required form field: confirm");
        assert_eq!(check(r#"{"env":"prod","confirm":true,"unknown":1}"#).unwrap(), "Unknown form field: unknown");
        assert_eq!(check(r#"{"env":"prod","confirm":true,"replicas":9}"#).unwrap(), "Form field is too large: replicas");
        assert_eq!(check(r#"{"env":"prod","confirm":true,"replicas":2.5}"#).unwrap(), "Expected integer for form field: replicas");
        assert_eq!(check(r#"{"env":"prod","confirm":true}"#), None);
    }
}
