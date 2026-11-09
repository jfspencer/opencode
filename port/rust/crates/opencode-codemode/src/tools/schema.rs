//! Model-visible TypeScript signatures from JSON Schema (tool-schema.ts `renderSchema`, `jsdoc`).

use crate::json::{Json, JsonObj};
use std::collections::HashMap;

const MAX_RENDER_DEPTH: usize = 8;

fn get<'a>(schema: &'a JsonObj, key: &str) -> Option<&'a Json> {
    schema.get(key)
}

fn render_literal(value: &Json) -> String {
    value.to_json_string()
}

pub fn is_identifier_segment(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn render_key(name: &str) -> String {
    if is_identifier_segment(name) { name.to_string() } else { Json::str(name).to_json_string() }
}

fn decode_pointer_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// JsonPointer.parseUriFragment for `#/$defs/Name`: the two tokens when it is a definitions reference.
fn definition_name(reference: &str) -> Option<String> {
    let fragment = reference.strip_prefix('#')?;
    if fragment.is_empty() {
        return None;
    }
    let fragment = fragment.strip_prefix('/')?;
    let decoded: String = percent_decode(fragment)?;
    let tokens: Vec<String> = decoded.split('/').map(decode_pointer_token).collect();
    if tokens.len() == 2 && (tokens[0] == "$defs" || tokens[0] == "definitions") { Some(tokens[1].clone()) } else { None }
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn is_type(schema: &JsonObj, name: &str) -> bool {
    matches!(get(schema, "type"), Some(Json::Str(t)) if t.eq_str(name))
}

fn effect_number_sentinel(schema: &JsonObj) -> bool {
    is_type(schema, "string")
        && matches!(get(schema, "enum"), Some(Json::Arr(values)) if !values.is_empty()
            && values.iter().all(|v| matches!(v, Json::Str(s) if s.eq_str("NaN") || s.eq_str("Infinity") || s.eq_str("-Infinity"))))
}

const ANNOTATIONS: [&str; 6] = ["title", "description", "default", "examples", "readOnly", "writeOnly"];

fn is_effect_empty_struct(schema: &JsonObj) -> bool {
    let Some(Json::Obj(not)) = get(schema, "not") else { return false };
    is_type(not, "null") && not.len() == 1 && schema.iter().all(|(k, _)| k.eq_str("not") || ANNOTATIONS.iter().any(|a| k.eq_str(a)))
}

fn intersection(members: Vec<String>) -> String {
    let concrete: Vec<String> = members.into_iter().filter(|m| m != "unknown").collect();
    match concrete.len() {
        0 => "unknown".into(),
        1 => concrete.into_iter().next().unwrap(),
        _ => concrete.iter().map(|m| if m.contains(" | ") { format!("({m})") } else { m.clone() }).collect::<Vec<_>>().join(" & "),
    }
}

type Defs = HashMap<String, JsonObj>;

fn obj_list<'a>(schema: &'a JsonObj, key: &str) -> Option<Vec<&'a JsonObj>> {
    match get(schema, key) {
        Some(Json::Arr(items)) => Some(items.iter().filter_map(|i| i.as_obj()).collect()),
        _ => None,
    }
}

fn has_unresolved_ref(schema: &JsonObj, definitions: &Defs, seen: &[String], visited: &mut Vec<*const JsonObj>) -> bool {
    let me = schema as *const JsonObj;
    if visited.contains(&me) {
        return false;
    }
    visited.push(me);
    if let Some(Json::Str(reference)) = get(schema, "$ref") {
        let name = definition_name(&reference.to_rust_lossy());
        match name {
            Some(name) if definitions.contains_key(&name) && !seen.contains(&name) => {
                let mut next = seen.to_vec();
                next.push(name.clone());
                if has_unresolved_ref(&definitions[&name], definitions, &next, &mut visited.clone()) {
                    return true;
                }
            }
            _ => return true,
        }
    }
    let mut children: Vec<&JsonObj> = Vec::new();
    for key in ["anyOf", "oneOf", "allOf"] {
        children.extend(obj_list(schema, key).unwrap_or_default());
    }
    if let Some(Json::Obj(properties)) = get(schema, "properties") {
        children.extend(properties.iter().filter_map(|(_, v)| v.as_obj()));
    }
    if let Some(Json::Obj(items)) = get(schema, "items") {
        children.push(items);
    }
    if let Some(Json::Obj(additional)) = get(schema, "additionalProperties") {
        children.push(additional);
    }
    let snapshot = visited.clone();
    children.into_iter().any(|child| has_unresolved_ref(child, definitions, seen, &mut snapshot.clone()))
}

