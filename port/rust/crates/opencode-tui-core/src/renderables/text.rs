//! `<text>` and its spans: `Text.ts` (TextRenderable composing its chunks from its text nodes in a lifecycle
//! pass), `TextBufferRenderable.ts` (fg, bg, attributes, wrap mode, alignment, truncation, scrolling) on the native
//! scene's Text node (`opencode_tui_text::SceneText`: buffer, view, measure function, layout and draw source), and
//! `TextNode.ts` (spans: style merging and chunk gathering).
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use opencode_tui_text::{SceneText, SceneTextOptions, StyledChunk, TextContext, WidthMethod};
pub use opencode_tui_text::{TextAlign, WrapMode};

use crate::buffer::OptimizedBuffer;
use crate::color::RGBA;
use crate::layout::{MeasureMode, Value};
use crate::tree::{self, Element, Kind};

/// `TextChunk`.
#[derive(Clone, Debug, PartialEq)]
pub struct TextChunk {
    pub text: String,
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub attributes: u32,
    pub link: Option<String>,
}

thread_local! {
    static TEXT_CONTEXT: TextContext = TextContext::new();
}

/// The renderer's text context (links shared by every text buffer).
pub fn text_context() -> TextContext {
    TEXT_CONTEXT.with(|c| c.clone())
}

fn default_options() -> SceneTextOptions {
    SceneTextOptions {
        fg: RGBA::from_values(1.0, 1.0, 1.0, 1.0).0,
        bg: RGBA::from_values(0.0, 0.0, 0.0, 0.0).0,
        attributes: 0,
        wrap_mode: WrapMode::Word,
        text_align: TextAlign::Left,
        truncate: false,
        scroll_x: 0.0,
        scroll_y: 0.0,
        first_line_offset: 0,
        tab_indicator: 0,
        tab_indicator_color: None,
    }
}

/// `TextRenderable`.
pub struct TextKind {
    pub scene: Rc<RefCell<SceneText>>,
    pub options: SceneTextOptions,
    /// The root text node's composition is stale.
    pub dirty: bool,
    /// `content` was set directly (`_hasManualStyledText`).
    pub manual: bool,
}

