//! What OpenTUI 0.8.1's Markdown, Code, Diff and TextTable renderables compute, for the Rust TUI: everything up to
//! styled text and structure, which `opencode-tui-core`'s renderables lay out and draw.
//!
//! - [`marked`]: the `marked` 17.0.1 lexer.
//! - [`markdown`]: `markdown-parser.ts` and `Markdown.ts`'s block model, inline chunks and tables.
//! - [`tree_sitter`]: `lib/tree-sitter` (parsers, queries, injections, conceal metadata), [`styled_text`]:
//!   `tree-sitter-styled-text.ts`, [`code`]: `Code.ts`'s highlight pipeline, [`links`]: `detect-links.ts` and the
//!   markdown link highlights, [`resolve_ft`]: filetype resolution.
//! - [`diff`]: `Diff.ts`'s parsing (jsdiff's `parsePatch`) and line model.
//! - [`table`]: `TextTable.ts`'s layout and `text-table-width.ts`.
//! - [`style`]: RGBA, syntax style definitions, text chunks and styled text.
//!
//! Text is UTF-16 ([`js::JsString`]) and every offset (highlights, source ranges) counts UTF-16 code units, as in
//! TS.

pub mod code;
pub mod highlight;
pub mod js;
pub mod links;
pub mod markdown;
pub mod marked;
pub mod resolve_ft;
pub mod style;
pub mod styled_text;
pub mod tree_sitter;
