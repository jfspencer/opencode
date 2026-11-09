//! The Rust side of harness/diff.ts: reads one JSON case per line from the file named by the first argument and
//! writes one JSON answer per line to the file named by the second. The ops and answer shapes mirror
//! harness/ts-driver.ts.

mod json;

use json::J;
use opencode_tui_markup::js::JsString;
use opencode_tui_markup::links;
use opencode_tui_markup::{diff, table};
use opencode_tui_markup::markdown::{
    BorderStyle, BoxRole, CodeNode, ColumnFitter, ColumnWidthMode, InternalBlockMode, Markdown, MarkdownOptions, MarkdownTableOptions,
    MarkdownTableStyle, Node, WrapMode,
};
use opencode_tui_markup::marked::{self, Align, ListStart, Nullable, TableCell, Token};
use opencode_tui_markup::code::{self, HighlightOptions, HighlightOutcome};
use opencode_tui_markup::highlight::SimpleHighlight;
use opencode_tui_markup::style::{Rgba, StyleDefinition, SyntaxStyle, TextChunk};
use opencode_tui_markup::tree_sitter::{self, TreeSitterClient};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::rc::Rc;

pub fn token(t: &Token) -> J {
    let mut f: Vec<(&'static str, J)> = vec![("type", J::s(t.kind.as_str())), ("raw", J::js(&t.raw))];
    if let Some(text) = &t.text {
        f.push(("text", J::js(text)));
    }
    if let Some(tokens) = &t.tokens {
        f.push(("tokens", J::Arr(tokens.iter().map(token).collect())));
    }
    if let Some(depth) = t.depth {
        f.push(("depth", J::Num(depth as f64)));
    }
    if let Some(lang) = &t.lang {
        f.push(("lang", J::js(lang)));
    }
    if t.code_block_style_indented {
        f.push(("codeBlockStyle", J::s("indented")));
    }
    if let Some(e) = t.escaped {
        f.push(("escaped", J::Bool(e)));
    }
    if let Some(h) = &t.href {
        f.push(("href", J::js(h)));
    }
    match &t.title {
        Nullable::Undefined => {}
        Nullable::Null => f.push(("title", J::Null)),
        Nullable::Str(s) => f.push(("title", J::js(s))),
    }
    if let Some(tag) = &t.tag {
        f.push(("tag", J::js(tag)));
    }
    if let Some(o) = t.ordered {
        f.push(("ordered", J::Bool(o)));
    }
    match t.start {
        Some(ListStart::Empty) => f.push(("start", J::s(""))),
        Some(ListStart::Number(n)) => f.push(("start", J::Num(n))),
        None => {}
    }
    if let Some(l) = t.loose {
        f.push(("loose", J::Bool(l)));
    }
    if let Some(items) = &t.items {
        f.push(("items", J::Arr(items.iter().map(token).collect())));
    }
    if let Some(task) = t.task {
        f.push(("task", J::Bool(task)));
    }
    if let Some(c) = t.checked {
        f.push(("checked", J::Bool(c)));
    }
    if let Some(p) = t.pre {
        f.push(("pre", J::Bool(p)));
    }
    if let Some(b) = t.block {
        f.push(("block", J::Bool(b)));
    }
    if let Some(b) = t.in_link {
        f.push(("inLink", J::Bool(b)));
    }
    if let Some(b) = t.in_raw_block {
        f.push(("inRawBlock", J::Bool(b)));
    }
    if let Some(header) = &t.header {
        f.push(("header", J::Arr(header.iter().map(cell).collect())));
    }
    if let Some(align) = &t.align {
        f.push(("align", J::Arr(align.iter().map(|a| align_json(*a)).collect())));
    }
    if let Some(rows) = &t.rows {
        f.push(("rows", J::Arr(rows.iter().map(|r| J::Arr(r.iter().map(cell).collect())).collect())));
    }
    J::Obj(f)
}

fn align_json(a: Option<Align>) -> J {
    match a {
        None => J::Null,
        Some(Align::Left) => J::s("left"),
        Some(Align::Center) => J::s("center"),
        Some(Align::Right) => J::s("right"),
    }
}

fn cell(c: &TableCell) -> J {
    J::Obj(vec![
        ("text", J::js(&c.text)),
        ("tokens", J::Arr(c.tokens.iter().map(token).collect())),
        ("header", J::Bool(c.header)),
        ("align", align_json(c.align)),
    ])
}

fn nullable(n: &Nullable) -> Option<J> {
    match n {
        Nullable::Undefined => None,
        Nullable::Null => Some(J::Null),
        Nullable::Str(s) => Some(J::js(s)),
    }
}

/// `Object.entries` order: array-index keys ascending, then the rest in insertion order.
fn js_property_order<V>(entries: &[(JsString, V)]) -> Vec<&(JsString, V)> {
    let index = |k: &JsString| -> Option<u64> {
        let s = k.to_string();
        let canonical = !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
        s.parse::<u64>().ok().filter(|&n| canonical && n < 4294967295)
    };
    let mut indexed: Vec<_> = entries.iter().filter_map(|e| index(&e.0).map(|n| (n, e))).collect();
    indexed.sort_by_key(|(n, _)| *n);
    let mut out: Vec<_> = indexed.into_iter().map(|(_, e)| e).collect();
    out.extend(entries.iter().filter(|e| index(&e.0).is_none()));
    out
}

fn rgba(c: Option<Rgba>) -> J {
    J::opt(c, |c| J::Arr(c.0.iter().map(|&v| J::Num(v as f64)).collect()))
}

pub fn chunk(c: &TextChunk) -> J {
    J::Obj(vec![
        ("text", J::js(&c.text)),
        ("fg", rgba(c.fg)),
        ("bg", rgba(c.bg)),
        ("attributes", J::opt(c.attributes, |a| J::Num(a as f64))),
        ("link", J::opt(c.link.as_ref(), J::js)),
    ])
}

fn chunks(list: &[TextChunk]) -> J {
    J::Arr(list.iter().map(chunk).collect())
}

pub fn theme() -> SyntaxStyle {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/harness/theme.json");
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut style = SyntaxStyle::new();
    for (name, def) in value.as_object().unwrap() {
        let color = |k: &str| def.get(k).and_then(|v| v.as_str()).map(Rgba::from_hex);
        let flag = |k: &str| def.get(k).and_then(|v| v.as_bool());
        style.register_style(
            name,
            StyleDefinition {
                fg: color("fg"),
                bg: color("bg"),
                bold: flag("bold"),
                italic: flag("italic"),
                underline: flag("underline"),
                dim: flag("dim"),
            },
        );
    }
    style
}

fn dump_node(node: &Node, md: &Markdown, uids: &mut HashMap<u64, usize>) -> J {
    let next = uids.len() + 1;
    let uid = *uids.entry(node.uid()).or_insert(next);
    let mut f: Vec<(&'static str, J)> = vec![
        ("uid", J::Num(uid as f64)),
        ("id", J::s(node.id())),
        ("marginTop", J::Num(node.margin_top())),
        ("marginBottom", J::Num(node.margin_bottom())),
    ];
    match node {
        Node::Code(c) => {
            f.push(("kind", J::s("code")));
            f.push(("content", J::js(&c.content)));
            f.push(("filetype", J::opt(c.filetype.as_deref(), J::s)));
            f.push(("fg", rgba(c.fg)));
            f.push(("bg", rgba(c.bg)));
            f.push(("conceal", J::Bool(c.conceal)));
            f.push(("drawUnstyledText", J::Bool(c.draw_unstyled_text)));
            f.push(("streaming", J::Bool(c.streaming)));
            f.push(("initialStyledText", J::opt(c.initial_styled_text.as_ref(), |t| chunks(&t.chunks))));
            f.push(("baseHighlight", J::opt(c.base_highlight.as_deref(), J::s)));
            f.push(("hooks", J::Bool(c.markdown_hooks == Some(md.hook_generation()))));
        }
        Node::Table(t) => {
            let o = &t.options;
            f.push(("kind", J::s("table")));
            f.push((
                "content",
                J::Arr(t.content.iter().map(|row| J::Arr(row.iter().map(|c| J::opt(c.as_ref(), |c| chunks(c))).collect())).collect()),
            ));
            f.push(("columnWidthMode", J::s(match o.column_width_mode { ColumnWidthMode::Content => "content", ColumnWidthMode::Full => "full" })));
            f.push(("columnFitter", J::s(match o.column_fitter { ColumnFitter::Proportional => "proportional", ColumnFitter::Balanced => "balanced" })));
            f.push(("wrapMode", J::s(match o.wrap_mode { WrapMode::None => "none", WrapMode::Char => "char", WrapMode::Word => "word" })));
            f.push(("cellPaddingX", J::Num(o.cell_padding_x)));
            f.push(("cellPaddingY", J::Num(o.cell_padding_y)));
            f.push(("columnGap", J::Num(o.column_gap)));
            f.push(("border", J::Bool(o.border)));
            f.push(("outerBorder", J::Bool(o.outer_border)));
            f.push(("showBorders", J::Bool(o.show_borders)));
            f.push((
                "borderStyle",
                J::s(match o.border_style {
                    BorderStyle::Single => "single",
                    BorderStyle::Double => "double",
                    BorderStyle::Rounded => "rounded",
                    BorderStyle::Heavy => "heavy",
                }),
            ));
            f.push(("borderColor", rgba(Some(o.border_color))));
            f.push(("fg", rgba(o.fg)));
            f.push(("selectable", J::Bool(o.selectable)));
        }
        Node::Text(t) => {
            f.push(("kind", J::s("text")));
            f.push(("chunks", chunks(&t.content.chunks)));
            f.push(("width", J::Num(t.width)));
        }
        Node::Box(b) => {
            f.push(("kind", J::s("box")));
            let border = match b.role {
                BoxRole::Blockquote => Some("left"),
                BoxRole::Hr => Some("top"),
                _ => None,
            };
            f.push(("border", border.map_or(J::Bool(false), |side| J::Arr(vec![J::s(side)]))));
            f.push(("borderColor", if border.is_some() { rgba(b.border_color) } else { J::Null }));
            f.push(("children", J::Arr(b.children.iter().map(|c| dump_node(c, md, uids)).collect())));
        }
    }
    J::Obj(f)
}

fn dump_markdown(md: &Markdown, uids: &mut HashMap<u64, usize>) -> J {
    let tree: Vec<J> = md.children().map(|n| dump_node(n, md, uids)).collect();
    J::Obj(vec![
        ("tree", J::Arr(tree)),
        ("stable", J::Num(md.stable_block_count as f64)),
        ("tokens", J::opt(md.parse_state.as_ref(), |p| J::Arr(p.tokens.iter().map(|t| token(t)).collect()))),
        ("stableTokens", J::opt(md.parse_state.as_ref(), |p| J::Num(p.stable_token_count as f64))),
    ])
}

fn table_options(v: &Value) -> Option<MarkdownTableOptions> {
    let o = v.as_object()?;
    let num = |k: &str| o.get(k).and_then(|v| v.as_f64());
    Some(MarkdownTableOptions {
        style: o.get("style").and_then(|v| v.as_str()).map(|s| if s == "grid" { MarkdownTableStyle::Grid } else { MarkdownTableStyle::Columns }),
        width_mode: o.get("widthMode").and_then(|v| v.as_str()).map(|s| if s == "full" { ColumnWidthMode::Full } else { ColumnWidthMode::Content }),
        column_fitter: o.get("columnFitter").and_then(|v| v.as_str()).map(|s| if s == "balanced" { ColumnFitter::Balanced } else { ColumnFitter::Proportional }),
        wrap_mode: o.get("wrapMode").and_then(|v| v.as_str()).map(|s| match s {
            "none" => WrapMode::None,
            "char" => WrapMode::Char,
            _ => WrapMode::Word,
        }),
        cell_padding: num("cellPadding"),
        cell_padding_x: num("cellPaddingX"),
        cell_padding_y: num("cellPaddingY"),
        borders: o.get("borders").and_then(|v| v.as_bool()),
        outer_border: o.get("outerBorder").and_then(|v| v.as_bool()),
        border_style: None,
        border_color: None,
        selectable: o.get("selectable").and_then(|v| v.as_bool()),
    })
}

fn markdown_case(case: &Value) -> J {
    let style = Rc::new(theme());
    let steps = case["steps"].as_array().unwrap();
    let first = &steps[0];
    let color = |k: &str| case.get(k).and_then(|v| v.as_str()).map(Rgba::from_hex);
    let mut md = Markdown::new(MarkdownOptions {
        id: "md".into(),
        content: s(first, "content"),
        syntax_style: Some(style),
        fg: color("fg"),
        bg: color("bg"),
        conceal: case.get("conceal").and_then(|v| v.as_bool()).unwrap_or(true),
        conceal_code: case.get("concealCode").and_then(|v| v.as_bool()).unwrap_or(false),
        streaming: first.get("streaming").and_then(|v| v.as_bool()).unwrap_or(false),
        table_options: case.get("tableOptions").and_then(table_options),
        internal_block_mode: if case.get("internalBlockMode").and_then(|v| v.as_str()) == Some("top-level") {
            InternalBlockMode::TopLevel
        } else {
            InternalBlockMode::Coalesced
        },
        hyperlinks: case.get("hyperlinks").and_then(|v| v.as_bool()).unwrap_or(false),
    });
    let mut uids = HashMap::new();
    let mut out = vec![dump_markdown(&md, &mut uids)];
    for step in &steps[1..] {
        if let Some(content) = step.get("content").and_then(|v| v.as_str()) {
            md.set_content(JsString::from(content));
        }
        if let Some(streaming) = step.get("streaming").and_then(|v| v.as_bool()) {
            md.set_streaming(streaming);
        }
        if let Some(conceal) = step.get("conceal").and_then(|v| v.as_bool()) {
            md.set_conceal(conceal);
            md.refresh_styles();
        }
        if let Some(h) = step.get("hyperlinks").and_then(|v| v.as_bool()) {
            md.set_hyperlinks(h);
        }
        out.push(dump_markdown(&md, &mut uids));
    }
    J::Arr(out)
}

fn highlight_json(h: &SimpleHighlight) -> J {
    let mut items = vec![J::Num(h.start as f64), J::Num(h.end as f64), J::s(&h.group)];
    if let Some(m) = &h.meta {
        let mut f: Vec<(&'static str, J)> = Vec::new();
        if let Some(v) = m.is_injection {
            f.push(("isInjection", J::Bool(v)));
        }
        if let Some(v) = &m.injection_lang {
            f.push(("injectionLang", J::s(v)));
        }
        if let Some(v) = m.contains_injection {
            f.push(("containsInjection", J::Bool(v)));
        }
        if let Some(v) = nullable(&m.conceal) {
            f.push(("conceal", v));
        }
        if let Some(v) = nullable(&m.conceal_lines) {
            f.push(("concealLines", v));
        }
        items.push(J::Obj(f));
    }
    J::Arr(items)
}

fn client() -> &'static TreeSitterClient {
    static CLIENT: std::sync::OnceLock<TreeSitterClient> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        tree_sitter::add_default_parsers(tree_sitter::tui_parsers());
        TreeSitterClient::new()
    })
}

