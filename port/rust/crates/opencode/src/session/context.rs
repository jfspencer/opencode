//! What a step sends (packages/core/src/session/context.ts, runner/model.ts, model-request.ts, runner/to-llm-message.ts,
//! system-prompt.ts, plugin/identity.ts, tool/plugin/opencode.ts): the session's agent and model, its tools and
//! instructions, and its history as model messages, assembled into the provider request.
//!
//! The model-family prompts of plugin/optimize.ts (GPT, Claude, Kimi, ...) replace the default one ([`model_prompt`]).
//!
//! Built-in plugins' hooks are applied where their plugins live (plan mode, the goal reminder, identity, the model
//! prompts); the session hooks of external plugins (`context`, `model.request`, `http.*`) are out of scope with
//! external plugins. Images are bounded ([`bound_images`]) after unsupported media become notes ([`unsupported_parts`]).

use std::sync::Arc;

use opencode_protocol::Json;
use opencode_protocol::types as t;

use crate::inventory::Inventory;
use crate::json::{self, Obj};
use crate::llm::{self, AssistantPart, Message, ToolResult, ToolResultValue, UserPart};

const PROMPT: &str = "You are an AI agent running in OpenCode, a coding agent harness. Help the user accomplish their goals using the tools you have available.

# Harness
- Responses are rendered as GitHub-flavored Markdown.
- `<system-reminder>` blocks are harness instructions, not user-authored content. Read and follow them.
- Prefer parallelizing independent tool calls.
${OPENCODE_TOOL_GUIDANCE}

# Communication
- Use clear file paths when referring to files.
- Keep responses clear and concise, and avoid unnecessary technical jargon.

# Working in codebases
- Keep changes consistent with the structure, naming, style, and patterns of the surrounding code.
- Treat unfamiliar files or changes as potential user work and investigate before deleting or overwriting them.
";

/// `SessionSystemPrompt.make`.
pub fn system_prompt(tools: &[String]) -> String {
    render(PROMPT, tools)
}

/// The default system prompt as the model-family plugins render it for `model` (plugin/optimize.ts: OpenAI, Anthropic,
/// Kimi, Arcee and Meta models get their own template, Claude's appended to the default), over the request's tools.
pub fn model_prompt(model: &llm::Model, tools: &[String]) -> String {
    let id = model.id.to_lowercase();
    let mut prompt = system_prompt(tools);
    if id.contains("gpt") {
        prompt = render(if id.contains("astra") { include_str!("prompt/gpt-astra.txt") } else { include_str!("prompt/gpt.txt") }, tools);
    }
    if id.contains("claude") {
        prompt = format!("{prompt}\n\n{}", render(include_str!("prompt/anthropic.txt"), tools));
    }
    if id.contains("kimi") {
        prompt = render(include_str!("prompt/kimi.txt"), tools);
    }
    if id.contains("trinity") {
        prompt = render(include_str!("prompt/trinity.txt"), tools);
    }
    if id.contains("muse") {
        prompt = render(&include_str!("prompt/meta.txt").replace("{{MODEL_NAME}}", &model.name), tools);
    }
    prompt
}

/// `SessionSystemPrompt.render`: the tool guidance for the tools the request offers.
fn render(prompt: &str, tools: &[String]) -> String {
    let has = |name: &str| tools.iter().any(|t| t == name);
    let mut guidance: Vec<&str> = Vec::new();
    if has("shell") {
        guidance.push("- Prefer dedicated tools over shell commands; fall back to the shell when a tool cannot do what you need.");
        guidance.push("- Do not chain shell commands with separators like `echo \"====\";` or `printf '---'`; the output becomes noisy in a way that makes the user's side of the conversation worse.");
    }
    if has("write") {
        guidance.push("- Use the write tool to create files or completely replace their content. Prefer using the edit tool for targeted changes.");
    }
    if has("edit") {
        guidance.push("- Use the edit tool for targeted changes to existing text files. It replaces the exact text in `oldString` with `newString`, and the values must differ. By default, `oldString` must occur exactly once. If it occurs multiple times, include more surrounding context to make it unique or set `replaceAll` to true to replace every occurrence.");
    }
    prompt.replacen("${OPENCODE_TOOL_GUIDANCE}", &guidance.join("\n"), 1)
}

