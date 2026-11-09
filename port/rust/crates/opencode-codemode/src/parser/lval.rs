//! Patterns: converting expressions to assignment targets, binding patterns, and lvalue checks (acorn's
//! `lval.js`).

use super::ast::*;
use super::expression::{ArrowParam, ForInit};
use super::state::*;

// `to_assignable` and `to_assignable_list` keep acorn's names (`toAssignable`), not Rust's `to_*` convention.
#[allow(clippy::wrong_self_convention)]
impl<'a> Parser<'a> {
    /// Convert an existing expression atom to an assignable pattern if possible.
    pub(super) fn to_assignable(&mut self, expr: Expr, is_binding: bool, ref_de: Option<&DestructuringErrors>) -> R<Pat> {
        match expr {
            Expr::Identifier(id) => {
                if self.in_async() && &*id.name == "await" {
                    return Err(self.raise(id.span.start as usize, "Cannot use 'await' as identifier inside an async function"));
                }
                Ok(Pat::Identifier(id))
            }
            Expr::Object(object) => {
                self.check_pattern_errors(ref_de, true)?;
                let object = *object;
                let mut properties = Vec::with_capacity(object.properties.len());
                for prop in object.properties {
                    match prop {
                        ObjectMember::Property(prop) => {
                            let prop = *prop;
                            if prop.kind != PropKind::Init {
                                return Err(self.raise(prop.key.span().start as usize, "Object pattern can't contain getter or setter"));
                            }
                            let value = match prop.value {
                                // A shorthand property's `=` default is already an AssignmentPattern.
                                Expr::Assignment(assign) if prop.shorthand => {
                                    let assign = *assign;
                                    Pat::Assignment(Box::new(AssignmentPattern { span: assign.span, left: assign.left, right: assign.right }))
                                }
                                value => self.to_assignable(value, is_binding, None)?,
                            };
                            properties.push(ObjectPatternMember::Property(Box::new(AssignmentProperty {
                                span: prop.span,
                                key: prop.key,
                                value,
                                shorthand: prop.shorthand,
                                computed: prop.computed,
                            })));
                        }
                        ObjectMember::Spread(spread) => {
                            let rest = self.spread_to_rest(*spread, is_binding)?;
                            // AssignmentRestProperty: it is a Syntax Error if the target is an array or object
                            // literal.
                            if matches!(rest.argument, Pat::Array(_) | Pat::Object(_)) {
                                return Err(self.raise(rest.argument.span().start as usize, "Unexpected token"));
                            }
                            properties.push(ObjectPatternMember::Rest(Box::new(rest)));
                        }
                    }
                }
                Ok(Pat::Object(Box::new(ObjectPattern { span: object.span, properties })))
            }
            Expr::Array(array) => {
                self.check_pattern_errors(ref_de, true)?;
                let array = *array;
                let mut elements = Vec::with_capacity(array.elements.len());
                for elt in array.elements {
                    elements.push(match elt {
                        None => None,
                        Some(ExprOrSpread::Expr(expr)) => Some(self.to_assignable(expr, is_binding, None)?),
                        Some(ExprOrSpread::Spread(spread)) => Some(Pat::Rest(Box::new(self.spread_to_rest(*spread, is_binding)?))),
                    });
                }
                Ok(Pat::Array(Box::new(ArrayPattern { span: array.span, elements })))
            }
            Expr::Assignment(assign) => {
                let assign = *assign;
                if assign.operator != "=" {
                    return Err(self.raise(assign.left.span().end as usize, "Only '=' operator can be used for specifying default value."));
                }
                self.reassign_pattern(&assign.left, is_binding)?;
                Ok(Pat::Assignment(Box::new(AssignmentPattern { span: assign.span, left: assign.left, right: assign.right })))
            }
            Expr::Chain(chain) => Err(self.raise(chain.span.start as usize, "Optional chaining cannot appear in left-hand side")),
            Expr::Member(member) if !is_binding => Ok(Pat::Member(member)),
            other => Err(self.raise(other.span().start as usize, "Assigning to rvalue")),
        }
    }

    /// `toAssignable` applied to a node that is already a pattern.
    fn reassign_pattern(&self, pat: &Pat, is_binding: bool) -> R<()> {
        match pat {
            Pat::Identifier(id) => {
                if self.in_async() && &*id.name == "await" {
                    return Err(self.raise(id.span.start as usize, "Cannot use 'await' as identifier inside an async function"));
                }
                Ok(())
            }
            Pat::Member(member) if is_binding => Err(self.raise(member.span.start as usize, "Assigning to rvalue")),
            _ => Ok(()),
        }
    }

