//! The syntax tree acorn builds (ESTree, `ecmaVersion: "latest"`, `locations: true`), as Rust types. Every node keeps
//! acorn's `start`/`end` offsets and `loc` positions, all counted in UTF-16 code units of the source like acorn's
//! (lines 1-based, columns 0-based). Field names follow ESTree; `estree.rs` writes a tree as acorn's JSON.
//!
//! Functions are shared (`Rc`) so a closure keeps the node it was created from.

use crate::jsstr::JsStr;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub loc_start: Position,
    pub loc_end: Position,
}

#[derive(Debug)]
pub struct Program {
    pub span: Span,
    pub body: Vec<Stmt>,
    // sourceType is always "script".
}

#[derive(Debug, Clone)]
pub struct Ident {
    pub span: Span,
    pub name: Rc<str>,
    /// `name` as a JS string, for property access.
    pub key: JsStr,
}

#[derive(Debug)]
pub struct PrivateIdent {
    pub span: Span,
    pub name: Rc<str>,
}

#[derive(Debug, Clone)]
pub enum LitValue {
    String(JsStr),
    Number(f64),
    Boolean(bool),
    Null,
    /// `value` is a RegExp, or null when the host cannot construct it; acorn sets `regex: { pattern, flags }`.
    RegExp { pattern: Rc<str>, flags: Rc<str> },
    /// `value` is a BigInt; acorn sets `bigint` to the digits (decimal form for non-decimal literals).
    BigInt(Rc<str>),
}

#[derive(Debug)]
pub struct Literal {
    pub span: Span,
    pub value: LitValue,
    /// The source text of the literal.
    pub raw: Rc<str>,
}

#[derive(Debug)]
pub enum Stmt {
    Expression(Box<ExpressionStatement>),
    Block(Box<BlockStatement>),
    Empty(Span),
    Debugger(Span),
    With(Box<WithStatement>),
    Return(Box<ReturnStatement>),
    Labeled(Box<LabeledStatement>),
    Break(Box<BreakStatement>),
    Continue(Box<ContinueStatement>),
    If(Box<IfStatement>),
    Switch(Box<SwitchStatement>),
    Throw(Box<ThrowStatement>),
    Try(Box<TryStatement>),
    While(Box<WhileStatement>),
    DoWhile(Box<DoWhileStatement>),
    For(Box<ForStatement>),
    ForIn(Box<ForInStatement>),
    ForOf(Box<ForOfStatement>),
    /// `FunctionDeclaration`
    Function(Rc<Function>),
    Variable(Box<VariableDeclaration>),
    Class(Box<Class>),
    Import(Box<ImportDeclaration>),
    ExportNamed(Box<ExportNamedDeclaration>),
    ExportDefault(Box<ExportDefaultDeclaration>),
    ExportAll(Box<ExportAllDeclaration>),
}

#[derive(Debug)]
pub struct ExpressionStatement {
    pub span: Span,
    pub expression: Expr,
    /// Set on the directive prologue (`"use strict"`): the raw string without quotes.
    pub directive: Option<Rc<str>>,
}

#[derive(Debug)]
pub struct BlockStatement {
    pub span: Span,
    pub body: Vec<Stmt>,
}

#[derive(Debug)]
pub struct WithStatement {
    pub span: Span,
    pub object: Expr,
    pub body: Stmt,
}

#[derive(Debug)]
pub struct ReturnStatement {
    pub span: Span,
    pub argument: Option<Expr>,
}

#[derive(Debug)]
pub struct LabeledStatement {
    pub span: Span,
    pub label: Ident,
    pub body: Stmt,
}

#[derive(Debug)]
pub struct BreakStatement {
    pub span: Span,
    pub label: Option<Ident>,
}

#[derive(Debug)]
pub struct ContinueStatement {
    pub span: Span,
    pub label: Option<Ident>,
}

#[derive(Debug)]
pub struct IfStatement {
    pub span: Span,
    pub test: Expr,
    pub consequent: Stmt,
    pub alternate: Option<Stmt>,
}

