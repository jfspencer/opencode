//! One-shot text generation (packages/core/src/generate.ts `Generate.text`, server/src/handlers/generate.ts): a
//! stateless request for a prompt, run in the global location (the config directory). It retries only failures the
//! provider clearly never ran (`ModelRetry.retryOneShot`: rate limits, overload, connections refused or unresolved),
//! at most three times.
//!
//! TODO(port): image and decision generation, `session.generate` (generation from a session's context).

use std::sync::Arc;

use opencode_protocol::Json;
use tokio::sync::mpsc;

use crate::app::App;
use crate::bus::LocationRef;
use crate::http::{Failure, Result};
use crate::json::{self, Obj};
use crate::llm::error::Kind;
use crate::llm::{self, AIError, Event};
use crate::session::retry::Retry;

/// `Global.Path.config`: `$OPENCODE_CONFIG_DIR`, else `$XDG_CONFIG_HOME/opencode`.
pub fn global_config() -> String {
    if let Some(dir) = std::env::var("OPENCODE_CONFIG_DIR").ok().filter(|v| !v.is_empty()) {
        return crate::project::absolutize(&dir);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.config", std::env::var("HOME").unwrap_or_default()));
    format!("{base}/opencode")
}

/// `notRun`: whether a failed one-shot call clearly never ran at the provider.
fn not_run(error: &AIError) -> bool {
    let header = error.http.as_ref().and_then(|h| h.headers.iter().find(|(k, _)| k == "x-should-retry")).map(|(_, v)| v.as_str());
    match header {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    match error.kind {
        Kind::RateLimit => true,
        Kind::ProviderInternal => matches!(error.http.as_ref().map(|h| h.status), Some(503) | Some(529)),
        Kind::Transport => error.code.as_deref().is_some_and(|c| ["ECONNREFUSED", "ConnectionRefused", "ENOTFOUND", "EAI_AGAIN"].contains(&c)),
        _ => false,
    }
}

/// `providerError`: the HTTP error for a failed generation.
fn provider_error(provider: &str, error: &AIError) -> Failure {
    let invalid = |message: &str| Failure::invalid(message, None, None);
    if error.kind == Kind::InvalidRequest && error.http.is_none() {
        return invalid(&error.message);
    }
    if error.kind == Kind::UnsupportedOperation {
        return invalid(&error.message);
    }
    let status = error.http.as_ref().map(|h| h.status);
    let reason = match error.kind {
        Kind::Authentication => "authentication",
        Kind::RateLimit | Kind::QuotaExceeded => "rate_limit",
        Kind::ContentPolicy => "content_filter",
        Kind::InvalidRequest => "invalid_request",
        Kind::ProviderInternal if matches!(status, Some(503) | Some(529)) => "overloaded",
        _ => "provider",
    };
    let mut o = Obj::new();
    o.str("_tag", "ProviderRequestError").str("providerID", provider).str("reason", reason).str("message", &error.message);
    if let Some(status) = status {
        o.int("status", status as i64);
    }
    if matches!(error.kind, Kind::RateLimit | Kind::QuotaExceeded | Kind::ProviderInternal) {
        if let Some(ms) = error.retry_after_ms {
            o.set("retryAfterMs", Json::Number(ms));
        }
    }
    Failure::new(502, o.done())
}

async fn once(request: &llm::Request) -> std::result::Result<String, AIError> {
    let (sender, mut receiver) = mpsc::channel(64);
    let owned = request.clone();
    let streaming = tokio::spawn(async move { llm::stream(&owned, sender).await });
    let mut text = String::new();
    let mut failure = None;
    while let Some(next) = receiver.recv().await {
        match next {
            Ok(Event::TextDelta { text: delta, .. }) => text.push_str(&delta),
            Ok(_) => {}
            Err(error) => failure = Some(error),
        }
    }
    let _ = streaming.await;
    match failure {
        Some(error) => Err(error),
        None => Ok(text),
    }
}

/// `POST /api/experimental/generate/text`.
pub async fn text(app: &App, payload: &Json) -> Result<Json> {
    let location = app.locations.get(app, &LocationRef::directory(global_config())).await?;
    let inventory = &location.inventory;
    // `resolver.resolve(model)`: the requested model, else the configured default.
    let session = match json::get(payload, "model").filter(|m| !matches!(m, Json::Null)) {
        Some(reference) => {
            let provider = json::get_str(reference, "providerID").unwrap_or_default();
            let id = json::get_str(reference, "id").unwrap_or_default();
            if inventory.model(provider, id).is_none() {
                let mut o = Obj::new();
                o.str("_tag", "ModelNotFoundError").str("providerID", provider).str("modelID", id);
                o.str("message", &format!("Model not found: {provider}/{id}"));
                return Err(Failure::new(404, o.done()));
            }
            let mut o = Obj::new();
            o.set("model", reference.clone());
            o.done()
        }
        None => json::object(),
    };
    let model: Arc<llm::Model> = match crate::session::context::resolve_model(inventory, &session, None) {
        Ok(model) => model,
        Err(_) => {
            let mut o = Obj::new();
            o.str("_tag", "ServiceUnavailableError").str("message", "No model specified and no supported model is available");
            return Err(Failure::new(503, o.done()));
        }
    };
    let request = llm::Request {
        model: model.clone(),
        system: vec![],
        messages: vec![llm::Message::User(vec![llm::UserPart::Text(json::get_str(payload, "prompt").unwrap_or_default().to_owned())])],
        tools: vec![],
        tool_choice: None,
        max_tokens: None,
        prompt_cache_key: None,
        // Gateways require session attribution even for a stateless call; no session is stored.
        headers: vec![("x-opencode-session".into(), crate::id::session())],
    };
    let mut retry = Retry::default();
    let mut retries = 0;
    loop {
        match once(&request).await {
            Ok(text) => {
                let mut data = Obj::new();
                data.str("text", &text);
                return Ok(data.done());
            }
            Err(error) => {
                let decision = retry.decide(&error, not_run(&error));
                match decision {
                    Some(decision) if decision.retry && retries < 3 => {
                        retries += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(decision.delay as u64)).await;
                    }
                    _ => return Err(provider_error(&model.provider, &error)),
                }
            }
        }
    }
}
