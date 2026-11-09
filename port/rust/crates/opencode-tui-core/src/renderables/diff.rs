//! `<diff>`: the drawing side of `Diff.ts` (DiffRenderable). It owns its panes and gutters: one
//! `<line_number>` per side around a code pane, built from `opencode_tui_markup::diff`'s line model (jsdiff's
//! `parsePatch`, `buildUnifiedView`, `buildSplitView`), the error view (a red text over the raw diff), rebuilds
//! (at once for the unified view, on the next tick for the split view), the wrap alignment of split panes from the
//! panes' line sources, hunk row offsets, and the synced scroll of split panes.
//!
//! Every setter is a `set_diff_*` method on [`Element`], with TS's semantics (which rebuild, which only forward
//! to the panes).
//!
//! TODO(port-tui): the session slice's Code renderable (`<code>`: tree-sitter highlighting, conceal,
//! `drawUnstyledText`, selection colors). Until it lands the panes are plain `<text>`s: `filetype`,
//! `syntaxStyle`, `conceal`, `drawUnstyledText` and the selection colors are kept and passed on once panes are
//! code, so only highlight colors differ (and a pane is never highlighting).
use std::any::Any;

use opencode_tui_markup::diff::{self as model, DiffColors, DiffModel, ParsedDiff, SideModel};
use opencode_tui_markup::js::JsString;
use opencode_tui_markup::style::Rgba;

use crate::buffer::OptimizedBuffer;
use crate::color::{RGBA, parse_color};
use crate::layout::Value;
use crate::renderables::line_number::{
    GutterLayout, LineColor, LineColorConfig, LineNumberOptions, LineSign, create_line_number, line_info,
};
use crate::renderables::text::WrapMode;
use crate::tree::{self, Element, FlexDirection, Kind, MouseEvent, MouseEventType};

/// Width of the " +" and " -" signs.
const SIGN_WIDTH: u32 = 2;

/// `"unified" | "split"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DiffView {
    #[default]
    Unified,
    Split,
}

/// `DiffGutterOptions`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DiffGutterOptions {
    /// Columns before the line numbers, painted in each row's gutter color. Defaults to 1.
    pub padding_left: Option<u32>,
    /// Columns after the line numbers and signs. Defaults to 1.
    pub padding_right: Option<u32>,
    /// Reserve room for at least this many line-number digits, and always reserve the sign column.
    pub min_digits: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

fn to_core(color: Rgba) -> RGBA {
    RGBA(color.0)
}

fn to_markup(color: RGBA) -> Rgba {
    Rgba(color.0)
}

/// `DiffRenderable`'s state.
pub struct DiffKind {
    diff: String,
    sync_scroll: bool,
    view: DiffView,
    parsed: ParsedDiff,
    /// Source-line anchors for hunk starts.
    hunk_start_lines: Vec<usize>,
    hunk_row_offsets: Option<Vec<usize>>,

    // CodeRenderable options
    fg: Option<RGBA>,
    filetype: Option<String>,
    wrap_mode: Option<WrapMode>,
    conceal: bool,
    selection_bg: Option<RGBA>,
    selection_fg: Option<RGBA>,

    // LineNumberRenderable options
    show_line_numbers: bool,
    line_number_fg: RGBA,
    gutter: DiffGutterOptions,

    // Diff styling (with lineNumberBg).
    colors: DiffColors,

    left_side: Option<Element>,
    right_side: Option<Element>,
    left_side_added: bool,
    right_side_added: bool,
    left_code: Option<Element>,
    right_code: Option<Element>,
    pending_rebuild: bool,
    last_code_widths: (f32, f32),
    error_text: Option<Element>,
    error_code: Option<Element>,
    waiting_for_highlight: bool,
    /// The line-info listeners on the panes: (pane, token).
    listeners: Vec<(Element, u64)>,
}

