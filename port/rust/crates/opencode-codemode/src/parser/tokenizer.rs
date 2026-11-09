//! acorn's tokenizer (`tokenize.js`) and token context tracking (`tokencontext.js`).

use super::chars::{code_point_to_string, is_identifier_char, is_identifier_start, is_new_line, is_non_ascii_whitespace, push_code_point};
use super::regexp::RegExpValidationState;
use super::state::{Ctx, Fail, Parser, R, Tok, Value, next_line_break};
use crate::jsstr::JsStr;
use std::rc::Rc;

impl<'a> Parser<'a> {
    /// Move to the next token.
    pub(super) fn next(&mut self) -> R<()> {
        self.next_with(false)
    }

    pub(super) fn next_with(&mut self, ignore_escape_sequence_in_keyword: bool) -> R<()> {
        if !ignore_escape_sequence_in_keyword && self.contains_esc {
            if let Some(keyword) = self.ty.keyword() {
                return Err(self.raise(self.start, format!("Escape sequence in keyword {keyword}")));
            }
        }
        self.last_tok_end = self.end;
        self.last_tok_start = self.start;
        self.last_tok_end_loc = self.end_loc;
        self.last_tok_start_loc = self.start_loc;
        self.next_token()
    }

    pub(super) fn cur_context(&self) -> Option<Ctx> {
        self.context.last().copied()
    }

    /// Read a single token, updating the parser object's token-related properties.
    pub(super) fn next_token(&mut self) -> R<()> {
        let cur = self.cur_context();
        if cur != Some(Ctx::QTmpl) {
            self.skip_space()?;
        }
        self.start = self.pos;
        self.start_loc = self.cur_position();
        if self.pos >= self.input.len() {
            self.finish_token(Tok::Eof, Value::None);
            return Ok(());
        }
        if cur == Some(Ctx::QTmpl) {
            return self.try_read_template_token();
        }
        let code = self.full_char_code_at_pos();
        self.read_token(code)
    }

    fn read_token(&mut self, code: i64) -> R<()> {
        if is_identifier_start(code) || code == 92 {
            return self.read_word();
        }
        self.get_token_from_code(code)
    }

    /// acorn's `fullCharCodeAtPos`; `-1` stands for the `NaN` it yields at the end of the input or for a lead
    /// surrogate in the last position.
    pub(super) fn full_char_code_at_pos(&self) -> i64 {
        let code = self.cu(self.pos);
        if code < 0 {
            return -1;
        }
        if code <= 0xd7ff || code >= 0xdc00 {
            return code;
        }
        let next = self.cu(self.pos + 1);
        if next < 0 {
            return -1;
        }
        if next <= 0xdbff || next >= 0xe000 {
            return code;
        }
        (code << 10) + next - 0x35fdc00
    }

    fn skip_block_comment(&mut self) -> R<()> {
        let start = self.pos;
        self.pos += 2;
        let Some(end) = super::state::find2(self.input, self.pos, 42, 47) else {
            return Err(self.raise(self.pos - 2, "Unterminated comment"));
        };
        self.pos = end + 2;
        let mut pos = start;
        while let Some(next) = next_line_break(self.input, pos, self.pos) {
            self.cur_line += 1;
            self.line_start = next;
            pos = next;
        }
        Ok(())
    }

    pub(super) fn skip_line_comment(&mut self, start_skip: usize) {
        self.pos += start_skip;
        let mut ch = self.cu(self.pos);
        while self.pos < self.input.len() && !is_new_line(ch) {
            self.pos += 1;
            ch = self.cu(self.pos);
        }
    }

