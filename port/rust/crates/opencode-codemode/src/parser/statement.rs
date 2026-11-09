//! Statement parsing (acorn's `statement.js`).

use super::ast::*;
use super::chars::{is_identifier_char, is_identifier_start};
use super::expression::ForInit;
use super::state::*;
use crate::jsstr::JsStr;
use std::collections::HashSet;
use std::rc::Rc;

pub(super) const FUNC_STATEMENT: u32 = 1;
pub(super) const FUNC_HANGING_STATEMENT: u32 = 2;
pub(super) const FUNC_NULLABLE_ID: u32 = 4;

/// The `context` argument of `parseStatement`: `"if"`, `"label"`, `"dolabel"`, ...
pub(super) type StmtCtx = Option<&'static str>;

fn label_context(context: StmtCtx) -> &'static str {
    match context {
        None => "label",
        Some(context) if context.contains("label") => context,
        Some("if") => "iflabel",
        Some("do") => "dolabel",
        Some("for") => "forlabel",
        Some("while") => "whilelabel",
        Some("with") => "withlabel",
        Some(other) => other,
    }
}

/// A class element's key, for the checks acorn runs on `element.key`.
fn key_is(key: &Expr, computed: bool, name: &str) -> bool {
    if computed {
        return false;
    }
    match key {
        Expr::Identifier(id) => &*id.name == name,
        Expr::Literal(lit) => matches!(&lit.value, LitValue::String(s) if s.eq_str(name)),
        _ => false,
    }
}

impl<'a> Parser<'a> {
    pub(super) fn parse_program(&mut self) -> R<Program> {
        let start = Start { pos: 0, loc: Position { line: 1, column: 0 } };
        self.next_token()?;
        let mut exports: HashSet<Vec<u16>> = HashSet::new();
        let mut body = Vec::new();
        while self.ty != Tok::Eof {
            body.push(self.parse_statement(None, true, Some(&mut exports))?);
        }
        self.adapt_directive_prologue(&mut body);
        self.next()?;
        Ok(Program { span: self.finish(start), body })
    }

    /// acorn's `isLet`.
    pub(super) fn is_let(&self, context: StmtCtx) -> bool {
        if !self.is_contextual("let") {
            return false;
        }
        let next = self.skip_white_space(self.pos);
        let mut next_ch = self.cu(next);
        if next_ch == 91 || next_ch == 92 {
            return true;
        }
        if context.is_some() {
            return false;
        }
        if next_ch == 123 || next_ch > 0xd7ff && next_ch < 0xdc00 {
            return true;
        }
        if is_identifier_start(next_ch) {
            let mut pos = next + 1;
            loop {
                next_ch = self.cu(pos);
                if !is_identifier_char(next_ch) {
                    break;
                }
                pos += 1;
            }
            if next_ch == 92 || next_ch > 0xd7ff && next_ch < 0xdc00 {
                return true;
            }
            let ident = &self.input[next..pos];
            let is_in = ident.iter().copied().eq("in".encode_utf16());
            let is_instanceof = ident.iter().copied().eq("instanceof".encode_utf16());
            if !is_in && !is_instanceof {
                return true;
            }
        }
        false
    }

    /// `async [no LineTerminator here] function`
    pub(super) fn is_async_function(&self) -> bool {
        if !self.is_contextual("async") {
            return false;
        }
        let next = self.skip_white_space(self.pos);
        if self.has_line_break(self.pos, next) {
            return false;
        }
        if !self.slice_is(next, "function") {
            return false;
        }
        if next + 8 == self.input.len() {
            return true;
        }
        let after = self.cu(next + 8);
        !(is_identifier_char(after) || after > 0xd7ff && after < 0xdc00)
    }

    /// `this.input.slice(start, start + text.length) === text`.
    pub(super) fn slice_is(&self, start: usize, text: &str) -> bool {
        (start..).zip(text.encode_utf16()).all(|(i, unit)| self.input.get(i) == Some(&unit))
    }

    pub(super) fn is_using_keyword(&self, is_await_using: bool, is_for: bool) -> bool {
        if !self.is_contextual(if is_await_using { "await" } else { "using" }) {
            return false;
        }
        let next = self.skip_white_space(self.pos);
        if self.has_line_break(self.pos, next) {
            return false;
        }
        let mut next = next;
        if is_await_using {
            let await_end_pos = next + 5;
            if !self.slice_is(next, "using") || await_end_pos == self.input.len() {
                return false;
            }
            let after = self.cu(await_end_pos);
            if is_identifier_char(after) || after > 0xd7ff && after < 0xdc00 {
                return false;
            }
            let skip_after_using = self.skip_white_space(await_end_pos);
            if self.has_line_break(await_end_pos, skip_after_using) {
                return false;
            }
            // acorn keeps looking at the token after `await`, not after `using`
            let _ = &mut next;
        }
        if is_for {
            let of_end_pos = next + 2;
            if self.slice_is(next, "of") {
                if of_end_pos == self.input.len() {
                    return false;
                }
                let after = self.cu(of_end_pos);
                if !is_identifier_char(after) && !(after > 0xd7ff && after < 0xdc00) {
                    return false;
                }
            }
        }
        let ch = self.cu(next);
        is_identifier_start(ch) || ch == 92
    }

