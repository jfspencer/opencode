//! Expression parsing (acorn's `expression.js`).

use super::ast::*;
use super::state::*;
use crate::jsstr::JsStr;
use std::rc::Rc;

/// acorn's `forInit` argument: falsy, `true`, or `"await"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForInit {
    No,
    Yes,
    Await,
}

impl ForInit {
    pub(super) fn truthy(self) -> bool {
        self != ForInit::No
    }
}

/// An expression parsed before it is known to be an arrow function's parameter list.
pub(super) enum ArrowParam {
    Expr(Expr),
    Spread(Box<SpreadElement>),
    Rest(Box<RestElement>),
}

/// The result of one `parseSubscript` step.
enum Subscript {
    Same(Expr),
    New(Expr),
    Arrow(Expr),
}

fn is_private_field_access(node: &Expr) -> bool {
    match node {
        Expr::Member(member) => matches!(member.property, Expr::PrivateIdentifier(_)),
        Expr::Chain(chain) => is_private_field_access(&chain.expression),
        _ => false,
    }
}

impl<'a> Parser<'a> {
    fn op_value(&self) -> &'static str {
        match &self.value {
            Value::Op(op) => op,
            _ => self.ty.keyword().unwrap_or(""),
        }
    }

    /// Parse a full expression.
    pub(super) fn parse_expression(
        &mut self,
        for_init: ForInit,
        mut ref_de: Option<&mut DestructuringErrors>,
    ) -> R<Expr> {
        let start = self.start_node();
        let expr = self.parse_maybe_assign(for_init, ref_de.as_deref_mut())?;
        if self.ty == Tok::Comma {
            let mut expressions = vec![expr];
            while self.eat(Tok::Comma)? {
                expressions.push(self.parse_maybe_assign(for_init, ref_de.as_deref_mut())?);
            }
            let span = self.finish(start);
            return Ok(Expr::Sequence(Box::new(SequenceExpression { span, expressions })));
        }
        Ok(expr)
    }

    /// Parse an assignment expression, including applications of operators like `+=`.
    pub(super) fn parse_maybe_assign(
        &mut self,
        for_init: ForInit,
        ref_de: Option<&mut DestructuringErrors>,
    ) -> R<Expr> {
        self.check_stack()?;
        if self.is_contextual("yield") {
            if self.in_generator() {
                return self.parse_yield(for_init);
            }
            // The tokenizer will assume an expression is allowed after `yield`, but this isn't that kind of yield
            self.expr_allowed = false;
        }

        let mut own = DestructuringErrors::new();
        let own_de = ref_de.is_none();
        let de: &mut DestructuringErrors = match ref_de {
            Some(de) => de,
            None => &mut own,
        };
        let mut old_paren_assign = -1;
        let mut old_trailing_comma = -1;
        let mut old_double_proto = -1;
        if !own_de {
            old_paren_assign = de.parenthesized_assign;
            old_trailing_comma = de.trailing_comma;
            old_double_proto = de.double_proto;
            de.parenthesized_assign = -1;
            de.trailing_comma = -1;
        }

        let start = self.start_node();
        if self.ty == Tok::ParenL || self.ty == Tok::Name {
            self.potential_arrow_at = self.start as i64;
            self.potential_arrow_in_for_await = for_init == ForInit::Await;
        }
        let left = self.parse_maybe_conditional(for_init, Some(&mut *de))?;
        if self.ty.is_assign() {
            let operator = self.op_value();
            let is_eq = self.ty == Tok::Eq;
            let left_start = left.span().start as i64;
            let mut left = Some(left);
            let pattern = if is_eq { Some(self.to_assignable(left.take().unwrap(), false, Some(&*de))?) } else { None };
            if !own_de {
                de.parenthesized_assign = -1;
                de.trailing_comma = -1;
                de.double_proto = -1;
            }
            if de.shorthand_assign >= left_start {
                // reset because shorthand default was used correctly
                de.shorthand_assign = -1;
            }
            let left = match (pattern, left) {
                (Some(pattern), _) => {
                    self.check_lval_pattern(&pattern, Bind::None, None)?;
                    pattern
                }
                (None, Some(left)) => {
                    self.check_lval_simple_expr(&left, Bind::None)?;
                    match left {
                        Expr::Identifier(id) => Pat::Identifier(id),
                        Expr::Member(member) => Pat::Member(member),
                        _ => unreachable!("checkLValSimple accepts only identifiers and member expressions"),
                    }
                }
                (None, None) => unreachable!(),
            };
            self.next()?;
            let right = self.parse_maybe_assign(for_init, None)?;
            if old_double_proto > -1 {
                de.double_proto = old_double_proto;
            }
            let span = self.finish(start);
            return Ok(Expr::Assignment(Box::new(AssignmentExpression { span, operator, left, right })));
        } else if own_de {
            self.check_expression_errors(Some(&*de))?;
        }
        if old_paren_assign > -1 {
            de.parenthesized_assign = old_paren_assign;
        }
        if old_trailing_comma > -1 {
            de.trailing_comma = old_trailing_comma;
        }
        Ok(left)
    }

    /// Parse a ternary conditional (`?:`) operator.
    fn parse_maybe_conditional(&mut self, for_init: ForInit, mut ref_de: Option<&mut DestructuringErrors>) -> R<Expr> {
        let start = self.start_node();
        let expr = self.parse_expr_ops(for_init, ref_de.as_deref_mut())?;
        if self.has_expression_errors(ref_de.as_deref()) {
            return Ok(expr);
        }
        if self.eat(Tok::Question)? {
            let consequent = self.parse_maybe_assign(ForInit::No, None)?;
            self.expect(Tok::Colon)?;
            let alternate = self.parse_maybe_assign(for_init, None)?;
            let span = self.finish(start);
            return Ok(Expr::Conditional(Box::new(ConditionalExpression { span, test: expr, consequent, alternate })));
        }
        Ok(expr)
    }

    /// Start the precedence parser.
    fn parse_expr_ops(&mut self, for_init: ForInit, mut ref_de: Option<&mut DestructuringErrors>) -> R<Expr> {
        let start = self.start_node();
        let expr = self.parse_maybe_unary(ref_de.as_deref_mut(), false, false, for_init)?;
        if self.has_expression_errors(ref_de.as_deref()) {
            return Ok(expr);
        }
        if expr.span().start as usize == start.pos
            && matches!(&expr, Expr::Function(f) if f.kind == FunctionKind::Arrow)
        {
            return Ok(expr);
        }
        self.parse_expr_op(expr, start, -1, for_init)
    }

    /// Parse binary operators with the operator precedence parsing algorithm.
    fn parse_expr_op(&mut self, mut left: Expr, left_start: Start, min_prec: i32, for_init: ForInit) -> R<Expr> {
        // acorn recurses for each operator here; the loop builds the same left-nested tree.
        let mut links = 0;
        loop {
            links += 1;
            self.check_chain(links)?;
            let Some(mut prec) = self.ty.binop() else { return Ok(left) };
            if for_init.truthy() && self.ty == Tok::In {
                return Ok(left);
            }
            if prec <= min_prec {
                return Ok(left);
            }
            let logical = self.ty == Tok::LogicalOr || self.ty == Tok::LogicalAnd;
            let coalesce = self.ty == Tok::Coalesce;
            if coalesce {
                // Handle the precedence of `??` as equal to the range of logical expressions.
                prec = 2;
            }
            let op = self.op_value();
            self.next()?;
            let start = self.start_node();
            let operand = self.parse_maybe_unary(None, false, false, for_init)?;
            let right = self.parse_expr_op(operand, start, prec, for_init)?;
            let node = self.build_binary(left_start, left, right, op, logical || coalesce)?;
            if logical && self.ty == Tok::Coalesce
                || coalesce && (self.ty == Tok::LogicalOr || self.ty == Tok::LogicalAnd)
            {
                return Err(self.raise(
                    self.start,
                    "Logical expressions and coalesce expressions cannot be mixed. Wrap either by parentheses",
                ));
            }
            left = node;
        }
    }

    fn build_binary(
        &mut self,
        start: Start,
        left: Expr,
        right: Expr,
        operator: &'static str,
        logical: bool,
    ) -> R<Expr> {
        if let Expr::PrivateIdentifier(right) = &right {
            return Err(
                self.raise(right.span.start as usize, "Private identifier can only be left side of binary expression")
            );
        }
        let span = self.finish(start);
        Ok(if logical {
            Expr::Logical(Box::new(LogicalExpression { span, operator, left, right }))
        } else {
            Expr::Binary(Box::new(BinaryExpression { span, operator, left, right }))
        })
    }

    /// Parse unary operators, both prefix and postfix.
    pub(super) fn parse_maybe_unary(
        &mut self,
        mut ref_de: Option<&mut DestructuringErrors>,
        mut saw_unary: bool,
        inc_dec: bool,
        for_init: ForInit,
    ) -> R<Expr> {
        self.check_stack()?;
        let start = self.start_node();
        let mut expr;
        if self.is_contextual("await") && self.can_await() {
            expr = self.parse_await(for_init)?;
            saw_unary = true;
        } else if self.ty.prefix() {
            let unary;
            (expr, unary) = self.parse_prefix_unary(ref_de.as_deref(), for_init)?;
            saw_unary |= unary;
        } else if !saw_unary && self.ty == Tok::PrivateId {
            if for_init.truthy() || self.private_name_stack.is_empty() {
                return Err(self.unexpected());
            }
            expr = Expr::PrivateIdentifier(Box::new(self.parse_private_ident()?));
            // only could be private fields in 'in', such as #x in obj
            if self.ty != Tok::In {
                return Err(self.unexpected());
            }
        } else {
            expr = self.parse_expr_subscripts(ref_de.as_deref_mut(), for_init)?;
            if self.has_expression_errors(ref_de.as_deref()) {
                return Ok(expr);
            }
            while self.ty.postfix() && !self.can_insert_semicolon() {
                let operator = self.op_value();
                self.check_lval_simple_expr(&expr, Bind::None)?;
                self.next()?;
                let span = self.finish(start);
                expr = Expr::Update(Box::new(UpdateExpression { span, operator, prefix: false, argument: expr }));
            }
        }

        if !inc_dec && self.eat(Tok::StarStar)? {
            if saw_unary {
                return Err(self.unexpected_at(self.last_tok_start));
            }
            let right = self.parse_maybe_unary(None, false, false, for_init)?;
            return self.build_binary(start, expr, right, "**", false);
        }
        Ok(expr)
    }

    /// The prefix-operator branch of `parseMaybeUnary`; also says whether it counts as a unary operator for `**`.
    fn parse_prefix_unary(&mut self, ref_de: Option<&DestructuringErrors>, for_init: ForInit) -> R<(Expr, bool)> {
        let node = self.start_node();
        let update = self.ty == Tok::IncDec;
        let operator = self.op_value();
        self.next()?;
        let argument = self.parse_maybe_unary(None, true, update, for_init)?;
        self.check_expression_errors(ref_de)?;
        let mut saw_unary = false;
        if update {
            self.check_lval_simple_expr(&argument, Bind::None)?;
        } else if self.strict && operator == "delete" && matches!(argument, Expr::Identifier(_)) {
            return Err(self.raise(node.pos, "Deleting local variable in strict mode"));
        } else if operator == "delete" && is_private_field_access(&argument) {
            return Err(self.raise(node.pos, "Private fields can not be deleted"));
        } else {
            saw_unary = true;
        }
        let span = self.finish(node);
        let expr = if update {
            Expr::Update(Box::new(UpdateExpression { span, operator, prefix: true, argument }))
        } else {
            Expr::Unary(Box::new(UnaryExpression { span, operator, argument }))
        };
        Ok((expr, saw_unary))
    }

    /// Parse call, dot, and `[]`-subscript expressions.
    pub(super) fn parse_expr_subscripts(
        &mut self,
        ref_de: Option<&mut DestructuringErrors>,
        for_init: ForInit,
    ) -> R<Expr> {
        let start = self.start_node();
        let mut ref_de = ref_de;
        let expr = self.parse_expr_atom(ref_de.as_deref_mut(), for_init, false)?;
        if matches!(&expr, Expr::Function(f) if f.kind == FunctionKind::Arrow)
            && !(self.last_tok_end == self.last_tok_start + 1 && self.cu(self.last_tok_start) == 41)
        {
            return Ok(expr);
        }
        let result = self.parse_subscripts(expr, start, false, for_init)?;
        if let Some(de) = ref_de
            && let Expr::Member(member) = &result
        {
            let result_start = member.span.start as i64;
            if de.parenthesized_assign >= result_start {
                de.parenthesized_assign = -1;
            }
            if de.parenthesized_bind >= result_start {
                de.parenthesized_bind = -1;
            }
            if de.trailing_comma >= result_start {
                de.trailing_comma = -1;
            }
        }
        Ok(result)
    }

    pub(super) fn parse_subscripts(
        &mut self,
        mut base: Expr,
        start: Start,
        no_calls: bool,
        for_init: ForInit,
    ) -> R<Expr> {
        let maybe_async_arrow = match &base {
            Expr::Identifier(id) => {
                &*id.name == "async"
                    && self.last_tok_end == id.span.end as usize
                    && !self.can_insert_semicolon()
                    && id.span.end - id.span.start == 5
                    && self.potential_arrow_at == id.span.start as i64
            }
            _ => false,
        };
        let mut optional_chained = false;

        let mut links = 0;
        loop {
            match self.parse_subscript(base, start, no_calls, maybe_async_arrow, optional_chained, for_init)? {
                Subscript::New(element) => {
                    let optional = match &element {
                        Expr::Member(member) => member.optional,
                        Expr::Call(call) => call.optional,
                        _ => false,
                    };
                    if optional {
                        optional_chained = true;
                    }
                    base = element;
                    links += 1;
                    self.check_chain(links)?;
                }
                Subscript::Same(element) | Subscript::Arrow(element) => {
                    if optional_chained {
                        let span = self.finish(start);
                        return Ok(Expr::Chain(Box::new(ChainExpression { span, expression: element })));
                    }
                    return Ok(element);
                }
            }
        }
    }

    fn parse_subscript(
        &mut self,
        base: Expr,
        start: Start,
        no_calls: bool,
        maybe_async_arrow: bool,
        optional_chained: bool,
        for_init: ForInit,
    ) -> R<Subscript> {
        let optional = self.eat(Tok::QuestionDot)?;
        if no_calls && optional {
            return Err(
                self.raise(self.last_tok_start, "Optional chaining cannot appear in the callee of new expressions")
            );
        }

        let computed = self.eat(Tok::BracketL)?;
        if computed || (optional && self.ty != Tok::ParenL && self.ty != Tok::BackQuote) || self.eat(Tok::Dot)? {
            let property = if computed {
                let property = self.parse_expression(ForInit::No, None)?;
                self.expect(Tok::BracketR)?;
                property
            } else if self.ty == Tok::PrivateId && !matches!(base, Expr::Super(_)) {
                Expr::PrivateIdentifier(Box::new(self.parse_private_ident()?))
            } else {
                Expr::Identifier(Box::new(self.parse_ident(true)?))
            };
            let span = self.finish(start);
            return Ok(Subscript::New(Expr::Member(Box::new(MemberExpression {
                span,
                object: base,
                property,
                computed,
                optional,
            }))));
        } else if !no_calls && self.eat(Tok::ParenL)? {
            let mut ref_de = DestructuringErrors::new();
            let old_yield_pos = self.yield_pos;
            let old_await_pos = self.await_pos;
            let old_await_ident_pos = self.await_ident_pos;
            self.yield_pos = 0;
            self.await_pos = 0;
            self.await_ident_pos = 0;
            let expr_list = self.parse_expr_list(Tok::ParenR, true, false, Some(&mut ref_de))?;
            if maybe_async_arrow && !optional && !self.can_insert_semicolon() && self.eat(Tok::Arrow)? {
                self.check_pattern_errors(Some(&ref_de), false)?;
                self.check_yield_await_in_default_params()?;
                if self.await_ident_pos > 0 {
                    return Err(
                        self.raise(self.await_ident_pos, "Cannot use 'await' as identifier inside an async function")
                    );
                }
                self.yield_pos = old_yield_pos;
                self.await_pos = old_await_pos;
                self.await_ident_pos = old_await_ident_pos;
                let params = expr_list
                    .into_iter()
                    .map(|item| match item.unwrap() {
                        ExprOrSpread::Expr(expr) => ArrowParam::Expr(expr),
                        ExprOrSpread::Spread(spread) => ArrowParam::Spread(spread),
                    })
                    .collect();
                let function = self.parse_arrow_expression(start, params, true, for_init)?;
                return Ok(Subscript::Arrow(Expr::Function(Rc::new(function))));
            }
            self.check_expression_errors(Some(&ref_de))?;
            if old_yield_pos != 0 {
                self.yield_pos = old_yield_pos;
            }
            if old_await_pos != 0 {
                self.await_pos = old_await_pos;
            }
            if old_await_ident_pos != 0 {
                self.await_ident_pos = old_await_ident_pos;
            }
            let arguments = expr_list.into_iter().map(Option::unwrap).collect();
            let span = self.finish(start);
            return Ok(Subscript::New(Expr::Call(Box::new(CallExpression {
                span,
                callee: base,
                arguments,
                optional,
            }))));
        } else if self.ty == Tok::BackQuote {
            if optional || optional_chained {
                return Err(
                    self.raise(self.start, "Optional chaining cannot appear in the tag of tagged template expressions")
                );
            }
            let quasi = self.parse_template(true)?;
            let span = self.finish(start);
            return Ok(Subscript::New(Expr::TaggedTemplate(Box::new(TaggedTemplateExpression {
                span,
                tag: base,
                quasi,
            }))));
        }
        Ok(Subscript::Same(base))
    }

    /// Parse an atomic expression.
    pub(super) fn parse_expr_atom(
        &mut self,
        ref_de: Option<&mut DestructuringErrors>,
        for_init: ForInit,
        for_new: bool,
    ) -> R<Expr> {
        self.check_stack()?;
        // If a division operator appears in an expression position, the tokenizer got confused, and we force it to
        // read a regexp instead.
        if self.ty == Tok::Slash {
            self.read_regexp()?;
        }

        // Each case is its own function so that this frame, on every level of expression nesting, stays small.
        let can_be_arrow = self.potential_arrow_at == self.start as i64;
        match self.ty {
            Tok::Super => self.parse_super_atom(),
            Tok::This => {
                let node = self.start_node();
                self.next()?;
                Ok(Expr::This(self.finish(node)))
            }
            Tok::Name => self.parse_name_atom(can_be_arrow, for_init),
            Tok::Regexp | Tok::Num | Tok::String => Ok(Expr::Literal(Box::new(self.parse_literal()?))),
            Tok::Null | Tok::True | Tok::False => self.parse_keyword_literal(),
            Tok::ParenL => self.parse_paren_atom(ref_de, can_be_arrow, for_init),
            Tok::BracketL => self.parse_array_atom(ref_de),
            Tok::BraceL => {
                self.override_context(Ctx::BExpr);
                self.parse_obj_expr(ref_de)
            }
            Tok::Function => self.parse_function_atom(),
            Tok::Class => self.parse_class_atom(),
            Tok::New => self.parse_new(),
            Tok::BackQuote => Ok(Expr::Template(Box::new(self.parse_template(false)?))),
            Tok::Import => self.parse_expr_import(for_new),
            _ => Err(self.unexpected()),
        }
    }

    fn parse_super_atom(&mut self) -> R<Expr> {
        if !self.allow_super() {
            return Err(self.raise(self.start, "'super' keyword outside a method"));
        }
        let node = self.start_node();
        self.next()?;
        if self.ty == Tok::ParenL && !self.allow_direct_super() {
            return Err(self.raise(node.pos, "super() call outside constructor of a subclass"));
        }
        // The `super` keyword can appear as `super [ Expression ]`, `super . IdentifierName`, `super ( Arguments )`.
        if self.ty != Tok::Dot && self.ty != Tok::BracketL && self.ty != Tok::ParenL {
            return Err(self.unexpected());
        }
        Ok(Expr::Super(self.finish(node)))
    }

    fn parse_name_atom(&mut self, can_be_arrow: bool, for_init: ForInit) -> R<Expr> {
        let start = self.start_node();
        let contains_esc = self.contains_esc;
        let id = self.parse_ident(false)?;
        if !contains_esc && &*id.name == "async" && !self.can_insert_semicolon() && self.eat(Tok::Function)? {
            self.override_context(Ctx::FExpr);
            let function = self.parse_function(start, 0, true, for_init)?;
            return Ok(Expr::Function(Rc::new(function)));
        }
        if can_be_arrow && !self.can_insert_semicolon() {
            if self.eat(Tok::Arrow)? {
                let params = vec![ArrowParam::Expr(Expr::Identifier(Box::new(id)))];
                let function = self.parse_arrow_expression(start, params, false, for_init)?;
                return Ok(Expr::Function(Rc::new(function)));
            }
            if &*id.name == "async"
                && self.ty == Tok::Name
                && !contains_esc
                && (!self.potential_arrow_in_for_await || !self.value_is("of") || self.contains_esc)
            {
                let id = self.parse_ident(false)?;
                if self.can_insert_semicolon() || !self.eat(Tok::Arrow)? {
                    return Err(self.unexpected());
                }
                let params = vec![ArrowParam::Expr(Expr::Identifier(Box::new(id)))];
                let function = self.parse_arrow_expression(start, params, true, for_init)?;
                return Ok(Expr::Function(Rc::new(function)));
            }
        }
        Ok(Expr::Identifier(Box::new(id)))
    }

    fn parse_keyword_literal(&mut self) -> R<Expr> {
        let node = self.start_node();
        let value = match self.ty {
            Tok::Null => LitValue::Null,
            Tok::True => LitValue::Boolean(true),
            _ => LitValue::Boolean(false),
        };
        let raw: Rc<str> = Rc::from(self.ty.keyword().unwrap());
        self.next()?;
        Ok(Expr::Literal(Box::new(Literal { span: self.finish(node), value, raw })))
    }

    fn parse_paren_atom(
        &mut self,
        ref_de: Option<&mut DestructuringErrors>,
        can_be_arrow: bool,
        for_init: ForInit,
    ) -> R<Expr> {
        let start = self.start;
        let expr = self.parse_paren_and_distinguish_expression(can_be_arrow, for_init)?;
        if let Some(de) = ref_de {
            if de.parenthesized_assign < 0 && !matches!(expr, Expr::Identifier(_) | Expr::Member(_)) {
                de.parenthesized_assign = start as i64;
            }
            if de.parenthesized_bind < 0 {
                de.parenthesized_bind = start as i64;
            }
        }
        Ok(expr)
    }

    fn parse_array_atom(&mut self, ref_de: Option<&mut DestructuringErrors>) -> R<Expr> {
        let node = self.start_node();
        self.next()?;
        let elements = self.parse_expr_list(Tok::BracketR, true, true, ref_de)?;
        Ok(Expr::Array(Box::new(ArrayExpression { span: self.finish(node), elements })))
    }

    fn parse_function_atom(&mut self) -> R<Expr> {
        let node = self.start_node();
        self.next()?;
        let function = self.parse_function(node, 0, false, ForInit::No)?;
        Ok(Expr::Function(Rc::new(function)))
    }

    fn parse_class_atom(&mut self) -> R<Expr> {
        let node = self.start_node();
        Ok(Expr::Class(Box::new(self.parse_class(node, Some(false))?)))
    }

    fn parse_expr_import(&mut self, for_new: bool) -> R<Expr> {
        let node = self.start_node();
        // Consume `import` as an identifier for `import.meta`.
        if self.contains_esc {
            return Err(self.raise(self.start, "Escape sequence in keyword import"));
        }
        self.next()?;

        if self.ty == Tok::ParenL && !for_new {
            return self.parse_dynamic_import(node);
        }
        if self.ty == Tok::Dot {
            let atom = self.atom_str("import");
            let meta = Ident { span: self.finish(node), name: atom.name, key: atom.key };
            // parseImportMeta
            self.next()?;
            let contains_esc = self.contains_esc;
            let property = self.parse_ident(true)?;
            if &*property.name != "meta" {
                return Err(self
                    .raise(property.span.start as usize, "The only valid meta property for import is 'import.meta'"));
            }
            if contains_esc {
                return Err(self.raise(node.pos, "'import.meta' must not contain escaped characters"));
            }
            let span = self.finish(node);
            return Ok(Expr::MetaProperty(Box::new(MetaProperty { span, meta, property })));
        }
        Err(self.unexpected())
    }

    fn parse_dynamic_import(&mut self, node: Start) -> R<Expr> {
        self.next()?; // skip `(`
        let source = self.parse_maybe_assign(ForInit::No, None)?;
        let mut options = None;
        if !self.eat(Tok::ParenR)? {
            self.expect(Tok::Comma)?;
            if !self.after_trailing_comma(Tok::ParenR, false)? {
                options = Some(self.parse_maybe_assign(ForInit::No, None)?);
                if !self.eat(Tok::ParenR)? {
                    self.expect(Tok::Comma)?;
                    if !self.after_trailing_comma(Tok::ParenR, false)? {
                        return Err(self.unexpected());
                    }
                }
            }
        }
        let span = self.finish(node);
        Ok(Expr::Import(Box::new(ImportExpression { span, source, options })))
    }

    /// acorn's `parseLiteral(this.value)` for the current num, string or regexp token.
    pub(super) fn parse_literal(&mut self) -> R<Literal> {
        let node = self.start_node();
        let value = match &self.value {
            Value::Num(n) => LitValue::Number(*n),
            Value::BigInt(digits) => LitValue::BigInt(digits.clone()),
            Value::Str(s) => LitValue::String(s.clone()),
            Value::Regex { pattern, flags } => LitValue::RegExp { pattern: pattern.clone(), flags: flags.clone() },
            _ => LitValue::Null,
        };
        let raw: Rc<str> = Rc::from(self.text(self.start, self.end));
        self.next()?;
        Ok(Literal { span: self.finish(node), value, raw })
    }

    pub(super) fn parse_string_literal(&mut self) -> R<Literal> {
        self.parse_literal()
    }

    pub(super) fn parse_paren_expression(&mut self) -> R<Expr> {
        self.expect(Tok::ParenL)?;
        let val = self.parse_expression(ForInit::No, None)?;
        self.expect(Tok::ParenR)?;
        Ok(val)
    }

    fn parse_paren_and_distinguish_expression(&mut self, can_be_arrow: bool, for_init: ForInit) -> R<Expr> {
        let start = self.start_node();
        self.next()?;

        let inner_start = self.start_node();
        let mut expr_list: Vec<ArrowParam> = Vec::new();
        let mut first = true;
        let mut last_is_comma = false;
        let mut ref_de = DestructuringErrors::new();
        let old_yield_pos = self.yield_pos;
        let old_await_pos = self.await_pos;
        let mut spread_start = 0;
        self.yield_pos = 0;
        self.await_pos = 0;
        // Do not save awaitIdentPos to allow checking awaits nested in parameters
        while self.ty != Tok::ParenR {
            if first {
                first = false;
            } else {
                self.expect(Tok::Comma)?;
            }
            if self.after_trailing_comma(Tok::ParenR, true)? {
                last_is_comma = true;
                break;
            } else if self.ty == Tok::Ellipsis {
                spread_start = self.start;
                let rest = self.parse_rest_binding()?;
                expr_list.push(ArrowParam::Rest(Box::new(rest)));
                if self.ty == Tok::Comma {
                    return Err(self.raise(self.start, "Comma is not permitted after the rest element"));
                }
                break;
            } else {
                expr_list.push(ArrowParam::Expr(self.parse_maybe_assign(ForInit::No, Some(&mut ref_de))?));
            }
        }
        let inner_end = self.last_tok_end;
        let inner_end_loc = self.last_tok_end_loc;
        self.expect(Tok::ParenR)?;

        if can_be_arrow && !self.can_insert_semicolon() && self.eat(Tok::Arrow)? {
            self.check_pattern_errors(Some(&ref_de), false)?;
            self.check_yield_await_in_default_params()?;
            self.yield_pos = old_yield_pos;
            self.await_pos = old_await_pos;
            let function = self.parse_arrow_expression(start, expr_list, false, for_init)?;
            return Ok(Expr::Function(Rc::new(function)));
        }

        if expr_list.is_empty() || last_is_comma {
            return Err(self.unexpected_at(self.last_tok_start));
        }
        if spread_start != 0 {
            return Err(self.unexpected_at(spread_start));
        }
        self.check_expression_errors(Some(&ref_de))?;
        if old_yield_pos != 0 {
            self.yield_pos = old_yield_pos;
        }
        if old_await_pos != 0 {
            self.await_pos = old_await_pos;
        }

        let mut expressions: Vec<Expr> = expr_list
            .into_iter()
            .map(|item| match item {
                ArrowParam::Expr(expr) => expr,
                _ => unreachable!(),
            })
            .collect();
        if expressions.len() > 1 {
            let span = self.finish_at(inner_start, inner_end, inner_end_loc);
            return Ok(Expr::Sequence(Box::new(SequenceExpression { span, expressions })));
        }
        Ok(expressions.pop().unwrap())
    }

    fn parse_new(&mut self) -> R<Expr> {
        if self.contains_esc {
            return Err(self.raise(self.start, "Escape sequence in keyword new"));
        }
        let node = self.start_node();
        self.next()?;
        if self.ty == Tok::Dot {
            let atom = self.atom_str("new");
            let meta = Ident { span: self.finish(node), name: atom.name, key: atom.key };
            self.next()?;
            let contains_esc = self.contains_esc;
            let property = self.parse_ident(true)?;
            if &*property.name != "target" {
                return Err(
                    self.raise(property.span.start as usize, "The only valid meta property for new is 'new.target'")
                );
            }
            if contains_esc {
                return Err(self.raise(node.pos, "'new.target' must not contain escaped characters"));
            }
            if !self.allow_new_dot_target() {
                return Err(self.raise(node.pos, "'new.target' can only be used in functions and class static block"));
            }
            let span = self.finish(node);
            return Ok(Expr::MetaProperty(Box::new(MetaProperty { span, meta, property })));
        }
        let start = self.start_node();
        let atom = self.parse_expr_atom(None, ForInit::No, true)?;
        let callee = self.parse_subscripts(atom, start, true, ForInit::No)?;
        let arguments = if self.eat(Tok::ParenL)? {
            self.parse_expr_list(Tok::ParenR, true, false, None)?.into_iter().map(Option::unwrap).collect()
        } else {
            Vec::new()
        };
        let span = self.finish(node);
        Ok(Expr::New(Box::new(NewExpression { span, callee, arguments })))
    }

    fn parse_template_element(&mut self, is_tagged: bool) -> R<TemplateElement> {
        let elem = self.start_node();
        let (raw, cooked) = if self.ty == Tok::InvalidTemplate {
            if !is_tagged {
                return Err(self.raise(self.start, "Bad escape sequence in untagged template literal"));
            }
            let raw = match &self.value {
                Value::Str(raw) => normalize_crlf(raw.units()),
                _ => JsStr::new(),
            };
            (raw, None)
        } else {
            let end = self.end.min(self.input.len());
            let raw = normalize_crlf(&self.input[self.start.min(end)..end]);
            let cooked = match &self.value {
                Value::Str(cooked) => Some(cooked.clone()),
                _ => None,
            };
            (raw, cooked)
        };
        self.next()?;
        let tail = self.ty == Tok::BackQuote;
        Ok(TemplateElement { span: self.finish(elem), cooked, raw, tail })
    }

    pub(super) fn parse_template(&mut self, is_tagged: bool) -> R<TemplateLiteral> {
        let node = self.start_node();
        self.next()?;
        let mut expressions = Vec::new();
        let mut cur_elt = self.parse_template_element(is_tagged)?;
        let mut tail = cur_elt.tail;
        let mut quasis = vec![cur_elt];
        while !tail {
            if self.ty == Tok::Eof {
                return Err(self.raise(self.pos, "Unterminated template literal"));
            }
            self.expect(Tok::DollarBraceL)?;
            expressions.push(self.parse_expression(ForInit::No, None)?);
            self.expect(Tok::BraceR)?;
            cur_elt = self.parse_template_element(is_tagged)?;
            tail = cur_elt.tail;
            quasis.push(cur_elt);
        }
        self.next()?;
        Ok(TemplateLiteral { span: self.finish(node), expressions, quasis })
    }

    fn is_async_prop(&self, key: &Expr, computed: bool) -> bool {
        !computed
            && matches!(key, Expr::Identifier(id) if &*id.name == "async")
            && (matches!(self.ty, Tok::Name | Tok::Num | Tok::String | Tok::BracketL | Tok::Star)
                || self.ty.keyword().is_some())
            && !self.has_line_break(self.last_tok_end, self.start)
    }

    /// Parse an object literal.
    fn parse_obj_expr(&mut self, mut ref_de: Option<&mut DestructuringErrors>) -> R<Expr> {
        let node = self.start_node();
        let mut first = true;
        let mut has_proto = false;
        let mut properties = Vec::new();
        self.next()?;
        while !self.eat(Tok::BraceR)? {
            if !first {
                self.expect(Tok::Comma)?;
                if self.after_trailing_comma(Tok::BraceR, false)? {
                    break;
                }
            } else {
                first = false;
            }
            let prop = self.parse_property(ref_de.as_deref_mut())?;
            self.check_prop_clash(&prop, &mut has_proto, ref_de.as_deref_mut())?;
            properties.push(prop);
        }
        Ok(Expr::Object(Box::new(ObjectExpression { span: self.finish(node), properties })))
    }

    fn check_prop_clash(
        &self,
        prop: &ObjectMember,
        has_proto: &mut bool,
        ref_de: Option<&mut DestructuringErrors>,
    ) -> R<()> {
        let ObjectMember::Property(prop) = prop else { return Ok(()) };
        if prop.computed || prop.method || prop.shorthand {
            return Ok(());
        }
        let is_proto = match &prop.key {
            Expr::Identifier(id) => &*id.name == "__proto__",
            Expr::Literal(lit) => matches!(&lit.value, LitValue::String(s) if s.eq_str("__proto__")),
            _ => return Ok(()),
        };
        if is_proto && prop.kind == PropKind::Init {
            if *has_proto {
                let key_start = prop.key.span().start as i64;
                match ref_de {
                    Some(de) => {
                        if de.double_proto < 0 {
                            de.double_proto = key_start;
                        }
                    }
                    None => return Err(self.raise(key_start as usize, "Redefinition of __proto__ property")),
                }
            }
            *has_proto = true;
        }
        Ok(())
    }

    fn parse_property(&mut self, mut ref_de: Option<&mut DestructuringErrors>) -> R<ObjectMember> {
        let prop = self.start_node();
        if self.eat(Tok::Ellipsis)? {
            let argument = self.parse_maybe_assign(ForInit::No, ref_de.as_deref_mut())?;
            // To disallow trailing comma via `this.toAssignable()`.
            if self.ty == Tok::Comma
                && let Some(de) = ref_de
                && de.trailing_comma < 0
            {
                de.trailing_comma = self.start as i64;
            }
            return Ok(ObjectMember::Spread(Box::new(SpreadElement { span: self.finish(prop), argument })));
        }
        let start_pos = if ref_de.is_some() { Some(self.start_node()) } else { None };
        let mut is_generator = self.eat(Tok::Star)?;
        let contains_esc = self.contains_esc;
        let (mut key, mut computed) = self.parse_property_name()?;
        let is_async;
        if !contains_esc && !is_generator && self.is_async_prop(&key, computed) {
            is_async = true;
            is_generator = self.eat(Tok::Star)?;
            (key, computed) = self.parse_property_name()?;
        } else {
            is_async = false;
        }

        // parsePropertyValue
        if (is_generator || is_async) && self.ty == Tok::Colon {
            return Err(self.unexpected());
        }
        let mut method = false;
        let mut shorthand = false;
        let kind;
        let value;
        if self.eat(Tok::Colon)? {
            value = self.parse_maybe_assign(ForInit::No, ref_de)?;
            kind = PropKind::Init;
        } else if self.ty == Tok::ParenL {
            method = true;
            value = Expr::Function(Rc::new(self.parse_method(is_generator, is_async, false)?));
            kind = PropKind::Init;
        } else if !contains_esc
            && !computed
            && matches!(&key, Expr::Identifier(id) if &*id.name == "get" || &*id.name == "set")
            && self.ty != Tok::Comma
            && self.ty != Tok::BraceR
            && self.ty != Tok::Eq
        {
            if is_generator || is_async {
                return Err(self.unexpected());
            }
            // parseGetterSetter
            let Expr::Identifier(id) = &key else { unreachable!() };
            kind = if &*id.name == "get" { PropKind::Get } else { PropKind::Set };
            (key, computed) = self.parse_property_name()?;
            let function = self.parse_method(false, false, false)?;
            let param_count = if kind == PropKind::Get { 0 } else { 1 };
            if function.params.len() != param_count {
                let start = function.span.start as usize;
                return Err(self.raise(
                    start,
                    if kind == PropKind::Get {
                        "getter should have no params"
                    } else {
                        "setter should have exactly one param"
                    },
                ));
            } else if kind == PropKind::Set
                && let Pat::Rest(rest) = &function.params[0]
            {
                return Err(self.raise(rest.span.start as usize, "Setter cannot use rest params"));
            }
            value = Expr::Function(Rc::new(function));
        } else if !computed && matches!(key, Expr::Identifier(_)) {
            if is_generator || is_async {
                return Err(self.unexpected());
            }
            let Expr::Identifier(id) = &key else { unreachable!() };
            let id = (**id).clone();
            self.check_unreserved(id.span.start as usize, Some(&id.name))?;
            if &*id.name == "await" && self.await_ident_pos == 0 {
                self.await_ident_pos = start_pos.map_or(0, |s| s.pos);
            }
            match (ref_de, start_pos) {
                (Some(de), Some(start)) if self.ty == Tok::Eq => {
                    if de.shorthand_assign < 0 {
                        de.shorthand_assign = self.start as i64;
                    }
                    // parseMaybeDefault(startPos, startLoc, copyNode(prop.key)): an AssignmentPattern, encoded as an
                    // `=` assignment of a shorthand property (see estree.rs and toAssignable)
                    self.next()?;
                    let right = self.parse_maybe_assign(ForInit::No, None)?;
                    let span = self.finish(start);
                    let left = Pat::Identifier(Box::new(id));
                    value = Expr::Assignment(Box::new(AssignmentExpression { span, operator: "=", left, right }));
                }
                _ => value = Expr::Identifier(Box::new(id)),
            }
            kind = PropKind::Init;
            shorthand = true;
        } else {
            return Err(self.unexpected());
        }
        Ok(ObjectMember::Property(Box::new(Property {
            span: self.finish(prop),
            key,
            value,
            kind,
            method,
            shorthand,
            computed,
        })))
    }

    pub(super) fn parse_property_name(&mut self) -> R<(Expr, bool)> {
        if self.eat(Tok::BracketL)? {
            let key = self.parse_maybe_assign(ForInit::No, None)?;
            self.expect(Tok::BracketR)?;
            return Ok((key, true));
        }
        let key = if self.ty == Tok::Num || self.ty == Tok::String {
            self.parse_expr_atom(None, ForInit::No, false)?
        } else {
            Expr::Identifier(Box::new(self.parse_ident(true)?))
        };
        Ok((key, false))
    }

    /// Parse object or class method.
    pub(super) fn parse_method(&mut self, is_generator: bool, is_async: bool, allow_direct_super: bool) -> R<Function> {
        let node = self.start_node();
        let old_yield_pos = self.yield_pos;
        let old_await_pos = self.await_pos;
        let old_await_ident_pos = self.await_ident_pos;

        self.yield_pos = 0;
        self.await_pos = 0;
        self.await_ident_pos = 0;
        self.enter_scope(
            function_flags(is_async, is_generator)
                | SCOPE_SUPER
                | if allow_direct_super { SCOPE_DIRECT_SUPER } else { 0 },
        );

        self.expect(Tok::ParenL)?;
        let params: Vec<Pat> = self.parse_binding_list(Tok::ParenR, false, true)?.into_iter().flatten().collect();
        self.check_yield_await_in_default_params()?;
        let body = self.parse_function_body(node, None, &params, false, true, ForInit::No)?;

        self.yield_pos = old_yield_pos;
        self.await_pos = old_await_pos;
        self.await_ident_pos = old_await_ident_pos;
        Ok(Function {
            span: self.finish(node),
            kind: FunctionKind::Expression,
            id: None,
            params,
            body,
            generator: is_generator,
            is_async,
        })
    }

    /// Parse arrow function expression with given parameters.
    pub(super) fn parse_arrow_expression(
        &mut self,
        node: Start,
        params: Vec<ArrowParam>,
        is_async: bool,
        for_init: ForInit,
    ) -> R<Function> {
        let old_yield_pos = self.yield_pos;
        let old_await_pos = self.await_pos;
        let old_await_ident_pos = self.await_ident_pos;

        self.enter_scope(function_flags(is_async, false) | SCOPE_ARROW);

        self.yield_pos = 0;
        self.await_pos = 0;
        self.await_ident_pos = 0;

        let params = self.to_assignable_list(params, true)?;
        let body = self.parse_function_body(node, None, &params, true, false, for_init)?;

        self.yield_pos = old_yield_pos;
        self.await_pos = old_await_pos;
        self.await_ident_pos = old_await_ident_pos;
        Ok(Function {
            span: self.finish(node),
            kind: FunctionKind::Arrow,
            id: None,
            params,
            body,
            generator: false,
            is_async,
        })
    }

    /// Parse function body and check parameters.
    pub(super) fn parse_function_body(
        &mut self,
        node: Start,
        id: Option<&Ident>,
        params: &[Pat],
        is_arrow_function: bool,
        is_method: bool,
        for_init: ForInit,
    ) -> R<FunctionBody> {
        let is_expression = is_arrow_function && self.ty != Tok::BraceL;
        let old_strict = self.strict;
        let mut use_strict = false;

        let body = if is_expression {
            let body = self.parse_maybe_assign(for_init, None)?;
            self.check_params(params, false)?;
            FunctionBody::Expr(body)
        } else {
            let simple = params.iter().all(|param| matches!(param, Pat::Identifier(_)));
            let non_simple = !simple;
            if !old_strict || non_simple {
                use_strict = self.strict_directive(self.end);
                // If this is a strict mode function, verify that argument names are not repeated, and it does not
                // try to bind the words `eval` or `arguments`.
                if use_strict && non_simple {
                    return Err(self
                        .raise(node.pos, "Illegal 'use strict' directive in function with non-simple parameter list"));
                }
            }
            // Start a new scope with regard to labels and the `inFunction` flag.
            let old_labels = std::mem::take(&mut self.labels);
            if use_strict {
                self.strict = true;
            }

            // Add the params to varDeclaredNames to ensure that an error is thrown if a let/const declaration in the
            // function clashes with one of the params.
            self.check_params(params, !old_strict && !use_strict && !is_arrow_function && !is_method && simple)?;
            // Ensure the function name isn't a forbidden identifier in strict mode, e.g. 'eval'
            if self.strict
                && let Some(id) = id
            {
                self.check_lval_simple_ident(id, Bind::Outside, None)?;
            }
            let mut block = self.parse_block(false, None, use_strict && !old_strict)?;
            self.adapt_directive_prologue(&mut block.body);
            self.labels = old_labels;
            FunctionBody::Block(block)
        };
        self.exit_scope();
        Ok(body)
    }

    /// Checks function params for various disallowed patterns such as using "eval" or "arguments" and duplicate
    /// parameters.
    fn check_params(&mut self, params: &[Pat], allow_duplicates: bool) -> R<()> {
        let mut name_hash = NameSet::default();
        for param in params {
            self.check_lval_inner_pattern(
                param,
                Bind::Var,
                if allow_duplicates { None } else { Some(&mut name_hash) },
            )?;
        }
        Ok(())
    }

    /// Parses a comma-separated list of expressions.
    pub(super) fn parse_expr_list(
        &mut self,
        close: Tok,
        allow_trailing_comma: bool,
        allow_empty: bool,
        mut ref_de: Option<&mut DestructuringErrors>,
    ) -> R<Vec<Option<ExprOrSpread>>> {
        let mut elts = Vec::new();
        let mut first = true;
        while !self.eat(close)? {
            if !first {
                self.expect(Tok::Comma)?;
                if allow_trailing_comma && self.after_trailing_comma(close, false)? {
                    break;
                }
            } else {
                first = false;
            }

            let elt = if allow_empty && self.ty == Tok::Comma {
                None
            } else if self.ty == Tok::Ellipsis {
                let spread = self.parse_spread(ref_de.as_deref_mut())?;
                if self.ty == Tok::Comma
                    && let Some(de) = ref_de.as_deref_mut()
                    && de.trailing_comma < 0
                {
                    de.trailing_comma = self.start as i64;
                }
                Some(ExprOrSpread::Spread(Box::new(spread)))
            } else {
                Some(ExprOrSpread::Expr(self.parse_maybe_assign(ForInit::No, ref_de.as_deref_mut())?))
            };
            elts.push(elt);
        }
        Ok(elts)
    }

    pub(super) fn check_unreserved(&self, start: usize, name: Option<&str>) -> R<()> {
        let Some(name) = name else { return Ok(()) };
        if self.in_generator() && name == "yield" {
            return Err(self.raise(start, "Cannot use 'yield' as identifier inside a generator"));
        }
        if self.in_async() && name == "await" {
            return Err(self.raise(start, "Cannot use 'await' as identifier inside an async function"));
        }
        if self.current_this_scope_flags() & SCOPE_VAR == 0 && name == "arguments" {
            return Err(self.raise(start, "Cannot use 'arguments' in class field initializer"));
        }
        if self.in_class_static_block() && (name == "arguments" || name == "await") {
            return Err(self.raise(start, format!("Cannot use {name} in class static initialization block")));
        }
        let units: Vec<u16> = name.encode_utf16().collect();
        if Tok::from_keyword(&units).is_some() {
            return Err(self.raise(start, format!("Unexpected keyword '{name}'")));
        }
        let reserved = if self.strict {
            matches!(
                name,
                "enum"
                    | "implements"
                    | "interface"
                    | "let"
                    | "package"
                    | "private"
                    | "protected"
                    | "public"
                    | "static"
                    | "yield"
            )
        } else {
            name == "enum"
        };
        if reserved {
            if !self.in_async() && name == "await" {
                return Err(self.raise(start, "Cannot use keyword 'await' outside an async function"));
            }
            return Err(self.raise(start, format!("The keyword '{name}' is reserved")));
        }
        Ok(())
    }

    /// Parse the next token as an identifier. If `liberal` is true (used when parsing properties), it will also
    /// convert keywords into identifiers.
    pub(super) fn parse_ident(&mut self, liberal: bool) -> R<Ident> {
        let start = self.start_node();
        let atom = if self.ty == Tok::Name {
            match &self.value {
                Value::Word(atom) => atom.clone(),
                _ => unreachable!(),
            }
        } else if let Some(keyword) = self.ty.keyword() {
            // `class` and `function` keywords push new context into this.context; there is no chance to pop it if
            // the keyword is consumed as an identifier such as a property name.
            if (keyword == "class" || keyword == "function")
                && (self.last_tok_end != self.last_tok_start + 1 || self.cu(self.last_tok_start) != 46)
            {
                self.context.pop();
            }
            self.ty = Tok::Name;
            self.atom_str(keyword)
        } else {
            return Err(self.unexpected());
        };
        self.next_with(liberal)?;
        let node = Ident { span: self.finish(start), name: atom.name, key: atom.key };
        if !liberal {
            self.check_unreserved(start.pos, Some(&node.name))?;
            if &*node.name == "await" && self.await_ident_pos == 0 {
                self.await_ident_pos = start.pos;
            }
        }
        Ok(node)
    }

    pub(super) fn parse_private_ident(&mut self) -> R<PrivateIdent> {
        let start = self.start_node();
        let name = match (&self.value, self.ty) {
            (Value::Word(atom), Tok::PrivateId) => atom.name.clone(),
            _ => return Err(self.unexpected()),
        };
        self.next()?;
        let node = PrivateIdent { span: self.finish(start), name };

        // For validating existence
        match self.private_name_stack.last_mut() {
            None => {
                return Err(self.raise(
                    node.span.start as usize,
                    format!("Private field '#{}' must be declared in an enclosing class", node.name),
                ));
            }
            Some(names) => names.used.push((node.name.clone(), node.span.start as usize)),
        }
        Ok(node)
    }

    /// Parses yield expression inside generator.
    fn parse_yield(&mut self, for_init: ForInit) -> R<Expr> {
        if self.yield_pos == 0 {
            self.yield_pos = self.start;
        }
        let node = self.start_node();
        self.next()?;
        let (delegate, argument) = if self.ty == Tok::Semi
            || self.can_insert_semicolon()
            || (self.ty != Tok::Star && !self.ty.starts_expr())
        {
            (false, None)
        } else {
            let delegate = self.eat(Tok::Star)?;
            (delegate, Some(self.parse_maybe_assign(for_init, None)?))
        };
        let span = self.finish(node);
        Ok(Expr::Yield(Box::new(YieldExpression { span, argument, delegate })))
    }

    fn parse_await(&mut self, for_init: ForInit) -> R<Expr> {
        if self.await_pos == 0 {
            self.await_pos = self.start;
        }
        let node = self.start_node();
        self.next()?;
        let argument = self.parse_maybe_unary(None, true, false, for_init)?;
        let span = self.finish(node);
        Ok(Expr::Await(Box::new(AwaitExpression { span, argument })))
    }
}

/// acorn's `.replace(/\r\n?/g, "\n")`.
fn normalize_crlf(units: &[u16]) -> JsStr {
    if !units.contains(&13) {
        return JsStr::from_slice(units);
    }
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        if units[i] == 13 {
            out.push(10);
            if units.get(i + 1) == Some(&10) {
                i += 1;
            }
        } else {
            out.push(units[i]);
        }
        i += 1;
    }
    JsStr::from_units(out)
}
