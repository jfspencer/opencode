//! `util/tool-display.ts` and `util/record.ts`: tool names, input summaries, execute calls, a tool state's
//! metadata and content; plus `strip-ansi` 7.1.2 (ansi-regex 6.3.0) and `JSON.stringify(value, null, 2)`, which the
//! tool views use.
use std::sync::LazyLock;

use opencode_protocol::Json;
use opencode_tui_sync::js;
use regex::Regex;

/// `canonicalToolName(name)`.
pub fn canonical_tool_name(name: &str) -> &str {
    match name {
        "bash" => "shell",
        "task" => "subagent",
        "apply_patch" => "patch",
        other => other,
    }
}

/// `isRecord(value)`.
pub fn is_record(value: Option<&Json>) -> bool {
    matches!(value, Some(Json::Object(_)))
}

/// A JSON field (`value?.[key]`, `undefined` for a non-object).
pub fn field<'a>(value: Option<&'a Json>, key: &str) -> Option<&'a Json> {
    match value {
        Some(Json::Object(o)) => o.get(key),
        _ => None,
    }
}

/// `stringValue(value)`: a string, or undefined.
pub fn string_value(value: Option<&Json>) -> Option<&str> {
    match value {
        Some(Json::String(s)) => Some(s),
        _ => None,
    }
}

/// `finiteNumber(value)`.
pub fn finite_number(value: Option<&Json>) -> Option<f64> {
    match value {
        Some(Json::Number(n)) if n.is_finite() => Some(*n),
        _ => None,
    }
}

/// `primitiveInputSummary(input, omit)`: `[key=value, ...]` of the string, number and boolean entries.
pub fn primitive_input_summary(input: &Json, omit: &[&str]) -> String {
    let Json::Object(object) = input else { return String::new() };
    let entries: Vec<String> = object
        .iter()
        .filter(|(key, _)| !omit.contains(&key.as_str()))
        .filter_map(|(key, value)| match value {
            Json::String(_) | Json::Number(_) | Json::Bool(_) => Some(format!("{key}={}", js::string(value))),
            _ => None,
        })
        .collect();
    if entries.is_empty() {
        return String::new();
    }
    format!("[{}]", entries.join(", "))
}

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\t\n\x0B\x0C\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]+").unwrap());

/// `.replace(/\s+/g, " ")`.
pub fn collapse_whitespace(text: &str) -> String {
    WHITESPACE.replace_all(text, " ").into_owned()
}

/// `ExecuteCall`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecuteCall {
    pub tool: String,
    /// `"running" | "completed" | "error"`.
    pub status: String,
    pub input: Option<Json>,
}

/// `executeCalls(value)`.
pub fn execute_calls(value: Option<&Json>) -> Vec<ExecuteCall> {
    let Some(Json::Array(items)) = value else { return Vec::new() };
    items
        .iter()
        .filter_map(|call| {
            if !is_record(Some(call)) {
                return None;
            }
            let tool = string_value(field(Some(call), "tool"))?;
            let status = string_value(field(Some(call), "status"))?;
            if !matches!(status, "running" | "completed" | "error") {
                return None;
            }
            let input = field(Some(call), "input").filter(|i| is_record(Some(i))).cloned();
            Some(ExecuteCall { tool: tool.to_string(), status: status.to_string(), input })
        })
        .collect()
}

/// `executeCallSummary(call)`.
pub fn execute_call_summary(call: &ExecuteCall) -> String {
    let empty = Json::Object(Default::default());
    let args = collapse_whitespace(&primitive_input_summary(call.input.as_ref().unwrap_or(&empty), &[]));
    if args.is_empty() { call.tool.clone() } else { format!("{} {args}", call.tool) }
}

/// `webSearchProviderName(provider)`.
pub fn web_search_provider_name(provider: Option<&Json>) -> String {
    match provider {
        Some(Json::String(p)) if !p.is_empty() => {
            if p == "opencode" {
                return "OpenCode".into();
            }
            // `provider[0].toUpperCase()`: the first UTF-16 unit.
            let mut chars = p.chars();
            let first = chars.next().unwrap();
            format!("{}{}", first.to_uppercase(), chars.as_str())
        }
        _ => String::new(),
    }
}