#[derive(Debug)]
pub struct SwitchStatement {
    pub span: Span,
    pub discriminant: Expr,
    pub cases: Vec<SwitchCase>,
}

#[derive(Debug)]
pub struct SwitchCase {
    pub span: Span,
    pub test: Option<Expr>,
    pub consequent: Vec<Stmt>,
}

#[derive(Debug)]
pub struct ThrowStatement {
    pub span: Span,
    pub argument: Expr,
}

#[derive(Debug)]
pub struct TryStatement {
    pub span: Span,
    pub block: BlockStatement,
    pub handler: Option<CatchClause>,
    pub finalizer: Option<BlockStatement>,
}

#[derive(Debug)]
pub struct CatchClause {
    pub span: Span,
    pub param: Option<Pat>,
    pub body: BlockStatement,
}

#[derive(Debug)]
pub struct WhileStatement {
    pub span: Span,
    pub test: Expr,
    pub body: Stmt,
}

#[derive(Debug)]
pub struct DoWhileStatement {
    pub span: Span,
    pub body: Stmt,
    pub test: Expr,
}

#[derive(Debug)]
pub enum ForInit {
    Variable(Box<VariableDeclaration>),
    Expr(Expr),
}

#[derive(Debug)]
pub struct ForStatement {
    pub span: Span,
    pub init: Option<ForInit>,
    pub test: Option<Expr>,
    pub update: Option<Expr>,
    pub body: Stmt,
}

/// The left side of `for...in` / `for...of`.
#[derive(Debug)]
pub enum ForHead {
    Variable(Box<VariableDeclaration>),
    Pattern(Pat),
}

#[derive(Debug)]
pub struct ForInStatement {
    pub span: Span,
    pub left: ForHead,
    pub right: Expr,
    pub body: Stmt,
}

#[derive(Debug)]
pub struct ForOfStatement {
    pub span: Span,
    pub left: ForHead,
    pub right: Expr,
    pub body: Stmt,
    pub is_await: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarKind {
    Var,
    Let,
    Const,
    Using,
    AwaitUsing,
}

impl VarKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VarKind::Var => "var",
            VarKind::Let => "let",
            VarKind::Const => "const",
            VarKind::Using => "using",
            VarKind::AwaitUsing => "await using",
        }
    }
}

#[derive(Debug)]
pub struct VariableDeclaration {
    pub span: Span,
    pub kind: VarKind,
    pub declarations: Vec<VariableDeclarator>,
}