fn number_text(value: f64) -> String {
    crate::numconv::to_string(value)
}

fn doc_tags(schema: &JsonObj) -> Vec<String> {
    let mut tags = Vec::new();
    if matches!(get(schema, "deprecated"), Some(Json::Bool(true))) {
        tags.push("@deprecated".into());
    }
    if let Some(default) = get(schema, "default") {
        tags.push(format!("@default {}", default.to_json_string()));
    }
    if let Some(Json::Str(format)) = get(schema, "format") {
        tags.push(format!("@format {format}"));
    }
    let integer = is_type(schema, "integer");
    if integer {
        tags.push("@integer".into());
    }
    if let Some(Json::Num(min)) = get(schema, "minimum")
        && !(integer && *min == -9007199254740991.0)
    {
        tags.push(format!("@minimum {}", number_text(*min)));
    }
    if let Some(Json::Num(max)) = get(schema, "maximum")
        && !(integer && *max == 9007199254740991.0)
    {
        tags.push(format!("@maximum {}", number_text(*max)));
    }
    for (key, tag) in [
        ("exclusiveMinimum", "@exclusiveMinimum"),
        ("exclusiveMaximum", "@exclusiveMaximum"),
        ("multipleOf", "@multipleOf"),
        ("minLength", "@minLength"),
        ("maxLength", "@maxLength"),
    ] {
        if let Some(Json::Num(value)) = get(schema, key) {
            tags.push(format!("{tag} {}", number_text(*value)));
        }
    }
    if let Some(Json::Str(pattern)) = get(schema, "pattern") {
        tags.push(format!("@pattern {pattern}"));
    }
    for (key, tag) in [("minItems", "@minItems"), ("maxItems", "@maxItems")] {
        if let Some(Json::Num(value)) = get(schema, key) {
            tags.push(format!("{tag} {}", number_text(*value)));
        }
    }
    if matches!(get(schema, "uniqueItems"), Some(Json::Bool(true))) {
        tags.push("@uniqueItems true".into());
    }
    tags
}

