//! Model requests (packages/ai: `LLMRequest`, `LLMEvent`, `AIError`, the route client and HTTP transport), reduced to
//! the one provider in scope: OpenRouter's chat-completions route (packages/ai/src/providers/openrouter.ts over
//! protocols/openai-chat.ts). [`openrouter`] lowers a [`Request`] to the provider body and parses the stream into
//! [`Event`]s; [`http`] is the HTTP client (proxy-aware, rustls); [`error`] classifies failures as TS does.
//!
//! The runner reads a model's events through [`stream`], which applies the transport's timeouts (headers, each
//! chunk, the whole request) as `packages/ai/src/route/transport/http.ts` does.

pub mod error;
pub mod http;
pub mod openrouter;

use std::sync::Arc;
use std::time::Duration;

use opencode_protocol::Json;
use tokio::sync::mpsc;

pub use error::AIError;

/// Timeouts of a model's transport (`Provider.timeouts`): `None` is no limit.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub total: Option<u64>,
    pub headers: Option<u64>,
    pub chunk: Option<u64>,
}

/// A resolved model (`SessionRunnerModel.Resolved`, reduced to what OpenRouter requests use).
// Some fields complete the canonical shape (`LLMRequest` / `LLMEvent`) for later slices.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Model {
    /// `Model.Ref` (`{id, providerID, variant?}`).
    pub reference: Json,
    pub provider: String,
    pub id: String,
    /// The provider's own model ID (`modelID`), sent as `model`.
    pub model_id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    /// Provider options from the model's settings and its variant (`reasoning`, `usage`, ...).
    pub options: Json,
    /// Request headers of the provider, model and variant (`settings.headers` of the route).
    pub headers: Vec<(String, String)>,
    /// A body overlay of the provider, model and variant, deep-merged into the request body (`http.body`).
    pub body: Option<Json>,
    pub limit_context: f64,
    /// `limit.input` (0 when the catalog has none).
    pub limit_input: f64,
    pub limit_output: f64,
    pub cost: Vec<Json>,
    /// `capabilities.input`.
    pub input: Vec<String>,
    pub timeouts: Timeouts,
    /// The prompt-cache lifetime of the model's `cache.ttl` setting (the request's `cache`); session requests carry it,
    /// one-shot generations clear it.
    pub cache_ttl: Option<String>,
}

// Some fields complete the canonical shape (`LLMRequest` / `LLMEvent`) for later slices.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum UserPart {
    Text(String),
    /// Inline media (base64 `data`): attachments are read inline before a request is built.
    Media { mime: String, data: String, filename: Option<String> },
}

#[derive(Debug, Clone)]
pub enum ToolResultValue {
    Text(String),
    Content(Vec<Json>),
    Error { error: Json, content: Vec<Json> },
}

// Some fields complete the canonical shape (`LLMRequest` / `LLMEvent`) for later slices.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub id: String,
    pub name: String,
    pub value: ToolResultValue,
    pub executed: bool,
    pub native: Option<Json>,
}

// Some fields complete the canonical shape (`LLMRequest` / `LLMEvent`) for later slices.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum AssistantPart {
    Text { text: String, native: Option<Json> },
    Reasoning { text: String, native: Option<Json> },
    ToolCall { id: String, name: String, input: Json, executed: bool, native: Option<Json> },
    ToolResult(ToolResult),
}

/// One message of the canonical history (`@opencode/ai` `Message`).
#[derive(Debug, Clone)]
pub enum Message {
    User(Vec<UserPart>),
    Assistant(Vec<AssistantPart>),
    Tool(Vec<ToolResult>),
    /// A chronological system update (`Message.system`).
    System(String),
}

/// A request (`LLMRequest`).
#[derive(Debug, Clone)]
pub struct Request {
    pub model: Arc<Model>,
    pub system: Vec<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<crate::tool::Definition>,
    /// `"none"` once the agent's step limit is reached.
    pub tool_choice: Option<&'static str>,
    pub max_tokens: Option<f64>,
    pub prompt_cache_key: Option<String>,
    /// Request headers beyond the route's (`x-opencode-*`, user agent).
    pub headers: Vec<(String, String)>,
}

/// Token usage of a step (`Usage`): inclusive totals and the derived breakdown.
// Some fields complete the canonical shape (`Usage`) for later slices.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub non_cached_input: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub reasoning: Option<f64>,
    pub total: Option<f64>,
    pub cost: Option<f64>,
}

impl Usage {
    pub fn visible_output(&self) -> f64 {
        (self.output.unwrap_or(0.0) - self.reasoning.unwrap_or(0.0)).max(0.0)
    }
}

/// A finish reason: normalized (`stop`, `length`, `tool-calls`, `content-filter`, `unknown`) and as sent.
#[derive(Debug, Clone)]
pub struct Finish {
    pub normalized: String,
    pub raw: Option<String>,
}

