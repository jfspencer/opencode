//! OpenRouter's chat-completions route (packages/ai/src/providers/openrouter.ts over protocols/openai-chat.ts): the
//! request body (`OpenAIChat.fromRequest` + OpenRouter's `bodyOptions`, in their object-literal key order, which is
//! what `JSON.stringify` sends) and the stream parser (`OpenAIChat.protocol.stream`: `step`, `finishEvents`, with
//! `Lifecycle` and `ToolStream`), fed by the SSE framing of `Framing.sseWithDone` (Effect's SSE parser).
//!
//! TODO(port): media parts, custom (freeform) tools, cache-control markers, Kimi-style reasoning details.

use std::collections::{BTreeMap, BTreeSet};

use opencode_protocol::Json;
use opencode_protocol::wire::Out;

use super::error::{AIError, Kind};
use super::{AssistantPart, Event, Finish, Message, Model, Request, ToolResult, ToolResultValue, Usage, UserPart};
use crate::json::{self, Obj};

const ROUTE: &str = "openrouter/openrouter";

fn require_reasoning(model: &Model) -> bool {
    let id = model.model_id.to_ascii_lowercase();
    model.provider == "deepseek" || model.base_url.to_ascii_lowercase().contains("deepseek.com") || id.contains("deepseek")
}

fn encode(value: &Json) -> String {
    let mut out = Out::new();
    out.json(value);
    out.finish()
}

/// `ProviderShared.encodeJson(part.input === undefined ? {} : part.input)`.
fn arguments(input: &Json) -> String {
    encode(input)
}

fn tool_result_text(result: &ToolResult) -> String {
    match &result.value {
        ToolResultValue::Text(text) => text.clone(),
        ToolResultValue::Content(content) => content
            .iter()
            .filter(|item| json::get_str(item, "type") == Some("text"))
            .filter_map(|item| json::get_str(item, "text").map(str::to_owned))
            .collect::<Vec<_>>()
            .join("\n"),
        // `toolResultText` of an error result: its structured value as JSON.
        ToolResultValue::Error { error, content } => {
            let mut o = Obj::new();
            o.set("error", error.clone()).set("content", Json::Array(content.clone()));
            encode(&o.done())
        }
    }
}

fn wrap_system_update(text: &str) -> String {
    let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!("<system-update>\n{escaped}\n</system-update>")
}

/// A lowered chat message (`OpenAIChatMessage`), kept as JSON so its key order is the object literal's.
fn lower_messages(request: &Request) -> Vec<Json> {
    let model = &request.model;
    let mut messages: Vec<Json> = Vec::new();
    if !request.system.is_empty() {
        let mut o = Obj::new();
        o.str("role", "system").str("content", &request.system.join("\n"));
        messages.push(o.done());
    }
    let require = require_reasoning(model);
    for message in &request.messages {
        match message {
            Message::System(text) => {
                let text = wrap_system_update(text);
                match messages.last_mut() {
                    Some(previous) if json::get_str(previous, "role") == Some("user") && json::get_str(previous, "content").is_some() => {
                        let joined = format!("{}\n{text}", json::get_str(previous, "content").unwrap_or_default());
                        json::set(previous, "content", json::string(&joined));
                    }
                    // A user message with content parts (attachments) gets the update as one more text part.
                    Some(previous) if json::get_str(previous, "role") == Some("user") => {
                        if let Some(Json::Array(content)) = json::get_mut(previous, "content") {
                            content.push(lower_user_part(&UserPart::Text(text)));
                        }
                    }
                    _ => {
                        let mut o = Obj::new();
                        o.str("role", "user").str("content", &text);
                        messages.push(o.done());
                    }
                }
            }
            Message::User(parts) => {
                let mut o = Obj::new();
                o.str("role", "user");
                if parts.iter().all(|part| matches!(part, UserPart::Text(_))) {
                    let text: String = parts.iter().map(|part| if let UserPart::Text(text) = part { text.as_str() } else { "" }).collect();
                    o.str("content", &text);
                } else {
                    o.set("content", Json::Array(parts.iter().map(lower_user_part).collect()));
                }
                messages.push(o.done());
            }
            Message::Tool(results) => {
                for result in results {
                    let mut o = Obj::new();
                    o.str("role", "tool").str("tool_call_id", &result.id).str("content", &tool_result_text(result));
                    messages.push(o.done());
                }
            }
            Message::Assistant(parts) => {
                if parts.iter().all(|p| matches!(p, AssistantPart::Text { text, .. } if text.trim().is_empty())) {
                    continue;
                }
                messages.push(lower_assistant(parts, require));
            }
        }
    }
    messages
}

/// A user content part (`lowerUserMessage` / `lowerMedia`): PDFs as inline `file` parts, images and videos as data URLs.
/// Other media never get here: attachments only lower images and PDFs, and unsupported media became text.
fn lower_user_part(part: &UserPart) -> Json {
    let mut o = Obj::new();
    match part {
        UserPart::Text(text) => {
            o.str("type", "text").str("text", text);
        }
        UserPart::Media { mime, data, filename } => {
            let mime = mime.to_lowercase();
            let url = format!("data:{mime};base64,{data}");
            let mut inner = Obj::new();
            if mime == "application/pdf" {
                inner.str("filename", filename.as_deref().unwrap_or("document.pdf")).str("file_data", &url);
                o.str("type", "file").set("file", inner.done());
            } else if mime.starts_with("video/") {
                inner.str("url", &url);
                o.str("type", "video_url").set("video_url", inner.done());
            } else {
                inner.str("url", &url);
                o.str("type", "image_url").set("image_url", inner.done());
            }
        }
    }
    o.done()
}