fn js_trim_end(text: &str) -> &str {
    text.trim_end_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn doc_lines(schema: &JsonObj, width: usize) -> Vec<String> {
    let summary = doc_tags(schema).join(" ");
    let description = match get(schema, "description") {
        Some(Json::Str(text)) => text.to_rust_lossy(),
        _ => String::new(),
    };
    let mut lines: Vec<String> = description.split('\n').map(|line| js_trim_end(line).to_string()).collect();
    while !lines.is_empty() && js_trim(&lines[0]).is_empty() {
        lines.remove(0);
    }
    while !lines.is_empty() && js_trim(lines.last().unwrap()).is_empty() {
        lines.pop();
    }
    let inline = if lines.len() == 1 {
        format!("{}{} {summary}", lines[0], if lines[0].ends_with('.') { "" } else { "." })
    } else {
        summary.clone()
    };
    if !summary.is_empty() && lines.len() == 1 && !summary.contains('\n') && width + utf16_len(&inline) + 7 <= 120 {
        return vec![inline];
    }
    let mut out = lines;
    if !summary.is_empty() {
        out.extend(summary.split('\n').map(String::from));
    }
    out
}

fn types_of(schema: &JsonObj) -> Vec<String> {
    match get(schema, "type") {
        Some(Json::Str(t)) => vec![t.to_rust_lossy()],
        Some(Json::Arr(items)) => items.iter().filter_map(|i| i.as_str().map(|s| s.to_rust_lossy())).collect(),
        _ => Vec::new(),
    }
}

// Neutralize `*\/` so model-provided schema text cannot terminate generated documentation.
fn jsdoc(schema: &JsonObj, pad: &str) -> String {
    let mut content = doc_lines(schema, pad.len());
    let types = types_of(schema);
    let append = |label: &str, child: &JsonObj, content: &mut Vec<String>| {
        for (index, line) in doc_lines(child, pad.len() + label.len() + 2).into_iter().enumerate() {
            if index == 0 {
                content.push(format!("{label}: {line}"));
                continue;
            }
            content.push(if line.is_empty() { String::new() } else { format!("  {line}") });
        }
    };
    if types.iter().any(|t| t == "array")
        && let Some(Json::Obj(items)) = get(schema, "items")
    {
        append("Each item", items, &mut content);
    }
    if (types.iter().any(|t| t == "object") || get(schema, "properties").is_some())
        && let Some(Json::Obj(additional)) = get(schema, "additionalProperties")
    {
        let has_properties = matches!(get(schema, "properties"), Some(Json::Obj(p)) if !p.is_empty());
        let label = if has_properties { "Each additional value" } else { "Each value" };
        append(label, additional, &mut content);
    }
    if content.is_empty() {
        return String::new();
    }
    let escaped: Vec<String> = content.iter().map(|line| line.replace("*/", "* /")).collect();
    if escaped.len() == 1 && pad.len() + utf16_len(&escaped[0]) + 7 <= 120 {
        return format!("{pad}/** {} */\n", escaped[0]);
    }
    let body: Vec<String> = escaped.iter().map(|line| if line.is_empty() { format!("{pad} *") } else { format!("{pad} * {line}") }).collect();
    format!("{pad}/**\n{}\n{pad} */\n", body.join("\n"))
}

fn without(schema: &JsonObj, keys: &[&str]) -> JsonObj {
    let mut copy = JsonObj::new();
    for (key, value) in schema.iter() {
        if keys.iter().any(|k| key.eq_str(k)) {
            continue;
        }
        copy.insert(key.clone(), value.clone());
    }
    copy
}

fn with_type(schema: &JsonObj, ty: &Json) -> JsonObj {
    let mut copy = JsonObj::new();
    let mut placed = false;
    for (key, value) in schema.iter() {
        if key.eq_str("type") {
            copy.insert(key.clone(), ty.clone());
            placed = true;
        } else {
            copy.insert(key.clone(), value.clone());
        }
    }
    if !placed {
        copy.insert_str("type", ty.clone());
    }
    copy
}

fn render(schema: &JsonObj, definitions: &Defs, pretty: bool, depth: usize, seen: &[String]) -> String {
    if depth > MAX_RENDER_DEPTH {
        return "unknown".into();
    }
    let mut nested_defs;
    let definitions = if get(schema, "definitions").is_none() && get(schema, "$defs").is_none() {
        definitions
    } else {
        nested_defs = definitions.clone();
        for key in ["definitions", "$defs"] {
            if let Some(Json::Obj(defs)) = get(schema, key) {
                for (name, value) in defs.iter() {
                    if let Some(obj) = value.as_obj() {
                        nested_defs.insert(name.to_rust_lossy(), obj.clone());
                    }
                }
            }
        }
        &nested_defs
    };
    if let Some(Json::Str(reference)) = get(schema, "$ref")
        && !reference.is_empty()
    {
        let Some(name) = definition_name(&reference.to_rust_lossy()) else { return "unknown".into() };
        let Some(definition) = definitions.get(&name) else { return "unknown".into() };
        if seen.contains(&name) {
            return "unknown".into();
        }
        let mut next = seen.to_vec();
        next.push(name.clone());
        return intersection(vec![
            render(definition, definitions, pretty, depth, &next),
            render(&without(schema, &["$ref"]), definitions, pretty, depth + 1, seen),
        ]);
    }
    if let Some(constant) = get(schema, "const") {
        return render_literal(constant);
    }
    if let Some(Json::Arr(values)) = get(schema, "enum") {
        return values.iter().map(render_literal).collect::<Vec<_>>().join(" | ");
    }
    let listed = obj_list(schema, "anyOf").or_else(|| obj_list(schema, "oneOf"));
    if let Some(listed) = listed {
        let alternatives: Vec<&JsonObj> = if listed.iter().any(|item| is_type(item, "number")) {
            listed.iter().copied().filter(|item| !effect_number_sentinel(item)).collect()
        } else {
            listed.clone()
        };
        if alternatives.iter().any(|item| is_type(item, "number"))
            && alternatives.iter().all(|item| is_type(item, "number") || effect_number_sentinel(item))
        {
            return "number".into();
        }
        if alternatives.len() == 2
            && is_type(alternatives[0], "object")
            && get(alternatives[0], "properties").is_none()
            && is_type(alternatives[1], "array")
            && get(alternatives[1], "items").is_none()
        {
            return "{}".into();
        }
        let members: Vec<String> = alternatives.iter().map(|item| render(item, definitions, pretty, depth + 1, seen)).collect();
        if members.iter().any(|member| member == "unknown") {
            return "unknown".into();
        }
        return intersection(vec![
            members.join(" | "),
            render(&without(schema, &["anyOf", "oneOf"]), definitions, pretty, depth + 1, seen),
        ]);
    }
    if let Some(all_of) = obj_list(schema, "allOf") {
        if all_of.iter().any(|item| has_unresolved_ref(item, definitions, &[], &mut Vec::new())) {
            return "unknown".into();
        }
        let mut members = vec![render(&without(schema, &["allOf"]), definitions, pretty, depth + 1, seen)];
        members.extend(all_of.iter().map(|item| render(item, definitions, pretty, depth + 1, seen)));
        return intersection(members);
    }
    if let Some(Json::Arr(types)) = get(schema, "type") {
        return types.iter().map(|ty| render(&with_type(schema, ty), definitions, pretty, depth + 1, seen)).collect::<Vec<_>>().join(" | ");
    }
    if is_type(schema, "string") {
        return "string".into();
    }
    if is_type(schema, "number") || is_type(schema, "integer") {
        return "number".into();
    }
    if is_type(schema, "boolean") {
        return "boolean".into();
    }
    if is_type(schema, "null") {
        return "null".into();
    }
    if is_type(schema, "array") {
        let items = match get(schema, "items") {
            Some(Json::Obj(items)) => items.clone(),
            _ => JsonObj::new(),
        };
        return format!("Array<{}>", render(&items, definitions, pretty, depth + 1, seen));
    }
    if is_type(schema, "object") || get(schema, "properties").is_some() {
        let required: Vec<String> = match get(schema, "required") {
            Some(Json::Arr(items)) => items.iter().filter_map(|i| i.as_str().map(|s| s.to_rust_lossy())).collect(),
            _ => Vec::new(),
        };
        let properties: Vec<(String, JsonObj)> = match get(schema, "properties") {
            Some(Json::Obj(props)) => props.iter().map(|(k, v)| (k.to_rust_lossy(), v.as_obj().cloned().unwrap_or_default())).collect(),
            _ => Vec::new(),
        };
        let index_type = match get(schema, "additionalProperties") {
            Some(Json::Obj(additional)) => Some(render(additional, definitions, pretty, depth + 1, seen)),
            _ => None,
        };
        let field = |name: &str, value: &JsonObj| {
            format!(
                "{}{}: {}",
                render_key(name),
                if required.iter().any(|r| r == name) { "" } else { "?" },
                render(value, definitions, pretty, depth + 1, seen)
            )
        };
        if !pretty {
            let mut fields: Vec<String> = properties.iter().map(|(n, v)| field(n, v)).collect();
            if let Some(index_type) = &index_type {
                fields.push(format!("[key: string]: {index_type}"));
            }
            return if fields.is_empty() { "{}".into() } else { format!("{{ {} }}", fields.join("; ")) };
        }
        if properties.is_empty() && index_type.is_none() {
            return "{}".into();
        }
        let pad = "  ".repeat(depth + 1);
        let mut lines: Vec<String> = properties.iter().map(|(n, v)| format!("{}{pad}{},", jsdoc(v, &pad), field(n, v))).collect();
        if let Some(index_type) = &index_type {
            lines.push(format!("{pad}[key: string]: {index_type},"));
        }
        return format!("{{\n{}\n{}}}", lines.join("\n"), "  ".repeat(depth));
    }
    if is_effect_empty_struct(schema) {
        return "{}".into();
    }
    "unknown".into()
}

/// `jsonSchemaToTypeScript(schema, pretty)`.
pub fn to_typescript(schema: &Json, pretty: bool) -> String {
    match schema {
        Json::Obj(obj) => render(obj, &Defs::new(), pretty, 0, &[]),
        // A non-object schema reads every keyword as absent.
        _ => "unknown".into(),
    }
}

pub struct InputProperty {
    pub name: String,
    pub description: Option<String>,
}

/// `inputProperties` for a JSON Schema input.
pub fn input_properties(schema: &Json) -> Vec<InputProperty> {
    let Some(obj) = schema.as_obj() else { return Vec::new() };
    let mut definitions = Defs::new();
    for key in ["definitions", "$defs"] {
        if let Some(Json::Obj(defs)) = get(obj, key) {
            for (name, value) in defs.iter() {
                if let Some(def) = value.as_obj() {
                    definitions.insert(name.to_rust_lossy(), def.clone());
                }
            }
        }
    }
    let mut target = obj.clone();
    if let Some(Json::Str(reference)) = get(obj, "$ref") {
        match definition_name(&reference.to_rust_lossy()).and_then(|name| definitions.get(&name).cloned()) {
            Some(resolved) => target = resolved,
            None => return Vec::new(),
        }
    }
    match get(&target, "properties") {
        Some(Json::Obj(properties)) => properties
            .iter()
            .map(|(name, value)| InputProperty {
                name: name.to_rust_lossy(),
                description: match value.as_obj().and_then(|v| v.get("description")) {
                    Some(Json::Str(text)) => Some(text.to_rust_lossy()),
                    _ => None,
                },
            })
            .collect(),
        _ => Vec::new(),
    }
}
