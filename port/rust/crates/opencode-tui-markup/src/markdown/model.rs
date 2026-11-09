//! `MarkdownRenderable` (Markdown.ts) without drawing: the block states, which child renderables it would create,
//! update in place or replace, and their props. A renderer mirrors [`Markdown::children`] into real renderables
//! (see the crate docs of the module).

use super::inline::InlineContext;
use super::parser::{parse_markdown_incremental, ParseState};
use super::table::{TableContentCache, TextTableContent};
use crate::js::{JsRegex, JsStr, JsString};
use crate::marked::{Token, TokenType};
use crate::resolve_ft::info_string_to_filetype;
use crate::style::{Rgba, StyleDefinition, StyledText, SyntaxStyle, TextChunk};
use std::rc::Rc;
use std::sync::LazyLock;

static TRAILING_MARKDOWN_BLOCK_BREAKS_RE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile(r"(?:\r?\n){2,}$", ""));
static TRAILING_MARKDOWN_BLOCK_NEWLINES_RE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile(r"(?:\r?\n)+$", ""));
static LIST_ITEM_BLANK_END_RE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile(r"\n[ \t]*\n$", ""));

/// `internalBlockMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InternalBlockMode {
    #[default]
    Coalesced,
    TopLevel,
}

/// `MarkdownTableStyle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownTableStyle {
    Grid,
    Columns,
}

/// `TextTableColumnWidthMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnWidthMode {
    Content,
    Full,
}

/// `TextTableColumnFitter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnFitter {
    Proportional,
    Balanced,
}

/// A text wrap mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapMode {
    None,
    Char,
    Word,
}

/// `BorderStyle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderStyle {
    Single,
    Double,
    Rounded,
    Heavy,
}

/// `MarkdownTableOptions`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarkdownTableOptions {
    pub style: Option<MarkdownTableStyle>,
    pub width_mode: Option<ColumnWidthMode>,
    pub column_fitter: Option<ColumnFitter>,
    pub wrap_mode: Option<WrapMode>,
    pub cell_padding: Option<f64>,
    pub cell_padding_x: Option<f64>,
    pub cell_padding_y: Option<f64>,
    pub borders: Option<bool>,
    pub outer_border: Option<bool>,
    pub border_style: Option<BorderStyle>,
    pub border_color: Option<Rgba>,
    pub selectable: Option<bool>,
}

/// `ResolvedTableRenderableOptions`: the TextTableRenderable props of a markdown table.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTableOptions {
    pub column_width_mode: ColumnWidthMode,
    pub column_fitter: ColumnFitter,
    pub wrap_mode: WrapMode,
    pub cell_padding: f64,
    pub cell_padding_x: f64,
    pub cell_padding_y: f64,
    pub column_gap: f64,
    pub border: bool,
    pub outer_border: bool,
    pub show_borders: bool,
    pub border_style: BorderStyle,
    pub border_color: Rgba,
    pub fg: Option<Rgba>,
    pub selectable: bool,
}

/// The identity of a child renderable: a new value means TS created a new renderable object.
pub type NodeUid = u64;

/// A `CodeRenderable` child. Props are as last set; `applies` counts the in-place updates
/// (`applyMarkdownCodeRenderable` / `applyCodeBlockRenderable`), each of which a renderer replays by setting the
/// props in TS order (`initialStyledText` first, which TS routes to `updateStreamingPreview` while streaming
/// and highlighting, then filetype, syntaxStyle, fg, bg, conceal, drawUnstyledText, streaming, baseHighlight,
/// onHighlight, content, marginBottom).
#[derive(Clone, Debug, PartialEq)]
pub struct CodeNode {
    pub uid: NodeUid,
    pub id: String,
    pub content: JsString,
    pub filetype: Option<String>,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub conceal: bool,
    pub draw_unstyled_text: bool,
    pub streaming: bool,
    pub initial_styled_text: Option<Rc<StyledText>>,
    pub base_highlight: Option<String>,
    /// For markdown text blocks: `onHighlight` is the renderable's `addMarkdownLinkHighlights` hook (this value
    /// changes when TS assigns a new hook function) and `onChunks` is `detectLinks`. `None` for fenced code.
    pub markdown_hooks: Option<u64>,
    pub margin_top: f64,
    pub margin_bottom: f64,
    pub applies: u64,
}

/// What a `BoxRenderable` child is for; each role has the fixed layout props noted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxRole {
    /// width 100%, border ["left"], paddingLeft 1, flexShrink 0; one markdown code child (baseHighlight
    /// markup.quote).
    Blockquote,
    /// width 100%, flexDirection column, flexShrink 0.
    List,
    /// An item row: width 100%, flexDirection row, flexShrink 0; children: the marker text, the content box.
    ListItem,
    /// flexDirection column, flexGrow 1, flexShrink 1.
    ListItemContent,
    /// width 100%, height 1, border ["top"], flexShrink 0.
    Hr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoxNode {
    pub uid: NodeUid,
    pub id: String,
    pub role: BoxRole,
    /// Blockquote and hr border colour.
    pub border_color: Option<Rgba>,
    pub children: Vec<Node>,
    pub margin_top: f64,
    pub margin_bottom: f64,
}

/// A `TextRenderable` child: a list marker (width as given, flexShrink 0).
#[derive(Clone, Debug, PartialEq)]
pub struct TextNode {
    pub uid: NodeUid,
    pub id: String,
    pub content: StyledText,
    pub width: f64,
    pub margin_top: f64,
    pub margin_bottom: f64,
}

/// A `TextTableRenderable` child (width 100%). `content_sets` counts assignments to `content`.
#[derive(Clone, Debug, PartialEq)]
pub struct TableNode {
    pub uid: NodeUid,
    pub id: String,
    pub content: TextTableContent,
    pub options: ResolvedTableOptions,
    pub margin_top: f64,
    pub margin_bottom: f64,
    pub content_sets: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Code(CodeNode),
    Box(BoxNode),
    Text(TextNode),
    Table(TableNode),
}

impl Node {
    pub fn uid(&self) -> NodeUid {
        match self {
            Node::Code(n) => n.uid,
            Node::Box(n) => n.uid,
            Node::Text(n) => n.uid,
            Node::Table(n) => n.uid,
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Node::Code(n) => &n.id,
            Node::Box(n) => &n.id,
            Node::Text(n) => &n.id,
            Node::Table(n) => &n.id,
        }
    }

    fn margins(&mut self) -> (&mut f64, &mut f64) {
        match self {
            Node::Code(n) => (&mut n.margin_top, &mut n.margin_bottom),
            Node::Box(n) => (&mut n.margin_top, &mut n.margin_bottom),
            Node::Text(n) => (&mut n.margin_top, &mut n.margin_bottom),
            Node::Table(n) => (&mut n.margin_top, &mut n.margin_bottom),
        }
    }