fn reasoning_details(native: &Option<Json>) -> Option<&Vec<Json>> {
    match native.as_ref().and_then(|n| json::get(n, "reasoningDetails")) {
        Some(Json::Array(details)) => Some(details),
        _ => None,
    }
}

/// `lowerAssistantMessage` with OpenRouter's `assistant` override.
fn lower_assistant(parts: &[AssistantPart], require: bool) -> Json {
    let mut content = String::new();
    let mut reasoning: Vec<(&String, &Option<Json>)> = Vec::new();
    let mut calls: Vec<Json> = Vec::new();
    for part in parts {
        match part {
            AssistantPart::Text { text, .. } => content.push_str(text),
            AssistantPart::Reasoning { text, native } => reasoning.push((text, native)),
            AssistantPart::ToolCall { id, name, input, .. } => {
                let mut function = Obj::new();
                function.str("name", name).str("arguments", &arguments(input));
                let mut o = Obj::new();
                o.str("id", id).str("type", "function").set("function", function.done());
                calls.push(o.done());
            }
            AssistantPart::ToolResult(_) => {}
        }
    }
    let text: String = reasoning.iter().map(|(t, _)| t.as_str()).collect();
    // `reasoningDetails`: the known details of every part, when any part carries them.
    let details: Option<Vec<Json>> = if reasoning.iter().any(|(_, native)| reasoning_details(native).is_some()) {
        Some(
            reasoning
                .iter()
                .flat_map(|(_, native)| reasoning_details(native).cloned().unwrap_or_default())
                .filter_map(|detail| known_detail(&detail))
                .map(|detail| lower_detail(&detail))
                .collect(),
        )
    } else {
        None
    };
    let _ = require;
    let mut o = Obj::new();
    o.str("role", "assistant");
    if content.is_empty() && !calls.is_empty() {
        o.set("content", Json::Null);
    } else {
        o.str("content", &content);
    }
    if !calls.is_empty() {
        o.set("tool_calls", Json::Array(calls));
    }
    if let Some(details) = &details {
        o.set("reasoning_details", Json::Array(details.clone()));
    }
    // OpenRouter replays readable reasoning only beside its details (`reasoning_content`/`reasoning_text` dropped).
    if !text.is_empty() && details.as_ref().is_some_and(|d| !d.is_empty()) {
        o.str("reasoning", &text);
    }
    o.done()
}

/// `knownReasoningDetails` for one detail: the shapes OpenRouter (and Kimi) send, others dropped.
fn known_detail(detail: &Json) -> Option<Json> {
    let Json::Object(fields) = detail else { return None };
    let ty = json::get_str(detail, "type")?;
    let string_or_null = |key: &str| matches!(fields.get(key), None | Some(Json::String(_)) | Some(Json::Null));
    let string = |key: &str| matches!(fields.get(key), None | Some(Json::String(_)));
    let number = |key: &str| matches!(fields.get(key), None | Some(Json::Number(_)));
    let identity = string_or_null("id") && string("format") && number("index") && string_or_null("signature");
    match ty {
        "reasoning.text" if identity && string("text") => Some(ordered(fields, &["type", "text", "id", "format", "index", "signature"])),
        "reasoning.summary" if identity && string("summary") => {
            Some(ordered(fields, &["type", "summary", "id", "format", "index", "signature"]))
        }
        "reasoning.encrypted" if identity && matches!(fields.get("data"), Some(Json::String(_))) => {
            Some(ordered(fields, &["type", "data", "id", "format", "index", "signature"]))
        }
        "summary" if matches!(fields.get("summary"), Some(Json::String(_))) && number("index") => {
            Some(only(fields, &["type", "summary", "index"]))
        }
        "encrypted" if matches!(fields.get("encrypted"), Some(Json::String(_))) && number("index") => {
            Some(only(fields, &["type", "encrypted", "index"]))
        }
        _ => None,
    }
}

/// `lowerReasoningDetail`: Kimi rejects its stream-only `index` on requests.
fn lower_detail(detail: &Json) -> Json {
    match json::get_str(detail, "type") {
        Some("summary") | Some("encrypted") => {
            let mut out = detail.clone();
            json::remove(&mut out, "index");
            out
        }
        _ => detail.clone(),
    }
}

/// A struct-with-rest decode: declared keys first (when present), then the rest in order.
fn ordered(fields: &opencode_protocol::wire::Object, declared: &[&str]) -> Json {
    let mut o = Obj::new();
    for key in declared {
        if let Some(value) = fields.get(*key) {
            o.set(key, value.clone());
        }
    }
    for (key, value) in fields {
        if !declared.contains(&key.as_str()) {
            o.set(key, value.clone());
        }
    }
    o.done()
}