/// `IdentityPlugin.identity`.
pub fn identity(name: &str, reference: &Json) -> String {
    format!(
        "# Your Model\n- Name: {name}\n- Provider ID: {}\n- Model ID: {}",
        json::get_str(reference, "providerID").unwrap_or_default(),
        json::get_str(reference, "id").unwrap_or_default()
    )
}

/// Appended by the `opencode` tools plugin to primary, compaction and generate requests.
pub const WORKTREE_HINT: &str = "When you create a worktree outside the current working directory and intend to use it as your primary working directory, consider using `execute` to call `tools.opencode.session_move` and make the worktree the session's working directory.";

/// An agent of the inventory as JSON (`Agent.Info`).
pub fn agent(inventory: &Inventory, id: &str) -> Option<Json> {
    inventory.agent(id).map(json::of)
}

/// `Agent.select`: the session's agent, or the default one.
pub fn select_agent(inventory: &Inventory, session: &Json) -> Result<(String, Json), Json> {
    match inventory.select(json::get_str(session, "agent")) {
        (id, Some(info)) => Ok((id, json::of(info))),
        (id, None) => {
            let mut o = Obj::new();
            o.str("type", "unknown").str("message", &format!("Agent not found: \"{id}\""));
            Err(o.done())
        }
    }
}

fn no_route(message: String) -> Json {
    let mut o = Obj::new();
    o.str("type", "provider.no-route").str("message", &message);
    o.done()
}

/// `Provider.timeout`: a positive integer of milliseconds, or `false` (no limit); anything else is unset.
fn timeout(value: Option<&Json>) -> Option<Option<u64>> {
    match value {
        Some(Json::Bool(false)) => Some(None),
        Some(Json::Number(n)) if *n > 0.0 && n.fract() == 0.0 => Some(Some(*n as u64)),
        _ => None,
    }
}

/// Settings the route reads itself (`toConfig`: the connection) and opencode's own (`Provider.nativeSettings` omits
/// `Provider.Settings`' fields: timeouts, compaction, cache, transport, encoding), not provider options.
const CONNECTION_KEYS: &[&str] =
    &["baseURL", "apiKey", "headers", "body", "encoding", "timeout", "headerTimeout", "chunkTimeout", "compaction", "cache", "transport"];

/// A model reference's variant, `None` for none.
fn variant_of(reference: &Json) -> Option<&str> {
    json::get_str(reference, "variant")
}

/// `ModelResolver.resolveModel`: a catalog model (already merged with its provider) at a variant. The variant's
/// overlays apply over the model's (`withVariant`); the provider's own timeouts come from the provider's settings.
fn build(inventory: &Inventory, info: &t::ModelInfo, variant: Option<&str>) -> Result<llm::Model, Json> {
    let model = crate::inventory::providers::with_variant(info, variant).map_err(|_| {
        no_route(format!("Variant unavailable for {}/{}: {}", info.provider_id.0, info.id, variant.unwrap_or_default()))
    })?;
    let model = json::of(&model);
    let provider_id = info.provider_id.0.clone();
    let settings = json::get(&model, "settings").cloned().unwrap_or_else(json::object);
    let provider_settings = inventory
        .provider(&provider_id)
        .map(json::of)
        .and_then(|p| json::get(&p, "settings").cloned())
        .unwrap_or_else(json::object);
    let mut options = json::object();
    if let Json::Object(fields) = &settings {
        for (key, value) in fields {
            if !CONNECTION_KEYS.contains(&key.as_str()) {
                json::set(&mut options, key, value.clone());
            }
        }
    }
    let headers = match json::get(&model, "headers") {
        Some(Json::Object(headers)) => {
            headers.iter().filter_map(|(k, v)| json::as_str(v).map(|v| (k.clone(), v.to_owned()))).collect()
        }
        _ => vec![],
    };
    let mut reference = Obj::new();
    reference.str("id", &info.id).str("providerID", &provider_id);
    if let Some(variant) = variant {
        reference.str("variant", variant);
    }
    // `fromCatalogModel`: a provider compaction policy (the model's over the provider's) is rejected up front, so the
    // misconfiguration surfaces before any step runs; OpenRouter's route has neither a compaction trigger nor an
    // endpoint (`UnsupportedCompactionError`).
    let compaction = |settings: &Json| json::get(settings, "compaction").and_then(|c| json::get_str(c, "type")).map(str::to_owned);
    if compaction(&settings).or_else(|| compaction(&provider_settings)).as_deref() == Some("native") {
        let mut o = Obj::new();
        o.str("type", "provider.unsupported-operation")
            .str("message", &format!("Provider compaction is not supported by {provider_id}/{} (openrouter)", info.id));
        return Err(o.done());
    }
    let limit = json::get(&model, "limit");
    Ok(llm::Model {
        reference: reference.done(),
        provider: provider_id.clone(),
        model_id: info.model_id.clone(),
        id: info.id.clone(),
        name: info.name.clone(),
        base_url: json::get_str(&settings, "baseURL").unwrap_or("https://openrouter.ai/api/v1").to_owned(),
        api_key: crate::credential::api_key(inventory, &provider_id, json::get_str(&settings, "apiKey")),
        options,
        headers,
        body: json::get(&model, "body").filter(|b| matches!(b, Json::Object(o) if !o.is_empty())).cloned(),
        limit_context: limit.and_then(|l| json::get_num(l, "context")).unwrap_or(0.0),
        limit_input: limit.and_then(|l| json::get_num(l, "input")).unwrap_or(0.0),
        limit_output: limit.and_then(|l| json::get_num(l, "output")).unwrap_or(0.0),
        cost: match json::get(&model, "cost") {
            Some(Json::Array(cost)) => cost.clone(),
            _ => vec![],
        },
        input: info.capabilities.input.clone(),
        timeouts: llm::Timeouts {
            total: timeout(json::get(&provider_settings, "timeout")).flatten(),
            headers: timeout(json::get(&provider_settings, "headerTimeout")).unwrap_or(Some(300_000)),
            chunk: timeout(json::get(&provider_settings, "chunkTimeout")).unwrap_or(Some(900_000)),
        },
        // `runtimeInfo.settings.cache`: the model's over the provider's (a variant cannot change it).
        cache_ttl: [&settings, &provider_settings]
            .iter()
            .find_map(|s| json::get(s, "cache").and_then(|cache| json::get_str(cache, "ttl")))
            .map(str::to_owned),
    })
}