#[derive(Debug)]
pub struct VariableDeclarator {
    pub span: Span,
    pub id: Pat,
    pub init: Option<Expr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionKind {
    Declaration,
    Expression,
    Arrow,
}

#[derive(Debug)]
pub enum FunctionBody {
    Block(BlockStatement),
    /// An arrow function's expression body (`expression: true`).
    Expr(Expr),
}

/// `FunctionDeclaration`, `FunctionExpression`, or `ArrowFunctionExpression`.
#[derive(Debug)]
pub struct Function {
    pub span: Span,
    pub kind: FunctionKind,
    pub id: Option<Ident>,
    pub params: Vec<Pat>,
    pub body: FunctionBody,
    pub generator: bool,
    pub is_async: bool,
}

#[derive(Debug)]
pub enum Expr {
    Identifier(Box<Ident>),
    PrivateIdentifier(Box<PrivateIdent>),
    Literal(Box<Literal>),
    This(Span),
    Super(Span),
    Array(Box<ArrayExpression>),
    Object(Box<ObjectExpression>),
    /// `FunctionExpression` or `ArrowFunctionExpression`
    Function(Rc<Function>),
    Class(Box<Class>),
    Unary(Box<UnaryExpression>),
    Update(Box<UpdateExpression>),
    Binary(Box<BinaryExpression>),
    Logical(Box<LogicalExpression>),
    Assignment(Box<AssignmentExpression>),
    Member(Box<MemberExpression>),
    Conditional(Box<ConditionalExpression>),
    Call(Box<CallExpression>),
    New(Box<NewExpression>),
    Sequence(Box<SequenceExpression>),
    Yield(Box<YieldExpression>),
    Await(Box<AwaitExpression>),
    Template(Box<TemplateLiteral>),
    TaggedTemplate(Box<TaggedTemplateExpression>),
    Chain(Box<ChainExpression>),
    MetaProperty(Box<MetaProperty>),
    Import(Box<ImportExpression>),
}

/// An array element or call argument: an expression or `...spread`. Holes are `None` in the owning vector.
#[derive(Debug)]
pub enum ExprOrSpread {
    Expr(Expr),
    Spread(Box<SpreadElement>),
}

#[derive(Debug)]
pub struct SpreadElement {
    pub span: Span,
    pub argument: Expr,
}

#[derive(Debug)]
pub struct ArrayExpression {
    pub span: Span,
    pub elements: Vec<Option<ExprOrSpread>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropKind {
    Init,
    Get,
    Set,
}

#[derive(Debug)]
pub enum ObjectMember {
    Property(Box<Property>),
    Spread(Box<SpreadElement>),
}

#[derive(Debug)]
pub struct ObjectExpression {
    pub span: Span,
    pub properties: Vec<ObjectMember>,
}

/// A property of an object literal. `key` is an `Identifier`, `Literal`, or (when `computed`) any expression;
/// methods, getters, and setters have a `FunctionExpression` value.
#[derive(Debug)]
pub struct Property {
    pub span: Span,
    pub key: Expr,
    pub value: Expr,
    pub kind: PropKind,
    pub method: bool,
    pub shorthand: bool,
    pub computed: bool,
}

#[derive(Debug)]
pub struct UnaryExpression {
    pub span: Span,
    /// `-`, `+`, `!`, `~`, `typeof`, `void`, `delete`
    pub operator: &'static str,
    pub argument: Expr,
    // prefix is always true
}

#[derive(Debug)]
pub struct UpdateExpression {
    pub span: Span,
    /// `++` or `--`
    pub operator: &'static str,
    pub prefix: bool,
    pub argument: Expr,
}

#[derive(Debug)]
pub struct BinaryExpression {
    pub span: Span,
    pub operator: &'static str,
    /// A `PrivateIdentifier` only in `#x in obj`.
    pub left: Expr,
    pub right: Expr,
}

#[derive(Debug)]
pub struct LogicalExpression {
    pub span: Span,
    /// `&&`, `||`, `??`
    pub operator: &'static str,
    pub left: Expr,
    pub right: Expr,
}

#[derive(Debug)]
pub struct AssignmentExpression {
    pub span: Span,
    pub operator: &'static str,
    pub left: Pat,
    pub right: Expr,
}

#[derive(Debug)]
pub struct MemberExpression {
    pub span: Span,
    /// May be `Super`.
    pub object: Expr,
    /// An `Identifier` or `PrivateIdentifier` unless `computed`.
    pub property: Expr,
    pub computed: bool,
    pub optional: bool,
}

#[derive(Debug)]
pub struct ConditionalExpression {
    pub span: Span,
    pub test: Expr,
    pub consequent: Expr,
    pub alternate: Expr,
}

#[derive(Debug)]
pub struct CallExpression {
    pub span: Span,
    /// May be `Super`.
    pub callee: Expr,
    pub arguments: Vec<ExprOrSpread>,
    pub optional: bool,
}

#[derive(Debug)]
pub struct NewExpression {
    pub span: Span,
    pub callee: Expr,
    pub arguments: Vec<ExprOrSpread>,
}

#[derive(Debug)]
pub struct SequenceExpression {
    pub span: Span,
    pub expressions: Vec<Expr>,
}

#[derive(Debug)]
pub struct YieldExpression {
    pub span: Span,
    pub argument: Option<Expr>,
    pub delegate: bool,
}

#[derive(Debug)]
pub struct AwaitExpression {
    pub span: Span,
    pub argument: Expr,
}

#[derive(Debug)]
pub struct TemplateElement {
    pub span: Span,
    /// None when the text holds an invalid escape (allowed only in a tagged template).
    pub cooked: Option<JsStr>,
    pub raw: JsStr,
    pub tail: bool,
}

#[derive(Debug)]
pub struct TemplateLiteral {
    pub span: Span,
    pub expressions: Vec<Expr>,
    pub quasis: Vec<TemplateElement>,
}

#[derive(Debug)]
pub struct TaggedTemplateExpression {
    pub span: Span,
    pub tag: Expr,
    pub quasi: TemplateLiteral,
}

#[derive(Debug)]
pub struct ChainExpression {
    pub span: Span,
    /// A `CallExpression` or `MemberExpression`.
    pub expression: Expr,
}

#[derive(Debug)]
pub struct MetaProperty {
    pub span: Span,
    pub meta: Ident,
    pub property: Ident,
}

#[derive(Debug)]
pub struct ImportExpression {
    pub span: Span,
    pub source: Expr,
    pub options: Option<Expr>,
}

/// `ClassDeclaration` (`is_declaration`) or `ClassExpression`.
#[derive(Debug)]
pub struct Class {
    pub span: Span,
    pub is_declaration: bool,
    pub id: Option<Ident>,
    pub super_class: Option<Expr>,
    pub body: ClassBody,
}

#[derive(Debug)]
pub struct ClassBody {
    pub span: Span,
    pub body: Vec<ClassElement>,
}

#[derive(Debug)]
pub enum ClassElement {
    Method(Box<MethodDefinition>),
    Property(Box<PropertyDefinition>),
    StaticBlock(Box<StaticBlock>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    Constructor,
    Method,
    Get,
    Set,
}

#[derive(Debug)]
pub struct MethodDefinition {
    pub span: Span,
    pub key: Expr,
    pub value: Rc<Function>,
    pub kind: MethodKind,
    pub computed: bool,
    pub is_static: bool,
}

#[derive(Debug)]
pub struct PropertyDefinition {
    pub span: Span,
    pub key: Expr,
    pub value: Option<Expr>,
    pub computed: bool,
    pub is_static: bool,
}

#[derive(Debug)]
pub struct StaticBlock {
    pub span: Span,
    pub body: Vec<Stmt>,
}

/// A binding or assignment target.
#[derive(Debug)]
pub enum Pat {
    Identifier(Box<Ident>),
    /// Only as an assignment target (`a.b = 1`, `[a.b] = []`, `for (a.b of c)`).
    Member(Box<MemberExpression>),
    Object(Box<ObjectPattern>),
    Array(Box<ArrayPattern>),
    Rest(Box<RestElement>),
    Assignment(Box<AssignmentPattern>),
}

#[derive(Debug)]
pub enum ObjectPatternMember {
    Property(Box<AssignmentProperty>),
    Rest(Box<RestElement>),
}

#[derive(Debug)]
pub struct ObjectPattern {
    pub span: Span,
    pub properties: Vec<ObjectPatternMember>,
}

/// A property of an object pattern (`Property` with kind "init" and a pattern value in ESTree).
#[derive(Debug)]
pub struct AssignmentProperty {
    pub span: Span,
    pub key: Expr,
    pub value: Pat,
    pub shorthand: bool,
    pub computed: bool,
}

#[derive(Debug)]
pub struct ArrayPattern {
    pub span: Span,
    pub elements: Vec<Option<Pat>>,
}

#[derive(Debug)]
pub struct RestElement {
    pub span: Span,
    pub argument: Pat,
}

#[derive(Debug)]
pub struct AssignmentPattern {
    pub span: Span,
    pub left: Pat,
    pub right: Expr,
}

#[derive(Debug)]
pub struct ImportDeclaration {
    pub span: Span,
    pub specifiers: Vec<ImportSpecifierKind>,
    pub source: Literal,
    pub attributes: Vec<ImportAttribute>,
}

/// An import or export name: an `Identifier` or a string `Literal`.
#[derive(Debug)]
pub enum ModuleName {
    Identifier(Ident),
    Literal(Literal),
}

#[derive(Debug)]
pub enum ImportSpecifierKind {
    /// `ImportSpecifier`: `{ imported as local }`
    Named { span: Span, imported: ModuleName, local: Ident },
    /// `ImportDefaultSpecifier`
    Default { span: Span, local: Ident },
    /// `ImportNamespaceSpecifier`
    Namespace { span: Span, local: Ident },
}

#[derive(Debug)]
pub struct ImportAttribute {
    pub span: Span,
    pub key: ModuleName,
    pub value: Literal,
}

#[derive(Debug)]
pub struct ExportSpecifier {
    pub span: Span,
    pub local: ModuleName,
    pub exported: ModuleName,
}

#[derive(Debug)]
pub struct ExportNamedDeclaration {
    pub span: Span,
    pub declaration: Option<Stmt>,
    pub specifiers: Vec<ExportSpecifier>,
    pub source: Option<Literal>,
    pub attributes: Vec<ImportAttribute>,
}

#[derive(Debug)]
pub enum ExportDefaultKind {
    /// A `FunctionDeclaration` or `ClassDeclaration` (possibly anonymous).
    Declaration(Stmt),
    Expr(Expr),
}

#[derive(Debug)]
pub struct ExportDefaultDeclaration {
    pub span: Span,
    pub declaration: ExportDefaultKind,
}

#[derive(Debug)]
pub struct ExportAllDeclaration {
    pub span: Span,
    pub exported: Option<ModuleName>,
    pub source: Literal,
    pub attributes: Vec<ImportAttribute>,
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Identifier(node) => node.span,
            Expr::PrivateIdentifier(node) => node.span,
            Expr::Literal(node) => node.span,
            Expr::This(span) | Expr::Super(span) => *span,
            Expr::Array(node) => node.span,
            Expr::Object(node) => node.span,
            Expr::Function(node) => node.span,
            Expr::Class(node) => node.span,
            Expr::Unary(node) => node.span,
            Expr::Update(node) => node.span,
            Expr::Binary(node) => node.span,
            Expr::Logical(node) => node.span,
            Expr::Assignment(node) => node.span,
            Expr::Member(node) => node.span,
            Expr::Conditional(node) => node.span,
            Expr::Call(node) => node.span,
            Expr::New(node) => node.span,
            Expr::Sequence(node) => node.span,
            Expr::Yield(node) => node.span,
            Expr::Await(node) => node.span,
            Expr::Template(node) => node.span,
            Expr::TaggedTemplate(node) => node.span,
            Expr::Chain(node) => node.span,
            Expr::MetaProperty(node) => node.span,
            Expr::Import(node) => node.span,
        }
    }