    pub fn margin_top(&self) -> f64 {
        match self {
            Node::Code(n) => n.margin_top,
            Node::Box(n) => n.margin_top,
            Node::Text(n) => n.margin_top,
            Node::Table(n) => n.margin_top,
        }
    }

    pub fn margin_bottom(&self) -> f64 {
        match self {
            Node::Code(n) => n.margin_bottom,
            Node::Box(n) => n.margin_bottom,
            Node::Text(n) => n.margin_bottom,
            Node::Table(n) => n.margin_bottom,
        }
    }

    fn set_margin_top(&mut self, v: f64) {
        *self.margins().0 = v;
    }

    fn set_margin_bottom(&mut self, v: f64) {
        *self.margins().1 = v;
    }

    fn children_mut(&mut self) -> Option<&mut Vec<Node>> {
        match self {
            Node::Box(b) => Some(&mut b.children),
            _ => None,
        }
    }
}

/// `BlockState`.
#[derive(Clone, Debug)]
pub struct BlockState {
    pub token: Rc<Token>,
    pub token_raw: JsString,
    pub margin_top: Option<f64>,
    pub node: Node,
    pub table_content_cache: Option<TableContentCache>,
    pub tracks_inter_block_margin: Option<bool>,
    pub can_update_in_place: bool,
}

/// `MarkdownRenderBlock`.
struct RenderBlock {
    token: Rc<Token>,
    source_token_end: usize,
    margin_top: f64,
}

/// `ListItemRenderInput`.
struct ListItemInput<'a> {
    item: &'a Token,
    marker: JsString,
    marker_width: usize,
    id: String,
}

/// `MarkdownOptions` (the content side).
#[derive(Clone, Debug)]
pub struct MarkdownOptions {
    /// The renderable's id; child ids derive from it.
    pub id: String,
    pub content: JsString,
    pub syntax_style: Option<Rc<SyntaxStyle>>,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub conceal: bool,
    pub conceal_code: bool,
    pub streaming: bool,
    pub table_options: Option<MarkdownTableOptions>,
    pub internal_block_mode: InternalBlockMode,
    /// `ctx.capabilities?.hyperlinks === true`.
    pub hyperlinks: bool,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        MarkdownOptions {
            id: "markdown".into(),
            content: JsString::new(),
            syntax_style: None,
            fg: None,
            bg: None,
            conceal: true,
            conceal_code: false,
            streaming: false,
            table_options: None,
            internal_block_mode: InternalBlockMode::Coalesced,
            hyperlinks: false,
        }
    }
}

/// The content state of a `MarkdownRenderable`.
#[derive(Debug)]
pub struct Markdown {
    id: String,
    content: JsString,
    syntax_style: Option<Rc<SyntaxStyle>>,
    fg: Option<Rgba>,
    bg: Option<Rgba>,
    conceal: bool,
    conceal_code: bool,
    table_options: Option<MarkdownTableOptions>,
    internal_block_mode: InternalBlockMode,
    hyperlinks: bool,
    streaming: bool,
    pub parse_state: Option<ParseState>,
    pub block_states: Vec<BlockState>,
    pub stable_block_count: usize,
    style_dirty: bool,
    /// The generation of `_highlightMarkdownLinks` (TS replaces the function on a capability change).
    hook_generation: u64,
    next_uid: NodeUid,
}

impl Markdown {
    /// `new MarkdownRenderable(ctx, options)`.
    pub fn new(options: MarkdownOptions) -> Markdown {
        let mut md = Markdown {
            id: options.id,
            content: options.content,
            syntax_style: options.syntax_style,
            fg: options.fg,
            bg: options.bg,
            conceal: options.conceal,
            conceal_code: options.conceal_code,
            table_options: options.table_options,
            internal_block_mode: options.internal_block_mode,
            hyperlinks: options.hyperlinks,
            streaming: options.streaming,
            parse_state: None,
            block_states: Vec::new(),
            stable_block_count: 0,
            style_dirty: false,
            hook_generation: 0,
            next_uid: 1,
        };
        md.update_blocks(false);
        md
    }

    /// The child renderables in order.
    pub fn children(&self) -> impl Iterator<Item = &Node> {
        self.block_states.iter().map(|s| &s.node)
    }

    pub fn content(&self) -> &JsString {
        &self.content
    }

    /// The `onHighlight` generation markdown code children carry (see [`CodeNode::markdown_hooks`]).
    pub fn hook_generation(&self) -> u64 {
        self.hook_generation
    }

    pub fn conceal(&self) -> bool {
        self.conceal
    }

    pub fn hyperlinks(&self) -> bool {
        self.hyperlinks
    }

    pub fn streaming(&self) -> bool {
        self.streaming
    }

    pub fn syntax_style(&self) -> Option<&Rc<SyntaxStyle>> {
        self.syntax_style.as_ref()
    }

    /// `set content`.
    pub fn set_content(&mut self, value: JsString) {
        if self.content != value {
            self.content = value;
            self.update_blocks(false);
        }
    }

