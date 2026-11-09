//! One activation (interpreter.ts `Frame`): the program body or a single function call, evaluating statements
//! against its own scope chain. Expressions are in `expr.rs`.

use crate::interp::callback::preserve_consumer_error;
use crate::interp::coerce::describe_value;
use crate::interp::cursor::{Cursor, Step};
use crate::interp::errors::{Fail, invalid_data, materialize, syntax_error, type_error, unsupported_syntax};
use crate::interp::object::{define, get, has_own, get_own, keys};
use crate::interp::runner::{Boundary, activation, drive_inline};
use crate::interp::scope::{Binding, Scope, ScopeStack};
use crate::interp::value::*;
use crate::interp::{Ctx, Fut, Loc, R, boxed};
use crate::jsstr::JsStr;
use crate::parser::ast::*;
use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

pub type LFut<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

#[derive(Clone)]
pub enum Completion {
    Normal,
    Return(Value),
    Break(Option<Rc<str>>),
    Continue(Option<Rc<str>>),
}

impl Completion {
    fn is_abrupt(&self) -> bool {
        !matches!(self, Completion::Normal)
    }
}

pub struct Frame {
    pub ctx: Ctx,
    pub scopes: ScopeStack,
    /// The body of a generator function, where `yield` suspends.
    pub generator: Cell<bool>,
    pub generator_async: Cell<bool>,
}

pub fn loc_of(span: Span) -> Loc {
    Loc::of(span)
}

// What a loop does with its body's result: exit with a completion, or None to keep iterating.
fn loop_exit(result: Completion, labels: Option<&[Rc<str>]>) -> Option<Completion> {
    let carries = |label: &Rc<str>| labels.is_some_and(|labels| labels.iter().any(|l| l == label));
    match result {
        Completion::Return(_) => Some(result),
        Completion::Break(Some(ref label)) if !carries(label) => Some(result),
        Completion::Break(_) => Some(Completion::Normal),
        Completion::Continue(Some(ref label)) if !carries(label) => Some(result),
        _ => None,
    }
}

pub fn collect_pattern_names(pattern: &Pat, out: &mut Vec<Rc<str>>) {
    match pattern {
        Pat::Identifier(id) => out.push(id.name.clone()),
        Pat::Assignment(node) => collect_pattern_names(&node.left, out),
        Pat::Rest(node) => collect_pattern_names(&node.argument, out),
        Pat::Array(node) => {
            for element in node.elements.iter().flatten() {
                collect_pattern_names(element, out);
            }
        }
        Pat::Object(node) => {
            for property in &node.properties {
                match property {
                    ObjectPatternMember::Rest(rest) => collect_pattern_names(&rest.argument, out),
                    ObjectPatternMember::Property(prop) => collect_pattern_names(&prop.value, out),
                }
            }
        }
        Pat::Member(_) => {}
    }
}

fn pattern_names(pattern: &Pat) -> Vec<Rc<str>> {
    let mut out = Vec::new();
    collect_pattern_names(pattern, &mut out);
    out
}

// `var` names declared anywhere in a function body except inside nested functions.
fn collect_var_names(node: &Stmt, out: &mut Vec<Rc<str>>) {
    match node {
        Stmt::Variable(decl) => {
            if decl.kind == VarKind::Var {
                for declarator in &decl.declarations {
                    collect_pattern_names(&declarator.id, out);
                }
            }
        }
        Stmt::Block(block) => block.body.iter().for_each(|s| collect_var_names(s, out)),
        Stmt::If(node) => {
            collect_var_names(&node.consequent, out);
            if let Some(alternate) = &node.alternate {
                collect_var_names(alternate, out);
            }
        }
        Stmt::For(node) => {
            if let Some(ForInit::Variable(decl)) = &node.init
                && decl.kind == VarKind::Var
            {
                for declarator in &decl.declarations {
                    collect_pattern_names(&declarator.id, out);
                }
            }
            collect_var_names(&node.body, out);
        }
        Stmt::ForIn(node) => {
            if let ForHead::Variable(decl) = &node.left
                && decl.kind == VarKind::Var
            {
                for declarator in &decl.declarations {
                    collect_pattern_names(&declarator.id, out);
                }
            }
            collect_var_names(&node.body, out);
        }
        Stmt::ForOf(node) => {
            if let ForHead::Variable(decl) = &node.left
                && decl.kind == VarKind::Var
            {
                for declarator in &decl.declarations {
                    collect_pattern_names(&declarator.id, out);
                }
            }
            collect_var_names(&node.body, out);
        }
        Stmt::While(node) => collect_var_names(&node.body, out),
        Stmt::DoWhile(node) => collect_var_names(&node.body, out),
        Stmt::Labeled(node) => collect_var_names(&node.body, out),
        Stmt::Switch(node) => {
            for case in &node.cases {
                case.consequent.iter().for_each(|s| collect_var_names(s, out));
            }
        }
        Stmt::Try(node) => {
            node.block.body.iter().for_each(|s| collect_var_names(s, out));
            if let Some(handler) = &node.handler {
                handler.body.body.iter().for_each(|s| collect_var_names(s, out));
            }
            if let Some(finalizer) = &node.finalizer {
                finalizer.body.iter().for_each(|s| collect_var_names(s, out));
            }
        }
        _ => {}
    }
}

fn declares_lexically(statements: &[Stmt]) -> bool {
    statements.iter().any(|statement| match statement {
        Stmt::Function(_) => true,
        Stmt::Variable(decl) => decl.kind != VarKind::Var,
        _ => false,
    })
}

struct LoopDeclaration<'a> {
    pattern: &'a Pat,
    mutable: bool,
    lexical: bool,
}

fn loop_declaration<'a>(left: &'a ForHead, statement: &str) -> R<Option<LoopDeclaration<'a>>> {
    let ForHead::Variable(decl) = left else { return Ok(None) };
    if decl.declarations.len() != 1 {
        return Err(type_error(format!("{statement} supports one declared binding.")).at(Some(loc_of(decl.span))));
    }
    Ok(Some(LoopDeclaration {
        pattern: &decl.declarations[0].id,
        mutable: decl.kind != VarKind::Const,
        lexical: decl.kind != VarKind::Var,
    }))
}

