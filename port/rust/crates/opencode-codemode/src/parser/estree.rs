//! Writes a tree as acorn's ESTree JSON, for the differential check against acorn (`harness/parser`).
//!
//! The text matches `harness/parser/acorn-dump.ts`: object keys sorted, every field acorn sets, and these encodings
//! for values JSON cannot hold: a BigInt literal's value is `{"$bigint": digits}`, a regex literal's value is
//! `{"$regexp": true}`, and a non-finite number is `{"$num": "Infinity"}`.

use super::ast::*;

enum J<'a> {
    Null,
    Bool(bool),
    Int(u32),
    Num(f64),
    Str(&'a str),
    Units(&'a [u16]),
    Arr(Vec<J<'a>>),
    Obj(Vec<(&'static str, J<'a>)>),
}

/// The stack the recursive conversion gets: a chain the parser builds in a loop can be any length.
const WRITER_STACK: usize = 1 << 30;

/// The JSON line for a parse result: the Program, or `{"error": {message, pos, loc}}`.
pub fn result_to_json(result: &Result<Program, SyntaxError>) -> String {
    stacker::grow(WRITER_STACK, || {
        let value = match result {
            Ok(program) => program_json(program),
            Err(error) => J::Obj(vec![(
                "error",
                J::Obj(vec![("message", J::Str(&error.message)), ("pos", J::Int(error.pos)), ("loc", position(error.loc))]),
            )]),
        };
        let mut out = String::new();
        write(&value, &mut out);
        out
    })
}

pub fn program_to_json(program: &Program) -> String {
    stacker::grow(WRITER_STACK, || {
        let mut out = String::new();
        write(&program_json(program), &mut out);
        out
    })
}

fn write(value: &J, out: &mut String) {
    match value {
        J::Null => out.push_str("null"),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        J::Int(n) => out.push_str(&n.to_string()),
        J::Num(n) => {
            let mut buffer = ryu_js::Buffer::new();
            out.push_str(buffer.format(*n));
        }
        J::Str(s) => write_units(&s.encode_utf16().collect::<Vec<u16>>(), out),
        J::Units(units) => write_units(units, out),
        J::Arr(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write(item, out);
            }
            out.push(']');
        }
        J::Obj(fields) => {
            let mut order: Vec<usize> = (0..fields.len()).collect();
            order.sort_by(|&a, &b| fields[a].0.cmp(fields[b].0));
            out.push('{');
            for (index, &field) in order.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_units(&fields[field].0.encode_utf16().collect::<Vec<u16>>(), out);
                out.push(':');
                write(&fields[field].1, out);
            }
            out.push('}');
        }
    }
}

/// `JSON.stringify` of a string: lone surrogates become `\udXXX`.
fn write_units(units: &[u16], out: &mut String) {
    out.push('"');
    let mut i = 0;
    while i < units.len() {
        let unit = units[i];
        match unit {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0..=0x1f => out.push_str(&format!("\\u{unit:04x}")),
            0xd800..=0xdbff if i + 1 < units.len() && (0xdc00..=0xdfff).contains(&units[i + 1]) => {
                let code = 0x10000 + ((unit as u32 - 0xd800) << 10) + (units[i + 1] as u32 - 0xdc00);
                out.push(char::from_u32(code).unwrap());
                i += 1;
            }
            0xd800..=0xdfff => out.push_str(&format!("\\u{unit:04x}")),
            _ => out.push(char::from_u32(unit as u32).unwrap()),
        }
        i += 1;
    }
    out.push('"');
}

fn position<'a>(position: Position) -> J<'a> {
    J::Obj(vec![("line", J::Int(position.line)), ("column", J::Int(position.column))])
}

fn node<'a>(ty: &'static str, span: Span, mut fields: Vec<(&'static str, J<'a>)>) -> J<'a> {
    fields.push(("type", J::Str(ty)));
    fields.push(("start", J::Int(span.start)));
    fields.push(("end", J::Int(span.end)));
    fields.push(("loc", J::Obj(vec![("start", position(span.loc_start)), ("end", position(span.loc_end))])));
    J::Obj(fields)
}

fn opt<'a, T>(value: &'a Option<T>, f: impl Fn(&'a T) -> J<'a>) -> J<'a> {
    match value {
        Some(value) => f(value),
        None => J::Null,
    }
}

fn program_json(program: &Program) -> J<'_> {
    node("Program", program.span, vec![("body", stmts(&program.body)), ("sourceType", J::Str("script"))])
}

fn stmts(list: &[Stmt]) -> J<'_> {
    J::Arr(list.iter().map(stmt).collect())
}

