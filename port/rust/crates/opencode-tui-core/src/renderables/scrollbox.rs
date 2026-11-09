//! `<scrollbox>`: `ScrollBox.ts` and `ScrollBoxParts.ts` with the native scroll node of `scene.zig`
//! (`ScrollState`, `setScroll`, `scrollTo`, `requestScroll`, `resolveScroll`, the SCROLL and LAYOUT requests,
//! `paintScrollFills`, `paintBars`, `sliderThumb`/`paintSlider`), the wheel through `lib/scroll-chain.ts` and
//! `lib/scroll-acceleration.ts`, and the scrollbar keys of `ScrollBar.ts`.
//!
//! The box's Yoga node has one hidden child, the viewport node (overflow hidden, flex 1 1), whose only child is the
//! hidden content node (align-self flex-start, min size 100%); the box's children lay out in the content node and
//! paint at the content origin minus the offset, clipped to the viewport. Each layout round resolves the scroll
//! state (range, sticky follow, requests, anchors, bar toggles and their gutters); the SCROLL request reports what
//! changed to the listening parts (bar `change`, viewport and content `resize`, `afterLayout` callbacks).
//!
//! Divergences, none visible in a frame: no viewport culling (children outside the viewport paint clipped);
//! drag auto-scroll of a selection is not ported (the renderer has no selection yet). TODO(port-tui): both.
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::buffer::OptimizedBuffer;
use crate::color::RGBA;
use crate::input::KeyEvent;
use crate::layout::{self, Value};
use crate::renderables::box_::BoxKind;
use crate::tree::{self, ChildFrame, Element, Kind, MouseEvent, MouseEventType, ScrollDirection};

/// `ScrollAcceleration`.
pub trait ScrollAcceleration {
    fn tick(&self, now: f64) -> f64;
    fn reset(&self);
}

/// `LinearScrollAccel`.
pub struct LinearScrollAccel;

impl ScrollAcceleration for LinearScrollAccel {
    fn tick(&self, _now: f64) -> f64 {
        1.0
    }
    fn reset(&self) {}
}

/// `MacOSScrollAccel`.
pub struct MacOSScrollAccel {
    pub a: f64,
    pub tau: f64,
    pub max_multiplier: f64,
    last_tick: Cell<f64>,
    history: RefCell<Vec<f64>>,
}

impl MacOSScrollAccel {
    pub fn new(a: Option<f64>, tau: Option<f64>, max_multiplier: Option<f64>) -> MacOSScrollAccel {
        MacOSScrollAccel {
            a: a.unwrap_or(0.8),
            tau: tau.unwrap_or(3.0),
            max_multiplier: max_multiplier.unwrap_or(6.0),
            last_tick: Cell::new(0.0),
            history: RefCell::new(Vec::new()),
        }
    }
}

impl ScrollAcceleration for MacOSScrollAccel {
    fn tick(&self, now: f64) -> f64 {
        let last = self.last_tick.get();
        let dt = if last != 0.0 { now - last } else { f64::INFINITY };
        if dt == f64::INFINITY || dt > 150.0 {
            self.last_tick.set(now);
            self.history.borrow_mut().clear();
            return 1.0;
        }
        if dt < 6.0 {
            return 1.0;
        }
        self.last_tick.set(now);
        let mut history = self.history.borrow_mut();
        history.push(dt);
        if history.len() > 3 {
            history.remove(0);
        }
        let average = history.iter().sum::<f64>() / history.len() as f64;
        let velocity = 100.0 / average;
        let x = velocity / self.tau;
        (1.0 + self.a * (x.exp() - 1.0)).min(self.max_multiplier)
    }
    fn reset(&self) {
        self.last_tick.set(0.0);
        self.history.borrow_mut().clear();
    }
}

/// `stickyStart`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StickyStart {
    Bottom,
    Top,
    Left,
    Right,
}

/// `ScrollUnit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollUnit {
    Absolute,
    Viewport,
    Content,
    Step,
}

/// `scrollChildIntoView`'s `align`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollAlign {
    Nearest,
    Start,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Policy {
    Auto,
    Always,
    Never,
}

/// `arrowOptions` of a bar (without `direction`).
#[derive(Clone, Debug, Default)]
pub struct ArrowOptions {
    pub attributes: Option<u32>,
    pub foreground: Option<RGBA>,
    pub background: Option<RGBA>,
    /// `arrowChars` for the bar's two ends (up/down or left/right).
    pub chars: [Option<String>; 2],
}

/// `scrollbarOptions` / `verticalScrollbarOptions` / `horizontalScrollbarOptions`: each `Some` field is assigned.
#[derive(Clone, Debug, Default)]
pub struct ScrollBarOptions {
    /// `visible`: `Some(Some(b))` holds the bar shown or hidden, `Some(None)` follows the range again.
    pub visible: Option<Option<bool>>,
    pub show_arrows: Option<bool>,
    pub arrow_options: Option<ArrowOptions>,
    /// `trackOptions.foregroundColor` (the thumb).
    pub track_foreground: Option<RGBA>,
    /// `trackOptions.backgroundColor`.
    pub track_background: Option<RGBA>,
    /// `width` of the vertical bar, `height` of the horizontal one.
    pub thickness: Option<f64>,
    /// `paddingLeft` of the vertical bar, `paddingTop` of the horizontal one.
    pub gap: Option<f64>,
    /// `position: "absolute"` overlays the content's edge instead of taking a gutter.
    pub overlay: Option<bool>,
    pub scroll_step: Option<Option<f64>>,
}

const DEFAULT_TRACK: RGBA = RGBA::rgb(37, 37, 39, 255);
const DEFAULT_THUMB: RGBA = RGBA::rgb(154, 158, 163, 255);

struct Bar {
    policy: Policy,
    overlay: bool,
    thickness: u32,
    gap: u32,
    arrows: bool,
    foreground: RGBA,
    background: RGBA,
    arrow_options: ArrowOptions,
    scroll_step: Option<f64>,
}

impl Default for Bar {
    fn default() -> Self {
        Bar {
            policy: Policy::Auto,
            overlay: false,
            thickness: 1,
            gap: 0,
            arrows: false,
            foreground: DEFAULT_THUMB,
            background: DEFAULT_TRACK,
            arrow_options: ArrowOptions::default(),
            scroll_step: None,
        }
    }
}

/// A rect local to the box, or in screen cells.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

const CHANGED_OFFSET_X: u32 = 1;
const CHANGED_OFFSET_Y: u32 = 2;
const CHANGED_CONTENT: u32 = 4;
const CHANGED_VIEWPORT: u32 = 8;
const CHANGED_LAYOUT: u32 = 16;

type Listeners<T> = Vec<(u64, Rc<dyn Fn(T)>)>;

enum BarPress {
    Thumb { axis: usize, offset: f64 },
    Arrow { timer: Rc<Cell<Option<u64>>> },
}

/// The box's scroll state (`ScrollState`) and its parts' options and listeners.
struct Inner {
    viewport_node: Box<layout::Node>,
    content_node: Box<layout::Node>,
    axes: [bool; 2],
    sticky_scroll: bool,
    sticky_start: Option<StickyStart>,
    culling: bool,
    paddings: HashMap<&'static str, u32>,
    fills: [Option<RGBA>; 3],
    bars: [Bar; 2],
    // ScrollState
    follow: Option<StickyStart>,
    padding: [f64; 4],
    stuck: bool,
    offset: [f64; 2],
    max: [f64; 2],
    extent: [f64; 2],
    viewport: Rect,
    content_origin: [f64; 2],
    visible: [bool; 2],
    toggle_frame: u64,
    toggles: u32,
    reported: [f64; 6],
    changes: u32,
    target: [Option<f64>; 2],
    reveal: Option<(Element, ScrollAlign, f64)>,
    request_frame: u64,
    anchor: Option<(Element, Option<[f64; 2]>)>,
    anchor_token: u64,
    layout_frame: u64,
    // ScrollBoxRenderable
    accel: Rc<dyn ScrollAcceleration>,
    accumulators: [f64; 2],
    layout_callbacks: Vec<(u64, Box<dyn FnOnce()>)>,
    change: [Listeners<f64>; 2],
    on_change: [Option<Rc<dyn Fn(f64)>>; 2],
    viewport_resize: Listeners<()>,
    content_resize: Listeners<()>,
    hook: bool,
    next_token: u64,
    bar_press: Option<BarPress>,
}

