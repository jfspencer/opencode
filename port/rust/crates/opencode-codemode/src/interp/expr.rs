//! Expression evaluation (interpreter.ts `Frame.evaluate` and its helpers): operators and coercion, member access,
//! calls, assignment, literals, templates, `await`, `yield`, and `new`.

use crate::interp::callback::Hint;
use crate::interp::coerce::{
    is_opaque, reject_circular_insertion, to_number_plain, to_string_plain, typeof_value,
};
use crate::interp::errors::{Fail, invalid_data, type_error, unsupported_syntax};
use crate::interp::eval::{Frame, LFut, Reference, literal_value, loc_of};
use crate::interp::limits::check_string_length;
use crate::interp::object::{define, has, remove, set};
use crate::interp::runner::{Boundary, activation, drive_inline, yield_leaf};
use crate::interp::value::*;
use crate::interp::{Ctx, Fut, Loc, R, boxed};
use crate::jsstr::{JsStr, JsStrBuf};
use crate::parser::ast::*;
use std::rc::Rc;

/// An evaluated property key before ToPropertyKey's canonical form: tool paths reject numbers.
#[derive(Clone)]
pub enum RawKey {
    Str(JsStr),
    Num(f64),
    Sym(Sym),
}

impl RawKey {
    pub fn to_prop(&self) -> PropKey {
        match self {
            RawKey::Str(text) => PropKey::from_js(text.clone()),
            RawKey::Num(n) => PropKey::from_number(*n),
            RawKey::Sym(sym) => PropKey::Sym(*sym),
        }
    }
    pub fn display(&self) -> JsStr {
        match self {
            RawKey::Str(text) => text.clone(),
            RawKey::Num(n) => JsStr::from(crate::numconv::to_string(*n)),
            RawKey::Sym(sym) => JsStr::from(format!("Symbol({})", sym.description())),
        }
    }
}

/// ToPropertyKey on a primitive (or an opaque value, which keeps its built-in string form).
pub fn property_key(value: &Value) -> RawKey {
    match value {
        Value::Str(text) => RawKey::Str(text.clone()),
        Value::Num(n) => RawKey::Num(*n),
        Value::Sym(sym) => RawKey::Sym(*sym),
        other => RawKey::Str(to_string_plain(other)),
    }
}

const COMPOUND: [&str; 12] = ["+=", "-=", "*=", "/=", "%=", "**=", "&=", "|=", "^=", "<<=", ">>=", ">>>="];

pub fn is_compound(operator: &str) -> bool {
    COMPOUND.contains(&operator)
}

fn primitive_operator(operator: &str) -> bool {
    matches!(operator, "+" | "-" | "*" | "/" | "%" | "**" | "<" | "<=" | ">" | ">=" | "&" | "|" | "^" | "<<" | ">>" | ">>>")
}

// Native engines name the callee (`search(...).catch is not a function`), including call chains.
pub fn callee_description(node: Option<&Expr>) -> Option<String> {
    match node? {
        Expr::Identifier(id) => Some(id.name.to_string()),
        Expr::Call(call) => callee_description(Some(&call.callee)).map(|target| format!("{target}(...)")),
        Expr::Member(member) => {
            let key = match (&member.property, member.computed) {
                (Expr::Identifier(id), false) => id.name.to_string(),
                (Expr::Literal(lit), _) => match &lit.value {
                    LitValue::String(text) => text.to_rust_lossy(),
                    _ => return None,
                },
                _ => return None,
            };
            callee_description(Some(&member.object)).map(|object| format!("{object}.{key}"))
        }
        _ => None,
    }
}

fn symbol_error(to: &str) -> Fail {
    Fail::Host(Rc::new((JsStr::from("TypeError"), JsStr::from(format!("Cannot convert a symbol to a {to}")))))
}

/// ToNumber on a primitive, as the host operators apply it (a symbol throws).
pub fn prim_number(value: &Value) -> R<f64> {
    if let Value::Sym(_) = value {
        return Err(symbol_error("number"));
    }
    Ok(to_number_plain(value))
}

fn prim_string(value: &Value) -> R<JsStr> {
    if let Value::Sym(_) = value {
        return Err(symbol_error("string"));
    }
    Ok(to_string_plain(value))
}

/// Math.pow / `**` with JS semantics.
pub fn js_pow(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() {
        return f64::NAN;
    }
    if exponent == 0.0 {
        return 1.0;
    }
    if (base == 1.0 || base == -1.0) && exponent.is_infinite() {
        return f64::NAN;
    }
    base.powf(exponent)
}

/// `==` between two primitives (host IsLooselyEqual).
pub fn loose_eq_primitive(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
        (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => false,
        (Value::Num(a), Value::Num(b)) => a == b,
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Sym(a), Value::Sym(b)) => a == b,
        (Value::Sym(_), _) | (_, Value::Sym(_)) => false,
        (Value::Bool(b), other) => loose_eq_primitive(&Value::Num(if *b { 1.0 } else { 0.0 }), other),
        (other, Value::Bool(b)) => loose_eq_primitive(other, &Value::Num(if *b { 1.0 } else { 0.0 })),
        (Value::Num(a), Value::Str(b)) => *a == crate::numconv::string_to_number(b.units()),
        (Value::Str(a), Value::Num(b)) => crate::numconv::string_to_number(a.units()) == *b,
        _ => false,
    }
}

fn is_objectish(value: &Value) -> bool {
    matches!(value, Value::Obj(_) | Value::Tool(_))
}

pub fn instanceof_value(lhs: &Value, rhs: &Value, loc: Loc) -> R<bool> {
    let Value::Obj(ctor) = rhs else {
        return Err(type_error("The right-hand side of 'instanceof' is not callable.").at(Some(loc)));
    };
    if !ctor.is_callable() {
        return Err(type_error("The right-hand side of 'instanceof' is not callable.").at(Some(loc)));
    }
    let Value::Obj(proto) = crate::interp::object::get_data(ctor, &PropKey::from_str("prototype")) else {
        return Err(type_error("The right-hand side of 'instanceof' has no 'prototype' object.").at(Some(loc)));
    };
    Ok(crate::interp::object::has_prototype(lhs, &proto))
}