/// One event of a model's stream (`LLMEvent`). `native` is the provider's metadata (`providerMetadata.openrouter`).
// Some fields complete the canonical shape (`LLMRequest` / `LLMEvent`) for later slices.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum Event {
    StepStart,
    TextStart { id: String, native: Option<Json> },
    TextDelta { id: String, text: String },
    TextEnd { id: String, text: Option<String>, native: Option<Json> },
    ReasoningStart { id: String, native: Option<Json> },
    ReasoningDelta { id: String, text: String, native: Option<Json> },
    ReasoningEnd { id: String, text: Option<String>, native: Option<Json> },
    ToolInputStart { id: String, name: String },
    ToolInputDelta { id: String, name: String, text: String },
    ToolInputEnd { id: String, name: String },
    ToolCall { id: String, name: String, input: Json, native: Option<Json> },
    ToolInputError { id: String, name: String, raw: String },
    StepFinish { finish: Finish, usage: Option<Usage>, native: Option<Json> },
    Finish,
}

/// `mergeJsonRecords(base, overlay)`: records merge key by key (the base's keys keep their places, new ones follow);
/// any other overlay value replaces.
fn merge_records(base: &mut Json, overlay: &Json) {
    let Json::Object(fields) = overlay else { return };
    for (key, value) in fields {
        match (crate::json::get_mut(base, key), value) {
            (Some(existing @ Json::Object(_)), Json::Object(_)) => merge_records(existing, value),
            _ => crate::json::set(base, key, value.clone()),
        }
    }
}

const DEFAULT_HTTP_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_CHUNK_TIMEOUT_MS: u64 = 900_000;

/// Streams a request's events into `events`: the provider's events, then the stream's end (`Ok`) or its failure.
/// Dropping the receiver cancels the request.
pub async fn stream(request: &Request, events: mpsc::Sender<Result<Event, AIError>>) {
    let result = run(request, &events).await;
    if let Err(error) = result {
        let _ = events.send(Err(error)).await;
    }
}

async fn run(request: &Request, events: &mpsc::Sender<Result<Event, AIError>>) -> Result<(), AIError> {
    let model = &request.model;
    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    openrouter::check_media(request)?;
    let body = openrouter::body(request);
    // `bodyWithOverlay`: the configured body overlay is deep-merged into the request body.
    let body = match &model.body {
        Some(overlay) => {
            let mut merged = crate::json::parse(&body).unwrap_or_else(crate::json::object);
            merge_records(&mut merged, overlay);
            crate::json::text(&merged)
        }
        None => body,
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(key) = &model.api_key {
        headers.push(("Authorization".into(), format!("Bearer {key}")));
    }
    headers.push(("Content-Type".into(), "application/json".into()));
    headers.extend(model.headers.iter().cloned());
    headers.extend(request.headers.iter().cloned());
    let started = tokio::time::Instant::now();
    let total = model.timeouts.total.map(Duration::from_millis);
    let header_limit = Duration::from_millis(model.timeouts.headers.unwrap_or(DEFAULT_HTTP_TIMEOUT_MS));
    let header_limit = total.map_or(header_limit, |t| t.min(header_limit));
    let response = match tokio::time::timeout(header_limit, http::post(&url, &headers, body)).await {
        Ok(Ok(response)) => response,
        Ok(Err(failure)) => return Err(error::transport(&failure, "request", None, &url)),
        Err(_) => return Err(error::timeout("request", "Timed out waiting for response headers", None, &url)),
    };
    let status = response.status;
    if status >= 400 {
        let headers = response.headers.clone();
        let text = response.text().await.ok();
        return Err(error::http_status(status, &headers, text.as_deref(), &url));
    }
    // Errors raised while the stream is read carry the response's HTTP context.
    let context = error::HttpContext { url: url.clone(), status, headers: response.headers.clone() };
    let attach = |mut error: AIError| {
        if error.http.is_none() {
            error.http = Some(context.clone());
        }
        error
    };
    let chunk_limit = Duration::from_millis(model.timeouts.chunk.unwrap_or(DEFAULT_CHUNK_TIMEOUT_MS));
    let mut body = response.body;
    let mut framing = openrouter::Framing::default();
    let mut parser = openrouter::Parser::new(model);
    loop {
        let remaining = total.map(|t| t.saturating_sub(started.elapsed()));
        let limit = remaining.map_or(chunk_limit, |r| r.min(chunk_limit));
        let chunk = match tokio::time::timeout(limit, body.next()).await {
            Ok(Some(Ok(bytes))) => bytes,
            Ok(Some(Err(failure))) => return Err(attach(error::read_failure(&failure, None, &url))),
            Ok(None) => break,
            Err(_) => {
                let message = if remaining.is_some_and(|r| r <= chunk_limit) {
                    "Timed out waiting for the response to complete"
                } else {
                    "Timed out waiting for response data"
                };
                return Err(attach(error::timeout("read", message, None, &url)));
            }
        };
        let mut done = false;
        for frame in framing.feed(&chunk) {
            let output = parser.frame(&frame).map_err(attach)?;
            for event in output {
                if events.send(Ok(event)).await.is_err() {
                    return Ok(());
                }
            }
            if frame == "[DONE]" {
                done = true;
                break;
            }
        }
        if done {
            break;
        }
    }
    for event in parser.finish().map_err(attach)? {
        if events.send(Ok(event)).await.is_err() {
            return Ok(());
        }
    }
    Ok(())
}