/// An available (enabled) model by reference (`Model.available().find`).
fn available<'a>(inventory: &'a Inventory, reference: &Json) -> Option<&'a t::ModelInfo> {
    let provider = json::get_str(reference, "providerID")?;
    let id = json::get_str(reference, "id")?;
    inventory.available_models().find(|m| m.provider_id.0 == provider && m.id == id)
}

/// `ModelResolver.resolve(requested)`: a requested model (enabled or not) at its variant, else the default model;
/// `None` when there is no such model.
pub fn resolve(inventory: &Inventory, requested: Option<&Json>) -> Option<Result<llm::Model, Json>> {
    match requested {
        Some(reference) => {
            let info = inventory.model(json::get_str(reference, "providerID")?, json::get_str(reference, "id")?)?;
            Some(build(inventory, info, variant_of(reference)))
        }
        None => {
            let info = inventory.default_model().filter(|m| m.package.is_some()).or_else(|| {
                inventory
                    .available_models()
                    .find(|m| m.package.is_some() && crate::inventory::providers::supports_text(&m.capabilities))
            })?;
            Some(build(inventory, info, None))
        }
    }
}

/// `SessionRunnerModel.resolve`: the session's model, else its agent's, else the default model.
pub fn resolve_model(inventory: &Inventory, session: &Json, agent: Option<&Json>) -> Result<Arc<llm::Model>, Json> {
    let session_id = json::get_str(session, "id").unwrap_or_default();
    if let Some(selected) = json::get(session, "model") {
        let Some(info) = available(inventory, selected) else {
            return Err(no_route(format!(
                "Model unavailable: {}/{}",
                json::get_str(selected, "providerID").unwrap_or_default(),
                json::get_str(selected, "id").unwrap_or_default()
            )));
        };
        return build(inventory, info, variant_of(selected)).map(Arc::new);
    }
    if let Some(preferred) = agent.and_then(|a| json::get(a, "model")) {
        if let Some(info) = available(inventory, preferred) {
            return build(inventory, info, variant_of(preferred)).map(Arc::new);
        }
    }
    match resolve(inventory, None) {
        Some(model) => model.map(Arc::new),
        None => Err(no_route(format!("No model is available for session {session_id}"))),
    }
}

fn native_of(item: &Json) -> Option<Json> {
    json::get(item, "native").filter(|n| !matches!(n, Json::Null)).cloned()
}

