//! The webfetch tool's HTML processing, ported from the TS reference:
//!
//! - [`html_to_markdown`]: `convertHTMLToMarkdown` (`packages/core/src/tool/html-markdown.ts`), with its output
//!   budget ([`MAX_MARKDOWN_BYTES`], and 64 KiB less for content) and truncation;
//! - [`html_to_text`]: `extractTextFromHTML` (`packages/core/src/tool/plugin/webfetch.ts`);
//! - [`webfetch`]: the other pure steps the tool applies to a response (Accept header, content-type checks, body
//!   limit, decoding, choice of conversion).
//!
//! Both converters run on a port of htmlparser2 8.0.2's tokenizer and parser ([`parser`]), so they see exactly the
//! events the TS converters see, quirks included. Strings follow JS semantics: lone surrogates, UTF-16 chunking and
//! `TextEncoder` byte counts are reproduced (see `js.rs`); the returned `String` is the TS result as `TextEncoder`
//! would encode it (a lone surrogate, which TS can return, becomes U+FFFD).
//!
//! # Differential check
//!
//! `harness/run.sh` compares both converters with the TS reference byte for byte on a corpus: the HTML the
//! conformance scenarios serve, the inputs of the upstream tests, hand-written pages (`harness/pages/`), and generated
//! tag soup and edge cases around the 64 KiB chunks and the 5 MiB budget. Expected outputs always come from running
//! the TS functions in `port/.reference` (set up by `port/scripts/worktree-setup.sh` in a worktree).
//!
//! ```sh
//! port/rust/crates/opencode-html/harness/run.sh                  # default corpus (seed 1)
//! port/rust/crates/opencode-html/harness/run.sh --count 50000 --seed 7
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