impl Default for DiffKind {
    fn default() -> Self {
        DiffKind {
            diff: String::new(),
            sync_scroll: false,
            view: DiffView::Unified,
            parsed: ParsedDiff::None,
            hunk_start_lines: Vec::new(),
            hunk_row_offsets: None,
            fg: None,
            filetype: None,
            wrap_mode: None,
            conceal: false,
            selection_bg: None,
            selection_fg: None,
            show_line_numbers: true,
            line_number_fg: parse_color("#888888"),
            gutter: DiffGutterOptions::default(),
            colors: DiffColors::default(),
            left_side: None,
            right_side: None,
            left_side_added: false,
            right_side_added: false,
            left_code: None,
            right_code: None,
            pending_rebuild: false,
            last_code_widths: (0.0, 0.0),
            error_text: None,
            error_code: None,
            waiting_for_highlight: false,
            listeners: Vec::new(),
        }
    }
}

impl Kind for DiffKind {
    fn name(&self) -> &'static str {
        "diff"
    }
    fn paints(&self) -> bool {
        false
    }
    fn render(&mut self, _el: Element, _buffer: &mut OptimizedBuffer, _x: i32, _y: i32, _width: u32, _height: u32) {}
    /// Synced scrolling of split panes.
    fn on_mouse_event(&mut self, _el: Element, event: &mut MouseEvent) {
        if event.kind != MouseEventType::Scroll || self.view != DiffView::Split || !self.sync_scroll {
            return;
        }
        let (Some(left), Some(right)) = (self.left_code, self.right_code) else { return };
        let Some(target) = event.target else { return };
        let inside = |container: Element| {
            let mut current = Some(target);
            while let Some(el) = current {
                if el == container {
                    return true;
                }
                current = el.parent();
            }
            false
        };
        if inside(left) {
            right.set_text_scroll_y(line_info::scroll_y(left));
        } else if inside(right) {
            left.set_text_scroll_y(line_info::scroll_y(right));
        }
    }
    /// `destroySelf`: every pane, attached or not.
    fn destroy(&mut self, _el: Element) {
        self.pending_rebuild = false;
        for (pane, token) in self.listeners.drain(..) {
            pane.off(token);
        }
        for child in [self.left_side, self.right_side, self.left_code, self.right_code, self.error_text, self.error_code]
            .into_iter()
            .flatten()
        {
            if !child.is_destroyed() {
                child.destroy_recursively();
            }
        }
        self.left_side = None;
        self.right_side = None;
        self.left_code = None;
        self.right_code = None;
        self.error_text = None;
        self.error_code = None;
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn is_wrapping(mode: Option<WrapMode>) -> bool {
    matches!(mode, Some(WrapMode::Word) | Some(WrapMode::Char))
}

fn side_line_colors(side: &SideModel) -> Vec<(i64, LineColor)> {
    side.line_colors
        .iter()
        .map(|(line, config)| {
            (
                *line as i64,
                LineColor::Config(LineColorConfig { gutter: Some(to_core(config.gutter)), content: Some(to_core(config.content)) }),
            )
        })
        .collect()
}

fn side_line_signs(side: &SideModel) -> Vec<(i64, LineSign)> {
    side.line_signs
        .iter()
        .map(|(line, sign)| {
            (*line as i64, LineSign { before: None, before_color: None, after: Some(sign.after.to_string()), after_color: Some(to_core(sign.after_color)) })
        })
        .collect()
}

impl DiffKind {
    /// `parseDiff()`.
    fn parse_diff(&mut self) {
        self.parsed = model::parse_diff(&JsString::from(self.diff.as_str()));
    }

    fn model(&self) -> Option<DiffModel<'_>> {
        match &self.parsed {
            ParsedDiff::Patch(patch) => Some(DiffModel { patch, colors: self.colors }),
            _ => None,
        }
    }

    /// `buildView()`.
    fn build_view(&mut self, el: Element) {
        if el.is_destroyed() {
            return;
        }
        self.hunk_start_lines.clear();
        self.hunk_row_offsets = None;
        match &self.parsed {
            ParsedDiff::Error(message) => {
                let message = message.clone();
                self.build_error_view(el, &message);
            }
            ParsedDiff::None => {}
            ParsedDiff::Patch(patch) if patch.hunks.is_empty() => {}
            ParsedDiff::Patch(_) => match self.view {
                DiffView::Unified => self.build_unified_view(el),
                DiffView::Split => self.build_split_view(el),
            },
        }
    }

    /// `requestRebuild()`: once, on the next tick.
    fn request_rebuild(&mut self, el: Element) {
        if self.pending_rebuild {
            return;
        }
        self.pending_rebuild = true;
        crate::renderer::next_tick(move || {
            if el.is_destroyed() {
                return;
            }
            el.with_kind::<DiffKind, _>(|k| {
                if k.pending_rebuild {
                    k.pending_rebuild = false;
                    k.build_view(el);
                    el.request_render();
                }
            });
        });
    }

    /// `rebuildView()`.
    fn rebuild_view(&mut self, el: Element) {
        if self.view == DiffView::Split {
            self.request_rebuild(el);
        } else {
            self.build_view(el);
        }
    }

    /// `handleLineInfoChange`: wrapping can change the visual row of a hunk anchor, and resized panes need a new
    /// alignment.
    fn handle_line_info_change(&mut self, el: Element) {
        self.hunk_row_offsets = None;
        let (Some(left), Some(right)) = (self.left_code, self.right_code) else { return };
        if self.view == DiffView::Split
            && is_wrapping(self.wrap_mode)
            && (left.width() != self.last_code_widths.0 || right.width() != self.last_code_widths.1)
        {
            self.request_rebuild(el);
        }
        if !self.waiting_for_highlight {
            return;
        }
        // TODO(port-tui): Code's `isHighlighting`; a plain pane never highlights.
        self.waiting_for_highlight = false;
        self.request_rebuild(el);
    }

    /// `attachLineInfoListeners()`: the hunk offsets depend on lineInfo even when syncScroll is off.
    fn attach_line_info_listeners(&mut self, el: Element) {
        for pane in [self.left_code, self.right_code].into_iter().flatten() {
            if let Some(index) = self.listeners.iter().position(|(p, _)| *p == pane) {
                let (_, token) = self.listeners.remove(index);
                pane.off(token);
            }
            let token = pane.on(
                "line-info-change",
                std::rc::Rc::new(move || {
                    el.with_kind::<DiffKind, _>(|k| k.handle_line_info_change(el));
                }),
            );
            self.listeners.push((pane, token));
        }
    }

    /// `buildErrorView()`.
    fn build_error_view(&mut self, el: Element, message: &str) {
        el.set_flex_direction(FlexDirection::Column);
        if let Some(side) = self.left_side.filter(|_| self.left_side_added) {
            el.remove(side);
            self.left_side_added = false;
        }
        if let Some(side) = self.right_side.filter(|_| self.right_side_added) {
            el.remove(side);
            self.right_side_added = false;
        }
        let id = el.id();
        match self.error_text {
            None => {
                let text = crate::elements::text();
                if !id.is_empty() {
                    text.set_id(&format!("{id}-error-text"));
                }
                text.set_content(message);
                text.set_fg(Some(parse_color("#ef4444")));
                text.set_width(Some(Value::Percent(100.0)));
                text.set_flex_shrink(Some(0.0));
                el.add(text, None);
                self.error_text = Some(text);
            }
            Some(text) => {
                text.set_content(message);
                if !el.children().contains(&text) {
                    el.add(text, None);
                }
            }
        }
        match self.error_code {
            None => {
                // TODO(port-tui): a `<code>` with filetype "diff" (highlighted) once Code lands.
                let code = crate::elements::text();
                if !id.is_empty() {
                    code.set_id(&format!("{id}-error-code"));
                }
                code.set_content(&self.diff);
                code.set_wrap_mode(self.wrap_mode.unwrap_or(WrapMode::Word));
                code.set_width(Some(Value::Percent(100.0)));
                code.set_flex_grow(Some(1.0));
                code.set_flex_shrink(Some(1.0));
                el.add(code, None);
                self.error_code = Some(code);
            }
            Some(code) => {
                code.set_content(&self.diff);
                code.set_wrap_mode(self.wrap_mode.unwrap_or(WrapMode::None));
                if !el.children().contains(&code) {
                    el.add(code, None);
                }
            }
        }
    }

    fn remove_error_view(&mut self, el: Element) {
        let children = el.children();
        for view in [self.error_text, self.error_code].into_iter().flatten() {
            if children.contains(&view) {
                el.remove(view);
            }
        }
    }

    /// `createOrUpdateCodeRenderable(side, content, wrapMode, drawUnstyledText)`.
    fn create_or_update_code(&mut self, el: Element, side: Side, content: &str, wrap_mode: Option<WrapMode>) -> Element {
        let existing = match side {
            Side::Left => self.left_code,
            Side::Right => self.right_code,
        };
        match existing {
            None => {
                let code = crate::elements::text();
                let id = el.id();
                if !id.is_empty() {
                    code.set_id(&format!("{id}-{}-code", if side == Side::Left { "left" } else { "right" }));
                }
                code.set_content(content);
                code.set_wrap_mode(wrap_mode.unwrap_or(WrapMode::Word));
                code.set_width(Some(Value::Percent(100.0)));
                code.set_height(Some(Value::Percent(100.0)));
                if let Some(fg) = self.fg {
                    code.set_fg(Some(fg));
                }
                match side {
                    Side::Left => self.left_code = Some(code),
                    Side::Right => self.right_code = Some(code),
                }
                code
            }
            Some(code) => {
                code.set_content(content);
                code.set_wrap_mode(wrap_mode.unwrap_or(WrapMode::None));
                if let Some(fg) = self.fg {
                    code.set_fg(Some(fg));
                }
                code
            }
        }
    }

    /// The gutter layout, set through `setGutterLayout` (a `paddingRight` option would also be layout padding).
    fn gutter_layout(&self) -> GutterLayout {
        GutterLayout {
            // The paddings and digits set the width; a minWidth floor would turn into extra left padding.
            min_width: 0,
            padding_left: self.gutter.padding_left.unwrap_or(1),
            padding_right: self.gutter.padding_right.unwrap_or(1),
            min_digits: self.gutter.min_digits.unwrap_or(0),
            // A fixed digit count only lines diffs up if sides without +/- lines keep the sign column too.
            min_after_width: if self.gutter.min_digits.is_none() { 0 } else { SIGN_WIDTH },
        }
    }

    /// `createOrUpdateSide(side, target, ...)`.
    fn create_or_update_side(&mut self, el: Element, side: Side, target: Element, model: &SideModel, width: f32) {
        let existing = match side {
            Side::Left => self.left_side,
            Side::Right => self.right_side,
        };
        let added = match side {
            Side::Left => self.left_side_added,
            Side::Right => self.right_side_added,
        };
        let colors = side_line_colors(model);
        let mut signs = side_line_signs(model);
        let numbers = model.line_numbers.iter().map(|(l, n)| (*l as i64, *n)).collect();
        let hide = model.hide_line_numbers.iter().map(|l| *l as i64).collect();
        match existing {
            None => {
                let line_number = create_line_number(LineNumberOptions {
                    target: Some(target),
                    fg: Some(self.line_number_fg),
                    bg: Some(to_core(self.colors.line_number_bg)),
                    line_colors: colors,
                    line_signs: signs,
                    line_number_offset: 0,
                    hide_line_numbers: hide,
                    line_numbers: numbers,
                    ..Default::default()
                });
                let id = el.id();
                if !id.is_empty() {
                    line_number.set_id(&format!("{id}-{}", if side == Side::Left { "left" } else { "right" }));
                }
                line_number.set_width(Some(Value::Percent(width)));
                line_number.set_show_line_numbers(self.show_line_numbers);
                line_number.set_gutter_layout(self.gutter_layout());
                el.add(line_number, None);
                match side {
                    Side::Left => {
                        self.left_side = Some(line_number);
                        self.left_side_added = true;
                    }
                    Side::Right => {
                        self.right_side = Some(line_number);
                        self.right_side_added = true;
                    }
                }
            }
            Some(line_number) => {
                line_number.set_width(Some(Value::Percent(width)));
                line_number.set_line_number_fg(Some(self.line_number_fg));
                line_number.set_line_number_bg(Some(to_core(self.colors.line_number_bg)));
                line_number.set_gutter_layout(self.gutter_layout());
                line_number.set_line_colors(&colors);
                // Preserve the viewer's width-only gutter reservation across alignment rebuilds.
                if let Some(padding) = line_number.line_signs().get(&-1).cloned() {
                    signs.retain(|(l, _)| *l != -1);
                    signs.push((-1, padding));
                }
                line_number.set_line_signs(signs);
                line_number.set_line_numbers(numbers);
                line_number.set_hide_line_numbers(hide);
                if !added {
                    el.add(line_number, None);
                    match side {
                        Side::Left => self.left_side_added = true,
                        Side::Right => self.right_side_added = true,
                    }
                }
            }
        }
    }

    /// `buildUnifiedView()`: hunks flattened into the left pane.
    fn build_unified_view(&mut self, el: Element) {
        let Some((side, hunk_starts)) = self.model().map(|m| m.unified_view()) else { return };
        el.set_flex_direction(FlexDirection::Column);
        self.remove_error_view(el);
        self.hunk_start_lines = hunk_starts;
        let content = side.content.to_string();
        let code = self.create_or_update_code(el, Side::Left, &content, self.wrap_mode);
        self.attach_line_info_listeners(el);
        self.create_or_update_side(el, Side::Left, code, &side, 100.0);
        if let Some(right) = self.right_side.filter(|_| self.right_side_added) {
            el.remove(right);
            self.right_side_added = false;
        }
    }

    /// `buildSplitView()`: removes and adds paired by change block, wrapped rows aligned once the panes have a
    /// width.
    fn build_split_view(&mut self, el: Element) {
        let Some(unaligned) = self.model().map(|m| m.split_view(None)) else { return };
        el.set_flex_direction(FlexDirection::Row);
        self.remove_error_view(el);
        let can_align = el.width() > 0.0 && is_wrapping(self.wrap_mode);
        self.last_code_widths = (
            self.left_code.map(|c| c.width()).unwrap_or(0.0),
            self.right_code.map(|c| c.width()).unwrap_or(0.0),
        );
        // TODO(port-tui): with Code, `drawUnstyledText = !(wrapping && conceal && filetype)` goes to both panes.
        let left = self.create_or_update_code(el, Side::Left, &unaligned.pre_left.to_string(), self.wrap_mode);
        let right = self.create_or_update_code(el, Side::Right, &unaligned.pre_right.to_string(), self.wrap_mode);
        self.attach_line_info_listeners(el);
        // A pane is never highlighting until Code lands, so alignment is never deferred for a highlight.
        let view = if can_align {
            let left_sources: Vec<usize> = line_info::all_line_sources(left).into_iter().map(|s| s as usize).collect();
            let right_sources: Vec<usize> = line_info::all_line_sources(right).into_iter().map(|s| s as usize).collect();
            self.model().map(|m| m.split_view(Some((&left_sources, &right_sources)))).unwrap_or(unaligned)
        } else {
            unaligned
        };
        self.hunk_start_lines = view.hunk_start_lines.clone();
        left.set_content(&view.left.content.to_string());
        right.set_content(&view.right.content.to_string());
        self.create_or_update_side(el, Side::Left, left, &view.left, 50.0);
        self.create_or_update_side(el, Side::Right, right, &view.right, 50.0);
    }

    /// `computeHunkRowOffsets()`.
    fn compute_hunk_row_offsets(&self) -> Vec<usize> {
        if self.hunk_start_lines.is_empty() {
            return Vec::new();
        }
        let sources = self.left_code.map(line_info::all_line_sources).unwrap_or_default();
        if sources.is_empty() {
            return self.hunk_start_lines.clone();
        }
        let mut offsets = Vec::new();
        let mut row = 0usize;
        for &start in &self.hunk_start_lines {
            // If the exact hunk line is concealed, navigate to the next visible row.
            while row < sources.len() && (sources[row] as usize) < start {
                row += 1;
            }
            offsets.push(if row < sources.len() { row } else { start });
        }
        offsets
    }
}

