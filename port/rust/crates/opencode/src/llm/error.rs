//! Model failures (packages/ai/src/schema/errors.ts `AIError`, provider-error.ts `classifyProviderFailure`,
//! route/executor.ts `httpFailure` / `httpError`, utils/provider-codes.ts) and their session form
//! (packages/core/src/session/to-session-error.ts).
//!
//! Transport failures are worded as the TS reference's HTTP client (Bun's fetch) words them, because that text is
//! what the session records (`provider.transport` errors): a connection closed before or while the response is read
//! is `ECONNRESET: The socket connection was closed unexpectedly. ...`.

use std::sync::OnceLock;

use opencode_protocol::Json;
use regex::Regex;

use crate::json::{self, Obj};

use super::http::{Failure, FailureKind};

/// `AIError.reason._tag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    InvalidRequest,
    UnsupportedOperation,
    Authentication,
    RateLimit,
    QuotaExceeded,
    ContentPolicy,
    ProviderInternal,
    Transport,
    InvalidProviderOutput,
    UnknownProvider,
    Timeout,
}

/// The HTTP exchange a failure came from (`HttpContext`).
#[derive(Debug, Clone)]
pub struct HttpContext {
    pub url: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct AIError {
    pub kind: Kind,
    pub message: String,
    pub body: Option<String>,
    pub http: Option<HttpContext>,
    pub retry_after_ms: Option<f64>,
    /// `InvalidRequest`: `context-overflow` / `payload-too-large`; `InvalidProviderOutput`: `incomplete-stream`.
    pub classification: Option<&'static str>,
    /// `Transport`: `request` / `read`.
    pub operation: Option<&'static str>,
    /// `Transport`: the system error code (`ECONNRESET`, `Timeout`, ...).
    pub code: Option<String>,
    pub url: Option<String>,
}

impl AIError {
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            body: None,
            http: None,
            retry_after_ms: None,
            classification: None,
            operation: None,
            code: None,
            url: None,
        }
    }

    /// `eventError`: invalid provider output.
    pub fn output(message: impl Into<String>, body: Option<String>) -> Self {
        Self { body, ..Self::new(Kind::InvalidProviderOutput, message) }
    }

    pub fn incomplete(message: impl Into<String>) -> Self {
        Self { classification: Some("incomplete-stream"), ..Self::new(Kind::InvalidProviderOutput, message) }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.http.as_ref()?.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// `AIError.isRetryable`.
    pub fn retryable(&self) -> bool {
        match self.header("x-should-retry") {
            Some("true") => return true,
            Some("false") => return false,
            _ => {}
        }
        match self.kind {
            Kind::RateLimit | Kind::ProviderInternal | Kind::Transport | Kind::UnknownProvider => true,
            Kind::InvalidProviderOutput => self.classification == Some("incomplete-stream"),
            _ => false,
        }
    }

    pub fn context_overflow(&self) -> bool {
        self.kind == Kind::InvalidRequest && self.classification == Some("context-overflow")
    }

    /// `ModelRetry.isUnreachable`.
    pub fn unreachable(&self) -> bool {
        self.kind == Kind::Transport
            && self.code.as_deref().is_some_and(|c| ["ENOTFOUND", "EAI_AGAIN", "ENETUNREACH", "EHOSTUNREACH", "ENETDOWN"].contains(&c))
    }

    /// The delay a rate limit or provider failure asked for (`retryAfter` of model-retry.ts).
    pub fn retry_after(&self) -> Option<f64> {
        match self.kind {
            Kind::RateLimit | Kind::ProviderInternal => self.retry_after_ms.map(|ms| ms.min(15.0 * 60_000.0)),
            _ => None,
        }
    }

    /// `toSessionError` (`SessionError.Error` as JSON).
    pub fn session_error(&self) -> Json {
        if self.body.as_deref().is_some_and(|b| b.contains("subscription_sharing_usage_limit_exceeded")) {
            return self.provider_error("provider.chatgpt-limit");
        }
        let ty = match self.kind {
            Kind::RateLimit => "provider.rate-limit",
            Kind::Authentication => "provider.auth",
            Kind::QuotaExceeded => "provider.quota",
            Kind::ContentPolicy => "provider.content-filter",
            Kind::Transport => {
                if self.unreachable() {
                    let host = self.url.as_deref().and_then(host_of).filter(|h| !h.is_empty());
                    let mut error = self.provider_error("provider.transport");
                    json::set(
                        &mut error,
                        "message",
                        json::string(&format!(
                            "Can't reach {}. Check the network connection. ({})",
                            host.unwrap_or("the provider"),
                            self.message
                        )),
                    );
                    return error;
                }
                "provider.transport"
            }
            Kind::ProviderInternal => "provider.internal",
            Kind::InvalidProviderOutput => "provider.invalid-output",
            Kind::InvalidRequest => "provider.invalid-request",
            Kind::UnsupportedOperation => "provider.unsupported-operation",
            Kind::UnknownProvider => "provider.unknown",
            Kind::Timeout => "provider.timeout",
        };
        // TODO(port): OpenCode Zen / Go / ChatGPT token sharing messages (not reachable through OpenRouter).
        self.provider_error(ty)
    }

    fn provider_error(&self, ty: &str) -> Json {
        let mut o = Obj::new();
        o.str("type", ty).str("message", &self.message);
        if let Some(http) = &self.http {
            o.int("status", http.status as i64);
        }
        if let Some(body) = &self.body {
            let mut response = Obj::new();
            response.str("body", body);
            o.set("response", response.done());
        }
        if matches!(self.kind, Kind::RateLimit | Kind::QuotaExceeded | Kind::ProviderInternal) {
            if let Some(ms) = self.retry_after_ms {
                o.set("retryAfterMs", Json::Number(ms));
            }
        }
        o.done()
    }
}

fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    Some(authority.rsplit_once(':').map_or(authority, |(h, _)| h))
}

/// What Bun's fetch reports for a native failure: its code and message.
fn native(failure: &Failure, host: &str) -> (String, String) {
    match failure.kind {
        FailureKind::Closed => (
            "ECONNRESET".into(),
            "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()".into(),
        ),
        FailureKind::Refused => ("ConnectionRefused".into(), "Unable to connect. Is the computer able to access the url?".into()),
        FailureKind::Unresolved => ("ENOTFOUND".into(), format!("getaddrinfo ENOTFOUND {host}")),
        FailureKind::Tls => ("UNABLE_TO_VERIFY_LEAF_SIGNATURE".into(), failure.message.clone()),
        FailureKind::Other => (String::new(), failure.message.clone()),
    }
}

fn detail(code: &str, message: &str) -> String {
    if !code.is_empty() && !message.contains(code) { format!("{code}: {message}") } else { message.to_owned() }
}

/// A request that failed before any response (`httpError`, operation `request`).
pub fn transport(failure: &Failure, operation: &'static str, status: Option<u16>, url: &str) -> AIError {
    let (code, message) = native(failure, host_of(url).unwrap_or_default());
    AIError {
        operation: Some(operation),
        code: (!code.is_empty()).then(|| code.clone()),
        url: Some(url.to_owned()),
        http: status.map(|status| HttpContext { url: url.to_owned(), status, headers: vec![] }),
        ..AIError::new(Kind::Transport, detail(&code, &message))
    }
}

/// The response body failed while it was read (`Connection lost while reading the response`).
pub fn read_failure(failure: &Failure, status: Option<u16>, url: &str) -> AIError {
    let (code, message) = native(failure, host_of(url).unwrap_or_default());
    let text = detail(&code, &message);
    AIError {
        operation: Some("read"),
        code: (!code.is_empty()).then(|| code.clone()),
        url: Some(url.to_owned()),
        http: status.map(|status| HttpContext { url: url.to_owned(), status, headers: vec![] }),
        ..AIError::new(
            Kind::Transport,
            if text.is_empty() {
                "Connection lost while reading the response".to_owned()
            } else {
                format!("Connection lost while reading the response: {text}")
            },
        )
    }
}