fn ident(id: &Ident) -> J<'_> {
    node("Identifier", id.span, vec![("name", J::Str(&id.name))])
}

fn private_ident(id: &PrivateIdent) -> J<'_> {
    node("PrivateIdentifier", id.span, vec![("name", J::Str(&id.name))])
}

fn literal(lit: &Literal) -> J<'_> {
    let mut fields = vec![("raw", J::Str(&lit.raw))];
    let value = match &lit.value {
        LitValue::String(s) => J::Units(s.units()),
        LitValue::Number(n) if n.is_finite() => J::Num(*n),
        LitValue::Number(n) => J::Obj(vec![(
            "$num",
            J::Str(if *n > 0.0 {
                "Infinity"
            } else if *n < 0.0 {
                "-Infinity"
            } else {
                "NaN"
            }),
        )]),
        LitValue::Boolean(b) => J::Bool(*b),
        LitValue::Null => J::Null,
        LitValue::RegExp { pattern, flags } => {
            fields.push(("regex", J::Obj(vec![("pattern", J::Str(pattern)), ("flags", J::Str(flags))])));
            J::Obj(vec![("$regexp", J::Bool(true))])
        }
        LitValue::BigInt(digits) => {
            fields.push(("bigint", J::Str(digits)));
            J::Obj(vec![("$bigint", J::Str(digits))])
        }
    };
    fields.push(("value", value));
    node("Literal", lit.span, fields)
}

fn block(block: &BlockStatement) -> J<'_> {
    node("BlockStatement", block.span, vec![("body", stmts(&block.body))])
}

fn var_decl(decl: &VariableDeclaration) -> J<'_> {
    let declarations = decl
        .declarations
        .iter()
        .map(|d| node("VariableDeclarator", d.span, vec![("id", pat(&d.id)), ("init", opt(&d.init, expr))]))
        .collect();
    node(
        "VariableDeclaration",
        decl.span,
        vec![("declarations", J::Arr(declarations)), ("kind", J::Str(decl.kind.as_str()))],
    )
}

fn for_head(head: &ForHead) -> J<'_> {
    match head {
        ForHead::Variable(decl) => var_decl(decl),
        ForHead::Pattern(p) => pat(p),
    }
}

fn module_name(name: &ModuleName) -> J<'_> {
    match name {
        ModuleName::Identifier(id) => ident(id),
        ModuleName::Literal(lit) => literal(lit),
    }
}

fn attributes(list: &[ImportAttribute]) -> J<'_> {
    J::Arr(
        list.iter()
            .map(|a| node("ImportAttribute", a.span, vec![("key", module_name(&a.key)), ("value", literal(&a.value))]))
            .collect(),
    )
}