    /// `set syntaxStyle` (by identity, as TS compares objects).
    pub fn set_syntax_style(&mut self, value: Option<Rc<SyntaxStyle>>) {
        let same = match (&self.syntax_style, &value) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.syntax_style = value;
            self.style_dirty = true;
        }
    }

    pub fn set_fg(&mut self, value: Option<Rgba>) {
        if self.fg != value {
            self.fg = value;
            self.style_dirty = true;
        }
    }

    pub fn set_bg(&mut self, value: Option<Rgba>) {
        if self.bg != value {
            self.bg = value;
            self.style_dirty = true;
        }
    }

    pub fn set_conceal(&mut self, value: bool) {
        if self.conceal != value {
            self.conceal = value;
            self.style_dirty = true;
        }
    }

    pub fn set_conceal_code(&mut self, value: bool) {
        if self.conceal_code != value {
            self.conceal_code = value;
            self.style_dirty = true;
        }
    }

    /// Whether a style change waits for the lifecycle pass (`_needsLifecyclePass`).
    pub fn style_dirty(&self) -> bool {
        self.style_dirty
    }

    /// `rerenderDirtyStyles()`: the lifecycle pass before layout.
    pub fn rerender_dirty_styles(&mut self) {
        if !self.style_dirty {
            return;
        }
        self.style_dirty = false;
        self.rerender_blocks();
    }

    /// `refreshStyles()`.
    pub fn refresh_styles(&mut self) {
        self.style_dirty = false;
        self.rerender_blocks();
    }

    /// `set streaming`.
    pub fn set_streaming(&mut self, value: bool) {
        if self.streaming != value {
            self.streaming = value;
            self.update_blocks(true);
        }
    }

    /// `set tableOptions`.
    pub fn set_table_options(&mut self, value: Option<MarkdownTableOptions>) {
        self.table_options = value;
        self.apply_table_options_to_blocks();
    }

    /// `set internalBlockMode`.
    pub fn set_internal_block_mode(&mut self, value: InternalBlockMode) {
        if self.internal_block_mode != value {
            self.internal_block_mode = value;
            self.update_blocks(true);
        }
    }

    /// `clearCache()`.
    pub fn clear_cache(&mut self) {
        self.parse_state = None;
        self.clear_block_states();
        self.update_blocks(false);
    }

    /// The `capabilities` listener: TS calls `handleCapabilities` when hyperlink support flips.
    pub fn set_hyperlinks(&mut self, value: bool) {
        if self.hyperlinks != value {
            self.hyperlinks = value;
            self.handle_capabilities();
        }
    }

    fn uid(&mut self) -> NodeUid {
        let uid = self.next_uid;
        self.next_uid += 1;
        uid
    }

    fn inline_ctx(&self) -> InlineContext<'_> {
        InlineContext {
            syntax_style: self.syntax_style.as_deref(),
            conceal: self.conceal,
            hyperlinks: self.hyperlinks,
            streaming: self.streaming,
        }
    }

    fn get_style(&self, group: &str) -> Option<StyleDefinition> {
        self.inline_ctx().get_style(group)
    }

    fn create_chunk(&self, text: JsString, group: &str) -> TextChunk {
        self.inline_ctx().create_chunk(text, group, None)
    }

    /// `createInitialStyledText(token)`.
    fn create_initial_styled_text(&self, token: &Token) -> Option<Rc<StyledText>> {
        self.inline_ctx().create_initial_styled_text(token).map(Rc::new)
    }

    /// `handleCapabilities()`.
    fn handle_capabilities(&mut self) {
        if !self.content.includes(&[b']' as u16]) {
            return;
        }
        self.hook_generation += 1;

        let states = std::mem::take(&mut self.block_states);
        let mut states = states;
        for state in states.iter_mut() {
            let token = state.token.clone();
            let is_top = true;
            let cache = state.table_content_cache.take();
            let new_cache = self.capabilities_walk(&mut state.node, Some(&token), is_top, cache.as_ref());
            state.table_content_cache = match new_cache {
                Some(c) => Some(c),
                None => cache,
            };
        }
        self.block_states = states;
    }

    /// The walk of `handleCapabilities` over one renderable; returns a new table cache for a top-level table.
    fn capabilities_walk(
        &mut self,
        node: &mut Node,
        token: Option<&Token>,
        is_top: bool,
        top_cache: Option<&TableContentCache>,
    ) -> Option<TableContentCache> {
        if let (Node::Table(table), Some(token)) = (&mut *node, token) {
            if token.kind == TokenType::Table {
                let (cache, _) = self.build_table_content_cache(token, if is_top { top_cache } else { None }, true);
                if let Some(cache) = cache {
                    table.content = cache.content.clone();
                    table.content_sets += 1;
                    if is_top {
                        return Some(cache);
                    }
                }
                return None;
            }
        }
        let previous = self.hook_generation - 1;
        if let Node::Code(code) = &mut *node {
            if code.filetype.as_deref() == Some("markdown") && code.markdown_hooks == Some(previous) {
                if self.streaming {
                    if let Some(token) = token {
                        code.initial_styled_text = self.create_initial_styled_text(token);
                        code.applies += 1;
                    }
                }
                code.markdown_hooks = Some(self.hook_generation);
            }
        }
        if let (Some(token), Node::Box(list)) = (token, &mut *node) {
            if token.kind == TokenType::List && list.role == BoxRole::List {
                let items = token.items.as_deref().unwrap_or(&[]);
                for (index, item) in items.iter().enumerate() {
                    let tokens = renderable_list_item_tokens(item);
                    let Some(Node::Box(row)) = list.children.get_mut(index) else { continue };
                    let Some(Node::Box(content)) = row.children.get_mut(1) else { continue };
                    for (ci, child) in content.children.iter_mut().enumerate() {
                        self.capabilities_walk(child, tokens.get(ci).copied(), false, None);
                    }
                }
                return None;
            }
        }
        if let Some(children) = node.children_mut() {
            for child in children.iter_mut() {
                self.capabilities_walk(child, token, false, None);
            }
        }
        None
    }

    // ---- renderable construction

    /// `createMarkdownCodeRenderable(content, id, marginBottom, onChunks, baseHighlight, initialStyledText)`.
    fn create_markdown_code_renderable(
        &mut self,
        content: JsString,
        id: String,
        margin_bottom: f64,
        base_highlight: Option<&str>,
        initial_styled_text: Option<Rc<StyledText>>,
    ) -> Node {
        Node::Code(CodeNode {
            uid: self.uid(),
            id,
            content,
            filetype: Some("markdown".into()),
            fg: self.fg,
            bg: self.bg,
            conceal: self.conceal,
            draw_unstyled_text: initial_styled_text.is_some(),
            streaming: true,
            initial_styled_text,
            base_highlight: base_highlight.map(str::to_owned),
            markdown_hooks: Some(self.hook_generation),
            margin_top: 0.0,
            margin_bottom,
            applies: 0,
        })
    }

    fn get_blockquote_content(token: &Token) -> JsString {
        match &token.text {
            Some(t) if !t.is_empty() => t.clone(),
            _ => JsString::from(" "),
        }
    }

    fn get_blockquote_border_color(&self) -> Rgba {
        self.get_style("conceal")
            .and_then(|s| s.fg)
            .or_else(|| self.get_style("default").and_then(|s| s.fg))
            .or(self.fg)
            .unwrap_or_else(|| Rgba::from_hex("#FFFFFF"))
    }

    fn create_blockquote_renderable(&mut self, token: &Token, id: String, margin_bottom: f64) -> Node {
        let uid = self.uid();
        let child = self.create_markdown_code_renderable(
            Self::get_blockquote_content(token),
            format!("{id}-content"),
            0.0,
            Some("markup.quote"),
            None,
        );
        Node::Box(BoxNode {
            uid,
            id,
            role: BoxRole::Blockquote,
            border_color: Some(self.get_blockquote_border_color()),
            children: vec![child],
            margin_top: 0.0,
            margin_bottom,
        })
    }

    fn create_list_renderable(&mut self, token: &Token, id: String, margin_bottom: f64) -> Node {
        let uid = self.uid();
        let mut children = Vec::new();
        for input in list_item_inputs(token, &id) {
            children.push(self.create_list_item_renderable(&input));
        }
        Node::Box(BoxNode { uid, id, role: BoxRole::List, border_color: None, children, margin_top: 0.0, margin_bottom })
    }

    fn marker_text(&self, input: &ListItemInput<'_>) -> StyledText {
        let mut text = JsString::new();
        for _ in input.marker.len()..input.marker_width {
            text.push_literal(" ");
        }
        text.push_str(&input.marker);
        text.push_literal(" ");
        StyledText::new(vec![self.create_chunk(text, "markup.list")])
    }

    fn create_list_item_renderable(&mut self, input: &ListItemInput<'_>) -> Node {
        let row_uid = self.uid();
        let marker_uid = self.uid();
        let marker = Node::Text(TextNode {
            uid: marker_uid,
            id: format!("{}-marker", input.id),
            content: self.marker_text(input),
            width: (input.marker_width + 1) as f64,
            margin_top: 0.0,
            margin_bottom: 0.0,
        });
        let content_uid = self.uid();
        let mut children = Vec::new();
        let mut pending_margin_top = 0.0;
        for (index, child) in input.item.children().iter().enumerate() {
            if child.kind == TokenType::Checkbox {
                continue;
            }
            if child.kind == TokenType::Space {
                pending_margin_top = f64::max(pending_margin_top, 1.0);
                continue;
            }
            let Some(mut node) = self.create_list_child_renderable(child, format!("{}-child-{index}", input.id)) else {
                continue;
            };
            node.set_margin_top(if child.kind == TokenType::List { 0.0 } else { pending_margin_top });
            pending_margin_top = 0.0;
            children.push(node);
        }
        let content = Node::Box(BoxNode {
            uid: content_uid,
            id: format!("{}-content", input.id),
            role: BoxRole::ListItemContent,
            border_color: None,
            children,
            margin_top: 0.0,
            margin_bottom: 0.0,
        });
        Node::Box(BoxNode {
            uid: row_uid,
            id: input.id.clone(),
            role: BoxRole::ListItem,
            border_color: None,
            children: vec![marker, content],
            margin_top: 0.0,
            margin_bottom: if LIST_ITEM_BLANK_END_RE.test(&input.item.raw) { 1.0 } else { 0.0 },
        })
    }

    fn create_list_child_renderable(&mut self, token: &Token, id: String) -> Option<Node> {
        match token.kind {
            TokenType::Text | TokenType::Paragraph => {
                let initial = self.create_initial_styled_text(token);
                Some(self.create_markdown_code_renderable(normalize_scrollback_block_raw(&token.raw), id, 0.0, None, initial))
            }
            TokenType::List => Some(self.create_list_renderable(token, id, 0.0)),
            TokenType::Code => Some(self.create_code_renderable(token, id, 0.0)),
            TokenType::Blockquote => Some(self.create_blockquote_renderable(token, id, 0.0)),
            TokenType::Hr => Some(self.create_horizontal_rule_renderable(id, 0.0)),
            TokenType::Table => Some(self.create_table_block(token, id, 0.0, None, false).0),
            _ => {
                if token.raw.is_empty() {
                    None
                } else {
                    let initial = self.create_initial_styled_text(token);
                    Some(self.create_markdown_code_renderable(token.raw.clone(), id, 0.0, None, initial))
                }
            }
        }
    }

    fn get_horizontal_rule_color(&self) -> Rgba {
        self.get_style("conceal").and_then(|s| s.fg).or(self.fg).unwrap_or_else(|| Rgba::from_hex("#888888"))
    }

    fn create_horizontal_rule_renderable(&mut self, id: String, margin_bottom: f64) -> Node {
        Node::Box(BoxNode {
            uid: self.uid(),
            id,
            role: BoxRole::Hr,
            border_color: Some(self.get_horizontal_rule_color()),
            children: Vec::new(),
            margin_top: 0.0,
            margin_bottom,
        })
    }

    fn create_code_renderable(&mut self, token: &Token, id: String, margin_bottom: f64) -> Node {
        Node::Code(CodeNode {
            uid: self.uid(),
            id,
            content: token.text_or_empty(),
            filetype: info_string_to_filetype(&token.lang.clone().unwrap_or_default().to_string()),
            fg: self.fg,
            bg: self.bg,
            conceal: self.conceal_code,
            draw_unstyled_text: !self.streaming,
            streaming: self.streaming,
            initial_styled_text: None,
            base_highlight: None,
            markdown_hooks: None,
            margin_top: 0.0,
            margin_bottom,
            applies: 0,
        })
    }

    // ---- in-place updates

    /// `applyMarkdownCodeRenderable(renderable, content, marginBottom, baseHighlight, initialStyledText)`.
    fn apply_markdown_code_renderable(
        &self,
        node: &mut Node,
        content: JsString,
        margin_bottom: f64,
        base_highlight: Option<&str>,
        initial_styled_text: Option<Rc<StyledText>>,
    ) {
        let Node::Code(code) = node else { return };
        code.applies += 1;
        code.draw_unstyled_text = initial_styled_text.is_some();
        code.initial_styled_text = initial_styled_text;
        code.filetype = Some("markdown".into());
        code.fg = self.fg;
        code.bg = self.bg;
        code.conceal = self.conceal;
        code.streaming = true;
        code.base_highlight = base_highlight.map(str::to_owned);
        code.markdown_hooks = Some(self.hook_generation);
        code.content = content;
        code.margin_bottom = margin_bottom;
    }

    /// `applyBlockquoteRenderable(renderable, token, marginBottom)`.
    fn apply_blockquote_renderable(&mut self, node: &mut Node, token: &Token, margin_bottom: f64) {
        let border = self.get_blockquote_border_color();
        let Node::Box(b) = node else { return };
        b.border_color = Some(border);
        b.margin_bottom = margin_bottom;
        if let Some(child @ Node::Code(_)) = b.children.first_mut() {
            self.apply_markdown_code_renderable(child, Self::get_blockquote_content(token), 0.0, Some("markup.quote"), None);
            return;
        }
        let id = format!("{}-content", b.id);
        b.children.clear();
        let child = self.create_markdown_code_renderable(Self::get_blockquote_content(token), id, 0.0, Some("markup.quote"), None);
        if let Node::Box(b) = node {
            b.children.push(child);
        }
    }

    /// `applyCodeBlockRenderable(renderable, token, marginBottom)`.
    fn apply_code_block_renderable(&self, node: &mut Node, token: &Token, margin_bottom: f64) {
        let Node::Code(code) = node else { return };
        code.applies += 1;
        code.filetype = info_string_to_filetype(&token.lang.clone().unwrap_or_default().to_string());
        code.fg = self.fg;
        code.bg = self.bg;
        code.conceal = self.conceal_code;
        code.draw_unstyled_text = !self.streaming;
        code.streaming = self.streaming;
        code.content = token.text_or_empty();
        code.margin_bottom = margin_bottom;
    }

    /// `applyListRenderable(renderable, token, previousToken, id, marginBottom)`.
    fn apply_list_renderable(
        &mut self,
        node: &mut Node,
        token: &Token,
        previous: Option<&Token>,
        id: &str,
        margin_bottom: f64,
    ) -> bool {
        let Node::Box(list) = node else { return false };
        list.margin_bottom = margin_bottom;
        let inputs = list_item_inputs(token, id);
        let previous_items: &[Token] = previous.and_then(|p| p.items.as_deref()).unwrap_or(&[]);
        let mut rows = std::mem::take(&mut list.children);
        // Rows past the inputs are destroyed afterwards; rows that cannot be updated are replaced at their index.
        for (index, input) in inputs.iter().enumerate() {
            if index < rows.len() {
                let ok = matches!(rows[index], Node::Box(_))
                    && self.apply_list_item_renderable(&mut rows[index], input, previous_items.get(index));
                if ok {
                    continue;
                }
                let replacement = self.create_list_item_renderable(input);
                rows[index] = replacement;
            } else {
                let row = self.create_list_item_renderable(input);
                rows.push(row);
            }
        }
        rows.truncate(inputs.len());
        if let Node::Box(list) = node {
            list.children = rows;
        }
        true
    }

    fn apply_list_item_marker(&self, row: &mut BoxNode, input: &ListItemInput<'_>, previous: Option<&Token>) {
        let margin_bottom = if LIST_ITEM_BLANK_END_RE.test(&input.item.raw) { 1.0 } else { 0.0 };
        let marker_width = (input.marker_width + 1) as f64;
        let text = self.marker_text(input);
        if !matches!(row.children.first(), Some(Node::Text(_))) {
            return;
        }
        if row.margin_bottom != margin_bottom {
            row.margin_bottom = margin_bottom;
        }
        let Some(Node::Text(marker)) = row.children.first_mut() else { return };
        if marker.width != marker_width {
            marker.width = marker_width;
        }
        let first_text = marker.content.chunks.first().map(|c| c.text.clone());
        if previous.is_none() || first_text.as_ref() != Some(&text.chunks[0].text) {
            marker.content = text;
        }
    }

    fn apply_list_item_renderable(&mut self, row: &mut Node, input: &ListItemInput<'_>, previous: Option<&Token>) -> bool {
        let Node::Box(row) = row else { return false };
        self.apply_list_item_marker(row, input, previous);
        if !matches!(row.children.get(1), Some(Node::Box(_))) {
            return false;
        }
        if let Some(p) = previous {
            if p.raw == input.item.raw {
                return true;
            }
        }
        let mut content = row.children.remove(1);
        let ok = self.apply_list_item_children(&mut content, input.item, previous, &input.id);
        row.children.insert(1, content);
        ok
    }

    fn apply_list_item_children(&mut self, content: &mut Node, item: &Token, previous: Option<&Token>, id: &str) -> bool {
        let previous_tokens: Vec<&Token> = previous.map(renderable_list_item_tokens).unwrap_or_default();
        let Node::Box(b) = content else { return false };
        let mut children = std::mem::take(&mut b.children);
        let mut child_index = 0;
        let mut pending_margin_top = 0.0;
        let mut result = true;
        for (token_index, token) in item.children().iter().enumerate() {
            if token.kind == TokenType::Checkbox {
                continue;
            }
            if token.kind == TokenType::Space {
                pending_margin_top = f64::max(pending_margin_top, 1.0);
                continue;
            }
            let child_id = format!("{id}-child-{token_index}");
            let margin_top = if token.kind == TokenType::List { 0.0 } else { pending_margin_top };
            pending_margin_top = 0.0;
            if child_index >= children.len() {
                let Some(mut node) = self.create_list_child_renderable(token, child_id) else {
                    result = false;
                    break;
                };
                node.set_margin_top(margin_top);
                children.insert(child_index, node);
                child_index += 1;
                continue;
            }
            if !self.apply_list_child_renderable(&mut children[child_index], token, previous_tokens.get(child_index).copied(), &child_id) {
                result = false;
                break;
            }
            children[child_index].set_margin_top(margin_top);
            child_index += 1;
        }
        if result {
            children.truncate(child_index);
        }
        if let Node::Box(b) = content {
            b.children = children;
        }
        result
    }

    fn apply_list_child_renderable(&mut self, node: &mut Node, token: &Token, previous: Option<&Token>, id: &str) -> bool {
        match (token.kind, &*node) {
            (TokenType::Text | TokenType::Paragraph, Node::Code(_)) => {
                self.apply_markdown_code_renderable(node, normalize_scrollback_block_raw(&token.raw), 0.0, None, None);
                true
            }
            (TokenType::List, Node::Box(_)) => self.apply_list_renderable(node, token, previous, id, 0.0),
            (TokenType::Code, Node::Code(_)) => {
                self.apply_code_block_renderable(node, token, 0.0);
                true
            }
            (TokenType::Blockquote, Node::Box(_)) => {
                self.apply_blockquote_renderable(node, token, 0.0);
                true
            }
            _ => previous.is_some_and(|p| p.raw == token.raw),
        }
    }

    // ---- tables

    /// `buildTableContentCache(table, previous, forceRegenerate)`.
    fn build_table_content_cache(
        &self,
        table: &Token,
        previous: Option<&TableContentCache>,
        force: bool,
    ) -> (Option<TableContentCache>, bool) {
        super::table::build_table_content_cache(&self.inline_ctx(), table, previous, force)
    }

    /// `resolveTableStyle(options)`.
    fn resolve_table_style(&self) -> MarkdownTableStyle {
        match self.table_options.as_ref().and_then(|o| o.style) {
            Some(s) => s,
            None => {
                if self.internal_block_mode == InternalBlockMode::TopLevel {
                    MarkdownTableStyle::Columns
                } else {
                    MarkdownTableStyle::Grid
                }
            }
        }
    }

    /// `resolveTableRenderableOptions()`.
    pub fn resolve_table_renderable_options(&self) -> ResolvedTableOptions {
        let style = self.resolve_table_style();
        let o = self.table_options.clone().unwrap_or_default();
        let borders = o.borders.unwrap_or(style == MarkdownTableStyle::Grid);
        let borderless_columns = style == MarkdownTableStyle::Columns && !borders;
        ResolvedTableOptions {
            column_width_mode: o.width_mode.unwrap_or(if style == MarkdownTableStyle::Columns {
                ColumnWidthMode::Content
            } else {
                ColumnWidthMode::Full
            }),
            column_fitter: o.column_fitter.unwrap_or(ColumnFitter::Proportional),
            wrap_mode: o.wrap_mode.unwrap_or(WrapMode::Word),
            cell_padding: o.cell_padding.unwrap_or(0.0),
            cell_padding_x: o.cell_padding_x.or(o.cell_padding).unwrap_or(0.0),
            cell_padding_y: o.cell_padding_y.or(o.cell_padding).unwrap_or(0.0),
            column_gap: if borderless_columns { 2.0 } else { 0.0 },
            border: borders,
            outer_border: o.outer_border.unwrap_or(borders),
            show_borders: borders,
            border_style: o.border_style.unwrap_or(BorderStyle::Single),
            border_color: o
                .border_color
                .or_else(|| self.get_style("conceal").and_then(|s| s.fg))
                .unwrap_or_else(|| Rgba::from_hex("#888888")),
            fg: self.fg,
            selectable: o.selectable.unwrap_or(true),
        }
    }

    fn apply_table_options_to_blocks(&mut self) {
        let options = self.resolve_table_renderable_options();
        for state in &mut self.block_states {
            if let Node::Table(t) = &mut state.node {
                t.options = options.clone();
            }
        }
    }

    fn create_text_table_renderable(&mut self, content: TextTableContent, id: String, margin_bottom: f64) -> Node {
        let options = self.resolve_table_renderable_options();
        Node::Table(TableNode { uid: self.uid(), id, content, options, margin_top: 0.0, margin_bottom, content_sets: 0 })
    }

    /// `createTableBlock(table, id, marginBottom, previousCache, forceRegenerate)`.
    fn create_table_block(
        &mut self,
        table: &Token,
        id: String,
        margin_bottom: f64,
        previous: Option<&TableContentCache>,
        force: bool,
    ) -> (Node, Option<TableContentCache>) {
        let (cache, _) = self.build_table_content_cache(table, previous, force);
        match cache {
            None => (self.create_markdown_code_renderable(table.raw.clone(), id, margin_bottom, None, None), None),
            Some(cache) => {
                let node = self.create_text_table_renderable(cache.content.clone(), id, margin_bottom);
                (node, Some(cache))
            }
        }
    }

    // ---- block structure

    fn get_inter_block_margin(token: &Token, next: Option<&Token>) -> f64 {
        let Some(next) = next else { return 0.0 };
        if should_render_separately(token) {
            return 1.0;
        }
        if !should_render_separately(next) {
            return 0.0;
        }
        if TRAILING_MARKDOWN_BLOCK_NEWLINES_RE.test(&token.raw) { 0.0 } else { 1.0 }
    }

    fn apply_inter_block_margin(state: &mut BlockState, token: &Token, next: Option<&Token>) {
        if state.tracks_inter_block_margin == Some(false) {
            return;
        }
        state.node.set_margin_bottom(Self::get_inter_block_margin(token, next));
    }

    /// `buildRenderableTokens(tokens)` (coalesced mode).
    fn build_renderable_tokens(tokens: &[Rc<Token>]) -> Vec<Rc<Token>> {
        let mut out: Vec<Rc<Token>> = Vec::new();
        let mut markdown_raw = JsString::new();
        let flush = |raw: &mut JsString, out: &mut Vec<Rc<Token>>| {
            if raw.is_empty() {
                return;
            }
            let normalized = TRAILING_MARKDOWN_BLOCK_BREAKS_RE.replace_str(raw, "\n");
            if !normalized.is_empty() {
                let mut t = Token::new(TokenType::Paragraph, normalized.clone());
                t.text = Some(normalized);
                t.tokens = Some(Vec::new());
                out.push(Rc::new(t));
            }
            *raw = JsString::new();
        };
        let mut i = 0;
        while i < tokens.len() {
            let token = &tokens[i];
            if token.kind == TokenType::Space {
                if markdown_raw.is_empty() {
                    i += 1;
                    continue;
                }
                let mut next = i + 1;
                while next < tokens.len() && tokens[next].kind == TokenType::Space {
                    next += 1;
                }
                if let Some(next_token) = tokens.get(next) {
                    if !should_render_separately(next_token) {
                        markdown_raw.push_str(&token.raw);
                    }
                }
                i += 1;
                continue;
            }
            if should_render_separately(token) {
                flush(&mut markdown_raw, &mut out);
                out.push(token.clone());
                i += 1;
                continue;
            }
            markdown_raw.push_str(&token.raw);
            i += 1;
        }
        flush(&mut markdown_raw, &mut out);
        out
    }

    /// `buildTopLevelRenderBlocks(tokens)`.
    fn build_top_level_render_blocks(tokens: &[Rc<Token>]) -> Vec<RenderBlock> {
        let mut blocks: Vec<RenderBlock> = Vec::new();
        let mut gap_before = JsString::new();
        for (i, token) in tokens.iter().enumerate() {
            if token.kind == TokenType::Space {
                gap_before.push_str(&token.raw);
                continue;
            }
            let margin_top = match blocks.last() {
                Some(prev) if should_add_top_level_margin(&prev.token, token, &gap_before) => 1.0,
                _ => 0.0,
            };
            blocks.push(RenderBlock { token: token.clone(), source_token_end: i + 1, margin_top });
            gap_before = JsString::new();
        }
        blocks
    }

    fn get_stable_block_count(&self, blocks: &[RenderBlock], stable_token_count: usize) -> usize {
        if self.internal_block_mode != InternalBlockMode::TopLevel {
            return 0;
        }
        blocks.iter().take_while(|b| b.source_token_end <= stable_token_count).count()
    }

    fn get_top_level_block_raw(token: &Token) -> Option<JsString> {
        if token.raw.is_empty() {
            return None;
        }
        Some(if should_render_separately(token) { token.raw.clone() } else { normalize_scrollback_block_raw(&token.raw) })
    }

    fn create_top_level_default_renderable(&mut self, block: &RenderBlock, index: usize) -> (Option<Node>, Option<TableContentCache>, bool) {
        let token = &*block.token;
        let id = format!("{}-block-{index}", self.id);
        let (mut node, cache) = match token.kind {
            TokenType::Code => (self.create_code_renderable(token, id, 0.0), None),
            TokenType::Table => {
                let (n, c) = self.create_table_block(token, id, 0.0, None, false);
                (n, c)
            }
            TokenType::Blockquote => (self.create_blockquote_renderable(token, id, 0.0), None),
            TokenType::List => (self.create_list_renderable(token, id, 0.0), None),
            TokenType::Hr => (self.create_horizontal_rule_renderable(id, 0.0), None),
            _ => {
                let Some(raw) = Self::get_top_level_block_raw(token) else { return (None, None, true) };
                let initial = self.create_initial_styled_text(token);
                (self.create_markdown_code_renderable(raw, id, 0.0, None, initial), None)
            }
        };
        node.set_margin_top(block.margin_top);
        (Some(node), cache, true)
    }

    fn create_default_renderable(&mut self, token: &Token, index: usize, next: Option<&Token>) -> Option<Node> {
        let id = format!("{}-block-{index}", self.id);
        let margin_bottom = Self::get_inter_block_margin(token, next);
        match token.kind {
            TokenType::Code => Some(self.create_code_renderable(token, id, margin_bottom)),
            TokenType::Blockquote => Some(self.create_blockquote_renderable(token, id, margin_bottom)),
            TokenType::List => Some(self.create_list_renderable(token, id, margin_bottom)),
            TokenType::Hr => Some(self.create_horizontal_rule_renderable(id, margin_bottom)),
            TokenType::Table => Some(self.create_table_block(token, id, margin_bottom, None, false).0),
            TokenType::Space => None,
            _ => {
                if token.raw.is_empty() {
                    return None;
                }
                let initial = self.create_initial_styled_text(token);
                Some(self.create_markdown_code_renderable(token.raw.clone(), id, margin_bottom, None, initial))
            }
        }
    }

    /// `updateBlockRenderable(state, token, index, nextToken, forceRefresh)`.
    fn update_block_renderable(&mut self, state: &mut BlockState, token: &Rc<Token>, index: usize, next: Option<&Token>, force: bool) {
        let margin_bottom = Self::get_inter_block_margin(token, next);
        let id = format!("{}-block-{index}", self.id);
        match token.kind {
            TokenType::Code => {
                self.apply_code_block_renderable(&mut state.node, token, margin_bottom);
            }
            TokenType::Blockquote => {
                self.apply_blockquote_renderable(&mut state.node, token, margin_bottom);
            }
            TokenType::List => {
                let previous = if force { None } else { Some(state.token.clone()) };
                if !self.apply_list_renderable(&mut state.node, token, previous.as_deref(), &id, margin_bottom) {
                    state.node = self.create_list_renderable(token, id, margin_bottom);
                }
            }
            TokenType::Hr => {
                let color = self.get_horizontal_rule_color();
                if let Node::Box(b) = &mut state.node {
                    b.border_color = Some(color);
                }
                state.node.set_margin_bottom(margin_bottom);
            }
            TokenType::Table => {
                let (cache, changed) = self.build_table_content_cache(token, state.table_content_cache.as_ref(), force);
                let Some(cache) = cache else {
                    if matches!(state.node, Node::Code(_)) {
                        self.apply_markdown_code_renderable(&mut state.node, token.raw.clone(), margin_bottom, None, None);
                        state.table_content_cache = None;
                        return;
                    }
                    state.node = self.create_markdown_code_renderable(token.raw.clone(), id, margin_bottom, None, None);
                    state.table_content_cache = None;
                    return;
                };
                if let Node::Table(t) = &mut state.node {
                    if changed {
                        t.content = cache.content.clone();
                        t.content_sets += 1;
                    }
                    t.options = self.resolve_table_renderable_options();
                    t.margin_bottom = margin_bottom;
                    state.table_content_cache = Some(cache);
                    return;
                }
                state.node = self.create_text_table_renderable(cache.content.clone(), id, margin_bottom);
                state.table_content_cache = Some(cache);
            }
            _ => {
                let raw = if self.internal_block_mode == InternalBlockMode::TopLevel {
                    Self::get_top_level_block_raw(token).unwrap_or_else(|| token.raw.clone())
                } else {
                    token.raw.clone()
                };
                let initial = self.create_initial_styled_text(token);
                if matches!(state.node, Node::Code(_)) {
                    self.apply_markdown_code_renderable(&mut state.node, raw, margin_bottom, None, initial);
                    return;
                }
                state.node = self.create_markdown_code_renderable(raw, id, margin_bottom, None, initial);
            }
        }
    }

    /// `updateTopLevelBlocks(tokens, forceTableRefresh)`.
    fn update_top_level_blocks(&mut self, tokens: &[Rc<Token>], force: bool) {
        let blocks = Self::build_top_level_render_blocks(tokens);
        let stable_tokens = self.parse_state.as_ref().map_or(0, |p| p.stable_token_count);
        self.stable_block_count = self.get_stable_block_count(&blocks, stable_tokens);

        let mut block_index = 0;
        for i in 0..blocks.len() {
            let block = &blocks[i];
            let existing_matches_identity =
                self.block_states.get(block_index).is_some_and(|e| Rc::ptr_eq(&e.token, &block.token)) && !force;
            let existing_matches_raw = self.block_states.get(block_index).is_some_and(|e| {
                e.token_raw == block.token.raw && e.token.kind == block.token.kind
            }) && !force;
            if existing_matches_identity || existing_matches_raw {
                let existing = &mut self.block_states[block_index];
                if existing.margin_top != Some(block.margin_top) {
                    existing.node.set_margin_top(block.margin_top);
                    existing.node.set_margin_bottom(0.0);
                }
                sync_top_level_block_state(existing, block, None);
                block_index += 1;
                continue;
            }

            let can_update = self.block_states.get(block_index).is_some_and(|e| {
                e.can_update_in_place
                    && e.token.kind == block.token.kind
                    && (block.token.kind != TokenType::Table
                        || (!block.token.rows.as_deref().unwrap_or(&[]).is_empty()) == matches!(e.node, Node::Table(_)))
            });
            if can_update {
                let mut existing = self.block_states[block_index].clone();
                let next_token = blocks.get(i + 1).map(|b| b.token.clone());
                self.update_block_renderable(&mut existing, &block.token, block_index, next_token.as_deref(), force);
                existing.node.set_margin_bottom(0.0);
                if existing.margin_top != Some(block.margin_top) {
                    existing.node.set_margin_top(block.margin_top);
                    existing.node.set_margin_bottom(0.0);
                }
                sync_top_level_block_state(&mut existing, block, None);
                self.block_states[block_index] = existing;
                block_index += 1;
                continue;
            }

            let (node, cache, can_update_in_place) = self.create_top_level_default_renderable(block, block_index);
            if let Some(node) = node {
                let state = BlockState {
                    token: block.token.clone(),
                    token_raw: block.token.raw.clone(),
                    margin_top: Some(block.margin_top),
                    node,
                    table_content_cache: cache,
                    tracks_inter_block_margin: None,
                    can_update_in_place,
                };
                if block_index < self.block_states.len() {
                    self.block_states[block_index] = state;
                } else {
                    self.block_states.push(state);
                }
            } else if block_index < self.block_states.len() {
                // TS destroys the existing renderable but keeps its state in the array until it is overwritten or
                // popped; the stale state then compares against the next block.
                self.block_states[block_index].node = destroyed_placeholder(&self.block_states[block_index].node);
            }
            block_index += 1;
        }
        self.block_states.truncate(block_index.min(self.block_states.len()));
    }

    /// `updateBlocks(forceTableRefresh)`.
    fn update_blocks(&mut self, force: bool) {
        if self.content.is_empty() {
            self.clear_block_states();
            self.parse_state = None;
            self.stable_block_count = 0;
            return;
        }
        let trailing = if self.streaming { 2 } else { 0 };
        self.parse_state = Some(parse_markdown_incremental(&self.content, self.parse_state.as_ref(), trailing));
        let tokens = self.parse_state.as_ref().unwrap().tokens.clone();

        if tokens.is_empty() && !self.content.is_empty() {
            self.clear_block_states();
            self.stable_block_count = 0;
            let fallback = self.create_markdown_code_renderable(self.content.clone(), format!("{}-fallback", self.id), 0.0, None, None);
            let mut token = Token::new(TokenType::Text, self.content.clone());
            token.text = Some(self.content.clone());
            self.block_states = vec![BlockState {
                token: Rc::new(token),
                token_raw: self.content.clone(),
                margin_top: Some(0.0),
                node: fallback,
                table_content_cache: None,
                tracks_inter_block_margin: Some(true),
                can_update_in_place: true,
            }];
            return;
        }

        if self.internal_block_mode == InternalBlockMode::TopLevel {
            self.update_top_level_blocks(&tokens, force);
            return;
        }

        self.stable_block_count = 0;
        let block_tokens = Self::build_renderable_tokens(&tokens);
        let mut block_index = 0;
        for i in 0..block_tokens.len() {
            let token = block_tokens[i].clone();
            let next = block_tokens.get(i + 1).cloned();
            let raw_matches = self
                .block_states
                .get(block_index)
                .is_some_and(|e| e.token_raw == token.raw && e.token.kind == token.kind);
            if raw_matches {
                let mut existing = self.block_states[block_index].clone();
                existing.token = token.clone();
                if force {
                    self.update_block_renderable(&mut existing, &token, block_index, next.as_deref(), true);
                } else {
                    Self::apply_inter_block_margin(&mut existing, &token, next.as_deref());
                }
                self.block_states[block_index] = existing;
                block_index += 1;
                continue;
            }

            let can_update = self
                .block_states
                .get(block_index)
                .is_some_and(|e| e.can_update_in_place && e.token.kind == token.kind);
            if can_update {
                let mut existing = self.block_states[block_index].clone();
                self.update_block_renderable(&mut existing, &token, block_index, next.as_deref(), false);
                existing.token = token.clone();
                existing.token_raw = token.raw.clone();
                existing.tracks_inter_block_margin = Some(true);
                self.block_states[block_index] = existing;
                block_index += 1;
                continue;
            }

            let mut table_cache = None;
            let node = if token.kind == TokenType::Table {
                let margin = Self::get_inter_block_margin(&token, next.as_deref());
                let (n, c) = self.create_table_block(&token, format!("{}-block-{block_index}", self.id), margin, None, false);
                table_cache = c;
                Some(n)
            } else {
                self.create_default_renderable(&token, block_index, next.as_deref())
            };
            if token.kind == TokenType::Table && table_cache.is_none() && matches!(node, Some(Node::Table(_))) {
                table_cache = self.build_table_content_cache(&token, None, false).0;
            }
            match node {
                Some(node) => {
                    let state = BlockState {
                        token: token.clone(),
                        token_raw: token.raw.clone(),
                        margin_top: None,
                        node,
                        table_content_cache: table_cache,
                        tracks_inter_block_margin: Some(true),
                        can_update_in_place: true,
                    };
                    if block_index < self.block_states.len() {
                        self.block_states[block_index] = state;
                    } else {
                        self.block_states.push(state);
                    }
                }
                None => {
                    if block_index < self.block_states.len() {
                        self.block_states[block_index].node = destroyed_placeholder(&self.block_states[block_index].node);
                    }
                }
            }
            block_index += 1;
        }
        self.block_states.truncate(block_index.min(self.block_states.len()));
    }

    fn clear_block_states(&mut self) {
        self.block_states.clear();
        self.stable_block_count = 0;
    }

    /// `rerenderBlocks()`.
    fn rerender_blocks(&mut self) {
        if self.internal_block_mode == InternalBlockMode::TopLevel {
            self.update_blocks(true);
            return;
        }
        for i in 0..self.block_states.len() {
            let mut state = self.block_states[i].clone();
            let token = state.token.clone();
            let next = self.block_states.get(i + 1).map(|s| s.token.clone());
            self.update_block_renderable(&mut state, &token, i, next.as_deref(), true);
            self.block_states[i] = state;
        }
    }
}