/// Creates a `<diff>` (`new DiffRenderable(ctx, { id })`: no diff yet, the unified view).
pub fn create_diff() -> Element {
    let el = tree::create(Box::new(DiffKind::default()), "diff", false);
    el.set_flex_direction(FlexDirection::Column);
    el
}

impl Element {
    fn with_diff<R>(self, f: impl FnOnce(&mut DiffKind, Element) -> R) -> Option<R> {
        self.with_kind::<DiffKind, _>(|k| f(k, self))
    }

    /// `diff = value`.
    pub fn set_diff(self, value: &str) {
        self.with_diff(|k, el| {
            if k.diff != value {
                k.diff = value.to_string();
                k.waiting_for_highlight = false;
                k.parse_diff();
                k.rebuild_view(el);
            }
        });
    }

    pub fn diff_text(self) -> String {
        self.with_diff(|k, _| k.diff.clone()).unwrap_or_default()
    }

    pub fn set_diff_sync_scroll(self, value: bool) {
        self.with_diff(|k, _| k.sync_scroll = value);
    }

    /// `view = value`.
    pub fn set_diff_view(self, value: DiffView) {
        self.with_diff(|k, el| {
            if k.view != value {
                k.view = value;
                el.set_flex_direction(if value == DiffView::Split { FlexDirection::Row } else { FlexDirection::Column });
                k.build_view(el);
            }
        });
    }

