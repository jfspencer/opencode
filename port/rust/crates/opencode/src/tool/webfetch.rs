//! The webfetch tool (packages/core/src/tool/plugin/webfetch.ts, tool/http-body.ts): fetches an HTTP(S) URL and returns
//! its body as Markdown (the default), text or HTML. The body processing is `opencode-html`'s; this module keeps the
//! URL checks, the permission checks (the URL as written is asked for; every redirect target must not be denied), the
//! request (through [`crate::llm::http`], so it honors the proxy settings), the redirects it follows itself (at most
//! [`webfetch::MAX_REDIRECTS`]), the one retry of a Cloudflare challenge with another User-Agent, and the timeout
//! around the request and the body.
//!
//! A failure is `ToolFailure("Unable to fetch <url>", error)`, which the session error unwraps to its cause: plain
//! errors are `unknown` with their own message, permission errors `permission.rejected`.
//!
//! TODO(port): Effect's redaction of signed query parameters (`X-Amz-Signature` and friends) in the status error's URL.

use opencode_html::webfetch::{self, BoundedBody, Format, MAX_RESPONSE_BYTES};
use opencode_protocol::Json;

use super::arguments::{self, Issue};
use super::{Context, Definition, Execution, Failure, Output, Tool, ToolError};
use crate::json::Obj;
use crate::llm::http;
use crate::url::Url;

const NAME: &str = "webfetch";

pub struct WebFetch {
    pub definition: Definition,
}

/// The decoded input (`Input`): `format` defaults to markdown, `timeout` is in seconds.
struct Input {
    url: String,
    format: Format,
    timeout: Option<f64>,
}

impl Input {
    /// The decoded input as the permission request shows it (`metadata: input`).
    fn metadata(&self, url: &str) -> Json {
        let mut o = Obj::new();
        o.str("url", url).str("format", self.format.as_str());
        o.opt("timeout", self.timeout.map(Json::Number));
        o.done()
    }
}

/// `Schema.decodeUnknown(Input)` with every issue reported.
fn decode(value: &Json) -> Result<Input, Vec<Issue>> {
    let fields = arguments::object(value, "object").map_err(|issue| vec![issue])?;
    let mut issues = Vec::new();
    let url = match fields.get("url") {
        Some(Json::String(url)) => Some(url.clone()),
        Some(_) => {
            issues.push(Issue::new("url", "Expected string"));
            None
        }
        None => {
            issues.push(Issue::new("url", "Missing key"));
            None
        }
    };
    let format = match fields.get("format") {
        None => Some(Format::Markdown),
        Some(Json::String(text)) if Format::parse(text).is_some() => Format::parse(text),
        Some(_) => {
            issues.push(Issue::new("format", "Expected \"text\" | \"markdown\" | \"html\""));
            None
        }
    };
    let timeout = match fields.get("timeout") {
        None => None,
        Some(Json::Number(n)) => {
            let n = *n;
            if !n.is_finite() {
                issues.push(Issue::new("timeout", "Expected a finite number"));
            }
            if n.is_nan() || n <= 0.0 {
                issues.push(Issue::new("timeout", "Expected a value greater than 0"));
            }
            if n.is_nan() || n > webfetch::MAX_TIMEOUT_SECONDS {
                issues.push(Issue::new("timeout", "Expected a value less than or equal to 120"));
            }
            Some(n)
        }
        Some(_) => {
            issues.push(Issue::new("timeout", "Expected number"));
            None
        }
    };
    match (url, format) {
        (Some(url), Some(format)) if issues.is_empty() => Ok(Input { url, format, timeout }),
        _ => Err(issues),
    }
}

/// An error the tool wraps (`ToolFailure`'s cause), as the session error shows it.
enum Cause {
    /// A plain `Error` (`{type: "unknown", message}`).
    Plain(String),
    /// A permission failure, already a tool failure.
    Permission(Failure),
    /// `HttpClientError` of a non-2xx response; a Cloudflare challenge is retried.
    Status { status: u16, url: String, challenge: bool },
}

impl From<String> for Cause {
    fn from(message: String) -> Self {
        Cause::Plain(message)
    }
}

/// `assertHttpUrl`.
fn check_url(url: &Url) -> Result<(), String> {
    if url.scheme != "http" && url.scheme != "https" {
        return Err("URL must use http:// or https://".into());
    }
    if !url.username.is_empty() || !url.password.is_empty() {
        return Err("URL must not include credentials".into());
    }
    Ok(())
}

fn header<'a>(response: &'a http::Response, name: &str) -> Option<&'a str> {
    response.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

/// One GET of `url` (as written; `target` is its parsed form), following redirects (`execute`).
async fn execute(
    context: &Context,
    input: &Input,
    url: &str,
    target: &Url,
    user_agent: &str,
) -> Result<http::Response, Cause> {
    let mut url = url.to_owned();
    let mut target = target.clone();
    let mut redirects = 0;
    loop {
        let headers = [
            ("User-Agent".to_owned(), user_agent.to_owned()),
            ("Accept".to_owned(), webfetch::accept_header(input.format).to_owned()),
            ("Accept-Language".to_owned(), webfetch::ACCEPT_LANGUAGE.to_owned()),
        ];
        let response = http::send("GET", &target.href(), &headers, String::new())
            .await
            .map_err(|_| Cause::Plain(format!("Transport error (GET {url})")))?;
        let location = header(&response, "location").filter(|l| !l.is_empty()).map(str::to_owned);
        let Some(location) = location.filter(|_| webfetch::is_redirect_status(response.status)) else {
            if (200..300).contains(&response.status) {
                return Ok(response);
            }
            let challenge = webfetch::is_cloudflare_challenge(response.status, header(&response, "cf-mitigated"));
            return Err(Cause::Status { status: response.status, url, challenge });
        };
        drop(response);
        if redirects >= webfetch::MAX_REDIRECTS {
            return Err(format!("Too many redirects (more than {})", webfetch::MAX_REDIRECTS).into());
        }
        let mut next = target.join(&location).ok_or_else(|| "Invalid URL".to_owned())?;
        check_url(&next)?;
        next.fragment = None;
        let href = next.href();
        context
            .assert_not_denied(crate::permission::Ask {
                action: NAME.into(),
                resources: vec![href.clone()],
                save: None,
                metadata: Some(input.metadata(&href)),
            })
            .map_err(Cause::Permission)?;
        url = href;
        target = next;
        redirects += 1;
    }
}

