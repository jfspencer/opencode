//! `Markdown.ts`'s inline rendering: `getStyle`, `createChunk`, `renderInlineContent`, `renderInlineToken`,
//! `renderInlineTokenWithStyle` and `createInitialStyledText`, which turn marked inline tokens into chunks (the
//! streaming preview and table cells), plus `isSupportedLinkTarget`.

use crate::js::{JsStr, JsString};
use crate::marked::{self, Token, TokenType};
use crate::style::{StyleDefinition, StyledText, SyntaxStyle, TextChunk};

/// `MAX_LINK_URL_BYTES` (zig.ts): the longest hyperlink target the native core stores.
pub const MAX_LINK_URL_BYTES: usize = 512;

/// `isSupportedLinkTarget(url)`.
pub fn is_supported_link_target(url: &[u16]) -> bool {
    if url.len() > MAX_LINK_URL_BYTES {
        return false;
    }
    String::from_utf16_lossy(url).len() <= MAX_LINK_URL_BYTES
}

/// What chunk creation reads from the renderable.
#[derive(Clone, Copy)]
pub struct InlineContext<'a> {
    pub syntax_style: Option<&'a SyntaxStyle>,
    pub conceal: bool,
    /// `ctx.capabilities?.hyperlinks === true`.
    pub hyperlinks: bool,
    pub streaming: bool,
}

impl<'a> InlineContext<'a> {
    /// `getStyle(group)`: the style or its base scope's (none before the syntax style is set).
    pub fn get_style(&self, group: &str) -> Option<StyleDefinition> {
        let style = self.syntax_style?;
        let mut found = style.get_style(group);
        if found.is_none() && group.contains('.') {
            found = style.get_style(group.split('.').next().unwrap());
        }
        found
    }

    /// `createChunk(text, group, link)`.
    pub fn create_chunk(&self, text: JsString, group: &str, link: Option<&JsString>) -> TextChunk {
        let style = self.get_style(group).or_else(|| self.get_style("default"));
        let mut chunk = TextChunk::styled(text, style.as_ref());
        chunk.link = link.cloned();
        chunk
    }

    fn chunk(&self, text: &str, group: &str, link: Option<&JsString>) -> TextChunk {
        self.create_chunk(JsString::from(text), group, link)
    }

    /// `createDefaultChunk(text, link)`.
    pub fn create_default_chunk(&self, text: JsString, link: Option<&JsString>) -> TextChunk {
        self.create_chunk(text, "default", link)
    }

    /// `createInitialStyledText(token)`: the streaming preview of a block, from marked's inline tokens.
    pub fn create_initial_styled_text(&self, token: &Token) -> Option<StyledText> {
        if !self.streaming {
            return None;
        }
        let mut chunks = Vec::new();
        if let Some(tokens) = &token.tokens {
            self.render_inline_content(tokens, &mut chunks, None);
        }
        if chunks.is_empty() {
            if let Some(text) = &token.text {
                // lexInline throws only where lex would have; TS would propagate it, which cannot happen for
                // text lex already accepted.
                if let Ok(tokens) = marked::lex_inline(text) {
                    self.render_inline_content(&tokens, &mut chunks, None);
                }
            }
        }
        if chunks.is_empty() { None } else { Some(StyledText::new(chunks)) }
    }

    /// `renderInlineContent(tokens, chunks, link)`.
    pub fn render_inline_content(&self, tokens: &[Token], chunks: &mut Vec<TextChunk>, link: Option<&JsString>) {
        for token in tokens {
            self.render_inline_token(token, chunks, link);
        }
    }

    fn render_children_with_style(&self, token: &Token, chunks: &mut Vec<TextChunk>, group: &str, link: Option<&JsString>) {
        for child in token.children() {
            self.render_inline_token_with_style(child, chunks, group, link);
        }
    }