fn head_span(left: &ForHead) -> Span {
    match left {
        ForHead::Variable(decl) => decl.span,
        ForHead::Pattern(pattern) => pattern.span(),
    }
}

impl Frame {
    pub fn new(ctx: &Ctx, scopes: Vec<Scope>) -> Frame {
        Frame { ctx: ctx.clone(), scopes: ScopeStack::new(scopes), generator: Cell::new(false), generator_async: Cell::new(false) }
    }

    /// The program body: its completion value (a thenable is adopted first).
    pub async fn run_program(&self, program: &Program) -> R<Value> {
        // Keep top-level declarations separate so they can shadow builtins.
        self.scopes.push_new();
        let result = self.run_program_body(program).await;
        self.scopes.pop();
        result
    }

    async fn run_program_body(&self, program: &Program) -> R<Value> {
        // Imports load before any code runs, as in a module.
        for statement in &program.body {
            if let Stmt::Import(node) = statement {
                self.bind_import(node)?;
            }
        }
        self.predeclare_lexical(&program.body)?;
        self.hoist_functions(&program.body);
        self.hoist_vars(&program.body, None);
        let mut value = Value::Undefined;
        let count = program.body.len();
        for (index, statement) in program.body.iter().enumerate() {
            if matches!(statement, Stmt::Import(_)) {
                continue;
            }
            if index == count - 1
                && let Stmt::Expression(node) = statement
            {
                value = self.eval(&node.expression).await?;
                break;
            }
            match self.exec(statement).await? {
                Completion::Return(returned) => {
                    value = returned;
                    break;
                }
                Completion::Break(_) => {
                    return Err(type_error("Unexpected 'break' outside of a loop.").at(Some(loc_of(statement.span()))));
                }
                Completion::Continue(_) => {
                    return Err(type_error("Unexpected 'continue' outside of a loop.").at(Some(loc_of(statement.span()))));
                }
                Completion::Normal => {}
            }
        }
        // The implicit async body adopts returned promises before copy-out.
        if is_thenable(&self.ctx, &value) {
            return self.ctx.await_value(value, None).await;
        }
        Ok(value)
    }

    fn bind_import(&self, node: &ImportDeclaration) -> R<()> {
        let specifier = match &node.source.value {
            LitValue::String(text) => text.clone(),
            _ => JsStr::from(node.source.raw.as_ref()),
        };
        if node.specifiers.is_empty() {
            self.ctx.module(&specifier, Some(loc_of(node.span)))?;
        }
        for binding in &node.specifiers {
            let (local, value) = match binding {
                ImportSpecifierKind::Default { span, local } => (local, self.ctx.module(&specifier, Some(loc_of(*span)))?),
                ImportSpecifierKind::Namespace { span, local } => {
                    (local, Value::Obj(self.ctx.namespace(&specifier, Some(loc_of(*span)))?))
                }
                ImportSpecifierKind::Named { span, imported, local } => {
                    let namespace = self.ctx.namespace(&specifier, Some(loc_of(*span)))?;
                    let name = match imported {
                        ModuleName::Identifier(id) => id.key.clone(),
                        ModuleName::Literal(lit) => match &lit.value {
                            LitValue::String(text) => text.clone(),
                            _ => JsStr::from(lit.raw.as_ref()),
                        },
                    };
                    let key = PropKey::from_js(name.clone());
                    if !has_own(&namespace, &key) {
                        return Err(syntax_error(format!("\"{specifier}\" has no export named \"{name}\"."))
                            .at(Some(loc_of(*span))));
                    }
                    (local, get_own(&self.ctx, &namespace, &key)?)
                }
            };
            let span = match binding {
                ImportSpecifierKind::Default { span, .. }
                | ImportSpecifierKind::Namespace { span, .. }
                | ImportSpecifierKind::Named { span, .. } => *span,
            };
            self.scopes.declare(&local.name, value, false, loc_of(span))?;
        }
        Ok(())
    }

    pub fn exec<'a>(&'a self, node: &'a Stmt) -> LFut<'a, R<Completion>> {
        Box::pin(self.exec_inner(node))
    }

    async fn exec_inner(&self, node: &Stmt) -> R<Completion> {
        match node {
            Stmt::Expression(statement) => {
                self.eval(&statement.expression).await?;
                Ok(Completion::Normal)
            }
            Stmt::Variable(decl) => {
                self.eval_variable_declaration(decl).await?;
                Ok(Completion::Normal)
            }
            Stmt::Return(statement) => {
                let Some(argument) = &statement.argument else { return Ok(Completion::Return(Value::Undefined)) };
                let value = self.eval(argument).await?;
                // An async generator awaits what it returns, as it awaits what it yields.
                if self.generator_async.get() {
                    return Ok(Completion::Return(self.ctx.await_value(value, Some(loc_of(statement.span))).await?));
                }
                Ok(Completion::Return(value))
            }
            Stmt::Block(block) => self.eval_block(block).await,
            Stmt::If(statement) => {
                let test = self.eval(&statement.test).await?;
                if test.truthy() {
                    self.exec(&statement.consequent).await
                } else if let Some(alternate) = &statement.alternate {
                    self.exec(alternate).await
                } else {
                    Ok(Completion::Normal)
                }
            }
            Stmt::Switch(statement) => self.eval_switch(statement).await,
            Stmt::Labeled(statement) => self.eval_labeled(statement).await,
            Stmt::While(statement) => self.eval_while(statement, None).await,
            Stmt::DoWhile(statement) => self.eval_do_while(statement, None).await,
            Stmt::For(statement) => self.eval_for(statement, None).await,
            Stmt::ForOf(statement) => self.eval_for_of(statement, None).await,
            Stmt::ForIn(statement) => self.eval_for_in(statement, None).await,
            Stmt::Break(statement) => Ok(Completion::Break(statement.label.as_ref().map(|l| l.name.clone()))),
            Stmt::Continue(statement) => Ok(Completion::Continue(statement.label.as_ref().map(|l| l.name.clone()))),
            Stmt::Throw(statement) => Err(Fail::Throw(self.eval(&statement.argument).await?)),
            Stmt::Try(statement) => self.eval_try(statement).await,
            Stmt::Empty(_) | Stmt::Function(_) => Ok(Completion::Normal),
            Stmt::Import(statement) => Err(syntax_error(
                "An import declaration may only appear at the top level of the program.",
            )
            .at(Some(loc_of(statement.span)))),
            other => Err(unsupported_syntax(other.type_name(), loc_of(other.span()))),
        }
    }