fn tool_input(tool: &Json) -> Json {
    let state = json::get(tool, "state").cloned().unwrap_or(Json::Null);
    let input = json::get(&state, "input").cloned().unwrap_or(Json::Null);
    if json::get_str(&state, "status") == Some("streaming") {
        if let Json::String(text) = &input {
            return json::parse(text).unwrap_or(input);
        }
    }
    input
}

fn tool_result(tool: &Json, native: Option<Json>) -> Option<ToolResult> {
    let state = json::get(tool, "state")?;
    let value = match json::get_str(state, "status")? {
        "completed" => {
            let content = match json::get(state, "content") {
                Some(Json::Array(content)) => content.clone(),
                _ => vec![],
            };
            // `normalizeToolResult` (ai/src/tool-history.ts): empty text is shown as "(no tool output)".
            const EMPTY: &str = "(no tool output)";
            match content.as_slice() {
                [single] if json::get_str(single, "type") == Some("text") => {
                    let text = json::get_str(single, "text").unwrap_or_default();
                    ToolResultValue::Text(if text.is_empty() { EMPTY.to_owned() } else { text.to_owned() })
                }
                _ => {
                    let kept: Vec<Json> = content
                        .iter()
                        .filter(|item| json::get_str(item, "type") != Some("text") || json::get_str(item, "text") != Some(""))
                        .cloned()
                        .collect();
                    if kept.is_empty() { ToolResultValue::Text(EMPTY.to_owned()) } else { ToolResultValue::Content(kept) }
                }
            }
        }
        "error" => ToolResultValue::Error {
            error: json::get(state, "error").cloned().unwrap_or(Json::Null),
            content: match json::get(state, "content") {
                Some(Json::Array(content)) => content.clone(),
                _ => vec![],
            },
        },
        _ => return None,
    };
    Some(ToolResult {
        id: json::get_str(tool, "id").unwrap_or_default().to_owned(),
        name: json::get_str(tool, "name").unwrap_or_default().to_owned(),
        value,
        executed: json::get_bool(tool, "executed") == Some(true),
        native,
    })
}

/// `toLLMMessage` of an assistant message.
fn assistant(message: &Json, model: &Json) -> Vec<Message> {
    let reference = json::get(message, "model").cloned().unwrap_or(Json::Null);
    let same_provider = json::get(&reference, "providerID") == json::get(model, "providerID");
    let same_model = same_provider && json::get(&reference, "id") == json::get(model, "id");
    let failed = json::get(message, "error").is_some_and(|e| !matches!(e, Json::Null));
    let reuse = same_model && !failed;
    let content = match json::get(message, "content") {
        Some(Json::Array(content)) => content.clone(),
        _ => vec![],
    };
    let interrupted = content.iter().any(|item| {
        json::get_str(item, "type") == Some("reasoning")
            && json::get(item, "time").is_some_and(|t| json::get(t, "completed").is_none())
    });
    let mut parts = Vec::new();
    let mut results = Vec::new();
    for item in &content {
        match json::get_str(item, "type") {
            Some("text") => parts.push(AssistantPart::Text {
                text: json::get_str(item, "text").unwrap_or_default().to_owned(),
                native: if reuse { native_of(item) } else { None },
            }),
            Some("reasoning") => {
                if failed || interrupted {
                    continue;
                }
                let text = json::get_str(item, "text").unwrap_or_default().to_owned();
                if reuse {
                    parts.push(AssistantPart::Reasoning { text, native: native_of(item) });
                } else if !text.is_empty() {
                    parts.push(AssistantPart::Reasoning { text, native: None });
                }
            }
            Some("tool") => {
                let executed = json::get_bool(item, "executed") == Some(true);
                let status = json::get(item, "state").and_then(|s| json::get_str(s, "status"));
                let reuse_call = reuse || (same_model && executed && matches!(status, Some("completed") | Some("error")));
                parts.push(AssistantPart::ToolCall {
                    id: json::get_str(item, "id").unwrap_or_default().to_owned(),
                    name: json::get_str(item, "name").unwrap_or_default().to_owned(),
                    input: tool_input(item),
                    executed,
                    native: if reuse_call { native_of(item) } else { None },
                });
                if executed {
                    if let Some(result) = tool_result(item, if same_provider { native_of(item) } else { None }) {
                        parts.push(AssistantPart::ToolResult(result));
                    }
                } else if let Some(result) = tool_result(item, if reuse { native_of(item) } else { None }) {
                    results.push(Message::Tool(vec![result]));
                }
            }
            _ => {}
        }
    }
    let meaningful: Vec<AssistantPart> = parts
        .into_iter()
        .filter(|part| match part {
            AssistantPart::Text { text, .. } => !text.is_empty(),
            AssistantPart::Reasoning { text, native } => {
                !text.is_empty() || matches!(native, Some(Json::Object(o)) if !o.is_empty())
            }
            _ => true,
        })
        .collect();
    if meaningful.is_empty() {
        return results;
    }
    let mut out = vec![Message::Assistant(meaningful)];
    out.extend(results);
    out
}