impl Frame {
    pub fn eval<'a>(&'a self, node: &'a Expr) -> LFut<'a, R<Value>> {
        Box::pin(async move {
            match self.eval_opt(node).await? {
                Some(value) => Ok(value),
                None => Ok(Value::Undefined),
            }
        })
    }

    /// Evaluates an expression, keeping an optional chain's short-circuit (None) for the chain that contains it.
    pub fn eval_opt<'a>(&'a self, node: &'a Expr) -> LFut<'a, R<Option<Value>>> {
        Box::pin(async move {
            match node {
                Expr::Member(member) => self.eval_member(member).await,
                Expr::Call(call) => self.eval_call(call).await,
                other => self.eval_value(other).await.map(Some),
            }
        })
    }

    async fn eval_value(&self, node: &Expr) -> R<Value> {
        let ctx = &self.ctx;
        match node {
            Expr::Literal(lit) => {
                if let LitValue::RegExp { pattern, flags } = &lit.value {
                    let regex = crate::stdlib::regexp::construct_regexp(
                        ctx,
                        &[Value::str(pattern), Value::str(flags)],
                        None,
                    )?;
                    return Ok(Value::Obj(regex));
                }
                literal_value(lit)
            }
            Expr::Identifier(id) => self.scopes.get(&id.name, loc_of(id.span)),
            Expr::This(span) => self.scopes.get("this", loc_of(*span)),
            Expr::Binary(binary) => self.eval_binary(binary).await,
            Expr::Logical(logical) => {
                let left = self.eval(&logical.left).await?;
                match logical.operator {
                    "&&" => {
                        if left.truthy() {
                            self.eval(&logical.right).await
                        } else {
                            Ok(left)
                        }
                    }
                    "||" => {
                        if left.truthy() {
                            Ok(left)
                        } else {
                            self.eval(&logical.right).await
                        }
                    }
                    _ => {
                        if !left.is_nullish() {
                            Ok(left)
                        } else {
                            self.eval(&logical.right).await
                        }
                    }
                }
            }
            Expr::Unary(unary) => self.eval_unary(unary).await,
            Expr::Assignment(assignment) => self.eval_assignment(assignment).await,
            Expr::Sequence(sequence) => {
                let mut result = Value::Undefined;
                for expression in &sequence.expressions {
                    result = self.eval(expression).await?;
                }
                Ok(result)
            }
            Expr::Function(function) => Ok(Value::Obj(self.create_function(function, None))),
            Expr::Chain(chain) => Ok(self.eval_opt(&chain.expression).await?.unwrap_or(Value::Undefined)),
            Expr::Object(object) => self.eval_object(object).await,
            Expr::Array(array) => self.eval_array(array).await,
            Expr::Template(template) => Ok(Value::Str(self.eval_template(template).await?)),
            Expr::TaggedTemplate(tagged) => self.eval_tagged_template(tagged).await,
            Expr::Conditional(conditional) => {
                if self.eval(&conditional.test).await?.truthy() {
                    self.eval(&conditional.consequent).await
                } else {
                    self.eval(&conditional.alternate).await
                }
            }
            Expr::Update(update) => self.eval_update(update).await,
            Expr::Await(node) => {
                // Await always suspends, including for plain values.
                let value = self.eval(&node.argument).await?;
                ctx.await_value(value, Some(loc_of(node.span))).await
            }
            Expr::Yield(node) => self.eval_yield(node).await,
            Expr::New(node) => self.eval_new(node).await,
            Expr::Import(node) => self.eval_import(node).await,
            Expr::Member(_) | Expr::Call(_) => Ok(self.eval_opt(node).await?.unwrap_or(Value::Undefined)),
            other => Err(unsupported_syntax(other.type_name(), loc_of(other.span()))),
        }
    }

    /// ToPrimitive for an operand: data objects run their own methods; opaque values stay.
    pub async fn to_primitive(&self, value: Value, hint: Hint, loc: Loc) -> R<Value> {
        crate::interp::callback::to_primitive(&self.ctx, value, hint).await.map_err(|e| e.at(Some(loc)))
    }

    async fn eval_binary(&self, node: &BinaryExpression) -> R<Value> {
        if let Expr::PrivateIdentifier(private) = &node.left {
            return Err(unsupported_syntax("PrivateIdentifier", loc_of(private.span)));
        }
        let loc = loc_of(node.span);
        let left = self.eval(&node.left).await?;
        let right = self.eval(&node.right).await?;
        if node.operator == "instanceof" {
            return instanceof_value(&left, &right, loc).map(Value::Bool);
        }
        if matches!(left, Value::Obj(_)) || matches!(right, Value::Obj(_)) {
            return self.apply_operator(node.operator, left, right, loc).await;
        }
        self.apply_binary(node.operator, &left, &right, loc)
    }

    // Arithmetic, relational, and bitwise operators convert both operands first, left then right.
    pub async fn apply_operator(&self, operator: &str, lhs: Value, rhs: Value, loc: Loc) -> R<Value> {
        if !(matches!(lhs, Value::Obj(_)) || matches!(rhs, Value::Obj(_))) {
            return self.apply_binary(operator, &lhs, &rhs, loc);
        }
        let equality = operator == "==" || operator == "!=";
        // `in` checks the right operand before ToPropertyKey on the left.
        if operator == "in"
            && let Value::Obj(left) = &lhs
            && !left.is_opaque()
            && is_objectish(&rhs)
        {
            let key = self.to_property_key(lhs.clone(), loc).await?;
            return Ok(Value::Bool(self.has_key(&rhs, &key)?));
        }
        let other = if matches!(lhs, Value::Obj(_)) { &rhs } else { &lhs };
        let converts = primitive_operator(operator) || (equality && !other.is_nullish() && !is_objectish(other));
        if !converts {
            return self.apply_binary(operator, &lhs, &rhs, loc);
        }
        let hint = if operator == "+" || equality { Hint::Default } else { Hint::Number };
        let l = self.to_primitive(lhs, hint, loc).await?;
        let r = self.to_primitive(rhs, hint, loc).await?;
        self.apply_binary(operator, &l, &r, loc)
    }