    fn spread_to_rest(&mut self, spread: SpreadElement, is_binding: bool) -> R<RestElement> {
        let argument = self.to_assignable(spread.argument, is_binding, None)?;
        if let Pat::Assignment(assign) = &argument {
            return Err(self.raise(assign.span.start as usize, "Rest elements cannot have a default value"));
        }
        Ok(RestElement { span: spread.span, argument })
    }

    /// Convert a list of expression atoms to a binding list.
    pub(super) fn to_assignable_list(&mut self, list: Vec<ArrowParam>, is_binding: bool) -> R<Vec<Pat>> {
        let mut out = Vec::with_capacity(list.len());
        for elt in list {
            out.push(match elt {
                ArrowParam::Expr(expr) => self.to_assignable(expr, is_binding, None)?,
                ArrowParam::Spread(spread) => Pat::Rest(Box::new(self.spread_to_rest(*spread, is_binding)?)),
                ArrowParam::Rest(rest) => Pat::Rest(rest),
            });
        }
        Ok(out)
    }

    /// Parses spread element.
    pub(super) fn parse_spread(&mut self, ref_de: Option<&mut DestructuringErrors>) -> R<SpreadElement> {
        let node = self.start_node();
        self.next()?;
        let argument = self.parse_maybe_assign(ForInit::No, ref_de)?;
        Ok(SpreadElement { span: self.finish(node), argument })
    }

    pub(super) fn parse_rest_binding(&mut self) -> R<RestElement> {
        let node = self.start_node();
        self.next()?;
        let argument = self.parse_binding_atom()?;
        Ok(RestElement { span: self.finish(node), argument })
    }

    /// Parses lvalue (assignable) atom.
    pub(super) fn parse_binding_atom(&mut self) -> R<Pat> {
        self.check_stack()?;
        match self.ty {
            Tok::BracketL => {
                let node = self.start_node();
                self.next()?;
                let elements = self.parse_binding_list(Tok::BracketR, true, true)?;
                Ok(Pat::Array(Box::new(ArrayPattern { span: self.finish(node), elements })))
            }
            Tok::BraceL => self.parse_obj_pattern(),
            _ => Ok(Pat::Identifier(Box::new(self.parse_ident(false)?))),
        }
    }

    pub(super) fn parse_binding_list(&mut self, close: Tok, allow_empty: bool, allow_trailing_comma: bool) -> R<Vec<Option<Pat>>> {
        let mut elts = Vec::new();
        let mut first = true;
        while !self.eat(close)? {
            if first {
                first = false;
            } else {
                self.expect(Tok::Comma)?;
            }
            if allow_empty && self.ty == Tok::Comma {
                elts.push(None);
            } else if allow_trailing_comma && self.after_trailing_comma(close, false)? {
                break;
            } else if self.ty == Tok::Ellipsis {
                let rest = self.parse_rest_binding()?;
                elts.push(Some(Pat::Rest(Box::new(rest))));
                if self.ty == Tok::Comma {
                    return Err(self.raise(self.start, "Comma is not permitted after the rest element"));
                }
                self.expect(close)?;
                break;
            } else {
                let start = self.start_node();
                elts.push(Some(self.parse_maybe_default(start, None)?));
            }
        }
        Ok(elts)
    }

    /// Parses assignment pattern around given atom if possible.
    pub(super) fn parse_maybe_default(&mut self, start: Start, left: Option<Pat>) -> R<Pat> {
        let left = match left {
            Some(left) => left,
            None => self.parse_binding_atom()?,
        };
        if !self.eat(Tok::Eq)? {
            return Ok(left);
        }
        let right = self.parse_maybe_assign(ForInit::No, None)?;
        Ok(Pat::Assignment(Box::new(AssignmentPattern { span: self.finish(start), left, right })))
    }

