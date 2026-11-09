//! `util/form.ts`: a form field's label, rows, initial values, validation and display, on the fields' JSON
//! (`FormField`) and answers (`FormValue`: a string, number, boolean or list of strings).
//!
//! Divergences: `new URL(value)` and `new Date(value)` (the `uri` and `date-time` formats) are approximations of
//! the WHATWG URL parser and V8's date parser: a scheme (and a host for the special schemes), and the ISO forms
//! and a few common ones. TODO(port-tui): neither is in the conformance standard.
use std::collections::HashMap;

use opencode_protocol::Json;
use opencode_tui_markup::js::{JsRegex, JsString};
use opencode_tui_sync::js;
use opencode_tui_sync::json::JsonExt;

/// `FormRow`.
#[derive(Clone, Debug, PartialEq)]
pub struct FormRow {
    pub value: Json,
    pub label: String,
    pub description: Option<String>,
}

pub fn field_type(field: &Json) -> &str {
    field.str("type").unwrap_or("")
}

pub fn field_key(field: &Json) -> &str {
    field.str("key").unwrap_or("")
}

/// `isFormAnswerField(field)`.
pub fn is_answer_field(field: &Json) -> bool {
    field_type(field) != "external"
}

/// `formLabel(field)`.
pub fn form_label(field: &Json) -> String {
    match field.str("title") {
        Some(title) => title.to_string(),
        None => {
            if field_type(field) == "external" {
                field.str("url").unwrap_or("").to_string()
            } else {
                field_key(field).to_string()
            }
        }
    }
}

fn options(field: &Json) -> Option<&Vec<Json>> {
    match field.get("options") {
        Some(Json::Array(items)) => Some(items),
        _ => None,
    }
}

/// `formInitialValues(fields)`: the answer fields' defaults, and the custom text of a string default that is not
/// one of its options.
pub fn initial_values(fields: &[Json]) -> (HashMap<String, Json>, HashMap<String, String>) {
    let mut answers = HashMap::new();
    let mut custom = HashMap::new();
    for field in fields {
        if is_answer_field(field) {
            if let Some(default) = field.get("default") {
                answers.insert(field_key(field).to_string(), default.clone());
            }
        }
        if field_type(field) != "string" || field.get("custom") != Some(&Json::Bool(true)) {
            continue;
        }
        let (Some(options), Some(Json::String(default))) = (options(field), field.get("default")) else { continue };
        if options.iter().any(|o| o.str("value") == Some(default.as_str())) {
            continue;
        }
        custom.insert(field_key(field).to_string(), default.clone());
    }
    (answers, custom)
}

/// `formTextual(field)`.
pub fn textual(field: Option<&Json>) -> bool {
    let Some(field) = field else { return false };
    match field_type(field) {
        "number" | "integer" => true,
        "string" => options(field).is_none(),
        _ => false,
    }
}

/// `formCustom(field)`.
pub fn custom(field: Option<&Json>) -> bool {
    let Some(field) = field else { return false };
    let flag = field.get("custom") == Some(&Json::Bool(true));
    if field_type(field) == "string" && options(field).is_some() {
        return flag;
    }
    field_type(field) == "multiselect" && flag
}