fn stmt(s: &Stmt) -> J<'_> {
    match s {
        Stmt::Expression(e) => {
            let mut fields = vec![("expression", expr(&e.expression))];
            if let Some(directive) = &e.directive {
                fields.push(("directive", J::Str(directive)));
            }
            node("ExpressionStatement", e.span, fields)
        }
        Stmt::Block(b) => block(b),
        Stmt::Empty(span) => node("EmptyStatement", *span, vec![]),
        Stmt::Debugger(span) => node("DebuggerStatement", *span, vec![]),
        Stmt::With(w) => node("WithStatement", w.span, vec![("object", expr(&w.object)), ("body", stmt(&w.body))]),
        Stmt::Return(r) => node("ReturnStatement", r.span, vec![("argument", opt(&r.argument, expr))]),
        Stmt::Labeled(l) => node("LabeledStatement", l.span, vec![("body", stmt(&l.body)), ("label", ident(&l.label))]),
        Stmt::Break(b) => node("BreakStatement", b.span, vec![("label", opt(&b.label, ident))]),
        Stmt::Continue(c) => node("ContinueStatement", c.span, vec![("label", opt(&c.label, ident))]),
        Stmt::If(i) => node(
            "IfStatement",
            i.span,
            vec![("test", expr(&i.test)), ("consequent", stmt(&i.consequent)), ("alternate", opt(&i.alternate, stmt))],
        ),
        Stmt::Switch(sw) => {
            let cases = sw
                .cases
                .iter()
                .map(|c| {
                    node("SwitchCase", c.span, vec![("consequent", stmts(&c.consequent)), ("test", opt(&c.test, expr))])
                })
                .collect();
            node("SwitchStatement", sw.span, vec![("discriminant", expr(&sw.discriminant)), ("cases", J::Arr(cases))])
        }
        Stmt::Throw(t) => node("ThrowStatement", t.span, vec![("argument", expr(&t.argument))]),
        Stmt::Try(t) => {
            let handler = opt(&t.handler, |h| {
                node("CatchClause", h.span, vec![("param", opt(&h.param, pat)), ("body", block(&h.body))])
            });
            node(
                "TryStatement",
                t.span,
                vec![("block", block(&t.block)), ("handler", handler), ("finalizer", opt(&t.finalizer, block))],
            )
        }
        Stmt::While(w) => node("WhileStatement", w.span, vec![("test", expr(&w.test)), ("body", stmt(&w.body))]),
        Stmt::DoWhile(d) => node("DoWhileStatement", d.span, vec![("body", stmt(&d.body)), ("test", expr(&d.test))]),
        Stmt::For(f) => {
            let init = opt(&f.init, |init| match init {
                ForInit::Variable(decl) => var_decl(decl),
                ForInit::Expr(e) => expr(e),
            });
            node(
                "ForStatement",
                f.span,
                vec![
                    ("init", init),
                    ("test", opt(&f.test, expr)),
                    ("update", opt(&f.update, expr)),
                    ("body", stmt(&f.body)),
                ],
            )
        }
        Stmt::ForIn(f) => node(
            "ForInStatement",
            f.span,
            vec![("left", for_head(&f.left)), ("right", expr(&f.right)), ("body", stmt(&f.body))],
        ),
        Stmt::ForOf(f) => node(
            "ForOfStatement",
            f.span,
            vec![
                ("await", J::Bool(f.is_await)),
                ("left", for_head(&f.left)),
                ("right", expr(&f.right)),
                ("body", stmt(&f.body)),
            ],
        ),
        Stmt::Function(f) => function(f),
        Stmt::Variable(v) => var_decl(v),
        Stmt::Class(c) => class(c),
        Stmt::Import(i) => {
            let specifiers = i
                .specifiers
                .iter()
                .map(|s| match s {
                    ImportSpecifierKind::Named { span, imported, local } => node(
                        "ImportSpecifier",
                        *span,
                        vec![("imported", module_name(imported)), ("local", ident(local))],
                    ),
                    ImportSpecifierKind::Default { span, local } => {
                        node("ImportDefaultSpecifier", *span, vec![("local", ident(local))])
                    }
                    ImportSpecifierKind::Namespace { span, local } => {
                        node("ImportNamespaceSpecifier", *span, vec![("local", ident(local))])
                    }
                })
                .collect();
            node(
                "ImportDeclaration",
                i.span,
                vec![
                    ("specifiers", J::Arr(specifiers)),
                    ("source", literal(&i.source)),
                    ("attributes", attributes(&i.attributes)),
                ],
            )
        }
        Stmt::ExportNamed(e) => {
            let specifiers = e
                .specifiers
                .iter()
                .map(|s| {
                    node(
                        "ExportSpecifier",
                        s.span,
                        vec![("local", module_name(&s.local)), ("exported", module_name(&s.exported))],
                    )
                })
                .collect();
            node(
                "ExportNamedDeclaration",
                e.span,
                vec![
                    ("declaration", opt(&e.declaration, stmt)),
                    ("specifiers", J::Arr(specifiers)),
                    ("source", opt(&e.source, literal)),
                    ("attributes", attributes(&e.attributes)),
                ],
            )
        }
        Stmt::ExportDefault(e) => {
            let declaration = match &e.declaration {
                ExportDefaultKind::Declaration(s) => stmt(s),
                ExportDefaultKind::Expr(x) => expr(x),
            };
            node("ExportDefaultDeclaration", e.span, vec![("declaration", declaration)])
        }
        Stmt::ExportAll(e) => node(
            "ExportAllDeclaration",
            e.span,
            vec![
                ("exported", opt(&e.exported, module_name)),
                ("source", literal(&e.source)),
                ("attributes", attributes(&e.attributes)),
            ],
        ),
    }
}

fn function(f: &Function) -> J<'_> {
    let ty = match f.kind {
        FunctionKind::Declaration => "FunctionDeclaration",
        FunctionKind::Expression => "FunctionExpression",
        FunctionKind::Arrow => "ArrowFunctionExpression",
    };
    let (body, expression) = match &f.body {
        FunctionBody::Block(b) => (block(b), false),
        FunctionBody::Expr(e) => (expr(e), true),
    };
    node(
        ty,
        f.span,
        vec![
            ("id", opt(&f.id, ident)),
            ("expression", J::Bool(expression)),
            ("generator", J::Bool(f.generator)),
            ("async", J::Bool(f.is_async)),
            ("params", J::Arr(f.params.iter().map(pat).collect())),
            ("body", body),
        ],
    )
}