    pub async fn eval_block(&self, node: &BlockStatement) -> R<Completion> {
        let scoped = declares_lexically(&node.body);
        if scoped {
            self.scopes.push_new();
            let prepared = self.predeclare_lexical(&node.body).map(|_| self.hoist_functions(&node.body));
            if let Err(error) = prepared {
                self.scopes.pop();
                return Err(error);
            }
        }
        let result = self.eval_statements(&node.body).await;
        if scoped {
            self.scopes.pop();
        }
        result
    }

    async fn eval_statements(&self, body: &[Stmt]) -> R<Completion> {
        for statement in body {
            let result = self.exec(statement).await?;
            if result.is_abrupt() {
                return Ok(result);
            }
        }
        Ok(Completion::Normal)
    }

    pub fn create_function(&self, node: &Rc<Function>, name: Option<JsStr>) -> Obj {
        let name = name.unwrap_or_else(|| match (&node.kind, &node.id) {
            (FunctionKind::Arrow, _) => JsStr::new(),
            (_, Some(id)) => id.key.clone(),
            _ => JsStr::new(),
        });
        let ctx = &self.ctx;
        let builtins = &ctx.builtins;
        let length = node
            .params
            .iter()
            .position(|p| matches!(p, Pat::Assignment(_) | Pat::Rest(_)))
            .unwrap_or(node.params.len());
        let function = ctx.new_function_js(
            Callable::Program(ProgramFn {
                node: node.clone(),
                captured: std::cell::RefCell::new(self.scopes.capture()),
                arrow: node.kind == FunctionKind::Arrow,
            }),
            name,
            length as u32,
        );
        // Each generator function gets its own prototype, so `g() instanceof g` holds as in JS.
        if node.generator {
            let proto = if node.is_async { &builtins.async_generator } else { &builtins.generator };
            let own = ctx.new_object(Some(proto.clone()), Kind::Ordinary);
            define(&function, PropKey::from_str("prototype"), Value::Obj(own), HIDDEN);
        }
        // The body of a named function expression sees its own name, read-only.
        if node.kind == FunctionKind::Expression
            && let Some(id) = &node.id
            && let Some(program) = function.program_fn()
        {
            program.captured.borrow_mut().push(Scope::with(
                id.name.clone(),
                Binding { mutable: false, value: Value::Obj(function.clone()), initialized: true },
            ));
        }
        function
    }

    /// NamedEvaluation: an anonymous function definition takes the name of what it is assigned to.
    pub async fn eval_named(&self, node: &Expr, name: &JsStr) -> R<Value> {
        if let Expr::Function(function) = node
            && (function.kind == FunctionKind::Arrow || function.id.is_none())
        {
            return Ok(Value::Obj(self.create_function(function, Some(name.clone()))));
        }
        self.eval(node).await
    }

    // Repeated `function` declarations and `var` clashes are legal: the last declaration wins.
    fn hoist_functions(&self, statements: &[Stmt]) {
        let scope = self.scopes.current();
        for node in statements {
            let Stmt::Function(function) = node else { continue };
            let Some(id) = &function.id else { continue };
            let value = Value::Obj(self.create_function(function, None));
            scope.set_binding(id.name.clone(), Binding { mutable: true, value, initialized: true });
        }
    }

    // Hoisted `var` bindings start undefined, or copy a same-named parameter.
    fn hoist_vars(&self, statements: &[Stmt], parameters: Option<&Scope>) {
        let mut names = Vec::new();
        for statement in statements {
            collect_var_names(statement, &mut names);
        }
        let scope = self.scopes.current();
        for name in names {
            if scope.has(&name) {
                continue;
            }
            let value = parameters
                .and_then(|params| params.0.borrow().get(&name).map(|binding| binding.value.clone()))
                .unwrap_or(Value::Undefined);
            scope.set_binding(name, Binding { mutable: true, value, initialized: true });
        }
    }

    fn predeclare_lexical(&self, statements: &[Stmt]) -> R<()> {
        for statement in statements {
            let Stmt::Variable(decl) = statement else { continue };
            if decl.kind == VarKind::Var {
                continue;
            }
            for declarator in &decl.declarations {
                for name in pattern_names(&declarator.id) {
                    self.scopes.reserve(&name, decl.kind != VarKind::Const, loc_of(declarator.span))?;
                }
            }
        }
        Ok(())
    }

    fn predeclare_pattern(&self, pattern: &Pat, mutable: bool, loc: Loc) -> R<()> {
        for name in pattern_names(pattern) {
            self.scopes.reserve(&name, mutable, loc)?;
        }
        Ok(())
    }

    async fn eval_switch(&self, node: &SwitchStatement) -> R<Completion> {
        let discriminant = self.eval(&node.discriminant).await?;
        self.scopes.push_new();
        let result = self.eval_switch_body(node, discriminant).await;
        self.scopes.pop();
        result
    }

    async fn eval_switch_body(&self, node: &SwitchStatement, discriminant: Value) -> R<Completion> {
        let statements: Vec<&Stmt> = node.cases.iter().flat_map(|case| case.consequent.iter()).collect();
        for statement in &statements {
            let Stmt::Variable(decl) = statement else { continue };
            if decl.kind == VarKind::Var {
                continue;
            }
            for declarator in &decl.declarations {
                for name in pattern_names(&declarator.id) {
                    self.scopes.reserve(&name, decl.kind != VarKind::Const, loc_of(declarator.span))?;
                }
            }
        }
        let scope = self.scopes.current();
        for statement in &statements {
            if let Stmt::Function(function) = statement
                && let Some(id) = &function.id
            {
                let value = Value::Obj(self.create_function(function, None));
                scope.set_binding(id.name.clone(), Binding { mutable: true, value, initialized: true });
            }
        }
        let mut default_index = None;
        let mut selected = None;
        for (index, case) in node.cases.iter().enumerate() {
            let Some(test) = &case.test else {
                default_index = Some(index);
                continue;
            };
            if self.eval(test).await?.strict_eq(&discriminant) {
                selected = Some(index);
                break;
            }
        }
        let Some(start) = selected.or(default_index) else { return Ok(Completion::Normal) };
        for case in &node.cases[start..] {
            for statement in &case.consequent {
                match self.exec(statement).await? {
                    Completion::Break(None) => return Ok(Completion::Normal),
                    result @ (Completion::Break(_) | Completion::Return(_) | Completion::Continue(_)) => return Ok(result),
                    Completion::Normal => {}
                }
            }
        }
        Ok(Completion::Normal)
    }