const IMAGE_MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

fn decoded_text(data: &str) -> String {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `attachmentLocation`: the local path of an attachment read from a `file:` URI.
fn attachment_location(file: &Json) -> Option<String> {
    let source = json::get(file, "source")?;
    if json::get_str(source, "type") != Some("uri") {
        return None;
    }
    crate::session::attachment::file_url_path(json::get_str(source, "uri")?)
}

fn source_uri(file: &Json) -> Option<&str> {
    json::get(file, "source").filter(|s| json::get_str(s, "type") == Some("uri")).and_then(|s| json::get_str(s, "uri"))
}

/// `attachmentContent`: text and directory attachments inline as text; every other attachment as media, so a model
/// that cannot read it reports it instead of the attachment disappearing.
fn attachment_content(file: &Json) -> Vec<UserPart> {
    let mime = json::get_str(file, "mime").unwrap_or_default();
    let data = json::get_str(file, "data").unwrap_or_default();
    let name = json::get_str(file, "name");
    let description = json::get_str(file, "description").map(|d| format!("Description: {d}"));
    match mime {
        "text/plain" => {
            let label = name.or(source_uri(file)).unwrap_or("inline attachment");
            let mut lines = vec![format!("Attached file: {label}")];
            lines.extend(description);
            lines.push(String::new());
            lines.push(decoded_text(data));
            vec![UserPart::Text(format!("\n\n{}", lines.join("\n")))]
        }
        "application/x-directory" => {
            let label = attachment_location(file).or(name.map(str::to_owned)).or(source_uri(file).map(str::to_owned));
            let mut lines = vec![format!("Attached directory: {}", label.as_deref().unwrap_or("directory"))];
            lines.extend(description);
            if !data.is_empty() {
                lines.push(String::new());
                lines.push(decoded_text(data));
            }
            vec![UserPart::Text(format!("\n\n{}", lines.join("\n")))]
        }
        _ => {
            let mut parts: Vec<UserPart> = attachment_location(file)
                .map(|location| UserPart::Text(format!("\n\nAttached file: {location}")))
                .into_iter()
                .collect();
            parts.push(UserPart::Media { mime: mime.to_owned(), data: data.to_owned(), filename: name.map(str::to_owned) });
            parts
        }
    }
}

/// `userAttachmentContent`: a repeated inline image under the same mention is sent once.
fn user_attachments(files: &[Json]) -> Vec<UserPart> {
    let eligible = |file: &Json| {
        IMAGE_MIMES.contains(&json::get_str(file, "mime").unwrap_or_default())
            && json::get(file, "source").and_then(|s| json::get_str(s, "type")) == Some("inline")
            && json::get(file, "mention").and_then(|m| json::get_str(m, "text")).is_some_and(|t| !t.is_empty())
    };
    if files.iter().filter(|f| eligible(f)).count() < 2 {
        return files.iter().flat_map(attachment_content).collect();
    }
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for file in files {
        if eligible(file) {
            let key = ["mime", "name", "description"].iter().map(|k| json::text(json::get(file, k).unwrap_or(&Json::Null))).collect::<Vec<_>>();
            let mention = json::get(file, "mention").and_then(|m| json::get_str(m, "text")).unwrap_or_default();
            let metadata = format!("{}|{}", key.join("|"), mention);
            if !seen.insert((metadata, json::get_str(file, "data").unwrap_or_default().to_owned())) {
                continue;
            }
        }
        out.extend(attachment_content(file));
    }
    out
}

fn modality(mime: &str) -> Option<&'static str> {
    if mime.starts_with("image/") {
        Some("image")
    } else if mime.starts_with("audio/") {
        Some("audio")
    } else if mime.starts_with("video/") {
        Some("video")
    } else if mime == "application/pdf" {
        Some("pdf")
    } else {
        None
    }
}

