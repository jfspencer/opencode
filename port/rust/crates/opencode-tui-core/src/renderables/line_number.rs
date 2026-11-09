//! `<line_number>`: `LineNumberRenderable.ts` (the row that holds a gutter and its target, the content-wide line
//! backgrounds) and its `GutterRenderable` (numbers, signs and gutter backgrounds per visual row, measured from the
//! target's line counts).
//!
//! The target is a line-info provider: a `<text>` or a `<code>` (their scene text gives the visual rows' source
//! lines, the line counts and the scroll offset; see [`line_info`]). As in TS, the first such child a
//! `<line_number>` gets becomes its target, with the gutter placed before it; other children are not kept.
//!
//! The line-number state TS shares between LineNumberRenderable and its gutter by reference (the color, sign,
//! number and hidden-number maps, the layout, the colors and the offset) is one shared [`State`] here.
use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use crate::buffer::OptimizedBuffer;
use crate::color::{RGBA, parse_color};
use crate::layout::Value;
use crate::tree::{self, Element, FlexDirection, Kind};
use crate::unicode::{WidthMethod, text_width};

/// `LineSign`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LineSign {
    pub before: Option<String>,
    pub before_color: Option<RGBA>,
    pub after: Option<String>,
    pub after_color: Option<RGBA>,
}

/// `LineColorConfig`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineColorConfig {
    pub gutter: Option<RGBA>,
    pub content: Option<RGBA>,
}

/// `string | RGBA | LineColorConfig`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineColor {
    Color(RGBA),
    Config(LineColorConfig),
}

/// `GutterLayout`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GutterLayout {
    pub min_width: u32,
    /// Columns before the line numbers, painted in each line's gutter color.
    pub padding_left: u32,
    pub padding_right: u32,
    /// Reserve room for at least this many line-number digits.
    pub min_digits: u32,
    /// Reserve at least this many columns for `after` signs, even when no line has one.
    pub min_after_width: u32,
}

/// `LineNumberOptions` (the renderable options TS spreads into the box are set by the caller).
#[derive(Clone, Default)]
pub struct LineNumberOptions {
    pub target: Option<Element>,
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    /// Also the box's `minWidth`, as TS spreads the options into `Renderable`.
    pub min_width: Option<u32>,
    /// Also the box's `paddingRight`.
    pub padding_right: Option<u32>,
    pub line_colors: Vec<(i64, LineColor)>,
    pub line_signs: Vec<(i64, LineSign)>,
    pub line_number_offset: i64,
    pub hide_line_numbers: HashSet<i64>,
    pub line_numbers: HashMap<i64, f64>,
}

const DEFAULT_GUTTER_FG: &str = "#888888";

fn transparent() -> RGBA {
    RGBA::from_values(0.0, 0.0, 0.0, 0.0)
}

/// What LineNumberRenderable and its gutter share.
pub struct State {
    pub fg: RGBA,
    pub bg: RGBA,
    pub layout: GutterLayout,
    pub gutter_colors: HashMap<i64, RGBA>,
    pub content_colors: HashMap<i64, RGBA>,
    pub signs: BTreeMap<i64, LineSign>,
    pub offset: i64,
    pub hide: HashSet<i64>,
    pub numbers: HashMap<i64, f64>,
}

/// `darkenColor`: 20% darker.
fn darken(color: RGBA) -> RGBA {
    RGBA::from_values(color.r() * 0.8, color.g() * 0.8, color.b() * 0.8, color.a())
}

impl State {
    /// `parseLineColor`.
    fn parse_line_color(&mut self, line: i64, color: LineColor) {
        match color {
            LineColor::Config(config) => {
                if let Some(gutter) = config.gutter {
                    self.gutter_colors.insert(line, gutter);
                }
                if let Some(content) = config.content {
                    self.content_colors.insert(line, content);
                } else if let Some(gutter) = config.gutter {
                    self.content_colors.insert(line, darken(gutter));
                }
            }
            LineColor::Color(color) => {
                self.gutter_colors.insert(line, color);
                self.content_colors.insert(line, darken(color));
            }
        }
    }
}

/// The target side of a line-number gutter (`LineInfoProvider`).
pub mod line_info {
    use super::*;
    use crate::renderables::text::TextKind;
    use opencode_tui_text::SceneText;

