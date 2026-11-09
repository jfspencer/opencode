//! marked 17.0.1's lexer (`Lexer.ts`, `Tokenizer.ts`, `rules.ts`, `helpers.ts`), the version `@opentui/core` 0.8.1
//! depends on, with the options OpenTUI lexes with (`{ gfm: true }`; `lexInline` uses the defaults, which are
//! the same grammar). Tokens keep marked's shape: a [`Token`] has the fields its `type` has in TS, and fields
//! TS leaves undefined are `None`.
//!
//! Divergences from the TS API:
//! - Strings are [`JsString`] (UTF-16), so `raw.length` and every offset count what TS counts.
//! - Block tokens are built before their inline tokens, as in marked: the lexer queues each inline source with
//!   a slot id and fills `tokens` after the block pass, in queue order, so the queue edits marked makes
//!   (`inlineQueue.at(-1).src = ...`, pops, the task-list rewrite) have the same effect.
//! - Extensions, hooks, `walkTokens` and the pedantic/breaks grammars are not ported: OpenTUI uses none.

mod lexer;
pub mod rules;

pub use lexer::{lex, lex_inline, lex_str, LexError};

use crate::js::JsString;

/// `token.type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenType {
    Space,
    Code,
    Heading,
    Table,
    Hr,
    Blockquote,
    List,
    ListItem,
    Paragraph,
    Html,
    Text,
    Def,
    Escape,
    Link,
    Image,
    Strong,
    Em,
    Codespan,
    Br,
    Del,
    Checkbox,
}

impl TokenType {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenType::Space => "space",
            TokenType::Code => "code",
            TokenType::Heading => "heading",
            TokenType::Table => "table",
            TokenType::Hr => "hr",
            TokenType::Blockquote => "blockquote",
            TokenType::List => "list",
            TokenType::ListItem => "list_item",
            TokenType::Paragraph => "paragraph",
            TokenType::Html => "html",
            TokenType::Text => "text",
            TokenType::Def => "def",
            TokenType::Escape => "escape",
            TokenType::Link => "link",
            TokenType::Image => "image",
            TokenType::Strong => "strong",
            TokenType::Em => "em",
            TokenType::Codespan => "codespan",
            TokenType::Br => "br",
            TokenType::Del => "del",
            TokenType::Checkbox => "checkbox",
        }
    }
}

/// A JS value that may be a string, `null` or absent (`undefined`), like a link's `title`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Nullable {
    #[default]
    Undefined,
    Null,
    Str(JsString),
}

impl Nullable {
    pub fn as_str(&self) -> Option<&JsString> {
        match self {
            Nullable::Str(s) => Some(s),
            _ => None,
        }
    }

    /// JS truthiness.
    pub fn truthy(&self) -> bool {
        matches!(self, Nullable::Str(s) if !s.is_empty())
    }
}

/// A table column alignment (`null` is `None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A list's `start`: a number for ordered lists, `""` otherwise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ListStart {
    Empty,
    Number(f64),
}

/// `Tokens.TableCell`.
#[derive(Clone, Debug, PartialEq)]
pub struct TableCell {
    pub text: JsString,
    pub tokens: Vec<Token>,
    pub header: bool,
    pub align: Option<Align>,
    pub(crate) slot: Option<usize>,
}

/// A marked token. `kind` is `type`; the other fields are those its type has (see `Tokens` in marked.d.ts),
/// `None` where the TS object lacks the property.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenType,
    pub raw: JsString,
    pub text: Option<JsString>,
    /// Child tokens. `None` for tokens without the property (space, code, hr, html, escape, codespan, br,
    /// checkbox, def, and text from the inline lexer).
    pub tokens: Option<Vec<Token>>,
    /// heading
    pub depth: Option<u32>,
    /// code: `lang` (fences always set it, possibly empty; indented code has none).
    pub lang: Option<JsString>,
    /// code: `codeBlockStyle: "indented"`.
    pub code_block_style_indented: bool,
    /// text: `escaped` (inline text only).
    pub escaped: Option<bool>,
    /// link, image, def
    pub href: Option<JsString>,
    /// link, image, def
    pub title: Nullable,
    /// def
    pub tag: Option<JsString>,
    /// list
    pub ordered: Option<bool>,
    pub start: Option<ListStart>,
    /// list, list_item
    pub loose: Option<bool>,
    pub items: Option<Vec<Token>>,
    /// list_item
    pub task: Option<bool>,
    /// list_item, checkbox
    pub checked: Option<bool>,
    /// html
    pub pre: Option<bool>,
    pub block: Option<bool>,
    pub in_link: Option<bool>,
    pub in_raw_block: Option<bool>,
    /// table
    pub header: Option<Vec<TableCell>>,
    pub align: Option<Vec<Option<Align>>>,
    pub rows: Option<Vec<Vec<TableCell>>>,
    /// The inline-queue slot whose tokens fill `tokens` after the block pass.
    pub(crate) slot: Option<usize>,
}

impl Token {
    pub(crate) fn new(kind: TokenType, raw: JsString) -> Token {
        Token {
            kind,
            raw,
            text: None,
            tokens: None,
            depth: None,
            lang: None,
            code_block_style_indented: false,
            escaped: None,
            href: None,
            title: Nullable::Undefined,
            tag: None,
            ordered: None,
            start: None,
            loose: None,
            items: None,
            task: None,
            checked: None,
            pre: None,
            block: None,
            in_link: None,
            in_raw_block: None,
            header: None,
            align: None,
            rows: None,
            slot: None,
        }
    }

    /// `token.text` or empty.
    pub fn text_or_empty(&self) -> JsString {
        self.text.clone().unwrap_or_default()
    }

    /// The child tokens, empty when the property is absent.
    pub fn children(&self) -> &[Token] {
        self.tokens.as_deref().unwrap_or(&[])
    }
}

/// `Tokens.Def`'s link: `{ href, title }` in `tokens.links`.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkDef {
    pub href: JsString,
    pub title: Nullable,
}

/// `TokensList`: the tokens plus the reference definitions (`tokens.links`) in insertion order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TokensList {
    pub tokens: Vec<Token>,
    pub links: Vec<(JsString, LinkDef)>,
}