    pub fn apply_binary(&self, operator: &str, lhs: &Value, rhs: &Value, loc: Loc) -> R<Value> {
        match operator {
            "===" => return Ok(Value::Bool(lhs.strict_eq(rhs))),
            "!==" => return Ok(Value::Bool(!lhs.strict_eq(rhs))),
            "==" => return self.loosely_equal(lhs, rhs, loc).map(Value::Bool),
            "!=" => return self.loosely_equal(lhs, rhs, loc).map(|equal| Value::Bool(!equal)),
            _ => {}
        }
        if operator == "in" && is_objectish(rhs) && !is_opaque(lhs) {
            return Ok(Value::Bool(self.has_key(rhs, &property_key(lhs).to_prop())?));
        }
        if is_opaque(lhs) || is_opaque(rhs) {
            return Err(invalid_data("Binary operators require data values.").at(Some(loc)));
        }
        let number = |value: &Value| prim_number(value);
        Ok(match operator {
            "+" => {
                if matches!(lhs, Value::Str(_)) || matches!(rhs, Value::Str(_)) {
                    let mut buf = JsStrBuf::new();
                    buf.push_js(&prim_string(lhs)?);
                    buf.push_js(&prim_string(rhs)?);
                    check_string_length(buf.len() as f64)?;
                    Value::Str(buf.finish())
                } else {
                    Value::Num(number(lhs)? + number(rhs)?)
                }
            }
            "-" => Value::Num(number(lhs)? - number(rhs)?),
            "*" => Value::Num(number(lhs)? * number(rhs)?),
            "/" => Value::Num(number(lhs)? / number(rhs)?),
            "%" => Value::Num(number(lhs)? % number(rhs)?),
            "**" => Value::Num(js_pow(number(lhs)?, number(rhs)?)),
            "<" | "<=" | ">" | ">=" => {
                if let (Value::Str(a), Value::Str(b)) = (lhs, rhs) {
                    Value::Bool(match operator {
                        "<" => a < b,
                        "<=" => a <= b,
                        ">" => a > b,
                        _ => a >= b,
                    })
                } else {
                    let (a, b) = (number(lhs)?, number(rhs)?);
                    Value::Bool(match operator {
                        "<" => a < b,
                        "<=" => a <= b,
                        ">" => a > b,
                        _ => a >= b,
                    })
                }
            }
            "&" => Value::Num((crate::numconv::to_int32(number(lhs)?) & crate::numconv::to_int32(number(rhs)?)) as f64),
            "|" => Value::Num((crate::numconv::to_int32(number(lhs)?) | crate::numconv::to_int32(number(rhs)?)) as f64),
            "^" => Value::Num((crate::numconv::to_int32(number(lhs)?) ^ crate::numconv::to_int32(number(rhs)?)) as f64),
            "<<" => {
                let shift = crate::numconv::to_uint32(number(rhs)?) & 31;
                Value::Num(crate::numconv::to_int32(number(lhs)?).wrapping_shl(shift) as f64)
            }
            ">>" => {
                let shift = crate::numconv::to_uint32(number(rhs)?) & 31;
                Value::Num((crate::numconv::to_int32(number(lhs)?) >> shift) as f64)
            }
            ">>>" => {
                let shift = crate::numconv::to_uint32(number(rhs)?) & 31;
                Value::Num((crate::numconv::to_uint32(number(lhs)?) >> shift) as f64)
            }
            "in" => {
                return Err(type_error("The 'in' operator requires a data object on the right-hand side.").at(Some(loc)));
            }
            other => return Err(type_error(format!("Unsupported binary operator '{other}'.")).at(Some(loc))),
        })
    }

    // A tool reference has the names Object.keys lists for it.
    fn has_key(&self, target: &Value, key: &PropKey) -> R<bool> {
        match target {
            Value::Obj(obj) => Ok(has(obj, key)),
            Value::Tool(tool) => {
                if key.is_symbol() {
                    return Ok(false);
                }
                let name = key.to_js();
                Ok(self.ctx.tools.keys(tool.path())?.contains(&name))
            }
            _ => Ok(false),
        }
    }

    // IsLooselyEqual: objects (including functions and tool references) compare by identity.
    fn loosely_equal(&self, lhs: &Value, rhs: &Value, loc: Loc) -> R<bool> {
        let (lo, ro) = (is_objectish(lhs), is_objectish(rhs));
        if lo == ro {
            return Ok(if lo { lhs.strict_eq(rhs) } else { loose_eq_primitive(lhs, rhs) });
        }
        let primitive = if lo { rhs } else { lhs };
        if primitive.is_nullish() {
            return Ok(false);
        }
        Err(invalid_data("Binary operators require data values.").at(Some(loc)))
    }

    async fn eval_unary(&self, node: &UnaryExpression) -> R<Value> {
        let operator = node.operator;
        let loc = loc_of(node.span);
        if operator == "delete" {
            return self.eval_delete(&node.argument).await.map(Value::Bool);
        }
        // Undeclared names short-circuit, but declared TDZ bindings must still throw.
        if operator == "typeof"
            && let Expr::Identifier(id) = &node.argument
            && !self.scopes.resolves(&id.name)
        {
            return Ok(Value::str("undefined"));
        }
        let value = self.eval(&node.argument).await?;
        match operator {
            "typeof" => return Ok(Value::str(self.typeof_operand(&value))),
            "!" => return Ok(Value::Bool(!value.truthy())),
            "void" => return Ok(Value::Undefined),
            _ => {}
        }
        let operand = self.to_primitive(value, Hint::Number, loc).await?;
        if is_opaque(&operand) {
            return Err(invalid_data("Unary operators require data values.").at(Some(loc)));
        }
        let number = prim_number(&operand)?;
        Ok(Value::Num(match operator {
            "+" => number,
            "-" => -number,
            "~" => !crate::numconv::to_int32(number) as f64,
            other => return Err(type_error(format!("Unsupported unary operator '{other}'.")).at(Some(loc))),
        }))
    }