/// Providers reject other image types (BMP, HEIC, AVIF...); xAI also rejects GIF.
const XAI_IMAGE_MIMES: &[&str] = &["image/png", "image/jpeg", "image/webp"];

/// `unsupportedParts`: media the model cannot read (of a kind it does not take, an image type providers reject, or a
/// file of unknown kind) become a note, in its own paragraph, asking the model to tell the user.
///
/// A tool result's files (`toolResult` of `replaceMedia`) are replaced the same way, by a text item of the note. Then
/// the images are bounded ([`bound_images`]), as every request TS builds bounds them after this pass.
pub fn unsupported_parts(messages: &mut [Message], input: &[String], provider: &str) {
    unsupported(messages, input, provider);
    bound_images(messages);
}

const IMAGE_BYTES_TRIGGER: usize = 25 * 1024 * 1024;
const IMAGE_BYTES_TARGET: usize = 15 * 1024 * 1024;
const IMAGE_REMOVED: &str = "[This image was removed to reduce the request size and is no longer visible. Do not make claims about its contents from memory. If needed, retrieve it again with an available tool or ask the user to attach it again.]";

/// `boundImages`: replaying the images in order, the oldest are dropped down to 15 MiB of inline base64 whenever the
/// kept payload passes 25 MiB, so the replaced prefix (and the provider's prompt cache) only moves every 10 MiB.
pub fn bound_images(messages: &mut [Message]) {
    let image = |mime: &str| mime.to_lowercase().starts_with("image/");
    let file_image = |item: &Json| json::get_str(item, "type") == Some("file") && image(json::get_str(item, "mime").unwrap_or_default());
    let mut sizes: Vec<usize> = Vec::new();
    let tool_sizes = |result: &crate::llm::ToolResult, sizes: &mut Vec<usize>| {
        if let crate::llm::ToolResultValue::Content(content) = &result.value {
            sizes.extend(content.iter().filter(|item| file_image(item)).map(|item| json::get_str(item, "uri").unwrap_or_default().encode_utf16().count()));
        }
    };
    for message in messages.iter() {
        match message {
            Message::User(parts) => sizes.extend(parts.iter().filter_map(|part| match part {
                UserPart::Media { mime, data, .. } if image(mime) => Some(data.len()),
                _ => None,
            })),
            Message::Tool(results) => results.iter().for_each(|result| tool_sizes(result, &mut sizes)),
            Message::Assistant(parts) => parts.iter().for_each(|part| {
                if let crate::llm::AssistantPart::ToolResult(result) = part {
                    tool_sizes(result, &mut sizes);
                }
            }),
            Message::System(_) => {}
        }
    }
    let (mut cut, mut kept) = (0, 0);
    for size in &sizes {
        kept += size;
        if kept <= IMAGE_BYTES_TRIGGER {
            continue;
        }
        while kept > IMAGE_BYTES_TARGET {
            kept -= sizes[cut];
            cut += 1;
        }
    }
    if cut == 0 {
        return;
    }
    let mut index = 0;
    let mut remove = || {
        index += 1;
        index <= cut
    };
    let tool_result = |result: &mut crate::llm::ToolResult, remove: &mut dyn FnMut() -> bool| {
        let crate::llm::ToolResultValue::Content(content) = &mut result.value else { return };
        for item in content.iter_mut().filter(|item| file_image(item)) {
            if remove() {
                let mut o = Obj::new();
                o.str("type", "text").str("text", IMAGE_REMOVED);
                *item = o.done();
            }
        }
    };
    for message in messages.iter_mut() {
        match message {
            Message::User(parts) => {
                for part in parts.iter_mut() {
                    let UserPart::Media { mime, .. } = part else { continue };
                    if image(mime) && remove() {
                        *part = UserPart::Text(format!("\n\n{IMAGE_REMOVED}"));
                    }
                }
            }
            Message::Tool(results) => results.iter_mut().for_each(|result| tool_result(result, &mut remove)),
            Message::Assistant(parts) => parts.iter_mut().for_each(|part| {
                if let crate::llm::AssistantPart::ToolResult(result) = part {
                    tool_result(result, &mut remove);
                }
            }),
            Message::System(_) => {}
        }
    }
}

