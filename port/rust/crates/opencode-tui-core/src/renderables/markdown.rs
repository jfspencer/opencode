//! `<markdown>`: the drawing side of `Markdown.ts` (MarkdownRenderable) on `opencode-tui-markup`'s block model
//! ([`opencode_tui_markup::markdown::Markdown`]). The model decides which child renderables TS creates, updates
//! in place or replaces (as [`Node`]s with uids); this renderable mirrors them into real children: a `<code>` per
//! code node (markdown blocks with the link hooks), boxes for blockquotes, lists, list items and rules, `<text>`
//! list markers and `<text_table>`s.
//!
//! A setter changes the model and mirrors at once (TS builds its children inside the setter); style changes
//! (`syntaxStyle`, `fg`, `bg`, `conceal`, `concealCode`) wait for the lifecycle pass before layout, as
//! `rerenderDirtyStyles` does.
//!
//! Divergences: `renderNode` is not ported (see the model's module doc); `treeSitterClient` is the shared client.
use std::any::Any;
use std::rc::Rc;

use opencode_tui_markup::js::JsString;
use opencode_tui_markup::links::{add_markdown_link_highlights, detect_links};
use opencode_tui_markup::markdown::{
    BoxNode, BoxRole, CodeNode, InternalBlockMode, Markdown, MarkdownOptions, MarkdownTableOptions, Node, NodeUid,
    TableNode, TextNode,
};
use opencode_tui_markup::style::{Rgba, SyntaxStyle, TextChunk};

use crate::color::RGBA;
use crate::layout::Value;
use crate::renderables::box_::{BorderProp, Side};
use crate::renderables::code::{CodeOptions, OnChunks, OnHighlight, create_code_with};
use crate::renderables::text::TextChunk as CoreChunk;
use crate::tree::{self, Edge, Element, FlexDirection, Kind};

/// One mirrored child: the model node's uid, its renderable, and what was last replayed on it.
struct Mirror {
    uid: NodeUid,
    el: Element,
    /// A code node's `applies` count, a table's `content_sets` count.
    version: u64,
    /// A code node's `onHighlight` generation.
    hooks: Option<u64>,
    children: Vec<Mirror>,
}

/// `MarkdownRenderable`.
pub struct MarkdownKind {
    pub model: Markdown,
    mirrors: Vec<Mirror>,
}