impl Kind for TextKind {
    fn name(&self) -> &'static str {
        "text"
    }
    fn render(&mut self, _el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: u32, height: u32) {
        let mut scene = self.scene.borrow_mut();
        if scene.needs_prepare(width, height) {
            scene.apply_layout(width, height);
        }
        scene.draw(y, |src| buffer.draw_text_buffer(src, x, y));
    }
    fn needs_lifecycle_pass(&self) -> bool {
        self.dirty && !self.manual
    }
    fn lifecycle_pass(&mut self, el: Element) {
        self.dirty = false;
        let style = Style { fg: None, bg: None, attributes: 0, link: None };
        let mut chunks = Vec::new();
        for child in el.children() {
            gather(child, &style, &mut chunks);
        }
        if set_chunks(&self.scene, &chunks) {
            el.mark_layout_dirty();
        } else {
            el.request_render();
        }
        el.emit("line-info-change");
    }
    fn on_resize(&mut self, el: Element, width: f32, height: f32) {
        self.scene.borrow_mut().apply_layout(width.max(0.0) as u32, height.max(0.0) as u32);
        el.request_render();
        el.emit("line-info-change");
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// `setText(styledText)`: whether the bytes changed (the node re-measures).
fn set_chunks(scene: &Rc<RefCell<SceneText>>, chunks: &[TextChunk]) -> bool {
    let styled: Vec<StyledChunk> = chunks
        .iter()
        .map(|c| StyledChunk {
            text: &c.text,
            fg: c.fg.map(|c| c.0),
            bg: c.bg.map(|c| c.0),
            attributes: c.attributes,
            link: c.link.as_deref(),
        })
        .collect();
    scene.borrow_mut().set_styled_text(&styled).unwrap_or(true)
}

#[derive(Clone)]
struct Style {
    fg: Option<RGBA>,
    bg: Option<RGBA>,
    attributes: u32,
    link: Option<String>,
}

/// `gatherChunks`: the node's own text, then its children, with merged styles.
fn gather(el: Element, parent: &Style, out: &mut Vec<TextChunk>) {
    let Some((style, text)) = el.with_kind::<TextNodeKind, _>(|k| {
        (
            Style {
                fg: k.fg.or(parent.fg),
                bg: k.bg.or(parent.bg),
                attributes: k.attributes | parent.attributes,
                link: k.link.clone().or_else(|| parent.link.clone()),
            },
            k.text.clone(),
        )
    }) else {
        return;
    };
    if let Some(text) = text {
        out.push(TextChunk { text, fg: style.fg, bg: style.bg, attributes: style.attributes, link: style.link.clone() });
    }
    for child in el.children() {
        gather(child, &style, out);
    }
}

/// `TextNodeRenderable` (span, b, i, u, a, br and the reconciler's text nodes).
#[derive(Default)]
pub struct TextNodeKind {
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub attributes: u32,
    pub link: Option<String>,
    /// The string child (`TextNode.fromString`, `replace(value, 0)`).
    pub text: Option<String>,
}

impl Kind for TextNodeKind {
    fn name(&self) -> &'static str {
        "text-node"
    }
    fn paints(&self) -> bool {
        false
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Creates a `<text>` (TextRenderable with the default options).
pub fn create_text() -> Element {
    let scene = Rc::new(RefCell::new(SceneText::new(WidthMethod::Unicode, &text_context())));
    let options = default_options();
    let measured = scene.clone();
    let el = tree::create(Box::new(TextKind { scene, options, dirty: false, manual: false }), "text", false);
    el.set_selectable(true);
    el.with_kind::<TextKind, _>(|k| {
        let _ = k.scene.borrow_mut().set_options(k.options, || [None, None]);
    });
    let measure_el = el;
    el.set_measure_func(Some(Box::new(move |width, width_mode, height, _height_mode| {
        let mode = match width_mode {
            MeasureMode::Undefined => 0,
            MeasureMode::Exactly => 1,
            MeasureMode::AtMost => 2,
        };
        let absolute = measure_el.with_base(|b| b.position_absolute).unwrap_or(false);
        measured.borrow_mut().yoga_measure(width, mode, height, absolute)
    })));
    el
}

/// Creates a text node (`TextNodeRenderable`).
pub fn create_text_node(kind: TextNodeKind, prefix: &str) -> Element {
    tree::create(Box::new(kind), prefix, true)
}

/// `TextNode.fromString(value)`.
pub fn text_node_from_string(value: &str) -> Element {
    create_text_node(TextNodeKind { text: Some(value.to_string()), ..TextNodeKind::default() }, "text-node")
}

/// The owning `<text>` of a text node (walking up), or the node itself if it is a text.
fn owning_text(mut el: Element) -> Option<Element> {
    loop {
        if el.is::<TextKind>() {
            return Some(el);
        }
        el = el.parent()?;
    }
}

/// A text node changed: its text recomposes at the next lifecycle pass.
pub fn mark_text_dirty(el: Element) {
    if let Some(text) = owning_text(el) {
        text.with_kind::<TextKind, _>(|k| k.dirty = true);
        text.request_render();
    }
}

/// `replaceText(textNode, value)` (`replace(value, 0)`).
pub fn replace_text(node: Element, value: &str) {
    node.with_kind::<TextNodeKind, _>(|k| k.text = Some(value.to_string()));
    mark_text_dirty(node);
}

/// The plain text a node holds (for errors).
pub fn text_of(node: Element) -> String {
    let mut out = node.with_kind::<TextNodeKind, _>(|k| k.text.clone().unwrap_or_default()).unwrap_or_default();
    for child in node.children() {
        out.push_str(&text_of(child));
    }
    out
}

/// `text.add(node, index)` / `textNode.add(node, index)`: text children are text nodes, outside Yoga.
pub fn add_text_child(parent: Element, node: Element, index: Option<usize>) {
    parent.add(node, index);
    mark_text_dirty(parent);
}

pub fn remove_text_child(parent: Element, node: Element) {
    parent.remove(node);
    mark_text_dirty(parent);
}

impl Element {
    /// `setTextOptions` with a change applied: re-measure when the measure depends on it, else repaint.
    fn text_options(self, f: impl FnOnce(&mut SceneTextOptions)) -> Option<()> {
        let (scene, options) = self.with_kind::<TextKind, _>(|k| {
            f(&mut k.options);
            (k.scene.clone(), k.options)
        })?;
        let size = self
            .with_base(|b| {
                let point = |v: Value| match v {
                    Value::Point(p) => Some(p),
                    _ => None,
                };
                [point(b.width_spec), point(b.height_spec)]
            })
            .unwrap_or([None, None]);
        let change = scene.borrow_mut().set_options(options, || size);
        match change {
            Ok(change) if change.measure_changed => self.mark_layout_dirty(),
            _ => self.request_render(),
        }
        Some(())
    }

    /// `content = value` on a `<text>`.
    pub fn set_content(self, value: &str) {
        self.set_styled_content(vec![TextChunk { text: value.to_string(), fg: None, bg: None, attributes: 0, link: None }]);
    }

    /// `content = styledText` on a `<text>`.
    pub fn set_styled_content(self, chunks: Vec<TextChunk>) {
        let Some(scene) = self.with_kind::<TextKind, _>(|k| {
            k.manual = true;
            k.scene.clone()
        }) else {
            return;
        };
        if set_chunks(&scene, &chunks) {
            self.mark_layout_dirty();
        } else {
            self.request_render();
        }
        self.emit("line-info-change");
    }

    /// `fg`: a text's default foreground, or a span's.
    pub fn set_fg(self, fg: Option<RGBA>) {
        if self.is_text_node() {
            self.with_kind::<TextNodeKind, _>(|k| k.fg = fg);
            mark_text_dirty(self);
        } else {
            self.text_options(|o| o.fg = fg.unwrap_or(RGBA::from_values(1.0, 1.0, 1.0, 1.0)).0);
        }
    }

    pub fn set_bg(self, bg: Option<RGBA>) {
        if self.is_text_node() {
            self.with_kind::<TextNodeKind, _>(|k| k.bg = bg);
            mark_text_dirty(self);
        } else {
            self.text_options(|o| o.bg = bg.unwrap_or(RGBA::from_values(0.0, 0.0, 0.0, 0.0)).0);
        }
    }

    pub fn set_attributes(self, attributes: Option<u32>) {
        let attributes = attributes.unwrap_or(0);
        if self.is_text_node() {
            self.with_kind::<TextNodeKind, _>(|k| k.attributes = attributes);
            mark_text_dirty(self);
        } else {
            self.text_options(|o| o.attributes = attributes);
        }
    }

    /// A span's `style` prop (`{ fg, bg, bold, ... }`): attributes replace the previous style's.
    pub fn set_span_style(self, fg: Option<RGBA>, bg: Option<RGBA>, attributes: u32, previous_attributes: u32) {
        self.with_kind::<TextNodeKind, _>(|k| {
            k.attributes = (k.attributes & !previous_attributes) | attributes;
            k.fg = fg;
            k.bg = bg;
        });
        mark_text_dirty(self);
    }

    pub fn set_link(self, url: Option<String>) {
        self.with_kind::<TextNodeKind, _>(|k| k.link = url);
        mark_text_dirty(self);
    }

    pub fn set_wrap_mode(self, mode: WrapMode) {
        self.text_options(|o| o.wrap_mode = mode);
    }

    pub fn set_text_align(self, align: TextAlign) {
        self.text_options(|o| o.text_align = align);
    }

    pub fn set_truncate(self, truncate: bool) {
        self.text_options(|o| o.truncate = truncate);
    }

    /// `scrollY = value` on a text (the scene clamps it to the content).
    pub fn set_text_scroll_y(self, y: f64) {
        self.text_options(|o| o.scroll_y = y.max(0.0));
    }

    /// A `<text>`'s plain text.
    pub fn plain_text(self) -> String {
        self.with_kind::<TextKind, _>(|k| k.scene.borrow().text()).unwrap_or_default()
    }

    /// `virtualLineCount`.
    pub fn virtual_line_count(self) -> u32 {
        self.with_kind::<TextKind, _>(|k| k.scene.borrow_mut().metrics().virtual_line_count).unwrap_or(0)
    }
}