    /// The ESTree node type.
    pub fn type_name(&self) -> &'static str {
        match self {
            Expr::Identifier(_) => "Identifier",
            Expr::PrivateIdentifier(_) => "PrivateIdentifier",
            Expr::Literal(_) => "Literal",
            Expr::This(_) => "ThisExpression",
            Expr::Super(_) => "Super",
            Expr::Array(_) => "ArrayExpression",
            Expr::Object(_) => "ObjectExpression",
            Expr::Function(node) => match node.kind {
                FunctionKind::Arrow => "ArrowFunctionExpression",
                _ => "FunctionExpression",
            },
            Expr::Class(_) => "ClassExpression",
            Expr::Unary(_) => "UnaryExpression",
            Expr::Update(_) => "UpdateExpression",
            Expr::Binary(_) => "BinaryExpression",
            Expr::Logical(_) => "LogicalExpression",
            Expr::Assignment(_) => "AssignmentExpression",
            Expr::Member(_) => "MemberExpression",
            Expr::Conditional(_) => "ConditionalExpression",
            Expr::Call(_) => "CallExpression",
            Expr::New(_) => "NewExpression",
            Expr::Sequence(_) => "SequenceExpression",
            Expr::Yield(_) => "YieldExpression",
            Expr::Await(_) => "AwaitExpression",
            Expr::Template(_) => "TemplateLiteral",
            Expr::TaggedTemplate(_) => "TaggedTemplateExpression",
            Expr::Chain(_) => "ChainExpression",
            Expr::MetaProperty(_) => "MetaProperty",
            Expr::Import(_) => "ImportExpression",
        }
    }
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Expression(node) => node.span,
            Stmt::Block(node) => node.span,
            Stmt::Empty(span) | Stmt::Debugger(span) => *span,
            Stmt::With(node) => node.span,
            Stmt::Return(node) => node.span,
            Stmt::Labeled(node) => node.span,
            Stmt::Break(node) => node.span,
            Stmt::Continue(node) => node.span,
            Stmt::If(node) => node.span,
            Stmt::Switch(node) => node.span,
            Stmt::Throw(node) => node.span,
            Stmt::Try(node) => node.span,
            Stmt::While(node) => node.span,
            Stmt::DoWhile(node) => node.span,
            Stmt::For(node) => node.span,
            Stmt::ForIn(node) => node.span,
            Stmt::ForOf(node) => node.span,
            Stmt::Function(node) => node.span,
            Stmt::Variable(node) => node.span,
            Stmt::Class(node) => node.span,
            Stmt::Import(node) => node.span,
            Stmt::ExportNamed(node) => node.span,
            Stmt::ExportDefault(node) => node.span,
            Stmt::ExportAll(node) => node.span,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Stmt::Expression(_) => "ExpressionStatement",
            Stmt::Block(_) => "BlockStatement",
            Stmt::Empty(_) => "EmptyStatement",
            Stmt::Debugger(_) => "DebuggerStatement",
            Stmt::With(_) => "WithStatement",
            Stmt::Return(_) => "ReturnStatement",
            Stmt::Labeled(_) => "LabeledStatement",
            Stmt::Break(_) => "BreakStatement",
            Stmt::Continue(_) => "ContinueStatement",
            Stmt::If(_) => "IfStatement",
            Stmt::Switch(_) => "SwitchStatement",
            Stmt::Throw(_) => "ThrowStatement",
            Stmt::Try(_) => "TryStatement",
            Stmt::While(_) => "WhileStatement",
            Stmt::DoWhile(_) => "DoWhileStatement",
            Stmt::For(_) => "ForStatement",
            Stmt::ForIn(_) => "ForInStatement",
            Stmt::ForOf(_) => "ForOfStatement",
            Stmt::Function(_) => "FunctionDeclaration",
            Stmt::Variable(_) => "VariableDeclaration",
            Stmt::Class(_) => "ClassDeclaration",
            Stmt::Import(_) => "ImportDeclaration",
            Stmt::ExportNamed(_) => "ExportNamedDeclaration",
            Stmt::ExportDefault(_) => "ExportDefaultDeclaration",
            Stmt::ExportAll(_) => "ExportAllDeclaration",
        }
    }
}

impl Pat {
    pub fn span(&self) -> Span {
        match self {
            Pat::Identifier(node) => node.span,
            Pat::Member(node) => node.span,
            Pat::Object(node) => node.span,
            Pat::Array(node) => node.span,
            Pat::Rest(node) => node.span,
            Pat::Assignment(node) => node.span,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Pat::Identifier(_) => "Identifier",
            Pat::Member(_) => "MemberExpression",
            Pat::Object(_) => "ObjectPattern",
            Pat::Array(_) => "ArrayPattern",
            Pat::Rest(_) => "RestElement",
            Pat::Assignment(_) => "AssignmentPattern",
        }
    }
}

/// A parse failure as acorn raises it: its message (with acorn's ` (line:column)` suffix) and position.
#[derive(Debug, Clone)]
pub struct SyntaxError {
    /// Exactly acorn's `error.message`, e.g. `Unexpected token (1:8)`.
    pub message: String,
    /// acorn's `error.pos` (UTF-16 offset).
    pub pos: u32,
    /// acorn's `error.loc`: line 1-based, column 0-based.
    pub loc: Position,
}