/// `formRows(field)`.
pub fn rows(field: Option<&Json>) -> Vec<FormRow> {
    let Some(field) = field else { return Vec::new() };
    if field_type(field) == "boolean" {
        return vec![
            FormRow { value: Json::Bool(true), label: "Yes".into(), description: None },
            FormRow { value: Json::Bool(false), label: "No".into(), description: None },
        ];
    }
    if !matches!(field_type(field), "multiselect" | "string") {
        return Vec::new();
    }
    options(field)
        .map(|items| {
            items
                .iter()
                .map(|option| FormRow {
                    value: option.get("value").cloned().unwrap_or(Json::Null),
                    label: option.str("label").unwrap_or("").to_string(),
                    description: option.str("description").map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `formSelected(field, value)`.
pub fn selected(field: Option<&Json>, value: Option<&Json>) -> usize {
    let (Some(field), Some(value)) = (field, value) else { return 0 };
    if matches!(value, Json::Array(_)) {
        return 0;
    }
    let rows = rows(Some(field));
    if let Some(index) = rows.iter().position(|row| &row.value == value) {
        return index;
    }
    if matches!(value, Json::String(_)) && custom(Some(field)) {
        return rows.len();
    }
    0
}

fn num(field: &Json, key: &str) -> Option<f64> {
    field.num(key)
}

/// `formValidateValue(field, value)`.
pub fn validate_value(field: &Json, value: Option<&Json>) -> Option<String> {
    let required = field.get("required") == Some(&Json::Bool(true));
    let kind = field_type(field);
    let Some(value) = value else { return required.then(|| "Answer required".to_string()) };
    let empty = matches!(value, Json::String(s) if s.is_empty()) || matches!(value, Json::Array(a) if a.is_empty());
    if required && empty {
        return Some(if kind == "multiselect" { "Select at least one option" } else { "Answer required" }.into());
    }
    if kind == "string" {
        let Json::String(text) = value else { return Some("Expected text".into()) };
        let length = js::length(text) as f64;
        if let Some(min) = num(field, "minLength") {
            if length < min {
                return Some(format!("Must be at least {} characters", js::number(min)));
            }
        }
        if let Some(max) = num(field, "maxLength") {
            if length > max {
                return Some(format!("Must be at most {} characters", js::number(max)));
            }
        }
        if let Some(pattern) = field.str("pattern") {
            match JsRegex::new(pattern, "") {
                Ok(regex) => {
                    if !regex.test(JsString::from(text.as_str()).as_slice()) {
                        return Some(format!("Must match pattern: {pattern}"));
                    }
                }
                Err(_) => return Some(format!("Invalid pattern: {pattern}")),
            }
        }
        match field.str("format") {
            Some("email") if !valid_email(text) => return Some("Expected an email address".into()),
            Some("uri") if !valid_url(text) => return Some("Expected a URL".into()),
            Some("date") if !valid_date(text) => return Some("Expected a date (YYYY-MM-DD)".into()),
            Some("date-time") if !valid_date_time(text) => return Some("Expected a date and time".into()),
            _ => {}
        }
        if let Some(options) = options(field) {
            let custom = field.get("custom") == Some(&Json::Bool(true));
            if !custom && !options.iter().any(|o| o.get("value") == Some(value)) {
                return Some("Select an available option".into());
            }
        }
        return None;
    }
    if kind == "number" || kind == "integer" {
        let Json::Number(n) = value else { return Some("Expected a number".into()) };
        if !n.is_finite() {
            return Some("Expected a number".into());
        }
        if kind == "integer" && n.fract() != 0.0 {
            return Some("Expected an integer".into());
        }
        if let Some(min) = num(field, "minimum") {
            if *n < min {
                return Some(format!("Must be at least {}", js::number(min)));
            }
        }
        if let Some(max) = num(field, "maximum") {
            if *n > max {
                return Some(format!("Must be at most {}", js::number(max)));
            }
        }
        return None;
    }
    if kind == "boolean" {
        return if matches!(value, Json::Bool(_)) { None } else { Some("Expected yes or no".into()) };
    }
    let Json::Array(items) = value else { return Some("Expected selections".into()) };
    if let Some(min) = num(field, "minItems") {
        if (items.len() as f64) < min {
            return Some(format!("Select at least {}", js::number(min)));
        }
    }
    if let Some(max) = num(field, "maxItems") {
        if (items.len() as f64) > max {
            return Some(format!("Select at most {}", js::number(max)));
        }
    }
    let custom = field.get("custom") == Some(&Json::Bool(true));
    if !custom {
        let options = options(field).cloned().unwrap_or_default();
        if items.iter().any(|item| !options.iter().any(|o| o.get("value") == Some(item))) {
            return Some("Select only available options".into());
        }
    }
    None
}

/// `formDisplayValue(field, value, emptyMultiselect)`.
pub fn display_value(field: &Json, value: Option<&Json>, empty_multiselect: &str) -> String {
    let Some(value) = value else { return String::new() };
    let rows = rows(Some(field));
    let label = |item: &Json| rows.iter().find(|row| &row.value == item).map(|row| row.label.clone()).unwrap_or_else(|| js::string(item));
    match value {
        Json::Array(items) if items.is_empty() => empty_multiselect.to_string(),
        Json::Array(items) => items.iter().map(label).collect::<Vec<_>>().join(", "),
        other => label(other),
    }
}

fn strings(value: Option<&Json>) -> Vec<Json> {
    match value {
        Some(Json::Array(items)) => items.clone(),
        _ => Vec::new(),
    }
}

/// `formToggleMultiselect(value, item)`.
pub fn toggle_multiselect(value: Option<&Json>, item: &str) -> Json {
    let mut values = strings(value);
    match values.iter().position(|v| v.as_str_value() == Some(item)) {
        Some(index) => {
            values.remove(index);
        }
        None => values.push(Json::String(item.to_string())),
    }
    Json::Array(values)
}

/// `formSetMultiselectCustom(value, previous, next)`.
pub fn set_multiselect_custom(value: Option<&Json>, previous: Option<&str>, next: &str) -> Json {
    let mut current = strings(value);
    if let Some(previous) = previous.filter(|p| !p.is_empty()) {
        if let Some(index) = current.iter().position(|v| v.as_str_value() == Some(previous)) {
            current.remove(index);
        }
    }
    if !next.is_empty() && !current.iter().any(|v| v.as_str_value() == Some(next)) {
        current.push(Json::String(next.to_string()));
    }
    Json::Array(current)
}

trait StrValue {
    fn as_str_value(&self) -> Option<&str>;
}

impl StrValue for Json {
    fn as_str_value(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }
}

/// `/^[^\s@]+@[^\s@]+\.[^\s@]+$/`.
fn valid_email(value: &str) -> bool {
    let ok = |c: char| !js::is_space(c) && c != '@';
    let Some((local, domain)) = value.split_once('@') else { return false };
    if local.is_empty() || !local.chars().all(ok) || !domain.chars().all(ok) {
        return false;
    }
    // `[^\s@]+\.[^\s@]+`: some dot with text on both sides.
    domain.char_indices().any(|(i, c)| c == '.' && i > 0 && i + 1 < domain.len())
}

/// `new URL(value)` succeeds (approximately: a scheme, and a host after `//` for the special schemes).
fn valid_url(value: &str) -> bool {
    let value = js::trim(value);
    let Some(colon) = value.find(':') else { return false };
    let scheme = &value[..colon];
    let mut chars = scheme.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) || !chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
        return false;
    }
    let rest = &value[colon + 1..];
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" | "ws" | "wss" | "ftp" => {
            let host = rest.trim_start_matches(['/', '\\']);
            let host = host.split(['/', '\\', '?', '#']).next().unwrap_or("");
            let host = host.rsplit('@').next().unwrap_or("");
            !host.is_empty() && !host.starts_with(':') && !host.contains(' ')
        }
        _ => true,
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// `validDate(value)`: `YYYY-MM-DD` that round-trips through `Date`.
fn valid_date(value: &str) -> bool {
    let b = value.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
    if !digits(&b[..4]) || !digits(&b[5..7]) || !digits(&b[8..]) {
        return false;
    }
    let year: i32 = value[..4].parse().unwrap_or(0);
    let month: u32 = value[5..7].parse().unwrap_or(0);
    let day: u32 = value[8..].parse().unwrap_or(0);
    (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month)
}

/// `!Number.isNaN(new Date(value).getTime())`, for the ISO forms (`YYYY`, `YYYY-MM`, `YYYY-MM-DD`, with `THH:mm`,
/// seconds, fractions and a zone).
fn valid_date_time(value: &str) -> bool {
    let (date, time) = match value.split_once(['T', 't', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (value, None),
    };
    let parts: Vec<&str> = date.split('-').collect();
    let year_ok = parts.first().is_some_and(|y| y.len() == 4 && y.bytes().all(|b| b.is_ascii_digit()));
    if !year_ok || parts.len() > 3 {
        return false;
    }
    let month: u32 = match parts.get(1) {
        Some(m) if m.len() == 2 => m.parse().unwrap_or(0),
        Some(_) => return false,
        None => 1,
    };
    let day: u32 = match parts.get(2) {
        Some(d) if d.len() == 2 => d.parse().unwrap_or(0),
        Some(_) => return false,
        None => 1,
    };
    let year: i32 = parts[0].parse().unwrap_or(0);
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return false;
    }
    let Some(time) = time else { return true };
    let time = time.trim_end_matches(['Z', 'z']);
    let time = match time.rfind(['+', '-']) {
        Some(index) if index >= 5 => &time[..index],
        _ => time,
    };
    let fields: Vec<&str> = time.split(':').collect();
    if fields.len() < 2 || fields.len() > 3 {
        return false;
    }
    let hour: u32 = fields[0].parse().unwrap_or(99);
    let minute: u32 = fields[1].parse().unwrap_or(99);
    let second_ok = fields.get(2).is_none_or(|s| {
        let whole = s.split('.').next().unwrap_or("");
        whole.len() == 2 && whole.parse::<u32>().is_ok_and(|v| v < 60)
    });
    hour < 24 && minute < 60 && fields[0].len() == 2 && fields[1].len() == 2 && second_ok
}

/// `Number(text)`: JS's string-to-number conversion.
pub fn js_number(text: &str) -> f64 {
    let text = js::trim(text);
    if text.is_empty() {
        return 0.0;
    }
    let lower = text.to_ascii_lowercase();
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(digits) = lower.strip_prefix(prefix) {
            if digits.is_empty() {
                return f64::NAN;
            }
            return match u128::from_str_radix(digits, radix) {
                Ok(v) => v as f64,
                Err(_) => f64::NAN,
            };
        }
    }
    match text {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    let valid = text.bytes().all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b)) && !lower.contains("inf") && !lower.contains("nan");
    if !valid {
        return f64::NAN;
    }
    text.parse::<f64>().unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(text: &str) -> Json {
        opencode_protocol::wire::json::parse(text).unwrap()
    }

    #[test]
    fn validation() {
        let integer = field(r#"{"key":"replicas","type":"integer","title":"Replicas","minimum":1,"maximum":5}"#);
        assert_eq!(validate_value(&integer, Some(&Json::Number(9.0))).as_deref(), Some("Must be at most 5"));
        assert_eq!(validate_value(&integer, Some(&Json::Number(3.0))), None);
        let email = field(r#"{"key":"email","type":"string","format":"email"}"#);
        assert_eq!(validate_value(&email, Some(&Json::String("not-an-email".into()))).as_deref(), Some("Expected an email address"));
        assert_eq!(validate_value(&email, Some(&Json::String("ops@example.com".into()))), None);
        let date = field(r#"{"key":"date","type":"string","format":"date","required":true}"#);
        assert_eq!(validate_value(&date, Some(&Json::String("2026-02-30x".into()))).as_deref(), Some("Expected a date (YYYY-MM-DD)"));
        assert_eq!(validate_value(&date, Some(&Json::String("2026-02-30".into()))).as_deref(), Some("Expected a date (YYYY-MM-DD)"));
        assert_eq!(validate_value(&date, Some(&Json::String("2026-03-01".into()))), None);
        assert_eq!(validate_value(&date, None).as_deref(), Some("Answer required"));
        assert_eq!(js_number("0.5"), 0.5);
        assert!(js_number("1x").is_nan());
        assert_eq!(js_number(" 9 "), 9.0);
    }
}
