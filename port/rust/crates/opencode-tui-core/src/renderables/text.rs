//! `<text>` and its spans: `Text.ts` (TextRenderable composing its chunks from its text nodes in a lifecycle
//! pass), `TextBufferRenderable.ts` (fg, bg, attributes, wrap mode, alignment, truncation, scrolling, the native
//! measure function of `native-renderable.zig`) and `TextNode.ts` (spans: style merging and chunk gathering).
//!
//! TODO(port-tui): opencode-tui-text. Wrapping, widths and drawing run on a small stand-in
//! ([`TextLayout`]) until the text slice's TextBuffer/TextBufferView land; then `TextKind` holds those and
//! `render` calls `buffer.draw_text_buffer(view, x, y)`.
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use crate::buffer::OptimizedBuffer;
use crate::color::RGBA;
use crate::layout::MeasureMode;
use crate::tree::{self, Element, Kind};
use crate::unicode::{self, WidthMethod};

/// `TextChunk`.
#[derive(Clone, Debug, PartialEq)]
pub struct TextChunk {
    pub text: String,
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub attributes: u32,
    pub link: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WrapMode {
    None,
    Char,
    #[default]
    Word,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// The stand-in text buffer and view: styled chunks laid out in virtual lines.
#[derive(Clone, Debug)]
pub struct TextLayout {
    pub chunks: Vec<TextChunk>,
    pub default_fg: RGBA,
    pub default_bg: RGBA,
    pub default_attributes: u32,
    pub wrap: WrapMode,
    pub align: TextAlign,
    pub truncate: bool,
    pub scroll_y: u32,
    pub scroll_x: u32,
}

#[derive(Clone, Copy, Debug)]
struct Glyph {
    chunk: usize,
    start: usize,
    len: usize,
    width: u32,
}

impl TextLayout {
    fn new() -> TextLayout {
        TextLayout {
            chunks: Vec::new(),
            default_fg: RGBA::from_values(1.0, 1.0, 1.0, 1.0),
            default_bg: RGBA::from_values(0.0, 0.0, 0.0, 0.0),
            default_attributes: 0,
            wrap: WrapMode::Word,
            align: TextAlign::Left,
            truncate: false,
            scroll_y: 0,
            scroll_x: 0,
        }
    }

    /// Logical lines as glyph lists.
    fn logical_lines(&self) -> Vec<Vec<Glyph>> {
        let mut lines = vec![Vec::new()];
        for (index, chunk) in self.chunks.iter().enumerate() {
            let mut offset = 0;
            for part in chunk.text.split('\n') {
                if offset > 0 {
                    lines.push(Vec::new());
                }
                for cluster in unicode::clusters(part, 2, WidthMethod::Unicode) {
                    lines.last_mut().unwrap().push(Glyph {
                        chunk: index,
                        start: offset + cluster.byte_start,
                        len: cluster.byte_len,
                        width: cluster.width,
                    });
                }
                offset += part.len() + 1;
            }
        }
        lines
    }

    fn is_space(&self, glyph: &Glyph) -> bool {
        let text = &self.chunks[glyph.chunk].text[glyph.start..glyph.start + glyph.len];
        text == " " || text == "\t"
    }

    /// Virtual lines for a wrap width (None: no wrapping).
    fn virtual_lines(&self, width: Option<u32>) -> Vec<Vec<Glyph>> {
        let logical = self.logical_lines();
        let Some(width) = width.filter(|w| *w > 0 && self.wrap != WrapMode::None) else { return logical };
        let mut out = Vec::new();
        for line in logical {
            if line.is_empty() {
                out.push(line);
                continue;
            }
            let mut current: Vec<Glyph> = Vec::new();
            let mut current_width = 0;
            let mut last_break: Option<usize> = None;
            for glyph in line {
                if current_width + glyph.width > width && !current.is_empty() {
                    if self.wrap == WrapMode::Word {
                        if let Some(at) = last_break.filter(|at| *at < current.len()) {
                            let rest = current.split_off(at);
                            out.push(std::mem::take(&mut current));
                            current = rest;
                        } else {
                            out.push(std::mem::take(&mut current));
                        }
                    } else {
                        out.push(std::mem::take(&mut current));
                    }
                    current_width = current.iter().map(|g| g.width).sum();
                    last_break = None;
                }
                let space = self.is_space(&glyph);
                current_width += glyph.width;
                current.push(glyph);
                if space {
                    last_break = Some(current.len());
                }
            }
            out.push(current);
        }
        out
    }

    /// `measureForDimensions(width, height)`: (widest line, line count). A width of 0 does not wrap.
    pub fn measure(&self, width: u32) -> (u32, u32) {
        let lines = self.virtual_lines(if width == 0 { None } else { Some(width) });
        let widest = lines.iter().map(|l| l.iter().map(|g| g.width).sum::<u32>()).max().unwrap_or(0);
        (widest, lines.len() as u32)
    }

    pub fn virtual_line_count(&self, width: u32) -> u32 {
        self.measure(width).1
    }

    /// Plain text of all chunks.
    pub fn plain_text(&self) -> String {
        self.chunks.iter().map(|c| c.text.as_str()).collect()
    }

    /// Draws the visible virtual lines at (x, y) in a w×h area (`drawTextBuffer`).
    pub fn draw(&self, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: u32, height: u32) {
        let lines = self.virtual_lines(Some(width));
        for (row, line) in lines.iter().skip(self.scroll_y as usize).take(height as usize).enumerate() {
            let line_width: u32 = line.iter().map(|g| g.width).sum();
            let mut glyphs: Vec<Glyph> = line.clone();
            let mut ellipsis = false;
            if self.truncate && line_width > width {
                let mut used = 0;
                let mut keep = 0;
                for g in &glyphs {
                    if used + g.width > width.saturating_sub(1) {
                        break;
                    }
                    used += g.width;
                    keep += 1;
                }
                glyphs.truncate(keep);
                ellipsis = true;
            }
            let shown: u32 = glyphs.iter().map(|g| g.width).sum::<u32>() + ellipsis as u32;
            let offset = match self.align {
                TextAlign::Left => 0,
                TextAlign::Center => (width.saturating_sub(shown)) / 2,
                TextAlign::Right => width.saturating_sub(shown),
            } as i32;
            let mut cx = x + offset - self.scroll_x as i32;
            let cy = y + row as i32;
            for glyph in &glyphs {
                let chunk = &self.chunks[glyph.chunk];
                let fg = chunk.fg.unwrap_or(self.default_fg);
                let bg = chunk.bg.unwrap_or(self.default_bg);
                let attributes = chunk.attributes | self.default_attributes;
                let text = &chunk.text[glyph.start..glyph.start + glyph.len];
                if text == "\t" {
                    cx += glyph.width as i32;
                    continue;
                }
                buffer.draw_text(text, cx, cy, fg, Some(bg), attributes, None);
                cx += glyph.width as i32;
            }
            if ellipsis {
                let last = glyphs.last().map(|g| &self.chunks[g.chunk]);
                let fg = last.and_then(|c| c.fg).unwrap_or(self.default_fg);
                let bg = last.and_then(|c| c.bg).unwrap_or(self.default_bg);
                buffer.draw_text("…", cx, cy, fg, Some(bg), self.default_attributes, None);
            }
        }
    }
}

/// `TextRenderable`.
pub struct TextKind {
    pub layout: Rc<RefCell<TextLayout>>,
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
        self.layout.borrow().draw(buffer, x, y, width, height);
    }
    fn needs_lifecycle_pass(&self) -> bool {
        self.dirty && !self.manual
    }
    fn lifecycle_pass(&mut self, el: Element) {
        self.dirty = false;
        let layout = self.layout.borrow().clone();
        let style = Style { fg: None, bg: None, attributes: 0, link: None };
        let mut chunks = Vec::new();
        for child in el.children() {
            gather(child, &style, &mut chunks);
        }
        let _ = layout;
        let changed = {
            let mut layout = self.layout.borrow_mut();
            let changed = layout.chunks != chunks;
            layout.chunks = chunks;
            changed
        };
        if changed {
            el.mark_layout_dirty();
        }
    }
    fn on_resize(&mut self, el: Element, _width: f32, _height: f32) {
        el.request_render();
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
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
    let layout = Rc::new(RefCell::new(TextLayout::new()));
    let measured = layout.clone();
    let el = tree::create(Box::new(TextKind { layout, dirty: false, manual: false }), "text", false);
    el.set_selectable(true);
    let measure_el = el;
    el.set_measure_func(Some(Box::new(move |width, width_mode, height, _height_mode| {
        let effective_width = if width_mode == MeasureMode::Undefined || width.is_nan() { 0.0 } else { width };
        let effective_height = if height.is_nan() { 1.0 } else { height };
        let measure_width = if effective_width.is_finite() && effective_width > 0.0 { effective_width.floor() as u32 } else { 0 };
        let (widest, lines) = measured.borrow().measure(measure_width);
        let mut w = widest.max(1) as f32;
        let mut h = lines.max(1) as f32;
        let absolute = measure_el.with_base(|b| b.position_absolute).unwrap_or(false);
        if width_mode == MeasureMode::AtMost && !absolute {
            w = w.min(effective_width);
            h = h.min(effective_height);
        }
        (w, h)
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
    fn text_layout<R>(self, f: impl FnOnce(&mut TextLayout) -> R) -> Option<R> {
        let layout = self.with_kind::<TextKind, _>(|k| k.layout.clone())?;
        let r = f(&mut layout.borrow_mut());
        self.mark_layout_dirty();
        Some(r)
    }

    /// `content = value` on a `<text>`.
    pub fn set_content(self, value: &str) {
        self.with_kind::<TextKind, _>(|k| k.manual = true);
        self.text_layout(|l| {
            l.chunks = vec![TextChunk { text: value.to_string(), fg: None, bg: None, attributes: 0, link: None }]
        });
    }

    /// `content = styledText` on a `<text>`.
    pub fn set_styled_content(self, chunks: Vec<TextChunk>) {
        self.with_kind::<TextKind, _>(|k| k.manual = true);
        self.text_layout(|l| l.chunks = chunks);
    }

    /// `fg`: a text's default foreground, or a span's.
    pub fn set_fg(self, fg: Option<RGBA>) {
        if self.is_text_node() {
            self.with_kind::<TextNodeKind, _>(|k| k.fg = fg);
            mark_text_dirty(self);
        } else {
            self.text_layout(|l| l.default_fg = fg.unwrap_or(RGBA::from_values(1.0, 1.0, 1.0, 1.0)));
        }
    }

    pub fn set_bg(self, bg: Option<RGBA>) {
        if self.is_text_node() {
            self.with_kind::<TextNodeKind, _>(|k| k.bg = bg);
            mark_text_dirty(self);
        } else {
            self.text_layout(|l| l.default_bg = bg.unwrap_or(RGBA::from_values(0.0, 0.0, 0.0, 0.0)));
        }
    }

    pub fn set_attributes(self, attributes: Option<u32>) {
        let attributes = attributes.unwrap_or(0);
        if self.is_text_node() {
            self.with_kind::<TextNodeKind, _>(|k| k.attributes = attributes);
            mark_text_dirty(self);
        } else {
            self.text_layout(|l| l.default_attributes = attributes);
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
        self.text_layout(|l| l.wrap = mode);
    }

    pub fn set_text_align(self, align: TextAlign) {
        self.text_layout(|l| l.align = align);
    }

    pub fn set_truncate(self, truncate: bool) {
        self.text_layout(|l| l.truncate = truncate);
    }

    /// A `<text>`'s plain text.
    pub fn plain_text(self) -> String {
        self.with_kind::<TextKind, _>(|k| k.layout.borrow().plain_text()).unwrap_or_default()
    }
}
