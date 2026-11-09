//! `Lexer.ts` and `Tokenizer.ts`, method for method. See the module docs of `marked` for the inline queue.

use super::rules::{BLOCK, INLINE, OTHER};
use super::{Align, LinkDef, ListStart, Nullable, TableCell, Token, TokenType, TokensList};
use crate::js::{u, JsRegex, JsStr, JsString, Replacement};

/// An error marked throws: `Infinite loop on byte: <code>`, or a TypeError where its code dereferences a value
/// that is missing (both make OpenTUI fall back to no tokens).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError(pub String);

type R<T> = Result<T, LexError>;

/// [`lex`] on a Rust string.
pub fn lex_str(src: &str) -> R<TokensList> {
    lex(&JsString::from(src))
}

/// `Lexer.lex(src, { gfm: true })`.
pub fn lex(src: &[u16]) -> R<TokensList> {
    let mut lexer = Lexer::new();
    lexer.lex(src)
}

/// `Lexer.lexInline(src)`.
pub fn lex_inline(src: &[u16]) -> R<Vec<Token>> {
    let mut lexer = Lexer::new();
    lexer.inline_tokens(src)
}

struct QueueEntry {
    src: JsString,
    slot: usize,
}

struct Lexer {
    links: Vec<(JsString, LinkDef)>,
    inline_queue: Vec<QueueEntry>,
    next_slot: usize,
    in_link: bool,
    in_raw_block: bool,
    top: bool,
}

/// `rtrim(str, c, invert)`.
fn rtrim(s: &[u16], c: u16, invert: bool) -> JsString {
    let l = s.len();
    let mut suff = 0;
    while suff < l {
        let cur = s[l - suff - 1];
        if (cur == c && !invert) || (cur != c && invert) {
            suff += 1;
        } else {
            break;
        }
    }
    JsString(s[..l - suff].to_vec())
}

/// `splitCells(tableRow, count)`.
fn split_cells(row: &[u16], count: Option<usize>) -> Vec<JsString> {
    let mut f = |m: &crate::js::JsMatch, s: &[u16]| {
        let mut escaped = false;
        let mut curr = m.start as isize;
        loop {
            curr -= 1;
            if curr >= 0 && s[curr as usize] == b'\\' as u16 {
                escaped = !escaped;
            } else {
                break;
            }
        }
        if escaped { JsString::from("|") } else { JsString::from(" |") }
    };
    let row = OTHER.find_pipe.replace(row, Replacement::Func(&mut f));
    let mut cells: Vec<JsString> = OTHER.split_pipe.split(&row).into_iter().map(|c| c.unwrap_or_default()).collect();
    if cells.first().is_some_and(|c| c.js_trim().is_empty()) {
        cells.remove(0);
    }
    if !cells.is_empty() && cells.last().is_some_and(|c| c.js_trim().is_empty()) {
        cells.pop();
    }
    if let Some(count) = count.filter(|&c| c > 0) {
        if cells.len() > count {
            cells.truncate(count);
        } else {
            while cells.len() < count {
                cells.push(JsString::new());
            }
        }
    }
    cells.into_iter().map(|c| OTHER.slash_pipe.replace_str(&c.js_trim(), "|")).collect()
}

/// `findClosingBracket(str, b)`.
fn find_closing_bracket(s: &[u16], open: u16, close: u16) -> isize {
    if !s.contains(&close) {
        return -1;
    }
    let mut level = 0i64;
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\' as u16 {
            i += 1;
        } else if s[i] == open {
            level += 1;
        } else if s[i] == close {
            level -= 1;
            if level < 0 {
                return i as isize;
            }
        }
        i += 1;
    }
    if level > 0 { -2 } else { -1 }
}

/// `indentCodeCompensation(raw, text)`.
fn indent_code_compensation(raw: &[u16], text: &[u16]) -> JsString {
    let Some(m) = OTHER.indent_code_compensation.exec(raw) else { return JsString(text.to_vec()) };
    let indent = m.get_or_empty(raw, 1);
    let lines: Vec<JsString> = text
        .split_str(&u("\n"))
        .into_iter()
        .map(|node| {
            let Some(mi) = OTHER.beginning_space.exec(&node) else { return node };
            if mi.len() >= indent.len() { node.js_slice(indent.len() as isize, None) } else { node }
        })
        .collect();
    join(&lines, "\n")
}

fn join(parts: &[JsString], sep: &str) -> JsString {
    let sep = u(sep);
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(&sep);
        }
        out.extend_from_slice(p);
    }
    JsString(out)
}

fn text_token(raw: JsString) -> Token {
    let mut t = Token::new(TokenType::Text, raw.clone());
    t.text = Some(raw);
    t
}

fn type_error(what: &str) -> LexError {
    LexError(format!("TypeError: {what}"))
}

/// `src.substring(n)` without copying.
fn advance(s: &[u16], n: usize) -> &[u16] {
    &s[n.min(s.len())..]
}

impl Lexer {
    fn new() -> Lexer {
        Lexer { links: Vec::new(), inline_queue: Vec::new(), next_slot: 0, in_link: false, in_raw_block: false, top: true }
    }

    fn has_link(&self, tag: &[u16]) -> Option<&LinkDef> {
        self.links.iter().find(|(k, _)| k.as_slice() == tag).map(|(_, v)| v)
    }