/// A block whose renderable TS destroyed while keeping the state: an empty code renderable no renderer shows.
fn destroyed_placeholder(node: &Node) -> Node {
    Node::Code(CodeNode {
        uid: 0,
        id: format!("{}-destroyed", node.id()),
        content: JsString::new(),
        filetype: None,
        fg: None,
        bg: None,
        conceal: false,
        draw_unstyled_text: false,
        streaming: false,
        initial_styled_text: None,
        base_highlight: None,
        markdown_hooks: None,
        margin_top: 0.0,
        margin_bottom: 0.0,
        applies: 0,
    })
}

fn sync_top_level_block_state(state: &mut BlockState, block: &RenderBlock, cache: Option<TableContentCache>) {
    state.token = block.token.clone();
    state.token_raw = block.token.raw.clone();
    state.margin_top = Some(block.margin_top);
    if let Some(c) = cache {
        state.table_content_cache = Some(c);
    }
}

/// `shouldRenderSeparately(token)`.
pub fn should_render_separately(token: &Token) -> bool {
    matches!(token.kind, TokenType::Code | TokenType::Table | TokenType::Blockquote | TokenType::Hr)
}

/// `isSeparatedTopLevelBlock(token)`.
fn is_separated_top_level_block(token: &Token) -> bool {
    matches!(token.kind, TokenType::Heading | TokenType::List) || should_render_separately(token)
}

