//! `CodeMode.scanTools` (scan.ts): every `tools.<path>` a program references, in first-seen order, without running
//! it. It fails on syntax errors, on dynamic bracket access (`tools[name]`), and on aliasing or passing `tools` rather
//! than calling `tools.<path>(...)` directly. Core's schedule checks use it to vet a condition's tool calls.
//!
//! TS walks acorn's tree generically, visiting each node's children in the order acorn set its properties; `children`
//! lists them in that order (a `switch` case's statements before its test, a label after its body).

use crate::interp::Loc;
use crate::interp::errors::{Diagnostic, DiagnosticKind, format_location};
use crate::jsstr::JsStr;
use crate::parser::ast::*;

/// The tool paths `code` calls, or why it cannot be scanned.
///
/// TS's walks recurse on the JS stack, so a tree nested deeper than it holds (about 9,100 levels, as in a chain of
/// that many `.b`s; the parser itself gives up sooner on nested brackets) makes `scanTools` throw a RangeError
/// instead of returning. Here that is `Err` with the execution's nesting diagnostic (`ExecutionFailure`, "Execution
/// exceeded the maximum nesting depth.").
pub fn scan_tools(code: &str) -> Result<Vec<String>, Diagnostic> {
    if crate::api::js_trim(&JsStr::from(code)).is_empty() {
        return Err(Diagnostic::new(DiagnosticKind::ParseError, "Code cannot be empty."));
    }
    let program = crate::api::parse_program(code)?;
    let result = stacker::grow(crate::api::PARSER_STACK, || {
        let top = collect_scope(&program.body, true, 0).map_err(|TooDeep| too_deep())?;
        let mut scan = Scan { found: Vec::new(), scopes: vec![top], ancestors: Vec::new() };
        match scan.visit(Node::Program(&program)) {
            Some(Stop::Invalid(diagnostic)) => Err(diagnostic),
            Some(Stop::TooDeep) => Err(too_deep()),
            None => Ok(scan.found),
        }
    });
    crate::api::drop_program(program);
    result
}

/// How deep TS's walks reach before JavaScriptCore runs out of stack, in nodes on the current path; the walk over
/// `var`s and the main visit cost about the same per level, and add up when one runs inside the other.
const MAX_DEPTH: usize = 9_120;

/// The walk went deeper than TS's could.
struct TooDeep;

enum Stop {
    Invalid(Diagnostic),
    TooDeep,
}

fn too_deep() -> Diagnostic {
    Diagnostic::new(DiagnosticKind::ExecutionFailure, "Execution exceeded the maximum nesting depth.")
}

const DYNAMIC: &str =
    "Schedule checks must reference tools statically (e.g. tools.slack.read_thread), not with dynamic bracket expressions.";
const INDIRECT: &str = "Schedule checks must call tools directly as tools.<name>(...), not alias or pass the tools object.";