    /// `lex(src)`.
    fn lex(&mut self, src: &[u16]) -> R<TokensList> {
        let src = OTHER.carriage_return.replace_str(src, "\n");
        let mut tokens = Vec::new();
        self.block_tokens(&src, &mut tokens, false)?;
        let mut results: Vec<Option<Vec<Token>>> = (0..self.next_slot).map(|_| None).collect();
        let queue = std::mem::take(&mut self.inline_queue);
        for entry in queue {
            let lexed = self.inline_tokens(&entry.src)?;
            results[entry.slot].get_or_insert_with(Vec::new).extend(lexed);
        }
        for token in &mut tokens {
            resolve(token, &mut results);
        }
        Ok(TokensList { tokens, links: std::mem::take(&mut self.links) })
    }

    /// `inline(src, tokens)`: queues the source and returns the slot its tokens fill.
    fn inline(&mut self, src: JsString) -> usize {
        let slot = self.next_slot;
        self.next_slot += 1;
        self.inline_queue.push(QueueEntry { src, slot });
        slot
    }

    fn queue_last_src(&mut self, src: JsString) -> R<()> {
        match self.inline_queue.last_mut() {
            Some(e) => {
                e.src = src;
                Ok(())
            }
            None => Err(type_error("inlineQueue.at(-1) is undefined")),
        }
    }