    async fn tick(&self) {
        if self.ctx.runner.tick() {
            crate::interp::runner::slice(&self.ctx).await;
        }
    }

    async fn eval_while(&self, node: &WhileStatement, labels: Option<&[Rc<str>]>) -> R<Completion> {
        loop {
            if !self.eval(&node.test).await?.truthy() {
                break;
            }
            if let Some(exit) = loop_exit(self.exec(&node.body).await?, labels) {
                return Ok(exit);
            }
            self.tick().await;
        }
        Ok(Completion::Normal)
    }

    async fn eval_do_while(&self, node: &DoWhileStatement, labels: Option<&[Rc<str>]>) -> R<Completion> {
        loop {
            if let Some(exit) = loop_exit(self.exec(&node.body).await?, labels) {
                return Ok(exit);
            }
            self.tick().await;
            if !self.eval(&node.test).await?.truthy() {
                break;
            }
        }
        Ok(Completion::Normal)
    }

    async fn eval_for(&self, node: &ForStatement, labels: Option<&[Rc<str>]>) -> R<Completion> {
        self.scopes.push_new();
        let result = self.eval_for_body(node, labels).await;
        self.scopes.pop();
        result
    }

    async fn eval_for_body(&self, node: &ForStatement, labels: Option<&[Rc<str>]>) -> R<Completion> {
        let lexical_init = match &node.init {
            Some(ForInit::Variable(decl)) if decl.kind != VarKind::Var => Some(decl),
            _ => None,
        };
        if let Some(decl) = lexical_init {
            for declarator in &decl.declarations {
                for name in pattern_names(&declarator.id) {
                    self.scopes.reserve(&name, decl.kind != VarKind::Const, loc_of(declarator.span))?;
                }
            }
        }
        match &node.init {
            Some(ForInit::Variable(decl)) => self.eval_variable_declaration(decl).await?,
            Some(ForInit::Expr(expr)) => {
                self.eval(expr).await?;
            }
            None => {}
        }
        // Each iteration gets fresh copies of the loop's `let` bindings, but only closures can tell them apart.
        let per_iteration: Vec<Rc<str>> = if lexical_init.is_some()
            && (node.init.as_ref().map(for_init_creates_closure).transpose()?.unwrap_or(false)
                || node.test.as_ref().map(expr_creates_closure).transpose()?.unwrap_or(false)
                || node.update.as_ref().map(expr_creates_closure).transpose()?.unwrap_or(false)
                || stmt_creates_closure(&node.body)?)
        {
            self.scopes.current().0.borrow().keys().cloned().collect()
        } else {
            Vec::new()
        };
        let next_iteration = || {
            if per_iteration.is_empty() {
                return;
            }
            let current = self.scopes.current();
            let copied = Scope::new();
            {
                let source = current.0.borrow();
                let mut target = copied.0.borrow_mut();
                for name in &per_iteration {
                    if let Some(binding) = source.get(name) {
                        target.insert(name.clone(), binding.clone());
                    }
                }
            }
            self.scopes.replace_current(copied);
        };
        next_iteration();
        loop {
            if let Some(test) = &node.test
                && !self.eval(test).await?.truthy()
            {
                break;
            }
            if let Some(exit) = loop_exit(self.exec(&node.body).await?, labels) {
                return Ok(exit);
            }
            next_iteration();
            if let Some(update) = &node.update {
                self.eval(update).await?;
            }
            self.tick().await;
        }
        Ok(Completion::Normal)
    }

    async fn eval_for_of(&self, node: &ForOfStatement, labels: Option<&[Rc<str>]>) -> R<Completion> {
        let declared = loop_declaration(&node.left, "for...of")?;
        let lexical = declared.as_ref().is_some_and(|d| d.lexical);
        if lexical {
            self.scopes.push_new();
        }
        let result = self.eval_for_of_body(node, labels, declared).await;
        if lexical {
            self.scopes.pop();
        }
        result
    }

