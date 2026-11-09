//! `markdown-parser.ts`: incremental lexing that keeps the tokens whose `raw` still prefixes the content.

use crate::js::{JsStr, JsString};
use crate::marked::{self, Token};
use std::rc::Rc;

/// `ParseState`. Tokens are shared (`Rc`) so a reused token keeps its identity across parses, as the TS objects
/// do: `Markdown.ts` compares tokens with `===` (`Rc::ptr_eq` here).
#[derive(Clone, Debug, Default)]
pub struct ParseState {
    pub content: JsString,
    pub tokens: Vec<Rc<Token>>,
    pub stable_token_count: usize,
}

fn lex(content: &[u16]) -> Result<Vec<Rc<Token>>, marked::LexError> {
    Ok(marked::lex(content)?.tokens.into_iter().map(Rc::new).collect())
}

/// `parseMarkdownIncremental(newContent, prevState, trailingUnstable)`.
pub fn parse_markdown_incremental(
    new_content: &JsString,
    prev_state: Option<&ParseState>,
    trailing_unstable: usize,
) -> ParseState {
    let prev = match prev_state {
        Some(p) if !p.tokens.is_empty() => p,
        _ => {
            return match lex(new_content) {
                Ok(tokens) => {
                    let stable = tokens.len().saturating_sub(trailing_unstable);
                    ParseState { content: new_content.clone(), tokens, stable_token_count: stable }
                }
                Err(_) => ParseState { content: new_content.clone(), tokens: Vec::new(), stable_token_count: 0 },
            };
        }
    };

    let mut offset = 0;
    let mut reuse_count: usize = 0;
    for token in &prev.tokens {
        let len = token.raw.len();
        if offset + len <= new_content.len() && new_content.starts_with_at(&token.raw, offset) {
            reuse_count += 1;
            offset += len;
        } else {
            break;
        }
    }
    reuse_count = reuse_count.saturating_sub(trailing_unstable);
    let offset: usize = prev.tokens[..reuse_count].iter().map(|t| t.raw.len()).sum();
    let stable: Vec<Rc<Token>> = prev.tokens[..reuse_count].to_vec();
    let remaining = new_content.js_slice(offset as isize, None);
    if remaining.is_empty() {
        let n = stable.len();
        return ParseState { content: new_content.clone(), tokens: stable, stable_token_count: n };
    }
    match lex(&remaining) {
        Ok(new_tokens) => {
            let stable_count = if trailing_unstable == 0 { stable.len() + new_tokens.len() } else { stable.len() };
            let mut tokens = stable;
            tokens.extend(new_tokens);
            ParseState { content: new_content.clone(), tokens, stable_token_count: stable_count }
        }
        Err(_) => match lex(new_content) {
            Ok(tokens) => ParseState { content: new_content.clone(), tokens, stable_token_count: 0 },
            Err(_) => ParseState { content: new_content.clone(), tokens: Vec::new(), stable_token_count: 0 },
        },
    }
}