fn unsupported(messages: &mut [Message], input: &[String], provider: &str) {
    let note = |mime: &str, filename: Option<&str>| -> Option<String> {
        let modality = modality(mime);
        let images = if provider == "xai" { XAI_IMAGE_MIMES } else { IMAGE_MIMES };
        let unsupported = match modality {
            None => mime.to_owned(),
            Some(modality) if !input.iter().any(|item| item.starts_with(modality)) => modality.to_owned(),
            Some("image") if !images.contains(&mime.to_lowercase().as_str()) => mime.to_owned(),
            Some(_) => return None,
        };
        let subject =
            filename.filter(|n| !n.is_empty()).map(|n| format!("\"{n}\"")).unwrap_or_else(|| modality.unwrap_or("file").to_owned());
        Some(format!("ERROR: Cannot read {subject} (this model does not support {unsupported} input). Inform the user."))
    };
    let tool_result = |result: &mut crate::llm::ToolResult| {
        let crate::llm::ToolResultValue::Content(content) = &mut result.value else { return };
        for item in content.iter_mut() {
            if json::get_str(item, "type") != Some("file") {
                continue;
            }
            if let Some(text) = note(json::get_str(item, "mime").unwrap_or_default(), json::get_str(item, "name")) {
                let mut o = Obj::new();
                o.str("type", "text").str("text", &text);
                *item = o.done();
            }
        }
    };
    for message in messages {
        match message {
            Message::User(parts) => {
                for part in parts.iter_mut() {
                    let UserPart::Media { mime, filename, .. } = part else { continue };
                    if let Some(text) = note(mime, filename.as_deref()) {
                        // Providers may join a message's text parts, so the note starts its own paragraph.
                        *part = UserPart::Text(format!("\n\n{text}"));
                    }
                }
            }
            Message::Tool(results) => results.iter_mut().for_each(tool_result),
            Message::Assistant(parts) => {
                for part in parts.iter_mut() {
                    if let crate::llm::AssistantPart::ToolResult(result) = part {
                        tool_result(result);
                    }
                }
            }
            Message::System(_) => {}
        }
    }
}

/// `toLLMMessages`: projected history as model messages.
pub fn messages(history: &[Json], model: &Json) -> Vec<Message> {
    let mut out = Vec::new();
    for message in history {
        let text = || json::get_str(message, "text").unwrap_or_default().to_owned();
        match json::get_str(message, "type") {
            Some("user") => {
                let mut parts = Vec::new();
                if let Some(Json::Array(skills)) = json::get(message, "skills") {
                    parts.extend(skills.iter().filter_map(|s| json::get_str(s, "text")).map(|t| UserPart::Text(t.to_owned())));
                }
                let text = text();
                if !text.is_empty() {
                    parts.push(UserPart::Text(text));
                }
                if let Some(Json::Array(files)) = json::get(message, "files") {
                    parts.extend(user_attachments(files));
                }
                if !parts.is_empty() {
                    out.push(Message::User(parts));
                }
            }
            Some("synthetic") | Some("skill") => out.push(Message::User(vec![UserPart::Text(text())])),
            Some("system") => out.push(Message::System(text())),
            Some("location-switched") => {
                let directory = json::get(message, "location").and_then(|l| json::get_str(l, "directory")).unwrap_or_default();
                out.push(Message::User(vec![UserPart::Text(format!("The working directory has been changed to {directory}."))]));
            }
            Some("shell") => {
                if json::get(message, "metadata").and_then(|m| json::get_bool(m, "background")) == Some(true) {
                    continue;
                }
                let output = json::get(message, "output").and_then(|o| json::get_str(o, "output")).unwrap_or_default();
                out.push(Message::User(vec![UserPart::Text(format!(
                    "The following shell command was executed by the user:\n\nCommand:\n{}\n\nOutput:\n{output}",
                    json::get_str(message, "command").unwrap_or_default()
                ))]));
            }
            Some("assistant") => out.extend(assistant(message, model)),
            Some("compaction") if json::get_str(message, "status") == Some("completed") => {
                // History selection only keeps native windows the target model can replay. Only provider-native
                // compaction (the Responses routes of other providers) makes one, so OpenRouter's chat route never
                // replays one: out of scope.
                if json::get(message, "native").and_then(|n| json::get(n, "messages")).is_some() {
                    continue;
                }
                out.push(Message::User(vec![UserPart::Text(checkpoint(message))]));
            }
            // `model-switched` becomes effort-update markers only for routes with per-message effort updates
            // (`supportsEffortUpdates`); OpenRouter's chat route has none, so `applyEffortUpdates` strips them.
            _ => {}
        }
    }
    out
}