    async fn eval_for_of_body(
        &self,
        node: &ForOfStatement,
        labels: Option<&[Rc<str>]>,
        declared: Option<LoopDeclaration<'_>>,
    ) -> R<Completion> {
        let ctx = &self.ctx;
        let loc = Some(loc_of(node.span));
        let awaiting = node.is_await;
        let left_loc = loc_of(head_span(&node.left));
        if let Some(d) = &declared
            && d.lexical
        {
            self.predeclare_pattern(d.pattern, d.mutable, left_loc)?;
        }
        let right = self.eval(&node.right).await?;
        let builtin = ctx.builtin_cursor(&right);
        let custom = match &builtin {
            None => ctx.custom_iterator(&right, loc, awaiting).await?,
            Some(_) => None,
        };
        if custom.is_none() && builtin.is_none() {
            return Err(invalid_data(format!(
                "{} requires an iterable value, received {}.",
                if awaiting { "for await...of" } else { "for...of" },
                describe_value(&right)
            ))
            .at(loc));
        }
        // `for await` over a synchronous iterator drives it through CreateAsyncFromSyncIterator, as JS does.
        let iterator = if awaiting && !custom.as_ref().is_some_and(|c| c.asynchronous) {
            Some(crate::interp::interp::async_from_sync(ctx, builtin.clone(), custom.clone(), loc))
        } else {
            custom
        };
        let cursor: Option<Rc<Cursor>> = if iterator.is_none() { builtin } else { None };
        let close = || {
            let (ctx, iterator, cursor) = (ctx.clone(), iterator.clone(), cursor.clone());
            async move {
                match (iterator, cursor) {
                    (Some(iterator), _) => crate::interp::cursor::close_iterator(&ctx, &iterator.iterator, iterator.asynchronous, loc).await,
                    (None, Some(cursor)) => cursor.close_now(&ctx).await,
                    (None, None) => Ok(()),
                }
            }
        };
        if let ForHead::Pattern(pattern @ (Pat::Rest(_) | Pat::Assignment(_))) = &node.left {
            return Err(type_error("Unsupported for...of binding.").at(Some(loc_of(pattern.span()))));
        }
        let assignment = match &node.left {
            ForHead::Pattern(pattern) => Some(pattern),
            ForHead::Variable(_) => None,
        };
        // Each iteration gets a fresh binding, but only a closure in the body can tell.
        let reusable = match &declared {
            Some(d) if d.lexical && matches!(d.pattern, Pat::Identifier(_)) => !stmt_creates_closure(&node.body)?,
            _ => false,
        };
        let reused = match &declared {
            Some(d) if reusable => {
                let Pat::Identifier(id) = d.pattern else { unreachable!() };
                let scope = Scope::with(id.name.clone(), Binding { mutable: d.mutable, value: Value::Undefined, initialized: false });
                self.scopes.push(scope.clone());
                Some((scope, id.name.clone()))
            }
            _ => None,
        };
        let result = async {
            loop {
                let step: Step = match &iterator {
                    Some(it) => {
                        crate::interp::cursor::next_iterator_result(ctx, &it.iterator, &it.next, it.asynchronous, loc).await?
                    }
                    None => cursor.as_ref().unwrap().next_step(ctx).await?,
                };
                if step.done {
                    return Ok(Completion::Normal);
                }
                let body = async {
                    if let Some((scope, name)) = &reused {
                        if let Some(binding) = scope.0.borrow_mut().get_mut(name) {
                            binding.value = step.value.clone();
                            binding.initialized = true;
                        }
                        return self.exec(&node.body).await;
                    }
                    if let Some(d) = &declared
                        && d.lexical
                    {
                        self.scopes.push_new();
                        let result = async {
                            self.predeclare_pattern(d.pattern, d.mutable, left_loc)?;
                            self.declare_pattern(d.pattern, step.value.clone(), d.mutable, left_loc, true).await?;
                            self.exec(&node.body).await
                        }
                        .await;
                        self.scopes.pop();
                        return result;
                    }
                    if let Some(d) = &declared {
                        self.assign_pattern(d.pattern, step.value.clone(), left_loc).await?;
                    } else if let Some(pattern) = assignment {
                        self.assign_pattern(pattern, step.value.clone(), left_loc).await?;
                    }
                    self.exec(&node.body).await
                };
                let result = match body.await {
                    Ok(result) => result,
                    Err(error) => return preserve_consumer_error(close(), Err(error)).await,
                };
                if let Some(exit) = loop_exit(result, labels) {
                    close().await?;
                    return Ok(exit);
                }
                self.tick().await;
            }
        }
        .await;
        if reused.is_some() {
            self.scopes.pop();
        }
        result
    }

    async fn eval_for_in(&self, node: &ForInStatement, labels: Option<&[Rc<str>]>) -> R<Completion> {
        let declared = loop_declaration(&node.left, "for...in")?;
        let lexical = declared.as_ref().is_some_and(|d| d.lexical);
        if lexical {
            self.scopes.push_new();
        }
        let result = self.eval_for_in_body(node, labels, declared).await;
        if lexical {
            self.scopes.pop();
        }
        result
    }

    // for...in over null/undefined iterates nothing. After the own keys come each prototype's enumerable string
    // keys, skipping any key a nearer level has, even as a non-enumerable property.
    fn enumerable_keys(&self, value: &Value, loc: Loc) -> R<Vec<JsStr>> {
        let ctx = &self.ctx;
        if let Value::Tool(tool) = value {
            return ctx.tools.keys(tool.path());
        }
        if value.is_nullish() {
            return Ok(Vec::new());
        }
        let source = crate::stdlib::object::enumerable_source(ctx, "for...in", value, Some(loc))?;
        let mut chain = vec![source.clone()];
        let mut proto = ctx.primitive_prototype(value).or_else(|| source.proto());
        while let Some(level) = proto {
            proto = level.proto();
            chain.push(level);
        }
        let mut out = Vec::new();
        for (depth, target) in chain.iter().enumerate() {
            for key in keys(target) {
                if chain[..depth].iter().any(|level| has_own(level, &key)) {
                    continue;
                }
                out.push(key.to_js());
            }
        }
        Ok(out)
    }

    async fn eval_for_in_body(
        &self,
        node: &ForInStatement,
        labels: Option<&[Rc<str>]>,
        declared: Option<LoopDeclaration<'_>>,
    ) -> R<Completion> {
        let left_loc = loc_of(head_span(&node.left));
        if let Some(d) = &declared
            && d.lexical
        {
            self.predeclare_pattern(d.pattern, d.mutable, left_loc)?;
        }
        let right = self.eval(&node.right).await?;
        let keys = self.enumerable_keys(&right, loc_of(node.right.span()))?;
        if let ForHead::Pattern(pattern @ (Pat::Rest(_) | Pat::Assignment(_))) = &node.left {
            return Err(type_error("Unsupported for...in binding.").at(Some(loc_of(pattern.span()))));
        }
        for key in keys {
            let key = Value::Str(key);
            let result = if let Some(d) = &declared
                && d.lexical
            {
                self.scopes.push_new();
                let result = async {
                    self.predeclare_pattern(d.pattern, d.mutable, left_loc)?;
                    self.declare_pattern(d.pattern, key, d.mutable, left_loc, true).await?;
                    self.exec(&node.body).await
                }
                .await;
                self.scopes.pop();
                result?
            } else {
                if let Some(d) = &declared {
                    self.assign_pattern(d.pattern, key, left_loc).await?;
                } else if let ForHead::Pattern(pattern) = &node.left {
                    self.assign_pattern(pattern, key, left_loc).await?;
                }
                self.exec(&node.body).await?
            };
            if let Some(exit) = loop_exit(result, labels) {
                return Ok(exit);
            }
            self.tick().await;
        }
        Ok(Completion::Normal)
    }