    /// The target's scene text and `scrollY`.
    pub fn scene(target: Element) -> Option<(Rc<RefCell<SceneText>>, f64)> {
        target.with_kind::<TextKind, _>(|k| (k.scene.clone(), k.options.scroll_y))
    }

    /// Whether a node can be a gutter's target.
    pub fn is_provider(el: Element) -> bool {
        el.is::<TextKind>()
    }

    /// `getLineSources(target, start, count)`: the source line of each visual row.
    pub fn line_sources(target: Element, start: u32, count: u32) -> Vec<u32> {
        match scene(target) {
            Some((scene, _)) => scene.borrow_mut().line_info(start, count).line_sources,
            None => Vec::new(),
        }
    }

    /// The whole `lineInfo.lineSources`.
    pub fn all_line_sources(target: Element) -> Vec<u32> {
        match scene(target) {
            Some((scene, _)) => {
                let mut scene = scene.borrow_mut();
                let count = scene.metrics().virtual_line_count;
                scene.line_info(0, count).line_sources
            }
            None => Vec::new(),
        }
    }

    pub fn scroll_y(target: Element) -> f64 {
        scene(target).map(|(_, y)| y).unwrap_or(0.0)
    }

    /// (`lineCount`, `virtualLineCount`).
    pub fn counts(target: Element) -> (u32, u32) {
        match scene(target) {
            Some((scene, _)) => {
                let metrics = scene.borrow_mut().metrics();
                (metrics.line_count, metrics.virtual_line_count)
            }
            None => (0, 0),
        }
    }
}

/// `GutterRenderable`.
pub struct GutterKind {
    target: Element,
    state: Rc<RefCell<State>>,
    max_before_width: u32,
    max_after_width: u32,
    last_known_line_count: u32,
    sign_widths: HashMap<String, u32>,
    sign_width_method: Option<WidthMethod>,
}

impl GutterKind {
    fn calculate_sign_widths(&mut self) {
        let state = self.state.clone();
        let state = state.borrow();
        self.max_before_width = 0;
        self.max_after_width = state.layout.min_after_width;
        self.sign_widths.clear();
        self.sign_width_method = Some(WidthMethod::Unicode);
        for sign in state.signs.values() {
            if let Some(before) = &sign.before {
                self.max_before_width = self.max_before_width.max(self.sign_width(before));
            }
            if let Some(after) = &sign.after {
                self.max_after_width = self.max_after_width.max(self.sign_width(after));
            }
        }
    }

    /// Printable ASCII is one cell per byte; other signs are measured once with the glyph widths drawing uses.
    fn sign_width(&mut self, sign: &str) -> u32 {
        if sign.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
            return sign.len() as u32;
        }
        if let Some(width) = self.sign_widths.get(sign) {
            return *width;
        }
        let width = text_width(sign, 2, self.sign_width_method.unwrap_or_default());
        self.sign_widths.insert(sign.to_string(), width);
        width
    }

    /// `calculateWidth()`.
    fn calculate_width(&self) -> u32 {
        let (line_count, virtual_line_count) = line_info::counts(self.target);
        let state = self.state.borrow();
        let total_lines = line_count.max(virtual_line_count) as f64;
        let mut max_line_number = total_lines + state.offset as f64;
        for custom in state.numbers.values() {
            max_line_number = max_line_number.max(*custom);
        }
        let digits = if max_line_number > 0.0 { max_line_number.log10().floor() as u32 + 1 } else { 1 };
        let digits = digits.max(state.layout.min_digits);
        let base_width = state.layout.min_width.max(digits + state.layout.padding_left + state.layout.padding_right);
        base_width + self.max_before_width + self.max_after_width
    }
}

/// A JS number's `toString()` for line numbers.
fn number_string(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 { format!("{}", value as i64) } else { format!("{value}") }
}

