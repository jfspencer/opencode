//! The webfetch tool's HTML processing, ported from the TS reference:
//!
//! - [`html_to_markdown`]: `convertHTMLToMarkdown` (`packages/core/src/tool/html-markdown.ts`), with its output
//!   budget ([`MAX_MARKDOWN_BYTES`], and 64 KiB less for content) and truncation;
//! - [`html_to_text`]: `extractTextFromHTML` (`packages/core/src/tool/plugin/webfetch.ts`);
//! - [`webfetch`]: the other pure steps the tool applies to a response (Accept header, content-type checks, body
//!   limit, decoding, choice of conversion).
//!
//! Both converters run on a port of htmlparser2 8.0.2's tokenizer and parser (`parser.rs`), so they see exactly the
//! events the TS converters see, quirks included. Strings follow JS semantics: lone surrogates, UTF-16 chunking and
//! `TextEncoder` byte counts are reproduced (see `js.rs`); the returned `String` is the TS result as `TextEncoder`
//! would encode it (a lone surrogate, which TS can return, becomes U+FFFD).
//!
//! # Use in the webfetch tool
//!
//! After the URL and permission checks and the request (headers: `User-Agent` [`webfetch::USER_AGENT`], `Accept`
//! [`webfetch::accept_header`], `Accept-Language` [`webfetch::ACCEPT_LANGUAGE`]; redirects followed by the tool per
//! [`webfetch::is_redirect_status`], one retry on [`webfetch::is_cloudflare_challenge`]):
//!
//! ```
//! use opencode_html::webfetch::{self, BoundedBody, Format, MAX_RESPONSE_BYTES};
//!
//! # fn fetch(content_type: &str, content_length: Option<&str>, chunks: &[&[u8]]) -> Result<String, String> {
//! // `content_type` is the response's `content-type` header, `""` when absent.
//! webfetch::check_content_type(content_type)?;
//! let too_large = |_| webfetch::response_too_large_message();
//! let mut body = BoundedBody::new(content_length, MAX_RESPONSE_BYTES).map_err(too_large)?;
//! for chunk in chunks {
//!     body.push(chunk).map_err(too_large)?;
//! }
//! let body = body.finish();
//! let content = webfetch::decode_body(&body);
//! let output = webfetch::convert(&content, content_type, Format::Markdown);
//! # Ok(output.into_owned())
//! # }
//! # assert_eq!(fetch("text/html", None, &[b"<h1>Hi</h1>"]).unwrap(), "# Hi");
//! ```
//!
//! The tool result is then `{ url, contentType, format, output }`, with `contentType` the header as received.
//!
//! # Differential check
//!
//! `harness/run.sh` compares both converters and the helpers with the TS reference byte for byte on a corpus: the
//! HTML the conformance scenarios serve, the inputs of the upstream tests, hand-written pages (`harness/pages/`), and
//! generated tag soup, nested documents and edge cases (64 KiB chunk boundaries, end of input inside every
//! construct, list numbers, the 5 MiB budget). Expected outputs always come from running the TS functions in
//! `port/.reference` (set up by `port/scripts/worktree-setup.sh` in a worktree).
//!
//! ```sh
//! port/rust/crates/opencode-html/harness/run.sh                  # default corpus (seed 1, about 23k cases)
//! port/rust/crates/opencode-html/harness/run.sh --count 60000 --seed 7
//! port/rust/crates/opencode-html/harness/run.sh --coverage       # also report the crate lines the corpus reaches
//! bun harness/ts-one.ts m <file> [events]                        # one TS conversion (or its parser events)
//! target/release/examples/html-driver --m <file>                 # the same in Rust (--t for text)
//! ```

mod entities;
mod entities_data;
mod js;
mod markdown;
mod parser;
mod text;
pub mod webfetch;

/// html-markdown.ts `MAX_MARKDOWN_BYTES`: the most UTF-8 bytes [`html_to_markdown`] returns.
pub const MAX_MARKDOWN_BYTES: usize = 5 * 1024 * 1024;

/// `convertHTMLToMarkdown(html)`.
pub fn html_to_markdown(html: &str) -> String {
    markdown::convert(html)
}

/// `extractTextFromHTML(html)`.
pub fn html_to_text(html: &str) -> String {
    text::extract(html)
}