    /// The `typeof` operator: a namespace only holds tools, so it is an object.
    pub fn typeof_operand(&self, value: &Value) -> &'static str {
        if let Value::Tool(tool) = value
            && self.ctx.tools.is_namespace(tool.path())
        {
            return "object";
        }
        typeof_value(value)
    }

    async fn eval_assignment(&self, node: &AssignmentExpression) -> R<Value> {
        let operator = node.operator;
        let loc = loc_of(node.span);
        if let Pat::Identifier(left) = &node.left {
            let name = &left.name;
            let left_loc = loc_of(left.span);
            if operator == "=" {
                let right = self.eval_named(&node.right, &left.key).await?;
                return self.scopes.set(name, right, left_loc);
            }
            if is_compound(operator) {
                let current = self.scopes.get(name, left_loc)?;
                let right = self.eval(&node.right).await?;
                let binary = &operator[..operator.len() - 1];
                let next = if matches!(current, Value::Obj(_)) || matches!(right, Value::Obj(_)) {
                    self.apply_operator(binary, current, right, loc).await?
                } else {
                    self.apply_binary(binary, &current, &right, loc)?
                };
                return self.scopes.set(name, next, left_loc);
            }
        }
        if matches!(operator, "??=" | "||=" | "&&=") {
            return self.eval_logical_assignment(node).await;
        }
        if operator != "=" && !is_compound(operator) {
            return Err(type_error(format!("Unsupported assignment operator '{operator}'.")).at(Some(loc)));
        }
        if operator == "=" && matches!(node.left, Pat::Object(_) | Pat::Array(_)) {
            let value = self.eval(&node.right).await?;
            self.assign_pattern(&node.left, value.clone(), loc).await?;
            return Ok(value);
        }
        if let Pat::Member(member) = &node.left {
            let binary = if operator == "=" { "" } else { &operator[..operator.len() - 1] };
            return self
                .modify_member(member, |frame, current| {
                    Box::pin(async move {
                        let right = frame.eval(&node.right).await?;
                        if operator == "=" {
                            return Ok((true, right.clone(), right));
                        }
                        let next = frame.apply_operator(binary, current, right, loc).await?;
                        Ok((true, next.clone(), next))
                    })
                })
                .await;
        }
        Err(type_error("Assignment target must be an Identifier or MemberExpression.").at(Some(loc_of(node.left.span()))))
    }

    async fn eval_logical_assignment(&self, node: &AssignmentExpression) -> R<Value> {
        let operator = node.operator;
        let should_assign = move |current: &Value| match operator {
            "??=" => current.is_nullish(),
            "||=" => !current.truthy(),
            _ => current.truthy(),
        };
        match &node.left {
            Pat::Identifier(left) => {
                let current = self.scopes.get(&left.name, loc_of(left.span))?;
                if !should_assign(&current) {
                    return Ok(current);
                }
                let right = self.eval_named(&node.right, &left.key).await?;
                self.scopes.set(&left.name, right, loc_of(left.span))
            }
            Pat::Member(member) => {
                self.modify_member(member, |frame, current| {
                    Box::pin(async move {
                        if !should_assign(&current) {
                            return Ok((false, current.clone(), current));
                        }
                        let right = frame.eval(&node.right).await?;
                        Ok((true, right.clone(), right))
                    })
                })
                .await
            }
            other => Err(type_error("Assignment target must be an Identifier or MemberExpression.").at(Some(loc_of(other.span())))),
        }
    }

    async fn eval_update(&self, node: &UpdateExpression) -> R<Value> {
        let operator = node.operator;
        let increment = if operator == "++" { 1.0 } else { -1.0 };
        let argument_loc = loc_of(node.argument.span());
        // CodeMode numeric coercion, so opaque runtime references reject clearly.
        let operand = |current: &Value| -> R<f64> {
            if is_opaque(current) {
                return Err(invalid_data(format!("'{operator}' requires a data value.")).at(Some(argument_loc)));
            }
            prim_number(current)
        };
        match &node.argument {
            Expr::Identifier(id) => {
                let current = self.scopes.get(&id.name, loc_of(id.span))?;
                let current =
                    if matches!(current, Value::Obj(_)) { self.to_primitive(current, Hint::Number, argument_loc).await? } else { current };
                let before = operand(&current)?;
                let next = before + increment;
                self.scopes.set(&id.name, Value::Num(next), loc_of(id.span))?;
                Ok(Value::Num(if node.prefix { next } else { before }))
            }
            Expr::Member(member) => {
                let prefix = node.prefix;
                self.modify_member(member, |frame, current| {
                    Box::pin(async move {
                        let primitive = frame.to_primitive(current, Hint::Number, argument_loc).await?;
                        let value = operand(&primitive)?;
                        let next = value + increment;
                        Ok((true, Value::Num(next), Value::Num(if prefix { next } else { value })))
                    })
                })
                .await
            }
            _ => Err(type_error("Update target must be an Identifier or MemberExpression.").at(Some(argument_loc))),
        }
    }

    // A module that fails to load rejects the promise, as a failed dynamic import does.
    async fn eval_import(&self, node: &ImportExpression) -> R<Value> {
        let ctx = &self.ctx;
        let loc = Some(loc_of(node.span));
        let specifier = to_string_plain(&self.eval(&node.source).await?);
        if let Some(options) = &node.options {
            self.eval(options).await?;
        }
        let made = crate::interp::promises::capability(ctx, loc)?;
        match ctx.namespace(&specifier, loc) {
            Ok(namespace) => made.resolve(ctx, Value::Obj(namespace)),
            Err(error @ Fail::Pending(_)) => made.reject(ctx, error),
            Err(other) => return Err(other),
        }
        Ok(Value::Obj(made.promise().clone()))
    }

    // EvaluateCall: a member callee supplies its base object as `this`; anything else calls with undefined.
    async fn eval_call(&self, node: &CallExpression) -> R<Option<Value>> {
        let loc = loc_of(node.span);
        let (callable, this) = match &node.callee {
            Expr::Super(span) => return Err(unsupported_syntax("Super", loc_of(*span))),
            Expr::Member(member) => match self.read_method(member).await? {
                None => return Ok(None),
                Some(pair) => pair,
            },
            other => match self.eval_opt(other).await? {
                None => return Ok(None),
                Some(value) => (value, Value::Undefined),
            },
        };
        if callable.is_nullish() && node.optional {
            return Ok(None);
        }
        let args = self.eval_arguments(&node.arguments).await?;
        let work = dispatch(&self.ctx, callable, this, args, Some(loc), Some(&node.callee))?;
        Ok(Some(work.await?))
    }

    async fn read_method(&self, node: &MemberExpression) -> R<Option<(Value, Value)>> {
        Ok(match self.member_reference_of(node).await? {
            Reference::Short => None,
            Reference::Tool(tool) => Some((Value::Tool(tool), Value::Undefined)),
            Reference::Value(value) => Some((value, Value::Undefined)),
            Reference::Member { target, key, receiver } => {
                Some((self.read_reference(&target, &key, &receiver, node)?, receiver))
            }
        })
    }

    pub async fn eval_arguments(&self, nodes: &[ExprOrSpread]) -> R<Vec<Value>> {
        let ctx = &self.ctx;
        let mut args = Vec::with_capacity(nodes.len());
        for node in nodes {
            match node {
                ExprOrSpread::Spread(spread) => {
                    let value = self.eval(&spread.argument).await?;
                    let loc = Some(loc_of(spread.span));
                    let Some(cursor) = ctx.iterate(&value, loc).await? else {
                        return Err(type_error("Spread arguments require a synchronous iterable.").at(loc));
                    };
                    loop {
                        let step = cursor.next_step(ctx).await?;
                        if step.done {
                            break;
                        }
                        args.push(step.value);
                    }
                }
                ExprOrSpread::Expr(expr) => args.push(self.eval(expr).await?),
            }
        }
        Ok(args)
    }

    async fn eval_new(&self, node: &NewExpression) -> R<Value> {
        let loc = loc_of(node.span);
        let callee = self.eval(&node.callee).await?;
        let construct = match &callee {
            Value::Obj(obj) => obj.native_fn().and_then(|native| native.construct.borrow().clone()),
            _ => None,
        };
        let Some(construct) = construct else {
            // `new` itself is supported, so a non-constructible callee is a TypeError like JS.
            let name = callee_description(Some(&node.callee)).unwrap_or_else(|| "The called value".into());
            let message = match &callee {
                Value::Obj(obj) if obj.program_fn().is_some() => format!(
                    "{name} cannot be constructed: user-defined constructors and classes are not supported. Call it as a function that returns a plain object instead."
                ),
                Value::Obj(obj) if obj.native_fn().is_some() => {
                    format!("new {name}(...) is not supported; call {name}(...) without new instead.")
                }
                _ => format!("{name} is not a constructor."),
            };
            return Err(type_error(message).at(Some(loc)));
        };
        let args = self.eval_arguments(&node.arguments).await?;
        let Value::Obj(target) = callee else { unreachable!() };
        native_result(construct(&self.ctx, args, target, Some(loc)), Some(loc)).await
    }

    async fn eval_yield(&self, node: &YieldExpression) -> R<Value> {
        let ctx = &self.ctx;
        let loc = Some(loc_of(node.span));
        if !self.generator.get() {
            return Err(type_error("yield is only valid inside a generator.").at(loc));
        }
        let value = match &node.argument {
            Some(argument) => self.eval(argument).await?,
            None => Value::Undefined,
        };
        if node.delegate {
            return self.delegate_yield(value, loc).await;
        }
        let value = if self.generator_async.get() { ctx.await_value(value, loc).await? } else { value };
        self.suspend_generator(value).await
    }

    // Yield `value` and continue with the request that resumes the body.
    async fn suspend_generator(&self, value: Value) -> R<Value> {
        let request = yield_leaf(&self.ctx, value).await?;
        match request.kind {
            GenRequestKind::Next => Ok(request.value),
            GenRequestKind::Throw => Err(Fail::Throw(request.value)),
            GenRequestKind::Return => {
                let value = if self.generator_async.get() {
                    self.ctx.await_value(request.value, None).await?
                } else {
                    request.value
                };
                Err(Fail::GenReturn(value))
            }
        }
    }

    async fn delegate_yield(&self, value: Value, loc: Option<Loc>) -> R<Value> {
        let ctx = &self.ctx;
        let builtin = ctx.builtin_cursor(&value);
        if let Some(cursor) = &builtin
            && !self.generator_async.get()
        {
            loop {
                let step = cursor.next_step(ctx).await?;
                if step.done {
                    return Ok(Value::Undefined);
                }
                match self.suspend_generator(step.value).await {
                    Ok(_) => {}
                    Err(error @ Fail::GenReturn(_)) => {
                        cursor.close_now(ctx).await?;
                        return Err(error);
                    }
                    Err(Fail::Throw(_)) => {
                        cursor.close_now(ctx).await?;
                        return Err(type_error("The delegated iterator does not provide a throw() method.").at(loc));
                    }
                    Err(other) => return Err(other),
                }
            }
        }
        let custom = match &builtin {
            None => ctx.custom_iterator(&value, loc, self.generator_async.get()).await?,
            Some(_) => None,
        };
        if builtin.is_none() && custom.is_none() {
            return Err(type_error("yield* requires a compatible iterable value.").at(loc));
        }
        // An async generator delegates to a synchronous iterator through CreateAsyncFromSyncIterator.
        let iterator = if self.generator_async.get() && !custom.as_ref().is_some_and(|c| c.asynchronous) {
            crate::interp::interp::async_from_sync(ctx, builtin, custom, loc)
        } else {
            custom.unwrap()
        };
        let mut kind = GenRequestKind::Next;
        let mut input = Value::Undefined;
        loop {
            let method = match kind {
                GenRequestKind::Next => iterator.next.clone(),
                _ => crate::interp::object::get(ctx, &iterator.iterator, &PropKey::from_str(kind.as_str()))?,
            };
            if method.is_nullish() {
                if matches!(kind, GenRequestKind::Return) {
                    let value = if self.generator_async.get() { ctx.await_value(input, loc).await? } else { input };
                    return Err(Fail::GenReturn(value));
                }
                crate::interp::cursor::close_iterator(ctx, &iterator.iterator, iterator.asynchronous, loc).await?;
                return Err(type_error("The delegated iterator does not provide a throw() method.").at(loc));
            }
            let method = crate::interp::cursor::require_iterator_method(method, &format!("Iterator {}", kind.as_str()), loc)?;
            let called = ctx.call(method, Value::Obj(iterator.iterator.clone()), vec![input.clone()], loc).await?;
            let result = if iterator.asynchronous { ctx.await_value(called, loc).await? } else { called };
            let result =
                crate::interp::cursor::require_iterator_object(result, &format!("Iterator {}() result", kind.as_str()), loc)?;
            let done = crate::interp::object::get(ctx, &result, &PropKey::from_str("done"))?.truthy();
            let result_value = crate::interp::object::get(ctx, &result, &PropKey::from_str("value"))?;
            if done {
                if matches!(kind, GenRequestKind::Return) {
                    return Err(Fail::GenReturn(result_value));
                }
                return Ok(result_value);
            }
            // Delegated values are yielded as they are; an async generator's inner promise already settled them.
            match self.suspend_generator(result_value).await {
                Ok(next) => {
                    input = next;
                    kind = GenRequestKind::Next;
                }
                Err(Fail::GenReturn(value)) => {
                    kind = GenRequestKind::Return;
                    input = value;
                }
                Err(Fail::Throw(value)) => {
                    kind = GenRequestKind::Throw;
                    input = value;
                }
                Err(other) => return Err(other),
            }
        }
    }

    async fn eval_object(&self, node: &ObjectExpression) -> R<Value> {
        let ctx = &self.ctx;
        let object = ctx.new_plain();
        for property in &node.properties {
            match property {
                ObjectMember::Spread(spread) => {
                    let value = self.eval(&spread.argument).await?;
                    if value.is_nullish() {
                        continue;
                    }
                    let source =
                        crate::stdlib::object::enumerable_source(ctx, "Object spread", &value, Some(loc_of(spread.span)))?;
                    crate::interp::object::assign(ctx, &object, &source, None)?;
                }
                ObjectMember::Property(prop) => {
                    if prop.kind != PropKind::Init {
                        return Err(type_error("Only init object properties are supported.").at(Some(loc_of(prop.span))));
                    }
                    let key = if prop.computed {
                        let value = self.eval(&prop.key).await?;
                        self.to_property_key(value, loc_of(prop.key.span())).await?
                    } else {
                        match &prop.key {
                            Expr::Identifier(id) => PropKey::from_js(id.key.clone()),
                            Expr::Literal(lit) => property_key(&literal_value(lit)?).to_prop(),
                            other => {
                                return Err(type_error("Unsupported object property key shape.").at(Some(loc_of(other.span()))));
                            }
                        }
                    };
                    let name = match &key {
                        PropKey::Sym(Sym::Iterator) => JsStr::from("[Symbol.iterator]"),
                        PropKey::Sym(Sym::AsyncIterator) => JsStr::from("[Symbol.asyncIterator]"),
                        other => other.to_js(),
                    };
                    let value = self.eval_named(&prop.value, &name).await?;
                    define(&object, key, value, DATA);
                }
            }
        }
        Ok(Value::Obj(object))
    }

    async fn eval_array(&self, node: &ArrayExpression) -> R<Value> {
        let ctx = &self.ctx;
        let mut values: Vec<Option<Value>> = Vec::with_capacity(node.elements.len());
        for element in &node.elements {
            match element {
                // A literal elision is a real hole.
                None => values.push(None),
                Some(ExprOrSpread::Spread(spread)) => {
                    let value = self.eval(&spread.argument).await?;
                    let loc = Some(loc_of(spread.span));
                    let Some(cursor) = ctx.iterate(&value, loc).await? else {
                        return Err(type_error("Array spread requires a synchronous iterable.").at(loc));
                    };
                    loop {
                        let step = cursor.next_step(ctx).await?;
                        if step.done {
                            break;
                        }
                        values.push(Some(step.value));
                    }
                }
                Some(ExprOrSpread::Expr(expr)) => values.push(Some(self.eval(expr).await?)),
            }
        }
        Ok(Value::Obj(ctx.new_array_holes(values)))
    }

    async fn eval_template(&self, node: &TemplateLiteral) -> R<JsStr> {
        let mut output = JsStrBuf::new();
        for (index, quasi) in node.quasis.iter().enumerate() {
            // acorn only omits `cooked` for invalid escapes, which are a parse error outside a tagged template.
            let Some(cooked) = &quasi.cooked else {
                return Err(type_error("Invalid template literal quasi.").at(Some(loc_of(quasi.span))));
            };
            output.push_js(cooked);
            check_string_length(output.len() as f64)?;
            if let Some(expression) = node.expressions.get(index) {
                let raw = self.eval(expression).await?;
                let primitive = self.to_primitive(raw, Hint::String, loc_of(expression.span())).await?;
                output.push_js(&prim_string(&primitive)?);
                check_string_length(output.len() as f64)?;
            }
        }
        Ok(output.finish())
    }

    // `tag\`a${x}b\`` is `tag(strings, x)`: the tag is read like a callee.
    async fn eval_tagged_template(&self, node: &TaggedTemplateExpression) -> R<Value> {
        let ctx = &self.ctx;
        let (callable, this) = match &node.tag {
            Expr::Member(member) => match self.read_method(member).await? {
                None => (Value::Undefined, Value::Undefined),
                Some(pair) => pair,
            },
            other => (self.eval(other).await?, Value::Undefined),
        };
        let site = node as *const TaggedTemplateExpression as usize;
        let known = ctx.templates.borrow().get(&site).cloned();
        let strings = match known {
            Some(strings) => strings,
            None => {
                let cooked: Vec<Value> = node
                    .quasi
                    .quasis
                    .iter()
                    .map(|quasi| quasi.cooked.clone().map(Value::Str).unwrap_or(Value::Undefined))
                    .collect();
                let strings = ctx.new_array(cooked);
                let raw: Vec<Value> = node.quasi.quasis.iter().map(|quasi| Value::Str(quasi.raw.clone())).collect();
                define(&strings, PropKey::from_str("raw"), Value::Obj(ctx.new_array(raw)), FROZEN);
                ctx.templates.borrow_mut().insert(site, strings.clone());
                strings
            }
        };
        let mut args = vec![Value::Obj(strings)];
        for expression in &node.quasi.expressions {
            args.push(self.eval(expression).await?);
        }
        let loc = loc_of(node.span);
        dispatch(ctx, callable, this, args, Some(loc), Some(&node.tag))?.await
    }

    async fn eval_member(&self, node: &MemberExpression) -> R<Option<Value>> {
        Ok(match self.member_reference_of(node).await? {
            Reference::Short => None,
            Reference::Tool(tool) => Some(Value::Tool(tool)),
            Reference::Value(value) => Some(value),
            Reference::Member { target, key, receiver } => Some(self.read_reference(&target, &key, &receiver, node)?),
        })
    }

    pub async fn member_reference_of(&self, node: &MemberExpression) -> R<Reference> {
        if let Expr::Super(span) = &node.object {
            return Err(unsupported_syntax("Super", loc_of(*span)));
        }
        if let Expr::PrivateIdentifier(private) = &node.property {
            return Err(unsupported_syntax("PrivateIdentifier", loc_of(private.span)));
        }
        let Some(object) = self.eval_opt(&node.object).await? else { return Ok(Reference::Short) };
        if object.is_nullish() && node.optional {
            return Ok(Reference::Short);
        }
        let key = match (&node.property, node.computed) {
            (Expr::Identifier(id), false) => Value::Str(id.key.clone()),
            (property, _) => self.eval(property).await?,
        };
        self.member_reference(node, object, key).await
    }

    // GetValue applies ToObject to the base before ToPropertyKey, so a nullish base throws before the key's own
    // toString runs.
    async fn member_reference(&self, node: &MemberExpression, object: Value, key: Value) -> R<Reference> {
        let object_loc = loc_of(node.object.span());
        let property_loc = loc_of(node.property.span());
        if object.is_nullish() {
            let mut message = JsStrBuf::new();
            message.push_str(&format!("Cannot read properties of {} (reading '", if object.is_undefined() { "undefined" } else { "null" }));
            message.push_js(&to_string_plain(&key));
            message.push_str("').");
            return Err(type_error(message.finish()).at(Some(object_loc)));
        }
        let raw = if let Value::Obj(_) = key {
            let primitive = self.to_primitive(key, Hint::String, property_loc).await?;
            property_key(&primitive)
        } else {
            property_key(&key)
        };
        self.resolve_raw(&object, raw, object_loc, property_loc)
    }

    pub fn resolve_property(&self, object: &Value, key: PropKey, object_loc: Loc, property_loc: Loc) -> R<Reference> {
        let raw = match key {
            PropKey::Index(index) => RawKey::Num(index as f64),
            PropKey::Str(text) => RawKey::Str(text),
            PropKey::Sym(sym) => RawKey::Sym(sym),
        };
        // A destructuring key is a string; an index key from a pattern is a string too.
        let raw = match raw {
            RawKey::Num(n) => RawKey::Str(JsStr::from(crate::numconv::to_string(n))),
            other => other,
        };
        self.resolve_raw(object, raw, object_loc, property_loc)
    }

    fn resolve_raw(&self, object: &Value, key: RawKey, object_loc: Loc, property_loc: Loc) -> R<Reference> {
        let ctx = &self.ctx;
        match object {
            Value::Tool(tool) => {
                let RawKey::Str(name) = key else {
                    return Err(type_error("Tool paths must use string property names.").at(Some(property_loc)));
                };
                Ok(Reference::Tool(tool.child(&name)))
            }
            Value::Obj(obj) => Ok(Reference::Member { target: obj.clone(), key: key.to_prop(), receiver: object.clone() }),
            _ => {
                // Strings own length and indexes; every other primitive property reads through the wrapper prototype.
                let prop = key.to_prop();
                if let Value::Str(text) = object {
                    if prop.eq_str("length") {
                        return Ok(Reference::Value(Value::Num(text.len() as f64)));
                    }
                    if let PropKey::Index(index) = prop {
                        return Ok(Reference::Value(match text.units().get(index as usize) {
                            Some(unit) => Value::Str(JsStr::from_slice(&[*unit])),
                            None => Value::Undefined,
                        }));
                    }
                }
                if let Some(proto) = ctx.primitive_prototype(object) {
                    return Ok(Reference::Member { target: proto, key: prop, receiver: object.clone() });
                }
                if object.is_nullish() {
                    let mut message = JsStrBuf::new();
                    message.push_str(&format!(
                        "Cannot read properties of {} (reading '",
                        if object.is_undefined() { "undefined" } else { "null" }
                    ));
                    message.push_js(&key.display());
                    message.push_str("').");
                    return Err(type_error(message.finish()).at(Some(object_loc)));
                }
                Err(type_error("Cannot access a property on a non-object value.").at(Some(object_loc)))
            }
        }
    }

    pub fn read_reference(&self, target: &Obj, key: &PropKey, receiver: &Value, node: &MemberExpression) -> R<Value> {
        // Reject unknown promise properties so a missing await cannot hide.
        if target.is_promise() && !has(target, key) {
            return Err(invalid_data(
                "This value is an un-awaited Promise; await it first - e.g. `const result = await tools.ns.tool(...)`.",
            )
            .at(Some(loc_of(node.object.span()))));
        }
        self.read_property(target, key, loc_of(node.span), receiver)
    }

    pub async fn write_member(&self, node: &MemberExpression, value: Value) -> R<Value> {
        self.modify_member(node, move |_, _| {
            let value = value.clone();
            Box::pin(async move { Ok((true, value.clone(), value)) })
        })
        .await
    }

    async fn eval_delete(&self, argument: &Expr) -> R<bool> {
        let target = match argument {
            Expr::Chain(chain) => &chain.expression,
            other => other,
        };
        if let Expr::Identifier(_) = target {
            return Err(type_error("Only data fields may be deleted.").at(Some(loc_of(argument.span()))));
        }
        // `delete <non-reference>` evaluates the operand and is true, as in JS.
        let Expr::Member(member) = target else {
            self.eval(target).await?;
            return Ok(true);
        };
        let loc = loc_of(member.span);
        match self.member_reference_of(member).await? {
            Reference::Short => Ok(true),
            Reference::Member { target: obj, key, receiver } if matches!(&receiver, Value::Obj(r) if r.ptr_eq(&obj)) => {
                if remove(&obj, &key) {
                    return Ok(true);
                }
                let mut message = JsStrBuf::new();
                message.push_str("Cannot delete property '");
                message.push_js(&key.to_js());
                message.push_str("'.");
                Err(type_error(message.finish()).at(Some(loc)))
            }
            _ => Err(invalid_data("Only data fields may be deleted.").at(Some(loc))),
        }
    }

    // Resolve side-effecting object and key expressions exactly once.
    async fn modify_member<'a, F>(&'a self, node: &'a MemberExpression, compute: F) -> R<Value>
    where
        F: FnOnce(&'a Frame, Value) -> LFut<'a, R<(bool, Value, Value)>>,
    {
        let loc = loc_of(node.span);
        let reference = self.member_reference_of(node).await?;
        let Reference::Member { target, key, receiver } = reference else {
            return Err(type_error("Only data fields may be assigned.").at(Some(loc)));
        };
        if !matches!(&receiver, Value::Obj(r) if r.ptr_eq(&target)) {
            let mut message = JsStrBuf::new();
            message.push_str("Cannot create property '");
            message.push_js(&key.to_js());
            message.push_str(&format!("' on {} '", primitive_typeof(&receiver)));
            message.push_js(&to_string_plain(&receiver));
            message.push_str("'.");
            return Err(type_error(message.finish()).at(Some(loc)));
        }
        let current = self.read_reference(&target, &key, &receiver, node)?;
        let (write, next, result) = compute(self, current).await?;
        if write {
            self.assign_to_reference(&target, &key, next, loc)?;
        }
        Ok(result)
    }

    fn assign_to_reference(&self, target: &Obj, key: &PropKey, next: Value, loc: Loc) -> R<()> {
        let label = if target.is_array() { "Array assignment result" } else { "Object assignment result" };
        let written = reject_circular_insertion(target, &next, label)
            .and_then(|_| set(&self.ctx, target, key, next))
            .map_err(|e| e.at(Some(loc)))?;
        if written {
            return Ok(());
        }
        let mut message = JsStrBuf::new();
        message.push_str("Cannot assign to read only property '");
        message.push_js(&key.to_js());
        message.push_str("'.");
        Err(type_error(message.finish()).at(Some(loc)))
    }

    // ToPropertyKey: a data object converts through its own `toString`/`valueOf` first.
    pub async fn to_property_key(&self, value: Value, loc: Loc) -> R<PropKey> {
        if !matches!(value, Value::Obj(_)) {
            return Ok(property_key(&value).to_prop());
        }
        let primitive = self.to_primitive(value, Hint::String, loc).await?;
        Ok(property_key(&primitive).to_prop())
    }
}