impl Kind for GutterKind {
    fn name(&self) -> &'static str {
        "line-number-gutter"
    }
    fn needs_lifecycle_pass(&self) -> bool {
        true
    }
    /// Detects line count changes before layout.
    fn lifecycle_pass(&mut self, el: Element) {
        let (_, current) = line_info::counts(self.target);
        let stale = self.sign_width_method != Some(WidthMethod::Unicode);
        if stale {
            self.calculate_sign_widths();
        }
        if current != self.last_known_line_count || stale {
            self.last_known_line_count = current;
            el.mark_layout_dirty();
            el.request_render();
        }
    }
    /// Draws the visible rows every frame. An opaque line color replaces the row's background fill, and glyphs
    /// blend over the fills with a transparent background.
    fn render(&mut self, _el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: u32, height: u32) {
        let start = 0.max(-y);
        let end = (height as i32).min(buffer.height() as i32 - y);
        if end <= start || x >= buffer.width() as i32 || x + width as i32 <= 0 {
            return;
        }
        // The source of the row above the window tells whether the first row continues a wrapped line.
        let start_line = line_info::scroll_y(self.target).trunc() as i32 + start;
        let source_start = 0.max(start_line - 1);
        let source_offset = start_line - source_start;
        let sources = line_info::line_sources(self.target, source_start as u32, (end - start + source_offset) as u32);
        let mut last_source: Option<i64> = if source_offset > 0 { sources.first().map(|s| *s as i64) } else { Some(-1) };
        let state = self.state.clone();
        let state = state.borrow();
        let width = width as i32;
        for i in source_offset..end - start + source_offset {
            let row = y + start + i - source_offset;
            let logical = sources.get(i as usize).map(|s| *s as i64);
            let line_bg = logical.and_then(|l| state.gutter_colors.get(&l)).copied();
            if line_bg.is_none_or(|c| c.a() < 1.0) {
                buffer.fill_rect(x, row, width, 1, state.bg);
            }
            if let Some(bg) = line_bg {
                buffer.fill_rect(x, row, width, 1, bg);
            }
            // Only the first visual row of a logical line gets its number and signs.
            if let Some(line) = logical.filter(|l| Some(*l) != last_source) {
                let sign = state.signs.get(&line).cloned();
                if let Some(before) = sign.as_ref().and_then(|s| s.before.clone()) {
                    // Padded to the widest before sign, for alignment.
                    let before_x = x + self.max_before_width as i32 - self.sign_width(&before) as i32;
                    let color = sign.as_ref().and_then(|s| s.before_color).unwrap_or(state.fg);
                    buffer.draw_text(&before, before_x, row, color, Some(transparent()), 0, None);
                }
                // The number is right-aligned in its space, after the left padding.
                if !state.hide.contains(&line) {
                    let number = state.numbers.get(&line).copied().unwrap_or((line + 1 + state.offset) as f64);
                    let text = number_string(number);
                    let number_x =
                        width - self.max_after_width as i32 - state.layout.padding_right as i32 - text.len() as i32;
                    if number_x >= (self.max_before_width + state.layout.padding_left) as i32 {
                        buffer.draw_text(&text, x + number_x, row, state.fg, Some(transparent()), 0, None);
                    }
                }
                if let Some(after) = sign.as_ref().and_then(|s| s.after.clone()) {
                    let after_x = x + width - state.layout.padding_right as i32 - self.max_after_width as i32;
                    let color = sign.as_ref().and_then(|s| s.after_color).unwrap_or(state.fg);
                    buffer.draw_text(&after, after_x, row, color, Some(transparent()), 0, None);
                }
            }
            last_source = logical;
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// `LineNumberRenderable`.
pub struct LineNumberKind {
    state: Rc<RefCell<State>>,
    target: Option<Element>,
    gutter: Option<Element>,
    listener: Option<u64>,
}

impl Kind for LineNumberKind {
    fn name(&self) -> &'static str {
        "line-number"
    }
    /// Draws the full-width line backgrounds before the children.
    fn render(&mut self, _el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: u32, height: u32) {
        let (Some(target), Some(gutter)) = (self.target, self.gutter) else { return };
        let state = self.state.borrow();
        if state.content_colors.is_empty() {
            return;
        }
        let start = 0.max(-y);
        let end = (height as i32).min(buffer.height() as i32 - y);
        if end <= start || x >= buffer.width() as i32 || x + width as i32 <= 0 {
            return;
        }
        let first = line_info::scroll_y(target).trunc() as i32 + start;
        let sources = line_info::line_sources(target, first.max(0) as u32, (end - start) as u32);
        // From after the gutter (if visible) to the end of the width.
        let gutter_width = if gutter.visible() { gutter.width() as i32 } else { 0 };
        let content_width = width as i32 - gutter_width;
        for (i, source) in sources.iter().enumerate() {
            if let Some(bg) = state.content_colors.get(&(*source as i64)) {
                buffer.fill_rect(x + gutter_width, y + start + i as i32, content_width, 1, *bg);
            }
        }
    }
    /// TS's `add` override: the first line-info provider added becomes the target, after a new gutter.
    fn children_changed(&mut self, el: Element) {
        if self.target.is_some_and(|t| !t.is_alive()) {
            self.clear_target(el);
        }
        if self.target.is_none() {
            let candidate = el.children().into_iter().find(|c| Some(*c) != self.gutter && line_info::is_provider(*c));
            if let Some(target) = candidate {
                self.set_target(el, target);
            }
        }
        // Children that are neither the gutter nor the target are ignored (TS does not add them).
        for child in el.children() {
            if Some(child) != self.gutter && Some(child) != self.target && !child.is_text_node() {
                el.remove(child);
            }
        }
    }
    fn destroy(&mut self, _el: Element) {
        if let (Some(target), Some(token)) = (self.target, self.listener.take()) {
            target.off(token);
        }
        self.target = None;
        if let Some(gutter) = self.gutter.take() {
            if !gutter.is_destroyed() {
                gutter.destroy();
            }
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl LineNumberKind {
    /// `setTarget(target)`: listens for line info changes, creates the gutter, adds the gutter then the target.
    fn set_target(&mut self, el: Element, target: Element) {
        if target.is_destroyed() {
            return;
        }
        self.target = Some(target);
        let gutter_for_listener = Rc::new(RefCell::new(None::<Element>));
        let handle = gutter_for_listener.clone();
        // `handleLineInfoChange`: remeasure the gutter.
        self.listener = Some(target.on(
            "line-info-change",
            Rc::new(move || {
                if let Some(gutter) = *handle.borrow() {
                    remeasure(gutter);
                }
                el.request_render();
            }),
        ));
        let id = el.id();
        let gutter = create_gutter(target, self.state.clone(), if id.is_empty() { None } else { Some(format!("{id}-gutter")) });
        *gutter_for_listener.borrow_mut() = Some(gutter);
        self.gutter = Some(gutter);
        el.add(gutter, Some(0));
        if !el.children().contains(&target) {
            el.add(target, None);
        }
    }

    /// `clearTarget()`.
    fn clear_target(&mut self, el: Element) {
        if let Some(target) = self.target.take() {
            if let Some(token) = self.listener.take() {
                target.off(token);
            }
            if target.parent() == Some(el) {
                el.remove(target);
            }
        }
        if let Some(gutter) = self.gutter.take() {
            if gutter.parent() == Some(el) {
                el.remove(gutter);
            }
            gutter.destroy();
        }
    }
}

/// `gutter.remeasure()`.
fn remeasure(gutter: Element) {
    gutter.mark_layout_dirty();
    gutter.request_render();
}

fn create_gutter(target: Element, state: Rc<RefCell<State>>, id: Option<String>) -> Element {
    let (_, virtual_line_count) = line_info::counts(target);
    let mut kind = GutterKind {
        target,
        state,
        max_before_width: 0,
        max_after_width: 0,
        last_known_line_count: virtual_line_count,
        sign_widths: HashMap::new(),
        sign_width_method: None,
    };
    kind.calculate_sign_widths();
    let gutter = tree::create(Box::new(kind), "line-number-gutter", false);
    if let Some(id) = id {
        gutter.set_id(&id);
    }
    gutter.set_width(None);
    gutter.set_height(None);
    gutter.set_flex_grow(Some(0.0));
    gutter.set_flex_shrink(Some(0.0));
    gutter.set_measure_func(Some(Box::new(move |_width, _width_mode, _height, _height_mode| {
        let width = gutter.with_kind::<GutterKind, _>(|g| g.calculate_width()).unwrap_or(0);
        let (_, height) = line_info::counts(target);
        (width as f32, height as f32)
    })));
    gutter
}

/// Creates a `<line_number>` (`new LineNumberRenderable(ctx, options)`).
pub fn create_line_number(options: LineNumberOptions) -> Element {
    let mut state = State {
        fg: options.fg.unwrap_or_else(|| parse_color(DEFAULT_GUTTER_FG)),
        bg: options.bg.unwrap_or_else(transparent),
        layout: GutterLayout {
            min_width: options.min_width.unwrap_or(3),
            padding_left: 1,
            padding_right: options.padding_right.unwrap_or(1),
            min_digits: 0,
            min_after_width: 0,
        },
        gutter_colors: HashMap::new(),
        content_colors: HashMap::new(),
        signs: BTreeMap::new(),
        offset: options.line_number_offset,
        hide: options.hide_line_numbers,
        numbers: options.line_numbers,
    };
    for (line, color) in options.line_colors {
        state.parse_line_color(line, color);
    }
    for (line, sign) in options.line_signs {
        state.signs.insert(line, sign);
    }
    let kind = LineNumberKind { state: Rc::new(RefCell::new(state)), target: None, gutter: None, listener: None };
    let el = tree::create(Box::new(kind), "line-number", false);
    el.set_flex_direction(FlexDirection::Row);
    // By forcing height=auto, the parent box accounts for the full height.
    el.set_height(None);
    if let Some(min_width) = options.min_width {
        el.set_min_width(Some(Value::Point(min_width as f32)));
    }
    if let Some(padding_right) = options.padding_right {
        el.set_padding(tree::Edge::Right, Some(Value::Point(padding_right as f32)));
    }
    if let Some(target) = options.target {
        el.add(target, None);
    }
    el
}

impl Element {
    fn line_number_state(self) -> Option<(Rc<RefCell<State>>, Option<Element>)> {
        self.with_kind::<LineNumberKind, _>(|k| (k.state.clone(), k.gutter))
    }

    fn with_line_number_state(self, f: impl FnOnce(&mut State)) -> Option<Option<Element>> {
        let (state, gutter) = self.line_number_state()?;
        f(&mut state.borrow_mut());
        Some(gutter)
    }

    /// The gutter (`gutter.setLineColors` and the other setters repaint it).
    fn gutter_render(gutter: Option<Option<Element>>) {
        if let Some(Some(gutter)) = gutter {
            gutter.request_render();
        }
    }

    /// The gutter recomputes its sign widths, re-measuring if they changed (`gutter.setLineSigns`).
    fn gutter_signs_changed(gutter: Option<Option<Element>>) {
        if let Some(Some(gutter)) = gutter {
            let changed = gutter
                .with_kind::<GutterKind, _>(|g| {
                    let before = (g.max_before_width, g.max_after_width);
                    g.calculate_sign_widths();
                    before != (g.max_before_width, g.max_after_width)
                })
                .unwrap_or(false);
            if changed {
                gutter.mark_layout_dirty();
            }
            gutter.request_render();
        }
    }

    /// A line number's gutter, once it has a target.
    pub fn line_number_gutter(self) -> Option<Element> {
        self.with_kind::<LineNumberKind, _>(|k| k.gutter).flatten()
    }

    /// A line number's target.
    pub fn line_number_target(self) -> Option<Element> {
        self.with_kind::<LineNumberKind, _>(|k| k.target).flatten()
    }

    /// `showLineNumbers = value`: the gutter's visibility.
    pub fn set_show_line_numbers(self, value: bool) {
        if let Some(gutter) = self.line_number_gutter() {
            gutter.set_visible(value);
        }
    }

    pub fn show_line_numbers(self) -> bool {
        self.line_number_gutter().is_some_and(|g| g.visible())
    }

    /// `fg = value` on a `<line_number>`.
    pub fn set_line_number_fg(self, value: Option<RGBA>) {
        let gutter = self.with_line_number_state(|s| s.fg = value.unwrap_or_else(|| parse_color(DEFAULT_GUTTER_FG)));
        Self::gutter_render(gutter);
    }

    /// `bg = value` on a `<line_number>`.
    pub fn set_line_number_bg(self, value: Option<RGBA>) {
        let gutter = self.with_line_number_state(|s| s.bg = value.unwrap_or_else(transparent));
        Self::gutter_render(gutter);
    }

    pub fn line_number_fg(self) -> Option<RGBA> {
        self.line_number_state().map(|(s, _)| s.borrow().fg)
    }

    pub fn set_line_color(self, line: i64, color: LineColor) {
        let gutter = self.with_line_number_state(|s| s.parse_line_color(line, color));
        Self::gutter_render(gutter);
    }

    pub fn clear_line_color(self, line: i64) {
        let gutter = self.with_line_number_state(|s| {
            s.gutter_colors.remove(&line);
            s.content_colors.remove(&line);
        });
        Self::gutter_render(gutter);
    }

    pub fn clear_all_line_colors(self) {
        let gutter = self.with_line_number_state(|s| {
            s.gutter_colors.clear();
            s.content_colors.clear();
        });
        Self::gutter_render(gutter);
    }

    pub fn set_line_colors(self, colors: &[(i64, LineColor)]) {
        let gutter = self.with_line_number_state(|s| {
            s.gutter_colors.clear();
            s.content_colors.clear();
            for (line, color) in colors {
                s.parse_line_color(*line, *color);
            }
        });
        Self::gutter_render(gutter);
    }

    /// `getLineColors()`: (gutter, content).
    pub fn line_colors(self) -> (HashMap<i64, RGBA>, HashMap<i64, RGBA>) {
        self.line_number_state()
            .map(|(s, _)| {
                let s = s.borrow();
                (s.gutter_colors.clone(), s.content_colors.clone())
            })
            .unwrap_or_default()
    }

    pub fn set_line_sign(self, line: i64, sign: LineSign) {
        let gutter = self.with_line_number_state(|s| {
            s.signs.insert(line, sign);
        });
        Self::gutter_signs_changed(gutter);
    }

    pub fn clear_line_sign(self, line: i64) {
        let gutter = self.with_line_number_state(|s| {
            s.signs.remove(&line);
        });
        Self::gutter_signs_changed(gutter);
    }

    pub fn clear_all_line_signs(self) {
        let gutter = self.with_line_number_state(|s| s.signs.clear());
        Self::gutter_signs_changed(gutter);
    }

    pub fn set_line_signs(self, signs: Vec<(i64, LineSign)>) {
        let gutter = self.with_line_number_state(|s| {
            s.signs.clear();
            for (line, sign) in signs {
                s.signs.insert(line, sign);
            }
        });
        Self::gutter_signs_changed(gutter);
    }

    pub fn line_signs(self) -> BTreeMap<i64, LineSign> {
        self.line_number_state().map(|(s, _)| s.borrow().signs.clone()).unwrap_or_default()
    }

    pub fn gutter_layout(self) -> Option<GutterLayout> {
        self.line_number_state().map(|(s, _)| s.borrow().layout)
    }

    /// `setGutterLayout(layout)`: the gutter recomputes its sign widths and re-measures.
    pub fn set_gutter_layout(self, layout: GutterLayout) {
        let Some((state, gutter)) = self.line_number_state() else { return };
        if state.borrow().layout == layout {
            return;
        }
        state.borrow_mut().layout = layout;
        if let Some(gutter) = gutter {
            gutter.with_kind::<GutterKind, _>(|g| g.calculate_sign_widths());
            gutter.mark_layout_dirty();
            gutter.request_render();
        }
    }

    pub fn set_line_number_offset(self, offset: i64) {
        let Some((state, gutter)) = self.line_number_state() else { return };
        if state.borrow().offset == offset {
            return;
        }
        state.borrow_mut().offset = offset;
        if let Some(gutter) = gutter {
            gutter.mark_layout_dirty();
            gutter.request_render();
        }
    }

    pub fn line_number_offset(self) -> i64 {
        self.line_number_state().map(|(s, _)| s.borrow().offset).unwrap_or(0)
    }

    pub fn set_hide_line_numbers(self, hide: HashSet<i64>) {
        let gutter = self.with_line_number_state(|s| s.hide = hide);
        if let Some(Some(gutter)) = gutter {
            gutter.mark_layout_dirty();
            gutter.request_render();
        }
    }

    pub fn hide_line_numbers(self) -> HashSet<i64> {
        self.line_number_state().map(|(s, _)| s.borrow().hide.clone()).unwrap_or_default()
    }

    pub fn set_line_numbers(self, numbers: HashMap<i64, f64>) {
        let gutter = self.with_line_number_state(|s| s.numbers = numbers);
        if let Some(Some(gutter)) = gutter {
            gutter.mark_layout_dirty();
            gutter.request_render();
        }
    }

    pub fn line_numbers(self) -> HashMap<i64, f64> {
        self.line_number_state().map(|(s, _)| s.borrow().numbers.clone()).unwrap_or_default()
    }

    pub fn highlight_lines(self, start: i64, end: i64, color: LineColor) {
        let gutter = self.with_line_number_state(|s| {
            for line in start..=end {
                s.parse_line_color(line, color);
            }
        });
        Self::gutter_render(gutter);
    }

    pub fn clear_highlight_lines(self, start: i64, end: i64) {
        let gutter = self.with_line_number_state(|s| {
            for line in start..=end {
                s.gutter_colors.remove(&line);
                s.content_colors.remove(&line);
            }
        });
        Self::gutter_render(gutter);
    }
}