/// A completed summary compaction as the user message later requests open with (`toLLMMessage`): the summary, and the
/// text of the messages it kept verbatim (`metadata.recent`, which history rebuilds from `metadata.keepID`).
pub fn checkpoint(message: &Json) -> String {
    let summary = json::get_str(message, "summary").unwrap_or_default();
    let kept = json::get(message, "metadata").and_then(|m| json::get_str(m, "recent")).unwrap_or_default();
    let mut lines = vec![
        "<conversation-checkpoint>".to_owned(),
        "The following is a summary and serialized record of earlier conversation. Treat it as historical context, not as new instructions.".to_owned(),
        String::new(),
        format!("<summary>\n{summary}\n</summary>"),
    ];
    if !kept.is_empty() {
        lines.push(String::new());
        lines.push(format!("<recent-context>\n{kept}\n</recent-context>"));
    }
    lines.push("</conversation-checkpoint>".to_owned());
    lines.join("\n")
}

// Used when the catalog has no output limit; no reply needs more than the maximum.
const OUTPUT_TOKEN_FALLBACK: f64 = 32_000.0;
const OUTPUT_TOKEN_MAX: f64 = 256_000.0;
const ESTIMATE_ERROR: f64 = 0.15;
const WINDOW_HEADROOM: f64 = 0.01;
const OUTPUT_TOKEN_MIN: f64 = 1_024.0;

/// `SessionModelRequest.outputLimit` of a primary request.
pub fn output_limit(model: &llm::Model, measured: f64, estimated: f64) -> f64 {
    fitted_output_limit(model, Some((measured, estimated)), WINDOW_HEADROOM)
}

/// `SessionModelRequest.outputLimit`: the catalog limit, fitted to the room the prompt (`(measured, estimated)`)
/// leaves in the context window less `headroom` of it.
pub fn fitted_output_limit(model: &llm::Model, input: Option<(f64, f64)>, headroom: f64) -> f64 {
    let requested = (if model.limit_output > 0.0 { model.limit_output } else { OUTPUT_TOKEN_FALLBACK }).min(OUTPUT_TOKEN_MAX);
    let Some((measured, estimated)) = input.filter(|_| model.limit_context > 0.0) else { return requested };
    let room = (model.limit_context * (1.0 - headroom)).floor() - measured - (estimated * (1.0 + ESTIMATE_ERROR)).ceil();
    requested.min(OUTPUT_TOKEN_MIN.max(room))
}

/// The headers the session's model requests carry (`SessionModelRequest.prepare`).
pub fn headers(session: &Json) -> Vec<(String, String)> {
    let id = json::get_str(session, "id").unwrap_or_default().to_owned();
    let parent = json::get_str(session, "parentID").map(str::to_owned);
    let affinity = affinity(session);
    let mut out = vec![("x-opencode-session-id".to_owned(), id)];
    if let Some(parent) = &parent {
        out.push(("x-opencode-parent-session-id".into(), parent.clone()));
    }
    out.push(("x-session-affinity".into(), affinity.clone()));
    out.push(("X-Session-Id".into(), affinity.clone()));
    if let Some(parent) = &parent {
        out.push(("x-parent-session-id".into(), parent.clone()));
    }
    out.push(("User-Agent".into(), format!("opencode/{}/{}/cli", crate::app::VERSION, crate::app::CHANNEL)));
    out.push(("x-opencode-project".into(), json::get_str(session, "projectID").unwrap_or_default().to_owned()));
    out.push(("x-opencode-session".into(), affinity));
    out.push(("x-opencode-client".into(), "cli".into()));
    out
}

/// `SessionAffinity.get`: children share their parent's ID, forks their source's.
pub fn affinity(session: &Json) -> String {
    json::get_str(session, "parentID")
        .or_else(|| json::get(session, "fork").and_then(|f| json::get_str(f, "sessionID")))
        .or_else(|| json::get_str(session, "id"))
        .unwrap_or_default()
        .to_owned()
}

/// The prompt cache key (`promptCacheKey`): the affinity, without `ses_` for 64-hex IDs.
pub fn cache_key(session: &Json) -> String {
    let affinity = affinity(session);
    match affinity.strip_prefix("ses_") {
        Some(hex) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) => hex.to_owned(),
        _ => affinity,
    }
}