pub fn timeout(operation: &'static str, message: &str, status: Option<u16>, url: &str) -> AIError {
    AIError {
        operation: Some(operation),
        code: Some("Timeout".into()),
        url: Some(url.to_owned()),
        http: status.map(|status| HttpContext { url: url.to_owned(), status, headers: vec![] }),
        ..AIError::new(Kind::Transport, message)
    }
}

fn retry_after_ms(headers: &[(String, String)]) -> Option<f64> {
    let get = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    let clamp = |ms: f64| ms.ceil().clamp(0.0, 9_007_199_254_740_991.0);
    if let Some(ms) = get("retry-after-ms").and_then(js_number) {
        return Some(clamp(ms));
    }
    let value = get("retry-after").filter(|v| !v.is_empty())?;
    if let Some(seconds) = js_number(value) {
        return Some(clamp(seconds * 1000.0));
    }
    parse_http_date(value).map(|date| clamp(date - crate::id::now() as f64))
}

/// `Number(text)` when finite, for the decimal forms headers carry (`Number("")` is 0).
fn js_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    if trimmed.chars().any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E') {
        return None;
    }
    trimmed.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// `Date.parse` of an IMF-fixdate (`Wed, 21 Oct 2015 07:28:00 GMT`), in epoch milliseconds.
fn parse_http_date(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let (day, month, year, time) = match parts.as_slice() {
        [_, day, month, year, time, ..] => (*day, *month, *year, *time),
        _ => return None,
    };
    let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let month = months.iter().position(|m| m.eq_ignore_ascii_case(month))? as i64 + 1;
    let day: i64 = day.parse().ok()?;
    let year: i64 = year.parse().ok()?;
    let mut hms = time.split(':').map(|p| p.parse::<i64>().ok());
    let (h, m, s) = (hms.next()??, hms.next()??, hms.next()??);
    // Days from civil (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + h * 3600 + m * 60 + s) * 1000) as f64)
}

/// Whether a rate-limit header set or a retry-after is present (`rateLimitDetails` is only kept for diagnostics).
const MAX_BODY_CHARS: usize = 2000;