fn only(fields: &opencode_protocol::wire::Object, declared: &[&str]) -> Json {
    let mut o = Obj::new();
    for key in declared {
        if let Some(value) = fields.get(*key) {
            o.set(key, value.clone());
        }
    }
    o.done()
}

fn has_tool_history(request: &Request) -> bool {
    request.messages.iter().any(|m| match m {
        Message::Tool(_) => true,
        Message::Assistant(parts) => parts.iter().any(|p| matches!(p, AssistantPart::ToolCall { .. })),
        _ => false,
    })
}

/// The request body, exactly as the TS reference serializes it.
pub fn body(request: &Request) -> String {
    let model = &request.model;
    let options = match &model.options {
        Json::Object(o) => o.clone(),
        _ => Default::default(),
    };
    const KNOWN: &[&str] = &["models", "provider", "plugins", "debug", "user", "usage", "web_search_options", "reasoning"];
    let mut out = Out::new();
    {
        let mut o = out.object();
        // Unknown provider options go first (`...options`).
        for (key, value) in &options {
            if !KNOWN.contains(&key.as_str()) && key != "promptCacheKey" {
                o.key(key).json(value);
            }
        }
        o.key("model").string(&model.model_id);
        {
            let mut messages = o.key("messages").array();
            for message in lower_messages(request) {
                messages.item().json(&message);
            }
        }
        if !request.tools.is_empty() {
            let mut tools = o.key("tools").array();
            for tool in &request.tools {
                let mut t = tools.item().object();
                t.key("type").string("function");
                let mut f = t.key("function").object();
                f.key("name").string(&tool.name);
                f.key("description").string(&tool.description);
                f.key("parameters").json(&tool.parameters);
                f.key("strict").bool(false);
            }
        } else if has_tool_history(request) {
            o.key("tools").raw("[]");
        }
        if !request.tools.is_empty() {
            if let Some(choice) = request.tool_choice {
                o.key("tool_choice").string(choice);
            }
        }
        o.key("stream").bool(true);
        {
            let mut stream = o.key("stream_options").object();
            stream.key("include_usage").bool(true);
        }
        if let Some(max) = request.max_tokens {
            o.key("max_completion_tokens").number(max);
        }
        if let Some(key) = &request.prompt_cache_key {
            // OpenAI limits the key to 64 characters.
            let key: String = key.chars().take(64).collect();
            o.key("prompt_cache_key").string(&key);
        }
        // `usage`: `{include: true}` unless the options say otherwise.
        match options.get("usage") {
            None | Some(Json::Bool(true)) => {
                let mut usage = o.key("usage").object();
                usage.key("include").bool(true);
            }
            Some(Json::Bool(false)) => {
                let mut usage = o.key("usage").object();
                usage.key("include").bool(false);
            }
            Some(value @ Json::Object(_)) => o.key("usage").json(value),
            Some(_) => {}
        }
        for key in ["models", "provider", "plugins", "debug", "user", "web_search_options"] {
            if let Some(value) = options.get(key) {
                o.key(key).json(value);
            }
        }
        if let Some(reasoning @ Json::Object(_)) = options.get("reasoning") {
            o.key("reasoning").json(&fit_reasoning(reasoning, request.max_tokens));
        }
    }
    out.finish()
}

/// `fitReasoning`: a thinking budget above half the output limit is capped there (at least 1,024).
fn fit_reasoning(reasoning: &Json, max_tokens: Option<f64>) -> Json {
    let Some(budget) = json::get_num(reasoning, "max_tokens") else { return reasoning.clone() };
    let fitted = match max_tokens {
        Some(max) if budget > max / 2.0 => (max / 2.0).floor().max(1024.0),
        _ => budget,
    };
    let mut out = reasoning.clone();
    json::set(&mut out, "max_tokens", Json::Number(fitted));
    out
}

/// Effect's SSE parser over a UTF-8 byte stream: emits the `data` of each non-empty event (`Framing.sseWithDone`).
#[derive(Default)]
pub struct Framing {
    bytes: Vec<u8>,
    first: bool,
    started: bool,
    pending: String,
    discard_lf: bool,
    data: String,
    out: Vec<String>,
}

impl Framing {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.bytes.extend_from_slice(chunk);
        // Decode the complete UTF-8 prefix; an incomplete sequence waits for the next chunk.
        let valid = match std::str::from_utf8(&self.bytes) {
            Ok(_) => self.bytes.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => self.bytes.len(),
        };
        let text = String::from_utf8_lossy(&self.bytes[..valid]).into_owned();
        self.bytes.drain(..valid);
        self.text(&text);
        std::mem::take(&mut self.out)
    }

    fn text(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let mut chunk = chunk;
        if !self.started {
            self.started = true;
            self.first = true;
            if let Some(rest) = chunk.strip_prefix('\u{feff}') {
                chunk = rest;
            }
        }
        let mut rest = chunk;
        loop {
            if self.discard_lf {
                self.discard_lf = false;
                if let Some(after) = rest.strip_prefix('\n') {
                    rest = after;
                    continue;
                }
            }
            match rest.find(['\r', '\n']) {
                None => {
                    self.pending.push_str(rest);
                    break;
                }
                Some(end) => {
                    self.discard_lf = rest.as_bytes()[end] == b'\r';
                    let mut line = std::mem::take(&mut self.pending);
                    line.push_str(&rest[..end]);
                    self.line(&line);
                    rest = &rest[end + 1..];
                    if rest.is_empty() {
                        break;
                    }
                }
            }
        }
    }

    fn line(&mut self, line: &str) {
        if line.is_empty() {
            let data = std::mem::take(&mut self.data);
            if !data.is_empty() {
                let data = &data[..data.len() - 1];
                if !data.is_empty() && data != "null" && data != ": keepalive" {
                    self.out.push(data.to_owned());
                }
            }
            return;
        }
        let (field, value) = match line.find(':') {
            Some(colon) => {
                let value = &line[colon + 1..];
                (&line[..colon], value.strip_prefix(' ').unwrap_or(value))
            }
            None => (line, ""),
        };
        if field == "data" {
            self.data.push_str(value);
            self.data.push('\n');
        }
    }
}