fn class(c: &Class) -> J<'_> {
    let body = c
        .body
        .body
        .iter()
        .map(|element| match element {
            ClassElement::Method(m) => node(
                "MethodDefinition",
                m.span,
                vec![
                    ("static", J::Bool(m.is_static)),
                    ("computed", J::Bool(m.computed)),
                    ("key", expr(&m.key)),
                    (
                        "kind",
                        J::Str(match m.kind {
                            MethodKind::Constructor => "constructor",
                            MethodKind::Method => "method",
                            MethodKind::Get => "get",
                            MethodKind::Set => "set",
                        }),
                    ),
                    ("value", function(&m.value)),
                ],
            ),
            ClassElement::Property(p) => node(
                "PropertyDefinition",
                p.span,
                vec![
                    ("static", J::Bool(p.is_static)),
                    ("computed", J::Bool(p.computed)),
                    ("key", expr(&p.key)),
                    ("value", opt(&p.value, expr)),
                ],
            ),
            ClassElement::StaticBlock(s) => node("StaticBlock", s.span, vec![("body", stmts(&s.body))]),
        })
        .collect();
    node(
        if c.is_declaration { "ClassDeclaration" } else { "ClassExpression" },
        c.span,
        vec![
            ("id", opt(&c.id, ident)),
            ("superClass", opt(&c.super_class, expr)),
            ("body", node("ClassBody", c.body.span, vec![("body", J::Arr(body))])),
        ],
    )
}

fn expr_or_spread(item: &ExprOrSpread) -> J<'_> {
    match item {
        ExprOrSpread::Expr(e) => expr(e),
        ExprOrSpread::Spread(s) => node("SpreadElement", s.span, vec![("argument", expr(&s.argument))]),
    }
}

fn template(t: &TemplateLiteral) -> J<'_> {
    let quasis = t
        .quasis
        .iter()
        .map(|q| {
            let cooked = match &q.cooked {
                Some(cooked) => J::Units(cooked.units()),
                None => J::Null,
            };
            node(
                "TemplateElement",
                q.span,
                vec![
                    ("value", J::Obj(vec![("raw", J::Units(q.raw.units())), ("cooked", cooked)])),
                    ("tail", J::Bool(q.tail)),
                ],
            )
        })
        .collect();
    node(
        "TemplateLiteral",
        t.span,
        vec![("expressions", J::Arr(t.expressions.iter().map(expr).collect())), ("quasis", J::Arr(quasis))],
    )
}

fn member(m: &MemberExpression) -> J<'_> {
    node(
        "MemberExpression",
        m.span,
        vec![
            ("object", expr(&m.object)),
            ("property", expr(&m.property)),
            ("computed", J::Bool(m.computed)),
            ("optional", J::Bool(m.optional)),
        ],
    )
}

