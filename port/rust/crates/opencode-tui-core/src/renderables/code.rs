//! `<code>`: `Code.ts` (CodeRenderable) on a [`TextKind`] (its `TextBufferRenderable` superclass, see
//! [`crate::tree::Kind::base_kind`]): the highlight state machine (dirty flag, snapshot ids, the rerun loop,
//! `drawUnstyledText`, streaming previews, initial styled text) with `opencode-tui-markup`'s content side
//! (`TreeSitterClient::highlight_once`, `treeSitterToTextChunks`, the conceal line source map).
//!
//! The tree-sitter worker is `spawn_blocking` work: `highlightOnce` runs off the UI task and its result is applied
//! on it, as the TS worker's reply is. `onHighlight`/`onChunks` hooks run on the UI task (TS awaits them; the
//! ported hooks are synchronous).
//!
//! Divergences: "line-info-change" raised from inside a frame hook (`onUpdate`) is emitted on the next tick, after
//! the kind is back in the tree (a listener may read the renderable).
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use opencode_tui_markup::code::get_conceal_lines_source_map;
use opencode_tui_markup::highlight::{SimpleHighlight, SourceRange};
use opencode_tui_markup::js::JsString;
use opencode_tui_markup::style::{StyledText, SyntaxStyle, TextChunk};
use opencode_tui_markup::styled_text::{TextChunkOptions, tree_sitter_to_text_chunks};
use opencode_tui_markup::tree_sitter::TreeSitterClient;
use opencode_tui_text::{SceneText, StyledChunk};

use crate::buffer::OptimizedBuffer;
use crate::layout::MeasureMode;
use crate::renderables::text::{TextKind, default_options, text_context};
use crate::tree::{self, Element, Kind};

/// `getTreeSitterClient()`: the shared client, created on first use with the default parsers registered by then.
pub fn tree_sitter_client() -> &'static TreeSitterClient {
    static CLIENT: OnceLock<TreeSitterClient> = OnceLock::new();
    CLIENT.get_or_init(TreeSitterClient::new)
}

/// `OnHighlightCallback`: may replace the highlights (`content`, `filetype` as in `HighlightContext`).
pub type OnHighlight = Rc<dyn Fn(&[SimpleHighlight], &JsString, &str, &SyntaxStyle) -> Option<Vec<SimpleHighlight>>>;

/// `ChunkRenderContext`.
pub struct ChunkContext<'a> {
    pub content: &'a JsString,
    pub filetype: &'a str,
    pub syntax_style: &'a SyntaxStyle,
    pub highlights: &'a [SimpleHighlight],
    pub source_ranges: Option<&'a [SourceRange]>,
}