#[derive(Debug, Clone)]
struct PendingTool {
    id: String,
    name: String,
    input: String,
}

#[derive(Debug, Clone, Default)]
struct Partial {
    id: Option<String>,
    name: Option<String>,
    input: String,
}

#[derive(Default)]
struct Lifecycle {
    step: bool,
    text: BTreeSet<String>,
    reasoning: BTreeSet<String>,
}

impl Lifecycle {
    fn step_start(&mut self, events: &mut Vec<Event>) {
        if !self.step {
            events.push(Event::StepStart);
            self.step = true;
        }
    }

    fn text_delta(&mut self, events: &mut Vec<Event>, id: &str, text: &str) {
        if !self.text.contains(id) {
            self.step_start(events);
            events.push(Event::TextStart { id: id.to_owned(), native: None });
            self.text.insert(id.to_owned());
        }
        events.push(Event::TextDelta { id: id.to_owned(), text: text.to_owned() });
    }

    fn reasoning_start(&mut self, events: &mut Vec<Event>, id: &str, native: &Json) {
        if self.reasoning.contains(id) {
            return;
        }
        self.step_start(events);
        events.push(Event::ReasoningStart { id: id.to_owned(), native: Some(native.clone()) });
        self.reasoning.insert(id.to_owned());
    }

    fn reasoning_delta(&mut self, events: &mut Vec<Event>, id: &str, text: &str, native: &Json) {
        self.reasoning_start(events, id, native);
        events.push(Event::ReasoningDelta { id: id.to_owned(), text: text.to_owned(), native: Some(native.clone()) });
    }

    fn reasoning_end(&mut self, events: &mut Vec<Event>, id: &str, native: &Json) {
        if !self.reasoning.contains(id) {
            return;
        }
        self.step_start(events);
        events.push(Event::ReasoningEnd { id: id.to_owned(), text: None, native: Some(native.clone()) });
        self.reasoning.remove(id);
    }

    fn finish(&mut self, events: &mut Vec<Event>, finish: Finish, usage: Option<Usage>) {
        self.step_start(events);
        for id in std::mem::take(&mut self.reasoning) {
            events.push(Event::ReasoningEnd { id, text: None, native: None });
        }
        for id in std::mem::take(&mut self.text) {
            events.push(Event::TextEnd { id, text: None, native: None });
        }
        events.push(Event::StepFinish { finish, usage, native: None });
        events.push(Event::Finish);
        self.step = false;
    }
}

/// The stream parser's state (`ParserState`).
pub struct Parser {
    tools: BTreeMap<i64, PendingTool>,
    pending: BTreeMap<i64, Partial>,
    tool_call_events: Vec<Event>,
    usage: Option<Usage>,
    finish: Option<Finish>,
    lifecycle: Lifecycle,
    reasoning_field: Option<String>,
    reasoning_text_observed: bool,
    reasoning_details: Vec<Json>,
    reasoning_details_observed: bool,
    reasoning_emitted: bool,
    latest_tool_index: Option<i64>,
    next_tool_index: i64,
}

fn str_field<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    json::get_str(value, key)
}

/// `optionalNull(Schema.String)`-style check: absent, null or a string.
fn string_or_null(value: &Json, key: &str) -> bool {
    matches!(json::get(value, key), None | Some(Json::Null) | Some(Json::String(_)))
}

fn number_or_null(value: &Json, key: &str) -> bool {
    matches!(json::get(value, key), None | Some(Json::Null) | Some(Json::Number(_)))
}

fn object_or_null(value: &Json, key: &str) -> bool {
    matches!(json::get(value, key), None | Some(Json::Null) | Some(Json::Object(_)))
}

fn valid_usage(usage: Option<&Json>) -> bool {
    match usage {
        None | Some(Json::Null) => true,
        Some(u @ Json::Object(_)) => {
            ["prompt_tokens", "completion_tokens", "total_tokens", "cached_tokens", "prompt_cache_hit_tokens", "cache_read_input_tokens", "cache_created_input_tokens"]
                .iter()
                .all(|k| number_or_null(u, k))
                && object_or_null(u, "prompt_tokens_details")
                && object_or_null(u, "completion_tokens_details")
                && json::get(u, "prompt_tokens_details").is_none_or(|d| {
                    matches!(d, Json::Null) || (number_or_null(d, "cached_tokens") && number_or_null(d, "cache_write_tokens"))
                })
                && json::get(u, "completion_tokens_details").is_none_or(|d| {
                    matches!(d, Json::Null)
                        || ["reasoning_tokens", "accepted_prediction_tokens", "rejected_prediction_tokens"].iter().all(|k| number_or_null(d, k))
                })
        }
        Some(_) => false,
    }
}