/// The scene's frame ids: the frame whose solves apply a request made now, and the last frame that painted.
fn next_frame() -> u64 {
    if crate::renderer::rendering() { crate::renderer::frame_id() } else { crate::renderer::frame_id() + 1 }
}

fn finished_frame() -> u64 {
    let frame = crate::renderer::frame_id();
    if crate::renderer::rendering() { frame.saturating_sub(1) } else { frame }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

impl Inner {
    fn follow_axis(&self) -> Option<usize> {
        match self.follow? {
            StickyStart::Top | StickyStart::Bottom => Some(1),
            StickyStart::Left | StickyStart::Right => Some(0),
        }
    }

    fn follow_edge(&self, axis: usize) -> f64 {
        if matches!(self.follow, Some(StickyStart::Bottom | StickyStart::Right)) { self.max[axis] } else { 0.0 }
    }

    fn restick(&mut self) {
        let Some(axis) = self.follow_axis() else { return };
        self.stuck = self.max[axis] <= 1.0 || self.offset[axis] == self.follow_edge(axis);
    }

    fn target_of(&self, axis: usize, position: f64) -> f64 {
        let end = position > 0.0 && position >= self.max[axis] && self.extent[axis] > 0.0;
        if end { f64::INFINITY } else { (position + 0.5).floor() }
    }

    fn inner_size(&self, axis: usize) -> f64 {
        let size = if axis == 1 { self.viewport.height } else { self.viewport.width };
        size - self.padding[axis] - self.padding[axis + 2]
    }

    fn reveal_offsets(&self, alignment: ScrollAlign, inset: f64, bounds: [f64; 4]) -> [f64; 2] {
        let mut positions = [0.0; 2];
        for axis in 0..2 {
            let start = bounds[axis] - inset;
            let end = bounds[axis] + bounds[axis + 2] + inset - self.inner_size(axis);
            positions[axis] = match alignment {
                ScrollAlign::Start => start,
                ScrollAlign::End => end,
                ScrollAlign::Nearest => {
                    let offset = self.offset[axis];
                    let before = start < offset;
                    if before == (end > offset) || end == start {
                        offset
                    } else if before == (end < start) {
                        start
                    } else {
                        end
                    }
                }
            };
        }
        positions
    }

    fn observed(&self) -> [f64; 6] {
        [self.offset[0], self.offset[1], self.extent[0], self.extent[1], self.viewport.width, self.viewport.height]
    }

    fn gutter(&self, axis: usize) -> f64 {
        let bar = &self.bars[axis];
        if self.visible[axis] && !bar.overlay { (bar.gap + bar.thickness) as f64 } else { 0.0 }
    }

    /// The gutters of visible inset bars as the box's right and bottom padding; whether one changed.
    fn apply_gutters(&self, el: Element, current: &mut [f64; 2]) -> bool {
        let mut changed = false;
        for (axis, edge) in [(0, layout::EDGE_BOTTOM), (1, layout::EDGE_RIGHT)] {
            let value = self.gutter(axis);
            if current[axis] == value {
                continue;
            }
            current[axis] = value;
            el.with_yoga(|y| y.set_value(layout::VALUE_PADDING, edge, Value::Point(value as f32)));
            changed = true;
        }
        changed
    }

    /// `barRects(axis, box, sides)`: (bar, track, [start arrow, end arrow]) in screen cells.
    fn bar_rects(&self, axis: usize, bx: Rect, sides: u32) -> (Rect, Rect, [Rect; 2]) {
        let options = &self.bars[axis];
        let left = (sides & 1 != 0) as u32 as f64;
        let top = (sides & 2 != 0) as u32 as f64;
        let right = (sides & 4 != 0) as u32 as f64;
        let bottom = (sides & 8 != 0) as u32 as f64;
        let thickness = options.thickness as f64;
        let mut bar = Rect {
            x: bx.x + left,
            y: bx.y + top,
            width: (bx.width - left - right).max(0.0),
            height: (bx.height - top - bottom).max(0.0),
        };
        if axis == 1 {
            bar.x += (bar.width - thickness).max(0.0);
            bar.width = bar.width.min(thickness);
        } else {
            bar.width = (bar.width - self.gutter(1)).max(0.0);
            bar.y += (bar.height - thickness).max(0.0);
            bar.height = bar.height.min(thickness);
        }
        let mut track = bar;
        let mut ends = [Rect::default(); 2];
        if options.arrows {
            let along: [f64; 2] = if axis == 1 { [0.0, 1.0] } else { [1.0, 0.0] };
            let length = if axis == 1 { bar.height } else { bar.width };
            let count = length.min(2.0);
            ends[0] = Rect {
                x: bar.x + ((bar.width - 1.0) / 2.0).floor() * along[1],
                y: bar.y + ((bar.height - 1.0) / 2.0).floor() * along[0],
                width: bar.width.min(1.0),
                height: bar.height.min(1.0),
            };
            ends[1] = ends[0];
            ends[1].x += (length - 1.0).max(0.0) * along[0];
            ends[1].y += (length - 1.0).max(0.0) * along[1];
            if axis == 1 {
                track.y += count.min(1.0);
                track.height -= count;
            } else {
                track.x += count.min(1.0);
                track.width -= count;
            }
        }
        (bar, track, ends)
    }

    /// `slider(axis)`: (max, value, viewport size).
    fn slider(&self, axis: usize) -> (f64, f64, f64) {
        (self.max[axis], self.offset[axis], self.inner_size(axis).max(1.0))
    }

    /// `moveTo`: whole cells within the last solve's limits; the anchor's row moves with the offset.
    fn move_to(&mut self, positions: [f64; 2]) {
        let before = self.offset;
        for axis in 0..2 {
            self.offset[axis] = (positions[axis] + 0.5).floor().clamp(0.0, self.max[axis]);
        }
        self.restick();
        if let Some((_, Some(row))) = self.anchor.as_mut() {
            for axis in 0..2 {
                row[axis] += before[axis] - self.offset[axis];
            }
        }
        // The host knows the offset it wrote, not a size change it has not been told of.
        self.reported[0] = self.offset[0];
        self.reported[1] = self.offset[1];
    }

    /// `aim`: scrolls now and stamps the request for the next frame's solves.
    fn aim(&mut self, positions: [f64; 2]) {
        self.request_frame = next_frame();
        self.move_to(positions);
        let Some(axis) = self.follow_axis() else { return };
        if !self.stuck || (positions[axis] + 0.5).floor() != self.offset[axis] {
            return;
        }
        self.target[axis] = None;
        self.reveal = None;
    }

    /// `scrollTo(target, relative, reapply)` of the scene.
    fn scroll_to(&mut self, target: [f64; 2], relative: [bool; 2], reapply: bool) {
        if !reapply || self.request_frame <= finished_frame() {
            self.target = [None, None];
        }
        self.reveal = None;
        let mut positions = [0.0; 2];
        for axis in 0..2 {
            let pending = self.target[axis].unwrap_or(self.offset[axis]);
            let base = if pending == f64::INFINITY { self.max[axis] } else { pending };
            positions[axis] = if relative[axis] { base + target[axis] } else { target[axis] };
            if reapply && !(relative[axis] && target[axis] == 0.0) {
                self.target[axis] = Some(self.target_of(axis, positions[axis]));
            }
            if self.target[axis] == Some(f64::INFINITY) {
                self.anchor = None;
            }
        }
        if reapply { self.aim(positions) } else { self.move_to(positions) }
    }

    fn listening(&self) -> bool {
        !self.change[0].is_empty()
            || !self.change[1].is_empty()
            || self.on_change[0].is_some()
            || self.on_change[1].is_some()
            || !self.viewport_resize.is_empty()
            || !self.content_resize.is_empty()
            || !self.layout_callbacks.is_empty()
    }

    /// `_refreshScrollHook`: a newly listening box reports from now on.
    fn refresh_hook(&mut self) {
        let listening = self.listening();
        if listening == self.hook {
            return;
        }
        self.hook = listening;
        if listening {
            self.reported = self.observed();
        }
    }

    fn edges(&self) -> [f64; 4] {
        let p = &self.paddings;
        let all = p.get("padding").copied().unwrap_or(0);
        let x = p.get("paddingX").copied().unwrap_or(all);
        let y = p.get("paddingY").copied().unwrap_or(all);
        [
            p.get("paddingLeft").copied().unwrap_or(x) as f64,
            p.get("paddingTop").copied().unwrap_or(y) as f64,
            p.get("paddingRight").copied().unwrap_or(x) as f64,
            p.get("paddingBottom").copied().unwrap_or(y) as f64,
        ]
    }
}

thread_local! {
    static BOXES: RefCell<HashMap<Element, Rc<RefCell<Inner>>>> = RefCell::new(HashMap::new());
}

fn inner_of(el: Element) -> Option<Rc<RefCell<Inner>>> {
    BOXES.with(|b| b.borrow().get(&el).cloned())
}

/// The scroll box renderable: the box it extends and its scroll state.
pub struct ScrollBoxKind {
    pub boxed: BoxKind,
    inner: Rc<RefCell<Inner>>,
    /// The gutters last written as the box's bottom and right padding.
    gutters: [f64; 2],
    resolved_frame: Cell<u64>,
}

impl ScrollBoxKind {
    fn box_rect(el: Element, x: i32, y: i32, w: u32, h: u32) -> (Rect, u32) {
        let sides = el.with_base(|b| b.border_mask).unwrap_or(0);
        (Rect { x: x as f64, y: y as f64, width: w as f64, height: h as f64 }, sides)
    }
}

/// The box's screen rect from its last layout.
fn screen_box(el: Element) -> Rect {
    let (x, y) = el.screen_position();
    let g = el.geometry();
    Rect { x: x.floor(), y: y.floor(), width: g.width as f64, height: g.height as f64 }
}

/// `target`'s rect in the content coordinates of the scroll box `box`, from the last solve; `None` when `target`
/// is not in the box's subtree. A nested scroll box adds its content origin minus its offset.
fn content_rect(bx: Element, target: Element) -> Option<[f64; 4]> {
    let mut layout = None;
    target.with_yoga(|y| layout = Some(y.computed_layout()));
    let layout = layout?;
    let mut bounds = [layout.left as f64, layout.top as f64, layout.width as f64, layout.height as f64];
    let mut cursor = target.parent();
    while let Some(parent) = cursor {
        if parent == bx {
            return Some(bounds);
        }
        let mut computed = None;
        parent.with_yoga(|y| computed = Some(y.computed_layout()));
        let computed = computed?;
        bounds[0] += computed.left as f64;
        bounds[1] += computed.top as f64;
        if let Some(nested) = inner_of(parent) {
            let nested = nested.borrow();
            for axis in 0..2 {
                bounds[axis] += nested.content_origin[axis] - nested.offset[axis];
            }
        }
        cursor = parent.parent();
    }
    None
}

impl Kind for ScrollBoxKind {
    fn name(&self) -> &'static str {
        "scrollbox"
    }
    fn paints(&self) -> bool {
        self.boxed.paints() || self.inner.borrow().fills.iter().any(Option::is_some)
    }
    fn render(&mut self, el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: u32, height: u32) {
        if self.boxed.paints() {
            self.boxed.render(el, buffer, x, y, width, height);
        }
        let inner = self.inner.borrow();
        if inner.fills.iter().all(Option::is_none) {
            return;
        }
        let (bx, sides) = Self::box_rect(el, x, y, width, height);
        let viewport = Rect { x: bx.x + inner.viewport.x, y: bx.y + inner.viewport.y, ..inner.viewport };
        let (columns, _, _) = inner.bar_rects(0, bx, sides);
        let (rows, _, _) = inner.bar_rects(1, bx, sides);
        let wrapper = Rect { x: columns.x, y: rows.y, width: columns.width, height: rows.height };
        let origin = [bx.x + inner.content_origin[0] - inner.offset[0], bx.y + inner.content_origin[1] - inner.offset[1]];
        let content = intersect(viewport, Rect { x: origin[0], y: origin[1], width: inner.extent[0], height: inner.extent[1] });
        for (fill, area) in inner.fills.iter().zip([wrapper, viewport, content]) {
            if let Some(color) = fill {
                let area = cells(area);
                buffer.fill_rect(area.x as i32, area.y as i32, area.width as i32, area.height as i32, *color);
            }
        }
    }
    fn resolves_layout(&self) -> bool {
        true
    }
    fn layout_resolved(&mut self, el: Element) {
        let inner_rc = self.inner.clone();
        let mut inner = inner_rc.borrow_mut();
        let viewport = inner.viewport_node.computed_layout();
        let content = inner.content_node.computed_layout();
        let before = (inner.offset, inner.max, inner.extent, inner.viewport, inner.content_origin, inner.visible);
        inner.viewport = Rect {
            x: viewport.left as f64,
            y: viewport.top as f64,
            width: viewport.width as f64,
            height: viewport.height as f64,
        };
        inner.content_origin = [(viewport.left + content.left) as f64, (viewport.top + content.top) as f64];
        inner.extent = [content.width as f64, content.height as f64];
        let sizes = [viewport.width as f64, viewport.height as f64];
        for axis in 0..2 {
            let range = inner.extent[axis] - (sizes[axis] - inner.padding[axis] - inner.padding[axis + 2]);
            inner.max[axis] = if inner.axes[axis] && range > 0.0 { range.ceil().min(i32::MAX as f64) } else { 0.0 };
        }
        let moved = (before.1, before.2, before.3) != (inner.max, inner.extent, inner.viewport);
        let frame_id = crate::renderer::frame_id();
        self.resolved_frame.set(frame_id);
        // A held anchor decides the offset over requests and sticky follow.
        let anchored = match inner.anchor {
            Some((node, _)) => {
                drop(inner);
                let rect = content_rect(el, node);
                inner = inner_rc.borrow_mut();
                rect
            }
            None => None,
        };
        if anchored.is_none() {
            inner.anchor = None;
        }
        let pending = inner.reveal.is_some() || inner.target[0].is_some() || inner.target[1].is_some();
        let requested =
            anchored.is_none() && pending && inner.request_frame <= frame_id && inner.request_frame > finished_frame();
        if let Some(bounds) = anchored {
            let offset = inner.offset;
            let row = inner.anchor.as_ref().and_then(|(_, row)| *row).unwrap_or([bounds[0] - offset[0], bounds[1] - offset[1]]);
            if let Some((_, slot)) = inner.anchor.as_mut() {
                *slot = Some(row);
            }
            for axis in 0..2 {
                inner.offset[axis] = bounds[axis] - row[axis];
            }
        } else if requested {
            if let Some((node, alignment, inset)) = inner.reveal {
                drop(inner);
                let rect = content_rect(el, node);
                inner = inner_rc.borrow_mut();
                if let Some(bounds) = rect {
                    inner.offset = inner.reveal_offsets(alignment, inset, bounds);
                }
            }
            for axis in 0..2 {
                if let Some(target) = inner.target[axis] {
                    inner.offset[axis] = target;
                }
            }
        }
        for axis in 0..2 {
            let max = inner.max[axis];
            inner.offset[axis] = (inner.offset[axis] + 0.5).floor().clamp(0.0, max);
        }
        if let Some(axis) = inner.follow_axis() {
            let aimed = anchored.is_some() || (requested && (inner.reveal.is_some() || inner.target[axis].is_some()));
            if aimed {
                inner.restick();
            } else if inner.stuck && moved {
                inner.offset[axis] = inner.follow_edge(axis);
            }
        }
        if inner.toggle_frame != frame_id {
            inner.toggle_frame = frame_id;
            inner.toggles = 0;
        }
        let mut toggled = false;
        for axis in 0..2 {
            let wanted = match inner.bars[axis].policy {
                Policy::Auto => inner.max[axis] > 0.0,
                Policy::Always => true,
                Policy::Never => false,
            };
            // Past two toggles a bar may still appear but not hide, so wrapping content cannot oscillate.
            if wanted == inner.visible[axis] || (inner.toggles >= 2 && !wanted) {
                continue;
            }
            inner.visible[axis] = wanted;
            inner.toggles += 1;
            toggled = true;
        }
        if toggled {
            inner.apply_gutters(el, &mut self.gutters);
        }
        if before != (inner.offset, inner.max, inner.extent, inner.viewport, inner.content_origin, inner.visible) {
            el.request_render();
        }
    }
    fn layout_notifications(&mut self, el: Element) -> Option<Box<dyn FnOnce()>> {
        let mut inner = self.inner.borrow_mut();
        if !inner.hook {
            return None;
        }
        let frame = crate::renderer::frame_id();
        let mut extra = 0;
        if inner.layout_frame != 0 && inner.layout_frame <= frame {
            extra = CHANGED_LAYOUT;
            inner.layout_frame = 0;
        }
        let current = inner.observed();
        let mut changes = extra;
        let bits = [CHANGED_OFFSET_X, CHANGED_OFFSET_Y, CHANGED_CONTENT, CHANGED_CONTENT, CHANGED_VIEWPORT, CHANGED_VIEWPORT];
        for i in 0..6 {
            if current[i] != inner.reported[i] {
                changes |= bits[i];
            }
        }
        if changes == 0 {
            return None;
        }
        inner.changes = changes;
        inner.reported = current;
        let offset = inner.offset;
        let change: [Vec<Rc<dyn Fn(f64)>>; 2] = [0, 1].map(|axis| {
            let mut out: Vec<Rc<dyn Fn(f64)>> = inner.on_change[axis].iter().cloned().collect();
            out.extend(inner.change[axis].iter().map(|(_, f)| f.clone()));
            out
        });
        let viewport: Vec<Rc<dyn Fn(())>> = inner.viewport_resize.iter().map(|(_, f)| f.clone()).collect();
        let content: Vec<Rc<dyn Fn(())>> = inner.content_resize.iter().map(|(_, f)| f.clone()).collect();
        let due: Vec<Box<dyn FnOnce()>> = if changes & CHANGED_LAYOUT != 0 {
            let callbacks = std::mem::take(&mut inner.layout_callbacks);
            let (due, waiting): (Vec<_>, Vec<_>) = callbacks.into_iter().partition(|(asked, _)| *asked < frame);
            inner.layout_callbacks = waiting;
            if !inner.layout_callbacks.is_empty() && inner.layout_frame == 0 {
                inner.layout_frame = crate::renderer::frame_id() + 1;
            }
            due.into_iter().map(|(_, f)| f).collect()
        } else {
            Vec::new()
        };
        drop(inner);
        let shared = self.inner.clone();
        Some(Box::new(move || {
            // Offsets first, then sizes, then the callbacks waiting for layout.
            for axis in [1, 0] {
                if changes & (axis as u32 + 1) == 0 {
                    continue;
                }
                for listener in &change[axis] {
                    listener(offset[axis]);
                }
                if el.is_destroyed() {
                    return;
                }
            }
            if changes & CHANGED_VIEWPORT != 0 {
                for listener in &viewport {
                    listener(());
                }
            }
            if !el.is_destroyed() && changes & CHANGED_CONTENT != 0 {
                for listener in &content {
                    listener(());
                }
            }
            for callback in due {
                if el.is_destroyed() {
                    break;
                }
                callback();
            }
            if !el.is_destroyed() {
                shared.borrow_mut().refresh_hook();
            }
        }))
    }
    fn child_frame(&self, _el: Element, _width: f32, _height: f32) -> Option<ChildFrame> {
        let inner = self.inner.borrow();
        let v = cells(inner.viewport);
        Some(ChildFrame {
            offset_x: inner.content_origin[0] - inner.offset[0],
            offset_y: inner.content_origin[1] - inner.offset[1],
            clip: (v.x as i32, v.y as i32, v.width.max(0.0) as i32, v.height.max(0.0) as i32),
        })
    }
    fn has_after_children(&self) -> bool {
        let inner = self.inner.borrow();
        inner.visible[0] || inner.visible[1]
    }
    fn render_after_children(&mut self, el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, w: u32, h: u32) {
        let inner = self.inner.borrow();
        let (bx, sides) = Self::box_rect(el, x, y, w, h);
        for axis in 0..2 {
            if !inner.visible[axis] {
                continue;
            }
            let (_, track, ends) = inner.bar_rects(axis, bx, sides);
            let bar = &inner.bars[axis];
            if track.width >= 1.0 && track.height >= 1.0 {
                let (max, value, viewport) = inner.slider(axis);
                paint_slider(buffer, axis, max, value, viewport, track, bar.foreground, bar.background);
            }
            if bar.arrows {
                for (end, arrow) in ends.iter().enumerate() {
                    if arrow.width < 1.0 || arrow.height < 1.0 {
                        continue;
                    }
                    let options = &bar.arrow_options;
                    let background = options.background;
                    let foreground = options.foreground.or(background).unwrap_or(RGBA::rgb(255, 255, 255, 255));
                    let background = background.unwrap_or(RGBA::rgb(0, 0, 0, 0));
                    let glyph = options.chars[end].clone().unwrap_or_else(|| {
                        let index = if axis == 1 { end } else { 2 + end };
                        ["▲", "▼", "◀", "▶"][index].to_string()
                    });
                    buffer.draw_text(&glyph, arrow.x as i32, arrow.y as i32, foreground, Some(background), options.attributes.unwrap_or(0), None);
                }
            }
        }
    }
    fn hits_after_children(&self, el: Element, x: i32, y: i32, w: u32, h: u32) -> Vec<(i32, i32, u32, u32)> {
        let inner = self.inner.borrow();
        let (bx, sides) = Self::box_rect(el, x, y, w, h);
        (0..2)
            .filter(|axis| inner.visible[*axis])
            .map(|axis| {
                let bar = cells(inner.bar_rects(axis, bx, sides).0);
                (bar.x as i32, bar.y as i32, bar.width.max(0.0) as u32, bar.height.max(0.0) as u32)
            })
            .collect()
    }
    fn handles_keys(&self) -> bool {
        true
    }
    fn handle_key_press(&mut self, el: Element, key: &mut KeyEvent) -> bool {
        let vertical = bar_key(el, 1, key);
        let handled = vertical || bar_key(el, 0, key);
        if handled {
            let mut inner = self.inner.borrow_mut();
            inner.accel.reset();
            inner.accumulators = [0.0, 0.0];
        }
        handled
    }
    fn on_mouse_event(&mut self, el: Element, event: &mut MouseEvent) {
        if event.kind == MouseEventType::Scroll {
            scroll_once(el, event);
        }
        handle_bar_mouse(el, event);
    }
    fn destroy(&mut self, el: Element) {
        BOXES.with(|b| b.borrow_mut().remove(&el));
        let mut inner = self.inner.borrow_mut();
        inner.layout_callbacks.clear();
        if let Some(BarPress::Arrow { timer }) = inner.bar_press.take() {
            if let Some(id) = timer.take() {
                crate::renderer::clear_timeout(id);
            }
        }
        inner.viewport_node.remove_child(&inner.content_node);
        el.with_yoga(|y| y.remove_child(&inner.viewport_node));
        el.set_children_yoga(None);
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn intersect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    Rect { x: x0, y: y0, width: (x1 - x0).max(0.0), height: (y1 - y0).max(0.0) }
}

/// `cellLayout`/`cellClip`: whole cells.
fn cells(rect: Rect) -> Rect {
    Rect { x: rect.x.floor(), y: rect.y.floor(), width: rect.width.max(0.0).floor(), height: rect.height.max(0.0).floor() }
}

/// `sliderThumb`: (size, start) in half cells.
fn slider_thumb(max: f64, value: f64, viewport_size: f64, length: f64) -> (f64, f64) {
    let track = length.min(i32::MAX as f64) * 2.0;
    let range = max;
    if range == 0.0 {
        return (track, 0.0);
    }
    let viewport_size = viewport_size.max(1.0);
    let content = range + viewport_size;
    let size = if content <= viewport_size { track } else { (track * (viewport_size / content)).floor().min(track).max(1.0) };
    let ratio = value / range;
    let offset = ratio * (track - size);
    let lower = offset.floor();
    let start = (if offset - lower < 0.5 { lower } else { lower + 1.0 }).copysign(offset);
    (size, start)
}

#[allow(clippy::too_many_arguments)]
fn paint_slider(buffer: &mut OptimizedBuffer, axis: usize, max: f64, value: f64, viewport: f64, layout: Rect, fg: RGBA, bg: RGBA) {
    let length = if axis == 0 { layout.width } else { layout.height };
    let (size, start) = slider_thumb(max, value, viewport, length);
    let x = layout.x as i32;
    let y = layout.y as i32;
    buffer.fill_rect(x, y, layout.width as i32, layout.height as i32, bg);
    let thumb_end = start + size;
    let dimensions = [layout.width, layout.height];
    let mut first = [0.0f64, 0.0];
    let mut end = [dimensions[0].ceil(), dimensions[1].ceil()];
    first[axis] = (start / 2.0).floor().max(0.0);
    end[axis] = dimensions[axis].floor().min((thumb_end / 2.0).ceil());
    if first[axis] >= end[axis] || first[1 - axis] >= end[1 - axis] {
        return;
    }
    let mut along = first[axis] as u32;
    while (along as f64) < end[axis] {
        let cell_start = along as f64 * 2.0;
        let from = start.max(cell_start);
        let coverage = thumb_end.min(cell_start + 2.0) - from;
        let ch = if coverage >= 2.0 {
            '█'
        } else if axis == 0 {
            if from == cell_start { '▌' } else { '▐' }
        } else if coverage > 0.0 {
            if from == cell_start { '▀' } else { '▄' }
        } else {
            ' '
        };
        let mut across = first[1 - axis] as u32;
        while (across as f64) < end[1 - axis] {
            let (column, row) = if axis == 0 { (along, across) } else { (across, along) };
            let cx = (layout.x + column as f64) as i32;
            let cy = (layout.y + row as f64) as i32;
            if cx >= 0 && cy >= 0 {
                buffer.set_cell_with_alpha_blending(cx, cy, ch, fg, bg, 0);
            }
            across += 1;
        }
        along += 1;
    }
}

/// `scrollOnce(event, box, scrollByWheel)`.
fn scroll_once(el: Element, event: &mut MouseEvent) {
    let mut node = event.scroll_taker;
    while let Some(current) = node {
        if current == el {
            return;
        }
        node = current.parent();
    }
    if scroll_by_wheel(el, event) {
        event.scroll_taker = Some(el);
    }
}

/// `scrollByWheel`: a wheel step when the box can still move that way.
fn scroll_by_wheel(el: Element, event: &MouseEvent) -> bool {
    let Some(inner) = inner_of(el) else { return false };
    let Some(scroll) = event.scroll else { return false };
    let mut direction = scroll.direction;
    if event.shift {
        direction = match direction {
            ScrollDirection::Up => ScrollDirection::Left,
            ScrollDirection::Down => ScrollDirection::Right,
            ScrollDirection::Right => ScrollDirection::Down,
            ScrollDirection::Left => ScrollDirection::Up,
        };
    }
    let vertical = matches!(direction, ScrollDirection::Up | ScrollDirection::Down);
    let axis = if vertical { 1 } else { 0 };
    let sign = if matches!(direction, ScrollDirection::Up | ScrollDirection::Left) { -1.0 } else { 1.0 };
    let (position, max, accel, mut remainder) = {
        let inner = inner.borrow();
        (inner.offset[axis], inner.max[axis], inner.accel.clone(), inner.accumulators[axis])
    };
    if if sign < 0.0 { position <= 0.0 } else { position >= max } {
        return false;
    }
    if remainder * sign < 0.0 {
        remainder = 0.0;
    }
    remainder += sign * scroll.delta * accel.tick(now_ms());
    let step = remainder.trunc();
    if step != 0.0 {
        el.scroll_axis(axis, position + step, false, false);
    }
    let Some(inner) = inner_of(el) else { return true };
    let mut inner = inner.borrow_mut();
    remainder = if inner.offset[axis] - position == step { remainder - step } else { 0.0 };
    inner.accumulators[axis] = remainder;
    true
}

/// `ScrollBar.handleKeyPress` of one bar.
fn bar_key(el: Element, axis: usize, key: &KeyEvent) -> bool {
    let vertical = axis == 1;
    let (delta, unit) = match key.name.as_str() {
        "left" | "h" if !vertical => (-1.0 / 5.0, ScrollUnit::Viewport),
        "right" | "l" if !vertical => (1.0 / 5.0, ScrollUnit::Viewport),
        "up" | "k" if vertical => (-1.0 / 5.0, ScrollUnit::Viewport),
        "down" | "j" if vertical => (1.0 / 5.0, ScrollUnit::Viewport),
        "pageup" => (-1.0 / 2.0, ScrollUnit::Viewport),
        "pagedown" => (1.0 / 2.0, ScrollUnit::Viewport),
        "home" => (-1.0, ScrollUnit::Content),
        "end" => (1.0, ScrollUnit::Content),
        _ => return false,
    };
    el.bar_scroll_by(axis, delta, unit);
    true
}

/// `handleBarMouse`: a press on an arrow scrolls and repeats, one on the track jumps there, and a drag that started
/// on the track or thumb moves the thumb with the pointer.
fn handle_bar_mouse(el: Element, event: &mut MouseEvent) -> bool {
    let Some(inner_rc) = inner_of(el) else { return false };
    let pressing = inner_rc.borrow().bar_press.is_some();
    if pressing && matches!(event.kind, MouseEventType::Drag | MouseEventType::Up | MouseEventType::DragEnd) {
        let thumb = match &inner_rc.borrow().bar_press {
            Some(BarPress::Thumb { axis, offset }) => Some((*axis, *offset)),
            _ => None,
        };
        if let Some((axis, offset)) = thumb.filter(|_| event.kind != MouseEventType::DragEnd) {
            drag_thumb(el, axis, offset, event);
        }
        if event.kind == MouseEventType::Up {
            end_bar_press(el);
        }
        event.stop_propagation();
        return true;
    }
    if event.kind != MouseEventType::Down || event.button != 0 || event.target != Some(el) {
        return false;
    }
    let bx = screen_box(el);
    let sides = el.with_base(|b| b.border_mask).unwrap_or(0);
    for axis in [1, 0] {
        let (visible, rects, arrows) = {
            let inner = inner_rc.borrow();
            (inner.visible[axis], inner.bar_rects(axis, bx, sides), inner.bars[axis].arrows)
        };
        if !visible {
            continue;
        }
        let (_, track, ends) = rects;
        let (px, py) = (event.x as f64, event.y as f64);
        let arrow = if arrows {
            if ends[0].contains(px, py) {
                -1.0
            } else if ends[1].contains(px, py) {
                1.0
            } else {
                0.0
            }
        } else {
            0.0
        };
        if arrow == 0.0 && !track.contains(px, py) {
            continue;
        }
        event.stop_propagation();
        event.prevent_default();
        end_bar_press(el);
        if arrow != 0.0 {
            press_arrow(el, axis, arrow);
        } else {
            press_track(el, axis, event);
        }
        return true;
    }
    false
}

fn press_arrow(el: Element, axis: usize, arrow: f64) {
    let Some(inner) = inner_of(el) else { return };
    let timer = Rc::new(Cell::new(None));
    inner.borrow_mut().bar_press = Some(BarPress::Arrow { timer: timer.clone() });
    // A half viewport now and after 500 ms, then a fifth every 200 ms.
    fn step(el: Element, axis: usize, arrow: f64, amount: f64, delay: u64, timer: Rc<Cell<Option<u64>>>) {
        el.bar_scroll_by(axis, arrow * amount, ScrollUnit::Viewport);
        let Some(inner) = inner_of(el) else { return };
        let still = matches!(&inner.borrow().bar_press, Some(BarPress::Arrow { timer: current }) if Rc::ptr_eq(current, &timer));
        if !still {
            return;
        }
        let next = timer.clone();
        let id = crate::renderer::set_timeout(delay, move || {
            step(el, axis, arrow, if delay == 500 { 0.5 } else { 0.2 }, 200, next);
        });
        timer.set(Some(id));
    }
    step(el, axis, arrow, 0.5, 500, timer);
}

/// `thumbPointer`: (at, length, start, size) in half cells along the track.
fn thumb_pointer(el: Element, axis: usize, event: &MouseEvent) -> (f64, f64, f64, f64) {
    let Some(inner) = inner_of(el) else { return (0.0, 0.0, 0.0, 0.0) };
    let inner = inner.borrow();
    let bx = screen_box(el);
    let sides = el.with_base(|b| b.border_mask).unwrap_or(0);
    let (_, track, _) = inner.bar_rects(axis, bx, sides);
    let along = if axis == 1 { event.y as f64 - track.y } else { event.x as f64 - track.x };
    let length = if axis == 1 { track.height } else { track.width };
    let (max, value, viewport) = inner.slider(axis);
    let (size, start) = slider_thumb(max, value, viewport, length);
    (along.clamp(0.0, length) * 2.0, length * 2.0, start, size)
}

fn press_track(el: Element, axis: usize, event: &MouseEvent) {
    let (mut at, mut length, mut start, mut size) = thumb_pointer(el, axis, event);
    let in_thumb = at >= (start / 2.0).floor() * 2.0 && at < ((start + size) / 2.0).ceil() * 2.0;
    if !in_thumb {
        let max = inner_of(el).map(|i| i.borrow().max[axis]).unwrap_or(0.0);
        el.scroll_axis(axis, if length == 0.0 { 0.0 } else { (at / length) * max }, false, false);
        if el.is_destroyed() {
            return;
        }
        (at, length, start, size) = thumb_pointer(el, axis, event);
    }
    let _ = length;
    if let Some(inner) = inner_of(el) {
        inner.borrow_mut().bar_press = Some(BarPress::Thumb { axis, offset: (at - start).clamp(0.0, size) });
    }
}

fn drag_thumb(el: Element, axis: usize, offset: f64, event: &MouseEvent) {
    let (at, length, _, size) = thumb_pointer(el, axis, event);
    let span = (length - size).max(0.0);
    let start = (at - offset).clamp(0.0, span);
    let max = inner_of(el).map(|i| i.borrow().max[axis]).unwrap_or(0.0);
    el.scroll_axis(axis, if span == 0.0 { 0.0 } else { (start / span) * max }, false, false);
}

fn end_bar_press(el: Element) {
    let Some(inner) = inner_of(el) else { return };
    let press = inner.borrow_mut().bar_press.take();
    if let Some(BarPress::Arrow { timer }) = press {
        if let Some(id) = timer.take() {
            crate::renderer::clear_timeout(id);
        }
    }
}

/// Creates a `<scrollbox>` (ScrollBox's constructor with no options: a row, stretching, clipped, focusable, the
/// vertical axis scrolling).
pub fn create_scrollbox() -> Element {
    let viewport_node = Box::new(layout::Node::new());
    let content_node = Box::new(layout::Node::new());
    viewport_node.set_enum(layout::ENUM_OVERFLOW, 1);
    content_node.set_enum(layout::ENUM_ALIGN_SELF, 1);
    viewport_node.set_float(layout::FLOAT_FLEX_GROW, 1.0);
    viewport_node.set_float(layout::FLOAT_FLEX_SHRINK, 1.0);
    content_node.set_value(layout::VALUE_MIN_WIDTH, 0, Value::Percent(100.0));
    content_node.set_value(layout::VALUE_MIN_HEIGHT, 0, Value::Percent(100.0));
    viewport_node.insert_child(&content_node, 0);
    let content_ptr = &*content_node as *const layout::Node;
    let inner = Rc::new(RefCell::new(Inner {
        viewport_node,
        content_node,
        axes: [false, true],
        sticky_scroll: false,
        sticky_start: None,
        culling: true,
        paddings: HashMap::new(),
        fills: [None, None, None],
        bars: [Bar::default(), Bar::default()],
        follow: None,
        padding: [0.0; 4],
        stuck: true,
        offset: [0.0; 2],
        max: [0.0; 2],
        extent: [0.0; 2],
        viewport: Rect::default(),
        content_origin: [0.0; 2],
        visible: [false; 2],
        toggle_frame: 0,
        toggles: 0,
        reported: [0.0; 6],
        changes: 0,
        target: [None, None],
        reveal: None,
        request_frame: 0,
        anchor: None,
        anchor_token: 0,
        layout_frame: 0,
        accel: Rc::new(LinearScrollAccel),
        accumulators: [0.0; 2],
        layout_callbacks: Vec::new(),
        change: [Vec::new(), Vec::new()],
        on_change: [None, None],
        viewport_resize: Vec::new(),
        content_resize: Vec::new(),
        hook: false,
        next_token: 0,
        bar_press: None,
    }));
    let kind = ScrollBoxKind { boxed: BoxKind::default(), inner: inner.clone(), gutters: [0.0, 0.0], resolved_frame: Cell::new(0) };
    let el = tree::create(Box::new(kind), "scrollbox", false);
    BOXES.with(|b| b.borrow_mut().insert(el, inner.clone()));
    el.with_yoga(|y| y.insert_child(&inner.borrow().viewport_node, 0));
    el.set_children_yoga(Some(content_ptr));
    el.set_flex_direction(crate::tree::FlexDirection::Row);
    el.set_align_items(crate::tree::Align::Stretch);
    el.set_overflow(crate::tree::Overflow::Hidden);
    el.set_focusable(true);
    // Inverted axes first, so both content size limits are written.
    el.apply_scroll_options(Some([true, false]));
    el
}

impl Element {
    fn scroll_inner(self) -> Option<Rc<RefCell<Inner>>> {
        inner_of(self)
    }

    /// `_applyScrollOptions` (the scene's `setScroll`).
    fn apply_scroll_options(self, previous_axes: Option<[bool; 2]>) {
        let Some(inner) = self.scroll_inner() else { return };
        let mut inner = inner.borrow_mut();
        let axes = inner.axes;
        for (axis, kind) in [(0, layout::VALUE_MAX_WIDTH), (1, layout::VALUE_MAX_HEIGHT)] {
            if previous_axes.is_some_and(|p| p[axis] == axes[axis]) {
                continue;
            }
            let value = if axes[axis] { Value::Undefined } else { Value::Percent(100.0) };
            inner.content_node.set_value(kind, 0, value);
        }
        let padding = inner.edges();
        for (i, edge) in [layout::EDGE_LEFT, layout::EDGE_TOP, layout::EDGE_RIGHT, layout::EDGE_BOTTOM].into_iter().enumerate() {
            inner.viewport_node.set_value(layout::VALUE_PADDING, edge, Value::Point(padding[i] as f32));
        }
        inner.padding = padding;
        let follow = if inner.sticky_scroll { inner.sticky_start } else { None };
        let follow_changed = follow != inner.follow;
        inner.follow = follow;
        drop(inner);
        self.mark_layout_dirty_scroll();
        if follow_changed {
            if let Some(inner) = self.scroll_inner() {
                inner.borrow_mut().restick();
            }
        }
        self.request_render();
    }

    fn mark_layout_dirty_scroll(self) {
        self.with_kind::<ScrollBoxKind, _>(|k| {
            let inner = k.inner.clone();
            let inner = inner.borrow();
            inner.apply_gutters(self, &mut k.gutters)
        });
    }

    /// `_scrollAxis(axis, value, relative, reapply)` with `commitScroll`: `"change"` reaches listeners at once.
    pub fn scroll_axis(self, axis: usize, value: f64, relative: bool, reapply: bool) {
        let Some(inner) = self.scroll_inner() else { return };
        if value.is_nan() {
            return;
        }
        let (before, after) = {
            let mut inner = inner.borrow_mut();
            let before = inner.offset;
            let x = if axis == 0 { value } else { 0.0 };
            let y = if axis == 1 { value } else { 0.0 };
            inner.scroll_to([x, y], [axis != 0 || relative, axis != 1 || relative], reapply);
            (before, inner.offset)
        };
        let moved = (if after[0] != before[0] { 1 } else { 0 }) | (if after[1] != before[1] { 2 } else { 0 });
        if moved != 0 {
            self.request_render();
        }
        self.notify_offsets(after, moved);
    }

    fn notify_offsets(self, offset: [f64; 2], moved: u32) {
        let Some(inner) = self.scroll_inner() else { return };
        for axis in [1usize, 0] {
            if moved & (axis as u32 + 1) == 0 {
                continue;
            }
            let listeners: Vec<Rc<dyn Fn(f64)>> = {
                let inner = inner.borrow();
                let mut out: Vec<Rc<dyn Fn(f64)>> = inner.on_change[axis].iter().cloned().collect();
                out.extend(inner.change[axis].iter().map(|(_, f)| f.clone()));
                out
            };
            for listener in listeners {
                listener(offset[axis]);
            }
            if self.is_destroyed() {
                return;
            }
        }
    }

    /// `scrollUnitSize(bar, unit)`.
    fn scroll_unit_size(self, axis: usize, unit: ScrollUnit) -> f64 {
        let Some(inner) = self.scroll_inner() else { return 1.0 };
        let inner = inner.borrow();
        match unit {
            ScrollUnit::Viewport => if axis == 1 { inner.viewport.height } else { inner.viewport.width },
            ScrollUnit::Content => inner.extent[axis],
            ScrollUnit::Step => inner.bars[axis].scroll_step.unwrap_or(1.0),
            ScrollUnit::Absolute => 1.0,
        }
    }

    /// A bar's `scrollBy(delta, unit)`: adds to a pending target and applies again after layout.
    fn bar_scroll_by(self, axis: usize, delta: f64, unit: ScrollUnit) {
        let size = self.scroll_unit_size(axis, unit);
        self.scroll_axis(axis, size * delta, true, true);
    }

    /// `scrollTop`.
    pub fn scroll_top(self) -> f64 {
        self.scroll_inner().map(|i| i.borrow().offset[1]).unwrap_or(0.0)
    }
    pub fn set_scroll_top(self, value: f64) {
        self.scroll_axis(1, value, false, false);
    }
    /// `scrollLeft`.
    pub fn scroll_left(self) -> f64 {
        self.scroll_inner().map(|i| i.borrow().offset[0]).unwrap_or(0.0)
    }
    pub fn set_scroll_left(self, value: f64) {
        self.scroll_axis(0, value, false, false);
    }
    /// `scrollHeight` (the content's height).
    pub fn scroll_height(self) -> f64 {
        self.scroll_inner().map(|i| i.borrow().extent[1]).unwrap_or(0.0)
    }
    /// `scrollWidth`.
    pub fn scroll_width(self) -> f64 {
        self.scroll_inner().map(|i| i.borrow().extent[0]).unwrap_or(0.0)
    }
    /// The largest offset on an axis (0 x, 1 y), from the last solve.
    pub fn scroll_max(self, axis: usize) -> f64 {
        self.scroll_inner().map(|i| i.borrow().max[axis]).unwrap_or(0.0)
    }
    /// Whether the view is stuck at its sticky edge.
    pub fn scroll_stuck(self) -> bool {
        self.scroll_inner().map(|i| i.borrow().stuck).unwrap_or(false)
    }

    /// `scrollBy(delta, unit)` (the vertical bar's).
    pub fn scroll_by(self, delta: f64, unit: ScrollUnit) {
        self.bar_scroll_by(1, delta, unit);
    }

    /// `scrollBy({ x, y }, unit)`.
    pub fn scroll_by_xy(self, x: f64, y: f64, unit: ScrollUnit) {
        self.bar_scroll_by(1, y, unit);
        if self.is_destroyed() {
            return;
        }
        self.bar_scroll_by(0, x, unit);
    }

    /// `scrollTo(position)`: now, and again after the next frame's layout (a position at or past the end stays at
    /// the end, `f64::INFINITY` included).
    pub fn scroll_to(self, y: f64) {
        self.scroll_axis(1, y, false, true);
    }

    /// `scrollTo({ x, y })`.
    pub fn scroll_to_xy(self, x: f64, y: f64) {
        self.scroll_axis(1, y, false, true);
        if self.is_destroyed() {
            return;
        }
        self.scroll_axis(0, x, false, true);
    }

    /// `scrollChildIntoView(child, { align, inset })`.
    pub fn scroll_child_into_view(self, child: Element, align: ScrollAlign, inset: f64) {
        let Some(inner) = self.scroll_inner() else { return };
        let Some(bounds) = content_rect(self, child) else { return };
        let (before, after) = {
            let mut inner = inner.borrow_mut();
            let before = inner.offset;
            inner.target = [None, None];
            inner.reveal = Some((child, align, inset));
            if !bounds[2].is_nan() {
                let positions = inner.reveal_offsets(align, inset, bounds);
                inner.aim(positions);
            } else {
                inner.request_frame = next_frame();
            }
            (before, inner.offset)
        };
        let moved = (if after[0] != before[0] { 1 } else { 0 }) | (if after[1] != before[1] { 2 } else { 0 });
        if moved != 0 {
            self.request_render();
        }
        self.notify_offsets(after, moved);
    }

    /// `afterLayout(callback)`: once in the next frame, after its layout and before it paints (asked during a frame,
    /// in the frame after); it waits while the box is hidden and goes with the box.
    pub fn after_layout(self, callback: impl FnOnce() + 'static) {
        let Some(inner) = self.scroll_inner() else { return };
        {
            let mut inner = inner.borrow_mut();
            inner.layout_callbacks.push((crate::renderer::frame_id(), Box::new(callback)));
            inner.refresh_hook();
            if inner.layout_frame == 0 {
                inner.layout_frame = crate::renderer::frame_id() + 1;
            }
        }
        self.request_render();
    }

    /// `anchor(node)`: keeps a descendant at its place in the viewport through layout changes until the returned
    /// release runs, the node leaves the box, a scroll to the end, or a later anchor.
    pub fn anchor(self, node: Element) -> Box<dyn FnOnce()> {
        let Some(inner) = self.scroll_inner() else { return Box::new(|| {}) };
        let bounds = content_rect(self, node);
        let token = {
            let mut inner = inner.borrow_mut();
            let offset = inner.offset;
            inner.anchor = bounds.map(|at| (node, (!at[2].is_nan()).then(|| [at[0] - offset[0], at[1] - offset[1]])));
            inner.anchor_token += 1;
            inner.anchor_token
        };
        Box::new(move || {
            let Some(inner) = inner_of(self) else { return };
            let mut inner = inner.borrow_mut();
            if inner.anchor_token != token {
                return;
            }
            inner.anchor = None;
            inner.anchor_token += 1;
        })
    }

    /// `viewport`: the hidden viewport node's rect in screen cells.
    pub fn scroll_viewport(self) -> Rect {
        let Some(inner) = self.scroll_inner() else { return Rect::default() };
        let inner = inner.borrow();
        let (x, y) = self.screen_position();
        Rect { x: x + inner.viewport.x, y: y + inner.viewport.y, width: inner.viewport.width, height: inner.viewport.height }
    }

    /// `content`: the content node's rect in screen cells (moving with the offset).
    pub fn scroll_content(self) -> Rect {
        let Some(inner) = self.scroll_inner() else { return Rect::default() };
        let inner = inner.borrow();
        let (x, y) = self.screen_position();
        Rect {
            x: x + inner.content_origin[0] - inner.offset[0],
            y: y + inner.content_origin[1] - inner.offset[1],
            width: inner.extent[0],
            height: inner.extent[1],
        }
    }

    /// Whether a bar is shown (`verticalScrollBar.visible` / `horizontalScrollBar.visible`).
    pub fn scrollbar_visible(self, vertical: bool) -> bool {
        self.scroll_inner().map(|i| i.borrow().visible[vertical as usize]).unwrap_or(false)
    }

    pub fn set_sticky_scroll(self, sticky: bool) {
        let Some(inner) = self.scroll_inner() else { return };
        inner.borrow_mut().sticky_scroll = sticky;
        self.apply_scroll_options(None);
    }

    pub fn set_sticky_start(self, start: Option<StickyStart>) {
        let Some(inner) = self.scroll_inner() else { return };
        inner.borrow_mut().sticky_start = start;
        self.apply_scroll_options(None);
    }

    /// `scrollX`: on an axis that does not scroll, the content is at most as wide as the viewport.
    pub fn set_scroll_x(self, scroll: bool) {
        let Some(inner) = self.scroll_inner() else { return };
        let previous = inner.borrow().axes;
        inner.borrow_mut().axes[0] = scroll;
        self.apply_scroll_options(Some(previous));
    }

    pub fn set_scroll_y(self, scroll: bool) {
        let Some(inner) = self.scroll_inner() else { return };
        let previous = inner.borrow().axes;
        inner.borrow_mut().axes[1] = scroll;
        self.apply_scroll_options(Some(previous));
    }

    pub fn set_viewport_culling(self, culling: bool) {
        let Some(inner) = self.scroll_inner() else { return };
        inner.borrow_mut().culling = culling;
        self.apply_scroll_options(None);
    }

    pub fn set_scroll_acceleration(self, accel: Rc<dyn ScrollAcceleration>) {
        let Some(inner) = self.scroll_inner() else { return };
        inner.borrow_mut().accel = accel;
    }

    /// `viewportOptions.padding*`: `key` is `padding`, `paddingX`, `paddingY`, `paddingLeft`, `paddingTop`,
    /// `paddingRight` or `paddingBottom`; `None` removes it.
    pub fn set_viewport_padding(self, key: &'static str, value: Option<u32>) {
        let Some(inner) = self.scroll_inner() else { return };
        match value {
            Some(value) => inner.borrow_mut().paddings.insert(key, value.min(0xffff)),
            None => inner.borrow_mut().paddings.remove(key),
        };
        self.apply_scroll_options(None);
    }

    /// `wrapperOptions.backgroundColor`, `viewportOptions.backgroundColor`, `contentOptions.backgroundColor`
    /// (`part` 0, 1, 2).
    pub fn set_scroll_fill(self, part: usize, color: Option<RGBA>) {
        let Some(inner) = self.scroll_inner() else { return };
        inner.borrow_mut().fills[part] = color;
        self.apply_scroll_options(None);
    }

    /// A style of the hidden content node (`contentOptions`).
    pub fn with_scroll_content(self, f: impl FnOnce(&layout::Node)) {
        let Some(inner) = self.scroll_inner() else { return };
        f(&inner.borrow().content_node);
        self.request_render();
    }

    /// `scrollbarOptions` (`both`), `verticalScrollbarOptions` or `horizontalScrollbarOptions`.
    pub fn set_scrollbar_options(self, vertical: Option<bool>, options: &ScrollBarOptions) {
        let Some(inner) = self.scroll_inner() else { return };
        let axes: &[usize] = match vertical {
            None => &[1, 0],
            Some(true) => &[1],
            Some(false) => &[0],
        };
        {
            let mut inner = inner.borrow_mut();
            for &axis in axes {
                let max = inner.max[axis];
                let bar = &mut inner.bars[axis];
                let old_policy = bar.policy;
                if let Some(visible) = options.visible {
                    bar.policy = match visible {
                        None => Policy::Auto,
                        Some(true) => Policy::Always,
                        Some(false) => Policy::Never,
                    };
                }
                if let Some(arrows) = options.show_arrows {
                    bar.arrows = arrows;
                }
                if let Some(arrow) = &options.arrow_options {
                    let current = &mut bar.arrow_options;
                    if arrow.attributes.is_some() {
                        current.attributes = arrow.attributes;
                    }
                    if arrow.foreground.is_some() {
                        current.foreground = arrow.foreground;
                    }
                    if arrow.background.is_some() {
                        current.background = arrow.background;
                    }
                    for i in 0..2 {
                        if arrow.chars[i].is_some() {
                            current.chars[i] = arrow.chars[i].clone();
                        }
                    }
                }
                if let Some(fg) = options.track_foreground {
                    bar.foreground = fg;
                }
                if let Some(bg) = options.track_background {
                    bar.background = bg;
                }
                if let Some(thickness) = options.thickness {
                    bar.thickness = (thickness.trunc().clamp(0.0, 255.0) as u32).max(1);
                }
                if let Some(gap) = options.gap {
                    bar.gap = gap.trunc().clamp(0.0, 255.0) as u32;
                }
                if let Some(overlay) = options.overlay {
                    bar.overlay = overlay;
                }
                if let Some(step) = options.scroll_step {
                    bar.scroll_step = step;
                }
                let policy = bar.policy;
                // A changed policy applies now; the next solve corrects an auto bar's visibility.
                if policy != old_policy {
                    inner.visible[axis] = match policy {
                        Policy::Auto => max > 0.0,
                        Policy::Always => true,
                        Policy::Never => false,
                    };
                }
            }
        }
        self.apply_scroll_options(None);
    }

    /// `verticalScrollBar.on("change", f)` / `horizontalScrollBar.on("change", f)`.
    pub fn on_scroll_change(self, vertical: bool, listener: Rc<dyn Fn(f64)>) -> u64 {
        let Some(inner) = self.scroll_inner() else { return 0 };
        let mut inner = inner.borrow_mut();
        inner.next_token += 1;
        let token = inner.next_token;
        inner.change[vertical as usize].push((token, listener));
        inner.refresh_hook();
        token
    }

    /// `verticalScrollbarOptions.onChange`.
    pub fn set_on_scroll_change(self, vertical: bool, listener: Option<Rc<dyn Fn(f64)>>) {
        let Some(inner) = self.scroll_inner() else { return };
        let mut inner = inner.borrow_mut();
        inner.on_change[vertical as usize] = listener;
        inner.refresh_hook();
    }

    /// `viewport.on("resize", f)`.
    pub fn on_viewport_resize(self, listener: Rc<dyn Fn(())>) -> u64 {
        let Some(inner) = self.scroll_inner() else { return 0 };
        let mut inner = inner.borrow_mut();
        inner.next_token += 1;
        let token = inner.next_token;
        inner.viewport_resize.push((token, listener));
        inner.refresh_hook();
        token
    }

    /// `content.on("resize", f)`.
    pub fn on_content_resize(self, listener: Rc<dyn Fn(())>) -> u64 {
        let Some(inner) = self.scroll_inner() else { return 0 };
        let mut inner = inner.borrow_mut();
        inner.next_token += 1;
        let token = inner.next_token;
        inner.content_resize.push((token, listener));
        inner.refresh_hook();
        token
    }

    /// Removes a listener added with `on_scroll_change`, `on_viewport_resize` or `on_content_resize`.
    pub fn off_scroll_listener(self, token: u64) {
        let Some(inner) = self.scroll_inner() else { return };
        let mut inner = inner.borrow_mut();
        for list in inner.change.iter_mut() {
            list.retain(|(t, _)| *t != token);
        }
        inner.viewport_resize.retain(|(t, _)| *t != token);
        inner.content_resize.retain(|(t, _)| *t != token);
        inner.refresh_hook();
    }
}
