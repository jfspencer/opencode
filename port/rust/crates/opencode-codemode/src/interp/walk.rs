//! Generic searches over the syntax tree, visiting every child node as the TS interpreter's `Object.values` walks
//! do (property keys, member names, and labels included).

use crate::parser::ast::*;

/// What a search does at one node.
pub enum Visit<'a> {
    Ident(&'a str),
    Function(&'a Function),
}

pub enum Step {
    Found,
    Skip,
    Descend,
}

pub struct Search<'f> {
    pub at: &'f dyn Fn(Visit<'_>) -> Step,
}

impl Search<'_> {
    fn ident(&self, id: &Ident) -> bool {
        matches!((self.at)(Visit::Ident(&id.name)), Step::Found)
    }

    pub fn function(&self, node: &Function) -> bool {
        match (self.at)(Visit::Function(node)) {
            Step::Found => true,
            Step::Skip => false,
            Step::Descend => self.function_parts(node),
        }
    }

    pub fn function_parts(&self, node: &Function) -> bool {
        node.id.as_ref().is_some_and(|id| self.ident(id))
            || node.params.iter().any(|p| self.pat(p))
            || match &node.body {
                FunctionBody::Block(block) => block.body.iter().any(|s| self.stmt(s)),
                FunctionBody::Expr(expr) => self.expr(expr),
            }
    }

    fn class(&self, node: &Class) -> bool {
        node.id.as_ref().is_some_and(|id| self.ident(id))
            || node.super_class.as_ref().is_some_and(|e| self.expr(e))
            || node.body.body.iter().any(|element| match element {
                ClassElement::Method(method) => self.expr(&method.key) || self.function(&method.value),
                ClassElement::Property(prop) => self.expr(&prop.key) || prop.value.as_ref().is_some_and(|v| self.expr(v)),
                ClassElement::StaticBlock(block) => block.body.iter().any(|s| self.stmt(s)),
            })
    }

    fn spread(&self, node: &ExprOrSpread) -> bool {
        match node {
            ExprOrSpread::Expr(expr) => self.expr(expr),
            ExprOrSpread::Spread(spread) => self.expr(&spread.argument),
        }
    }

    fn template(&self, node: &TemplateLiteral) -> bool {
        node.expressions.iter().any(|e| self.expr(e))
    }

    pub fn expr(&self, node: &Expr) -> bool {
        match node {
            Expr::Identifier(id) => self.ident(id),
            Expr::PrivateIdentifier(_) | Expr::Literal(_) | Expr::This(_) | Expr::Super(_) => false,
            Expr::Array(node) => node.elements.iter().flatten().any(|e| self.spread(e)),
            Expr::Object(node) => node.properties.iter().any(|p| match p {
                ObjectMember::Property(prop) => self.expr(&prop.key) || self.expr(&prop.value),
                ObjectMember::Spread(spread) => self.expr(&spread.argument),
            }),
            Expr::Function(function) => self.function(function),
            Expr::Class(class) => self.class(class),
            Expr::Unary(node) => self.expr(&node.argument),
            Expr::Update(node) => self.expr(&node.argument),
            Expr::Binary(node) => self.expr(&node.left) || self.expr(&node.right),
            Expr::Logical(node) => self.expr(&node.left) || self.expr(&node.right),
            Expr::Assignment(node) => self.pat(&node.left) || self.expr(&node.right),
            Expr::Member(node) => self.expr(&node.object) || self.expr(&node.property),
            Expr::Conditional(node) => self.expr(&node.test) || self.expr(&node.consequent) || self.expr(&node.alternate),
            Expr::Call(node) => self.expr(&node.callee) || node.arguments.iter().any(|a| self.spread(a)),
            Expr::New(node) => self.expr(&node.callee) || node.arguments.iter().any(|a| self.spread(a)),
            Expr::Sequence(node) => node.expressions.iter().any(|e| self.expr(e)),
            Expr::Yield(node) => node.argument.as_ref().is_some_and(|e| self.expr(e)),
            Expr::Await(node) => self.expr(&node.argument),
            Expr::Template(node) => self.template(node),
            Expr::TaggedTemplate(node) => self.expr(&node.tag) || self.template(&node.quasi),
            Expr::Chain(node) => self.expr(&node.expression),
            Expr::MetaProperty(node) => self.ident(&node.meta) || self.ident(&node.property),
            Expr::Import(node) => self.expr(&node.source) || node.options.as_ref().is_some_and(|e| self.expr(e)),
        }
    }

    pub fn pat(&self, node: &Pat) -> bool {
        match node {
            Pat::Identifier(id) => self.ident(id),
            Pat::Member(member) => self.expr(&member.object) || self.expr(&member.property),
            Pat::Object(node) => node.properties.iter().any(|p| match p {
                ObjectPatternMember::Property(prop) => self.expr(&prop.key) || self.pat(&prop.value),
                ObjectPatternMember::Rest(rest) => self.pat(&rest.argument),
            }),
            Pat::Array(node) => node.elements.iter().flatten().any(|e| self.pat(e)),
            Pat::Rest(rest) => self.pat(&rest.argument),
            Pat::Assignment(node) => self.pat(&node.left) || self.expr(&node.right),
        }
    }

    fn decl(&self, node: &VariableDeclaration) -> bool {
        node.declarations.iter().any(|d| self.pat(&d.id) || d.init.as_ref().is_some_and(|e| self.expr(e)))
    }

    fn module_name(&self, name: &ModuleName) -> bool {
        match name {
            ModuleName::Identifier(id) => self.ident(id),
            ModuleName::Literal(_) => false,
        }
    }

    pub fn stmt(&self, node: &Stmt) -> bool {
        match node {
            Stmt::Expression(node) => self.expr(&node.expression),
            Stmt::Block(node) => node.body.iter().any(|s| self.stmt(s)),
            Stmt::Empty(_) | Stmt::Debugger(_) => false,
            Stmt::With(node) => self.expr(&node.object) || self.stmt(&node.body),
            Stmt::Return(node) => node.argument.as_ref().is_some_and(|e| self.expr(e)),
            Stmt::Labeled(node) => self.ident(&node.label) || self.stmt(&node.body),
            Stmt::Break(node) => node.label.as_ref().is_some_and(|l| self.ident(l)),
            Stmt::Continue(node) => node.label.as_ref().is_some_and(|l| self.ident(l)),
            Stmt::If(node) => {
                self.expr(&node.test) || self.stmt(&node.consequent) || node.alternate.as_ref().is_some_and(|s| self.stmt(s))
            }
            Stmt::Switch(node) => {
                self.expr(&node.discriminant)
                    || node.cases.iter().any(|c| {
                        c.test.as_ref().is_some_and(|e| self.expr(e)) || c.consequent.iter().any(|s| self.stmt(s))
                    })
            }
            Stmt::Throw(node) => self.expr(&node.argument),
            Stmt::Try(node) => {
                node.block.body.iter().any(|s| self.stmt(s))
                    || node.handler.as_ref().is_some_and(|h| {
                        h.param.as_ref().is_some_and(|p| self.pat(p)) || h.body.body.iter().any(|s| self.stmt(s))
                    })
                    || node.finalizer.as_ref().is_some_and(|f| f.body.iter().any(|s| self.stmt(s)))
            }
            Stmt::While(node) => self.expr(&node.test) || self.stmt(&node.body),
            Stmt::DoWhile(node) => self.stmt(&node.body) || self.expr(&node.test),
            Stmt::For(node) => {
                node.init.as_ref().is_some_and(|init| match init {
                    ForInit::Variable(decl) => self.decl(decl),
                    ForInit::Expr(expr) => self.expr(expr),
                }) || node.test.as_ref().is_some_and(|e| self.expr(e))
                    || node.update.as_ref().is_some_and(|e| self.expr(e))
                    || self.stmt(&node.body)
            }
            Stmt::ForIn(node) => self.head(&node.left) || self.expr(&node.right) || self.stmt(&node.body),
            Stmt::ForOf(node) => self.head(&node.left) || self.expr(&node.right) || self.stmt(&node.body),
            Stmt::Function(function) => self.function(function),
            Stmt::Variable(decl) => self.decl(decl),
            Stmt::Class(class) => self.class(class),
            Stmt::Import(node) => node.specifiers.iter().any(|s| match s {
                ImportSpecifierKind::Named { imported, local, .. } => self.module_name(imported) || self.ident(local),
                ImportSpecifierKind::Default { local, .. } | ImportSpecifierKind::Namespace { local, .. } => self.ident(local),
            }) || node.attributes.iter().any(|a| self.module_name(&a.key)),
            Stmt::ExportNamed(node) => {
                node.declaration.as_ref().is_some_and(|s| self.stmt(s))
                    || node.specifiers.iter().any(|s| self.module_name(&s.local) || self.module_name(&s.exported))
            }
            Stmt::ExportDefault(node) => match &node.declaration {
                ExportDefaultKind::Declaration(stmt) => self.stmt(stmt),
                ExportDefaultKind::Expr(expr) => self.expr(expr),
            },
            Stmt::ExportAll(node) => node.exported.as_ref().is_some_and(|e| self.module_name(e)),
        }
    }

    fn head(&self, head: &ForHead) -> bool {
        match head {
            ForHead::Variable(decl) => self.decl(decl),
            ForHead::Pattern(pattern) => self.pat(pattern),
        }
    }
}