/// Whether a frame decodes as `OpenAIChatEvent`.
fn valid_event(event: &Json) -> bool {
    if !matches!(event, Json::Object(_)) || !valid_usage(json::get(event, "usage")) {
        return false;
    }
    match json::get(event, "error") {
        None | Some(Json::Null) => {}
        Some(error @ Json::Object(_)) => {
            if !matches!(json::get(error, "message"), Some(Json::String(_)))
                || !matches!(json::get(error, "code"), None | Some(Json::Null) | Some(Json::String(_)) | Some(Json::Number(_)))
            {
                return false;
            }
        }
        Some(_) => return false,
    }
    let choices = match json::get(event, "choices") {
        None | Some(Json::Null) => return true,
        Some(Json::Array(choices)) => choices,
        Some(_) => return false,
    };
    choices.iter().all(|choice| {
        if !matches!(choice, Json::Object(_))
            || !string_or_null(choice, "finish_reason")
            || !string_or_null(choice, "native_finish_reason")
            || !valid_usage(json::get(choice, "usage"))
        {
            return false;
        }
        match json::get(choice, "delta") {
            None | Some(Json::Null) => true,
            Some(delta @ Json::Object(_)) => {
                ["content", "refusal", "reasoning_content", "reasoning", "reasoning_text"].iter().all(|k| string_or_null(delta, k))
                    && match json::get(delta, "tool_calls") {
                        None | Some(Json::Null) => true,
                        Some(Json::Array(calls)) => calls.iter().all(|call| {
                            matches!(call, Json::Object(_))
                                && number_or_null(call, "index")
                                && string_or_null(call, "id")
                                && object_or_null(call, "function")
                                && object_or_null(call, "custom")
                                && json::get(call, "function").is_none_or(|f| {
                                    matches!(f, Json::Null) || (string_or_null(f, "name") && string_or_null(f, "arguments"))
                                })
                        }),
                        Some(_) => false,
                    }
            }
            Some(_) => false,
        }
    })
}

fn num(value: Option<&Json>) -> Option<f64> {
    match value {
        Some(Json::Number(n)) => Some(*n),
        _ => None,
    }
}

/// `mapUsage`.
fn map_usage(usage: Option<&Json>) -> Option<Usage> {
    let usage = usage.filter(|u| matches!(u, Json::Object(_)))?;
    let details = |key: &str, field: &str| json::get(usage, key).and_then(|d| num(json::get(d, field)));
    let input = num(json::get(usage, "prompt_tokens"));
    let output = num(json::get(usage, "completion_tokens"));
    let cached = details("prompt_tokens_details", "cached_tokens")
        .or_else(|| num(json::get(usage, "prompt_cache_hit_tokens")))
        .or_else(|| num(json::get(usage, "cached_tokens")))
        .or_else(|| num(json::get(usage, "cache_read_input_tokens")));
    let cache_write =
        details("prompt_tokens_details", "cache_write_tokens").or_else(|| num(json::get(usage, "cache_created_input_tokens")));
    let reasoning = details("completion_tokens_details", "reasoning_tokens");
    let sum = if cached.is_none() && cache_write.is_none() { None } else { Some(cached.unwrap_or(0.0) + cache_write.unwrap_or(0.0)) };
    let non_cached = match (input, sum) {
        (None, _) => None,
        (Some(input), None) => Some(input),
        (Some(input), Some(sum)) => Some((input - sum).max(0.0)),
    };
    let total = num(json::get(usage, "total_tokens")).or_else(|| {
        if input.is_none() && output.is_none() { None } else { Some(input.unwrap_or(0.0) + output.unwrap_or(0.0)) }
    });
    // `ReportedBilling`: a finite, non-negative `cost`.
    let cost = num(json::get(usage, "cost")).filter(|c| c.is_finite() && *c >= 0.0);
    Some(Usage { input, output, non_cached_input: non_cached, cache_read: cached, cache_write, reasoning, total, cost })
}

fn reasoning_metadata(field: &Option<String>, details: Option<&[Json]>) -> Json {
    let mut o = Obj::new();
    if let Some(field) = field {
        o.str("reasoningField", field);
    }
    if let Some(details) = details {
        o.set("reasoningDetails", Json::Array(details.to_vec()));
    }
    o.done()
}

fn detail_identity_conflict(previous: &Json, current: &Json) -> bool {
    let conflicting = |key: &str| {
        let a = json::get(previous, key).filter(|v| !matches!(v, Json::Null));
        let b = json::get(current, key).filter(|v| !matches!(v, Json::Null));
        matches!((a, b), (Some(a), Some(b)) if a != b)
    };
    let signature = |v: &Json| json::get_str(v, "signature").filter(|s| !s.is_empty()).map(str::to_owned);
    conflicting("id")
        || conflicting("index")
        || conflicting("format")
        || matches!((signature(previous), signature(current)), (Some(a), Some(b)) if a != b)
}