    async fn eval_labeled(&self, node: &LabeledStatement) -> R<Completion> {
        let mut labels: Vec<Rc<str>> = Vec::new();
        let mut body: &Stmt = &node.body;
        labels.push(node.label.name.clone());
        while let Stmt::Labeled(inner) = body {
            labels.push(inner.label.name.clone());
            body = &inner.body;
        }
        let result = match body {
            Stmt::While(loop_node) => self.eval_while(loop_node, Some(&labels)).await?,
            Stmt::DoWhile(loop_node) => self.eval_do_while(loop_node, Some(&labels)).await?,
            Stmt::For(loop_node) => self.eval_for(loop_node, Some(&labels)).await?,
            Stmt::ForOf(loop_node) => self.eval_for_of(loop_node, Some(&labels)).await?,
            Stmt::ForIn(loop_node) => self.eval_for_in(loop_node, Some(&labels)).await?,
            other => self.exec(other).await?,
        };
        match &result {
            Completion::Break(Some(label)) if labels.contains(label) => Ok(Completion::Normal),
            _ => Ok(result),
        }
    }

    async fn eval_try(&self, node: &TryStatement) -> R<Completion> {
        let mut result = Completion::Normal;
        let mut failure: Option<Fail> = None;
        match self.eval_block(&node.block).await {
            Ok(value) => result = value,
            Err(error) => {
                if error.is_interrupt() {
                    return Err(error);
                }
                // A generator's return() unwinds through `finally` but is not an exception `catch` sees.
                match (&error, &node.handler) {
                    (Fail::GenReturn(_), _) | (_, None) => {
                        if node.finalizer.is_none() {
                            return Err(error);
                        }
                        failure = Some(error);
                    }
                    (_, Some(handler)) => {
                        let caught = materialize(&self.ctx, &error);
                        self.scopes.push_new();
                        let handled = async {
                            if let Some(param) = &handler.param {
                                self.declare_pattern(param, caught, true, loc_of(handler.span), false).await?;
                            }
                            self.eval_block(&handler.body).await
                        }
                        .await;
                        self.scopes.pop();
                        match handled {
                            Ok(value) => result = value,
                            Err(error) => {
                                if node.finalizer.is_none() || error.is_interrupt() {
                                    return Err(error);
                                }
                                failure = Some(error);
                            }
                        }
                    }
                }
            }
        }
        let Some(finalizer) = &node.finalizer else { return Ok(result) };
        let finished = self.eval_block(finalizer).await?;
        if finished.is_abrupt() {
            return Ok(finished);
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(result)
    }

    pub async fn eval_variable_declaration(&self, node: &VariableDeclaration) -> R<()> {
        for declaration in &node.declarations {
            let init = declaration.init.as_ref();
            let id = &declaration.id;
            let loc = loc_of(declaration.span);
            // `var x` alone is a no-op: the binding was hoisted on function entry.
            if node.kind == VarKind::Var {
                if let Some(init) = init {
                    let value = match id {
                        Pat::Identifier(name) => self.eval_named(init, &name.key).await?,
                        _ => self.eval(init).await?,
                    };
                    self.assign_pattern(id, value, loc).await?;
                }
                continue;
            }
            let value = match (init, id) {
                (None, _) => Value::Undefined,
                (Some(init), Pat::Identifier(name)) => self.eval_named(init, &name.key).await?,
                (Some(init), _) => self.eval(init).await?,
            };
            self.declare_pattern(id, value, node.kind != VarKind::Const, loc, true).await?;
        }
        Ok(())
    }

    pub fn declare_pattern<'a>(
        &'a self,
        pattern: &'a Pat,
        value: Value,
        mutable: bool,
        loc: Loc,
        initialize: bool,
    ) -> LFut<'a, R<()>> {
        Box::pin(async move {
            match pattern {
                Pat::Identifier(id) => {
                    if initialize {
                        self.scopes.initialize(&id.name, value, loc)
                    } else {
                        self.scopes.declare(&id.name, value, mutable, loc)
                    }
                }
                Pat::Assignment(node) => {
                    let resolved = if value.is_undefined() { self.eval_default(node).await? } else { value };
                    self.declare_pattern(&node.left, resolved, mutable, loc, initialize).await
                }
                Pat::Object(node) => {
                    if value.is_nullish() {
                        let shown = if value.is_undefined() { "undefined" } else { "null" };
                        return Err(type_error(format!("Cannot destructure {shown} as it is {shown}."))
                            .at(Some(loc_of(node.span))));
                    }
                    let mut consumed: Vec<PropKey> = Vec::new();
                    for property in &node.properties {
                        match property {
                            ObjectPatternMember::Rest(rest) => {
                                let target = self.ctx.new_plain();
                                let source = crate::stdlib::object::enumerable_source(
                                    &self.ctx,
                                    "Object destructuring",
                                    &value,
                                    Some(loc_of(node.span)),
                                )?;
                                crate::interp::object::assign(&self.ctx, &target, &source, Some(&consumed))?;
                                self.declare_pattern(&rest.argument, Value::Obj(target), mutable, loc_of(rest.span), initialize)
                                    .await?;
                            }
                            ObjectPatternMember::Property(prop) => {
                                let key = self.destructuring_key(prop).await?;
                                consumed.push(key.clone());
                                let item = self.destructured_property(&value, &key, loc_of(prop.span))?;
                                self.declare_pattern(&prop.value, item, mutable, loc_of(prop.span), initialize).await?;
                            }
                        }
                    }
                    Ok(())
                }
                Pat::Array(node) => {
                    self.destructure_array(node, value, &|target, item, context| {
                        self.declare_pattern(target, item, mutable, context, initialize)
                    })
                    .await
                }
                other => Err(type_error(format!("Unsupported binding pattern '{}'.", other.type_name()))
                    .at(Some(loc_of(other.span())))),
            }
        })
    }

    pub fn assign_pattern<'a>(&'a self, pattern: &'a Pat, value: Value, loc: Loc) -> LFut<'a, R<()>> {
        Box::pin(async move {
            match pattern {
                Pat::Identifier(id) => {
                    self.scopes.set(&id.name, value, loc_of(id.span))?;
                    Ok(())
                }
                Pat::Member(member) => {
                    self.write_member(member, value).await?;
                    Ok(())
                }
                Pat::Assignment(node) => {
                    let resolved = if value.is_undefined() { self.eval_default(node).await? } else { value };
                    self.assign_pattern(&node.left, resolved, loc).await
                }
                Pat::Object(node) => {
                    if value.is_nullish() {
                        let shown = if value.is_undefined() { "undefined" } else { "null" };
                        return Err(type_error(format!("Cannot destructure {shown} as it is {shown}."))
                            .at(Some(loc_of(node.span))));
                    }
                    let mut consumed: Vec<PropKey> = Vec::new();
                    for property in &node.properties {
                        match property {
                            ObjectPatternMember::Rest(rest) => {
                                let target = self.ctx.new_plain();
                                let source = crate::stdlib::object::enumerable_source(
                                    &self.ctx,
                                    "Object destructuring",
                                    &value,
                                    Some(loc_of(node.span)),
                                )?;
                                crate::interp::object::assign(&self.ctx, &target, &source, Some(&consumed))?;
                                self.assign_pattern(&rest.argument, Value::Obj(target), loc_of(rest.span)).await?;
                            }
                            ObjectPatternMember::Property(prop) => {
                                let key = self.destructuring_key(prop).await?;
                                consumed.push(key.clone());
                                let item = self.destructured_property(&value, &key, loc_of(prop.span))?;
                                self.assign_pattern(&prop.value, item, loc_of(prop.span)).await?;
                            }
                        }
                    }
                    Ok(())
                }
                Pat::Array(node) => {
                    self.destructure_array(node, value, &|target, item, context| self.assign_pattern(target, item, context))
                        .await
                }
                Pat::Rest(_) => Err(type_error("Unsupported assignment pattern 'RestElement'.").at(Some(loc))),
            }
        })
    }

    async fn eval_default(&self, node: &AssignmentPattern) -> R<Value> {
        match &node.left {
            Pat::Identifier(id) => self.eval_named(&node.right, &id.key).await,
            _ => self.eval(&node.right).await,
        }
    }

    async fn destructure_array<'a>(
        &'a self,
        pattern: &'a ArrayPattern,
        value: Value,
        consume: &(dyn Fn(&'a Pat, Value, Loc) -> LFut<'a, R<()>> + 'a),
    ) -> R<()> {
        let ctx = &self.ctx;
        let loc = loc_of(pattern.span);
        let Some(cursor) = ctx.iterate(&value, Some(loc)).await? else {
            return Err(type_error("Array destructuring requires a supported iterable value.").at(Some(loc)));
        };
        let mut done = false;
        for element in &pattern.elements {
            if done {
                let Some(element) = element else { continue };
                match element {
                    Pat::Rest(rest) => {
                        consume(&rest.argument, Value::Obj(ctx.new_array(Vec::new())), loc_of(rest.span)).await?;
                        return Ok(());
                    }
                    other => consume(other, Value::Undefined, loc_of(other.span())).await?,
                }
                continue;
            }
            let step = cursor.next_step(ctx).await?;
            done = step.done;
            let Some(element) = element else { continue };
            if let Pat::Rest(rest) = element {
                let mut items = Vec::new();
                if !step.done {
                    items.push(step.value);
                }
                while !done {
                    let next = cursor.next_step(ctx).await?;
                    done = next.done;
                    if !done {
                        items.push(next.value);
                    }
                }
                consume(&rest.argument, Value::Obj(ctx.new_array(items)), loc_of(rest.span)).await?;
                return Ok(());
            }
            if step.done {
                consume(element, Value::Undefined, loc).await?;
            } else {
                let consumed = consume(element, step.value, loc).await;
                let close = {
                    let (ctx, cursor) = (ctx.clone(), cursor.clone());
                    async move { cursor.close_now(&ctx).await }
                };
                preserve_consumer_error(close, consumed).await?;
            }
        }
        if !done {
            cursor.close_now(ctx).await?;
        }
        Ok(())
    }

    async fn destructuring_key(&self, property: &AssignmentProperty) -> R<PropKey> {
        if property.computed {
            let value = self.eval(&property.key).await?;
            return self.to_property_key(value, loc_of(property.key.span())).await;
        }
        match &property.key {
            Expr::Identifier(id) => Ok(PropKey::from_js(id.key.clone())),
            Expr::Literal(lit) => Ok(PropKey::from_js(crate::interp::coerce::to_string_plain(&literal_value(lit)?))),
            other => Err(unsupported_syntax(other.type_name(), loc_of(other.span()))),
        }
    }

    // One destructured property, read the way a member expression would read it (primitives use their prototype).
    fn destructured_property(&self, source: &Value, key: &PropKey, loc: Loc) -> R<Value> {
        match self.resolve_property(source, key.clone(), loc, loc)? {
            Reference::Tool(tool) => Ok(Value::Tool(tool)),
            Reference::Value(value) => Ok(value),
            Reference::Member { target, key, receiver } => self.read_property(&target, &key, loc, &receiver),
            Reference::Short => Ok(Value::Undefined),
        }
    }

    pub fn read_property(&self, target: &Obj, key: &PropKey, loc: Loc, receiver: &Value) -> R<Value> {
        crate::interp::object::get_with(&self.ctx, target, key, receiver).map_err(|e| e.at(Some(loc)))
    }

    // --- Functions ---

    /// Binds parameters (defaults, destructuring, rest) into the parameter scope.
    pub async fn bind_parameters(&self, function: &Function, args: &[Value]) -> R<()> {
        let parameters = &function.params;
        for (index, parameter) in parameters.iter().enumerate() {
            match parameter {
                Pat::Rest(rest) => {
                    let items = args.get(index..).map(|rest| rest.to_vec()).unwrap_or_default();
                    let array = Value::Obj(self.ctx.new_array(items));
                    self.declare_pattern(&rest.argument, array, true, loc_of(rest.span), true).await?;
                    break;
                }
                Pat::Identifier(id) => {
                    // A sloppy simple parameter list may repeat a name; the last occurrence wins.
                    let last = parameters
                        .iter()
                        .rposition(|other| matches!(other, Pat::Identifier(o) if o.name == id.name))
                        .unwrap_or(index);
                    if last != index {
                        continue;
                    }
                    let value = args.get(index).cloned().unwrap_or_default();
                    self.scopes.initialize(&id.name, value, loc_of(id.span))?;
                }
                other => {
                    let value = args.get(index).cloned().unwrap_or_default();
                    self.declare_pattern(other, value, true, loc_of(other.span()), true).await?;
                }
            }
        }
        Ok(())
    }

    pub async fn function_body(&self, function: &Function, param_scope: &Scope) -> R<Value> {
        match &function.body {
            FunctionBody::Block(block) => {
                self.scopes.push_new();
                self.hoist_vars(&block.body, Some(param_scope));
                match self.eval_block(block).await? {
                    Completion::Return(value) => Ok(value),
                    _ => Ok(Value::Undefined),
                }
            }
            FunctionBody::Expr(expr) => self.eval(expr).await,
        }
    }
}

