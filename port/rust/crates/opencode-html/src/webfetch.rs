//! The pure parts of webfetch.ts, http-body.ts and mime.ts that handle a fetched body: the request's Accept header,
//! the content-type checks, the body size limit, decoding, and the choice of conversion.

use crate::js;
use std::borrow::Cow;

/// webfetch.ts `MAX_RESPONSE_BYTES`: the most body bytes read.
pub const MAX_RESPONSE_BYTES: usize = crate::MAX_MARKDOWN_BYTES;
/// webfetch.ts `DEFAULT_TIMEOUT_SECONDS`.
pub const DEFAULT_TIMEOUT_SECONDS: f64 = 30.0;
/// webfetch.ts `MAX_TIMEOUT_SECONDS`.
pub const MAX_TIMEOUT_SECONDS: f64 = 120.0;
/// webfetch.ts `MAX_REDIRECTS`.
pub const MAX_REDIRECTS: usize = 10;
/// The User-Agent of the first attempt.
pub const USER_AGENT: &str =
    "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; OpenCode-User/1.0; +https://opencode.ai";
/// The User-Agent of the retry after a Cloudflare challenge (403 with `cf-mitigated: challenge`).
pub const CHALLENGE_RETRY_USER_AGENT: &str = "opencode";
/// The `Accept-Language` header of every request.
pub const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";

/// The tool's `format` input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Text,
    Markdown,
    Html,
}

impl Format {
    /// The input literal (`"text"`, `"markdown"`, `"html"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Text => "text",
            Format::Markdown => "markdown",
            Format::Html => "html",
        }
    }

    pub fn parse(value: &str) -> Option<Format> {
        match value {
            "text" => Some(Format::Text),
            "markdown" => Some(Format::Markdown),
            "html" => Some(Format::Html),
            _ => None,
        }
    }
}

/// webfetch.ts `redirectStatuses`: a response with one of these and a `location` header is followed (the tool
/// follows redirects itself, checking each target against the permission rules).
pub fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// webfetch.ts `isCloudflareChallenge`: a 403 whose `cf-mitigated` header is `challenge` is retried once with
/// [`CHALLENGE_RETRY_USER_AGENT`].
pub fn is_cloudflare_challenge(status: u16, cf_mitigated: Option<&str>) -> bool {
    status == 403 && cf_mitigated == Some("challenge")
}

/// webfetch.ts `acceptHeader(format)`.
pub fn accept_header(format: Format) -> &'static str {
    match format {
        Format::Markdown => "text/markdown;q=1.0, text/x-markdown;q=0.9, text/plain;q=0.8, text/html;q=0.7, */*;q=0.1",
        Format::Text => "text/plain;q=1.0, text/markdown;q=0.9, text/html;q=0.8, */*;q=0.1",
        Format::Html => {
            "text/html;q=1.0, application/xhtml+xml;q=0.9, text/plain;q=0.8, text/markdown;q=0.7, */*;q=0.1"
        }
    }
}

/// webfetch.ts `mimeFrom(contentType)`: the part before the first `;`, trimmed and lower-cased.
pub fn mime_from(content_type: &str) -> String {
    let essence = content_type.split(';').next().unwrap_or("");
    js::to_lower_case(&js::into_string(js::trim(essence.as_bytes()).to_vec()))
}

/// webfetch.ts `isImageAttachment(mime)`.
pub fn is_image_attachment(mime: &str) -> bool {
    mime.starts_with("image/") && mime != "image/svg+xml" && mime != "image/vnd.fastbidsheet"
}

/// mime.ts `Mime.isTextual(mime)`.
pub fn is_textual(mime: &str) -> bool {
    mime.is_empty()
        || mime.starts_with("text/")
        || mime == "application/json"
        || mime.ends_with("+json")
        || mime == "application/xml"
        || mime.ends_with("+xml")
        || mime == "application/javascript"
        || mime == "application/x-javascript"
}

/// The tool's check of a response's `content-type` header (`""` when absent) before it reads the body: the error
/// message it fails with, if any.
pub fn check_content_type(content_type: &str) -> Result<(), String> {
    let mime = mime_from(content_type);
    if is_image_attachment(&mime) {
        return Err(format!("Unsupported fetched image content type: {mime}"));
    }
    if !is_textual(&mime) {
        return Err(format!("Unsupported fetched file content type: {mime}"));
    }
    Ok(())
}