/// `shouldAddTopLevelMargin(prev, current, gapBefore)`.
fn should_add_top_level_margin(prev: &Token, current: &Token, gap_before: &JsString) -> bool {
    if is_separated_top_level_block(prev) || is_separated_top_level_block(current) {
        return true;
    }
    if prev.kind != TokenType::Paragraph || current.kind != TokenType::Paragraph {
        return false;
    }
    TRAILING_MARKDOWN_BLOCK_BREAKS_RE.test(&prev.raw.concat(gap_before))
}

/// `normalizeScrollbackMarkdownBlockRaw(raw)`.
pub fn normalize_scrollback_block_raw(raw: &JsString) -> JsString {
    TRAILING_MARKDOWN_BLOCK_NEWLINES_RE.replace_str(raw, "")
}

/// `getRenderableListItemTokens(item)`.
fn renderable_list_item_tokens(item: &Token) -> Vec<&Token> {
    item.children().iter().filter(|t| !matches!(t.kind, TokenType::Checkbox | TokenType::Space)).collect()
}

/// `getListItemInputs(token, id)`.
fn list_item_inputs<'a>(token: &'a Token, id: &str) -> Vec<ListItemInput<'a>> {
    let items = token.items.as_deref().unwrap_or(&[]);
    let start = match token.start {
        Some(crate::marked::ListStart::Number(n)) => n,
        _ => 1.0,
    };
    let ordered = token.ordered == Some(true);
    let marker = |index: usize| -> JsString {
        if ordered { JsString::from(format!("{}.", js_number(start + index as f64))) } else { JsString::from("-") }
    };
    let marker_width = (0..items.len()).map(|i| marker(i).len()).fold(1, usize::max);
    items
        .iter()
        .enumerate()
        .map(|(index, item)| ListItemInput { item, marker: marker(index), marker_width, id: format!("{id}-item-{index}") })
        .collect()
}

/// A number as JS prints it, for the integers list markers use.
fn js_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e21 { format!("{}", n as i64) } else { format!("{n}") }
}