    pub fn diff_view(self) -> DiffView {
        self.with_diff(|k, _| k.view).unwrap_or_default()
    }

    /// `filetype = value`.
    pub fn set_diff_filetype(self, value: Option<String>) {
        self.with_diff(|k, el| {
            if k.filetype != value {
                k.filetype = value;
                k.rebuild_view(el);
            }
        });
    }

    /// `wrapMode = value`.
    pub fn set_diff_wrap_mode(self, value: Option<WrapMode>) {
        self.with_diff(|k, el| {
            if k.wrap_mode != value {
                k.wrap_mode = value;
                k.hunk_row_offsets = None;
                match (k.view, k.left_code) {
                    (DiffView::Unified, Some(code)) => code.set_wrap_mode(value.unwrap_or(WrapMode::None)),
                    (DiffView::Split, _) => k.request_rebuild(el),
                    _ => {}
                }
            }
        });
    }

    pub fn set_diff_conceal(self, value: bool) {
        self.with_diff(|k, el| {
            if k.conceal != value {
                k.conceal = value;
                k.rebuild_view(el);
            }
        });
    }

    /// `showLineNumbers = value`.
    pub fn set_diff_show_line_numbers(self, value: bool) {
        self.with_diff(|k, _| {
            if k.show_line_numbers != value {
                k.show_line_numbers = value;
                for side in [k.left_side, k.right_side].into_iter().flatten() {
                    side.set_show_line_numbers(value);
                }
            }
        });
    }