    /// Parse a single statement.
    pub(super) fn parse_statement(
        &mut self,
        context: StmtCtx,
        top_level: bool,
        exports: Option<&mut HashSet<Vec<u16>>>,
    ) -> R<Stmt> {
        self.check_stack()?;
        let mut starttype = self.ty;
        let node = self.start_node();
        let mut kind: Option<VarKind> = None;

        if self.is_let(context) {
            starttype = Tok::Var;
            kind = Some(VarKind::Let);
        }

        match starttype {
            Tok::Break | Tok::Continue => self.parse_break_continue_statement(node, starttype == Tok::Break),
            Tok::Debugger => {
                self.next()?;
                self.semicolon()?;
                Ok(Stmt::Debugger(self.finish(node)))
            }
            Tok::Do => self.parse_do_statement(node),
            Tok::For => self.parse_for_statement(node),
            Tok::Function => {
                if context.is_some() && (self.strict || context != Some("if") && context != Some("label")) {
                    return Err(self.unexpected());
                }
                self.parse_function_statement(node, false, context.is_none())
            }
            Tok::Class => {
                if context.is_some() {
                    return Err(self.unexpected());
                }
                let class = self.parse_class(node, Some(true))?;
                Ok(Stmt::Class(Box::new(class)))
            }
            Tok::If => self.parse_if_statement(node),
            Tok::Return => self.parse_return_statement(node),
            Tok::Switch => self.parse_switch_statement(node),
            Tok::Throw => self.parse_throw_statement(node),
            Tok::Try => self.parse_try_statement(node),
            Tok::Const | Tok::Var => {
                let kind = kind.unwrap_or(if self.ty == Tok::Const { VarKind::Const } else { VarKind::Var });
                if context.is_some() && kind != VarKind::Var {
                    return Err(self.unexpected());
                }
                self.parse_var_statement(node, kind)
            }
            Tok::While => self.parse_while_statement(node),
            Tok::With => self.parse_with_statement(node),
            Tok::BraceL => {
                let block = self.parse_block(true, Some(node), false)?;
                Ok(Stmt::Block(Box::new(block)))
            }
            Tok::Semi => {
                self.next()?;
                Ok(Stmt::Empty(self.finish(node)))
            }
            Tok::Export | Tok::Import => {
                if starttype == Tok::Import {
                    let next = self.skip_white_space(self.pos);
                    let next_ch = self.cu(next);
                    if next_ch == 40 || next_ch == 46 {
                        let expr = self.parse_expression(ForInit::No, None)?;
                        return self.parse_expression_statement(node, expr);
                    }
                }
                if starttype == Tok::Import { self.parse_import(node) } else { self.parse_export(node, exports) }
            }
            _ => {
                if self.is_async_function() {
                    if context.is_some() {
                        return Err(self.unexpected());
                    }
                    self.next()?;
                    return self.parse_function_statement(node, true, context.is_none());
                }

                let using_kind = if self.is_using_keyword(true, false) {
                    Some(VarKind::AwaitUsing)
                } else if self.is_using_keyword(false, false) {
                    Some(VarKind::Using)
                } else {
                    None
                };
                if let Some(using_kind) = using_kind {
                    if top_level {
                        return Err(self.raise(
                            self.start,
                            "Using declaration cannot appear in the top level when source type is `script`",
                        ));
                    }
                    if using_kind == VarKind::AwaitUsing {
                        if !self.can_await() {
                            return Err(self.raise(self.start, "Await using cannot appear outside of async function"));
                        }
                        self.next()?;
                    }
                    self.next()?;
                    let declarations = self.parse_var(false, using_kind)?;
                    self.semicolon()?;
                    let span = self.finish(node);
                    return Ok(Stmt::Variable(Box::new(VariableDeclaration { span, kind: using_kind, declarations })));
                }

                let maybe_name = match &self.value {
                    Value::Word(atom) => Some(atom.name.clone()),
                    _ => None,
                };
                let expr = self.parse_expression(ForInit::No, None)?;
                if starttype == Tok::Name
                    && let Expr::Identifier(_) = &expr
                    && self.eat(Tok::Colon)?
                {
                    let Expr::Identifier(label) = expr else { unreachable!() };
                    return self.parse_labeled_statement(node, maybe_name.unwrap(), *label, context);
                }
                self.parse_expression_statement(node, expr)
            }
        }
    }