/// `previous || current || (previous !== undefined ? previous : current)`.
fn merge_value(previous: Option<&Json>, current: Option<&Json>) -> Option<Json> {
    let truthy = |v: &Json| match v {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Number(n) => *n != 0.0 && !n.is_nan(),
        Json::String(s) => !s.is_empty(),
        _ => true,
    };
    if let Some(p) = previous.filter(|p| truthy(p)) {
        return Some(p.clone());
    }
    if let Some(c) = current.filter(|c| truthy(c)) {
        return Some(c.clone());
    }
    previous.or(current).cloned()
}

fn merge_details(previous: &Json, detail: &Json) -> Option<Json> {
    if detail_identity_conflict(previous, detail) {
        return None;
    }
    let ty = json::get_str(previous, "type")?;
    if ty != json::get_str(detail, "type")? {
        return None;
    }
    let field = match ty {
        "reasoning.text" => "text",
        "reasoning.summary" | "summary" => "summary",
        _ => return None,
    };
    let mut merged = previous.clone();
    if let Json::Object(fields) = detail {
        for (key, value) in fields {
            json::set(&mut merged, key, value.clone());
        }
    }
    let joined = format!("{}{}", json::get_str(previous, field).unwrap_or_default(), json::get_str(detail, field).unwrap_or_default());
    json::set(&mut merged, field, json::string(&joined));
    if ty != "summary" {
        if let Some(signature) = merge_value(json::get(previous, "signature"), json::get(detail, "signature")) {
            json::set(&mut merged, "signature", signature);
        }
        if let Some(format) = merge_value(json::get(previous, "format"), json::get(detail, "format")) {
            json::set(&mut merged, "format", format);
        }
    }
    Some(merged)
}

fn detail_text(details: &[Json], hide_kimi_summary: bool) -> Option<String> {
    let parts: Vec<String> = details
        .iter()
        .filter_map(|detail| match json::get_str(detail, "type") {
            Some("reasoning.text") => json::get_str(detail, "text").filter(|t| !t.is_empty()).map(str::to_owned),
            Some("reasoning.summary") => json::get_str(detail, "summary").filter(|t| !t.is_empty()).map(str::to_owned),
            Some("summary") if !hide_kimi_summary => json::get_str(detail, "summary").filter(|t| !t.is_empty()).map(str::to_owned),
            _ => None,
        })
        .collect();
    (!parts.is_empty()).then(|| parts.concat())
}

impl Parser {
    pub fn new(_model: &Model) -> Self {
        Self {
            tools: BTreeMap::new(),
            pending: BTreeMap::new(),
            tool_call_events: Vec::new(),
            usage: None,
            finish: None,
            lifecycle: Lifecycle::default(),
            reasoning_field: None,
            reasoning_text_observed: false,
            reasoning_details: Vec::new(),
            reasoning_details_observed: false,
            reasoning_emitted: false,
            latest_tool_index: None,
            next_tool_index: 0,
        }
    }

    /// One SSE `data` payload: `[DONE]` ends the stream (no events); anything else is a JSON event.
    pub fn frame(&mut self, frame: &str) -> Result<Vec<Event>, AIError> {
        if frame == "[DONE]" {
            return Ok(vec![]);
        }
        let event = match json::parse(frame) {
            Some(event) if valid_event(&event) => event,
            _ => return Err(AIError::output(format!("Invalid {ROUTE} stream event"), Some(frame.to_owned()))),
        };
        self.step(&event).map_err(|mut error| {
            // `frameError`: an error raised by a frame carries the frame as its body.
            if error.body.is_none() || error.body.as_deref() == Some(&encode(&event)) {
                error.body = Some(frame.to_owned());
            }
            error
        })
    }

