//! What a step sends (packages/core/src/session/context.ts, runner/model.ts, model-request.ts, runner/to-llm-message.ts,
//! system-prompt.ts, plugin/identity.ts, tool/plugin/opencode.ts): the session's agent and model, its tools and
//! instructions, and its history as model messages, assembled into the provider request.
//!
//! TODO(port): the session plugin hooks (`context`, `model.request`, `http.*`), model-family prompts
//! (plugin/optimize.ts: GPT, Claude, Kimi, ...), media replacement and image bounding, the model catalog's small
//! model.

use std::sync::Arc;

use opencode_protocol::Json;

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
    PROMPT.replacen("${OPENCODE_TOOL_GUIDANCE}", &guidance.join("\n"), 1)
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
    let requested = json::get_str(session, "agent");
    let id = match requested {
        Some(id) => id.to_owned(),
        None => inventory.default_agent().map(|a| a.id.0.clone()).unwrap_or_else(|| "build".into()),
    };
    match agent(inventory, &id) {
        Some(info) => Ok((id, info)),
        None => {
            let mut o = Obj::new();
            o.str("type", "unknown").str("message", &format!("Agent not found: {id}"));
            Err(o.done())
        }
    }
}

fn no_route(message: String) -> Json {
    let mut o = Obj::new();
    o.str("type", "provider.no-route").str("message", &message);
    o.done()
}

fn merge(target: &mut Json, source: Option<&Json>) {
    let Some(Json::Object(fields)) = source else { return };
    for (key, value) in fields {
        match (json::get_mut(target, key), value) {
            (Some(existing @ Json::Object(_)), Json::Object(_)) => merge(existing, Some(value)),
            _ => json::set(target, key, value.clone()),
        }
    }
}

/// `Provider.timeout`: a positive integer of milliseconds, or `false` (no limit); anything else is unset.
fn timeout(value: Option<&Json>) -> Option<Option<u64>> {
    match value {
        Some(Json::Bool(false)) => Some(None),
        Some(Json::Number(n)) if *n > 0.0 && n.fract() == 0.0 => Some(Some(*n as u64)),
        _ => None,
    }
}

const TRANSPORT_KEYS: &[&str] = &["baseURL", "apiKey", "timeout", "headerTimeout", "chunkTimeout", "encoding", "headers", "body"];

/// `ModelResolver.resolveModel` for a catalog model at a variant: settings layered provider → model → variant → agent.
fn build(inventory: &Inventory, info: &Json, variant: Option<&str>, agent: Option<&Json>) -> Result<llm::Model, Json> {
    let provider_id = json::get_str(info, "providerID").unwrap_or_default().to_owned();
    let provider = inventory.provider(&provider_id).map(json::of);
    let mut settings = json::object();
    merge(&mut settings, provider.as_ref().and_then(|p| json::get(p, "settings")));
    merge(&mut settings, json::get(info, "settings"));
    if let Some(variant) = variant.filter(|v| *v != "default") {
        let found = match json::get(info, "variants") {
            Some(Json::Array(variants)) => variants.iter().find(|v| json::get_str(v, "id") == Some(variant)).cloned(),
            _ => None,
        };
        // TODO(port): `ModelResolver.VariantUnavailableError` for an unknown variant.
        merge(&mut settings, found.as_ref().and_then(|v| json::get(v, "settings")));
    }
    merge(&mut settings, agent.and_then(|a| json::get(a, "request")).and_then(|r| json::get(r, "settings")));
    let mut options = json::object();
    if let Json::Object(fields) = &settings {
        for (key, value) in fields {
            if !TRANSPORT_KEYS.contains(&key.as_str()) {
                json::set(&mut options, key, value.clone());
            }
        }
    }
    let id = json::get_str(info, "id").unwrap_or_default().to_owned();
    let mut reference = Obj::new();
    reference.str("id", &id).str("providerID", &provider_id);
    if let Some(variant) = variant {
        reference.str("variant", variant);
    }
    // The credential: settings, then the provider's environment variables (the reference config: OPENROUTER_API_KEY).
    let api_key = json::get_str(&settings, "apiKey").map(str::to_owned).or_else(|| {
        let names: Vec<String> = match json::get(&inventory.config, "providers").and_then(|p| json::get(p, &provider_id)).and_then(|p| json::get(p, "env")) {
            Some(Json::Array(names)) => names.iter().filter_map(|n| json::as_str(n).map(str::to_owned)).collect(),
            _ => vec!["OPENROUTER_API_KEY".into()],
        };
        names.iter().find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
    });
    let limit = json::get(info, "limit");
    Ok(llm::Model {
        reference: reference.done(),
        provider: provider_id,
        model_id: json::get_str(info, "modelID").unwrap_or(&id).to_owned(),
        id,
        name: json::get_str(info, "name").unwrap_or_default().to_owned(),
        base_url: json::get_str(&settings, "baseURL").unwrap_or("https://openrouter.ai/api/v1").to_owned(),
        api_key,
        options,
        limit_context: limit.and_then(|l| json::get_num(l, "context")).unwrap_or(0.0),
        limit_output: limit.and_then(|l| json::get_num(l, "output")).unwrap_or(0.0),
        cost: match json::get(info, "cost") {
            Some(Json::Array(cost)) => cost.clone(),
            _ => vec![],
        },
        input: match json::get(info, "capabilities").and_then(|c| json::get(c, "input")) {
            Some(Json::Array(input)) => input.iter().filter_map(|i| json::as_str(i).map(str::to_owned)).collect(),
            _ => vec![],
        },
        timeouts: llm::Timeouts {
            total: timeout(json::get(&settings, "timeout")).flatten(),
            headers: timeout(json::get(&settings, "headerTimeout")).unwrap_or(Some(300_000)),
            chunk: timeout(json::get(&settings, "chunkTimeout")).unwrap_or(Some(900_000)),
        },
    })
}