    /// Parse an object binding pattern (`parseObj(true)`).
    fn parse_obj_pattern(&mut self) -> R<Pat> {
        let node = self.start_node();
        let mut first = true;
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
            properties.push(self.parse_pattern_property()?);
        }
        Ok(Pat::Object(Box::new(ObjectPattern { span: self.finish(node), properties })))
    }

    fn parse_pattern_property(&mut self) -> R<ObjectPatternMember> {
        let prop = self.start_node();
        if self.eat(Tok::Ellipsis)? {
            let argument = Pat::Identifier(Box::new(self.parse_ident(false)?));
            if self.ty == Tok::Comma {
                return Err(self.raise(self.start, "Comma is not permitted after the rest element"));
            }
            return Ok(ObjectPatternMember::Rest(Box::new(RestElement { span: self.finish(prop), argument })));
        }
        let start_pos = self.start_node();
        let (key, computed) = self.parse_property_name()?;
        let value;
        let mut shorthand = false;
        if self.eat(Tok::Colon)? {
            let start = self.start_node();
            value = self.parse_maybe_default(start, None)?;
        } else if self.ty == Tok::ParenL {
            return Err(self.unexpected());
        } else if let (false, Expr::Identifier(id)) = (computed, &key) {
            let id = (**id).clone();
            self.check_unreserved(id.span.start as usize, Some(&id.name))?;
            if &*id.name == "await" && self.await_ident_pos == 0 {
                self.await_ident_pos = start_pos.pos;
            }
            value = self.parse_maybe_default(start_pos, Some(Pat::Identifier(Box::new(id))))?;
            shorthand = true;
        } else {
            return Err(self.unexpected());
        }
        Ok(ObjectPatternMember::Property(Box::new(AssignmentProperty { span: self.finish(prop), key, value, shorthand, computed })))
    }

    // ## Lvalue checks

    /// `checkLValSimple` for an identifier.
    pub(super) fn check_lval_simple_ident(&mut self, id: &Ident, binding: Bind, clashes: Option<&mut NameSet>) -> R<()> {
        let is_bind = binding != Bind::None;
        let name = &*id.name;
        let start = id.span.start as usize;
        if self.strict
            && matches!(
                name,
                "enum" | "implements" | "interface" | "let" | "package" | "private" | "protected" | "public" | "static" | "yield" | "eval" | "arguments"
            )
        {
            return Err(self.raise(start, format!("{}{name} in strict mode", if is_bind { "Binding " } else { "Assigning to " })));
        }
        if is_bind {
            if binding == Bind::Lexical && name == "let" {
                return Err(self.raise(start, "let is disallowed as a lexically bound name"));
            }
            if let Some(clashes) = clashes {
                if clashes.contains(name) {
                    return Err(self.raise(start, "Argument name clash"));
                }
                clashes.push(&id.name);
            }
            if binding != Bind::Outside {
                self.declare_name(&id.name, binding, start)?;
            }
        }
        Ok(())
    }

    /// `checkLValSimple` for an expression that was not converted to a pattern.
    pub(super) fn check_lval_simple_expr(&mut self, expr: &Expr, binding: Bind) -> R<()> {
        let is_bind = binding != Bind::None;
        match expr {
            Expr::Identifier(id) => self.check_lval_simple_ident(id, binding, None),
            Expr::Chain(chain) => Err(self.raise(chain.span.start as usize, "Optional chaining cannot appear in left-hand side")),
            Expr::Member(member) => {
                if is_bind {
                    return Err(self.raise(member.span.start as usize, "Binding member expression"));
                }
                Ok(())
            }
            other => Err(self.raise(other.span().start as usize, if is_bind { "Binding rvalue" } else { "Assigning to rvalue" })),
        }
    }

    fn check_lval_simple_pat(&mut self, pat: &Pat, binding: Bind, clashes: Option<&mut NameSet>) -> R<()> {
        let is_bind = binding != Bind::None;
        match pat {
            Pat::Identifier(id) => self.check_lval_simple_ident(id, binding, clashes),
            Pat::Member(member) => {
                if is_bind {
                    return Err(self.raise(member.span.start as usize, "Binding member expression"));
                }
                Ok(())
            }
            other => Err(self.raise(other.span().start as usize, if is_bind { "Binding rvalue" } else { "Assigning to rvalue" })),
        }
    }

    pub(super) fn check_lval_pattern(&mut self, pat: &Pat, binding: Bind, mut clashes: Option<&mut NameSet>) -> R<()> {
        match pat {
            Pat::Object(object) => {
                for prop in &object.properties {
                    match prop {
                        ObjectPatternMember::Property(prop) => self.check_lval_inner_pattern(&prop.value, binding, clashes.as_deref_mut())?,
                        ObjectPatternMember::Rest(rest) => self.check_lval_pattern(&rest.argument, binding, clashes.as_deref_mut())?,
                    }
                }
                Ok(())
            }
            Pat::Array(array) => {
                for elem in array.elements.iter().flatten() {
                    self.check_lval_inner_pattern(elem, binding, clashes.as_deref_mut())?;
                }
                Ok(())
            }
            _ => self.check_lval_simple_pat(pat, binding, clashes),
        }
    }

    pub(super) fn check_lval_inner_pattern(&mut self, pat: &Pat, binding: Bind, clashes: Option<&mut NameSet>) -> R<()> {
        match pat {
            Pat::Assignment(assign) => self.check_lval_pattern(&assign.left, binding, clashes),
            Pat::Rest(rest) => self.check_lval_pattern(&rest.argument, binding, clashes),
            _ => self.check_lval_pattern(pat, binding, clashes),
        }
    }
}