/// A thenable the program's implicit async body adopts before copy-out.
pub fn is_thenable(ctx: &Ctx, value: &Value) -> bool {
    let Value::Obj(obj) = value else { return false };
    obj.is_promise()
        || crate::interp::coerce::is_callable_value(&get(ctx, obj, &PropKey::from_str("then")).unwrap_or_default())
}

pub fn literal_value(node: &Literal) -> R<Value> {
    match &node.value {
        LitValue::String(text) => Ok(Value::Str(text.clone())),
        LitValue::Number(n) => Ok(Value::Num(*n)),
        LitValue::Boolean(b) => Ok(Value::Bool(*b)),
        LitValue::Null => Ok(Value::Null),
        LitValue::BigInt(_) => Err(type_error("BigInt literals are not supported.").at(Some(loc_of(node.span)))),
        LitValue::RegExp { .. } => Err(unsupported_syntax("RegExpLiteral", loc_of(node.span))),
    }
}

/// A resolved member: the object to read through and the receiver inherited accessors see.
pub enum Reference {
    Member { target: Obj, key: PropKey, receiver: Value },
    Tool(ToolRef),
    Value(Value),
    Short,
}

// Whether evaluating a node can create a function, which could capture a loop's per-iteration bindings.
pub fn expr_creates_closure(node: &Expr) -> R<bool> {
    crate::interp::walk::expr_has_closure(node)
}