/// `providerMessage`: the provider's own message, else the status with the raw body.
fn provider_message(status: u16, body: Option<&str>) -> String {
    let fallback = format!("Provider request failed with HTTP {status}");
    let text = body.map(str::trim).unwrap_or_default();
    if let Some(message) = provider_error_message(text) {
        return message;
    }
    let lower = text.to_ascii_lowercase();
    if text.is_empty() || lower.starts_with("<!doctype") || lower.starts_with("<html") {
        return fallback;
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() > MAX_BODY_CHARS {
        format!("{fallback}: {}…", String::from_utf16_lossy(&units[..MAX_BODY_CHARS]))
    } else {
        format!("{fallback}: {text}")
    }
}

fn non_blank(value: Option<&Json>) -> Option<String> {
    json::as_str(value?).filter(|s| s.chars().any(|c| !c.is_whitespace())).map(str::to_owned)
}

/// `providerErrorMessage`: the message of the common error body layouts, in priority order.
pub fn provider_error_message(body: &str) -> Option<String> {
    let value = json::parse(body)?;
    if !matches!(value, Json::Object(_)) {
        return None;
    }
    if let Some(error) = json::get(&value, "error").filter(|e| matches!(e, Json::Object(_))) {
        if let Some(message) = non_blank(json::get(error, "message")) {
            return Some(message);
        }
    }
    for key in ["error", "message", "Message", "detail"] {
        if let Some(message) = non_blank(json::get(&value, key)) {
            return Some(message);
        }
    }
    if let Some(Json::Array(errors)) = json::get(&value, "errors") {
        if !errors.is_empty() && errors.iter().all(|e| non_blank(json::get(e, "message")).is_some()) {
            return non_blank(json::get(&errors[0], "message"));
        }
    }
    None
}

/// `providerCodes`: the error codes a body carries, in their original case.
pub fn provider_codes(value: &Json) -> Vec<String> {
    let Json::Object(_) = value else { return vec![] };
    let obj = |v: Option<&Json>| v.filter(|v| matches!(v, Json::Object(_))).cloned();
    let error = obj(json::get(value, "error"));
    let inner = error.as_ref().and_then(|e| obj(json::get(e, "innererror")));
    let metadata = error.as_ref().and_then(|e| obj(json::get(e, "metadata")));
    let response = obj(json::get(value, "response"));
    let response_error = response.as_ref().and_then(|r| obj(json::get(r, "error")));
    let exception = obj(json::get(value, "exception"));
    let mut out = Vec::new();
    if let Some(Json::String(s)) = json::get(value, "code") {
        out.push(s.clone());
    }
    out.extend(aws_error_type(json::get(value, "__type").and_then(json::as_str)));
    out.extend(aws_error_type(json::get(value, "code").and_then(json::as_str)));
    let mut push = |v: Option<&Json>| {
        if let Some(Json::String(s)) = v {
            out.push(s.clone());
        }
    };
    push(json::get(value, "error_type"));
    if let Some(error) = &error {
        push(json::get(error, "code"));
        push(json::get(error, "type"));
        push(json::get(error, "status"));
        push(json::get(error, "error_type"));
        if let Some(Json::Array(details)) = json::get(error, "details") {
            for detail in details {
                push(json::get(detail, "reason"));
            }
        }
    }
    if let Some(inner) = &inner {
        push(json::get(inner, "code"));
    }
    if let Some(metadata) = &metadata {
        push(json::get(metadata, "error_type"));
    }
    if let Some(response_error) = &response_error {
        push(json::get(response_error, "code"));
    }
    if let Some(response) = &response {
        push(json::get(response, "error_type"));
    }
    if let Some(exception) = &exception {
        push(json::get(exception, "type"));
    }
    out
}

fn aws_error_type(value: Option<&str>) -> Vec<String> {
    let Some(value) = value else { return vec![] };
    let value = value.split(':').next().unwrap_or_default();
    vec![value.rsplit_once('#').map_or(value, |(_, name)| name).to_owned()]
}

fn codes_of_text(text: &str) -> Vec<String> {
    json::parse(text).map(|v| provider_codes(&v)).unwrap_or_default()
}

struct Patterns {
    overflow: Vec<Regex>,
    overflow_status: Regex,
    payload: Vec<Regex>,
    exclusions: Vec<Regex>,
    gateway: Regex,
    auth: Regex,
    rate_limit: Regex,
    quota: Regex,
    quota_exceeded: Regex,
    content_policy: Regex,
    server_error: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let i = |p: &str| Regex::new(&format!("(?i){p}")).expect("pattern");
        Patterns {
            overflow: [
                r"prompt is too long",
                r"input is too long for (?:the )?(?:requested|selected) model",
                r"too large for the model'?s configured input limit",
                r"exceeded this model context window limit",
                r"exceeds the context window",
                r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))",
                r"input token count.*exceeds the maximum",
                r"number of input tokens exceeds maximum length",
                r"tokens in request more than max tokens allowed",
                r"maximum prompt length is \d+",
                r"reduce the length of the messages",
                r"requested input length \d+ exceeds maximum input length",
                r"maximum context length is \d+ tokens",
                r"exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?",
                r"input(?: \(\d+ tokens\))? is longer than the model'?s context length",
                r"exceeds the limit of \d+",
                r"exceeds the available context size",
                r"greater than the context length",
                r"`inputs` tokens \+ `max_new_tokens` must be <= \d+",
                r"context window exceeds limit",
                r"context window exceeded",
                r"exceeded model token limit",
                r"context[_ ]length[_ ]exceeded",
                r"context length is only \d+ tokens",
                r"input length.*exceeds.*context length",
                r"prompt (?:too long|exceeds max length|超长)",
                r"too large for model with \d+ maximum context length",
                r"prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?",
                r"model_context_window_exceeded",
                r"range of input length should be",
                r"too many tokens",
                r"token limit exceeded",
                r"request_too_large",
            ]
            .iter()
            .map(|p| i(p))
            .collect(),
            overflow_status: i(r"^4(?:00|13)\s*(status code)?\s*\(no body\)"),
            payload: [r"request entity too large", r"payload too large", r"request too large"].iter().map(|p| i(p)).collect(),
            exclusions: [
                r"^(throttling error|service unavailable):",
                r"rate limit",
                r"too many requests",
                r"max[_ ]tokens must be less than",
                r"too many tokens(?: per day)?, please wait",
            ]
            .iter()
            .map(|p| i(p))
            .collect(),
            gateway: Regex::new(r"^[^:\n]+: \[([A-Za-z0-9_.-]+)\]").expect("pattern"),
            auth: i(r"incorrect api key provided"),
            rate_limit: i(r"rate increased too quickly|rate[-_\s]?limit|too[_\s]?many[_\s]?requests"),
            quota: i(
                r"insufficient[-_\s]?(?:quota|balance)|budget exceeded|usage limit|limit exhausted|package has expired|plan does not yet include",
            ),
            quota_exceeded: i(r"quota[-_\s]?exceeded"),
            content_policy: i(
                r"violating our usage policy|blocked by content filtering policy|content[-_\s]?policy|rejected (?:as a result of our|by the) safety system|detected potentially unsafe or sensitive content",
            ),
            server_error: i(
                r"\b(?:try again|(?:please |you can )?retry (?:the |this |your )?request|try (?:the |this |your )?request again|(?:currently |temporarily )?at capacity|overloaded|temporarily unavailable|service[-_\s]?unavailable|(?:server|internal)[-_\s]?error|server (?:is )?busy|provider returned (?:an )?error|resource[-_\s]?exhausted|upstream (?:connect|connection|request)|request buffer limit while retrying upstream)\b",
            ),
        }
    })
}