    fn finish_reason(event: &Json, raw: &str) -> Result<&'static str, AIError> {
        Ok(match raw {
            "error" => {
                return Err(AIError {
                    body: Some(encode(event)),
                    ..AIError::new(Kind::UnknownProvider, "Provider reported an error (finish_reason: error)")
                });
            }
            "network_error" => {
                return Err(AIError {
                    body: Some(encode(event)),
                    ..AIError::new(Kind::ProviderInternal, "Provider reported a network error (finish_reason: network_error)")
                });
            }
            "stop" | "end" => "stop",
            "length" => "length",
            "content_filter" => "content-filter",
            "function_call" | "tool_calls" => "tool-calls",
            _ => "unknown",
        })
    }

    fn missing_tool() -> AIError {
        AIError::output("OpenAI Chat tool call delta is missing id or name", None)
    }

    fn step(&mut self, event: &Json) -> Result<Vec<Event>, AIError> {
        if let Some(error @ Json::Object(_)) = json::get(event, "error") {
            let message = json::get_str(error, "message").unwrap_or_default();
            return Err(super::error::stream_error(message, json::get(error, "code"), encode(event)));
        }
        let mut events = Vec::new();
        let choice = match json::get(event, "choices") {
            Some(Json::Array(choices)) => choices.first().cloned(),
            _ => None,
        };
        let choice = choice.unwrap_or(Json::Null);
        if let Some(usage) = map_usage(json::get(event, "usage")).or_else(|| map_usage(json::get(&choice, "usage"))) {
            self.usage = Some(usage);
        }
        let raw_finish = str_field(&choice, "finish_reason").filter(|s| !s.is_empty()).map(str::to_owned);
        let finish = match &raw_finish {
            Some(raw) => Some(Finish {
                normalized: Self::finish_reason(event, raw)?.to_owned(),
                raw: Some(str_field(&choice, "native_finish_reason").map(str::to_owned).unwrap_or_else(|| raw.clone())),
            }),
            None => None,
        };
        let finish_before = self.finish.is_some();
        let delta = json::get(&choice, "delta").cloned().unwrap_or(Json::Null);
        let tool_deltas: Vec<Json> = match json::get(&delta, "tool_calls") {
            Some(Json::Array(calls)) => calls.clone(),
            _ => vec![],
        };
        let reasoning = {
            let mut fields: Vec<String> = Vec::new();
            for field in [self.reasoning_field.clone(), Some("reasoning_content".into()), Some("reasoning".into()), Some("reasoning_text".into())]
                .into_iter()
                .flatten()
            {
                if !fields.contains(&field) {
                    fields.push(field);
                }
            }
            fields.into_iter().find_map(|field| {
                json::get_str(&delta, &field).filter(|t| !t.is_empty()).map(|t| (field.clone(), t.to_owned()))
            })
        };
        if finish_before
            && tool_deltas.iter().any(|tool| {
                let truthy = |v: Option<&str>| v.is_some_and(|s| !s.is_empty());
                let function = json::get(tool, "function").cloned().unwrap_or(Json::Null);
                let custom = json::get(tool, "custom").cloned().unwrap_or(Json::Null);
                truthy(json::get_str(tool, "id"))
                    || truthy(json::get_str(&function, "name"))
                    || truthy(json::get_str(&function, "arguments"))
                    || truthy(json::get_str(&custom, "input"))
            })
        {
            return Err(AIError::output("OpenAI Chat received content after the finish reason", Some(encode(event))));
        }
        if self.reasoning_field.is_none() {
            self.reasoning_field = reasoning.as_ref().map(|(field, _)| field.clone());
        }
        self.reasoning_text_observed = self.reasoning_text_observed || reasoning.is_some();
        let detail_delta: Option<Vec<Json>> = match json::get(&delta, "reasoning_details") {
            Some(Json::Array(details)) => Some(details.iter().filter_map(known_detail).collect()),
            _ => None,
        };
        if let Some(details) = &detail_delta {
            for detail in details {
                match self.reasoning_details.last().and_then(|previous| merge_details(previous, detail)) {
                    Some(merged) => *self.reasoning_details.last_mut().unwrap() = merged,
                    None => self.reasoning_details.push(detail.clone()),
                }
            }
        }
        self.reasoning_details_observed = self.reasoning_details_observed || detail_delta.is_some();
        let delta_metadata = reasoning_metadata(&self.reasoning_field, None);
        let text = match &detail_delta {
            Some(details) if !details.is_empty() => {
                detail_text(details, self.reasoning_text_observed).or_else(|| reasoning.as_ref().map(|(_, t)| t.clone()))
            }
            _ => reasoning.as_ref().map(|(_, t)| t.clone()),
        };
        let content = json::get_str(&delta, "content").filter(|s| !s.is_empty()).map(str::to_owned);
        let refusal = json::get_str(&delta, "refusal").filter(|s| !s.is_empty()).map(str::to_owned);
        if let Some(text) = &text {
            self.lifecycle.reasoning_delta(&mut events, "reasoning-0", text, &delta_metadata);
        } else if self.reasoning_details_observed
            && !self.lifecycle.reasoning.contains("reasoning-0")
            && (content.is_some() || refusal.is_some() || !tool_deltas.is_empty())
        {
            self.lifecycle.reasoning_start(&mut events, "reasoning-0", &delta_metadata);
        }
        self.reasoning_emitted = self.reasoning_emitted || self.lifecycle.reasoning.contains("reasoning-0");
        if let Some(content) = &content {
            self.lifecycle.text_delta(&mut events, "text-0", content);
        }
        if let Some(refusal) = &refusal {
            self.lifecycle.text_delta(&mut events, "text-0", refusal);
        }
        let count = tool_deltas.len();
        for (position, tool) in tool_deltas.iter().enumerate() {
            let position = position as i64;
            let tool_id = json::get_str(tool, "id").filter(|s| !s.is_empty()).map(str::to_owned);
            let function = json::get(tool, "function").cloned().unwrap_or(Json::Null);
            // `toolIndexByID`: pending entries, overridden by started ones, in key order.
            let matched = tool_id.as_ref().and_then(|id| {
                let mut keys: BTreeSet<i64> = self.pending.keys().copied().collect();
                keys.extend(self.tools.keys().copied());
                keys.into_iter().find(|key| match self.tools.get(key) {
                    Some(started) => &started.id == id,
                    None => self.pending.get(key).and_then(|p| p.id.as_ref()) == Some(id),
                })
            });
            let fallback = if count > 1 { position } else { self.latest_tool_index.unwrap_or(position) };
            let fallback_id = self
                .tools
                .get(&fallback)
                .map(|t| Some(t.id.clone()))
                .unwrap_or_else(|| self.pending.get(&fallback).and_then(|p| p.id.clone()));
            let index = match json::get(tool, "index") {
                Some(Json::Number(n)) => *n as i64,
                _ => matched.unwrap_or_else(|| match (&tool_id, &fallback_id) {
                    (Some(id), Some(existing)) if existing != id => self.next_tool_index,
                    _ => fallback,
                }),
            };
            let current = self.tools.get(&index).cloned();
            let pending = self.pending.get(&index).cloned();
            let id = current.as_ref().map(|c| c.id.clone()).or_else(|| pending.as_ref().and_then(|p| p.id.clone())).or(tool_id.clone());
            let name = current
                .as_ref()
                .map(|c| c.name.clone())
                .or_else(|| pending.as_ref().and_then(|p| p.name.clone()))
                .or_else(|| json::get_str(&function, "name").filter(|s| !s.is_empty()).map(str::to_owned));
            let fragment = json::get_str(&function, "arguments").unwrap_or_default();
            let text = format!("{}{fragment}", pending.as_ref().map(|p| p.input.as_str()).unwrap_or_default());
            self.latest_tool_index = Some(index);
            self.next_tool_index = self.next_tool_index.max(index + 1);
            if current.is_none() && (id.is_none() || name.is_none()) {
                self.pending.insert(index, Partial { id, name, input: text });
                continue;
            }
            if pending.is_some() {
                self.pending.remove(&index);
            }
            // `ToolStream.appendOrStart`.
            let (Some(id), Some(name)) = (id, name) else { return Err(Self::missing_tool()) };
            let mut produced = Vec::new();
            match &current {
                Some(existing) if text.is_empty() && existing.id == id && existing.name == name => {}
                _ => {
                    let input = format!("{}{text}", current.as_ref().map(|c| c.input.as_str()).unwrap_or_default());
                    if current.is_none() {
                        produced.push(Event::ToolInputStart { id: id.clone(), name: name.clone() });
                    }
                    if !text.is_empty() {
                        produced.push(Event::ToolInputDelta { id: id.clone(), name: name.clone(), text: text.clone() });
                    }
                    self.tools.insert(index, PendingTool { id, name, input });
                }
            }
            if !produced.is_empty() {
                self.lifecycle.step_start(&mut events);
            }
            events.extend(produced);
        }
        let incomplete = finish.as_ref().is_some_and(|f| f.normalized == "content-filter" || f.normalized == "length");
        if finish.is_some() && !incomplete && !finish_before && !self.pending.is_empty() {
            return Err(AIError::output("OpenAI Chat tool call delta is missing id or name", Some(encode(event))));
        }
        if finish.is_some() && !incomplete && !finish_before && !self.tools.is_empty() {
            self.tool_call_events = self.finish_tools();
        }
        if !finish_before {
            self.finish = finish;
        }
        Ok(events)
    }

    /// `ToolStream.finishAll`: every pending call parses its arguments (`{}` when empty) into a call, or a tool input
    /// error when they are not JSON.
    fn finish_tools(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        for tool in std::mem::take(&mut self.tools).into_values() {
            events.push(Event::ToolInputEnd { id: tool.id.clone(), name: tool.name.clone() });
            let raw = if tool.input.is_empty() { "{}" } else { tool.input.as_str() };
            match json::parse(raw) {
                Some(input) => events.push(Event::ToolCall { id: tool.id, name: tool.name, input, native: None }),
                None => events.push(Event::ToolInputError { id: tool.id, name: tool.name, raw: tool.input }),
            }
        }
        events
    }

    /// `finishEvents`: the stream's end.
    pub fn finish(&mut self) -> Result<Vec<Event>, AIError> {
        let Some(finish) = self.finish.clone() else {
            return Err(AIError::incomplete("OpenAI Chat stream ended without finish_reason"));
        };
        let mut events = Vec::new();
        let tool_calls = std::mem::take(&mut self.tool_call_events);
        let has_calls = !tool_calls.is_empty();
        let reason = Finish {
            normalized: if finish.normalized == "stop" && has_calls { "tool-calls".into() } else { finish.normalized.clone() },
            raw: finish.raw.clone(),
        };
        let details = self.reasoning_details_observed.then(|| self.reasoning_details.clone());
        let metadata = reasoning_metadata(&self.reasoning_field, details.as_deref());
        if self.reasoning_details_observed && !self.reasoning_emitted {
            let start = reasoning_metadata(&self.reasoning_field, None);
            self.lifecycle.reasoning_start(&mut events, "reasoning-0", &start);
        }
        self.lifecycle.reasoning_end(&mut events, "reasoning-0", &metadata);
        if has_calls {
            self.lifecycle.step_start(&mut events);
        }
        events.extend(tool_calls);
        let usage = self.usage.clone();
        self.lifecycle.finish(&mut events, reason, usage);
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_splits_events_across_chunks() {
        let mut framing = Framing::default();
        assert!(framing.feed(b": OPENROUTER PROCESSING\n\ndata: {\"a\"").is_empty());
        assert_eq!(framing.feed(b":1}\n\ndata: [DONE]\n\n"), vec!["{\"a\":1}".to_owned(), "[DONE]".to_owned()]);
        let mut crlf = Framing::default();
        assert_eq!(crlf.feed("data: x\r\n\r\ndata: é".as_bytes()), vec!["x".to_owned()]);
    }
}