    /// `fg = value`: forwarded to the panes.
    pub fn set_diff_fg(self, value: Option<RGBA>) {
        self.with_diff(|k, _| {
            if k.fg != value {
                k.fg = value;
                for code in [k.left_code, k.right_code].into_iter().flatten() {
                    code.set_fg(value);
                }
            }
        });
    }

    pub fn set_diff_selection_bg(self, value: Option<RGBA>) {
        self.with_diff(|k, _| k.selection_bg = value);
    }

    pub fn set_diff_selection_fg(self, value: Option<RGBA>) {
        self.with_diff(|k, _| k.selection_fg = value);
    }

    fn set_diff_color(self, f: impl FnOnce(&mut DiffColors)) {
        self.with_diff(|k, el| {
            f(&mut k.colors);
            k.rebuild_view(el);
        });
    }

    pub fn set_diff_added_bg(self, value: Option<RGBA>) {
        let value = to_markup(value.unwrap_or_else(|| parse_color("#1a4d1a")));
        self.set_diff_color(|c| c.added_bg = value);
    }

    pub fn set_diff_removed_bg(self, value: Option<RGBA>) {
        let value = to_markup(value.unwrap_or_else(|| parse_color("#4d1a1a")));
        self.set_diff_color(|c| c.removed_bg = value);
    }