    fn parse_break_continue_statement(&mut self, node: Start, is_break: bool) -> R<Stmt> {
        self.next()?;
        let label = if self.eat(Tok::Semi)? || self.can_insert_semicolon() {
            None
        } else if self.ty != Tok::Name {
            return Err(self.unexpected());
        } else {
            let label = self.parse_ident(false)?;
            self.semicolon()?;
            Some(label)
        };

        // Verify that there is an actual destination to break or continue to.
        let mut found = false;
        for lab in &self.labels {
            if label.is_none() || lab.name.as_deref() == label.as_ref().map(|l| &*l.name) {
                if lab.kind.is_some() && (is_break || lab.kind == Some(LabelKind::Loop)) {
                    found = true;
                    break;
                }
                if label.is_some() && is_break {
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return Err(self.raise(node.pos, if is_break { "Unsyntactic break" } else { "Unsyntactic continue" }));
        }
        let span = self.finish(node);
        Ok(if is_break {
            Stmt::Break(Box::new(BreakStatement { span, label }))
        } else {
            Stmt::Continue(Box::new(ContinueStatement { span, label }))
        })
    }

    fn loop_label() -> Label {
        Label { name: None, kind: Some(LabelKind::Loop), statement_start: None }
    }

    fn parse_do_statement(&mut self, node: Start) -> R<Stmt> {
        self.next()?;
        self.labels.push(Self::loop_label());
        let body = self.parse_statement(Some("do"), false, None)?;
        self.labels.pop();
        self.expect(Tok::While)?;
        let test = self.parse_paren_expression()?;
        self.eat(Tok::Semi)?;
        let span = self.finish(node);
        Ok(Stmt::DoWhile(Box::new(DoWhileStatement { span, body, test })))
    }

    fn parse_for_statement(&mut self, node: Start) -> R<Stmt> {
        self.next()?;
        let await_at: i64 =
            if self.can_await() && self.eat_contextual("await")? { self.last_tok_start as i64 } else { -1 };
        self.labels.push(Self::loop_label());
        self.enter_scope(0);
        self.expect(Tok::ParenL)?;
        if self.ty == Tok::Semi {
            if await_at > -1 {
                return Err(self.unexpected_at(await_at as usize));
            }
            return self.parse_for(node, None);
        }
        let is_let = self.is_let(None);
        if self.ty == Tok::Var || self.ty == Tok::Const || is_let {
            let init_start = self.start_node();
            let kind = if is_let {
                VarKind::Let
            } else if self.ty == Tok::Var {
                VarKind::Var
            } else {
                VarKind::Const
            };
            self.next()?;
            let declarations = self.parse_var(true, kind)?;
            let init = VariableDeclaration { span: self.finish(init_start), kind, declarations };
            return self.parse_for_after_init(node, init, await_at);
        }
        let starts_with_let = self.is_contextual("let");
        let mut is_for_of = false;

        let using_kind = if self.is_using_keyword(false, true) {
            Some(VarKind::Using)
        } else if self.is_using_keyword(true, true) {
            Some(VarKind::AwaitUsing)
        } else {
            None
        };
        if let Some(using_kind) = using_kind {
            let init_start = self.start_node();
            self.next()?;
            if using_kind == VarKind::AwaitUsing {
                self.next()?;
            }
            let declarations = self.parse_var(true, using_kind)?;
            let init = VariableDeclaration { span: self.finish(init_start), kind: using_kind, declarations };
            return self.parse_for_after_init(node, init, await_at);
        }
        let contains_esc = self.contains_esc;
        let mut ref_de = DestructuringErrors::new();
        let init_pos = self.start;
        let init = if await_at > -1 {
            self.parse_expr_subscripts(Some(&mut ref_de), ForInit::Await)?
        } else {
            self.parse_expression(ForInit::Yes, Some(&mut ref_de))?
        };
        if self.ty == Tok::In || {
            is_for_of = self.is_contextual("of");
            is_for_of
        } {
            let mut is_await = false;
            if await_at > -1 {
                if self.ty == Tok::In {
                    return Err(self.unexpected_at(await_at as usize));
                }
                is_await = true;
            } else if is_for_of
                && let Expr::Identifier(id) = &init
                && init.span().start as usize == init_pos
                && !contains_esc
                && &*id.name == "async"
            {
                return Err(self.unexpected());
            }
            if starts_with_let && is_for_of {
                return Err(self.raise(
                    init.span().start as usize,
                    "The left-hand side of a for-of loop may not start with 'let'.",
                ));
            }
            let pat = self.to_assignable(init, false, Some(&ref_de))?;
            self.check_lval_pattern(&pat, Bind::None, None)?;
            return self.parse_for_in(node, ForHead::Pattern(pat), is_await);
        }
        self.check_expression_errors(Some(&ref_de))?;
        if await_at > -1 {
            return Err(self.unexpected_at(await_at as usize));
        }
        self.parse_for(node, Some(super::ast::ForInit::Expr(init)))
    }

    fn parse_for_after_init(&mut self, node: Start, init: VariableDeclaration, await_at: i64) -> R<Stmt> {
        if (self.ty == Tok::In || self.is_contextual("of")) && init.declarations.len() == 1 {
            if self.ty == Tok::In && await_at > -1 {
                return Err(self.unexpected_at(await_at as usize));
            }
            return self.parse_for_in(node, ForHead::Variable(Box::new(init)), await_at > -1);
        }
        if await_at > -1 {
            return Err(self.unexpected_at(await_at as usize));
        }
        self.parse_for(node, Some(super::ast::ForInit::Variable(Box::new(init))))
    }

    fn parse_function_statement(&mut self, node: Start, is_async: bool, declaration_position: bool) -> R<Stmt> {
        self.next()?;
        let statement = FUNC_STATEMENT | if declaration_position { 0 } else { FUNC_HANGING_STATEMENT };
        let function = self.parse_function(node, statement, is_async, ForInit::No)?;
        Ok(Stmt::Function(Rc::new(function)))
    }

    fn parse_if_statement(&mut self, node: Start) -> R<Stmt> {
        self.next()?;
        let test = self.parse_paren_expression()?;
        // allow function declarations in branches, but only in non-strict mode
        let consequent = self.parse_statement(Some("if"), false, None)?;
        let alternate = if self.eat(Tok::Else)? { Some(self.parse_statement(Some("if"), false, None)?) } else { None };
        let span = self.finish(node);
        Ok(Stmt::If(Box::new(IfStatement { span, test, consequent, alternate })))
    }

    fn parse_return_statement(&mut self, node: Start) -> R<Stmt> {
        self.next()?;
        let argument = if self.eat(Tok::Semi)? || self.can_insert_semicolon() {
            None
        } else {
            let argument = self.parse_expression(ForInit::No, None)?;
            self.semicolon()?;
            Some(argument)
        };
        let span = self.finish(node);
        Ok(Stmt::Return(Box::new(ReturnStatement { span, argument })))
    }

    fn parse_switch_statement(&mut self, node: Start) -> R<Stmt> {
        self.next()?;
        let discriminant = self.parse_paren_expression()?;
        let mut cases: Vec<SwitchCase> = Vec::new();
        self.expect(Tok::BraceL)?;
        self.labels.push(Label { name: None, kind: Some(LabelKind::Switch), statement_start: None });
        self.enter_scope(0);

        let mut cur: Option<(Start, Option<Expr>, Vec<Stmt>)> = None;
        let mut saw_default = false;
        while self.ty != Tok::BraceR {
            if self.ty == Tok::Case || self.ty == Tok::Default {
                let is_case = self.ty == Tok::Case;
                if let Some((start, test, consequent)) = cur.take() {
                    cases.push(SwitchCase { span: self.finish(start), test, consequent });
                }
                let start = self.start_node();
                self.next()?;
                let test = if is_case {
                    Some(self.parse_expression(ForInit::No, None)?)
                } else {
                    if saw_default {
                        return Err(self.raise(self.last_tok_start, "Multiple default clauses"));
                    }
                    saw_default = true;
                    None
                };
                cur = Some((start, test, Vec::new()));
                self.expect(Tok::Colon)?;
            } else {
                if cur.is_none() {
                    return Err(self.unexpected());
                }
                let stmt = self.parse_statement(None, false, None)?;
                if let Some((_, _, consequent)) = cur.as_mut() {
                    consequent.push(stmt);
                }
            }
        }
        self.exit_scope();
        if let Some((start, test, consequent)) = cur.take() {
            cases.push(SwitchCase { span: self.finish(start), test, consequent });
        }
        self.next()?;
        self.labels.pop();
        let span = self.finish(node);
        Ok(Stmt::Switch(Box::new(SwitchStatement { span, discriminant, cases })))
    }

    fn parse_throw_statement(&mut self, node: Start) -> R<Stmt> {
        self.next()?;
        if self.has_line_break(self.last_tok_end, self.start) {
            return Err(self.raise(self.last_tok_end, "Illegal newline after throw"));
        }
        let argument = self.parse_expression(ForInit::No, None)?;
        self.semicolon()?;
        let span = self.finish(node);
        Ok(Stmt::Throw(Box::new(ThrowStatement { span, argument })))
    }

    fn parse_catch_clause_param(&mut self) -> R<Pat> {
        let param = self.parse_binding_atom()?;
        let simple = matches!(param, Pat::Identifier(_));
        self.enter_scope(if simple { SCOPE_SIMPLE_CATCH } else { 0 });
        self.check_lval_pattern(&param, if simple { Bind::SimpleCatch } else { Bind::Lexical }, None)?;
        self.expect(Tok::ParenR)?;
        Ok(param)
    }

    fn parse_try_statement(&mut self, node: Start) -> R<Stmt> {
        self.next()?;
        let block = self.parse_block(true, None, false)?;
        let mut handler = None;
        if self.ty == Tok::Catch {
            let clause = self.start_node();
            self.next()?;
            let param = if self.eat(Tok::ParenL)? {
                Some(self.parse_catch_clause_param()?)
            } else {
                self.enter_scope(0);
                None
            };
            let body = self.parse_block(false, None, false)?;
            self.exit_scope();
            handler = Some(CatchClause { span: self.finish(clause), param, body });
        }
        let finalizer = if self.eat(Tok::Finally)? { Some(self.parse_block(true, None, false)?) } else { None };
        if handler.is_none() && finalizer.is_none() {
            return Err(self.raise(node.pos, "Missing catch or finally clause"));
        }
        let span = self.finish(node);
        Ok(Stmt::Try(Box::new(TryStatement { span, block, handler, finalizer })))
    }

    fn parse_var_statement(&mut self, node: Start, kind: VarKind) -> R<Stmt> {
        self.next()?;
        let declarations = self.parse_var(false, kind)?;
        self.semicolon()?;
        let span = self.finish(node);
        Ok(Stmt::Variable(Box::new(VariableDeclaration { span, kind, declarations })))
    }

    fn parse_while_statement(&mut self, node: Start) -> R<Stmt> {
        self.next()?;
        let test = self.parse_paren_expression()?;
        self.labels.push(Self::loop_label());
        let body = self.parse_statement(Some("while"), false, None)?;
        self.labels.pop();
        let span = self.finish(node);
        Ok(Stmt::While(Box::new(WhileStatement { span, test, body })))
    }

    fn parse_with_statement(&mut self, node: Start) -> R<Stmt> {
        if self.strict {
            return Err(self.raise(self.start, "'with' in strict mode"));
        }
        self.next()?;
        let object = self.parse_paren_expression()?;
        let body = self.parse_statement(Some("with"), false, None)?;
        let span = self.finish(node);
        Ok(Stmt::With(Box::new(WithStatement { span, object, body })))
    }

    fn parse_labeled_statement(&mut self, node: Start, maybe_name: Rc<str>, expr: Ident, context: StmtCtx) -> R<Stmt> {
        for label in &self.labels {
            if label.name.as_deref() == Some(&*maybe_name) {
                return Err(self.raise(expr.span.start as usize, format!("Label '{maybe_name}' is already declared")));
            }
        }
        let kind = if self.ty.is_loop() {
            Some(LabelKind::Loop)
        } else if self.ty == Tok::Switch {
            Some(LabelKind::Switch)
        } else {
            None
        };
        for index in (0..self.labels.len()).rev() {
            let label = &mut self.labels[index];
            if label.statement_start == Some(node.pos) {
                // Update information about previous labels on this node
                label.statement_start = Some(self.start);
                label.kind = kind;
            } else {
                break;
            }
        }
        self.labels.push(Label { name: Some(maybe_name), kind, statement_start: Some(self.start) });
        let body = self.parse_statement(Some(label_context(context)), false, None)?;
        self.labels.pop();
        let span = self.finish(node);
        Ok(Stmt::Labeled(Box::new(LabeledStatement { span, label: expr, body })))
    }

    pub(super) fn parse_expression_statement(&mut self, node: Start, expression: Expr) -> R<Stmt> {
        self.semicolon()?;
        let span = self.finish(node);
        Ok(Stmt::Expression(Box::new(ExpressionStatement { span, expression, directive: None })))
    }

    /// Parse a brace-enclosed block of statements.
    pub(super) fn parse_block(
        &mut self,
        create_new_lexical_scope: bool,
        node: Option<Start>,
        exit_strict: bool,
    ) -> R<BlockStatement> {
        let node = node.unwrap_or_else(|| self.start_node());
        let mut body = Vec::new();
        self.expect(Tok::BraceL)?;
        if create_new_lexical_scope {
            self.enter_scope(0);
        }
        while self.ty != Tok::BraceR {
            body.push(self.parse_statement(None, false, None)?);
        }
        if exit_strict {
            self.strict = false;
        }
        self.next()?;
        if create_new_lexical_scope {
            self.exit_scope();
        }
        Ok(BlockStatement { span: self.finish(node), body })
    }

    fn parse_for(&mut self, node: Start, init: Option<super::ast::ForInit>) -> R<Stmt> {
        self.expect(Tok::Semi)?;
        let test = if self.ty == Tok::Semi { None } else { Some(self.parse_expression(ForInit::No, None)?) };
        self.expect(Tok::Semi)?;
        let update = if self.ty == Tok::ParenR { None } else { Some(self.parse_expression(ForInit::No, None)?) };
        self.expect(Tok::ParenR)?;
        let body = self.parse_statement(Some("for"), false, None)?;
        self.exit_scope();
        self.labels.pop();
        let span = self.finish(node);
        Ok(Stmt::For(Box::new(ForStatement { span, init, test, update, body })))
    }

    fn parse_for_in(&mut self, node: Start, init: ForHead, is_await: bool) -> R<Stmt> {
        let is_for_in = self.ty == Tok::In;
        self.next()?;
        if let ForHead::Variable(decl) = &init {
            let first = &decl.declarations[0];
            if first.init.is_some()
                && (!is_for_in || self.strict || decl.kind != VarKind::Var || !matches!(first.id, Pat::Identifier(_)))
            {
                return Err(self.raise(
                    decl.span.start as usize,
                    if is_for_in {
                        "for-in loop variable declaration may not have an initializer"
                    } else {
                        "for-of loop variable declaration may not have an initializer"
                    },
                ));
            }
        }
        let right = if is_for_in {
            self.parse_expression(ForInit::No, None)?
        } else {
            self.parse_maybe_assign(ForInit::No, None)?
        };
        self.expect(Tok::ParenR)?;
        let body = self.parse_statement(Some("for"), false, None)?;
        self.exit_scope();
        self.labels.pop();
        let span = self.finish(node);
        Ok(if is_for_in {
            Stmt::ForIn(Box::new(ForInStatement { span, left: init, right, body }))
        } else {
            Stmt::ForOf(Box::new(ForOfStatement { span, left: init, right, body, is_await }))
        })
    }

    /// Parse a list of variable declarations.
    pub(super) fn parse_var(&mut self, is_for: bool, kind: VarKind) -> R<Vec<VariableDeclarator>> {
        let mut declarations = Vec::new();
        loop {
            let decl = self.start_node();
            let id = self.parse_var_id(kind)?;
            let init = if self.eat(Tok::Eq)? {
                Some(self.parse_maybe_assign(if is_for { ForInit::Yes } else { ForInit::No }, None)?)
            } else if kind == VarKind::Const && !(self.ty == Tok::In || self.is_contextual("of")) {
                return Err(self.unexpected());
            } else if (kind == VarKind::Using || kind == VarKind::AwaitUsing)
                && self.ty != Tok::In
                && !self.is_contextual("of")
            {
                return Err(
                    self.raise(self.last_tok_end, format!("Missing initializer in {} declaration", kind.as_str()))
                );
            } else if !matches!(id, Pat::Identifier(_)) && !(is_for && (self.ty == Tok::In || self.is_contextual("of")))
            {
                return Err(self.raise(self.last_tok_end, "Complex binding patterns require an initialization value"));
            } else {
                None
            };
            declarations.push(VariableDeclarator { span: self.finish(decl), id, init });
            if !self.eat(Tok::Comma)? {
                break;
            }
        }
        Ok(declarations)
    }

    fn parse_var_id(&mut self, kind: VarKind) -> R<Pat> {
        let id = if kind == VarKind::Using || kind == VarKind::AwaitUsing {
            Pat::Identifier(Box::new(self.parse_ident(false)?))
        } else {
            self.parse_binding_atom()?
        };
        self.check_lval_pattern(&id, if kind == VarKind::Var { Bind::Var } else { Bind::Lexical }, None)?;
        Ok(id)
    }

    /// Parse a function declaration or literal (depending on `statement & FUNC_STATEMENT`).
    pub(super) fn parse_function(
        &mut self,
        node: Start,
        statement: u32,
        is_async: bool,
        for_init: ForInit,
    ) -> R<Function> {
        if self.ty == Tok::Star && statement & FUNC_HANGING_STATEMENT != 0 {
            return Err(self.unexpected());
        }
        let generator = self.eat(Tok::Star)?;

        let mut id = None;
        if statement & FUNC_STATEMENT != 0 {
            id = if statement & FUNC_NULLABLE_ID != 0 && self.ty != Tok::Name {
                None
            } else {
                Some(self.parse_ident(false)?)
            };
            if let Some(id) = &id
                && statement & FUNC_HANGING_STATEMENT == 0
            {
                // A sloppy-mode function declaration is subject to Annex B semantics (BIND_FUNCTION).
                let binding = if self.strict || generator || is_async {
                    if self.treat_functions_as_var() { Bind::Var } else { Bind::Lexical }
                } else {
                    Bind::Function
                };
                self.check_lval_simple_ident(id, binding, None)?;
            }
        }

        let old_yield_pos = self.yield_pos;
        let old_await_pos = self.await_pos;
        let old_await_ident_pos = self.await_ident_pos;
        self.yield_pos = 0;
        self.await_pos = 0;
        self.await_ident_pos = 0;
        self.enter_scope(function_flags(is_async, generator));

        if statement & FUNC_STATEMENT == 0 {
            id = if self.ty == Tok::Name { Some(self.parse_ident(false)?) } else { None };
        }

        self.expect(Tok::ParenL)?;
        let params = self.parse_binding_list(Tok::ParenR, false, true)?;
        let params: Vec<Pat> = params.into_iter().flatten().collect();
        self.check_yield_await_in_default_params()?;
        let body = self.parse_function_body(node, id.as_ref(), &params, false, false, for_init)?;

        self.yield_pos = old_yield_pos;
        self.await_pos = old_await_pos;
        self.await_ident_pos = old_await_ident_pos;
        let kind = if statement & FUNC_STATEMENT != 0 { FunctionKind::Declaration } else { FunctionKind::Expression };
        Ok(Function { span: self.finish(node), kind, id, params, body, generator, is_async })
    }

    /// Parse a class declaration or literal. `is_statement` is acorn's `true`, `"nullableID"` (`None`), or `false`.
    pub(super) fn parse_class(&mut self, node: Start, is_statement: Option<bool>) -> R<Class> {
        self.next()?;

        // A class definition is always strict mode code.
        let old_strict = self.strict;
        self.strict = true;

        let is_declaration = is_statement != Some(false);
        let id = if self.ty == Tok::Name {
            let id = self.parse_ident(false)?;
            if is_declaration {
                self.check_lval_simple_ident(&id, Bind::Lexical, None)?;
            }
            Some(id)
        } else {
            if is_statement == Some(true) {
                return Err(self.unexpected());
            }
            None
        };
        let super_class =
            if self.eat(Tok::Extends)? { Some(self.parse_expr_subscripts(None, ForInit::No)?) } else { None };
        self.private_name_stack.push(PrivateNames { declared: Default::default(), used: Vec::new() });
        let class_body = self.start_node();
        let mut had_constructor = false;
        let mut body = Vec::new();
        self.expect(Tok::BraceL)?;
        while self.ty != Tok::BraceR {
            let Some(element) = self.parse_class_element(super_class.is_some())? else { continue };
            match &element {
                ClassElement::Method(method) if method.kind == MethodKind::Constructor => {
                    if had_constructor {
                        return Err(self.raise(method.span.start as usize, "Duplicate constructor in the same class"));
                    }
                    had_constructor = true;
                }
                ClassElement::Method(method) => {
                    if let Expr::PrivateIdentifier(key) = &method.key {
                        let next = match method.kind {
                            MethodKind::Get => {
                                if method.is_static {
                                    "sget"
                                } else {
                                    "iget"
                                }
                            }
                            MethodKind::Set => {
                                if method.is_static {
                                    "sset"
                                } else {
                                    "iset"
                                }
                            }
                            _ => "true",
                        };
                        if self.is_private_name_conflicted(&key.name, next) {
                            return Err(self.raise(
                                key.span.start as usize,
                                format!("Identifier '#{}' has already been declared", key.name),
                            ));
                        }
                    }
                }
                ClassElement::Property(property) => {
                    if let Expr::PrivateIdentifier(key) = &property.key
                        && self.is_private_name_conflicted(&key.name, "true")
                    {
                        return Err(self.raise(
                            key.span.start as usize,
                            format!("Identifier '#{}' has already been declared", key.name),
                        ));
                    }
                }
                ClassElement::StaticBlock(_) => {}
            }
            body.push(element);
        }
        self.strict = old_strict;
        self.next()?;
        let body = ClassBody { span: self.finish(class_body), body };
        self.exit_class_body()?;
        Ok(Class { span: self.finish(node), is_declaration, id, super_class, body })
    }

    fn is_private_name_conflicted(&mut self, name: &Rc<str>, next: &'static str) -> bool {
        let map = &mut self.private_name_stack.last_mut().unwrap().declared;
        let curr = map.get(name).copied();
        if curr == Some("iget") && next == "iset"
            || curr == Some("iset") && next == "iget"
            || curr == Some("sget") && next == "sset"
            || curr == Some("sset") && next == "sget"
        {
            map.insert(name.clone(), "true");
            false
        } else if curr.is_none() {
            map.insert(name.clone(), next);
            false
        } else {
            true
        }
    }

    fn exit_class_body(&mut self) -> R<()> {
        let names = self.private_name_stack.pop().unwrap();
        let has_parent = !self.private_name_stack.is_empty();
        for (name, start) in names.used {
            if !names.declared.contains_key(&name) {
                if has_parent {
                    self.private_name_stack.last_mut().unwrap().used.push((name, start));
                } else {
                    return Err(
                        self.raise(start, format!("Private field '#{name}' must be declared in an enclosing class"))
                    );
                }
            }
        }
        Ok(())
    }

    fn is_class_element_name_start(&self) -> bool {
        matches!(self.ty, Tok::Name | Tok::PrivateId | Tok::Num | Tok::String | Tok::BracketL)
            || self.ty.keyword().is_some()
    }

    fn parse_class_element(&mut self, constructor_allows_super: bool) -> R<Option<ClassElement>> {
        if self.eat(Tok::Semi)? {
            return Ok(None);
        }

        let node = self.start_node();
        let mut key_name: Option<&'static str> = None;
        let mut is_generator = false;
        let mut is_async = false;
        let mut kind = MethodKind::Method;
        let mut is_static = false;

        if self.eat_contextual("static")? {
            // Parse static init block
            if self.eat(Tok::BraceL)? {
                return Ok(Some(ClassElement::StaticBlock(Box::new(self.parse_class_static_block(node)?))));
            }
            if self.is_class_element_name_start() || self.ty == Tok::Star {
                is_static = true;
            } else {
                key_name = Some("static");
            }
        }
        if key_name.is_none() && self.eat_contextual("async")? {
            if (self.is_class_element_name_start() || self.ty == Tok::Star) && !self.can_insert_semicolon() {
                is_async = true;
            } else {
                key_name = Some("async");
            }
        }
        if key_name.is_none() && self.eat(Tok::Star)? {
            is_generator = true;
        }
        if key_name.is_none() && !is_async && !is_generator {
            let last_value = if self.is_contextual("get") {
                Some("get")
            } else if self.is_contextual("set") {
                Some("set")
            } else {
                None
            };
            if let Some(last_value) = last_value {
                self.next()?;
                if self.is_class_element_name_start() {
                    kind = if last_value == "get" { MethodKind::Get } else { MethodKind::Set };
                } else {
                    key_name = Some(last_value);
                }
            }
        }

        // Parse element name
        let (key, computed) = if let Some(key_name) = key_name {
            // 'async', 'get', 'set', or 'static' were not a keyword contextually.
            let start = Start { pos: self.last_tok_start, loc: self.last_tok_start_loc };
            let atom = self.atom_str(key_name);
            let span = self.finish(start);
            (Expr::Identifier(Box::new(Ident { span, name: atom.name, key: atom.key })), false)
        } else {
            self.parse_class_element_name()?
        };

        // Parse element value
        if self.ty == Tok::ParenL || kind != MethodKind::Method || is_generator || is_async {
            let is_constructor = !is_static && key_is(&key, computed, "constructor");
            let allows_direct_super = is_constructor && constructor_allows_super;
            if is_constructor && kind != MethodKind::Method {
                return Err(self.raise(key.span().start as usize, "Constructor can't have get/set modifier"));
            }
            let kind = if is_constructor { MethodKind::Constructor } else { kind };
            let key_start = key.span().start as usize;
            if kind == MethodKind::Constructor {
                if is_generator {
                    return Err(self.raise(key_start, "Constructor can't be a generator"));
                }
                if is_async {
                    return Err(self.raise(key_start, "Constructor can't be an async method"));
                }
            } else if is_static && key_is(&key, computed, "prototype") {
                return Err(self.raise(key_start, "Classes may not have a static property named prototype"));
            }
            let value = self.parse_method(is_generator, is_async, allows_direct_super)?;
            if kind == MethodKind::Get && !value.params.is_empty() {
                return Err(self.raise(value.span.start as usize, "getter should have no params"));
            }
            if kind == MethodKind::Set && value.params.len() != 1 {
                return Err(self.raise(value.span.start as usize, "setter should have exactly one param"));
            }
            if kind == MethodKind::Set
                && let Pat::Rest(rest) = &value.params[0]
            {
                return Err(self.raise(rest.span.start as usize, "Setter cannot use rest params"));
            }
            let span = self.finish(node);
            return Ok(Some(ClassElement::Method(Box::new(MethodDefinition {
                span,
                key,
                value: Rc::new(value),
                kind,
                computed,
                is_static,
            }))));
        }

        // parseClassField
        let key_start = key.span().start as usize;
        if key_is(&key, computed, "constructor") {
            return Err(self.raise(key_start, "Classes can't have a field named 'constructor'"));
        } else if is_static && key_is(&key, computed, "prototype") {
            return Err(self.raise(key_start, "Classes can't have a static field named 'prototype'"));
        }
        let value = if self.eat(Tok::Eq)? {
            // To raise SyntaxError if 'arguments' exists in the initializer.
            self.enter_scope(SCOPE_CLASS_FIELD_INIT | SCOPE_SUPER);
            let value = self.parse_maybe_assign(ForInit::No, None)?;
            self.exit_scope();
            Some(value)
        } else {
            None
        };
        self.semicolon()?;
        let span = self.finish(node);
        Ok(Some(ClassElement::Property(Box::new(PropertyDefinition { span, key, value, computed, is_static }))))
    }

    fn parse_class_element_name(&mut self) -> R<(Expr, bool)> {
        if self.ty == Tok::PrivateId {
            if self.value_is("constructor") {
                return Err(self.raise(self.start, "Classes can't have an element named '#constructor'"));
            }
            let key = self.parse_private_ident()?;
            return Ok((Expr::PrivateIdentifier(Box::new(key)), false));
        }
        self.parse_property_name()
    }

    fn parse_class_static_block(&mut self, node: Start) -> R<StaticBlock> {
        let mut body = Vec::new();
        let old_labels = std::mem::take(&mut self.labels);
        self.enter_scope(SCOPE_CLASS_STATIC_BLOCK | SCOPE_SUPER);
        while self.ty != Tok::BraceR {
            body.push(self.parse_statement(None, false, None)?);
        }
        self.next()?;
        self.exit_scope();
        self.labels = old_labels;
        Ok(StaticBlock { span: self.finish(node), body })
    }

    // ## Modules

    fn check_export(&self, exports: &mut Option<&mut HashSet<Vec<u16>>>, name: &[u16], pos: usize) -> R<()> {
        let Some(exports) = exports else { return Ok(()) };
        if exports.contains(name) {
            return Err(self.raise(pos, format!("Duplicate export '{}'", String::from_utf16_lossy(name))));
        }
        exports.insert(name.to_vec());
        Ok(())
    }

    fn module_name_units(name: &ModuleName) -> &[u16] {
        match name {
            ModuleName::Identifier(id) => id.key.units(),
            ModuleName::Literal(lit) => match &lit.value {
                LitValue::String(s) => s.units(),
                _ => &[],
            },
        }
    }

    fn module_name_start(name: &ModuleName) -> usize {
        match name {
            ModuleName::Identifier(id) => id.span.start as usize,
            ModuleName::Literal(lit) => lit.span.start as usize,
        }
    }

    fn check_pattern_export(&self, exports: &mut Option<&mut HashSet<Vec<u16>>>, pat: &Pat) -> R<()> {
        match pat {
            Pat::Identifier(id) => self.check_export(exports, id.key.units(), id.span.start as usize),
            Pat::Object(object) => {
                for prop in &object.properties {
                    match prop {
                        ObjectPatternMember::Property(prop) => self.check_pattern_export(exports, &prop.value)?,
                        ObjectPatternMember::Rest(rest) => self.check_pattern_export(exports, &rest.argument)?,
                    }
                }
                Ok(())
            }
            Pat::Array(array) => {
                for elt in array.elements.iter().flatten() {
                    self.check_pattern_export(exports, elt)?;
                }
                Ok(())
            }
            Pat::Assignment(assign) => self.check_pattern_export(exports, &assign.left),
            Pat::Rest(rest) => self.check_pattern_export(exports, &rest.argument),
            Pat::Member(_) => Ok(()),
        }
    }

    fn parse_export_all_declaration(&mut self, node: Start, exports: &mut Option<&mut HashSet<Vec<u16>>>) -> R<Stmt> {
        let exported = if self.eat_contextual("as")? {
            let exported = self.parse_module_export_name()?;
            self.check_export(exports, Self::module_name_units(&exported), self.last_tok_start)?;
            Some(exported)
        } else {
            None
        };
        self.expect_contextual("from")?;
        if self.ty != Tok::String {
            return Err(self.unexpected());
        }
        let source = self.parse_string_literal()?;
        let attributes = self.parse_with_clause()?;
        self.semicolon()?;
        let span = self.finish(node);
        Ok(Stmt::ExportAll(Box::new(ExportAllDeclaration { span, exported, source, attributes })))
    }

    fn parse_export(&mut self, node: Start, mut exports: Option<&mut HashSet<Vec<u16>>>) -> R<Stmt> {
        self.next()?;
        // export * from '...'
        if self.eat(Tok::Star)? {
            return self.parse_export_all_declaration(node, &mut exports);
        }
        if self.eat(Tok::Default)? {
            // export default ...
            let default: Vec<u16> = "default".encode_utf16().collect();
            self.check_export(&mut exports, &default, self.last_tok_start)?;
            let declaration = self.parse_export_default_declaration()?;
            let span = self.finish(node);
            return Ok(Stmt::ExportDefault(Box::new(ExportDefaultDeclaration { span, declaration })));
        }
        // export var|const|let|function|class ...
        if self.should_parse_export_statement() {
            let declaration = self.parse_statement(None, false, None)?;
            match &declaration {
                Stmt::Variable(decl) => {
                    for d in &decl.declarations {
                        self.check_pattern_export(&mut exports, &d.id)?;
                    }
                }
                Stmt::Function(function) => {
                    let id = function.id.as_ref().unwrap();
                    self.check_export(&mut exports, id.key.units(), id.span.start as usize)?;
                }
                Stmt::Class(class) => {
                    let id = class.id.as_ref().unwrap();
                    self.check_export(&mut exports, id.key.units(), id.span.start as usize)?;
                }
                _ => {}
            }
            let span = self.finish(node);
            return Ok(Stmt::ExportNamed(Box::new(ExportNamedDeclaration {
                span,
                declaration: Some(declaration),
                specifiers: Vec::new(),
                source: None,
                attributes: Vec::new(),
            })));
        }
        // export { x, y as z } [from '...']
        let specifiers = self.parse_export_specifiers(&mut exports)?;
        let (source, attributes) = if self.eat_contextual("from")? {
            if self.ty != Tok::String {
                return Err(self.unexpected());
            }
            let source = self.parse_string_literal()?;
            let attributes = self.parse_with_clause()?;
            (Some(source), attributes)
        } else {
            for spec in &specifiers {
                // check for keywords used as local names
                match &spec.local {
                    ModuleName::Identifier(id) => self.check_unreserved(id.span.start as usize, Some(&id.name))?,
                    ModuleName::Literal(lit) => {
                        self.check_unreserved(lit.span.start as usize, None)?;
                        return Err(self.raise(
                            lit.span.start as usize,
                            "A string literal cannot be used as an exported binding without `from`.",
                        ));
                    }
                }
            }
            (None, Vec::new())
        };
        self.semicolon()?;
        let span = self.finish(node);
        Ok(Stmt::ExportNamed(Box::new(ExportNamedDeclaration {
            span,
            declaration: None,
            specifiers,
            source,
            attributes,
        })))
    }

    fn parse_export_default_declaration(&mut self) -> R<ExportDefaultKind> {
        let is_async = self.ty != Tok::Function && self.is_async_function();
        if self.ty == Tok::Function || is_async {
            let node = self.start_node();
            self.next()?;
            if is_async {
                self.next()?;
            }
            let function = self.parse_function(node, FUNC_STATEMENT | FUNC_NULLABLE_ID, is_async, ForInit::No)?;
            return Ok(ExportDefaultKind::Declaration(Stmt::Function(Rc::new(function))));
        }
        if self.ty == Tok::Class {
            let node = self.start_node();
            let class = self.parse_class(node, None)?;
            return Ok(ExportDefaultKind::Declaration(Stmt::Class(Box::new(class))));
        }
        let declaration = self.parse_maybe_assign(ForInit::No, None)?;
        self.semicolon()?;
        Ok(ExportDefaultKind::Expr(declaration))
    }

    fn should_parse_export_statement(&self) -> bool {
        matches!(self.ty, Tok::Var | Tok::Const | Tok::Class | Tok::Function)
            || self.is_let(None)
            || self.is_async_function()
    }

    fn parse_export_specifiers(&mut self, exports: &mut Option<&mut HashSet<Vec<u16>>>) -> R<Vec<ExportSpecifier>> {
        let mut nodes = Vec::new();
        let mut first = true;
        // export { x, y as z } [from '...']
        self.expect(Tok::BraceL)?;
        while !self.eat(Tok::BraceR)? {
            if !first {
                self.expect(Tok::Comma)?;
                if self.after_trailing_comma(Tok::BraceR, false)? {
                    break;
                }
            } else {
                first = false;
            }
            let node = self.start_node();
            let local = self.parse_module_export_name()?;
            let exported =
                if self.eat_contextual("as")? { self.parse_module_export_name()? } else { clone_module_name(&local) };
            self.check_export(exports, Self::module_name_units(&exported), Self::module_name_start(&exported))?;
            nodes.push(ExportSpecifier { span: self.finish(node), local, exported });
        }
        Ok(nodes)
    }

    fn parse_import(&mut self, node: Start) -> R<Stmt> {
        self.next()?;
        // import '...'
        let (specifiers, source) = if self.ty == Tok::String {
            (Vec::new(), self.parse_string_literal()?)
        } else {
            let specifiers = self.parse_import_specifiers()?;
            self.expect_contextual("from")?;
            if self.ty != Tok::String {
                return Err(self.unexpected());
            }
            (specifiers, self.parse_string_literal()?)
        };
        let attributes = self.parse_with_clause()?;
        self.semicolon()?;
        let span = self.finish(node);
        Ok(Stmt::Import(Box::new(ImportDeclaration { span, specifiers, source, attributes })))
    }

    fn parse_import_specifiers(&mut self) -> R<Vec<ImportSpecifierKind>> {
        let mut nodes = Vec::new();
        let mut first = true;
        if self.ty == Tok::Name {
            // import defaultObj, { x, y as z } from '...'
            let node = self.start_node();
            let local = self.parse_ident(false)?;
            self.check_lval_simple_ident(&local, Bind::Lexical, None)?;
            nodes.push(ImportSpecifierKind::Default { span: self.finish(node), local });
            if !self.eat(Tok::Comma)? {
                return Ok(nodes);
            }
        }
        if self.ty == Tok::Star {
            let node = self.start_node();
            self.next()?;
            self.expect_contextual("as")?;
            let local = self.parse_ident(false)?;
            self.check_lval_simple_ident(&local, Bind::Lexical, None)?;
            nodes.push(ImportSpecifierKind::Namespace { span: self.finish(node), local });
            return Ok(nodes);
        }
        self.expect(Tok::BraceL)?;
        while !self.eat(Tok::BraceR)? {
            if !first {
                self.expect(Tok::Comma)?;
                if self.after_trailing_comma(Tok::BraceR, false)? {
                    break;
                }
            } else {
                first = false;
            }
            let node = self.start_node();
            let imported = self.parse_module_export_name()?;
            let local = if self.eat_contextual("as")? {
                self.parse_ident(false)?
            } else {
                match &imported {
                    ModuleName::Identifier(id) => {
                        self.check_unreserved(id.span.start as usize, Some(&id.name))?;
                        id.clone()
                    }
                    ModuleName::Literal(lit) => {
                        self.check_unreserved(lit.span.start as usize, None)?;
                        return Err(self.raise(lit.span.start as usize, "Binding rvalue"));
                    }
                }
            };
            self.check_lval_simple_ident(&local, Bind::Lexical, None)?;
            nodes.push(ImportSpecifierKind::Named { span: self.finish(node), imported, local });
        }
        Ok(nodes)
    }

    fn parse_with_clause(&mut self) -> R<Vec<ImportAttribute>> {
        let mut nodes = Vec::new();
        if !self.eat(Tok::With)? {
            return Ok(nodes);
        }
        self.expect(Tok::BraceL)?;
        let mut attribute_keys: HashSet<Vec<u16>> = HashSet::new();
        let mut first = true;
        while !self.eat(Tok::BraceR)? {
            if !first {
                self.expect(Tok::Comma)?;
                if self.after_trailing_comma(Tok::BraceR, false)? {
                    break;
                }
            } else {
                first = false;
            }
            let node = self.start_node();
            let key = if self.ty == Tok::String {
                ModuleName::Literal(self.parse_string_literal()?)
            } else {
                ModuleName::Identifier(self.parse_ident(true)?)
            };
            self.expect(Tok::Colon)?;
            if self.ty != Tok::String {
                return Err(self.unexpected());
            }
            let value = self.parse_string_literal()?;
            let key_name = Self::module_name_units(&key).to_vec();
            // acorn keeps the keys in a plain object, where `__proto__` never becomes an own property.
            if key_name.iter().copied().eq("__proto__".encode_utf16()) {
                nodes.push(ImportAttribute { span: self.finish(node), key, value });
                continue;
            }
            if attribute_keys.contains(&key_name) {
                return Err(self.raise(
                    Self::module_name_start(&key),
                    format!("Duplicate attribute key '{}'", String::from_utf16_lossy(&key_name)),
                ));
            }
            attribute_keys.insert(key_name);
            nodes.push(ImportAttribute { span: self.finish(node), key, value });
        }
        Ok(nodes)
    }

    fn parse_module_export_name(&mut self) -> R<ModuleName> {
        if self.ty == Tok::String {
            let literal = self.parse_string_literal()?;
            if let LitValue::String(s) = &literal.value
                && !JsStr::is_well_formed(s)
            {
                return Err(self.raise(literal.span.start as usize, "An export name cannot include a lone surrogate."));
            }
            return Ok(ModuleName::Literal(literal));
        }
        Ok(ModuleName::Identifier(self.parse_ident(true)?))
    }

    /// Set `ExpressionStatement#directive` for the directive prologue.
    pub(super) fn adapt_directive_prologue(&self, statements: &mut [Stmt]) {
        for statement in statements.iter_mut() {
            let Stmt::Expression(statement) = statement else { break };
            let Expr::Literal(literal) = &statement.expression else { break };
            if !matches!(literal.value, LitValue::String(_)) {
                break;
            }
            let first = self.cu(statement.span.start as usize);
            if first != 34 && first != 39 {
                break;
            }
            let raw = &literal.raw;
            statement.directive = Some(Rc::from(&raw[1..raw.len() - 1]));
        }
    }
}

fn clone_module_name(name: &ModuleName) -> ModuleName {
    match name {
        ModuleName::Identifier(id) => ModuleName::Identifier(id.clone()),
        ModuleName::Literal(lit) => {
            ModuleName::Literal(Literal { span: lit.span, value: lit.value.clone(), raw: lit.raw.clone() })
        }
    }
}
