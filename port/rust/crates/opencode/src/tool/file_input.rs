//! A tool call's input as TS decodes it before running the tool: first repaired (plugin/tool-input-repair.ts, the
//! `execute.before` hook), then decoded against the tool's input schema (tool/runtime.ts `decodeInput`), whose
//! failure is the model-facing "Invalid arguments" error.
//!
//! The repair is tool-input-repair.ts's whole algorithm on the definition's JSON schema (local `$ref`s, type lists,
//! nullable and single-candidate compositions, stringified objects and arrays, tuples, typed dictionaries, single
//! values wrapped in an array). Its alias table (`bash` → `shell`) belongs to the registry, not to a tool.
//!
//! The checks follow the recorded JSON schema of the tool's definition, worded as Effect's Standard Schema formatter
//! words them (`errors: "all"`, no reported input), in Effect's order (a container's contents before its own checks):
//! string, boolean, integer and number fields with `minimum` / `exclusiveMinimum` / `maximum` / `minLength` /
//! `maxLength` / `pattern` (`isStartingWith`, `isPattern`) / `enum`, array items with `minItems` / `maxItems`, nested
//! objects, and local `$ref`s (recursive definitions, named by their identifier). `harness/tool-inputs/run.sh` checks
//! the repair and the texts against the reference on random inputs of every tool decoded here and of synthetic
//! schemas.
//!
//! What a JSON schema does not say comes from the caller: a branded or identified field's name (`Wording::Expected`,
//! the definition drops it) and a check's own message (`Wording::Message`). Compositions (`anyOf`, `oneOf`) are not
//! checked: Effect words a union from its members' ASTs (the candidates its index selects, the members' own expected
//! texts), which the JSON rendering does not keep, and no tool decoded here has one; such a tool decodes its input by
//! hand (question.rs).

use opencode_protocol::Json;

use super::arguments::Issue;
use super::{Definition, Failure};
use crate::json;

/// `repair` of tool-input-repair.ts for a root object schema.
pub fn repair(value: &Json, schema: &Json) -> Json {
    repair_value(value, schema, schema, 0)
}

const MAX_DEPTH: usize = 6;

/// JavaScript's `typeof` of a JSON value.
fn type_of(value: &Json) -> &'static str {
    match value {
        Json::String(_) => "string",
        Json::Number(_) => "number",
        Json::Bool(_) => "boolean",
        _ => "object",
    }
}

/// `Predicate.isObject`: a record, not an array.
fn is_record(value: &Json) -> bool {
    matches!(value, Json::Object(_))
}