    pub fn set_diff_context_bg(self, value: Option<RGBA>) {
        let value = to_markup(value.unwrap_or_else(|| parse_color("transparent")));
        self.set_diff_color(|c| c.context_bg = value);
    }

    pub fn set_diff_added_sign_color(self, value: Option<RGBA>) {
        let value = to_markup(value.unwrap_or_else(|| parse_color("#22c55e")));
        self.set_diff_color(|c| c.added_sign_color = value);
    }

    pub fn set_diff_removed_sign_color(self, value: Option<RGBA>) {
        let value = to_markup(value.unwrap_or_else(|| parse_color("#ef4444")));
        self.set_diff_color(|c| c.removed_sign_color = value);
    }

    pub fn set_diff_added_line_number_bg(self, value: Option<RGBA>) {
        let value = to_markup(value.unwrap_or_else(|| parse_color("transparent")));
        self.set_diff_color(|c| c.added_line_number_bg = value);
    }

    pub fn set_diff_removed_line_number_bg(self, value: Option<RGBA>) {
        let value = to_markup(value.unwrap_or_else(|| parse_color("transparent")));
        self.set_diff_color(|c| c.removed_line_number_bg = value);
    }

    pub fn set_diff_line_number_fg(self, value: Option<RGBA>) {
        self.with_diff(|k, el| {
            k.line_number_fg = value.unwrap_or_else(|| parse_color("#888888"));
            k.rebuild_view(el);
        });
    }