/// The request and the body, under the timeout: the response's content type and body.
async fn fetch(context: &Context, input: &Input, url: &str, target: &Url) -> Result<(String, Vec<u8>), Cause> {
    let mut response = match execute(context, input, url, target, webfetch::USER_AGENT).await {
        Err(Cause::Status { challenge: true, .. }) => {
            execute(context, input, url, target, webfetch::CHALLENGE_RETRY_USER_AGENT).await?
        }
        other => other?,
    };
    let content_type = header(&response, "content-type").unwrap_or_default().to_owned();
    webfetch::check_content_type(&content_type)?;
    let too_large = |_| Cause::Plain(webfetch::response_too_large_message());
    let mut body = BoundedBody::new(header(&response, "content-length"), MAX_RESPONSE_BYTES).map_err(too_large)?;
    while let Some(chunk) = response.body.next().await {
        let chunk = chunk.map_err(|_| Cause::Plain(format!("Transport error (GET {url})")))?;
        body.push(&chunk).map_err(too_large)?;
    }
    Ok((content_type, body.finish()))
}

async fn run(context: Context, input: Input) -> Result<Output, Cause> {
    // Rules match the URL as written; the fragment is never sent.
    let url = input.url.split('#').next().unwrap_or_default().to_owned();
    let target = Url::parse(&url).ok_or_else(|| "Invalid URL".to_owned())?;
    check_url(&target)?;
    context
        .assert(crate::permission::Ask {
            action: NAME.into(),
            resources: vec![url.clone()],
            save: Some(vec!["*".into()]),
            metadata: Some(input.metadata(&input.url)),
        })
        .await
        .map_err(Cause::Permission)?;
    let seconds = input.timeout.unwrap_or(webfetch::DEFAULT_TIMEOUT_SECONDS);
    let limit = std::time::Duration::from_secs_f64(seconds);
    let (content_type, body) = tokio::time::timeout(limit, fetch(&context, &input, &url, &target))
        .await
        .map_err(|_| Cause::Plain("Request timed out".into()))??;
    let format = input.format;
    let type_for_conversion = content_type.clone();
    // The conversions are CPU-bound (up to 5 MiB of HTML).
    let output = tokio::task::spawn_blocking(move || {
        let content = webfetch::decode_body(&body);
        webfetch::convert(&content, &type_for_conversion, format).into_owned()
    })
    .await
    .map_err(|_| Cause::Plain("Unable to convert the response".into()))?;
    let mut text = Obj::new();
    text.str("type", "text").str("text", &output);
    let mut metadata = Obj::new();
    metadata.str("contentType", &content_type);
    Ok(Output { content: vec![text.done()], metadata: Some(metadata.done()) })
}

impl Tool for WebFetch {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a> {
        Box::pin(async move {
            let decoded = decode(&input).map_err(|issues| Failure::Error(arguments::invalid(NAME, &issues, &input)))?;
            let url = decoded.url.clone();
            run(context, decoded).await.map_err(|cause| match cause {
                Cause::Permission(failure) => failure,
                Cause::Plain(message) => Failure::Error(ToolError { kind: "unknown", message, metadata: None }),
                Cause::Status { status, url: requested, .. } => Failure::Error(ToolError {
                    kind: "unknown",
                    message: format!("StatusCode: non 2xx status code ({status} GET {requested})"),
                    metadata: None,
                }),
            })
            .map_err(|failure| match failure {
                // A cause without a message keeps the tool's own (`Unable to fetch <url>`).
                Failure::Error(error) if error.message.is_empty() => {
                    Failure::Error(ToolError::new(format!("Unable to fetch {url}")))
                }
                other => other,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn issues(input: &str) -> Vec<String> {
        match decode(&json::parse(input).unwrap()) {
            Ok(_) => vec![],
            Err(issues) => issues.into_iter().map(|i| format!("{}: {}", i.path, i.message)).collect(),
        }
    }

    #[test]
    fn decoding() {
        assert_eq!(issues(r#"{"url":"x"}"#), Vec::<String>::new());
        assert_eq!(issues(r#""s""#), [": Expected object"]);
        assert_eq!(issues(r#"{}"#), ["url: Missing key"]);
        assert_eq!(issues(r#"{"url":"x","timeout":-1,"format":3}"#), [
            "format: Expected \"text\" | \"markdown\" | \"html\"",
            "timeout: Expected a value greater than 0"
        ]);
        assert_eq!(issues(r#"{"url":"x","timeout":1e400}"#), [
            "timeout: Expected a finite number",
            "timeout: Expected a value less than or equal to 120"
        ]);
        assert_eq!(issues(r#"{"url":"x","timeout":"1"}"#), ["timeout: Expected number"]);
    }
}
