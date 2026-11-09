//! The parser object: token state, the context stack, scopes, labels and private names, and the helpers acorn
//! keeps in `parseutil.js`, `location.js`, `node.js` and `scope.js`.

use super::STACK_OVERFLOW_MESSAGE;
use super::ast::{Position, Span, SyntaxError};
use super::chars::{is_js_space, is_new_line};
use crate::jsstr::JsStr;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// acorn's token types (`tokentype.js`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tok {
    Num,
    Regexp,
    String,
    Name,
    PrivateId,
    Eof,
    BracketL,
    BracketR,
    BraceL,
    BraceR,
    ParenL,
    ParenR,
    Comma,
    Semi,
    Colon,
    Dot,
    Question,
    QuestionDot,
    Arrow,
    Template,
    InvalidTemplate,
    Ellipsis,
    BackQuote,
    DollarBraceL,
    Eq,
    Assign,
    IncDec,
    Prefix,
    LogicalOr,
    LogicalAnd,
    BitwiseOr,
    BitwiseXor,
    BitwiseAnd,
    Equality,
    Relational,
    BitShift,
    PlusMin,
    Modulo,
    Star,
    Slash,
    StarStar,
    Coalesce,
    Break,
    Case,
    Catch,
    Continue,
    Debugger,
    Default,
    Do,
    Else,
    Finally,
    For,
    Function,
    If,
    Return,
    Switch,
    Throw,
    Try,
    Var,
    Const,
    While,
    With,
    New,
    This,
    Super,
    Class,
    Extends,
    Export,
    Import,
    Null,
    True,
    False,
    In,
    Instanceof,
    Typeof,
    Void,
    Delete,
}