/// `QUOTA_TEXT`, whose `(?<!token_)quota[-_\s]?exceeded` needs a look-behind.
fn quota_text(text: &str) -> bool {
    let p = patterns();
    if p.quota.is_match(text) {
        return true;
    }
    p.quota_exceeded.find_iter(text).any(|m| !text[..m.start()].to_ascii_lowercase().ends_with("token_"))
}

pub fn is_context_overflow(message: &str) -> bool {
    let p = patterns();
    !p.exclusions.iter().any(|r| r.is_match(message))
        && (p.overflow.iter().any(|r| r.is_match(message)) || p.overflow_status.is_match(message))
}

const QUOTA_CODES: &[&str] = &["insufficient_quota", "usage_not_included", "billing_error", "gousagelimiterror", "budgetlimiterror", "freeusagelimiterror", "creditlimitexceeded"];
const AUTH_CODES: &[&str] = &["authentication_error", "permission_error", "permission_denied", "api_key_invalid"];
const SERVER_CODES: &[&str] = &["api_error", "internal_error", "internalserverexception", "modelstreamerrorexception", "overloaded_error", "server_error", "server_is_overloaded", "slow_down", "serviceunavailableexception"];
const INVALID_REQUEST_CODES: &[&str] = &["model_not_found", "invalid_prompt", "invalid_request", "invalid_request_error", "validationexception"];
const CONTENT_POLICY_CODES: &[&str] = &["content_filter", "content_moderation", "responsibleaipolicyviolation", "content_policy_violation", "image_content_policy_violation", "refusal", "datainspectionfailed", "moderation_blocked"];