impl Kind for MarkdownKind {
    fn name(&self) -> &'static str {
        "markdown"
    }
    fn paints(&self) -> bool {
        false
    }
    fn needs_lifecycle_pass(&self) -> bool {
        self.model.style_dirty()
    }
    fn lifecycle_pass(&mut self, el: Element) {
        self.model.rerender_dirty_styles();
        self.mirror(el);
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn color(value: Option<Rgba>) -> Option<RGBA> {
    value.map(|c| RGBA(c.0))
}

fn core_chunks(chunks: &[TextChunk]) -> Vec<CoreChunk> {
    chunks
        .iter()
        .map(|c| CoreChunk {
            text: c.text.to_string(),
            fg: color(c.fg),
            bg: color(c.bg),
            attributes: c.attributes.unwrap_or(0),
            link: c.link.as_ref().map(|l| l.to_string()),
        })
        .collect()
}

fn margins(el: Element, top: f64, bottom: f64) {
    el.set_margin(Edge::Top, Some(Value::Point(top as f32)));
    el.set_margin(Edge::Bottom, Some(Value::Point(bottom as f32)));
}

impl MarkdownKind {
    /// The markdown hooks of a code child: `addMarkdownLinkHighlights` with this renderable's conceal and
    /// hyperlink support, and `detectLinks`.
    fn hooks(&self) -> (OnHighlight, OnChunks) {
        let conceal = self.model.conceal();
        let hyperlinks = self.model.hyperlinks();
        let highlight: OnHighlight = Rc::new(move |highlights, content: &JsString, _filetype, _style| {
            Some(add_markdown_link_highlights(highlights, content, conceal, hyperlinks))
        });
        let chunks: OnChunks = Rc::new(move |chunks, context| {
            Some(detect_links(chunks, context.content, context.highlights, context.source_ranges, Some(context.syntax_style)))
        });
        (highlight, chunks)
    }

    fn syntax(&self) -> Rc<SyntaxStyle> {
        self.model.syntax_style().cloned().unwrap_or_else(|| Rc::new(SyntaxStyle::new()))
    }

    /// Mirrors the model's children into the node's children.
    fn mirror(&mut self, el: Element) {
        let nodes: Vec<Node> = self.model.children().cloned().collect();
        let mut mirrors = std::mem::take(&mut self.mirrors);
        self.reconcile(el, &nodes, &mut mirrors);
        self.mirrors = mirrors;
    }

    fn reconcile(&self, parent: Element, nodes: &[Node], mirrors: &mut Vec<Mirror>) {
        let mut old: Vec<Mirror> = std::mem::take(mirrors);
        // Renderables the model replaced or dropped go first (`destroyRecursively`).
        old.retain(|m| {
            let kept = nodes.iter().any(|n| n.uid() == m.uid);
            if !kept {
                m.el.destroy_recursively();
            }
            kept
        });
        for (index, node) in nodes.iter().enumerate() {
            let mirror = match old.iter().position(|m| m.uid == node.uid()) {
                Some(found) => {
                    let mut mirror = old.remove(found);
                    self.update(&mut mirror, node);
                    mirror
                }
                None => self.create(node),
            };
            let children = parent.children();
            if children.get(index) != Some(&mirror.el) {
                parent.insert_before(mirror.el, children.get(index).copied().filter(|c| *c != mirror.el));
            }
            mirrors.push(mirror);
        }
    }

    fn create(&self, node: &Node) -> Mirror {
        match node {
            Node::Code(code) => self.create_code(code),
            Node::Box(b) => {
                let el = crate::elements::box_();
                el.set_id(&b.id);
                self.box_props(el, b, true);
                let mut children = Vec::new();
                self.reconcile(el, &b.children, &mut children);
                Mirror { uid: b.uid, el, version: 0, hooks: None, children }
            }
            Node::Text(t) => {
                let el = crate::elements::text();
                el.set_id(&t.id);
                self.text_props(el, t);
                el.set_flex_shrink(Some(0.0));
                Mirror { uid: t.uid, el, version: 0, hooks: None, children: Vec::new() }
            }
            Node::Table(t) => {
                let el = crate::renderables::text_table::create_text_table();
                el.set_id(&t.id);
                self.table_props(el, t, true);
                Mirror { uid: t.uid, el, version: t.content_sets, hooks: None, children: Vec::new() }
            }
        }
    }

    fn update(&self, mirror: &mut Mirror, node: &Node) {
        match node {
            Node::Code(code) => {
                if code.applies != mirror.version {
                    mirror.version = code.applies;
                    self.apply_code(mirror, code);
                }
                // A list child's top margin changes without an apply.
                margins(mirror.el, code.margin_top, code.margin_bottom);
            }
            Node::Box(b) => {
                self.box_props(mirror.el, b, false);
                let mut children = std::mem::take(&mut mirror.children);
                self.reconcile(mirror.el, &b.children, &mut children);
                mirror.children = children;
            }
            Node::Text(t) => self.text_props(mirror.el, t),
            Node::Table(t) => {
                let content = t.content_sets != mirror.version;
                mirror.version = t.content_sets;
                self.table_props(mirror.el, t, content);
            }
        }
    }

    fn create_code(&self, code: &CodeNode) -> Mirror {
        let (on_highlight, on_chunks) = match code.markdown_hooks {
            Some(_) => {
                let (h, c) = self.hooks();
                (Some(h), Some(c))
            }
            None => (None, None),
        };
        let el = create_code_with(CodeOptions {
            content: code.content.clone(),
            filetype: code.filetype.clone(),
            syntax_style: self.syntax(),
            conceal: code.conceal,
            draw_unstyled_text: code.draw_unstyled_text,
            streaming: code.streaming,
            initial_styled_text: code.initial_styled_text.clone(),
            base_highlight: code.base_highlight.clone(),
            on_highlight,
            on_chunks,
        });
        el.set_id(&code.id);
        el.set_fg(color(code.fg));
        el.set_bg(color(code.bg));
        el.set_width(Some(Value::Percent(100.0)));
        margins(el, code.margin_top, code.margin_bottom);
        Mirror { uid: code.uid, el, version: code.applies, hooks: code.markdown_hooks, children: Vec::new() }
    }

    /// Replays an in-place update in TS's order (`applyMarkdownCodeRenderable` / `applyCodeBlockRenderable`).
    fn apply_code(&self, mirror: &mut Mirror, code: &CodeNode) {
        let el = mirror.el;
        if code.markdown_hooks.is_some() {
            el.apply_initial_styled_text(code.content.clone(), code.initial_styled_text.clone());
        }
        el.set_filetype(code.filetype.clone());
        el.set_syntax_style(self.syntax());
        el.set_fg(color(code.fg));
        el.set_bg(color(code.bg));
        el.set_conceal(code.conceal);
        el.set_draw_unstyled_text(code.draw_unstyled_text);
        el.set_streaming(code.streaming);
        if code.markdown_hooks.is_some() {
            el.set_base_highlight(code.base_highlight.clone());
            let same = mirror.hooks == code.markdown_hooks;
            mirror.hooks = code.markdown_hooks;
            let (highlight, _) = self.hooks();
            el.set_on_highlight(Some(highlight), same);
        }
        el.set_code_content(code.content.clone());
    }

    fn box_props(&self, el: Element, b: &BoxNode, created: bool) {
        if created {
            match b.role {
                BoxRole::Blockquote => {
                    el.set_width(Some(Value::Percent(100.0)));
                    el.set_border(BorderProp::Sides(vec![Side::Left]));
                    el.set_padding(Edge::Left, Some(Value::Point(1.0)));
                    el.set_flex_shrink(Some(0.0));
                }
                BoxRole::List => {
                    el.set_width(Some(Value::Percent(100.0)));
                    el.set_flex_direction(FlexDirection::Column);
                    el.set_flex_shrink(Some(0.0));
                }
                BoxRole::ListItem => {
                    el.set_width(Some(Value::Percent(100.0)));
                    el.set_flex_direction(FlexDirection::Row);
                    el.set_flex_shrink(Some(0.0));
                }
                BoxRole::ListItemContent => {
                    el.set_flex_direction(FlexDirection::Column);
                    el.set_flex_grow(Some(1.0));
                    el.set_flex_shrink(Some(1.0));
                }
                BoxRole::Hr => {
                    el.set_width(Some(Value::Percent(100.0)));
                    el.set_height(Some(Value::Point(1.0)));
                    el.set_border(BorderProp::Sides(vec![Side::Top]));
                    el.set_flex_shrink(Some(0.0));
                }
            }
        }
        if let Some(border) = b.border_color {
            el.set_border_color(Some(RGBA(border.0)));
        }
        margins(el, b.margin_top, b.margin_bottom);
    }

    fn text_props(&self, el: Element, t: &TextNode) {
        el.set_styled_content(core_chunks(&t.content.chunks));
        el.set_width(Some(Value::Point(t.width as f32)));
        margins(el, t.margin_top, t.margin_bottom);
    }

    fn table_props(&self, el: Element, t: &TableNode, content: bool) {
        if content {
            el.set_table_content(t.content.clone());
        }
        el.set_table_options(t.options.clone());
        el.set_width(Some(Value::Percent(100.0)));
        margins(el, t.margin_top, t.margin_bottom);
    }
}

/// `new MarkdownRenderable(ctx, { id })`: flexDirection column, flexShrink 0, no content yet.
pub fn create_markdown() -> Element {
    let el = tree::create(
        Box::new(MarkdownKind { model: Markdown::new(MarkdownOptions::default()), mirrors: Vec::new() }),
        "markdown",
        false,
    );
    el.set_flex_direction(FlexDirection::Column);
    el.set_flex_shrink(Some(0.0));
    el
}

impl Element {
    fn with_markdown(self, f: impl FnOnce(&mut Markdown)) {
        self.with_kind::<MarkdownKind, _>(|k| {
            f(&mut k.model);
            k.mirror(self);
        });
        self.request_render();
    }

    /// `content = value` on a `<markdown>`.
    pub fn set_markdown_content(self, value: &str) {
        self.with_markdown(|m| m.set_content(JsString::from(value)));
    }

    /// `syntaxStyle = value` (by identity).
    pub fn set_markdown_syntax_style(self, value: Rc<SyntaxStyle>) {
        self.with_markdown(|m| m.set_syntax_style(Some(value)));
    }

    pub fn set_markdown_fg(self, value: Option<RGBA>) {
        self.with_markdown(|m| m.set_fg(value.map(|c| Rgba(c.0))));
    }

    pub fn set_markdown_bg(self, value: Option<RGBA>) {
        self.with_markdown(|m| m.set_bg(value.map(|c| Rgba(c.0))));
    }

    pub fn set_markdown_conceal(self, value: bool) {
        self.with_markdown(|m| m.set_conceal(value));
    }

    pub fn set_markdown_conceal_code(self, value: bool) {
        self.with_markdown(|m| m.set_conceal_code(value));
    }

    pub fn set_markdown_streaming(self, value: bool) {
        self.with_markdown(|m| m.set_streaming(value));
    }

    pub fn set_markdown_table_options(self, value: Option<MarkdownTableOptions>) {
        self.with_markdown(|m| m.set_table_options(value));
    }

    pub fn set_markdown_internal_block_mode(self, value: InternalBlockMode) {
        self.with_markdown(|m| m.set_internal_block_mode(value));
    }

    /// `markdown.content`.
    pub fn markdown_content(self) -> String {
        self.with_kind::<MarkdownKind, _>(|k| k.model.content().to_string()).unwrap_or_default()
    }
}