fn primitive_typeof(value: &Value) -> &'static str {
    match value {
        Value::Str(_) => "string",
        Value::Num(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Sym(_) => "symbol",
        Value::Undefined => "undefined",
        _ => "object",
    }
}

/// Finishes a built-in's result, locating a failure at the call that started it.
pub async fn native_result(result: R<NOut>, loc: Option<Loc>) -> R<Value> {
    match result {
        Err(error) => Err(error.at(loc)),
        Ok(NOut::Val(value)) => Ok(value),
        Ok(NOut::Work(work)) => work.await.map_err(|e| e.at(loc)),
    }
}

/// The single dispatch for every invocation: call expressions and callbacks share it.
pub fn dispatch(
    ctx: &Ctx,
    callable: Value,
    this: Value,
    args: Vec<Value>,
    site: Option<Loc>,
    callee: Option<&Expr>,
) -> R<Fut<R<Value>>> {
    let callee_loc = callee.map(|c| loc_of(c.span())).or(site);
    match &callable {
        Value::Obj(obj) => match obj.callable() {
            Some(Callable::Program(_)) => crate::interp::eval::invoke_function(ctx, obj, this, args, site),
            Some(Callable::Native(native)) => {
                let call = native.call.borrow().clone();
                let ctx = ctx.clone();
                // A loop made only of built-in calls has no other step to count, so count the call.
                let slice = ctx.runner.tick();
                if !slice {
                    ctx.runner.check_native_depth().map_err(|e| e.at(site))?;
                    let result = call(&ctx, this, args, site);
                    return Ok(match result {
                        Err(error) => return Err(error.at(site)),
                        Ok(NOut::Val(value)) => boxed(async move { Ok(value) }),
                        Ok(NOut::Work(work)) => {
                            let work = crate::interp::runner::native_work(&ctx, work);
                            boxed(async move { work.await.map_err(|e| e.at(site)) })
                        }
                    });
                }
                Ok(boxed(async move {
                    crate::interp::runner::slice(&ctx).await;
                    ctx.runner.check_native_depth().map_err(|e| e.at(site))?;
                    match call(&ctx, this, args, site) {
                        Err(error) => Err(error.at(site)),
                        Ok(NOut::Val(value)) => Ok(value),
                        Ok(NOut::Work(work)) => crate::interp::runner::native_work(&ctx, work).await.map_err(|e| e.at(site)),
                    }
                }))
            }
            None => Err(not_a_function(callee, callee_loc)),
        },
        Value::Tool(tool) => {
            if tool.path().is_empty() {
                return Err(type_error("The tools root is not callable.").at(callee_loc));
            }
            create_tool_call_promise(ctx, tool.path().to_vec(), args, site)
        }
        _ => Err(not_a_function(callee, callee_loc)),
    }
}