fn highlight_case(case: &Value) -> J {
    let result = client().highlight_once(&s(case, "content"), case["filetype"].as_str().unwrap());
    J::Obj(vec![
        ("highlights", J::opt(result.highlights.as_ref(), |h| J::Arr(h.iter().map(highlight_json).collect()))),
        ("warning", J::opt(result.warning.as_deref(), J::s)),
        ("error", J::opt(result.error.as_deref(), J::s)),
    ])
}

fn outcome_json(outcome: &HighlightOutcome) -> J {
    match outcome {
        HighlightOutcome::Plain => J::Obj(vec![("chunks", J::s("plain")), ("lineSources", J::Null)]),
        HighlightOutcome::Styled { text, rendered_line_sources } => J::Obj(vec![
            ("chunks", chunks(&text.chunks)),
            (
                "lineSources",
                J::opt(rendered_line_sources.as_ref(), |l| J::Arr(l.iter().map(|&n| J::Num(n as f64)).collect())),
            ),
        ]),
    }
}

fn code_case(case: &Value) -> J {
    // CodeRenderable (scheduleHighlights) runs no pass for empty content: its text buffer is never handed text.
    if s(case, "content").is_empty() {
        return J::Obj(vec![("chunks", J::Null), ("lineSources", J::Null)]);
    }
    let style = theme();
    let base = case.get("baseHighlight").and_then(|v| v.as_str());
    let outcome = code::highlight(
        client(),
        &s(case, "content"),
        case["filetype"].as_str().unwrap(),
        &style,
        &HighlightOptions {
            conceal: case.get("conceal").and_then(|v| v.as_bool()).unwrap_or(true),
            base_highlight: base,
            on_highlight: None,
            on_chunks: None,
        },
    );
    outcome_json(&outcome)
}