fn repair_value(value: &Json, schema: &Json, root: &Json, depth: usize) -> Json {
    if depth > MAX_DEPTH {
        return value.clone();
    }
    if let Some(reference) = json::get_str(schema, "$ref") {
        return match local_reference(reference, root) {
            Some(target) => repair_value(value, target, root, depth + 1),
            None => value.clone(),
        };
    }
    if let Some(Json::Array(types)) = json::get(schema, "type") {
        let names: Vec<&str> = types.iter().filter_map(json::as_str).collect();
        if (*value == Json::Null && names.contains(&"null")) || names.contains(&type_of(value)) {
            return value.clone();
        }
        let rest: Vec<&Json> = types.iter().filter(|t| json::as_str(t) != Some("null")).collect();
        if rest.len() != 1 {
            return value.clone();
        }
        let mut single = schema.clone();
        json::set(&mut single, "type", rest[0].clone());
        return repair_value(value, &single, root, depth + 1);
    }
    let Some(ty) = json::get(schema, "type") else {
        let (any, one) = (json::get(schema, "anyOf"), json::get(schema, "oneOf"));
        if matches!(any, Some(Json::Array(_))) && matches!(one, Some(Json::Array(_))) {
            return value.clone();
        }
        let branches = match any.filter(|a| matches!(a, Json::Array(_))).or(one) {
            Some(Json::Array(branches)) if *value != Json::Null => branches,
            _ => return value.clone(),
        };
        if branches.iter().any(|branch| !is_record(branch) || json::get_str(branch, "type") == Some(type_of(value))) {
            return value.clone();
        }
        let candidates: Vec<&Json> = branches.iter().filter(|b| json::get_str(b, "type") != Some("null")).collect();
        return match candidates.as_slice() {
            [only] => repair_value(value, only, root, depth + 1),
            _ => value.clone(),
        };
    };
    match json::as_str(ty) {
        Some("number") | Some("integer") => {
            let Json::String(text) = value else { return value.clone() };
            if crate::text::trim(text).is_empty() {
                return value.clone();
            }
            match js_number(text) {
                Some(parsed) if parsed.is_finite() && (json::as_str(ty) != Some("integer") || is_safe_integer(parsed)) => {
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
        Some("object") => repair_object(value, schema, root, depth),
        Some("array") => repair_array(value, schema, root, depth),
        _ => value.clone(),
    }
}

/// A `#/$defs/<name>` or `#/definitions/<name>` reference's target in the root schema (JSON Pointer escapes undone).
fn local_reference<'a>(reference: &str, root: &'a Json) -> Option<&'a Json> {
    let (container, name) = [("#/$defs/", "$defs"), ("#/definitions/", "definitions")]
        .iter()
        .find_map(|(prefix, key)| reference.strip_prefix(prefix).filter(|n| !n.is_empty() && !n.contains('/')).map(|n| (*key, n)))?;
    let definitions = json::get(root, container).filter(|d| is_record(d))?;
    json::get(definitions, &name.replace("~1", "/").replace("~0", "~")).filter(|target| is_record(target))
}

/// `repairArray`: a stringified array parsed, its items (or a tuple's members) repaired, and a single compatible
/// value wrapped in an array.
fn repair_array(value: &Json, schema: &Json, root: &Json, depth: usize) -> Json {
    let parsed = match value {
        Json::String(text) => json::parse(text),
        other => Some(other.clone()),
    };
    let prefix = json::get(schema, "prefixItems").filter(|p| matches!(p, Json::Array(_)));
    let tuple = prefix.or_else(|| json::get(schema, "items").filter(|i| matches!(i, Json::Array(_))));
    let tuple = tuple.and_then(json::array);
    if let Some(Json::Array(items)) = &parsed {
        let repaired: Vec<Json> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let member = match tuple {
                    Some(tuple) => tuple.get(index).or_else(|| {
                        if prefix.is_some() { json::get(schema, "items") } else { json::get(schema, "additionalItems") }
                    }),
                    None => json::get(schema, "items"),
                };
                match member.filter(|m| is_record(m)) {
                    Some(member) => repair_value(item, member, root, depth + 1),
                    None => item.clone(),
                }
            })
            .collect();
        return Json::Array(repaired);
    }
    let Some(items) = json::get(schema, "items").filter(|i| is_record(i)).filter(|_| tuple.is_none()) else {
        return value.clone();
    };
    let repaired = repair_value(value, items, root, depth + 1);
    let compatible = match json::get_str(items, "type") {
        Some("object") => is_record(&repaired),
        Some("array") => matches!(repaired, Json::Array(_)),
        Some("integer") => matches!(repaired, Json::Number(n) if is_safe_integer(n)),
        Some("number") => matches!(repaired, Json::Number(n) if n.is_finite()),
        Some(ty @ ("string" | "boolean")) => type_of(&repaired) == ty,
        _ => false,
    };
    if compatible { Json::Array(vec![repaired]) } else { value.clone() }
}

fn repair_object(value: &Json, schema: &Json, root: &Json, depth: usize) -> Json {
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
        let repaired = repair_value(current, property, root, depth + 1);
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

/// Whether a value has the JSON type a schema names (any value when it names none).
fn typed(ty: Option<&str>, value: &Json) -> bool {
    match (ty, value) {
        (Some("string"), Json::String(_)) | (Some("boolean"), Json::Bool(_)) => true,
        (Some("number") | Some("integer"), Json::Number(_)) => true,
        (Some("object"), Json::Object(_)) | (Some("array"), Json::Array(_)) => true,
        (None, _) => true,
        _ => false,
    }
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
    if !typed(ty, value) {
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
            let length = text.encode_utf16().count() as f64;
            if let Some(minimum) = json::get_num(property, "minLength") {
                if length < minimum {
                    issues.push(message(format!("Expected a value with a length of at least {}", number_text(minimum))));
                }
            }
            if let Some(maximum) = json::get_num(property, "maxLength") {
                if length > maximum {
                    issues.push(message(format!("Expected a value with a length of at most {}", number_text(maximum))));
                }
            }
            if let Some(pattern) = json::get_str(property, "pattern") {
                // `isStartingWith(prefix)` writes `^<prefix escaped>`; any other pattern is `isPattern`'s RegExp.
                match starting_with(pattern) {
                    Some(prefix) if !text.starts_with(&prefix) => {
                        issues.push(message(format!("Expected a string starting with {}", json::text(&Json::String(prefix)))))
                    }
                    Some(_) => {}
                    None => {
                        let source: Vec<u16> = pattern.encode_utf16().collect();
                        let matches = opencode_codemode::jsregex::JsRegExp::new(&source, &[])
                            .map(|regex| regex.test(&text.encode_utf16().collect::<Vec<u16>>()))
                            .unwrap_or(true);
                        if !matches {
                            issues.push(message(format!("Expected a string matching the RegExp {pattern}")));
                        }
                    }
                }
            }
        }
        Json::Array(items) => {
            let length = items.len() as f64;
            if let Some(minimum) = json::get_num(property, "minItems") {
                if length < minimum {
                    issues.push(message(format!("Expected a value with a length of at least {}", number_text(minimum))));
                }
            }
            if let Some(maximum) = json::get_num(property, "maxItems") {
                if length > maximum {
                    issues.push(message(format!("Expected a value with a length of at most {}", number_text(maximum))));
                }
            }
        }
        _ => {}
    }
    issues
}