fn not_a_function(callee: Option<&Expr>, loc: Option<Loc>) -> Fail {
    let name = callee_description(callee).unwrap_or_else(|| "The called value".into());
    type_error(format!("{name} is not a function.")).at(loc)
}

// The call returns its promise at once. Arguments cross on the caller's stack, and a promise argument is awaited
// first; the host call starts once they have crossed, so admission and hooks happen at the call site.
fn create_tool_call_promise(ctx: &Ctx, path: Vec<JsStr>, args: Vec<Value>, site: Option<Loc>) -> R<Fut<R<Value>>> {
    let runner = &ctx.runner;
    let promise = runner.new_promise(ctx)?;
    let resolving = runner.resolving(&promise, None);
    let body = {
        let (ctx, resolving) = (ctx.clone(), resolving.clone());
        async move {
            let mut json = Vec::with_capacity(args.len());
            for arg in &args {
                json.push(crate::interp::data::to_boundary(&ctx, arg.clone()).await?);
            }
            let work = ctx.tools.execute(&ctx, path, json);
            ctx.runner.host(&ctx, work, move |ctx, settled| match settled {
                Ok(value) => resolving.resolve(ctx, value),
                Err(error) => resolving.reject(ctx, error),
            });
            Ok(Value::Undefined)
        }
    };
    let boundary = Boundary::Async {
        settle: Box::new(move |ctx, completion| {
            if let Err(error) = completion {
                resolving.reject(ctx, error);
            }
        }),
        result: Value::Obj(promise),
    };
    let body = activation(ctx, boxed(body));
    let ctx = ctx.clone();
    Ok(boxed(async move { drive_inline(&ctx, body, boundary, None).await }))
}