/// What each code child of a markdown render ends up showing, as a renderer runs the pipeline for it.
fn markdown_render_case(case: &Value) -> J {
    let style = Rc::new(theme());
    let color = |k: &str| case.get(k).and_then(|v| v.as_str()).map(Rgba::from_hex);
    let md = Markdown::new(MarkdownOptions {
        id: "md".into(),
        content: s(case, "content"),
        syntax_style: Some(style.clone()),
        fg: color("fg"),
        bg: color("bg"),
        conceal: case.get("conceal").and_then(|v| v.as_bool()).unwrap_or(true),
        conceal_code: false,
        streaming: false,
        table_options: case.get("tableOptions").and_then(table_options),
        internal_block_mode: if case.get("internalBlockMode").and_then(|v| v.as_str()) == Some("top-level") {
            InternalBlockMode::TopLevel
        } else {
            InternalBlockMode::Coalesced
        },
        hyperlinks: case.get("hyperlinks").and_then(|v| v.as_bool()).unwrap_or(false),
    });
    let mut out = Vec::new();
    fn walk<'a>(node: &'a Node, out: &mut Vec<&'a CodeNode>) {
        match node {
            Node::Code(c) => out.push(c),
            Node::Box(b) => b.children.iter().for_each(|c| walk(c, out)),
            _ => {}
        }
    }
    let mut codes = Vec::new();
    for node in md.children() {
        walk(node, &mut codes);
    }
    let (conceal, hyperlinks) = (md.conceal(), md.hyperlinks());
    let on_highlight = |h: &[SimpleHighlight], content: &JsString, _: &str| {
        Some(links::add_markdown_link_highlights(h, content, conceal, hyperlinks))
    };
    let on_chunks = |chunks: Vec<TextChunk>, ctx: &code::ChunkContext<'_>| {
        links::detect_links(chunks, ctx.content, ctx.highlights, ctx.source_ranges, Some(ctx.syntax_style))
    };
    for node in codes {
        let chunks = if node.content.is_empty() {
            J::Null
        } else if let Some(filetype) = &node.filetype {
            let hooks = node.markdown_hooks.is_some();
            let outcome = code::highlight(
                client(),
                &node.content,
                filetype,
                &style,
                &HighlightOptions {
                    conceal: node.conceal,
                    base_highlight: node.base_highlight.as_deref(),
                    on_highlight: if hooks { Some(&on_highlight) } else { None },
                    on_chunks: if hooks { Some(&on_chunks) } else { None },
                },
            );
            match outcome {
                HighlightOutcome::Plain => J::s("plain"),
                HighlightOutcome::Styled { text, .. } => chunks(&text.chunks),
            }
        } else {
            J::s("plain")
        };
        out.push(J::Obj(vec![("id", J::s(&node.id)), ("chunks", chunks)]));
    }
    J::Arr(out)
}