    pub fn set_diff_line_number_bg(self, value: Option<RGBA>) {
        let value = to_markup(value.unwrap_or_else(|| parse_color("transparent")));
        self.set_diff_color(|c| c.line_number_bg = value);
    }

    /// `addedContentBg = value` (rebuilds only when it changed; TS compares the new RGBA object, so any set of
    /// a color rebuilds).
    pub fn set_diff_added_content_bg(self, value: Option<RGBA>) {
        self.with_diff(|k, el| {
            let value = value.map(to_markup);
            if k.colors.added_content_bg.is_some() || value.is_some() {
                k.colors.added_content_bg = value;
                k.rebuild_view(el);
            }
        });
    }

    pub fn set_diff_removed_content_bg(self, value: Option<RGBA>) {
        self.with_diff(|k, el| {
            let value = value.map(to_markup);
            if k.colors.removed_content_bg.is_some() || value.is_some() {
                k.colors.removed_content_bg = value;
                k.rebuild_view(el);
            }
        });
    }

    pub fn set_diff_context_content_bg(self, value: Option<RGBA>) {
        self.with_diff(|k, el| {
            let value = value.map(to_markup);
            if k.colors.context_content_bg.is_some() || value.is_some() {
                k.colors.context_content_bg = value;
                k.rebuild_view(el);
            }
        });
    }

    /// `gutter = value`.
    pub fn set_diff_gutter(self, value: DiffGutterOptions) {
        self.with_diff(|k, el| {
            if k.gutter != value {
                k.gutter = value;
                k.rebuild_view(el);
            }
        });
    }

    /// `getHunkRowOffsets()`: the visual row of each hunk start.
    pub fn diff_hunk_row_offsets(self) -> Vec<usize> {
        self.with_diff(|k, _| {
            if k.hunk_row_offsets.is_none() {
                k.hunk_row_offsets = Some(k.compute_hunk_row_offsets());
            }
            k.hunk_row_offsets.clone().unwrap_or_default()
        })
        .unwrap_or_default()
    }

    /// The side gutters (`leftSide`, `rightSide`).
    pub fn diff_sides(self) -> (Option<Element>, Option<Element>) {
        self.with_diff(|k, _| (k.left_side, k.right_side)).unwrap_or((None, None))
    }

    /// The code panes (`leftCodeRenderable`, `rightCodeRenderable`).
    pub fn diff_panes(self) -> (Option<Element>, Option<Element>) {
        self.with_diff(|k, _| (k.left_code, k.right_code)).unwrap_or((None, None))
    }

    /// `setLineColor(line, color)` on both sides.
    pub fn set_diff_line_color(self, line: i64, color: LineColor) {
        let (left, right) = self.diff_sides();
        for side in [left, right].into_iter().flatten() {
            side.set_line_color(line, color);
        }
    }

    pub fn clear_diff_line_color(self, line: i64) {
        let (left, right) = self.diff_sides();
        for side in [left, right].into_iter().flatten() {
            side.clear_line_color(line);
        }
    }

    pub fn diff_highlight_lines(self, start: i64, end: i64, color: LineColor) {
        let (left, right) = self.diff_sides();
        for side in [left, right].into_iter().flatten() {
            side.highlight_lines(start, end, color);
        }
    }

    pub fn diff_clear_highlight_lines(self, start: i64, end: i64) {
        let (left, right) = self.diff_sides();
        for side in [left, right].into_iter().flatten() {
            side.clear_highlight_lines(start, end);
        }
    }
}