fn closure_step(visit: Visit<'_>) -> Step {
    match visit {
        Visit::Function(_) => Step::Found,
        Visit::Ident(_) => Step::Skip,
    }
}

pub fn expr_has(node: &Expr, _: &dyn Fn(&Expr) -> bool, _: &dyn Fn(&Stmt) -> bool, _: bool) -> bool {
    Search { at: &closure_step }.expr(node)
}

pub fn stmt_has(node: &Stmt, _: &dyn Fn(&Expr) -> bool, _: &dyn Fn(&Stmt) -> bool, _: bool) -> bool {
    Search { at: &closure_step }.stmt(node)
}

pub fn pat_has_closure(node: &Pat) -> bool {
    Search { at: &closure_step }.pat(node)
}

/// Whether a function's parameters or body read `arguments`, looking through arrows but not nested functions.
pub fn uses_arguments(function: &Function) -> bool {
    let step = |visit: Visit<'_>| match visit {
        Visit::Ident(name) => {
            if name == "arguments" {
                Step::Found
            } else {
                Step::Skip
            }
        }
        Visit::Function(inner) => {
            if inner.kind == FunctionKind::Arrow {
                Step::Descend
            } else {
                Step::Skip
            }
        }
    };
    let search = Search { at: &step };
    function.params.iter().any(|p| search.pat(p))
        || match &function.body {
            FunctionBody::Block(block) => block.body.iter().any(|s| search.stmt(s)),
            FunctionBody::Expr(expr) => search.expr(expr),
        }
}