fn patch_json(p: &diff::StructuredPatch) -> J {
    let mut f: Vec<(&'static str, J)> = Vec::new();
    let mut opt_s = |k: &'static str, v: &Option<JsString>| {
        if let Some(v) = v {
            f.push((k, J::js(v)));
        }
    };
    opt_s("index", &p.index);
    opt_s("oldFileName", &p.old_file_name);
    opt_s("newFileName", &p.new_file_name);
    opt_s("oldHeader", &p.old_header);
    opt_s("newHeader", &p.new_header);
    opt_s("oldMode", &p.old_mode);
    opt_s("newMode", &p.new_mode);
    for (k, v) in [
        ("isGit", p.is_git),
        ("isRename", p.is_rename),
        ("isCopy", p.is_copy),
        ("isCreate", p.is_create),
        ("isDelete", p.is_delete),
        ("isBinary", p.is_binary),
    ] {
        if let Some(v) = v {
            f.push((k, J::Bool(v)));
        }
    }
    f.push((
        "hunks",
        J::Arr(
            p.hunks
                .iter()
                .map(|h| {
                    J::Obj(vec![
                        ("oldStart", J::Num(h.old_start)),
                        ("oldLines", J::Num(h.old_lines)),
                        ("newStart", J::Num(h.new_start)),
                        ("newLines", J::Num(h.new_lines)),
                        ("lines", J::Arr(h.lines.iter().map(J::js).collect())),
                    ])
                })
                .collect(),
        ),
    ));
    J::Obj(f)
}