/// One node of acorn's tree, whatever Rust type holds it.
#[derive(Clone, Copy)]
enum Node<'a> {
    Program(&'a Program),
    Stmt(&'a Stmt),
    Block(&'a BlockStatement),
    SwitchCase(&'a SwitchCase),
    Catch(&'a CatchClause),
    VarDecl(&'a VariableDeclaration),
    Declarator(&'a VariableDeclarator),
    Function(&'a Function),
    Class(&'a Class),
    ClassBody(&'a ClassBody),
    ClassElement(&'a ClassElement),
    ImportSpecifier(&'a ImportSpecifierKind),
    ImportAttribute(&'a ImportAttribute),
    ExportSpecifier(&'a ExportSpecifier),
    ModuleName(&'a ModuleName),
    Ident(&'a Ident),
    /// A literal (no children).
    Literal,
    Expr(&'a Expr),
    Spread(&'a SpreadElement),
    Property(&'a Property),
    Template(&'a TemplateLiteral),
    TemplateElement,
    Pat(&'a Pat),
    PatternProperty(&'a AssignmentProperty),
    Rest(&'a RestElement),
}

/// The property of its parent a node was reached through, where the scan cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Object,
    Property,
    Key,
    Label,
    Id,
    Callee,
    Expression,
    Other,
}

fn stmt_node(stmt: &Stmt) -> Node<'_> {
    match stmt {
        Stmt::Block(block) => Node::Block(block),
        Stmt::Function(function) => Node::Function(function),
        Stmt::Variable(declaration) => Node::VarDecl(declaration),
        Stmt::Class(class) => Node::Class(class),
        other => Node::Stmt(other),
    }
}

fn expr_node(expr: &Expr) -> Node<'_> {
    match expr {
        Expr::Identifier(ident) => Node::Ident(ident),
        Expr::Literal(_) => Node::Literal,
        Expr::Function(function) => Node::Function(function),
        Expr::Class(class) => Node::Class(class),
        Expr::Template(template) => Node::Template(template),
        other => Node::Expr(other),
    }
}

fn pat_node(pat: &Pat) -> Node<'_> {
    match pat {
        Pat::Identifier(ident) => Node::Ident(ident),
        Pat::Rest(rest) => Node::Rest(rest),
        other => Node::Pat(other),
    }
}

fn spread_node(item: &ExprOrSpread) -> Node<'_> {
    match item {
        ExprOrSpread::Expr(expr) => expr_node(expr),
        ExprOrSpread::Spread(spread) => Node::Spread(spread),
    }
}

impl<'a> Node<'a> {
    /// The node's children in acorn's property order, with the property that holds each.
    fn children(self) -> Vec<(Field, Node<'a>)> {
        use Field::*;
        let mut out: Vec<(Field, Node<'a>)> = Vec::new();
        let stmts = |out: &mut Vec<(Field, Node<'a>)>, body: &'a [Stmt]| out.extend(body.iter().map(|s| (Other, stmt_node(s))));
        match self {
            Node::Program(program) => stmts(&mut out, &program.body),
            Node::Block(block) => stmts(&mut out, &block.body),
            Node::Stmt(stmt) => match stmt {
                Stmt::Expression(node) => out.push((Expression, expr_node(&node.expression))),
                Stmt::Empty(_) | Stmt::Debugger(_) => {}
                Stmt::With(node) => {
                    out.push((Object, expr_node(&node.object)));
                    out.push((Other, stmt_node(&node.body)));
                }
                Stmt::Return(node) => out.extend(node.argument.iter().map(|e| (Other, expr_node(e)))),
                Stmt::Labeled(node) => {
                    out.push((Other, stmt_node(&node.body)));
                    out.push((Label, Node::Ident(&node.label)));
                }
                Stmt::Break(node) => out.extend(node.label.iter().map(|l| (Label, Node::Ident(l)))),
                Stmt::Continue(node) => out.extend(node.label.iter().map(|l| (Label, Node::Ident(l)))),
                Stmt::If(node) => {
                    out.push((Other, expr_node(&node.test)));
                    out.push((Other, stmt_node(&node.consequent)));
                    out.extend(node.alternate.iter().map(|s| (Other, stmt_node(s))));
                }
                Stmt::Switch(node) => {
                    out.push((Other, expr_node(&node.discriminant)));
                    out.extend(node.cases.iter().map(|c| (Other, Node::SwitchCase(c))));
                }
                Stmt::Throw(node) => out.push((Other, expr_node(&node.argument))),
                Stmt::Try(node) => {
                    out.push((Other, Node::Block(&node.block)));
                    out.extend(node.handler.iter().map(|h| (Other, Node::Catch(h))));
                    out.extend(node.finalizer.iter().map(|f| (Other, Node::Block(f))));
                }
                Stmt::While(node) => {
                    out.push((Other, expr_node(&node.test)));
                    out.push((Other, stmt_node(&node.body)));
                }
                Stmt::DoWhile(node) => {
                    out.push((Other, stmt_node(&node.body)));
                    out.push((Other, expr_node(&node.test)));
                }
                Stmt::For(node) => {
                    match &node.init {
                        Some(ForInit::Variable(declaration)) => out.push((Other, Node::VarDecl(declaration))),
                        Some(ForInit::Expr(expr)) => out.push((Other, expr_node(expr))),
                        None => {}
                    }
                    out.extend(node.test.iter().map(|e| (Other, expr_node(e))));
                    out.extend(node.update.iter().map(|e| (Other, expr_node(e))));
                    out.push((Other, stmt_node(&node.body)));
                }
                Stmt::ForIn(node) => {
                    out.push((Other, head_node(&node.left)));
                    out.push((Other, expr_node(&node.right)));
                    out.push((Other, stmt_node(&node.body)));
                }
                Stmt::ForOf(node) => {
                    out.push((Other, head_node(&node.left)));
                    out.push((Other, expr_node(&node.right)));
                    out.push((Other, stmt_node(&node.body)));
                }
                Stmt::Import(node) => {
                    out.extend(node.specifiers.iter().map(|s| (Other, Node::ImportSpecifier(s))));
                    out.push((Other, Node::Literal));
                    out.extend(node.attributes.iter().map(|a| (Other, Node::ImportAttribute(a))));
                }
                Stmt::ExportNamed(node) => {
                    out.extend(node.declaration.iter().map(|d| (Other, stmt_node(d))));
                    out.extend(node.specifiers.iter().map(|s| (Other, Node::ExportSpecifier(s))));
                    out.extend(node.source.iter().map(|_| (Other, Node::Literal)));
                    out.extend(node.attributes.iter().map(|a| (Other, Node::ImportAttribute(a))));
                }
                Stmt::ExportDefault(node) => match &node.declaration {
                    ExportDefaultKind::Declaration(stmt) => out.push((Other, stmt_node(stmt))),
                    ExportDefaultKind::Expr(expr) => out.push((Other, expr_node(expr))),
                },
                Stmt::ExportAll(node) => {
                    out.extend(node.exported.iter().map(|n| (Other, Node::ModuleName(n))));
                    out.push((Other, Node::Literal));
                    out.extend(node.attributes.iter().map(|a| (Other, Node::ImportAttribute(a))));
                }
                Stmt::Block(_) | Stmt::Function(_) | Stmt::Variable(_) | Stmt::Class(_) => unreachable!("normalized"),
            },
            Node::SwitchCase(case) => {
                stmts(&mut out, &case.consequent);
                out.extend(case.test.iter().map(|e| (Other, expr_node(e))));
            }
            Node::Catch(clause) => {
                out.extend(clause.param.iter().map(|p| (Other, pat_node(p))));
                out.push((Other, Node::Block(&clause.body)));
            }
            Node::VarDecl(declaration) => out.extend(declaration.declarations.iter().map(|d| (Other, Node::Declarator(d)))),
            Node::Declarator(declarator) => {
                out.push((Id, pat_node(&declarator.id)));
                out.extend(declarator.init.iter().map(|e| (Other, expr_node(e))));
            }
            Node::Function(function) => {
                out.extend(function.id.iter().map(|id| (Id, Node::Ident(id))));
                out.extend(function.params.iter().map(|p| (Other, pat_node(p))));
                match &function.body {
                    FunctionBody::Block(block) => out.push((Other, Node::Block(block))),
                    FunctionBody::Expr(expr) => out.push((Other, expr_node(expr))),
                }
            }
            Node::Class(class) => {
                out.extend(class.id.iter().map(|id| (Id, Node::Ident(id))));
                out.extend(class.super_class.iter().map(|e| (Other, expr_node(e))));
                out.push((Other, Node::ClassBody(&class.body)));
            }
            Node::ClassBody(body) => out.extend(body.body.iter().map(|e| (Other, Node::ClassElement(e)))),
            Node::ClassElement(element) => match element {
                ClassElement::Method(method) => {
                    out.push((Key, expr_node(&method.key)));
                    out.push((Other, Node::Function(&method.value)));
                }
                ClassElement::Property(property) => {
                    out.push((Key, expr_node(&property.key)));
                    out.extend(property.value.iter().map(|e| (Other, expr_node(e))));
                }
                ClassElement::StaticBlock(block) => stmts(&mut out, &block.body),
            },
            Node::ImportSpecifier(specifier) => match specifier {
                ImportSpecifierKind::Named { imported, local, .. } => {
                    out.push((Other, Node::ModuleName(imported)));
                    out.push((Other, Node::Ident(local)));
                }
                ImportSpecifierKind::Default { local, .. } | ImportSpecifierKind::Namespace { local, .. } => {
                    out.push((Other, Node::Ident(local)))
                }
            },
            Node::ImportAttribute(attribute) => {
                out.push((Key, Node::ModuleName(&attribute.key)));
                out.push((Other, Node::Literal));
            }
            Node::ExportSpecifier(specifier) => {
                out.push((Other, Node::ModuleName(&specifier.local)));
                out.push((Other, Node::ModuleName(&specifier.exported)));
            }
            Node::ModuleName(name) => match name {
                // Stands for the identifier or literal itself; see `visit`.
                ModuleName::Identifier(_) | ModuleName::Literal(_) => {}
            },
            Node::Ident(_) | Node::Literal | Node::TemplateElement => {}
            Node::Expr(expr) => match expr {
                Expr::PrivateIdentifier(_) | Expr::This(_) | Expr::Super(_) => {}
                Expr::Array(node) => out.extend(node.elements.iter().flatten().map(|e| (Other, spread_node(e)))),
                Expr::Object(node) => out.extend(node.properties.iter().map(|member| match member {
                    ObjectMember::Property(property) => (Other, Node::Property(property)),
                    ObjectMember::Spread(spread) => (Other, Node::Spread(spread)),
                })),
                Expr::Unary(node) => out.push((Other, expr_node(&node.argument))),
                Expr::Update(node) => out.push((Other, expr_node(&node.argument))),
                Expr::Binary(node) => {
                    out.push((Other, expr_node(&node.left)));
                    out.push((Other, expr_node(&node.right)));
                }
                Expr::Logical(node) => {
                    out.push((Other, expr_node(&node.left)));
                    out.push((Other, expr_node(&node.right)));
                }
                Expr::Assignment(node) => {
                    out.push((Other, pat_node(&node.left)));
                    out.push((Other, expr_node(&node.right)));
                }
                Expr::Member(node) => member_children(&mut out, node),
                Expr::Conditional(node) => {
                    out.push((Other, expr_node(&node.test)));
                    out.push((Other, expr_node(&node.consequent)));
                    out.push((Other, expr_node(&node.alternate)));
                }
                Expr::Call(node) => {
                    out.push((Callee, expr_node(&node.callee)));
                    out.extend(node.arguments.iter().map(|a| (Other, spread_node(a))));
                }
                Expr::New(node) => {
                    out.push((Callee, expr_node(&node.callee)));
                    out.extend(node.arguments.iter().map(|a| (Other, spread_node(a))));
                }
                Expr::Sequence(node) => out.extend(node.expressions.iter().map(|e| (Other, expr_node(e)))),
                Expr::Yield(node) => out.extend(node.argument.iter().map(|e| (Other, expr_node(e)))),
                Expr::Await(node) => out.push((Other, expr_node(&node.argument))),
                Expr::TaggedTemplate(node) => {
                    out.push((Other, expr_node(&node.tag)));
                    out.push((Other, Node::Template(&node.quasi)));
                }
                Expr::Chain(node) => out.push((Expression, expr_node(&node.expression))),
                Expr::MetaProperty(node) => {
                    out.push((Other, Node::Ident(&node.meta)));
                    out.push((Property, Node::Ident(&node.property)));
                }
                Expr::Import(node) => {
                    out.push((Other, expr_node(&node.source)));
                    out.extend(node.options.iter().map(|e| (Other, expr_node(e))));
                }
                Expr::Identifier(_) | Expr::Literal(_) | Expr::Function(_) | Expr::Class(_) | Expr::Template(_) => {
                    unreachable!("normalized")
                }
            },
            Node::Spread(spread) => out.push((Other, expr_node(&spread.argument))),
            Node::Property(property) => {
                out.push((Key, expr_node(&property.key)));
                out.push((Other, expr_node(&property.value)));
            }
            Node::Template(template) => {
                out.extend(template.expressions.iter().map(|e| (Other, expr_node(e))));
                out.extend(template.quasis.iter().map(|_| (Other, Node::TemplateElement)));
            }
            Node::Pat(pat) => match pat {
                Pat::Member(node) => member_children(&mut out, node),
                Pat::Object(node) => out.extend(node.properties.iter().map(|member| match member {
                    ObjectPatternMember::Property(property) => (Other, Node::PatternProperty(property)),
                    ObjectPatternMember::Rest(rest) => (Other, Node::Rest(rest)),
                })),
                Pat::Array(node) => out.extend(node.elements.iter().flatten().map(|p| (Other, pat_node(p)))),
                Pat::Assignment(node) => {
                    out.push((Other, pat_node(&node.left)));
                    out.push((Other, expr_node(&node.right)));
                }
                Pat::Identifier(_) | Pat::Rest(_) => unreachable!("normalized"),
            },
            Node::PatternProperty(property) => {
                out.push((Key, expr_node(&property.key)));
                out.push((Other, pat_node(&property.value)));
            }
            Node::Rest(rest) => out.push((Other, pat_node(&rest.argument))),
        }
        out
    }
}

fn head_node(head: &ForHead) -> Node<'_> {
    match head {
        ForHead::Variable(declaration) => Node::VarDecl(declaration),
        ForHead::Pattern(pat) => pat_node(pat),
    }
}

fn member_children<'a>(out: &mut Vec<(Field, Node<'a>)>, node: &'a MemberExpression) {
    out.push((Field::Object, expr_node(&node.object)));
    out.push((Field::Property, expr_node(&node.property)));
}

/// A node on the path from the root, with the property its next node on the path was reached through.
struct Ancestor<'a> {
    node: Node<'a>,
    field: Field,
}

struct Scan<'a> {
    found: Vec<String>,
    /// Per open scope: whether it declares `tools`.
    scopes: Vec<bool>,
    ancestors: Vec<Ancestor<'a>>,
}

fn fail(span: Span, message: &str) -> Diagnostic {
    let location = Loc::of(span);
    Diagnostic {
        kind: DiagnosticKind::UnsupportedSyntax,
        message: JsStr::from(format!("{message}{}", format_location(Some(location)))),
        location: Some(location),
        suggestions: None,
    }
}

impl<'a> Scan<'a> {
    fn visit(&mut self, node: Node<'a>) -> Option<Stop> {
        // The path so far, plus this node.
        if self.ancestors.len() + 1 > MAX_DEPTH {
            return Some(Stop::TooDeep);
        }
        stacker::maybe_grow(64 * 1024, 1024 * 1024, || self.visit_inner(node))
    }

    fn visit_inner(&mut self, node: Node<'a>) -> Option<Stop> {
        // An import or export name is its identifier or literal.
        let node = match node {
            Node::ModuleName(ModuleName::Identifier(ident)) => Node::Ident(ident),
            Node::ModuleName(ModuleName::Literal(_)) => Node::Literal,
            other => other,
        };
        if let Node::Ident(ident) = node
            && &*ident.name == "tools"
            && !self.scopes.iter().any(|declares| *declares)
        {
            if !self.is_reference_position() {
                return None;
            }
            return match self.read_static_tool_call(ident.span) {
                Ok(path) => {
                    if !self.found.contains(&path) {
                        self.found.push(path);
                    }
                    None
                }
                Err(diagnostic) => Some(Stop::Invalid(diagnostic)),
            };
        }
        let scope = match enter_node_scope(node, self.ancestors.len() + 1) {
            Ok(scope) => scope,
            Err(TooDeep) => return Some(Stop::TooDeep),
        };
        if let Some(declares) = scope {
            self.scopes.push(declares);
        }
        self.ancestors.push(Ancestor { node, field: Field::Other });
        let mut error = None;
        for (field, child) in node.children() {
            self.ancestors.last_mut().unwrap().field = field;
            if let Some(diagnostic) = self.visit(child) {
                error = Some(diagnostic);
                break;
            }
        }
        self.ancestors.pop();
        if scope.is_some() {
            self.scopes.pop();
        }
        error
    }

    fn is_reference_position(&self) -> bool {
        let Some(parent) = self.ancestors.last() else { return true };
        let field = parent.field;
        match parent.node {
            Node::Expr(Expr::Member(member)) | Node::Pat(Pat::Member(member)) => !(field == Field::Property && !member.computed),
            Node::Property(property) => !(field == Field::Key && !property.computed && !property.shorthand),
            Node::PatternProperty(property) => !(field == Field::Key && !property.computed && !property.shorthand),
            Node::ClassElement(ClassElement::Method(method)) => !(field == Field::Key && !method.computed),
            Node::Stmt(Stmt::Labeled(_) | Stmt::Break(_) | Stmt::Continue(_)) => field != Field::Label,
            // Arrow functions have no `id`.
            Node::Function(_) | Node::Class(_) | Node::Declarator(_) => field != Field::Id,
            _ => true,
        }
    }

    fn read_static_tool_call(&self, root: Span) -> Result<String, Diagnostic> {
        let mut segments: Vec<String> = Vec::new();
        let mut current = root;
        let mut index = self.ancestors.len();
        while index > 0 {
            let parent = &self.ancestors[index - 1];
            if let Node::Expr(Expr::Chain(chain)) = parent.node
                && parent.field == Field::Expression
            {
                current = chain.span;
                index -= 1;
                continue;
            }
            let member = match parent.node {
                Node::Expr(Expr::Member(member)) | Node::Pat(Pat::Member(member)) if parent.field == Field::Object => member,
                _ => break,
            };
            match (&member.property, member.computed) {
                (Expr::Identifier(ident), false) => segments.push(ident.name.to_string()),
                (Expr::Literal(literal), true) if matches!(literal.value, LitValue::String(_)) => {
                    let LitValue::String(text) = &literal.value else { unreachable!() };
                    segments.push(text.to_rust_lossy());
                }
                (property, _) => return Err(fail(property.span(), DYNAMIC)),
            }
            current = member.span;
            index -= 1;
        }
        let call = index.checked_sub(1).map(|at| &self.ancestors[at]);
        let called = matches!(call, Some(Ancestor { node: Node::Expr(Expr::Call(_)), field: Field::Callee }));
        if segments.is_empty() || !called {
            return Err(fail(current, INDIRECT));
        }
        Ok(segments.join("."))
    }
}

/// The scope a node opens, as whether it declares `tools`; `depth` is the node's own on the visit's path.
fn enter_node_scope(node: Node<'_>, depth: usize) -> Result<Option<bool>, TooDeep> {
    Ok(match node {
        Node::Function(function) => {
            let mut declares = function.id.as_ref().is_some_and(|id| &*id.name == "tools");
            declares |= function.params.iter().any(pattern_declares);
            if let FunctionBody::Block(block) = &function.body {
                declares |= collect_scope(&block.body, true, depth)?;
            }
            Some(declares)
        }
        Node::Block(block) => Some(collect_scope(&block.body, false, depth)?),
        Node::Catch(clause) => Some(clause.param.as_ref().is_some_and(pattern_declares)),
        Node::Stmt(Stmt::For(node)) => match &node.init {
            Some(ForInit::Variable(declaration)) if declaration.kind != VarKind::Var => Some(declarator_declares(declaration)),
            _ => None,
        },
        Node::Stmt(Stmt::ForIn(node)) => head_scope(&node.left),
        Node::Stmt(Stmt::ForOf(node)) => head_scope(&node.left),
        _ => None,
    })
}

fn head_scope(head: &ForHead) -> Option<bool> {
    match head {
        ForHead::Variable(declaration) if declaration.kind != VarKind::Var => Some(declarator_declares(declaration)),
        _ => None,
    }
}

fn declarator_declares(declaration: &VariableDeclaration) -> bool {
    declaration.declarations.iter().any(|d| pattern_declares(&d.id))
}

/// Whether a program or block declares `tools`; `depth` is where the walk over nested `var`s starts.
fn collect_scope(body: &[Stmt], include_var: bool, depth: usize) -> Result<bool, TooDeep> {
    let mut declares = false;
    for statement in body {
        match statement {
            Stmt::Function(function) => declares |= function.id.as_ref().is_some_and(|id| &*id.name == "tools"),
            Stmt::Class(class) => declares |= class.id.as_ref().is_some_and(|id| &*id.name == "tools"),
            Stmt::Variable(declaration) if include_var || declaration.kind != VarKind::Var => {
                declares |= declarator_declares(declaration)
            }
            _ => {}
        }
    }
    if include_var {
        // TS walks every statement, even after finding one.
        for statement in body {
            declares |= nested_vars_declare(stmt_node(statement), depth + 1)?;
        }
    }
    Ok(declares)
}

/// Whether a `var` anywhere under `node`, outside nested functions, declares `tools`; `depth` is the node's own.
fn nested_vars_declare(node: Node<'_>, depth: usize) -> Result<bool, TooDeep> {
    if depth > MAX_DEPTH {
        return Err(TooDeep);
    }
    stacker::maybe_grow(64 * 1024, 1024 * 1024, || match node {
        Node::Function(_) => Ok(false),
        other => {
            let mut declares = matches!(other, Node::VarDecl(declaration)
                if declaration.kind == VarKind::Var && declarator_declares(declaration));
            for (_, child) in other.children() {
                declares |= nested_vars_declare(child, depth + 1)?;
            }
            Ok(declares)
        }
    })
}

fn pattern_declares(pattern: &Pat) -> bool {
    match pattern {
        Pat::Identifier(ident) => &*ident.name == "tools",
        Pat::Assignment(node) => pattern_declares(&node.left),
        Pat::Rest(node) => pattern_declares(&node.argument),
        Pat::Array(node) => node.elements.iter().flatten().any(pattern_declares),
        Pat::Object(node) => node.properties.iter().any(|member| match member {
            ObjectPatternMember::Rest(rest) => pattern_declares(&rest.argument),
            ObjectPatternMember::Property(property) => pattern_declares(&property.value),
        }),
        Pat::Member(_) => false,
    }
}