    /// `blockTokens(src, tokens, lastParagraphClipped)`.
    fn block_tokens(&mut self, src: &[u16], tokens: &mut Vec<Token>, mut last_paragraph_clipped: bool) -> R<()> {
        let mut src: &[u16] = src;
        while !src.is_empty() {
            if let Some(token) = self.space(&src) {
                src = advance(src, token.raw.len());
                match tokens.last_mut() {
                    Some(last) if token.raw.len() == 1 => last.raw.push_literal("\n"),
                    _ => tokens.push(token),
                }
                continue;
            }

            if let Some(token) = self.code(&src) {
                src = advance(src, token.raw.len());
                match tokens.last_mut() {
                    Some(last) if matches!(last.kind, TokenType::Paragraph | TokenType::Text) => {
                        if !last.raw.js_ends_with(&u("\n")) {
                            last.raw.push_literal("\n");
                        }
                        last.raw.push_str(&token.raw);
                        let mut text = last.text_or_empty();
                        text.push_literal("\n");
                        text.push_str(&token.text_or_empty());
                        last.text = Some(text.clone());
                        self.queue_last_src(text)?;
                    }
                    _ => tokens.push(token),
                }
                continue;
            }

            if let Some(token) = self.fences(&src) {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.heading(&src) {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.hr(&src) {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.blockquote(&src)? {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.list(&src)? {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.html(&src) {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.def(&src) {
                src = advance(src, token.raw.len());
                match tokens.last_mut() {
                    Some(last) if matches!(last.kind, TokenType::Paragraph | TokenType::Text) => {
                        if !last.raw.js_ends_with(&u("\n")) {
                            last.raw.push_literal("\n");
                        }
                        last.raw.push_str(&token.raw);
                        let mut text = last.text_or_empty();
                        text.push_literal("\n");
                        text.push_str(&token.raw);
                        last.text = Some(text.clone());
                        self.queue_last_src(text)?;
                    }
                    _ => {
                        let tag = token.tag.clone().unwrap_or_default();
                        if self.has_link(&tag).is_none() {
                            self.links.push((
                                tag,
                                LinkDef { href: token.href.clone().unwrap_or_default(), title: token.title.clone() },
                            ));
                            tokens.push(token);
                        }
                    }
                }
                continue;
            }

            if let Some(token) = self.table(&src) {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.lheading(&src) {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if self.top {
                if let Some(token) = self.paragraph(&src) {
                    let raw_len = token.raw.len();
                    match tokens.last_mut() {
                        Some(last) if last_paragraph_clipped && last.kind == TokenType::Paragraph => {
                            if !last.raw.js_ends_with(&u("\n")) {
                                last.raw.push_literal("\n");
                            }
                            last.raw.push_str(&token.raw);
                            let mut text = last.text_or_empty();
                            text.push_literal("\n");
                            text.push_str(&token.text_or_empty());
                            last.text = Some(text.clone());
                            self.inline_queue.pop();
                            self.queue_last_src(text)?;
                        }
                        _ => tokens.push(token),
                    }
                    // No extensions clip the source, so cutSrc is src.
                    last_paragraph_clipped = false;
                    src = advance(src, raw_len);
                    continue;
                }
            }

            if let Some(token) = self.text(&src) {
                src = advance(src, token.raw.len());
                match tokens.last_mut() {
                    Some(last) if last.kind == TokenType::Text => {
                        if !last.raw.js_ends_with(&u("\n")) {
                            last.raw.push_literal("\n");
                        }
                        last.raw.push_str(&token.raw);
                        let mut text = last.text_or_empty();
                        text.push_literal("\n");
                        text.push_str(&token.text_or_empty());
                        last.text = Some(text.clone());
                        self.inline_queue.pop();
                        self.queue_last_src(text)?;
                    }
                    _ => tokens.push(token),
                }
                continue;
            }

            if !src.is_empty() {
                return Err(LexError(format!("Infinite loop on byte: {}", src[0])));
            }
        }
        self.top = true;
        Ok(())
    }

    /// `inlineTokens(src)`.
    fn inline_tokens(&mut self, src: &[u16]) -> R<Vec<Token>> {
        let mut tokens: Vec<Token> = Vec::new();
        let mut masked = JsString(src.to_vec());

        if !self.links.is_empty() {
            let mut last_index = 0;
            while let Some(m) = INLINE.reflink_search.exec_stateful(&masked, &mut last_index) {
                let whole = JsString(masked[m.start..m.end].to_vec());
                let label = whole.js_slice(whole.last_index_of(&u("[")) + 1, Some(-1));
                if self.links.iter().any(|(k, _)| *k == label) {
                    let mut next = masked.js_slice(0, Some(m.start as isize));
                    next.push_literal("[");
                    next.push_str(&u("a").repeat(m.len() - 2));
                    next.push_literal("]");
                    next.push_str(&masked.js_slice(last_index as isize, None));
                    masked = next;
                }
            }
        }

        let mut last_index = 0;
        while let Some(m) = INLINE.any_punctuation.exec_stateful(&masked, &mut last_index) {
            let mut next = masked.js_slice(0, Some(m.start as isize));
            next.push_literal("++");
            next.push_str(&masked.js_slice(last_index as isize, None));
            masked = next;
        }

        let mut last_index = 0;
        while let Some(m) = INLINE.block_skip.exec_stateful(&masked, &mut last_index) {
            let offset = if m.truthy(2) { m.groups[2].map_or(0, |(a, b)| b - a) } else { 0 };
            let count = m.len() as isize - offset as isize - 2;
            if count < 0 {
                return Err(LexError("RangeError: Invalid count value".into()));
            }
            let mut next = masked.js_slice(0, Some((m.start + offset) as isize));
            next.push_literal("[");
            next.push_str(&u("a").repeat(count as usize));
            next.push_literal("]");
            next.push_str(&masked.js_slice(last_index as isize, None));
            masked = next;
        }

        let mut src: &[u16] = src;
        let mut keep_prev_char = false;
        let mut prev_char = JsString::new();
        while !src.is_empty() {
            if !keep_prev_char {
                prev_char = JsString::new();
            }
            keep_prev_char = false;

            if let Some(token) = self.escape(&src) {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.tag(&src) {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.link(&src)? {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.reflink(&src)? {
                src = advance(src, token.raw.len());
                match tokens.last_mut() {
                    Some(last) if token.kind == TokenType::Text && last.kind == TokenType::Text => {
                        last.raw.push_str(&token.raw);
                        let mut text = last.text_or_empty();
                        text.push_str(&token.text_or_empty());
                        last.text = Some(text);
                    }
                    _ => tokens.push(token),
                }
                continue;
            }

            if let Some(token) = self.em_strong(&src, &masked, &prev_char)? {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.codespan(&src) {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.br(&src) {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.del(&src)? {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.autolink(&src) {
                src = advance(src, token.raw.len());
                tokens.push(token);
                continue;
            }

            if !self.in_link {
                if let Some(token) = self.url(&src) {
                    src = advance(src, token.raw.len());
                    tokens.push(token);
                    continue;
                }
            }

            if let Some(token) = self.inline_text(&src) {
                src = advance(src, token.raw.len());
                let last_unit = token.raw.js_slice(-1, None);
                if last_unit != "_" {
                    prev_char = last_unit;
                }
                keep_prev_char = true;
                match tokens.last_mut() {
                    Some(last) if last.kind == TokenType::Text => {
                        last.raw.push_str(&token.raw);
                        let mut text = last.text_or_empty();
                        text.push_str(&token.text_or_empty());
                        last.text = Some(text);
                    }
                    _ => tokens.push(token),
                }
                continue;
            }

            if !src.is_empty() {
                return Err(LexError(format!("Infinite loop on byte: {}", src[0])));
            }
        }
        Ok(tokens)
    }

    // --- Tokenizer: block level ---

    fn space(&self, src: &[u16]) -> Option<Token> {
        let cap = BLOCK.newline.exec(src)?;
        if cap.is_empty() {
            return None;
        }
        Some(Token::new(TokenType::Space, cap.get_or_empty(src, 0)))
    }

    fn code(&self, src: &[u16]) -> Option<Token> {
        let cap = BLOCK.code.exec(src)?;
        let raw = cap.get_or_empty(src, 0);
        let text = OTHER.code_remove_indent.replace_str(&raw, "");
        let mut t = Token::new(TokenType::Code, raw);
        t.code_block_style_indented = true;
        t.text = Some(rtrim(&text, b'\n' as u16, false));
        Some(t)
    }

    fn fences(&self, src: &[u16]) -> Option<Token> {
        let cap = BLOCK.fences.exec(src)?;
        let raw = cap.get_or_empty(src, 0);
        let text = indent_code_compensation(&raw, &cap.get_or_empty(src, 3));
        let lang = cap.get(src, 2).map(|l| {
            if l.is_empty() { l } else { INLINE.any_punctuation.replace_str(&l.js_trim(), "$1") }
        });
        let mut t = Token::new(TokenType::Code, raw);
        t.lang = lang;
        t.text = Some(text);
        Some(t)
    }

    fn heading(&mut self, src: &[u16]) -> Option<Token> {
        let cap = BLOCK.heading.exec(src)?;
        let mut text = cap.get_or_empty(src, 2).js_trim();
        if OTHER.ending_hash.test(&text) {
            let trimmed = rtrim(&text, b'#' as u16, false);
            if trimmed.is_empty() || OTHER.ending_space_char.test(&trimmed) {
                text = trimmed.js_trim();
            }
        }
        let mut t = Token::new(TokenType::Heading, cap.get_or_empty(src, 0));
        t.depth = Some(cap.get_or_empty(src, 1).len() as u32);
        t.text = Some(text.clone());
        t.tokens = Some(Vec::new());
        t.slot = Some(self.inline(text));
        Some(t)
    }

    fn hr(&self, src: &[u16]) -> Option<Token> {
        let cap = BLOCK.hr.exec(src)?;
        Some(Token::new(TokenType::Hr, rtrim(&cap.get_or_empty(src, 0), b'\n' as u16, false)))
    }

    fn blockquote(&mut self, src: &[u16]) -> R<Option<Token>> {
        let Some(cap) = BLOCK.blockquote.exec(src) else { return Ok(None) };
        let mut lines = rtrim(&cap.get_or_empty(src, 0), b'\n' as u16, false).split_str(&u("\n"));
        let mut raw = JsString::new();
        let mut text = JsString::new();
        let mut tokens: Vec<Token> = Vec::new();

        while !lines.is_empty() {
            let mut in_blockquote = false;
            let mut current_lines = Vec::new();
            let mut i = 0;
            while i < lines.len() {
                if OTHER.blockquote_start.test(&lines[i]) {
                    current_lines.push(lines[i].clone());
                    in_blockquote = true;
                } else if !in_blockquote {
                    current_lines.push(lines[i].clone());
                } else {
                    break;
                }
                i += 1;
            }
            lines = lines.split_off(i);

            let current_raw = join(&current_lines, "\n");
            let current_text = OTHER.blockquote_setext_replace.replace_str(&current_raw, "\n    $1");
            let current_text = OTHER.blockquote_setext_replace2.replace_str(&current_text, "");
            raw = if raw.is_empty() { current_raw.clone() } else { join(&[raw, current_raw.clone()], "\n") };
            text = if text.is_empty() { current_text.clone() } else { join(&[text, current_text.clone()], "\n") };

            let top = self.top;
            self.top = true;
            self.block_tokens(&current_text, &mut tokens, true)?;
            self.top = top;

            if lines.is_empty() {
                break;
            }

            let last_kind = tokens.last().map(|t| t.kind);
            match last_kind {
                Some(TokenType::Code) => break,
                Some(TokenType::Blockquote) => {
                    let old = tokens.last().unwrap().clone();
                    let new_text = join(&[old.raw.clone(), join(&lines, "\n")], "\n");
                    let new_token =
                        self.blockquote(&new_text)?.ok_or_else(|| type_error("blockquote continuation is undefined"))?;
                    raw = raw.js_substring(0, Some(raw.len() as isize - old.raw.len() as isize)).concat(&new_token.raw);
                    let old_text_len = old.text.as_ref().map_or(0, |t| t.len());
                    text = text
                        .js_substring(0, Some(text.len() as isize - old_text_len as isize))
                        .concat(&new_token.text_or_empty());
                    *tokens.last_mut().unwrap() = new_token;
                    break;
                }
                Some(TokenType::List) => {
                    let old = tokens.last().unwrap().clone();
                    let new_text = join(&[old.raw.clone(), join(&lines, "\n")], "\n");
                    let new_token = self.list(&new_text)?.ok_or_else(|| type_error("list continuation is undefined"))?;
                    raw = raw.js_substring(0, Some(raw.len() as isize - old.raw.len() as isize)).concat(&new_token.raw);
                    text = text.js_substring(0, Some(text.len() as isize - old.raw.len() as isize)).concat(&new_token.raw);
                    let new_raw_len = new_token.raw.len();
                    *tokens.last_mut().unwrap() = new_token;
                    lines = new_text.js_substring(new_raw_len as isize, None).split_str(&u("\n"));
                    continue;
                }
                _ => {}
            }
        }

        let mut t = Token::new(TokenType::Blockquote, raw);
        t.tokens = Some(tokens);
        t.text = Some(text);
        Ok(Some(t))
    }

    fn list(&mut self, src: &[u16]) -> R<Option<Token>> {
        let Some(cap) = BLOCK.list.exec(src) else { return Ok(None) };
        let mut src: &[u16] = src;
        let mut bull = cap.get_or_empty(&src, 1).js_trim();
        let ordered = bull.len() > 1;
        let mut list = Token::new(TokenType::List, JsString::new());
        list.ordered = Some(ordered);
        list.start = Some(if ordered {
            ListStart::Number(crate::js::parse_decimal(&bull.js_slice(0, Some(-1))))
        } else {
            ListStart::Empty
        });
        list.loose = Some(false);
        let mut items: Vec<Token> = Vec::new();

        let bull_src = if ordered {
            format!(r"\d{{1,9}}\{}", bull.js_slice(-1, None))
        } else {
            format!(r"\{}", bull)
        };
        bull = JsString::from(bull_src.as_str());
        let item_regex = OTHER.list_item_regex(&bull.to_string());
        let mut ends_with_blank_line = false;
        let mut list_raw = JsString::new();
        let mut loose = false;

        while !src.is_empty() {
            let mut end_early = false;
            let Some(cap) = item_regex.exec(&src) else { break };
            if BLOCK.hr.test(&src) {
                break;
            }
            let mut raw = cap.get_or_empty(&src, 0);
            let cap1 = cap.get_or_empty(&src, 1);
            let cap2 = cap.get_or_empty(&src, 2);
            src = advance(src, raw.len());

            let mut tab_replace =
                |m: &crate::js::JsMatch, _: &[u16]| -> JsString { u(" ").repeat(3 * (m.end - m.start)).into() };
            let mut line = OTHER.list_replace_tabs.replace(&cap2.split_first_on(&u("\n")), Replacement::Func(&mut tab_replace));
            let mut next_line = src.split_first_on(&u("\n"));
            let mut blank_line = line.js_trim().is_empty();

            let indent: isize;
            let mut item_contents = JsString::new();
            if blank_line {
                indent = cap1.len() as isize + 1;
            } else {
                let mut n = OTHER.non_space_char.search(&cap2);
                n = if n > 4 { 1 } else { n };
                item_contents = line.js_slice(n, None);
                indent = n + cap1.len() as isize;
            }

            if blank_line && OTHER.blank_line.test(&next_line) {
                raw.push_str(&next_line);
                raw.push_literal("\n");
                src = advance(src, next_line.len() + 1);
                end_early = true;
            }

            if !end_early {
                let next_bullet_regex = OTHER.next_bullet_regex(indent);
                let hr_regex = OTHER.hr_regex(indent);
                let fences_begin_regex = OTHER.fences_begin_regex(indent);
                let heading_begin_regex = OTHER.heading_begin_regex(indent);
                let html_begin_regex = OTHER.html_begin_regex(indent);

                while !src.is_empty() {
                    let raw_line = src.split_first_on(&u("\n"));
                    next_line = raw_line.clone();
                    let next_line_without_tabs = OTHER.tab_char_global.replace_str(&next_line, "    ");

                    if fences_begin_regex.test(&next_line) {
                        break;
                    }
                    if heading_begin_regex.test(&next_line) {
                        break;
                    }
                    if html_begin_regex.test(&next_line) {
                        break;
                    }
                    if next_bullet_regex.test(&next_line) {
                        break;
                    }
                    if hr_regex.test(&next_line) {
                        break;
                    }

                    if OTHER.non_space_char.search(&next_line_without_tabs) >= indent || next_line.js_trim().is_empty() {
                        item_contents.push_literal("\n");
                        item_contents.push_str(&next_line_without_tabs.js_slice(indent, None));
                    } else {
                        if blank_line {
                            break;
                        }
                        if OTHER.non_space_char.search(&OTHER.tab_char_global.replace_str(&line, "    ")) >= 4 {
                            break;
                        }
                        if fences_begin_regex.test(&line) {
                            break;
                        }
                        if heading_begin_regex.test(&line) {
                            break;
                        }
                        if hr_regex.test(&line) {
                            break;
                        }
                        item_contents.push_literal("\n");
                        item_contents.push_str(&next_line);
                    }

                    if !blank_line && next_line.js_trim().is_empty() {
                        blank_line = true;
                    }

                    raw.push_str(&raw_line);
                    raw.push_literal("\n");
                    src = advance(src, raw_line.len() + 1);
                    line = next_line_without_tabs.js_slice(indent, None);
                }
            }

            if !loose {
                if ends_with_blank_line {
                    loose = true;
                } else if OTHER.double_blank_line.test(&raw) {
                    ends_with_blank_line = true;
                }
            }

            let mut item = Token::new(TokenType::ListItem, raw.clone());
            item.task = Some(OTHER.list_is_task.test(&item_contents));
            item.loose = Some(false);
            item.text = Some(item_contents);
            item.tokens = Some(Vec::new());
            items.push(item);
            list_raw.push_str(&raw);
        }

        let Some(last) = items.last_mut() else { return Ok(None) };
        last.raw = last.raw.js_trim_end();
        last.text = Some(last.text_or_empty().js_trim_end());
        list_raw = list_raw.js_trim_end();

        for item in items.iter_mut() {
            self.top = false;
            let mut item_tokens = Vec::new();
            self.block_tokens(&item.text_or_empty(), &mut item_tokens, false)?;
            item.tokens = Some(item_tokens);
            if item.task == Some(true) {
                item.text = Some(OTHER.list_replace_task.replace_str(&item.text_or_empty(), ""));
                let tokens = item.tokens.as_mut().unwrap();
                if tokens.first().is_some_and(|t| matches!(t.kind, TokenType::Text | TokenType::Paragraph)) {
                    let first = &mut tokens[0];
                    first.raw = OTHER.list_replace_task.replace_str(&first.raw, "");
                    first.text = Some(OTHER.list_replace_task.replace_str(&first.text_or_empty(), ""));
                    for entry in self.inline_queue.iter_mut().rev() {
                        if OTHER.list_is_task.test(&entry.src) {
                            entry.src = OTHER.list_replace_task.replace_str(&entry.src, "");
                            break;
                        }
                    }
                }

                if let Some(m) = OTHER.list_task_checkbox.exec(&item.raw) {
                    let box_raw = m.get_or_empty(&item.raw, 0);
                    let mut checkbox = Token::new(TokenType::Checkbox, box_raw.concat(&u(" ")));
                    let checked = box_raw != "[ ]";
                    checkbox.checked = Some(checked);
                    item.checked = Some(checked);
                    let tokens = item.tokens.as_mut().unwrap();
                    if loose {
                        let first_fits = tokens.first().is_some_and(|t| {
                            matches!(t.kind, TokenType::Paragraph | TokenType::Text) && t.tokens.is_some()
                        });
                        if first_fits {
                            let first = &mut tokens[0];
                            first.raw = checkbox.raw.concat(&first.raw);
                            first.text = Some(checkbox.raw.concat(&first.text_or_empty()));
                            first.tokens.as_mut().unwrap().insert(0, checkbox);
                        } else {
                            let mut p = Token::new(TokenType::Paragraph, checkbox.raw.clone());
                            p.text = Some(checkbox.raw.clone());
                            p.tokens = Some(vec![checkbox]);
                            tokens.insert(0, p);
                        }
                    } else {
                        tokens.insert(0, checkbox);
                    }
                }
            }

            if !loose {
                let tokens = item.tokens.as_ref().unwrap();
                let spacers: Vec<&Token> = tokens.iter().filter(|t| t.kind == TokenType::Space).collect();
                loose = !spacers.is_empty() && spacers.iter().any(|t| OTHER.any_line.test(&t.raw));
            }
        }

        if loose {
            for item in items.iter_mut() {
                item.loose = Some(true);
                for token in item.tokens.as_mut().unwrap() {
                    if token.kind == TokenType::Text {
                        token.kind = TokenType::Paragraph;
                    }
                }
            }
        }

        list.raw = list_raw;
        list.loose = Some(loose);
        list.items = Some(items);
        Ok(Some(list))
    }

    fn html(&self, src: &[u16]) -> Option<Token> {
        let cap = BLOCK.html.exec(src)?;
        let raw = cap.get_or_empty(src, 0);
        let tag = cap.get(src, 1);
        let mut t = Token::new(TokenType::Html, raw.clone());
        t.block = Some(true);
        t.pre = Some(tag.is_some_and(|t| t == "pre" || t == "script" || t == "style"));
        t.text = Some(raw);
        Some(t)
    }

    fn def(&self, src: &[u16]) -> Option<Token> {
        let cap = BLOCK.def.exec(src)?;
        let tag = OTHER.multiple_space_global.replace_str(&cap.get_or_empty(src, 1).to_lower(), " ");
        let href = match cap.get(src, 2).filter(|h| !h.is_empty()) {
            Some(h) => INLINE.any_punctuation.replace_str(&OTHER.href_brackets.replace_str(&h, "$1"), "$1"),
            None => JsString::new(),
        };
        let title = match cap.get(src, 3) {
            Some(t) if !t.is_empty() => Nullable::Str(
                INLINE.any_punctuation.replace_str(&t.js_substring(1, Some(t.len() as isize - 1)), "$1"),
            ),
            Some(t) => Nullable::Str(t),
            None => Nullable::Undefined,
        };
        let mut t = Token::new(TokenType::Def, cap.get_or_empty(src, 0));
        t.tag = Some(tag);
        t.href = Some(href);
        t.title = title;
        Some(t)
    }

    fn table(&mut self, src: &[u16]) -> Option<Token> {
        let cap = BLOCK.table.exec(src)?;
        let cap2 = cap.get_or_empty(src, 2);
        if !OTHER.table_delimiter.test(&cap2) {
            return None;
        }
        let headers = split_cells(&cap.get_or_empty(src, 1), None);
        let aligns = OTHER.table_align_chars.replace_str(&cap2, "").split_str(&u("|"));
        let rows: Vec<JsString> = match cap.get(src, 3) {
            Some(c3) if !c3.js_trim().is_empty() => {
                OTHER.table_row_blank_line.replace_str(&c3, "").split_str(&u("\n"))
            }
            _ => Vec::new(),
        };
        if headers.len() != aligns.len() {
            return None;
        }
        let align: Vec<Option<Align>> = aligns
            .iter()
            .map(|a| {
                if OTHER.table_align_right.test(a) {
                    Some(Align::Right)
                } else if OTHER.table_align_center.test(a) {
                    Some(Align::Center)
                } else if OTHER.table_align_left.test(a) {
                    Some(Align::Left)
                } else {
                    None
                }
            })
            .collect();
        let mut header = Vec::new();
        for (i, h) in headers.iter().enumerate() {
            let slot = self.inline(h.clone());
            header.push(TableCell { text: h.clone(), tokens: Vec::new(), header: true, align: align[i], slot: Some(slot) });
        }
        let mut out_rows = Vec::new();
        for row in &rows {
            let cells = split_cells(row, Some(header.len()));
            let mut out = Vec::new();
            for (i, cell) in cells.into_iter().enumerate() {
                let slot = self.inline(cell.clone());
                out.push(TableCell {
                    text: cell,
                    tokens: Vec::new(),
                    header: false,
                    align: align.get(i).copied().flatten(),
                    slot: Some(slot),
                });
            }
            out_rows.push(out);
        }
        let mut t = Token::new(TokenType::Table, cap.get_or_empty(src, 0));
        t.header = Some(header);
        t.align = Some(align);
        t.rows = Some(out_rows);
        Some(t)
    }

    fn lheading(&mut self, src: &[u16]) -> Option<Token> {
        let cap = BLOCK.lheading.exec(src)?;
        let text = cap.get_or_empty(src, 1);
        let mut t = Token::new(TokenType::Heading, cap.get_or_empty(src, 0));
        t.depth = Some(if cap.get_or_empty(src, 2).char_at(0) == "=" { 1 } else { 2 });
        t.text = Some(text.clone());
        t.tokens = Some(Vec::new());
        t.slot = Some(self.inline(text));
        Some(t)
    }

    fn paragraph(&mut self, src: &[u16]) -> Option<Token> {
        let cap = BLOCK.paragraph.exec(src)?;
        let c1 = cap.get_or_empty(src, 1);
        let text = if c1.last() == Some(&(b'\n' as u16)) { c1.js_slice(0, Some(-1)) } else { c1 };
        let mut t = Token::new(TokenType::Paragraph, cap.get_or_empty(src, 0));
        t.text = Some(text.clone());
        t.tokens = Some(Vec::new());
        t.slot = Some(self.inline(text));
        Some(t)
    }

    fn text(&mut self, src: &[u16]) -> Option<Token> {
        let cap = BLOCK.text.exec(src)?;
        let raw = cap.get_or_empty(src, 0);
        let mut t = text_token(raw.clone());
        t.tokens = Some(Vec::new());
        t.slot = Some(self.inline(raw));
        Some(t)
    }

    // --- Tokenizer: inline level ---

    fn escape(&self, src: &[u16]) -> Option<Token> {
        let cap = INLINE.escape.exec(src)?;
        let mut t = Token::new(TokenType::Escape, cap.get_or_empty(src, 0));
        t.text = Some(cap.get_or_empty(src, 1));
        Some(t)
    }

    fn tag(&mut self, src: &[u16]) -> Option<Token> {
        let cap = INLINE.tag.exec(src)?;
        let raw = cap.get_or_empty(src, 0);
        if !self.in_link && OTHER.start_a_tag.test(&raw) {
            self.in_link = true;
        } else if self.in_link && OTHER.end_a_tag.test(&raw) {
            self.in_link = false;
        }
        if !self.in_raw_block && OTHER.start_pre_script_tag.test(&raw) {
            self.in_raw_block = true;
        } else if self.in_raw_block && OTHER.end_pre_script_tag.test(&raw) {
            self.in_raw_block = false;
        }
        let mut t = Token::new(TokenType::Html, raw.clone());
        t.in_link = Some(self.in_link);
        t.in_raw_block = Some(self.in_raw_block);
        t.block = Some(false);
        t.text = Some(raw);
        Some(t)
    }

    /// `outputLink(cap, link, raw, lexer)`.
    fn output_link(&mut self, cap0: &[u16], cap1: &[u16], href: JsString, title: Nullable, raw: JsString) -> R<Token> {
        let title = if title.truthy() { title } else { Nullable::Null };
        let text = OTHER.output_link_replace.replace_str(cap1, "$1");
        self.in_link = true;
        let kind = if cap0.char_at(0) == "!" { TokenType::Image } else { TokenType::Link };
        let mut t = Token::new(kind, raw);
        t.href = Some(href);
        t.title = title;
        t.text = Some(text.clone());
        t.tokens = Some(self.inline_tokens(&text)?);
        self.in_link = false;
        Ok(t)
    }

    fn link(&mut self, src: &[u16]) -> R<Option<Token>> {
        let Some(cap) = INLINE.link.exec(src) else { return Ok(None) };
        let mut cap0 = cap.get_or_empty(src, 0);
        let cap1 = cap.get_or_empty(src, 1);
        let mut cap2 = cap.get_or_empty(src, 2);
        let mut cap3 = cap.get(src, 3);
        let trimmed_url = cap2.js_trim();
        if OTHER.start_angle_bracket.test(&trimmed_url) {
            if !OTHER.end_angle_bracket.test(&trimmed_url) {
                return Ok(None);
            }
            let rtrim_slash = rtrim(&trimmed_url.js_slice(0, Some(-1)), b'\\' as u16, false);
            if (trimmed_url.len() - rtrim_slash.len()) % 2 == 0 {
                return Ok(None);
            }
        } else {
            let last_paren_index = find_closing_bracket(&cap2, b'(' as u16, b')' as u16);
            if last_paren_index == -2 {
                return Ok(None);
            }
            if last_paren_index > -1 {
                let start = if cap0.index_of(&u("!"), 0) == 0 { 5 } else { 4 };
                let link_len = start + cap1.len() as isize + last_paren_index;
                cap2 = cap2.js_substring(0, Some(last_paren_index));
                cap0 = cap0.js_substring(0, Some(link_len)).js_trim();
                cap3 = Some(JsString::new());
            }
        }
        let mut href = cap2.clone();
        let title = match &cap3 {
            Some(t) if !t.is_empty() => t.js_slice(1, Some(-1)),
            _ => JsString::new(),
        };
        href = href.js_trim();
        if OTHER.start_angle_bracket.test(&href) {
            href = href.js_slice(1, Some(-1));
        }
        let href = if href.is_empty() { href } else { INLINE.any_punctuation.replace_str(&href, "$1") };
        let title = if title.is_empty() { title } else { INLINE.any_punctuation.replace_str(&title, "$1") };
        let raw = cap0.clone();
        Ok(Some(self.output_link(&cap0, &cap1, href, Nullable::Str(title), raw)?))
    }

    fn reflink(&mut self, src: &[u16]) -> R<Option<Token>> {
        let cap = match INLINE.reflink.exec(src) {
            Some(c) => c,
            None => match INLINE.nolink.exec(src) {
                Some(c) => c,
                None => return Ok(None),
            },
        };
        let label = if cap.truthy(2) { cap.get_or_empty(src, 2) } else { cap.get_or_empty(src, 1) };
        let link_string = OTHER.multiple_space_global.replace_str(&label, " ");
        let key = link_string.to_lower();
        let cap0 = cap.get_or_empty(src, 0);
        let Some(link) = self.has_link(&key).cloned() else {
            let text = cap0.char_at(0);
            return Ok(Some(text_token(text)));
        };
        let cap1 = cap.get_or_empty(src, 1);
        Ok(Some(self.output_link(&cap0, &cap1, link.href, link.title, cap0.clone())?))
    }

    fn em_strong(&mut self, src: &[u16], masked: &[u16], prev_char: &[u16]) -> R<Option<Token>> {
        let Some(m) = INLINE.em_strong_l_delim.exec(src) else { return Ok(None) };
        if m.truthy(3) && OTHER.unicode_alpha_numeric.test(prev_char) {
            return Ok(None);
        }
        let next_char_empty = !m.truthy(1) && !m.truthy(2);
        if !(next_char_empty || prev_char.is_empty() || INLINE.punctuation.exec(prev_char).is_some()) {
            return Ok(None);
        }
        let m0 = m.get_or_empty(src, 0);
        let l_length = m0.code_point_len() as i64 - 1;
        let mut delim_total = l_length;
        let mut mid_delim_total = 0i64;
        let end_reg: &JsRegex =
            if m0.char_at(0) == "*" { &INLINE.em_strong_r_delim_ast } else { &INLINE.em_strong_r_delim_und };
        let masked = masked.js_slice(-(src.len() as isize) + l_length as isize, None);
        let mut last_index = 0;
        while let Some(rm) = end_reg.exec_stateful(&masked, &mut last_index) {
            let Some(group) = (1..=6).find(|&g| rm.truthy(g)) else { continue };
            let r_delim = rm.get_or_empty(&masked, group);
            let mut r_length = r_delim.code_point_len() as i64;
            if rm.truthy(3) || rm.truthy(4) {
                delim_total += r_length;
                continue;
            } else if (rm.truthy(5) || rm.truthy(6)) && l_length % 3 != 0 && (l_length + r_length) % 3 == 0 {
                mid_delim_total += r_length;
                continue;
            }
            delim_total -= r_length;
            if delim_total > 0 {
                continue;
            }
            r_length = r_length.min(r_length + delim_total + mid_delim_total);
            let rm0 = rm.get_or_empty(&masked, 0);
            let last_char_length = rm0.first_code_point_len() as i64;
            let end = l_length + rm.start as i64 + last_char_length + r_length;
            let raw = src.js_slice(0, Some(end as isize));
            if l_length.min(r_length) % 2 != 0 {
                let text = raw.js_slice(1, Some(-1));
                let mut t = Token::new(TokenType::Em, raw);
                t.text = Some(text.clone());
                t.tokens = Some(self.inline_tokens(&text)?);
                return Ok(Some(t));
            }
            let text = raw.js_slice(2, Some(-2));
            let mut t = Token::new(TokenType::Strong, raw);
            t.text = Some(text.clone());
            t.tokens = Some(self.inline_tokens(&text)?);
            return Ok(Some(t));
        }
        Ok(None)
    }

    fn codespan(&self, src: &[u16]) -> Option<Token> {
        let cap = INLINE.code.exec(src)?;
        let mut text = OTHER.new_line_char_global.replace_str(&cap.get_or_empty(src, 2), " ");
        let has_non_space = OTHER.non_space_char.test(&text);
        let both_ends = OTHER.starting_space_char.test(&text) && OTHER.ending_space_char.test(&text);
        if has_non_space && both_ends {
            text = text.js_substring(1, Some(text.len() as isize - 1));
        }
        let mut t = Token::new(TokenType::Codespan, cap.get_or_empty(src, 0));
        t.text = Some(text);
        Some(t)
    }

    fn br(&self, src: &[u16]) -> Option<Token> {
        let cap = INLINE.br.exec(src)?;
        Some(Token::new(TokenType::Br, cap.get_or_empty(src, 0)))
    }

    fn del(&mut self, src: &[u16]) -> R<Option<Token>> {
        let Some(cap) = INLINE.del.exec(src) else { return Ok(None) };
        let text = cap.get_or_empty(src, 2);
        let mut t = Token::new(TokenType::Del, cap.get_or_empty(src, 0));
        t.text = Some(text.clone());
        t.tokens = Some(self.inline_tokens(&text)?);
        Ok(Some(t))
    }

    fn autolink(&self, src: &[u16]) -> Option<Token> {
        let cap = INLINE.autolink.exec(src)?;
        let text = cap.get_or_empty(src, 1);
        let href = if cap.get(src, 2).is_some_and(|g| g == "@") { u("mailto:").to_js().concat(&text) } else { text.clone() };
        let mut t = Token::new(TokenType::Link, cap.get_or_empty(src, 0));
        t.text = Some(text.clone());
        t.href = Some(href);
        t.tokens = Some(vec![text_token(text)]);
        Some(t)
    }

    fn url(&self, src: &[u16]) -> Option<Token> {
        let cap = INLINE.url.exec(src)?;
        let mut cap0 = cap.get_or_empty(src, 0);
        let (text, href);
        if cap.get(src, 2).is_some_and(|g| g == "@") {
            text = cap0.clone();
            href = u("mailto:").to_js().concat(&text);
        } else {
            loop {
                let prev = cap0.clone();
                cap0 = INLINE.backpedal.exec(&cap0).map(|m| m.get_or_empty(&prev, 0)).unwrap_or_default();
                if prev == cap0 {
                    break;
                }
            }
            text = cap0.clone();
            href = if cap.get(src, 1).is_some_and(|g| g == "www.") { u("http://").to_js().concat(&cap0) } else { cap0.clone() };
        }
        let mut t = Token::new(TokenType::Link, cap0);
        t.text = Some(text.clone());
        t.href = Some(href);
        t.tokens = Some(vec![text_token(text)]);
        Some(t)
    }

    fn inline_text(&self, src: &[u16]) -> Option<Token> {
        let cap = INLINE.text.exec(src)?;
        let mut t = text_token(cap.get_or_empty(src, 0));
        t.escaped = Some(self.in_raw_block);
        Some(t)
    }
}

/// Fills each token's pending inline slot with the tokens lexed for it, after any it already holds.
fn resolve(token: &mut Token, results: &mut [Option<Vec<Token>>]) {
    if let Some(slot) = token.slot.take() {
        if let Some(lexed) = results[slot].take() {
            token.tokens.get_or_insert_with(Vec::new).extend(lexed);
        }
    }
    if let Some(children) = token.tokens.as_mut() {
        for child in children {
            resolve(child, results);
        }
    }
    if let Some(items) = token.items.as_mut() {
        for item in items {
            resolve(item, results);
        }
    }
    for cell in token.header.iter_mut().flatten().chain(token.rows.iter_mut().flatten().flatten()) {
        if let Some(slot) = cell.slot.take() {
            if let Some(lexed) = results[slot].take() {
                cell.tokens.extend(lexed);
            }
        }
    }
}