/// The prefix of a `^literal` pattern (RegExp escapes undone), as `isStartingWith` writes it.
fn starting_with(pattern: &str) -> Option<String> {
    let mut rest = pattern.strip_prefix('^')?.chars();
    let mut prefix = String::new();
    while let Some(c) = rest.next() {
        match c {
            '\\' => match rest.next()? {
                escaped if "^$\\.*+?()[]{}|/-".contains(escaped) => prefix.push(escaped),
                _ => return None,
            },
            c if "^$.*+?()[]{}|".contains(c) => return None,
            c => prefix.push(c),
        }
    }
    Some(prefix)
}

/// A value's issues against its property schema, each with its path below the value, in Effect's order: a value of
/// the wrong type is that alone; an array's or object's contents come before its own checks (an array's length is
/// checked only once its items decode). A local `$ref` is followed. The definitions left in `$defs` are recursive ones
/// (runtime.ts inlines the rest), written under their identifier, which a type error names: a `$ref` to one, or a
/// property inlined from one (equal to it).
fn issues(value: &Json, property: &Json, root: &Json, wording: Option<Wording>, depth: usize) -> Vec<(String, String)> {
    if let Some(reference) = json::get_str(property, "$ref") {
        let Some(target) = local_reference(reference, root).filter(|_| depth < 64) else { return vec![] };
        let name = reference.rsplit('/').next().unwrap_or_default().replace("~1", "/").replace("~0", "~");
        if !typed(json::get_str(target, "type"), value) {
            return vec![(String::new(), format!("Expected {name}"))];
        }
        return issues(value, target, root, wording, depth + 1);
    }
    if !typed(json::get_str(property, "type"), value) {
        let defined = match json::get(root, "$defs") {
            Some(Json::Object(definitions)) => definitions.iter().find(|(_, definition)| same(definition, property, root, 0)),
            _ => None,
        };
        if let (Some((name, _)), None) = (defined, wording) {
            return vec![(String::new(), format!("Expected {name}"))];
        }
    }
    let structured = matches!(value, Json::Array(_) | Json::Object(_)) && typed(json::get_str(property, "type"), value);
    if structured {
        let inner = nested(value, property, root, depth);
        if !inner.is_empty() {
            return inner;
        }
    }
    let own = check(value, property, wording);
    if !own.is_empty() || structured {
        return own.into_iter().map(|message| (String::new(), message)).collect();
    }
    nested(value, property, root, depth)
}

/// Whether two schemas are the same once local `$ref`s are expanded: runtime.ts inlines a recursive definition at its
/// first use, so the inlined copy and the definition differ only in which level keeps the `$ref`.
fn same(a: &Json, b: &Json, root: &Json, depth: usize) -> bool {
    if depth > 8 {
        return true;
    }
    let expand = |schema: &'_ Json| -> Option<Json> {
        json::get_str(schema, "$ref").and_then(|reference| local_reference(reference, root)).cloned()
    };
    match (expand(a), expand(b)) {
        (Some(a), Some(b)) => return same(&a, &b, root, depth + 1),
        (Some(a), None) => return same(&a, b, root, depth + 1),
        (None, Some(b)) => return same(a, &b, root, depth + 1),
        (None, None) => {}
    }
    match (a, b) {
        (Json::Object(a), Json::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(key, value)| b.get(key).is_some_and(|other| same(value, other, root, depth + 1)))
        }
        (Json::Array(a), Json::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x, y, root, depth + 1)),
        _ => a == b,
    }
}

/// The issues inside an array's items or an object's declared properties (`errors: "all"`, in order), each with its
/// path below the value (`[0].status`).
fn nested(value: &Json, property: &Json, root: &Json, depth: usize) -> Vec<(String, String)> {
    let mut found = Vec::new();
    match value {
        Json::Array(items) => {
            let Some(schema) = json::get(property, "items") else { return found };
            for (index, item) in items.iter().enumerate() {
                let inner = issues(item, schema, root, None, depth + 1);
                found.extend(inner.into_iter().map(|(suffix, message)| (format!("[{index}]{suffix}"), message)));
            }
        }
        Json::Object(fields) => {
            let Some(Json::Object(properties)) = json::get(property, "properties") else { return found };
            let required: Vec<&str> = json::array(json::get(property, "required").unwrap_or(&Json::Null))
                .map(|items| items.iter().filter_map(json::as_str).collect())
                .unwrap_or_default();
            for (key, schema) in properties {
                match fields.get(key) {
                    None if required.contains(&key.as_str()) => found.push((format!(".{key}"), "Missing key".into())),
                    None => {}
                    Some(item) => {
                        let inner = issues(item, schema, root, None, depth + 1);
                        found.extend(inner.into_iter().map(|(suffix, message)| (format!(".{key}{suffix}"), message)));
                    }
                }
            }
        }
        _ => {}
    }
    found
}