    /// `renderInlineToken(token, chunks, link)`.
    pub fn render_inline_token(&self, token: &Token, chunks: &mut Vec<TextChunk>, link: Option<&JsString>) {
        match token.kind {
            TokenType::Text | TokenType::Escape => chunks.push(self.create_default_chunk(token.text_or_empty(), link)),
            TokenType::Codespan => {
                // Codespans drop the inherited link, as in TS.
                if self.conceal {
                    chunks.push(self.create_chunk(token.text_or_empty(), "markup.raw", None));
                } else {
                    chunks.push(self.chunk("`", "markup.raw", None));
                    chunks.push(self.create_chunk(token.text_or_empty(), "markup.raw", None));
                    chunks.push(self.chunk("`", "markup.raw", None));
                }
            }
            TokenType::Strong | TokenType::Em | TokenType::Del => {
                let (marker, group) = match token.kind {
                    TokenType::Strong => ("**", "markup.strong"),
                    TokenType::Em => ("*", "markup.italic"),
                    _ => ("~~", "markup.strikethrough"),
                };
                if !self.conceal {
                    chunks.push(self.chunk(marker, group, link));
                }
                self.render_children_with_style(token, chunks, group, link);
                if !self.conceal {
                    chunks.push(self.chunk(marker, group, link));
                }
            }
            TokenType::Link => {
                let href = token.href.clone().unwrap_or_default();
                let link_href = Some(&href);
                let text = token.text_or_empty();
                let angle = {
                    let mut a = JsString::from("<");
                    a.push_str(&text);
                    a.push_literal(">");
                    a
                };
                if self.conceal {
                    self.render_children_with_style(token, chunks, "markup.link.label", link_href);
                    if (!self.hyperlinks || !is_supported_link_target(&href))
                        && text != href
                        && token.raw != text
                        && token.raw != angle
                    {
                        chunks.push(self.chunk(" (", "markup.link", link_href));
                        chunks.push(self.create_chunk(href.clone(), "markup.link.url", link_href));
                        chunks.push(self.chunk(")", "markup.link", link_href));
                    }
                } else if token.raw == text || token.raw == angle {
                    let starts_angle = token.raw.js_starts_with(&[b'<' as u16]);
                    if starts_angle {
                        chunks.push(self.chunk("<", "markup.link", link_href));
                    }
                    self.render_children_with_style(token, chunks, "markup.link.label", link_href);
                    if starts_angle {
                        chunks.push(self.chunk(">", "markup.link", link_href));
                    }
                } else {
                    chunks.push(self.chunk("[", "markup.link", link_href));
                    self.render_children_with_style(token, chunks, "markup.link.label", link_href);
                    chunks.push(self.chunk("](", "markup.link", link_href));
                    chunks.push(self.create_chunk(href.clone(), "markup.link.url", link_href));
                    chunks.push(self.chunk(")", "markup.link", link_href));
                }
            }
            TokenType::Image => {
                let own = token.href.clone().unwrap_or_default();
                let image_href = link.unwrap_or(&own);
                let text = token.text_or_empty();
                if self.conceal {
                    let label = if text.is_empty() { JsString::from("image") } else { text };
                    chunks.push(self.create_chunk(label, "markup.link.label", Some(image_href)));
                } else {
                    chunks.push(self.chunk("![", "markup.link", Some(image_href)));
                    chunks.push(self.create_chunk(text, "markup.link.label", Some(image_href)));
                    chunks.push(self.chunk("](", "markup.link", Some(image_href)));
                    chunks.push(self.create_chunk(own.clone(), "markup.link.url", Some(image_href)));
                    chunks.push(self.chunk(")", "markup.link", Some(image_href)));
                }
            }
            TokenType::Br => chunks.push(self.create_default_chunk(JsString::from("\n"), link)),
            _ => {
                if let Some(tokens) = &token.tokens {
                    self.render_inline_content(tokens, chunks, link);
                } else if let Some(text) = &token.text {
                    chunks.push(self.create_default_chunk(text.clone(), link));
                }
            }
        }
    }

    /// `renderInlineTokenWithStyle(token, chunks, styleGroup, link)`.
    pub fn render_inline_token_with_style(&self, token: &Token, chunks: &mut Vec<TextChunk>, group: &str, link: Option<&JsString>) {
        match token.kind {
            TokenType::Text | TokenType::Escape => chunks.push(self.create_chunk(token.text_or_empty(), group, link)),
            TokenType::Codespan => {
                if self.conceal {
                    chunks.push(self.create_chunk(token.text_or_empty(), "markup.raw", link));
                } else {
                    chunks.push(self.chunk("`", "markup.raw", link));
                    chunks.push(self.create_chunk(token.text_or_empty(), "markup.raw", link));
                    chunks.push(self.chunk("`", "markup.raw", link));
                }
            }
            _ => self.render_inline_token(token, chunks, link),
        }
    }
}