fn find_model<'a>(inventory: &'a Inventory, reference: &Json) -> Option<Json> {
    let provider = json::get_str(reference, "providerID")?;
    let id = json::get_str(reference, "id")?;
    inventory.model(provider, id).map(json::of)
}

/// `SessionRunnerModel.resolve`: the session's model, else its agent's, else the configured default.
pub fn resolve_model(inventory: &Inventory, session: &Json, agent: Option<&Json>) -> Result<Arc<llm::Model>, Json> {
    let session_id = json::get_str(session, "id").unwrap_or_default();
    if let Some(selected) = json::get(session, "model") {
        let Some(info) = find_model(inventory, selected) else {
            return Err(no_route(format!(
                "Model unavailable: {}/{}",
                json::get_str(selected, "providerID").unwrap_or_default(),
                json::get_str(selected, "id").unwrap_or_default()
            )));
        };
        return build(inventory, &info, json::get_str(selected, "variant"), agent).map(Arc::new);
    }
    if let Some(preferred) = agent.and_then(|a| json::get(a, "model")) {
        if let Some(info) = find_model(inventory, preferred) {
            return build(inventory, &info, json::get_str(preferred, "variant"), agent).map(Arc::new);
        }
    }
    // `resolver.resolve()`: the configured default model.
    let default = json::get_str(&inventory.config, "model").and_then(crate::inventory_standin::model_ref);
    match default.as_ref().and_then(|reference| find_model(inventory, reference).map(|info| (reference.clone(), info))) {
        Some((reference, info)) => build(inventory, &info, json::get_str(&reference, "variant"), agent).map(Arc::new),
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
            match content.as_slice() {
                [single] if json::get_str(single, "type") == Some("text") => {
                    ToolResultValue::Text(json::get_str(single, "text").unwrap_or_default().to_owned())
                }
                _ => ToolResultValue::Content(content),
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
                // TODO(port): attachments (text, directory and media files).
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
                // TODO(port): native compaction windows and the kept recent text.
                let summary = json::get_str(message, "summary").unwrap_or_default();
                out.push(Message::User(vec![UserPart::Text(format!(
                    "<conversation-checkpoint>\nThe following is a summary and serialized record of earlier conversation. Treat it as historical context, not as new instructions.\n\n<summary>\n{summary}\n</summary>\n</conversation-checkpoint>"
                ))]));
            }
            // TODO(port): `model-switched` effort updates.
            _ => {}
        }
    }
    out
}

// Used when the catalog has no output limit; no reply needs more than the maximum.
const OUTPUT_TOKEN_FALLBACK: f64 = 32_000.0;
const OUTPUT_TOKEN_MAX: f64 = 256_000.0;
const ESTIMATE_ERROR: f64 = 0.15;
const WINDOW_HEADROOM: f64 = 0.01;
const OUTPUT_TOKEN_MIN: f64 = 1_024.0;

/// `SessionModelRequest.outputLimit` of a primary request.
pub fn output_limit(model: &llm::Model, measured: f64, estimated: f64) -> f64 {
    let requested = (if model.limit_output > 0.0 { model.limit_output } else { OUTPUT_TOKEN_FALLBACK }).min(OUTPUT_TOKEN_MAX);
    if model.limit_context <= 0.0 {
        return requested;
    }
    let room = (model.limit_context * (1.0 - WINDOW_HEADROOM)).floor() - measured - (estimated * (1.0 + ESTIMATE_ERROR)).ceil();
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