/// The message of the error a body over [`MAX_RESPONSE_BYTES`] fails with.
pub fn response_too_large_message() -> String {
    format!("Response too large (exceeds {MAX_RESPONSE_BYTES} byte limit)")
}

/// http-body.ts: the size a `content-length` header declares, as `collectBoundedResponseBody` reads it
/// (`Number.parseInt(value, 10)`, kept only if a safe non-negative integer; an empty header declares nothing).
pub fn declared_content_length(value: &str) -> Option<u64> {
    if value.is_empty() {
        return None;
    }
    let s = value.trim_start_matches(|c: char| js::is_js_whitespace(c as u32));
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let digits = &digits[..digits.bytes().take_while(u8::is_ascii_digit).count()];
    if digits.is_empty() {
        return None;
    }
    let significant = digits.trim_start_matches('0');
    if significant.len() > 16 {
        return None;
    }
    let n: u64 = if significant.is_empty() { 0 } else { significant.parse().ok()? };
    if n > 9_007_199_254_740_991 || (negative && n != 0) {
        return None;
    }
    Some(n)
}

/// http-body.ts `collectBoundedResponseBody`: accumulates a body, failing (with [`response_too_large_message`]) as
/// soon as the declared or received size passes the limit.
pub struct BoundedBody {
    body: Vec<u8>,
    limit: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct TooLarge;

impl BoundedBody {
    /// Starts a body for a response with this `content-length` header (`None` when absent). A declared size over
    /// the limit fails before anything is read.
    pub fn new(content_length: Option<&str>, limit: usize) -> Result<Self, TooLarge> {
        let declared = content_length.and_then(declared_content_length);
        if declared.is_some_and(|d| d > limit as u64) {
            return Err(TooLarge);
        }
        Ok(BoundedBody { body: Vec::new(), limit })
    }

    /// Adds a received chunk; fails when the body would pass the limit.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), TooLarge> {
        if chunk.is_empty() {
            return Ok(());
        }
        if self.body.len() + chunk.len() > self.limit {
            return Err(TooLarge);
        }
        self.body.extend_from_slice(chunk);
        Ok(())
    }

    pub fn finish(self) -> Vec<u8> {
        self.body
    }
}

/// `new TextDecoder().decode(body)`: UTF-8, a leading byte order mark dropped, each invalid sequence replaced by
/// U+FFFD (WHATWG's maximal-subpart rule, which `String::from_utf8_lossy` follows).
pub fn decode_body(body: &[u8]) -> Cow<'_, str> {
    let body = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body);
    String::from_utf8_lossy(body)
}

/// webfetch.ts `convert(content, contentType, format)`: HTML (any `content-type` containing `text/html`, as
/// written) becomes Markdown or text; everything else, and HTML asked for as HTML, is returned as is.
pub fn convert<'a>(content: &'a str, content_type: &str, format: Format) -> Cow<'a, str> {
    if !content_type.contains("text/html") {
        return Cow::Borrowed(content);
    }
    match format {
        Format::Markdown => Cow::Owned(crate::html_to_markdown(content)),
        Format::Text => Cow::Owned(crate::html_to_text(content)),
        Format::Html => Cow::Borrowed(content),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(mime_from(" Text/HTML ; charset=utf-8"), "text/html");
        assert_eq!(check_content_type("image/png"), Err("Unsupported fetched image content type: image/png".into()));
        assert_eq!(
            check_content_type("application/zip"),
            Err("Unsupported fetched file content type: application/zip".into())
        );
        assert_eq!(check_content_type(""), Ok(()));
        assert_eq!(check_content_type("image/svg+xml"), Ok(()));
    }

    #[test]
    fn content_length() {
        assert_eq!(declared_content_length("99999999"), Some(99_999_999));
        assert_eq!(declared_content_length(" 12abc"), Some(12));
        assert_eq!(declared_content_length("-0"), Some(0));
        assert_eq!(declared_content_length("-1"), None);
        assert_eq!(declared_content_length("0x10"), Some(0));
        assert_eq!(declared_content_length("9007199254740992"), None);
        assert!(BoundedBody::new(Some("5242881"), MAX_RESPONSE_BYTES).is_err());
    }

    #[test]
    fn decodes_like_text_decoder() {
        assert_eq!(decode_body(b"\xEF\xBB\xBFhi"), "hi");
        assert_eq!(decode_body(b"a\xF0\x9F\x98"), "a\u{fffd}");
    }
}