pub fn stmt_creates_closure(node: &Stmt) -> R<bool> {
    crate::interp::walk::stmt_has_closure(node)
}

fn for_init_creates_closure(init: &ForInit) -> R<bool> {
    match init {
        ForInit::Expr(expr) => expr_creates_closure(expr),
        ForInit::Variable(decl) => {
            for d in &decl.declarations {
                if d.init.as_ref().map(expr_creates_closure).transpose()?.unwrap_or(false)
                    || crate::interp::walk::pat_has_closure(&d.id)?
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

/// Invokes a program function. A program call runs as its own activation; an async call returns its promise when
/// the body first suspends or ends; a generator call binds its parameters and returns the generator without running
/// the body.
pub fn invoke_function(ctx: &Ctx, callee: &Obj, this: Value, args: Vec<Value>, site: Option<Loc>) -> R<Fut<R<Value>>> {
    // The program's own activation is at the bottom of the stack.
    ctx.runner.check_depth().map_err(|e| e.at(site))?;
    let program = callee.program_fn().expect("a program function");
    let node = program.node.clone();
    let mut scopes = program.captured.borrow().clone();
    let param_scope = Scope::new();
    scopes.push(param_scope.clone());
    let frame = Rc::new(Frame::new(ctx, scopes));
    // `this` and `arguments` are scope bindings so arrows resolve them lexically.
    if !program.arrow {
        param_scope.set_binding(Rc::from("this"), Binding { mutable: false, value: this, initialized: true });
        // An overflow here reaches the program as the host's RangeError (TS throws it unlocated).
        if ctx.uses_arguments(&node)? {
            let arguments = ctx.new_arguments(&args);
            param_scope.set_binding(Rc::from("arguments"), Binding { mutable: true, value: Value::Obj(arguments), initialized: true });
        }
    }
    let simple = node.params.iter().all(|p| matches!(p, Pat::Identifier(_)));
    if simple {
        for (index, parameter) in node.params.iter().enumerate() {
            let Pat::Identifier(id) = parameter else { unreachable!() };
            let value = args.get(index).cloned().unwrap_or_default();
            param_scope.set_binding(id.name.clone(), Binding { mutable: true, value, initialized: true });
        }
    } else {
        for parameter in &node.params {
            for name in pattern_names(parameter) {
                param_scope.set_binding(name, Binding { mutable: true, value: Value::Undefined, initialized: false });
            }
        }
    }
    if node.generator {
        frame.generator.set(true);
        frame.generator_async.set(node.is_async);
        let create = {
            let (ctx, frame, node, param_scope, callee) = (ctx.clone(), frame.clone(), node.clone(), param_scope.clone(), callee.clone());
            move || -> R<Value> {
                // The prototype is read after the parameters bind, since a default may replace it.
                let proto = match get(&ctx, &callee, &PropKey::from_str("prototype"))? {
                    Value::Obj(proto) => proto,
                    _ if node.is_async => ctx.builtins.async_generator.clone(),
                    _ => ctx.builtins.generator.clone(),
                };
                let body = {
                    let (frame, node, param_scope) = (frame.clone(), node.clone(), param_scope.clone());
                    async move {
                        // A `return()` request ends the body with the returned value.
                        match frame.function_body(&node, &param_scope).await {
                            Err(Fail::GenReturn(value)) => Ok(value),
                            other => other,
                        }
                    }
                };
                let body = activation(&ctx, boxed(body));
                Ok(Value::Obj(crate::interp::generators::new_generator(&ctx, proto, node.is_async, body)))
            }
        };
        if simple {
            let created = create();
            return Ok(boxed(async move { created }));
        }
        return Ok(boxed(async move {
            frame.bind_parameters(&node, &args).await?;
            create()
        }));
    }
    let body = {
        let (frame, node) = (frame.clone(), node.clone());
        async move {
            if !simple {
                frame.bind_parameters(&node, &args).await?;
            }
            frame.function_body(&node, &param_scope).await
        }
    };
    let body = activation(ctx, boxed(body));
    if !node.is_async {
        return Ok(body);
    }
    let promise = ctx.runner.new_promise(ctx)?;
    let resolving = ctx.runner.resolving(&promise, None);
    let boundary = Boundary::Async {
        settle: Box::new(move |ctx, completion| resolving.settle(ctx, completion)),
        result: Value::Obj(promise),
    };
    let ctx = ctx.clone();
    Ok(boxed(async move { drive_inline(&ctx, body, boundary, None).await }))
}