impl Tok {
    pub(super) fn keyword(self) -> Option<&'static str> {
        Some(match self {
            Tok::Break => "break",
            Tok::Case => "case",
            Tok::Catch => "catch",
            Tok::Continue => "continue",
            Tok::Debugger => "debugger",
            Tok::Default => "default",
            Tok::Do => "do",
            Tok::Else => "else",
            Tok::Finally => "finally",
            Tok::For => "for",
            Tok::Function => "function",
            Tok::If => "if",
            Tok::Return => "return",
            Tok::Switch => "switch",
            Tok::Throw => "throw",
            Tok::Try => "try",
            Tok::Var => "var",
            Tok::Const => "const",
            Tok::While => "while",
            Tok::With => "with",
            Tok::New => "new",
            Tok::This => "this",
            Tok::Super => "super",
            Tok::Class => "class",
            Tok::Extends => "extends",
            Tok::Export => "export",
            Tok::Import => "import",
            Tok::Null => "null",
            Tok::True => "true",
            Tok::False => "false",
            Tok::In => "in",
            Tok::Instanceof => "instanceof",
            Tok::Typeof => "typeof",
            Tok::Void => "void",
            Tok::Delete => "delete",
            _ => return None,
        })
    }

    pub(super) fn from_keyword(word: &[u16]) -> Option<Tok> {
        if word.len() < 2 || word.len() > 10 || word.iter().any(|&c| !(97..=122).contains(&c)) {
            return None;
        }
        let mut buf = [0u8; 10];
        for (index, &c) in word.iter().enumerate() {
            buf[index] = c as u8;
        }
        Some(match &buf[..word.len()] {
            b"break" => Tok::Break,
            b"case" => Tok::Case,
            b"catch" => Tok::Catch,
            b"continue" => Tok::Continue,
            b"debugger" => Tok::Debugger,
            b"default" => Tok::Default,
            b"do" => Tok::Do,
            b"else" => Tok::Else,
            b"finally" => Tok::Finally,
            b"for" => Tok::For,
            b"function" => Tok::Function,
            b"if" => Tok::If,
            b"return" => Tok::Return,
            b"switch" => Tok::Switch,
            b"throw" => Tok::Throw,
            b"try" => Tok::Try,
            b"var" => Tok::Var,
            b"const" => Tok::Const,
            b"while" => Tok::While,
            b"with" => Tok::With,
            b"new" => Tok::New,
            b"this" => Tok::This,
            b"super" => Tok::Super,
            b"class" => Tok::Class,
            b"extends" => Tok::Extends,
            b"export" => Tok::Export,
            b"import" => Tok::Import,
            b"null" => Tok::Null,
            b"true" => Tok::True,
            b"false" => Tok::False,
            b"in" => Tok::In,
            b"instanceof" => Tok::Instanceof,
            b"typeof" => Tok::Typeof,
            b"void" => Tok::Void,
            b"delete" => Tok::Delete,
            _ => return None,
        })
    }

    pub(super) fn before_expr(self) -> bool {
        matches!(
            self,
            Tok::BracketL
                | Tok::BraceL
                | Tok::ParenL
                | Tok::Comma
                | Tok::Semi
                | Tok::Colon
                | Tok::Question
                | Tok::Arrow
                | Tok::Ellipsis
                | Tok::DollarBraceL
                | Tok::Eq
                | Tok::Assign
                | Tok::Prefix
                | Tok::LogicalOr
                | Tok::LogicalAnd
                | Tok::BitwiseOr
                | Tok::BitwiseXor
                | Tok::BitwiseAnd
                | Tok::Equality
                | Tok::Relational
                | Tok::BitShift
                | Tok::PlusMin
                | Tok::Modulo
                | Tok::Star
                | Tok::Slash
                | Tok::StarStar
                | Tok::Coalesce
                | Tok::Case
                | Tok::Default
                | Tok::Do
                | Tok::Else
                | Tok::Return
                | Tok::Throw
                | Tok::New
                | Tok::Extends
                | Tok::In
                | Tok::Instanceof
                | Tok::Typeof
                | Tok::Void
                | Tok::Delete
        )
    }

    pub(super) fn starts_expr(self) -> bool {
        matches!(
            self,
            Tok::Num
                | Tok::Regexp
                | Tok::String
                | Tok::Name
                | Tok::PrivateId
                | Tok::BracketL
                | Tok::BraceL
                | Tok::ParenL
                | Tok::BackQuote
                | Tok::DollarBraceL
                | Tok::IncDec
                | Tok::Prefix
                | Tok::PlusMin
                | Tok::Function
                | Tok::New
                | Tok::This
                | Tok::Super
                | Tok::Class
                | Tok::Import
                | Tok::Null
                | Tok::True
                | Tok::False
                | Tok::Typeof
                | Tok::Void
                | Tok::Delete
        )
    }

    pub(super) fn is_loop(self) -> bool {
        matches!(self, Tok::Do | Tok::For | Tok::While)
    }

    pub(super) fn is_assign(self) -> bool {
        matches!(self, Tok::Eq | Tok::Assign)
    }

    pub(super) fn prefix(self) -> bool {
        matches!(self, Tok::IncDec | Tok::Prefix | Tok::PlusMin | Tok::Typeof | Tok::Void | Tok::Delete)
    }

    pub(super) fn postfix(self) -> bool {
        self == Tok::IncDec
    }

    pub(super) fn binop(self) -> Option<i32> {
        Some(match self {
            Tok::LogicalOr | Tok::Coalesce => 1,
            Tok::LogicalAnd => 2,
            Tok::BitwiseOr => 3,
            Tok::BitwiseXor => 4,
            Tok::BitwiseAnd => 5,
            Tok::Equality => 6,
            Tok::Relational | Tok::In | Tok::Instanceof => 7,
            Tok::BitShift => 8,
            Tok::PlusMin => 9,
            Tok::Modulo | Tok::Star | Tok::Slash => 10,
            _ => return None,
        })
    }
}

/// acorn's token contexts (`tokencontext.js`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ctx {
    BStat,
    BExpr,
    BTmpl,
    PStat,
    PExpr,
    QTmpl,
    FStat,
    FExpr,
    FExprGen,
    FGen,
}