fn side_json(side: &str, m: &diff::SideModel, width: &str) -> J {
    J::Obj(vec![
        ("side", J::s(side)),
        ("content", J::js(&m.content)),
        (
            "lineColors",
            J::Arr(
                m.line_colors
                    .iter()
                    .map(|(k, v)| {
                        J::Arr(vec![
                            J::Num(*k as f64),
                            J::Obj(vec![("gutter", rgba(Some(v.gutter))), ("content", rgba(Some(v.content)))]),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "lineSigns",
            J::Arr(
                m.line_signs
                    .iter()
                    .map(|(k, v)| {
                        J::Arr(vec![J::Num(*k as f64), J::Obj(vec![("after", J::s(v.after)), ("afterColor", rgba(Some(v.after_color)))])])
                    })
                    .collect(),
            ),
        ),
        ("lineNumbers", J::Arr(m.line_numbers.iter().map(|(k, v)| J::Arr(vec![J::Num(*k as f64), J::Num(*v)])).collect())),
        ("hideLineNumbers", J::Arr(m.hide_line_numbers.iter().map(|&k| J::Num(k as f64)).collect())),
        ("width", J::s(width)),
    ])
}

fn diff_case(case: &Value) -> J {
    let text = s(case, "diff");
    let mut f: Vec<(&'static str, J)> = Vec::new();
    match diff::parse_patch(&text) {
        Ok(patches) => f.push(("patches", J::Arr(patches.iter().map(patch_json).collect()))),
        Err(e) => f.push(("error", J::s(&e))),
    }
    let mut sides = Vec::new();
    let mut hunk_starts: Vec<usize> = Vec::new();
    let mut error_text = J::Null;
    match diff::parse_diff(&text) {
        diff::ParsedDiff::None => {}
        diff::ParsedDiff::Error(message) => error_text = J::s(&message),
        diff::ParsedDiff::Patch(patch) if !patch.hunks.is_empty() => {
            let model = diff::DiffModel { patch: &patch, colors: diff::DiffColors::default() };
            if case["view"].as_str() == Some("split") {
                let view = model.split_view(None);
                sides.push(side_json("left", &view.left, "50%"));
                sides.push(side_json("right", &view.right, "50%"));
                hunk_starts = view.hunk_start_lines;
            } else {
                let (side, starts) = model.unified_view();
                sides.push(side_json("left", &side, "100%"));
                hunk_starts = starts;
            }
        }
        diff::ParsedDiff::Patch(_) => {}
    }
    f.push(("sides", J::Arr(sides)));
    f.push(("hunkStartLines", J::Arr(hunk_starts.iter().map(|&n| J::Num(n as f64)).collect())));
    f.push(("errorText", error_text));
    J::Obj(f)
}

fn table_case(case: &Value) -> J {
    let cells: Vec<Vec<Option<String>>> = case["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row.as_array().unwrap().iter().map(|c| c.as_str().map(str::to_owned)).collect())
        .collect();
    let o = &case["options"];
    let num = |k: &str| o.get(k).and_then(|v| v.as_f64());
    let wrap = match o.get("wrapMode").and_then(|v| v.as_str()) {
        Some("none") => WrapMode::None,
        Some("char") => WrapMode::Char,
        _ => WrapMode::Word,
    };
    let border = o.get("border").and_then(|v| v.as_bool()).unwrap_or(true);
    let options = table::TableOptions {
        wrap_mode: wrap,
        column_width_mode: if o.get("columnWidthMode").and_then(|v| v.as_str()) == Some("content") {
            ColumnWidthMode::Content
        } else {
            ColumnWidthMode::Full
        },
        column_fitter: if o.get("columnFitter").and_then(|v| v.as_str()) == Some("balanced") {
            ColumnFitter::Balanced
        } else {
            ColumnFitter::Proportional
        },
        cell_padding_x: table::resolve_non_negative(num("cellPaddingX").or(num("cellPadding")), 0),
        cell_padding_y: table::resolve_non_negative(num("cellPaddingY").or(num("cellPadding")), 0),
        column_gap: table::resolve_non_negative(num("columnGap"), 0),
        border,
        outer_border: o.get("outerBorder").and_then(|v| v.as_bool()).unwrap_or(border),
    };
    let columns = cells.iter().map(|r| r.len()).max().unwrap_or(0);
    let t = table::Table { rows: cells.len(), columns, options };
    // Replay the measures TS recorded; a measure TS never asked for is a difference.
    let recorded: Vec<(usize, usize, u32, Option<table::Measure>)> = case["tsMeasures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            let m = m.as_array().unwrap();
            let result = m[3].as_object().map(|o| table::Measure {
                line_count: o["lineCount"].as_u64().unwrap() as u32,
                width_cols_max: o["widthColsMax"].as_u64().unwrap() as u32,
            });
            (m[0].as_u64().unwrap() as usize, m[1].as_u64().unwrap() as usize, m[2].as_u64().unwrap() as u32, result)
        })
        .collect();
    let mut asked: Vec<J> = Vec::new();
    let mut measurer = |row: usize, col: usize, width: u32| -> Option<table::Measure> {
        let found = recorded.iter().find(|(r, c, w, _)| *r == row && *c == col && *w == width);
        let result = found.and_then(|f| f.3);
        asked.push(J::Arr(vec![
            J::Num(row as f64),
            J::Num(col as f64),
            J::Num(width as f64),
            match (found, result) {
                (None, _) => J::s("not measured by TS"),
                (Some(_), None) => J::Null,
                (Some(_), Some(m)) => J::Obj(vec![
                    ("lineCount", J::Num(m.line_count as f64)),
                    ("widthColsMax", J::Num(m.width_cols_max as f64)),
                ]),
            },
        ]));
        result
    };
    let max = case.get("maxWidth").and_then(|v| v.as_f64());
    let layout = t.compute_layout(t.resolve_layout_width_constraint(max), &mut measurer);
    let ints = |v: &[i64]| J::Arr(v.iter().map(|&n| J::Num(n as f64)).collect());
    J::Obj(vec![
        (
            "layout",
            J::Obj(vec![
                ("columnWidths", ints(&layout.column_widths)),
                ("rowHeights", ints(&layout.row_heights)),
                ("columnOffsets", ints(&layout.column_offsets)),
                ("rowOffsets", ints(&layout.row_offsets)),
                ("tableWidth", J::Num(layout.table_width as f64)),
                ("tableHeight", J::Num(layout.table_height as f64)),
            ]),
        ),
        ("measures", J::Arr(asked)),
    ])
}

fn s(v: &Value, key: &str) -> JsString {
    JsString::from(v[key].as_str().unwrap_or_else(|| panic!("case field {key}")))
}

fn answer(case: &Value) -> J {
    match case["op"].as_str().expect("op") {
        "lex" => match marked::lex(&s(case, "src")) {
            Ok(list) => J::Obj(vec![
                ("tokens", J::Arr(list.tokens.iter().map(token).collect())),
                (
                    "links",
                    J::Arr(
                        js_property_order(&list.links)
                            .into_iter()
                            .map(|(k, v)| {
                                let mut fields = vec![("href", J::js(&v.href))];
                                if let Some(t) = nullable(&v.title) {
                                    fields.push(("title", t));
                                }
                                J::Arr(vec![J::js(k), J::Obj(fields)])
                            })
                            .collect(),
                    ),
                ),
            ]),
            Err(e) => J::Obj(vec![("error", J::s(&e.0))]),
        },
        "lexInline" => match marked::lex_inline(&s(case, "src")) {
            Ok(tokens) => J::Obj(vec![("tokens", J::Arr(tokens.iter().map(token).collect()))]),
            Err(e) => J::Obj(vec![("error", J::s(&e.0))]),
        },
        "markdown" => markdown_case(case),
        "highlight" => highlight_case(case),
        "code" => code_case(case),
        "markdownRender" => markdown_render_case(case),
        "diff" => diff_case(case),
        "table" => table_case(case),
        other => panic!("unknown op {other}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input = std::fs::File::open(&args[1]).expect("input file");
    let mut output = std::io::BufWriter::new(std::fs::File::create(&args[2]).expect("output file"));
    let trace = std::env::var_os("MARKUP_DRIVER_TRACE").is_some();
    for (index, line) in std::io::BufReader::new(input).lines().enumerate() {
        let line = line.expect("line");
        if line.is_empty() {
            continue;
        }
        let case: Value = serde_json::from_str(&line).expect("case JSON");
        if trace {
            eprintln!("case {index}");
        }
        let started = std::time::Instant::now();
        let mut out = String::new();
        answer(&case).write(&mut out);
        if started.elapsed().as_millis() > 2000 {
            eprintln!("slow case {index}: {} ms: {}", started.elapsed().as_millis(), &line[..line.len().min(300)]);
        }
        out.push('\n');
        output.write_all(out.as_bytes()).expect("write");
    }
}
