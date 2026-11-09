//! `markdown-parser.ts` and the content side of `Markdown.ts`.
//!
//! [`Markdown`] is `MarkdownRenderable` without drawing: it holds the parse state and the block states, and
//! builds, updates in place or replaces child "renderables" exactly as TS does, as plain [`Node`] data. A
//! renderer keeps one real renderable per [`Node::uid`] (a new uid is a new TS object: destroy the old one, create
//! the new), sets each node's props on it, replays a [`CodeNode`]'s props when its `applies` count moves (see
//! there for the order), and runs [`crate::links::add_markdown_link_highlights`] and
//! [`crate::links::detect_links`] as the `onHighlight`/`onChunks` hooks of nodes with `markdown_hooks`.
//!
//! Divergences: the `renderNode` hook is not ported. The TUI passes `plugins.markdown()`, which is
//! `createMarkdownCodeBlockRenderer(...)` of the code-block renderers plugins register, or undefined when none
//! do; no built-in feature plugin registers one, so it is always undefined without external plugins (out of
//! scope). [`create_markdown_code_block_renderer_language`] is the language lookup such a renderer uses.

pub mod inline;
mod model;
pub mod parser;
pub mod table;

pub use model::*;
pub use parser::{parse_markdown_incremental, ParseState};

use crate::marked::{Token, TokenType};
use crate::resolve_ft::info_string_to_filetype;

/// The language `createMarkdownCodeBlockRenderer`'s render hook looks a renderer up by: the filetype of a code
/// token's info string (`None` for other tokens or no language).
pub fn create_markdown_code_block_renderer_language(token: &Token) -> Option<String> {
    if token.kind != TokenType::Code {
        return None;
    }
    info_string_to_filetype(&token.lang.clone().unwrap_or_default().to_string())
}