impl Ctx {
    pub(super) fn is_expr(self) -> bool {
        matches!(self, Ctx::BExpr | Ctx::PExpr | Ctx::QTmpl | Ctx::FExpr | Ctx::FExprGen)
    }
    pub(super) fn is_function(self) -> bool {
        matches!(self, Ctx::FStat | Ctx::FExpr | Ctx::FExprGen | Ctx::FGen)
    }
    pub(super) fn generator(self) -> bool {
        matches!(self, Ctx::FExprGen | Ctx::FGen)
    }
}

/// An identifier name, shared by every node that spells it.
#[derive(Clone, Debug)]
pub(super) struct Atom {
    pub name: Rc<str>,
    pub key: JsStr,
}

/// The value of the current token (`this.value`).
#[derive(Clone, Debug)]
pub(super) enum Value {
    None,
    /// name, keyword-spelled word, private name
    Word(Atom),
    /// an operator's text
    Op(&'static str),
    Num(f64),
    BigInt(Rc<str>),
    /// a string literal's or template chunk's cooked text, or an invalid template chunk's raw text
    Str(JsStr),
    Regex { pattern: Rc<str>, flags: Rc<str> },
}

/// A parse failure. `TemplateEscape` is acorn's `INVALID_TEMPLATE_ESCAPE_ERROR`, caught by the template reader.
pub(super) enum Fail {
    Syntax(Box<SyntaxError>),
    TemplateEscape,
}

pub(super) type R<T> = Result<T, Fail>;

#[derive(Clone, Copy, Debug)]
pub(super) struct Start {
    pub pos: usize,
    pub loc: Position,
}

/// acorn's `DestructuringErrors`.
#[derive(Clone, Copy, Debug)]
pub(super) struct DestructuringErrors {
    pub shorthand_assign: i64,
    pub trailing_comma: i64,
    pub parenthesized_assign: i64,
    pub parenthesized_bind: i64,
    pub double_proto: i64,
}

impl DestructuringErrors {
    pub(super) fn new() -> DestructuringErrors {
        DestructuringErrors { shorthand_assign: -1, trailing_comma: -1, parenthesized_assign: -1, parenthesized_bind: -1, double_proto: -1 }
    }
}

pub(super) const SCOPE_TOP: u32 = 1;
pub(super) const SCOPE_FUNCTION: u32 = 2;
pub(super) const SCOPE_ASYNC: u32 = 4;
pub(super) const SCOPE_GENERATOR: u32 = 8;
pub(super) const SCOPE_ARROW: u32 = 16;
pub(super) const SCOPE_SIMPLE_CATCH: u32 = 32;
pub(super) const SCOPE_SUPER: u32 = 64;
pub(super) const SCOPE_DIRECT_SUPER: u32 = 128;
pub(super) const SCOPE_CLASS_STATIC_BLOCK: u32 = 256;
pub(super) const SCOPE_CLASS_FIELD_INIT: u32 = 512;
pub(super) const SCOPE_VAR: u32 = SCOPE_TOP | SCOPE_FUNCTION | SCOPE_CLASS_STATIC_BLOCK;

pub(super) fn function_flags(is_async: bool, generator: bool) -> u32 {
    SCOPE_FUNCTION | if is_async { SCOPE_ASYNC } else { 0 } | if generator { SCOPE_GENERATOR } else { 0 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Bind {
    None,
    Var,
    Lexical,
    Function,
    SimpleCatch,
    Outside,
}

/// A set of names that stays a plain vector while small.
#[derive(Default)]
pub(super) struct NameSet {
    items: Vec<Rc<str>>,
    set: Option<HashSet<Rc<str>>>,
}

impl NameSet {
    pub(super) fn contains(&self, name: &str) -> bool {
        match &self.set {
            Some(set) => set.contains(name),
            None => self.items.iter().any(|item| &**item == name),
        }
    }
    pub(super) fn push(&mut self, name: &Rc<str>) {
        if let Some(set) = &mut self.set {
            set.insert(name.clone());
            return;
        }
        self.items.push(name.clone());
        if self.items.len() > 24 {
            self.set = Some(self.items.drain(..).collect());
        }
    }
}

pub(super) struct Scope {
    pub flags: u32,
    pub var: NameSet,
    pub lexical: NameSet,
    /// `scope.lexical[0]`, which a simple catch binding's scope compares against.
    pub first_lexical: Option<Rc<str>>,
    pub functions: NameSet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LabelKind {
    Loop,
    Switch,
}

pub(super) struct Label {
    pub name: Option<Rc<str>>,
    pub kind: Option<LabelKind>,
    pub statement_start: Option<usize>,
}

pub(super) struct PrivateNames {
    pub declared: HashMap<Rc<str>, &'static str>,
    pub used: Vec<(Rc<str>, usize)>,
}

pub(super) struct Parser<'a> {
    pub input: &'a [u16],
    pub pos: usize,
    pub line_start: usize,
    pub cur_line: u32,
    pub ty: Tok,
    pub value: Value,
    pub start: usize,
    pub end: usize,
    pub start_loc: Position,
    pub end_loc: Position,
    pub last_tok_end: usize,
    pub last_tok_start: usize,
    pub last_tok_end_loc: Position,
    pub last_tok_start_loc: Position,
    pub context: Vec<Ctx>,
    pub expr_allowed: bool,
    pub contains_esc: bool,
    pub in_template_element: bool,
    pub strict: bool,
    pub potential_arrow_at: i64,
    pub potential_arrow_in_for_await: bool,
    pub yield_pos: usize,
    pub await_pos: usize,
    pub await_ident_pos: usize,
    pub labels: Vec<Label>,
    pub scope_stack: Vec<Scope>,
    pub private_name_stack: Vec<PrivateNames>,
    atoms: HashMap<Box<[u16]>, Atom>,
    /// The address of a local in the frame that started the parse, and how far below it the parser may recurse.
    stack_base: usize,
    stack_limit: usize,
    #[cfg(test)]
    pub max_stack_used: usize,
}

/// The current stack address, for the recursion guard.
#[inline(always)]
pub(super) fn stack_address() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

impl<'a> Parser<'a> {
    pub(super) fn new(input: &'a [u16], stack_limit: usize) -> Parser<'a> {
        let mut parser = Parser {
            stack_base: stack_address(),
            stack_limit,
            #[cfg(test)]
            max_stack_used: 0,
            input,
            pos: 0,
            line_start: 0,
            cur_line: 1,
            ty: Tok::Eof,
            value: Value::None,
            start: 0,
            end: 0,
            start_loc: Position { line: 1, column: 0 },
            end_loc: Position { line: 1, column: 0 },
            last_tok_end: 0,
            last_tok_start: 0,
            last_tok_end_loc: Position { line: 1, column: 0 },
            last_tok_start_loc: Position { line: 1, column: 0 },
            context: vec![Ctx::BStat],
            expr_allowed: true,
            contains_esc: false,
            in_template_element: false,
            strict: false,
            potential_arrow_at: -1,
            potential_arrow_in_for_await: false,
            yield_pos: 0,
            await_pos: 0,
            await_ident_pos: 0,
            labels: Vec::new(),
            scope_stack: Vec::new(),
            private_name_stack: Vec::new(),
            atoms: HashMap::new(),
        };
        parser.strict = parser.strict_directive(0);
        if input.len() >= 2 && input[0] == 35 && input[1] == 33 {
            parser.skip_line_comment(2);
        }
        parser.enter_scope(SCOPE_TOP);
        parser
    }

    // ## Characters

    /// `this.input.charCodeAt(i)`, with `-1` for `NaN`.
    #[inline]
    pub(super) fn cu(&self, i: usize) -> i64 {
        match self.input.get(i) {
            Some(&c) => c as i64,
            None => -1,
        }
    }

    pub(super) fn atom(&mut self, units: &[u16]) -> Atom {
        if let Some(atom) = self.atoms.get(units) {
            return atom.clone();
        }
        let name: Rc<str> = Rc::from(String::from_utf16_lossy(units));
        let atom = Atom { name, key: JsStr::from_slice(units) };
        self.atoms.insert(Box::from(units), atom.clone());
        atom
    }

    pub(super) fn atom_str(&mut self, text: &str) -> Atom {
        let units: Vec<u16> = text.encode_utf16().collect();
        self.atom(&units)
    }

    /// Lossy Rust text of a slice of the input.
    pub(super) fn text(&self, start: usize, end: usize) -> String {
        let end = end.min(self.input.len());
        let start = start.min(end);
        String::from_utf16_lossy(&self.input[start..end])
    }

    /// `lineBreak.test(this.input.slice(start, end))`.
    pub(super) fn has_line_break(&self, start: usize, end: usize) -> bool {
        let end = end.min(self.input.len());
        start < end && self.input[start..end].iter().any(|&c| is_new_line(c as i64))
    }

    /// The end of the match of acorn's `skipWhiteSpace` regex at `pos`: whitespace, line and block comments.
    pub(super) fn skip_white_space(&self, mut pos: usize) -> usize {
        let input = self.input;
        loop {
            let Some(&c) = input.get(pos) else { return pos };
            if is_js_space(c as i64) {
                pos += 1;
                continue;
            }
            if c == 47 {
                match input.get(pos + 1) {
                    Some(47) => {
                        pos += 2;
                        while pos < input.len() && !is_new_line(input[pos] as i64) {
                            pos += 1;
                        }
                        continue;
                    }
                    Some(42) => match find2(input, pos + 2, 42, 47) {
                        Some(end) => {
                            pos = end + 2;
                            continue;
                        }
                        None => return pos,
                    },
                    _ => return pos,
                }
            }
            return pos;
        }
    }

    // ## Errors and locations

    pub(super) fn line_info(&self, offset: usize) -> Position {
        let input = self.input;
        let offset = offset.min(input.len());
        let mut line = 1;
        let mut cur = 0;
        loop {
            match next_line_break(input, cur, offset) {
                None => return Position { line, column: (offset - cur) as u32 },
                Some(next) => {
                    line += 1;
                    cur = next;
                }
            }
        }
    }

    pub(super) fn raise(&self, pos: usize, message: impl AsRef<str>) -> Fail {
        let loc = if pos > self.input.len() {
            let base = self.line_info(self.input.len());
            Position { line: base.line, column: base.column + (pos - self.input.len()) as u32 }
        } else {
            self.line_info(pos)
        };
        Fail::Syntax(Box::new(SyntaxError {
            message: format!("{} ({}:{})", message.as_ref(), loc.line, loc.column),
            pos: pos as u32,
            loc,
        }))
    }

    pub(super) fn unexpected_at(&self, pos: usize) -> Fail {
        self.raise(pos, "Unexpected token")
    }

    pub(super) fn unexpected(&self) -> Fail {
        self.raise(self.start, "Unexpected token")
    }

    /// Fails the parse, where acorn would overflow the JS stack, before the Rust one overflows: at every
    /// recursive descent point the parser checks how much stack it has used since the parse began.
    #[inline(always)]
    pub(super) fn check_stack(&mut self) -> R<()> {
        let used = self.stack_base.saturating_sub(stack_address());
        #[cfg(test)]
        {
            self.max_stack_used = self.max_stack_used.max(used);
        }
        if used > self.stack_limit {
            let loc = self.line_info(self.start);
            return Err(Fail::Syntax(Box::new(SyntaxError { message: STACK_OVERFLOW_MESSAGE.to_string(), pos: self.start as u32, loc })));
        }
        Ok(())
    }

    /// The guard for a chain the parser builds in a loop (`a + b + c`, `a.b.c()`): its `links` nested nodes cost
    /// no parser stack, but dropping (or walking) the tree recurses once per link.
    pub(super) fn check_chain(&mut self, links: usize) -> R<()> {
        // Stack per link of a recursive walk such as the tree's drop glue, generously.
        const LINK_COST: usize = if cfg!(debug_assertions) { 192 } else { 64 };
        if links % 64 != 0 {
            return Ok(());
        }
        let used = self.stack_base.saturating_sub(stack_address());
        if used + links * LINK_COST > self.stack_limit {
            let loc = self.line_info(self.start);
            return Err(Fail::Syntax(Box::new(SyntaxError { message: STACK_OVERFLOW_MESSAGE.to_string(), pos: self.start as u32, loc })));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn stack_base_for_test(&self) -> usize {
        self.stack_base - stack_address()
    }

    pub(super) fn cur_position(&self) -> Position {
        Position { line: self.cur_line, column: (self.pos - self.line_start) as u32 }
    }

    pub(super) fn start_node(&self) -> Start {
        Start { pos: self.start, loc: self.start_loc }
    }

    pub(super) fn finish(&self, start: Start) -> Span {
        Span { start: start.pos as u32, end: self.last_tok_end as u32, loc_start: start.loc, loc_end: self.last_tok_end_loc }
    }

    pub(super) fn finish_at(&self, start: Start, pos: usize, loc: Position) -> Span {
        Span { start: start.pos as u32, end: pos as u32, loc_start: start.loc, loc_end: loc }
    }

    // ## Parser utilities

    /// acorn's `strictDirective`: whether the directive prologue starting at `start` holds `"use strict"`.
    pub(super) fn strict_directive(&self, mut start: usize) -> bool {
        let input = self.input;
        loop {
            start = self.skip_white_space(start);
            let Some(&quote) = input.get(start) else { return false };
            if quote != 39 && quote != 34 {
                return false;
            }
            let mut i = start + 1;
            let close = loop {
                match input.get(i) {
                    None => return false,
                    Some(&92) => {
                        if i + 1 >= input.len() {
                            return false;
                        }
                        i += 2;
                    }
                    Some(&c) if c == quote => break i,
                    Some(_) => i += 1,
                }
            };
            let content = &input[start + 1..close];
            let matched_len = close + 1 - start;
            if content.iter().copied().eq("use strict".encode_utf16()) {
                let after = start + matched_len;
                let end = self.skip_white_space(after);
                let next = self.cu(end);
                if next == 59 || next == 125 {
                    return true;
                }
                if !self.has_line_break(after, end) {
                    return false;
                }
                let is_continuation = matches!(next, 40 | 96 | 46 | 91 | 43 | 45 | 47 | 42 | 37 | 60 | 62 | 61 | 44 | 63 | 94 | 38);
                return !(is_continuation || next == 33 && self.cu(end + 1) == 61);
            }
            start += matched_len;
            start = self.skip_white_space(start);
            if input.get(start) == Some(&59) {
                start += 1;
            }
        }
    }

    pub(super) fn eat(&mut self, ty: Tok) -> R<bool> {
        if self.ty == ty {
            self.next()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(super) fn is_contextual(&self, name: &str) -> bool {
        self.ty == Tok::Name && !self.contains_esc && matches!(&self.value, Value::Word(atom) if &*atom.name == name)
    }

    pub(super) fn value_is(&self, name: &str) -> bool {
        matches!(&self.value, Value::Word(atom) if &*atom.name == name)
    }

    pub(super) fn eat_contextual(&mut self, name: &str) -> R<bool> {
        if !self.is_contextual(name) {
            return Ok(false);
        }
        self.next()?;
        Ok(true)
    }

    pub(super) fn expect_contextual(&mut self, name: &str) -> R<()> {
        if !self.eat_contextual(name)? {
            return Err(self.unexpected());
        }
        Ok(())
    }

    pub(super) fn can_insert_semicolon(&self) -> bool {
        self.ty == Tok::Eof || self.ty == Tok::BraceR || self.has_line_break(self.last_tok_end, self.start)
    }

    pub(super) fn semicolon(&mut self) -> R<()> {
        if !self.eat(Tok::Semi)? && !self.can_insert_semicolon() {
            return Err(self.unexpected());
        }
        Ok(())
    }

    pub(super) fn after_trailing_comma(&mut self, ty: Tok, not_next: bool) -> R<bool> {
        if self.ty == ty {
            if !not_next {
                self.next()?;
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) fn expect(&mut self, ty: Tok) -> R<()> {
        if !self.eat(ty)? {
            return Err(self.unexpected());
        }
        Ok(())
    }

    pub(super) fn check_pattern_errors(&self, errors: Option<&DestructuringErrors>, is_assign: bool) -> R<()> {
        let Some(errors) = errors else { return Ok(()) };
        if errors.trailing_comma > -1 {
            return Err(self.raise(errors.trailing_comma as usize, "Comma is not permitted after the rest element"));
        }
        let parens = if is_assign { errors.parenthesized_assign } else { errors.parenthesized_bind };
        if parens > -1 {
            return Err(self.raise(parens as usize, if is_assign { "Assigning to rvalue" } else { "Parenthesized pattern" }));
        }
        Ok(())
    }

    /// `checkExpressionErrors(refDestructuringErrors, false)`.
    pub(super) fn has_expression_errors(&self, errors: Option<&DestructuringErrors>) -> bool {
        match errors {
            None => false,
            Some(errors) => errors.shorthand_assign >= 0 || errors.double_proto >= 0,
        }
    }

    /// `checkExpressionErrors(refDestructuringErrors, true)`.
    pub(super) fn check_expression_errors(&self, errors: Option<&DestructuringErrors>) -> R<()> {
        let Some(errors) = errors else { return Ok(()) };
        if errors.shorthand_assign >= 0 {
            return Err(self.raise(
                errors.shorthand_assign as usize,
                "Shorthand property assignments are valid only in destructuring patterns",
            ));
        }
        if errors.double_proto >= 0 {
            return Err(self.raise(errors.double_proto as usize, "Redefinition of __proto__ property"));
        }
        Ok(())
    }

    pub(super) fn check_yield_await_in_default_params(&self) -> R<()> {
        if self.yield_pos != 0 && (self.await_pos == 0 || self.yield_pos < self.await_pos) {
            return Err(self.raise(self.yield_pos, "Yield expression cannot be a default value"));
        }
        if self.await_pos != 0 {
            return Err(self.raise(self.await_pos, "Await expression cannot be a default value"));
        }
        Ok(())
    }

    // ## Scopes

    pub(super) fn enter_scope(&mut self, flags: u32) {
        self.scope_stack.push(Scope {
            flags,
            var: NameSet::default(),
            lexical: NameSet::default(),
            first_lexical: None,
            functions: NameSet::default(),
        });
    }

    pub(super) fn exit_scope(&mut self) {
        self.scope_stack.pop();
    }

    pub(super) fn treat_functions_as_var_in_scope(scope: &Scope) -> bool {
        scope.flags & SCOPE_FUNCTION != 0 || scope.flags & SCOPE_TOP != 0
    }

    pub(super) fn treat_functions_as_var(&self) -> bool {
        Self::treat_functions_as_var_in_scope(self.scope_stack.last().unwrap())
    }

    pub(super) fn declare_name(&mut self, name: &Rc<str>, binding: Bind, pos: usize) -> R<()> {
        let mut redeclared = false;
        match binding {
            Bind::Lexical => {
                let scope = self.scope_stack.last_mut().unwrap();
                redeclared = scope.lexical.contains(name) || scope.functions.contains(name) || scope.var.contains(name);
                push_lexical(scope, name);
            }
            Bind::SimpleCatch => {
                let scope = self.scope_stack.last_mut().unwrap();
                push_lexical(scope, name);
            }
            Bind::Function => {
                let as_var = self.treat_functions_as_var();
                let scope = self.scope_stack.last_mut().unwrap();
                redeclared = if as_var { scope.lexical.contains(name) } else { scope.lexical.contains(name) || scope.var.contains(name) };
                scope.functions.push(name);
            }
            _ => {
                for index in (0..self.scope_stack.len()).rev() {
                    let scope = &mut self.scope_stack[index];
                    let catch_param = scope.flags & SCOPE_SIMPLE_CATCH != 0 && scope.first_lexical.as_deref() == Some(&**name);
                    if scope.lexical.contains(name) && !catch_param
                        || !Self::treat_functions_as_var_in_scope(scope) && scope.functions.contains(name)
                    {
                        redeclared = true;
                        break;
                    }
                    scope.var.push(name);
                    if scope.flags & SCOPE_VAR != 0 {
                        break;
                    }
                }
            }
        }
        if redeclared {
            return Err(self.raise(pos, format!("Identifier '{name}' has already been declared")));
        }
        Ok(())
    }

    pub(super) fn current_var_scope_flags(&self) -> u32 {
        for scope in self.scope_stack.iter().rev() {
            if scope.flags & (SCOPE_VAR | SCOPE_CLASS_FIELD_INIT | SCOPE_CLASS_STATIC_BLOCK) != 0 {
                return scope.flags;
            }
        }
        0
    }

    pub(super) fn current_this_scope_flags(&self) -> u32 {
        for scope in self.scope_stack.iter().rev() {
            if scope.flags & (SCOPE_VAR | SCOPE_CLASS_FIELD_INIT | SCOPE_CLASS_STATIC_BLOCK) != 0 && scope.flags & SCOPE_ARROW == 0 {
                return scope.flags;
            }
        }
        0
    }

    // `inFunction` only gates `return`, which `allowReturnOutsideFunction` allows everywhere.
    pub(super) fn in_generator(&self) -> bool {
        self.current_var_scope_flags() & SCOPE_GENERATOR > 0
    }
    pub(super) fn in_async(&self) -> bool {
        self.current_var_scope_flags() & SCOPE_ASYNC > 0
    }
    pub(super) fn can_await(&self) -> bool {
        for scope in self.scope_stack.iter().rev() {
            if scope.flags & (SCOPE_CLASS_STATIC_BLOCK | SCOPE_CLASS_FIELD_INIT) != 0 {
                return false;
            }
            if scope.flags & SCOPE_FUNCTION != 0 {
                return scope.flags & SCOPE_ASYNC > 0;
            }
        }
        // allowAwaitOutsideFunction
        true
    }
    pub(super) fn allow_super(&self) -> bool {
        self.current_this_scope_flags() & SCOPE_SUPER > 0
    }
    pub(super) fn allow_direct_super(&self) -> bool {
        self.current_this_scope_flags() & SCOPE_DIRECT_SUPER > 0
    }
    pub(super) fn allow_new_dot_target(&self) -> bool {
        for scope in self.scope_stack.iter().rev() {
            if scope.flags & (SCOPE_CLASS_STATIC_BLOCK | SCOPE_CLASS_FIELD_INIT) != 0
                || (scope.flags & SCOPE_FUNCTION != 0 && scope.flags & SCOPE_ARROW == 0)
            {
                return true;
            }
        }
        false
    }
    pub(super) fn in_class_static_block(&self) -> bool {
        self.current_var_scope_flags() & SCOPE_CLASS_STATIC_BLOCK > 0
    }
}

fn push_lexical(scope: &mut Scope, name: &Rc<str>) {
    if scope.first_lexical.is_none() {
        scope.first_lexical = Some(name.clone());
    }
    scope.lexical.push(name);
}

/// acorn's `nextLineBreak`: the offset just past the first line break in `from..end`.
pub(super) fn next_line_break(input: &[u16], from: usize, end: usize) -> Option<usize> {
    let end = end.min(input.len());
    let mut i = from;
    while i < end {
        let next = input[i] as i64;
        if is_new_line(next) {
            return Some(if i < end - 1 && next == 13 && input[i + 1] == 10 { i + 2 } else { i + 1 });
        }
        i += 1;
    }
    None
}

/// The index of the first `a b` pair at or after `from`.
pub(super) fn find2(input: &[u16], from: usize, a: u16, b: u16) -> Option<usize> {
    let mut i = from;
    while i + 1 < input.len() {
        if input[i] == a && input[i + 1] == b {
            return Some(i);
        }
        i += 1;
    }
    None
}