fn expr(e: &Expr) -> J<'_> {
    match e {
        Expr::Identifier(id) => ident(id),
        Expr::PrivateIdentifier(id) => private_ident(id),
        Expr::Literal(lit) => literal(lit),
        Expr::This(span) => node("ThisExpression", *span, vec![]),
        Expr::Super(span) => node("Super", *span, vec![]),
        Expr::Array(a) => node(
            "ArrayExpression",
            a.span,
            vec![("elements", J::Arr(a.elements.iter().map(|item| opt(item, expr_or_spread)).collect()))],
        ),
        Expr::Object(o) => {
            let properties = o
                .properties
                .iter()
                .map(|p| match p {
                    ObjectMember::Property(p) => {
                        let value = match &p.value {
                            // A shorthand `{a = 1}` holds an AssignmentPattern (only valid once it becomes a pattern).
                            Expr::Assignment(a) if p.shorthand => node(
                                "AssignmentPattern",
                                a.span,
                                vec![("left", pat(&a.left)), ("right", expr(&a.right))],
                            ),
                            value => expr(value),
                        };
                        node(
                            "Property",
                            p.span,
                            vec![
                                ("method", J::Bool(p.method)),
                                ("shorthand", J::Bool(p.shorthand)),
                                ("computed", J::Bool(p.computed)),
                                ("key", expr(&p.key)),
                                (
                                    "kind",
                                    J::Str(match p.kind {
                                        PropKind::Init => "init",
                                        PropKind::Get => "get",
                                        PropKind::Set => "set",
                                    }),
                                ),
                                ("value", value),
                            ],
                        )
                    }
                    ObjectMember::Spread(s) => node("SpreadElement", s.span, vec![("argument", expr(&s.argument))]),
                })
                .collect();
            node("ObjectExpression", o.span, vec![("properties", J::Arr(properties))])
        }
        Expr::Function(f) => function(f),
        Expr::Class(c) => class(c),
        Expr::Unary(u) => node(
            "UnaryExpression",
            u.span,
            vec![("operator", J::Str(u.operator)), ("prefix", J::Bool(true)), ("argument", expr(&u.argument))],
        ),
        Expr::Update(u) => node(
            "UpdateExpression",
            u.span,
            vec![("operator", J::Str(u.operator)), ("prefix", J::Bool(u.prefix)), ("argument", expr(&u.argument))],
        ),
        Expr::Binary(b) => node(
            "BinaryExpression",
            b.span,
            vec![("left", expr(&b.left)), ("operator", J::Str(b.operator)), ("right", expr(&b.right))],
        ),
        Expr::Logical(l) => node(
            "LogicalExpression",
            l.span,
            vec![("left", expr(&l.left)), ("operator", J::Str(l.operator)), ("right", expr(&l.right))],
        ),
        Expr::Assignment(a) => node(
            "AssignmentExpression",
            a.span,
            vec![("operator", J::Str(a.operator)), ("left", pat(&a.left)), ("right", expr(&a.right))],
        ),
        Expr::Member(m) => member(m),
        Expr::Conditional(c) => node(
            "ConditionalExpression",
            c.span,
            vec![("test", expr(&c.test)), ("consequent", expr(&c.consequent)), ("alternate", expr(&c.alternate))],
        ),
        Expr::Call(c) => node(
            "CallExpression",
            c.span,
            vec![
                ("callee", expr(&c.callee)),
                ("arguments", J::Arr(c.arguments.iter().map(expr_or_spread).collect())),
                ("optional", J::Bool(c.optional)),
            ],
        ),
        Expr::New(n) => node(
            "NewExpression",
            n.span,
            vec![("callee", expr(&n.callee)), ("arguments", J::Arr(n.arguments.iter().map(expr_or_spread).collect()))],
        ),
        Expr::Sequence(s) => {
            node("SequenceExpression", s.span, vec![("expressions", J::Arr(s.expressions.iter().map(expr).collect()))])
        }
        Expr::Yield(y) => node(
            "YieldExpression",
            y.span,
            vec![("delegate", J::Bool(y.delegate)), ("argument", opt(&y.argument, expr))],
        ),
        Expr::Await(a) => node("AwaitExpression", a.span, vec![("argument", expr(&a.argument))]),
        Expr::Template(t) => template(t),
        Expr::TaggedTemplate(t) => {
            node("TaggedTemplateExpression", t.span, vec![("tag", expr(&t.tag)), ("quasi", template(&t.quasi))])
        }
        Expr::Chain(c) => node("ChainExpression", c.span, vec![("expression", expr(&c.expression))]),
        Expr::MetaProperty(m) => {
            node("MetaProperty", m.span, vec![("meta", ident(&m.meta)), ("property", ident(&m.property))])
        }
        Expr::Import(i) => {
            node("ImportExpression", i.span, vec![("source", expr(&i.source)), ("options", opt(&i.options, expr))])
        }
    }
}

fn pat(p: &Pat) -> J<'_> {
    match p {
        Pat::Identifier(id) => ident(id),
        Pat::Member(m) => member(m),
        Pat::Object(o) => {
            let properties = o
                .properties
                .iter()
                .map(|prop| match prop {
                    ObjectPatternMember::Property(p) => node(
                        "Property",
                        p.span,
                        vec![
                            ("method", J::Bool(false)),
                            ("shorthand", J::Bool(p.shorthand)),
                            ("computed", J::Bool(p.computed)),
                            ("key", expr(&p.key)),
                            ("kind", J::Str("init")),
                            ("value", pat(&p.value)),
                        ],
                    ),
                    ObjectPatternMember::Rest(r) => node("RestElement", r.span, vec![("argument", pat(&r.argument))]),
                })
                .collect();
            node("ObjectPattern", o.span, vec![("properties", J::Arr(properties))])
        }
        Pat::Array(a) => {
            node("ArrayPattern", a.span, vec![("elements", J::Arr(a.elements.iter().map(|e| opt(e, pat)).collect()))])
        }
        Pat::Rest(r) => node("RestElement", r.span, vec![("argument", pat(&r.argument))]),
        Pat::Assignment(a) => {
            node("AssignmentPattern", a.span, vec![("left", pat(&a.left)), ("right", expr(&a.right))])
        }
    }
}