/// `OnChunksCallback`: may replace the chunks.
pub type OnChunks = Rc<dyn Fn(Vec<TextChunk>, &ChunkContext<'_>) -> Option<Vec<TextChunk>>>;

/// `CodeRenderable`.
pub struct CodeKind {
    /// The `TextBufferRenderable` part.
    pub text: TextKind,
    content: JsString,
    filetype: Option<String>,
    syntax_style: Rc<SyntaxStyle>,
    is_highlighting: bool,
    highlights_dirty: bool,
    snapshot_id: u64,
    loop_active: bool,
    rerun: bool,
    conceal: bool,
    draw_unstyled_text: bool,
    should_render_text_buffer: bool,
    streaming: bool,
    initial_styled_text: Option<Rc<StyledText>>,
    had_initial_content: bool,
    base_highlight: Option<String>,
    on_highlight: Option<OnHighlight>,
    on_chunks: Option<OnChunks>,
    /// Temporary rendered-line -> source-line map for concealment.
    rendered_line_sources: Option<Vec<usize>>,
    destroyed: bool,
}

/// What a change asks of the node once the kind is back in the tree.
#[derive(Default)]
struct Effects {
    remeasure: bool,
    line_info: bool,
}

impl Effects {
    fn apply(self, el: Element) {
        if self.remeasure {
            el.mark_layout_dirty();
        } else {
            el.request_render();
        }
        if self.line_info {
            el.emit("line-info-change");
        }
    }
}

/// Chunk texts and links as UTF-8, for the text buffer.
fn chunks_of(text: &StyledText) -> Vec<(String, Option<String>, &TextChunk)> {
    text.chunks.iter().map(|c| (c.text.to_string(), c.link.as_ref().map(|l| l.to_string()), c)).collect()
}

impl CodeKind {
    fn scene(&self) -> &Rc<RefCell<SceneText>> {
        &self.text.scene
    }

    fn set_plain(&self, content: &JsString, fx: &mut Effects) {
        if self.scene().borrow_mut().set_text(&content.to_string()).unwrap_or(true) {
            fx.remeasure = true;
        }
    }

    fn set_styled(&self, text: &StyledText, fx: &mut Effects) {
        let owned = chunks_of(text);
        let chunks: Vec<StyledChunk> = owned
            .iter()
            .map(|(text, link, c)| StyledChunk {
                text,
                fg: c.fg.map(|c| c.0),
                bg: c.bg.map(|c| c.0),
                attributes: c.attributes.unwrap_or(0),
                link: link.as_deref(),
            })
            .collect();
        if self.scene().borrow_mut().set_styled_text(&chunks).unwrap_or(true) {
            fx.remeasure = true;
        }
    }

    fn invalidate_highlights(&mut self) {
        self.highlights_dirty = true;
        self.snapshot_id += 1;
    }

    fn set_rendered_line_sources(&mut self, sources: Option<Vec<usize>>) {
        self.rendered_line_sources = sources;
    }

    /// `set content`.
    fn set_content(&mut self, value: JsString, fx: &mut Effects) {
        if self.content == value {
            return;
        }
        // Streaming keeps the shown highlight until the next one lands (_hadInitialContent implies _streaming).
        if self.had_initial_content && self.should_render_text_buffer && self.filetype.is_some() && !self.draw_unstyled_text
        {
            self.content = value;
            self.invalidate_highlights();
            return;
        }
        match (&self.initial_styled_text, value.is_empty()) {
            (Some(initial), false) if self.draw_unstyled_text => {
                let initial = initial.clone();
                self.set_styled(&initial, fx)
            }
            _ => self.set_plain(&value, fx),
        }
        self.content = value;
        self.invalidate_highlights();
        self.set_rendered_line_sources(None);
        fx.line_info = true;
    }

    /// `updateStreamingPreview(content, initialStyledText)`.
    fn update_streaming_preview(&mut self, content: JsString, initial: Rc<StyledText>, fx: &mut Effects) {
        self.content = content;
        self.initial_styled_text = Some(initial.clone());
        self.invalidate_highlights();
        self.set_styled(&initial, fx);
        self.set_rendered_line_sources(None);
        fx.line_info = true;
    }

    /// `set initialStyledText`.
    fn set_initial_styled_text(&mut self, value: Option<Rc<StyledText>>, fx: &mut Effects) {
        let same = match (&self.initial_styled_text, &value) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if let Some(value) = &value {
            if self.streaming && self.draw_unstyled_text && self.is_highlighting {
                let content = self.content.clone();
                self.update_streaming_preview(content, value.clone(), fx);
                return;
            }
        }
        self.initial_styled_text = value;
        self.invalidate_highlights();
    }

    fn clear_pending_highlight(&mut self) {
        self.snapshot_id += 1;
        self.is_highlighting = false;
        self.rerun = false;
    }

    fn ensure_visible_text_before_highlight(&mut self, fx: &mut Effects) {
        let is_initial = self.streaming && !self.had_initial_content;
        let draw_now = if self.streaming { is_initial && self.draw_unstyled_text } else { self.draw_unstyled_text };
        if self.streaming && !is_initial {
            if self.draw_unstyled_text {
                self.should_render_text_buffer = true;
            }
        } else if draw_now {
            match self.initial_styled_text.clone() {
                Some(initial) => self.set_styled(&initial, fx),
                None => {
                    let content = self.content.clone();
                    self.set_plain(&content, fx)
                }
            }
            self.set_rendered_line_sources(None);
            self.should_render_text_buffer = true;
        } else {
            self.should_render_text_buffer = false;
        }
    }

    /// `scheduleHighlights()` (from `onUpdate`). Returns whether a highlight loop must start.
    fn schedule_highlights(&mut self, fx: &mut Effects) -> bool {
        if !self.highlights_dirty || self.destroyed {
            return false;
        }
        let has_content = !self.content.is_empty();
        if !has_content || self.filetype.is_none() {
            self.should_render_text_buffer = has_content;
            self.highlights_dirty = false;
            self.clear_pending_highlight();
            if has_content {
                let content = self.content.clone();
                self.set_plain(&content, fx);
                self.set_rendered_line_sources(None);
                fx.line_info = true;
            }
            return false;
        }
        self.ensure_visible_text_before_highlight(fx);
        self.highlights_dirty = false;
        if self.loop_active {
            self.is_highlighting = true;
            self.rerun = true;
            return false;
        }
        self.loop_active = true;
        true
    }

    /// The rendered-line map applied to a window of line sources (`getLineSources`).
    fn map_sources(&self, sources: Vec<u32>) -> Vec<u32> {
        match &self.rendered_line_sources {
            Some(map) => sources.into_iter().map(|line| map.get(line as usize).map_or(line, |s| *s as u32)).collect(),
            None => sources,
        }
    }
}

impl Kind for CodeKind {
    fn name(&self) -> &'static str {
        "code"
    }
    fn render(&mut self, el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: u32, height: u32) {
        if !self.should_render_text_buffer {
            return;
        }
        self.text.render(el, buffer, x, y, width, height);
    }
    fn on_update(&mut self, el: Element, _delta: f64) {
        let mut fx = Effects::default();
        let start = self.schedule_highlights(&mut fx);
        if fx.remeasure {
            el.mark_layout_dirty();
        }
        if fx.line_info {
            crate::renderer::next_tick(move || {
                if el.is_alive() {
                    el.emit("line-info-change")
                }
            });
        }
        if start {
            tokio::task::spawn_local(run_highlights(el));
        }
    }
    fn on_resize(&mut self, el: Element, width: f32, height: f32) {
        self.text.on_resize(el, width, height);
        crate::renderer::next_tick(move || {
            if el.is_alive() {
                el.emit("line-info-change")
            }
        });
    }
    fn destroy(&mut self, _el: Element) {
        self.destroyed = true;
        self.clear_pending_highlight();
    }
    fn base_kind(&mut self) -> Option<&mut dyn Kind> {
        Some(&mut self.text)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// `runHighlights()`: highlight passes until no rerun was asked for.
async fn run_highlights(el: Element) {
    loop {
        if el.with_kind::<CodeKind, _>(|k| k.rerun = false).is_none() {
            break;
        }
        start_highlight(el).await;
        let again = el
            .with_kind::<CodeKind, _>(|k| k.rerun && !k.destroyed && !k.content.is_empty() && k.filetype.is_some())
            .unwrap_or(false);
        if !again {
            break;
        }
    }
    el.with_kind::<CodeKind, _>(|k| {
        k.is_highlighting = false;
        k.loop_active = false;
        k.rerun = false;
    });
}

/// The worker's reply crossing back to the UI task.
struct Reply(opencode_tui_markup::tree_sitter::HighlightResult);

// SAFETY: `highlight_once` builds every `Rc` in its result during the call (group names are interned in a map
// local to it) and keeps none, so the whole result, with all clones of each `Rc`, moves to one other thread.
unsafe impl Send for Reply {}

/// `startHighlight()`.
async fn start_highlight(el: Element) {
    let Some(Some((content, filetype, snapshot))) = el.with_kind::<CodeKind, _>(|k| {
        k.snapshot_id += 1;
        let filetype = k.filetype.clone()?;
        if k.streaming && !k.had_initial_content {
            k.had_initial_content = true;
        }
        k.is_highlighting = true;
        Some((k.content.clone(), filetype, k.snapshot_id))
    }) else {
        return;
    };
    let client = tree_sitter_client();
    let (text, ft) = (content.clone(), filetype.clone());
    let result = tokio::task::spawn_blocking(move || Reply(client.highlight_once(&text, &ft))).await.map(|reply| reply.0);
    let current = el.with_kind::<CodeKind, _>(|k| k.snapshot_id);
    if current != Some(snapshot) {
        el.request_render();
        return;
    }
    let mut fx = Effects::default();
    let applied = el.with_kind::<CodeKind, _>(|k| {
        if k.destroyed {
            return false;
        }
        let highlights = match &result {
            Ok(result) if result.error.is_none() => Some(result.highlights.clone().unwrap_or_default()),
            _ => None,
        };
        match highlights {
            Some(mut highlights) => {
                if let Some(hook) = k.on_highlight.clone() {
                    if let Some(modified) = hook(&highlights, &content, &filetype, &k.syntax_style) {
                        highlights = modified;
                    }
                }
                if !highlights.is_empty() || k.on_chunks.is_some() || k.base_highlight.is_some() {
                    let mut ranges: Option<Vec<SourceRange>> = k.on_chunks.as_ref().map(|_| Vec::new());
                    let chunks = tree_sitter_to_text_chunks(
                        &content,
                        &highlights,
                        &k.syntax_style,
                        TextChunkOptions { enabled: Some(k.conceal), base_highlight: k.base_highlight.as_deref() },
                        ranges.as_mut(),
                    );
                    // onChunks may rewrite text arbitrarily, so the conceal-only source map would be invalid.
                    let rendered = if k.on_chunks.is_some() {
                        None
                    } else {
                        get_conceal_lines_source_map(k.conceal, &content, &highlights)
                    };
                    let chunks = match k.on_chunks.clone() {
                        Some(hook) => {
                            let context = ChunkContext {
                                content: &content,
                                filetype: &filetype,
                                syntax_style: &k.syntax_style,
                                highlights: &highlights,
                                source_ranges: ranges.as_deref(),
                            };
                            let original = chunks.clone();
                            hook(chunks, &context).unwrap_or(original)
                        }
                        None => chunks,
                    };
                    k.set_styled(&StyledText::new(chunks), &mut fx);
                    k.set_rendered_line_sources(rendered);
                } else {
                    k.set_plain(&content, &mut fx);
                    k.set_rendered_line_sources(None);
                }
            }
            None => {
                // "Code highlighting failed, falling back to plain text".
                k.set_plain(&content, &mut fx);
                k.set_rendered_line_sources(None);
            }
        }
        k.should_render_text_buffer = true;
        k.is_highlighting = false;
        k.highlights_dirty = false;
        fx.line_info = true;
        true
    });
    if applied == Some(true) {
        fx.apply(el);
        el.request_render();
    }
}

/// The props of `<code>` (`CodeOptions` beyond the text buffer's).
#[derive(Clone)]
pub struct CodeOptions {
    pub content: JsString,
    pub filetype: Option<String>,
    pub syntax_style: Rc<SyntaxStyle>,
    pub conceal: bool,
    pub draw_unstyled_text: bool,
    pub streaming: bool,
    pub initial_styled_text: Option<Rc<StyledText>>,
    pub base_highlight: Option<String>,
    pub on_highlight: Option<OnHighlight>,
    pub on_chunks: Option<OnChunks>,
}

impl Default for CodeOptions {
    fn default() -> Self {
        CodeOptions {
            content: JsString::new(),
            filetype: None,
            syntax_style: Rc::new(SyntaxStyle::new()),
            conceal: true,
            draw_unstyled_text: true,
            streaming: false,
            initial_styled_text: None,
            base_highlight: None,
            on_highlight: None,
            on_chunks: None,
        }
    }
}

/// `new CodeRenderable(ctx, options)`: the content applies at construction (the JSX element is created with its
/// static props; a Solid `<code>` gets the rest through setters).
pub fn create_code_with(options: CodeOptions) -> Element {
    let scene = Rc::new(RefCell::new(SceneText::new(crate::renderer::width_method(), &text_context())));
    let text = TextKind::new(scene.clone(), default_options(), true);
    let kind = CodeKind {
        text,
        content: options.content,
        filetype: options.filetype,
        syntax_style: options.syntax_style,
        is_highlighting: false,
        highlights_dirty: false,
        snapshot_id: 0,
        loop_active: false,
        rerun: false,
        conceal: options.conceal,
        draw_unstyled_text: options.draw_unstyled_text,
        should_render_text_buffer: true,
        streaming: options.streaming,
        initial_styled_text: options.initial_styled_text,
        had_initial_content: false,
        base_highlight: options.base_highlight,
        on_highlight: options.on_highlight,
        on_chunks: options.on_chunks,
        rendered_line_sources: None,
        destroyed: false,
    };
    let el = tree::create(Box::new(kind), "code", false);
    el.set_selectable(true);
    let measure_el = el;
    el.set_measure_func(Some(Box::new(move |width, width_mode, height, _height_mode| {
        let mode = match width_mode {
            MeasureMode::Undefined => 0,
            MeasureMode::Exactly => 1,
            MeasureMode::AtMost => 2,
        };
        let absolute = measure_el.with_base(|b| b.position_absolute).unwrap_or(false);
        scene.borrow_mut().yoga_measure(width, mode, height, absolute)
    })));
    let mut fx = Effects::default();
    el.with_kind::<CodeKind, _>(|k| {
        let _ = k.text.scene.borrow_mut().set_options(k.text.options, || [None, None]);
        if !k.content.is_empty() {
            match k.initial_styled_text.clone() {
                Some(initial) if k.draw_unstyled_text => k.set_styled(&initial, &mut fx),
                _ => {
                    let content = k.content.clone();
                    k.set_plain(&content, &mut fx)
                }
            }
            fx.line_info = true;
            k.should_render_text_buffer = k.draw_unstyled_text || k.filetype.is_none();
        }
        k.highlights_dirty = !k.content.is_empty();
    });
    fx.apply(el);
    el
}

/// `<code>` with the default options.
pub fn create_code() -> Element {
    create_code_with(CodeOptions::default())
}

impl Element {
    fn with_code(self, f: impl FnOnce(&mut CodeKind, &mut Effects)) {
        let mut fx = Effects::default();
        if self.with_kind::<CodeKind, _>(|k| f(k, &mut fx)).is_some() {
            fx.apply(self);
        }
    }

    /// `content = value` on a `<code>`.
    pub fn set_code_content(self, value: JsString) {
        self.with_code(|k, fx| k.set_content(value, fx));
    }

    pub fn code_content(self) -> JsString {
        self.with_kind::<CodeKind, _>(|k| k.content.clone()).unwrap_or_default()
    }

    pub fn set_filetype(self, value: Option<String>) {
        self.with_code(|k, _| {
            if k.filetype != value {
                k.filetype = value;
                k.invalidate_highlights();
            }
        });
    }

    /// `syntaxStyle = value` (by identity, as TS compares objects).
    pub fn set_syntax_style(self, value: Rc<SyntaxStyle>) {
        self.with_code(|k, _| {
            if !Rc::ptr_eq(&k.syntax_style, &value) {
                k.syntax_style = value;
                k.invalidate_highlights();
            }
        });
    }

    pub fn set_conceal(self, value: bool) {
        self.with_code(|k, _| {
            if k.conceal != value {
                k.conceal = value;
                k.invalidate_highlights();
            }
        });
    }

    pub fn set_draw_unstyled_text(self, value: bool) {
        self.with_code(|k, _| {
            if k.draw_unstyled_text != value {
                k.draw_unstyled_text = value;
                k.invalidate_highlights();
            }
        });
    }

    pub fn set_streaming(self, value: bool) {
        self.with_code(|k, _| {
            if k.streaming != value {
                k.streaming = value;
                k.had_initial_content = false;
                k.invalidate_highlights();
            }
        });
    }

    pub fn set_initial_styled_text(self, value: Option<Rc<StyledText>>) {
        self.with_code(|k, fx| k.set_initial_styled_text(value, fx));
    }

    /// `applyMarkdownCodeRenderable`'s first step: the streaming preview while a highlight is running, else
    /// `initialStyledText = value`.
    pub fn apply_initial_styled_text(self, content: JsString, initial: Option<Rc<StyledText>>) {
        self.with_code(|k, fx| match initial {
            Some(initial) if k.streaming && k.draw_unstyled_text && (k.is_highlighting || k.rerun) => {
                k.update_streaming_preview(content, initial, fx)
            }
            initial => k.set_initial_styled_text(initial, fx),
        });
    }

    pub fn update_streaming_preview(self, content: JsString, initial: Rc<StyledText>) {
        self.with_code(|k, fx| k.update_streaming_preview(content, initial, fx));
    }

    pub fn set_base_highlight(self, value: Option<String>) {
        self.with_code(|k, _| {
            if k.base_highlight != value {
                k.base_highlight = value;
                k.invalidate_highlights();
            }
        });
    }

    /// `onHighlight = value`; `same` tells whether TS would see the same function object.
    pub fn set_on_highlight(self, value: Option<OnHighlight>, same: bool) {
        self.with_code(|k, _| {
            if !same {
                k.on_highlight = value;
                k.invalidate_highlights();
            }
        });
    }

    /// `onChunks = value`; `same` tells whether TS would see the same function object.
    pub fn set_on_chunks(self, value: Option<OnChunks>, same: bool) {
        self.with_code(|k, _| {
            if !same {
                k.on_chunks = value;
                k.invalidate_highlights();
            }
        });
    }

    /// `isHighlighting`.
    pub fn is_highlighting(self) -> bool {
        self.with_kind::<CodeKind, _>(|k| k.is_highlighting || k.rerun).unwrap_or(false)
    }

    /// `getLineSources(startLine, lineCount)`, remapped through the conceal line map.
    pub fn code_line_sources(self, start: u32, count: u32) -> Vec<u32> {
        self.with_kind::<CodeKind, _>(|k| {
            let sources = k.text.scene.borrow_mut().line_info(start, count).line_sources;
            k.map_sources(sources)
        })
        .unwrap_or_default()
    }

    /// The text scene of a `<text>` or `<code>` (line info, metrics).
    pub fn text_scene(self) -> Option<Rc<RefCell<SceneText>>> {
        self.with_kind::<TextKind, _>(|k| k.scene.clone())
    }
}