/// `webSearchProviderLabel(provider)`.
pub fn web_search_provider_label(provider: Option<&Json>) -> String {
    let name = web_search_provider_name(provider);
    if name.is_empty() { "Web Search".into() } else { format!("Web Search via {name}") }
}

/// `toolDisplayMetadata(state)`: a settled state's metadata object, else `{}`.
pub fn tool_display_metadata(state: Option<&Json>) -> Json {
    let empty = Json::Object(Default::default());
    let Some(state @ Json::Object(_)) = state else { return empty };
    match string_value(field(Some(state), "status")) {
        None | Some("streaming") => return empty,
        _ => {}
    }
    match field(Some(state), "metadata") {
        Some(metadata @ Json::Object(_)) => metadata.clone(),
        _ => empty,
    }
}

/// `toolDisplayContent(state)`: a finished state's content entries.
pub fn tool_display_content(state: Option<&Json>) -> Vec<Json> {
    match string_value(field(state, "status")) {
        Some("streaming") | Some("running") => Vec::new(),
        _ => match field(state, "content") {
            Some(Json::Array(items)) => items.clone(),
            _ => Vec::new(),
        },
    }
}

static ANSI: LazyLock<Regex> = LazyLock::new(|| {
    let st = r"(?:\x07|\x1b\\|\u{9c})";
    let osc = format!(r"(?:\x1b\][^\x07\x1b\u{{9c}}]*{st})");
    let csi = r"[\x1b\u{9b}][\[\]()#;?]*(?:[0-9]{1,4}(?:[;:][0-9]{0,4})*)?[0-9A-PR-TZcf-nq-uy=><~]";
    Regex::new(&format!("{osc}|{csi}")).unwrap()
});

/// `stripAnsi(text)` (strip-ansi 7.1.2).
pub fn strip_ansi(text: &str) -> String {
    ANSI.replace_all(text, "").into_owned()
}

/// `JSON.stringify(value, null, 2)`.
pub fn stringify_pretty(value: &Json) -> String {
    fn write(value: &Json, indent: usize, out: &mut String) {
        match value {
            Json::Array(items) if !items.is_empty() => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('\n');
                    out.push_str(&" ".repeat(indent + 2));
                    write(item, indent + 2, out);
                }
                out.push('\n');
                out.push_str(&" ".repeat(indent));
                out.push(']');
            }
            Json::Object(object) if !object.is_empty() => {
                out.push('{');
                for (i, (key, item)) in object.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('\n');
                    out.push_str(&" ".repeat(indent + 2));
                    out.push_str(&opencode_tui_sync::client::stringify(&Json::String(key.clone())));
                    out.push_str(": ");
                    write(item, indent + 2, out);
                }
                out.push('\n');
                out.push_str(&" ".repeat(indent));
                out.push('}');
            }
            other => out.push_str(&opencode_tui_sync::client::stringify(other)),
        }
    }
    let mut out = String::new();
    write(value, 0, &mut out);
    out
}

/// `typeof value === "string" ? value : JSON.stringify(value, null, 2)`.
pub fn display_value(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        other => stringify_pretty(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries() {
        let input = opencode_protocol::wire::json::parse(r#"{"value":1,"flag":true,"label":"some   spaced   text","obj":{}}"#).unwrap();
        assert_eq!(
            collapse_whitespace(&primitive_input_summary(&input, &[])),
            "[value=1, flag=true, label=some spaced text]"
        );
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m \x1b]8;;http://x\x07link\x1b]8;;\x07"), "red link");
        let nested = opencode_protocol::wire::json::parse(r#"{"a":[1,{"b":null}],"c":{}}"#).unwrap();
        assert_eq!(stringify_pretty(&nested), "{\n  \"a\": [\n    1,\n    {\n      \"b\": null\n    }\n  ],\n  \"c\": {}\n}");
    }
}