/// Repairs and decodes a call's input against the tool's definition: the decoded (repaired) input, or the
/// "Invalid arguments" tool error. `wording` names the fields TS words differently.
pub fn decode(definition: &Definition, input: &Json, wording: &[(&str, Wording)]) -> Result<Json, Failure> {
    let schema = &definition.parameters;
    let value = repair(input, schema);
    let mut found: Vec<(String, String)> = Vec::new();
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
                        None if required.contains(&key.as_str()) => found.push((key.clone(), "Missing key".into())),
                        None => {}
                        Some(item) => {
                            let custom = wording.iter().find(|(field, _)| field == key).map(|(_, wording)| *wording);
                            let inner = issues(item, property, schema, custom, 0);
                            found.extend(inner.into_iter().map(|(suffix, message)| (format!("{key}{suffix}"), message)));
                        }
                    }
                }
            }
        }
        _ => found.push((String::new(), "Expected object".into())),
    }
    if found.is_empty() {
        return Ok(value);
    }
    let issues: Vec<Issue> = found.iter().map(|(path, message)| Issue::new(path, message.as_str())).collect();
    Err(Failure::Error(super::arguments::invalid(&definition.name, &issues, &value)))
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

    /// Inputs recorded through the reference's repair and decode (`TOOL_INPUT_CASES=<file>`, see
    /// harness/tool-inputs/run.sh): the repaired value, and the "Invalid arguments" text or success.
    #[test]
    #[ignore]
    fn recorded_inputs() {
        let path = std::env::var("TOOL_INPUT_CASES").expect("TOOL_INPUT_CASES names the recorded cases");
        let cases = json::parse(&std::fs::read_to_string(path).unwrap()).expect("cases are JSON");
        let (mut total, mut wrong) = (0, Vec::new());
        for case in json::array(&cases).unwrap() {
            total += 1;
            let tool = json::get_str(case, "tool").unwrap().to_owned();
            let parameters = json::get(case, "parameters").unwrap().clone();
            let input = json::get(case, "input").cloned().unwrap_or(Json::Null);
            let repaired = repair(&input, &parameters);
            let expected = json::get(case, "repaired").cloned().unwrap_or(Json::Null);
            if json::text(&repaired) != json::text(&expected) {
                wrong.push(format!("{tool} repair of {}: rust {} ts {}", json::text(&input), json::text(&repaired), json::text(&expected)));
                continue;
            }
            let Some(outcome) = json::get(case, "outcome") else { continue };
            let wording: Vec<(&str, Wording)> = json::array(json::get(case, "wording").unwrap())
                .unwrap()
                .iter()
                .map(|entry| {
                    let items = json::array(entry).unwrap();
                    let field: &'static str = Box::leak(json::as_str(&items[0]).unwrap().to_owned().into_boxed_str());
                    let text = |key: &str| json::get_str(&items[1], key).map(|t| &*Box::leak(t.to_owned().into_boxed_str()));
                    let wording = match (text("message"), text("expected")) {
                        (Some(message), _) => Wording::Message(message),
                        (None, expected) => Wording::Expected(expected.unwrap()),
                    };
                    (field, wording)
                })
                .collect();
            let definition = Definition { name: tool.clone(), description: String::new(), parameters };
            let got = match decode(&definition, &input, &wording) {
                Ok(_) => None,
                Err(Failure::Error(error)) => Some(error.message),
                Err(_) => Some("<other failure>".to_owned()),
            };
            let want = json::get_str(outcome, "error").map(str::to_owned);
            if got != want {
                wrong.push(format!("{tool} decode of {}:\n  rust {got:?}\n  ts   {want:?}", json::text(&input)));
            }
        }
        eprintln!("{total} cases, {} differences", wrong.len());
        assert!(wrong.is_empty(), "{}", wrong.iter().take(15).cloned().collect::<Vec<_>>().join("\n"));
    }

    #[test]
    fn repairs() {
        let input = json::parse(r#"{"path":"a","offset":"2","limit":null}"#).unwrap();
        assert_eq!(json::text(&decode(&definition(), &input, &[]).ok().unwrap()), r#"{"path":"a","offset":2}"#);
        let stringified = json::string(r#"{"path":"a"}"#);
        assert_eq!(json::text(&decode(&definition(), &stringified, &[]).ok().unwrap()), r#"{"path":"a"}"#);
    }
}