/// `classifyProviderFailure`.
pub fn classify(message: &str, status: Option<u16>, body: Option<&str>, http: Option<HttpContext>, retry_after: Option<f64>) -> AIError {
    let p = patterns();
    let raw = body.unwrap_or_default();
    let mut codes: Vec<String> = codes_of_text(raw);
    codes.extend(codes_of_text(message));
    if let Some(captures) = p.gateway.captures(message) {
        codes.push(captures[1].to_owned());
    }
    if let Some(http) = &http {
        if let Some((_, value)) = http.headers.iter().find(|(k, _)| k == "x-amzn-errortype") {
            codes.extend(aws_error_type(Some(value)));
        }
    }
    let codes: Vec<String> = codes.into_iter().map(|c| c.to_lowercase()).collect();
    let has = |set: &[&str]| codes.iter().any(|c| set.contains(&c.as_str()));
    let text = [message, raw].iter().filter(|v| !v.is_empty()).copied().collect::<Vec<_>>().join("\n");
    let client = status.is_none_or(|s| (400..500).contains(&s));
    let base = |kind: Kind| AIError {
        body: body.map(str::to_owned),
        http: http.clone(),
        ..AIError::new(kind, message)
    };
    if client
        && (codes.iter().any(|c| c == "context_length_exceeded" || c == "model_context_window_exceeded" || c == "request_too_large")
            || (status != Some(429) && !codes.iter().any(|c| c == "throttlingexception") && is_context_overflow(&text)))
    {
        return AIError { classification: Some("context-overflow"), ..base(Kind::InvalidRequest) };
    }
    if status == Some(413) || p.payload.iter().any(|r| r.is_match(&text)) {
        return AIError { classification: Some("payload-too-large"), ..base(Kind::InvalidRequest) };
    }
    if has(CONTENT_POLICY_CODES) || (client && p.content_policy.is_match(message)) {
        return base(Kind::ContentPolicy);
    }
    if status == Some(402) || has(QUOTA_CODES) || (status == Some(429) && quota_text(&text)) {
        return AIError { retry_after_ms: retry_after, ..base(Kind::QuotaExceeded) };
    }
    if status == Some(401) || status == Some(403) || has(AUTH_CODES) || (status == Some(400) && p.auth.is_match(&text)) {
        return base(Kind::Authentication);
    }
    if status == Some(429)
        || codes.iter().any(|c| c.contains("rate_limit") || c == "too_many_requests" || c == "throttlingexception")
        || p.rate_limit.is_match(&text)
    {
        return AIError { retry_after_ms: retry_after, ..base(Kind::RateLimit) };
    }
    if status == Some(408)
        || status == Some(409)
        || status.is_some_and(|s| s >= 500)
        || (status.is_none_or(|s| s < 400)
            && ((!has(INVALID_REQUEST_CODES) && p.server_error.is_match(&text))
                || codes.iter().any(|c| SERVER_CODES.contains(&c.as_str()) || c.contains("exhausted") || c.contains("unavailable"))))
    {
        return AIError { retry_after_ms: retry_after, ..base(Kind::ProviderInternal) };
    }
    if has(INVALID_REQUEST_CODES) {
        return base(Kind::InvalidRequest);
    }
    if status.is_some_and(|s| (400..500).contains(&s)) {
        return base(Kind::InvalidRequest);
    }
    base(Kind::UnknownProvider)
}

/// A response with an error status (`statusError` / `httpFailure`).
pub fn http_status(status: u16, headers: &[(String, String)], body: Option<&str>, url: &str) -> AIError {
    let message = provider_message(status, body);
    let retry = retry_after_ms(headers);
    let http = HttpContext { url: url.to_owned(), status, headers: headers.to_vec() };
    classify(&message, Some(status), body, Some(http), retry)
}

/// An `error` frame in the stream (`step`'s `event.error`).
pub fn stream_error(message: &str, code: Option<&Json>, body: String) -> AIError {
    let status = match code {
        Some(Json::Number(n)) if n.fract() == 0.0 && *n >= 0.0 && *n < 65536.0 => Some(*n as u16),
        _ => None,
    };
    classify(message, status, Some(&body), None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1_445_412_480_000.0));
    }

    #[test]
    fn classification() {
        assert_eq!(classify("x", Some(429), None, None, None).kind, Kind::RateLimit);
        assert_eq!(classify("x", Some(500), None, None, None).kind, Kind::ProviderInternal);
        assert_eq!(classify("x", Some(401), None, None, None).kind, Kind::Authentication);
        assert_eq!(classify("prompt is too long", Some(400), None, None, None).classification, Some("context-overflow"));
        assert_eq!(provider_error_message(r#"{"error":{"message":"boom"}}"#).as_deref(), Some("boom"));
        assert!(quota_text("insufficient_quota"));
        assert!(!quota_text("token_quota_exceeded"));
    }
}