    /// Skips whitespace and comments.
    pub(super) fn skip_space(&mut self) -> R<()> {
        while self.pos < self.input.len() {
            let ch = self.input[self.pos] as i64;
            match ch {
                32 | 160 => self.pos += 1,
                13 | 10 | 8232 | 8233 => {
                    if ch == 13 && self.cu(self.pos + 1) == 10 {
                        self.pos += 1;
                    }
                    self.pos += 1;
                    self.cur_line += 1;
                    self.line_start = self.pos;
                }
                47 => match self.cu(self.pos + 1) {
                    42 => self.skip_block_comment()?,
                    47 => self.skip_line_comment(2),
                    _ => break,
                },
                _ => {
                    if ch > 8 && ch < 14 || ch >= 5760 && is_non_ascii_whitespace(ch) {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Sets `end`, `value`, and maintains `context` and `exprAllowed`.
    pub(super) fn finish_token(&mut self, ty: Tok, value: Value) {
        self.end = self.pos;
        self.end_loc = self.cur_position();
        let prev = self.ty;
        self.ty = ty;
        self.value = value;
        self.update_context(prev);
    }

    // ## Token contexts

    fn brace_is_block(&self, prev: Tok) -> bool {
        let parent = self.cur_context();
        if parent == Some(Ctx::FExpr) || parent == Some(Ctx::FStat) {
            return true;
        }
        if prev == Tok::Colon && (parent == Some(Ctx::BStat) || parent == Some(Ctx::BExpr)) {
            return !parent.unwrap().is_expr();
        }
        if prev == Tok::Return || prev == Tok::Name && self.expr_allowed {
            return self.has_line_break(self.last_tok_end, self.start);
        }
        if matches!(prev, Tok::Else | Tok::Semi | Tok::Eof | Tok::ParenR | Tok::Arrow) {
            return true;
        }
        if prev == Tok::BraceL {
            return parent == Some(Ctx::BStat);
        }
        if matches!(prev, Tok::Var | Tok::Const | Tok::Name) {
            return false;
        }
        !self.expr_allowed
    }

    fn in_generator_context(&self) -> bool {
        for index in (1..self.context.len()).rev() {
            let context = self.context[index];
            if context.is_function() {
                return context.generator();
            }
        }
        false
    }

    fn update_context(&mut self, prev: Tok) {
        let ty = self.ty;
        if ty.keyword().is_some() && prev == Tok::Dot {
            self.expr_allowed = false;
            return;
        }
        match ty {
            Tok::ParenR | Tok::BraceR => {
                if self.context.len() == 1 {
                    self.expr_allowed = true;
                    return;
                }
                let mut out = self.context.pop();
                if out == Some(Ctx::BStat) && self.cur_context().is_some_and(Ctx::is_function) {
                    out = self.context.pop();
                }
                self.expr_allowed = !out.is_some_and(Ctx::is_expr);
            }
            Tok::BraceL => {
                let block = self.brace_is_block(prev);
                self.context.push(if block { Ctx::BStat } else { Ctx::BExpr });
                self.expr_allowed = true;
            }
            Tok::DollarBraceL => {
                self.context.push(Ctx::BTmpl);
                self.expr_allowed = true;
            }
            Tok::ParenL => {
                let statement_parens = matches!(prev, Tok::If | Tok::For | Tok::With | Tok::While);
                self.context.push(if statement_parens { Ctx::PStat } else { Ctx::PExpr });
                self.expr_allowed = true;
            }
            Tok::IncDec => {}
            Tok::Function | Tok::Class => {
                let cur = self.cur_context();
                if prev.before_expr()
                    && prev != Tok::Else
                    && !(prev == Tok::Semi && cur != Some(Ctx::PStat))
                    && !(prev == Tok::Return && self.has_line_break(self.last_tok_end, self.start))
                    && !((prev == Tok::Colon || prev == Tok::BraceL) && cur == Some(Ctx::BStat))
                {
                    self.context.push(Ctx::FExpr);
                } else {
                    self.context.push(Ctx::FStat);
                }
                self.expr_allowed = false;
            }
            Tok::Colon => {
                if self.cur_context().is_some_and(Ctx::is_function) {
                    self.context.pop();
                }
                self.expr_allowed = true;
            }
            Tok::BackQuote => {
                if self.cur_context() == Some(Ctx::QTmpl) {
                    self.context.pop();
                } else {
                    self.context.push(Ctx::QTmpl);
                }
                self.expr_allowed = false;
            }
            Tok::Star => {
                if prev == Tok::Function {
                    if let Some(last) = self.context.last_mut() {
                        *last = if *last == Ctx::FExpr { Ctx::FExprGen } else { Ctx::FGen };
                    }
                }
                self.expr_allowed = true;
            }
            Tok::Name => {
                let mut allowed = false;
                if prev != Tok::Dot && (self.value_is("of") && !self.expr_allowed || self.value_is("yield") && self.in_generator_context()) {
                    allowed = true;
                }
                self.expr_allowed = allowed;
            }
            _ => self.expr_allowed = ty.before_expr(),
        }
    }

    pub(super) fn override_context(&mut self, ctx: Ctx) {
        if self.cur_context() != Some(ctx) {
            if let Some(last) = self.context.last_mut() {
                *last = ctx;
            }
        }
    }

    // ## Token reading

    fn read_token_dot(&mut self) -> R<()> {
        let next = self.cu(self.pos + 1);
        if (48..=57).contains(&next) {
            return self.read_number(true);
        }
        let next2 = self.cu(self.pos + 2);
        if next == 46 && next2 == 46 {
            self.pos += 3;
            self.finish_token(Tok::Ellipsis, Value::None);
            return Ok(());
        }
        self.pos += 1;
        self.finish_token(Tok::Dot, Value::None);
        Ok(())
    }

    fn read_token_slash(&mut self) -> R<()> {
        let next = self.cu(self.pos + 1);
        if self.expr_allowed {
            self.pos += 1;
            return self.read_regexp();
        }
        if next == 61 {
            return Ok(self.finish_op(Tok::Assign, 2));
        }
        Ok(self.finish_op(Tok::Slash, 1))
    }

    fn read_token_mult_modulo_exp(&mut self, code: i64) -> R<()> {
        let mut next = self.cu(self.pos + 1);
        let mut size = 1;
        let mut ty = if code == 42 { Tok::Star } else { Tok::Modulo };
        if code == 42 && next == 42 {
            size += 1;
            ty = Tok::StarStar;
            next = self.cu(self.pos + 2);
        }
        if next == 61 {
            return Ok(self.finish_op(Tok::Assign, size + 1));
        }
        Ok(self.finish_op(ty, size))
    }

    fn read_token_pipe_amp(&mut self, code: i64) -> R<()> {
        let next = self.cu(self.pos + 1);
        if next == code {
            let next2 = self.cu(self.pos + 2);
            if next2 == 61 {
                return Ok(self.finish_op(Tok::Assign, 3));
            }
            return Ok(self.finish_op(if code == 124 { Tok::LogicalOr } else { Tok::LogicalAnd }, 2));
        }
        if next == 61 {
            return Ok(self.finish_op(Tok::Assign, 2));
        }
        Ok(self.finish_op(if code == 124 { Tok::BitwiseOr } else { Tok::BitwiseAnd }, 1))
    }

    fn read_token_caret(&mut self) -> R<()> {
        let next = self.cu(self.pos + 1);
        if next == 61 {
            return Ok(self.finish_op(Tok::Assign, 2));
        }
        Ok(self.finish_op(Tok::BitwiseXor, 1))
    }

    fn read_token_plus_min(&mut self, code: i64) -> R<()> {
        let next = self.cu(self.pos + 1);
        if next == code {
            if next == 45 && self.cu(self.pos + 2) == 62 && (self.last_tok_end == 0 || self.has_line_break(self.last_tok_end, self.pos)) {
                // A `-->` line comment
                self.skip_line_comment(3);
                self.skip_space()?;
                return self.next_token();
            }
            return Ok(self.finish_op(Tok::IncDec, 2));
        }
        if next == 61 {
            return Ok(self.finish_op(Tok::Assign, 2));
        }
        Ok(self.finish_op(Tok::PlusMin, 1))
    }

    fn read_token_lt_gt(&mut self, code: i64) -> R<()> {
        let next = self.cu(self.pos + 1);
        let mut size = 1;
        if next == code {
            size = if code == 62 && self.cu(self.pos + 2) == 62 { 3 } else { 2 };
            if self.cu(self.pos + size) == 61 {
                return Ok(self.finish_op(Tok::Assign, size + 1));
            }
            return Ok(self.finish_op(Tok::BitShift, size));
        }
        if next == 33 && code == 60 && self.cu(self.pos + 2) == 45 && self.cu(self.pos + 3) == 45 {
            // `<!--`, an XML-style comment that should be interpreted as a line comment
            self.skip_line_comment(4);
            self.skip_space()?;
            return self.next_token();
        }
        if next == 61 {
            size = 2;
        }
        Ok(self.finish_op(Tok::Relational, size))
    }

    fn read_token_eq_excl(&mut self, code: i64) -> R<()> {
        let next = self.cu(self.pos + 1);
        if next == 61 {
            let size = if self.cu(self.pos + 2) == 61 { 3 } else { 2 };
            return Ok(self.finish_op(Tok::Equality, size));
        }
        if code == 61 && next == 62 {
            self.pos += 2;
            self.finish_token(Tok::Arrow, Value::None);
            return Ok(());
        }
        Ok(self.finish_op(if code == 61 { Tok::Eq } else { Tok::Prefix }, 1))
    }

    fn read_token_question(&mut self) -> R<()> {
        let next = self.cu(self.pos + 1);
        if next == 46 {
            let next2 = self.cu(self.pos + 2);
            // `next2 < 48 || next2 > 57`, false for NaN
            if next2 >= 0 && (next2 < 48 || next2 > 57) {
                return Ok(self.finish_op(Tok::QuestionDot, 2));
            }
        }
        if next == 63 {
            let next2 = self.cu(self.pos + 2);
            if next2 == 61 {
                return Ok(self.finish_op(Tok::Assign, 3));
            }
            return Ok(self.finish_op(Tok::Coalesce, 2));
        }
        Ok(self.finish_op(Tok::Question, 1))
    }

    fn read_token_number_sign(&mut self) -> R<()> {
        self.pos += 1;
        let code = self.full_char_code_at_pos();
        if is_identifier_start(code) || code == 92 {
            let word = self.read_word1()?;
            let atom = self.atom(&word);
            self.finish_token(Tok::PrivateId, Value::Word(atom));
            return Ok(());
        }
        Err(self.raise(self.pos, format!("Unexpected character '{}'", code_point_to_string(code))))
    }

    fn get_token_from_code(&mut self, code: i64) -> R<()> {
        let punct = match code {
            46 => return self.read_token_dot(),
            40 => Tok::ParenL,
            41 => Tok::ParenR,
            59 => Tok::Semi,
            44 => Tok::Comma,
            91 => Tok::BracketL,
            93 => Tok::BracketR,
            123 => Tok::BraceL,
            125 => Tok::BraceR,
            58 => Tok::Colon,
            96 => Tok::BackQuote,
            48 => {
                let next = self.cu(self.pos + 1);
                if next == 120 || next == 88 {
                    return self.read_radix_number(16);
                }
                if next == 111 || next == 79 {
                    return self.read_radix_number(8);
                }
                if next == 98 || next == 66 {
                    return self.read_radix_number(2);
                }
                return self.read_number(false);
            }
            49..=57 => return self.read_number(false),
            34 | 39 => return self.read_string(code as u16),
            47 => return self.read_token_slash(),
            37 | 42 => return self.read_token_mult_modulo_exp(code),
            124 | 38 => return self.read_token_pipe_amp(code),
            94 => return self.read_token_caret(),
            43 | 45 => return self.read_token_plus_min(code),
            60 | 62 => return self.read_token_lt_gt(code),
            61 | 33 => return self.read_token_eq_excl(code),
            63 => return self.read_token_question(),
            126 => return Ok(self.finish_op(Tok::Prefix, 1)),
            35 => return self.read_token_number_sign(),
            _ => return Err(self.raise(self.pos, format!("Unexpected character '{}'", code_point_to_string(code)))),
        };
        self.pos += 1;
        self.finish_token(punct, Value::None);
        Ok(())
    }

    fn finish_op(&mut self, ty: Tok, size: usize) {
        let end = (self.pos + size).min(self.input.len());
        let op = op_str(&self.input[self.pos..end]);
        self.pos += size;
        self.finish_token(ty, Value::Op(op));
    }

    pub(super) fn read_regexp(&mut self) -> R<()> {
        let mut escaped = false;
        let mut in_class = false;
        let start = self.pos;
        loop {
            if self.pos >= self.input.len() {
                return Err(self.raise(start, "Unterminated regular expression"));
            }
            let ch = self.input[self.pos];
            if is_new_line(ch as i64) {
                return Err(self.raise(start, "Unterminated regular expression"));
            }
            if !escaped {
                if ch == 91 {
                    in_class = true;
                } else if ch == 93 && in_class {
                    in_class = false;
                } else if ch == 47 && !in_class {
                    break;
                }
                escaped = ch == 92;
            } else {
                escaped = false;
            }
            self.pos += 1;
        }
        let pattern_end = self.pos;
        self.pos += 1;
        let flags_start = self.pos;
        let flags = self.read_word1()?;
        if self.contains_esc {
            return Err(self.unexpected_at(flags_start));
        }
        let pattern = &self.input[start..pattern_end];
        let mut state = RegExpValidationState::new(start, pattern, &flags);
        self.validate_regexp_flags(&state)?;
        self.validate_regexp_pattern(&mut state)?;
        let pattern: Rc<str> = Rc::from(String::from_utf16_lossy(pattern));
        let flags: Rc<str> = Rc::from(String::from_utf16_lossy(&flags));
        self.finish_token(Tok::Regexp, Value::Regex { pattern, flags });
        Ok(())
    }

    /// Reads an integer in the given radix; `None` if zero digits were read (or, with `len`, not exactly `len`).
    pub(super) fn read_int(&mut self, radix: u32, len: Option<i64>, maybe_legacy_octal: bool) -> R<Option<f64>> {
        let allow_separators = len.is_none();
        let is_legacy_octal = maybe_legacy_octal && self.cu(self.pos) == 48;
        let start = self.pos;
        let mut total = 0f64;
        let mut last_code = 0i64;
        let limit = len.unwrap_or(i64::MAX);
        let mut i = 0i64;
        while i < limit {
            let code = self.cu(self.pos);
            if allow_separators && code == 95 {
                if is_legacy_octal {
                    return Err(self.raise(self.pos, "Numeric separator is not allowed in legacy octal numeric literals"));
                }
                if last_code == 95 {
                    return Err(self.raise(self.pos, "Numeric separator must be exactly one underscore"));
                }
                if i == 0 {
                    return Err(self.raise(self.pos, "Numeric separator is not allowed at the first of digits"));
                }
                last_code = code;
                i += 1;
                self.pos += 1;
                continue;
            }
            let val = if code >= 97 {
                code - 97 + 10
            } else if code >= 65 {
                code - 65 + 10
            } else if (48..=57).contains(&code) {
                code - 48
            } else {
                i64::MAX
            };
            if val >= radix as i64 {
                break;
            }
            last_code = code;
            total = total * radix as f64 + val as f64;
            i += 1;
            self.pos += 1;
        }
        if allow_separators && last_code == 95 {
            return Err(self.raise(self.pos - 1, "Numeric separator is not allowed at the last of digits"));
        }
        if self.pos == start || len.is_some_and(|len| (self.pos - start) as i64 != len) {
            return Ok(None);
        }
        Ok(Some(total))
    }

    fn read_radix_number(&mut self, radix: u32) -> R<()> {
        let start = self.pos;
        self.pos += 2;
        let val = self.read_int(radix, None, false)?;
        let Some(val) = val else {
            return Err(self.raise(self.start + 2, format!("Expected number in radix {radix}")));
        };
        if self.cu(self.pos) == 110 {
            let digits: Vec<u16> = self.input[start + 2..self.pos].iter().copied().filter(|&c| c != 95).collect();
            let value = big_to_decimal(&digits, radix);
            self.pos += 1;
            self.finish_token(Tok::Num, Value::BigInt(Rc::from(value)));
            return Ok(());
        }
        if is_identifier_start(self.full_char_code_at_pos()) {
            return Err(self.raise(self.pos, "Identifier directly after number"));
        }
        self.finish_token(Tok::Num, Value::Num(val));
        Ok(())
    }

    fn read_number(&mut self, starts_with_dot: bool) -> R<()> {
        let start = self.pos;
        if !starts_with_dot && self.read_int(10, None, true)?.is_none() {
            return Err(self.raise(start, "Invalid number"));
        }
        let mut octal = self.pos - start >= 2 && self.cu(start) == 48;
        if octal && self.strict {
            return Err(self.raise(start, "Invalid number"));
        }
        let mut next = self.cu(self.pos);
        if !octal && !starts_with_dot && next == 110 {
            let digits: Vec<u16> = self.input[start..self.pos].iter().copied().filter(|&c| c != 95).collect();
            let value = big_to_decimal(&digits, 10);
            self.pos += 1;
            if is_identifier_start(self.full_char_code_at_pos()) {
                return Err(self.raise(self.pos, "Identifier directly after number"));
            }
            self.finish_token(Tok::Num, Value::BigInt(Rc::from(value)));
            return Ok(());
        }
        if octal && self.input[start..self.pos].iter().any(|&c| c == 56 || c == 57) {
            octal = false;
        }
        if next == 46 && !octal {
            self.pos += 1;
            self.read_int(10, None, false)?;
            next = self.cu(self.pos);
        }
        if (next == 69 || next == 101) && !octal {
            self.pos += 1;
            next = self.cu(self.pos);
            if next == 43 || next == 45 {
                self.pos += 1;
            }
            if self.read_int(10, None, false)?.is_none() {
                return Err(self.raise(start, "Invalid number"));
            }
        }
        if is_identifier_start(self.full_char_code_at_pos()) {
            return Err(self.raise(self.pos, "Identifier directly after number"));
        }
        let text: String = self.input[start..self.pos].iter().filter(|&&c| c != 95).map(|&c| c as u8 as char).collect();
        let val = if octal { parse_octal(&text) } else { text.parse::<f64>().unwrap_or(f64::NAN) };
        self.finish_token(Tok::Num, Value::Num(val));
        Ok(())
    }

    fn read_code_point(&mut self) -> R<i64> {
        let ch = self.cu(self.pos);
        if ch == 123 {
            self.pos += 1;
            let code_pos = self.pos;
            let close = self.input[self.pos.min(self.input.len())..].iter().position(|&c| c == 125);
            let len = match close {
                Some(offset) => offset as i64,
                None => -1 - self.pos as i64,
            };
            let code = self.read_hex_char(len)?;
            self.pos += 1;
            if code > 0x10FFFF as f64 {
                return Err(self.invalid_string_token(code_pos, "Code point out of bounds"));
            }
            return Ok(code as i64);
        }
        Ok(self.read_hex_char(4)? as i64)
    }

    fn read_string(&mut self, quote: u16) -> R<()> {
        let mut out: Vec<u16> = Vec::new();
        self.pos += 1;
        let mut chunk_start = self.pos;
        loop {
            if self.pos >= self.input.len() {
                return Err(self.raise(self.start, "Unterminated string constant"));
            }
            let ch = self.input[self.pos];
            if ch == quote {
                break;
            }
            if ch == 92 {
                out.extend_from_slice(&self.input[chunk_start..self.pos]);
                self.read_escaped_char(false, &mut out)?;
                chunk_start = self.pos;
            } else if ch == 0x2028 || ch == 0x2029 {
                self.pos += 1;
                self.cur_line += 1;
                self.line_start = self.pos;
            } else {
                if is_new_line(ch as i64) {
                    return Err(self.raise(self.start, "Unterminated string constant"));
                }
                self.pos += 1;
            }
        }
        out.extend_from_slice(&self.input[chunk_start..self.pos]);
        self.pos += 1;
        self.finish_token(Tok::String, Value::Str(JsStr::from_units(out)));
        Ok(())
    }

    fn try_read_template_token(&mut self) -> R<()> {
        self.in_template_element = true;
        match self.read_tmpl_token() {
            Ok(()) => {}
            Err(Fail::TemplateEscape) => {
                self.in_template_element = false;
                self.read_invalid_template_token()?;
            }
            Err(err) => return Err(err),
        }
        self.in_template_element = false;
        Ok(())
    }

    fn invalid_string_token(&self, position: usize, message: &str) -> Fail {
        if self.in_template_element {
            Fail::TemplateEscape
        } else {
            self.raise(position, message)
        }
    }

    fn read_tmpl_token(&mut self) -> R<()> {
        let mut out: Vec<u16> = Vec::new();
        let mut chunk_start = self.pos;
        loop {
            if self.pos >= self.input.len() {
                return Err(self.raise(self.start, "Unterminated template"));
            }
            let ch = self.input[self.pos];
            if ch == 96 || ch == 36 && self.cu(self.pos + 1) == 123 {
                if self.pos == self.start && (self.ty == Tok::Template || self.ty == Tok::InvalidTemplate) {
                    if ch == 36 {
                        self.pos += 2;
                        self.finish_token(Tok::DollarBraceL, Value::None);
                    } else {
                        self.pos += 1;
                        self.finish_token(Tok::BackQuote, Value::None);
                    }
                    return Ok(());
                }
                out.extend_from_slice(&self.input[chunk_start..self.pos]);
                self.finish_token(Tok::Template, Value::Str(JsStr::from_units(out)));
                return Ok(());
            }
            if ch == 92 {
                out.extend_from_slice(&self.input[chunk_start..self.pos]);
                self.read_escaped_char(true, &mut out)?;
                chunk_start = self.pos;
            } else if is_new_line(ch as i64) {
                out.extend_from_slice(&self.input[chunk_start..self.pos]);
                self.pos += 1;
                match ch {
                    13 => {
                        if self.cu(self.pos) == 10 {
                            self.pos += 1;
                        }
                        out.push(10);
                    }
                    10 => out.push(10),
                    _ => out.push(ch),
                }
                self.cur_line += 1;
                self.line_start = self.pos;
                chunk_start = self.pos;
            } else {
                self.pos += 1;
            }
        }
    }

    /// Reads a template token to search for the end, without validating any escape sequences.
    fn read_invalid_template_token(&mut self) -> R<()> {
        while self.pos < self.input.len() {
            match self.input[self.pos] {
                92 => self.pos += 1,
                36 if self.cu(self.pos + 1) != 123 => {}
                36 | 96 => {
                    let raw = JsStr::from_slice(&self.input[self.start..self.pos]);
                    self.finish_token(Tok::InvalidTemplate, Value::Str(raw));
                    return Ok(());
                }
                13 | 10 | 0x2028 | 0x2029 => {
                    if self.input[self.pos] == 13 && self.cu(self.pos + 1) == 10 {
                        self.pos += 1;
                    }
                    self.cur_line += 1;
                    self.line_start = self.pos + 1;
                }
                _ => {}
            }
            self.pos += 1;
        }
        Err(self.raise(self.start, "Unterminated template"))
    }

    /// Reads an escaped character, appending its cooked value to `out`.
    fn read_escaped_char(&mut self, in_template: bool, out: &mut Vec<u16>) -> R<()> {
        self.pos += 1;
        let mut ch = self.cu(self.pos);
        self.pos += 1;
        match ch {
            110 => out.push(10),
            114 => out.push(13),
            120 => {
                let value = self.read_hex_char(2)?;
                out.push(value as u16);
            }
            117 => {
                let code = self.read_code_point()?;
                push_code_point(out, code as u32);
            }
            116 => out.push(9),
            98 => out.push(8),
            118 => out.push(11),
            102 => out.push(12),
            13 | 10 => {
                if ch == 13 && self.cu(self.pos) == 10 {
                    self.pos += 1;
                }
                self.line_start = self.pos;
                self.cur_line += 1;
            }
            _ => {
                if ch == 56 || ch == 57 {
                    if self.strict {
                        return Err(self.invalid_string_token(self.pos - 1, "Invalid escape sequence"));
                    }
                    if in_template {
                        return Err(self.invalid_string_token(self.pos - 1, "Invalid escape sequence in template string"));
                    }
                }
                if (48..=55).contains(&ch) {
                    let from = self.pos - 1;
                    let mut len = 0;
                    while len < 3 && from + len < self.input.len() && (48..=55).contains(&self.input[from + len]) {
                        len += 1;
                    }
                    let mut octal = 0u32;
                    for &c in &self.input[from..from + len] {
                        octal = octal * 8 + (c - 48) as u32;
                    }
                    if octal > 255 {
                        len -= 1;
                        octal /= 8;
                    }
                    self.pos += len - 1;
                    ch = self.cu(self.pos);
                    let is_zero = len == 1 && self.input[from] == 48;
                    if (!is_zero || ch == 56 || ch == 57) && (self.strict || in_template) {
                        let message = if in_template { "Octal literal in template string" } else { "Octal literal in strict mode" };
                        return Err(self.invalid_string_token(self.pos - 1 - len, message));
                    }
                    out.push(octal as u16);
                    return Ok(());
                }
                if is_new_line(ch) {
                    self.line_start = self.pos;
                    self.cur_line += 1;
                    return Ok(());
                }
                // `String.fromCharCode(NaN)` is "\0"
                out.push(if ch < 0 { 0 } else { ch as u16 });
            }
        }
        Ok(())
    }

    fn read_hex_char(&mut self, len: i64) -> R<f64> {
        let code_pos = self.pos;
        match self.read_int(16, Some(len), false)? {
            Some(n) => Ok(n),
            None => Err(self.invalid_string_token(code_pos, "Bad character escape sequence")),
        }
    }

    /// Reads an identifier; sets `contains_esc` to whether the word contained a `\u` escape.
    pub(super) fn read_word1(&mut self) -> R<Vec<u16>> {
        self.contains_esc = false;
        let mut word: Vec<u16> = Vec::new();
        let mut first = true;
        let mut chunk_start = self.pos;
        while self.pos < self.input.len() {
            let ch = self.full_char_code_at_pos();
            if is_identifier_char(ch) {
                self.pos += if ch <= 0xffff { 1 } else { 2 };
            } else if ch == 92 {
                self.contains_esc = true;
                word.extend_from_slice(&self.input[chunk_start..self.pos]);
                let esc_start = self.pos;
                self.pos += 1;
                if self.cu(self.pos) != 117 {
                    return Err(self.invalid_string_token(self.pos, "Expecting Unicode escape sequence \\uXXXX"));
                }
                self.pos += 1;
                let esc = self.read_code_point()?;
                let valid = if first { is_identifier_start(esc) } else { is_identifier_char(esc) };
                if !valid {
                    return Err(self.invalid_string_token(esc_start, "Invalid Unicode escape"));
                }
                push_code_point(&mut word, esc as u32);
                chunk_start = self.pos;
            } else {
                break;
            }
            first = false;
        }
        word.extend_from_slice(&self.input[chunk_start..self.pos]);
        Ok(word)
    }

    fn read_word(&mut self) -> R<()> {
        let word = self.read_word1()?;
        if let Some(keyword) = Tok::from_keyword(&word) {
            self.finish_token(keyword, Value::None);
            return Ok(());
        }
        let atom = self.atom(&word);
        self.finish_token(Tok::Name, Value::Word(atom));
        Ok(())
    }
}

fn op_str(text: &[u16]) -> &'static str {
    let mut buf = [0u8; 4];
    for (index, &c) in text.iter().take(4).enumerate() {
        buf[index] = c as u8;
    }
    match &buf[..text.len().min(4)] {
        b"=" => "=",
        b"+=" => "+=",
        b"-=" => "-=",
        b"*=" => "*=",
        b"/=" => "/=",
        b"%=" => "%=",
        b"**=" => "**=",
        b"<<=" => "<<=",
        b">>=" => ">>=",
        b">>>=" => ">>>=",
        b"|=" => "|=",
        b"^=" => "^=",
        b"&=" => "&=",
        b"||=" => "||=",
        b"&&=" => "&&=",
        b"??=" => "??=",
        b"++" => "++",
        b"--" => "--",
        b"!" => "!",
        b"~" => "~",
        b"||" => "||",
        b"&&" => "&&",
        b"|" => "|",
        b"^" => "^",
        b"&" => "&",
        b"==" => "==",
        b"!=" => "!=",
        b"===" => "===",
        b"!==" => "!==",
        b"<" => "<",
        b">" => ">",
        b"<=" => "<=",
        b">=" => ">=",
        b"<<" => "<<",
        b">>" => ">>",
        b">>>" => ">>>",
        b"+" => "+",
        b"-" => "-",
        b"%" => "%",
        b"*" => "*",
        b"/" => "/",
        b"**" => "**",
        b"??" => "??",
        b"?" => "?",
        b"?." => "?.",
        _ => "",
    }
}

/// `parseInt(text, 8)` of a legacy octal literal's digits, as JavaScriptCore (bun) computes it: digit by digit while
/// exact, and past 2^53 by its `parseIntOverflow`, which sums digit * 8^k from the last digit (not correctly
/// rounded).
fn parse_octal(text: &str) -> f64 {
    let digits = text.as_bytes();
    let mut number = 0f64;
    for &c in digits {
        number = number * 8.0 + (c - b'0') as f64;
    }
    if number >= 9007199254740992.0 {
        number = 0.0;
        let mut multiplier = 1f64;
        for &c in digits.iter().rev() {
            let digit = (c - b'0') as f64;
            if multiplier == f64::INFINITY {
                if digit != 0.0 {
                    number = f64::INFINITY;
                    break;
                }
            } else {
                number += digit * multiplier;
            }
            multiplier *= 8.0;
        }
    }
    number
}

/// The decimal digits of a non-negative integer written in `radix` (digits only, no prefix or separators).
fn big_to_decimal(digits: &[u16], radix: u32) -> String {
    // little-endian limbs in base 1e9
    let mut limbs: Vec<u64> = vec![0];
    for &c in digits {
        let d = match c {
            48..=57 => (c - 48) as u64,
            97..=122 => (c - 97 + 10) as u64,
            65..=90 => (c - 65 + 10) as u64,
            _ => continue,
        };
        let mut carry = d;
        for limb in limbs.iter_mut() {
            let value = *limb * radix as u64 + carry;
            *limb = value % 1_000_000_000;
            carry = value / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push(carry % 1_000_000_000);
            carry /= 1_000_000_000;
        }
    }
    let mut out = limbs.last().unwrap().to_string();
    for limb in limbs.iter().rev().skip(1) {
        out.push_str(&format!("{limb:09}"));
    }
    out
}
