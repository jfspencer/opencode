//! The renderable tree: `Renderable.ts` (BaseRenderable, Renderable, RootRenderable) together with the part of the
//! native scene (`scene.zig`) that decides layout feedback, paint order, clipping, opacity and hit testing.
//!
//! An [`Element`] is a `Copy` handle to a node in a thread-local arena. Each node has a [`Base`] (what
//! `Renderable` holds: id, parent, children in layout order, the Yoga node, visibility, z-index, opacity,
//! translation, focus, live count, event handlers, hooks) and a [`Kind`] (what a subclass adds: a box's borders,
//! a text's buffer, a scroll box's state). Text nodes (`TextNodeRenderable`, a BaseRenderable) live in the same
//! arena without a Yoga node.
//!
//! Divergences from 0.8.1's native scene, none visible on screen: frames repaint every member into the next
//! buffer (the scene's damage tracking, banding, culling and retained records only skip work whose result is
//! equal); a layout round reads every node's Yoga layout instead of the nodes Yoga marked.
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use crate::buffer::OptimizedBuffer;
use crate::layout::{self, Value};

/// A handle to a renderable or text node.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Element {
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

/// Mouse event types (`MouseEventType`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum MouseEventType {
    Down,
    Up,
    Move,
    Drag,
    DragEnd,
    Drop,
    Over,
    Out,
    Scroll,
}

impl MouseEventType {
    const ALL: [MouseEventType; 9] = [
        MouseEventType::Down,
        MouseEventType::Up,
        MouseEventType::Move,
        MouseEventType::Drag,
        MouseEventType::DragEnd,
        MouseEventType::Drop,
        MouseEventType::Over,
        MouseEventType::Out,
        MouseEventType::Scroll,
    ];
    fn slot(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap()
    }
}

/// `ScrollInfo`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ScrollInfo {
    pub direction: ScrollDirection,
    pub delta: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// `MouseEvent` (renderer.ts).
#[derive(Clone, Debug)]
pub struct MouseEvent {
    pub kind: MouseEventType,
    pub button: u32,
    pub x: i32,
    pub y: i32,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub scroll: Option<ScrollInfo>,
    pub target: Option<Element>,
    pub current_target: Option<Element>,
    pub source: Option<Element>,
    pub is_dragging: bool,
    pub propagation_stopped: bool,
    pub default_prevented: bool,
    /// The scroll box that took this wheel step (`lib/scroll-chain.ts`'s takers).
    pub scroll_taker: Option<Element>,
}

impl MouseEvent {
    pub fn stop_propagation(&mut self) {
        self.propagation_stopped = true;
    }
    pub fn prevent_default(&mut self) {
        self.default_prevented = true;
    }
}

pub type MouseHandler = Rc<dyn Fn(&mut MouseEvent)>;
pub type KeyHandler = Rc<dyn Fn(&mut crate::input::KeyEvent)>;
pub type PasteHandler = Rc<dyn Fn(&mut crate::input::PasteEvent)>;
/// `renderBefore` / `renderAfter`: draws into the frame with the member's clip and opacity.
pub type PaintHook = Rc<dyn Fn(Element, &mut OptimizedBuffer, f64)>;

/// What a renderable subclass adds to `Renderable`. Methods get the node's handle; they may call [`Element`]
/// methods, which borrow the tree briefly (the kind itself is taken out of the arena while it runs).
pub trait Kind: Any {
    fn name(&self) -> &'static str;
    /// Paints the node's body (`renderSelf` / the native body), in screen cells at `x`, `y`.
    fn render(&mut self, _el: Element, _buffer: &mut OptimizedBuffer, _x: i32, _y: i32, _width: u32, _height: u32) {}
    /// Whether [`Kind::render`] draws anything (a layout-only box paints nothing).
    fn paints(&self) -> bool {
        true
    }
    /// `onUpdate(deltaTime)`, once per frame before layout.
    fn on_update(&mut self, _el: Element, _delta: f64) {}
    /// `onAnimationTick(deltaTime, ticks, time)`: the shared animation clock's tick while
    /// [`Element::set_animation_fps`] is set and the node is drawn.
    fn on_animation_tick(&mut self, _el: Element, _delta: f64, _ticks: u64, _time: f64) {}
    /// `onResize(width, height)` after the layout gave the node a new size.
    fn on_resize(&mut self, _el: Element, _width: f32, _height: f32) {}
    /// `onLifecyclePass`, before layout (a text composing its chunks from its text nodes).
    fn lifecycle_pass(&mut self, _el: Element) {}
    fn needs_lifecycle_pass(&self) -> bool {
        false
    }
    /// `handleKeyPress`; true when handled.
    fn handle_key_press(&mut self, _el: Element, _key: &mut crate::input::KeyEvent) -> bool {
        false
    }
    fn handles_keys(&self) -> bool {
        false
    }
    /// `handlePaste`.
    fn handle_paste(&mut self, _el: Element, _event: &mut crate::input::PasteEvent) {}
    fn handles_paste(&self) -> bool {
        false
    }
    /// `onMouseEvent`, after the node's handlers.
    fn on_mouse_event(&mut self, _el: Element, _event: &mut MouseEvent) {}
    /// After a child was added, removed or moved (text composition, scroll content).
    fn children_changed(&mut self, _el: Element) {}
    /// Painted after the children (a scroll box's bars).
    fn render_after_children(&mut self, _el: Element, _buffer: &mut OptimizedBuffer, _x: i32, _y: i32, _w: u32, _h: u32) {}
    fn has_after_children(&self) -> bool {
        false
    }
    /// Where children are positioned and clipped relative to the node (a scroll box's content offset), if not the
    /// node itself: (offset x, offset y, clip rect in local cells).
    fn child_frame(&self, _el: Element, _width: f32, _height: f32) -> Option<ChildFrame> {
        None
    }
    /// After a layout round set the node's geometry, before its children's (a scroll box reads its hidden Yoga
    /// nodes, follows its sticky edge and toggles its bars).
    fn layout_resolved(&mut self, _el: Element) {}
    fn resolves_layout(&self) -> bool {
        false
    }
    /// After a layout round's resize events: what the node reports (a scroll box's SCROLL request), run outside
    /// the node.
    fn layout_notifications(&mut self, _el: Element) -> Option<Box<dyn FnOnce()>> {
        None
    }
    /// Hit rectangles painted after the children (a scroll box's bars), in screen cells.
    fn hits_after_children(&self, _el: Element, _x: i32, _y: i32, _w: u32, _h: u32) -> Vec<(i32, i32, u32, u32)> {
        Vec::new()
    }
    fn destroy(&mut self, _el: Element) {}
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// See [`Kind::child_frame`].
#[derive(Clone, Copy, Debug)]
pub struct ChildFrame {
    pub offset_x: f64,
    pub offset_y: f64,
    pub clip: (i32, i32, i32, i32),
}

/// The computed geometry of a node (`getLayout()` plus the screen position of the last layout round).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Geometry {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub screen_x: f64,
    pub screen_y: f64,
}

#[derive(Default)]
pub(crate) struct Handlers {
    pub mouse_any: Option<MouseHandler>,
    pub mouse: [Option<MouseHandler>; 9],
    pub key_down: Option<KeyHandler>,
    pub paste: Option<PasteHandler>,
    pub size_change: Option<Rc<dyn Fn()>>,
    pub render_before: Option<PaintHook>,
    pub render_after: Option<PaintHook>,
    /// `on(event, listener)` for named events ("resize", "focused", "blurred", "destroyed", custom ones).
    pub listeners: Vec<(&'static str, u64, Rc<dyn Fn()>)>,
}

/// What `Renderable` holds for every node.
pub struct Base {
    pub id: String,
    pub num: u32,
    pub parent: Option<Element>,
    pub children: Vec<Element>,
    pub(crate) yoga: Option<layout::Node>,
    pub text_node: bool,
    pub visible: bool,
    pub z_index: i32,
    pub opacity: f32,
    pub translate_x: f64,
    pub translate_y: f64,
    pub position_absolute: bool,
    pub overflow: Overflow,
    pub width_spec: Value,
    pub height_spec: Value,
    pub flex_shrink: f32,
    pub focusable: bool,
    pub focused: bool,
    pub selectable: bool,
    pub live: bool,
    pub live_count: u32,
    pub destroyed: bool,
    /// Border sides (left, top, right, bottom bits) the node paints: children of an overflow-clipping node are
    /// clipped inside them.
    pub border_mask: u32,
    pub geometry: Geometry,
    /// The geometry that last reached `onResize`.
    resized: Option<(f32, f32)>,
    pub(crate) handlers: Handlers,
    /// Attach order among siblings (equal z-index paints in attachment order).
    attach_order: u64,
    /// The Yoga node the children attach to when it is not the node's own (a scroll box's hidden content node,
    /// owned by its kind, which lives as long as the node).
    pub(crate) children_yoga: Option<*const layout::Node>,
    /// `animationFps`, the frame that last updated the node, and its subscription to the animation clock.
    animation_fps: Option<f64>,
    animation_drawn_frame: i64,
    animation_ticker: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overflow {
    Visible,
    Hidden,
    Scroll,
}

struct Slot {
    generation: u32,
    base: Option<Base>,
    kind: Option<Box<dyn Kind>>,
}

#[derive(Default)]
pub(crate) struct Tree {
    slots: Vec<Slot>,
    free: Vec<u32>,
    next_num: u32,
    next_attach: u64,
    next_listener: u64,
    pub(crate) root: Option<Element>,
    pub(crate) focused: Option<Element>,
    /// Nodes removed from a parent, destroyed at the end of the tick unless re-attached (`process.nextTick` in
    /// the reconciler's removeNode).
    pub(crate) pending_destroy: Vec<Element>,
    /// Numbered nodes for hit testing (`renderablesByNumber`).
    by_num: std::collections::HashMap<u32, Element>,
}

thread_local! {
    static TREE: RefCell<Tree> = RefCell::new(Tree::default());
}

pub(crate) fn with_tree<R>(f: impl FnOnce(&mut Tree) -> R) -> R {
    TREE.with(|tree| f(&mut tree.borrow_mut()))
}

fn request_render() {
    crate::renderer::request_render();
}

impl Tree {
    fn base(&self, el: Element) -> Option<&Base> {
        self.slots.get(el.index as usize).filter(|s| s.generation == el.generation).and_then(|s| s.base.as_ref())
    }

    fn base_mut(&mut self, el: Element) -> Option<&mut Base> {
        self.slots
            .get_mut(el.index as usize)
            .filter(|s| s.generation == el.generation)
            .and_then(|s| s.base.as_mut())
    }
}

/// Creates a node of a kind (`new XRenderable(ctx, { id })`). Text nodes have no Yoga node.
pub fn create(kind: Box<dyn Kind>, id_prefix: &str, text_node: bool) -> Element {
    with_tree(|tree| {
        tree.next_num += 1;
        let num = tree.next_num;
        let yoga = if text_node { None } else { Some(layout::Node::new()) };
        let base = Base {
            id: format!("{id_prefix}-{num}"),
            num,
            parent: None,
            children: Vec::new(),
            yoga,
            text_node,
            visible: true,
            z_index: 0,
            opacity: 1.0,
            translate_x: 0.0,
            translate_y: 0.0,
            position_absolute: false,
            overflow: Overflow::Visible,
            width_spec: Value::Auto,
            height_spec: Value::Auto,
            flex_shrink: 1.0,
            focusable: false,
            focused: false,
            selectable: false,
            live: false,
            live_count: 0,
            destroyed: false,
            border_mask: 0,
            geometry: Geometry::default(),
            resized: None,
            handlers: Handlers::default(),
            attach_order: 0,
            children_yoga: None,
            animation_fps: None,
            animation_drawn_frame: -1,
            animation_ticker: None,
        };
        let el = if let Some(index) = tree.free.pop() {
            let slot = &mut tree.slots[index as usize];
            slot.generation = slot.generation.wrapping_add(1);
            slot.base = Some(base);
            slot.kind = Some(kind);
            Element { index, generation: slot.generation }
        } else {
            let index = tree.slots.len() as u32;
            tree.slots.push(Slot { generation: 0, base: Some(base), kind: Some(kind) });
            Element { index, generation: 0 }
        };
        tree.by_num.insert(num, el);
        el
    })
    .with_default_yoga()
}

impl Element {
    /// Defaults `setupYogaProperties` applies for empty options: flexGrow 0, flexShrink 1, column, no wrap,
    /// stretch, flex-start, align-self auto.
    fn with_default_yoga(self) -> Element {
        self.with_yoga(|y| {
            y.set_float(layout::FLOAT_FLEX_GROW, 0.0);
            y.set_float(layout::FLOAT_FLEX_SHRINK, 1.0);
            y.set_enum(layout::ENUM_FLEX_DIRECTION, 0);
            y.set_enum(layout::ENUM_FLEX_WRAP, 0);
            y.set_enum(layout::ENUM_ALIGN_ITEMS, 4);
            y.set_enum(layout::ENUM_JUSTIFY_CONTENT, 0);
            y.set_enum(layout::ENUM_ALIGN_SELF, 0);
        });
        self
    }

    pub fn is_alive(self) -> bool {
        with_tree(|t| t.base(self).is_some_and(|b| !b.destroyed))
    }

    pub fn exists(self) -> bool {
        with_tree(|t| t.base(self).is_some())
    }

    /// Reads the base (None once the slot is gone).
    pub fn with_base<R>(self, f: impl FnOnce(&Base) -> R) -> Option<R> {
        with_tree(|t| t.base(self).map(f))
    }

    pub fn with_base_mut<R>(self, f: impl FnOnce(&mut Base) -> R) -> Option<R> {
        with_tree(|t| t.base_mut(self).map(f))
    }

    pub(crate) fn with_yoga(self, f: impl FnOnce(&layout::Node)) {
        with_tree(|t| {
            if let Some(y) = t.base(self).and_then(|b| b.yoga.as_ref()) {
                f(y)
            }
        })
    }

    /// Runs `f` on the node's kind, downcast to `K`. The kind is taken out of the arena while `f` runs, so `f` may
    /// use the tree; a nested call on the same node returns None.
    pub fn with_kind<K: Kind, R>(self, f: impl FnOnce(&mut K) -> R) -> Option<R> {
        let mut kind = with_tree(|t| {
            t.slots.get_mut(self.index as usize).filter(|s| s.generation == self.generation).and_then(|s| s.kind.take())
        })?;
        let result = kind.as_any_mut().downcast_mut::<K>().map(f);
        with_tree(|t| {
            if let Some(slot) = t.slots.get_mut(self.index as usize).filter(|s| s.generation == self.generation) {
                if slot.kind.is_none() {
                    slot.kind = Some(kind);
                }
            }
        });
        result
    }

    /// Runs `f` on the node's kind as a trait object.
    pub(crate) fn with_dyn_kind<R>(self, f: impl FnOnce(&mut dyn Kind) -> R) -> Option<R> {
        let mut kind = with_tree(|t| {
            t.slots.get_mut(self.index as usize).filter(|s| s.generation == self.generation).and_then(|s| s.kind.take())
        })?;
        let result = f(kind.as_mut());
        with_tree(|t| {
            if let Some(slot) = t.slots.get_mut(self.index as usize).filter(|s| s.generation == self.generation) {
                if slot.kind.is_none() {
                    slot.kind = Some(kind);
                }
            }
        });
        Some(result)
    }

    pub fn is<K: Kind>(self) -> bool {
        with_tree(|t| {
            t.slots
                .get(self.index as usize)
                .filter(|s| s.generation == self.generation)
                .and_then(|s| s.kind.as_ref())
                .is_some_and(|k| k.as_any().is::<K>())
        })
    }

    pub fn kind_name(self) -> &'static str {
        with_tree(|t| {
            t.slots
                .get(self.index as usize)
                .filter(|s| s.generation == self.generation)
                .and_then(|s| s.kind.as_ref())
                .map_or("", |k| k.name())
        })
    }

    pub fn id(self) -> String {
        self.with_base(|b| b.id.clone()).unwrap_or_default()
    }

    pub fn set_id(self, id: &str) {
        self.with_base_mut(|b| b.id = id.to_string());
    }

    pub fn num(self) -> u32 {
        self.with_base(|b| b.num).unwrap_or(0)
    }

    pub fn parent(self) -> Option<Element> {
        self.with_base(|b| b.parent).flatten()
    }

    pub fn children(self) -> Vec<Element> {
        self.with_base(|b| b.children.clone()).unwrap_or_default()
    }

    pub fn is_text_node(self) -> bool {
        self.with_base(|b| b.text_node).unwrap_or(false)
    }

    pub fn is_destroyed(self) -> bool {
        self.with_base(|b| b.destroyed).unwrap_or(true)
    }

    pub fn request_render(self) {
        request_render();
    }

    // -----------------------------------------------------------------------------------------------------------
    // Geometry
    // -----------------------------------------------------------------------------------------------------------

    pub fn geometry(self) -> Geometry {
        self.with_base(|b| b.geometry).unwrap_or_default()
    }

    /// The screen position as TS reads `x`/`y` (`composedLayout(.observed)`): the last layout's local position
    /// composed with the ancestors' current positions, translations and scroll offsets.
    pub fn screen_position(self) -> (f64, f64) {
        let Some((geometry, translate, parent)) = self.with_base(|b| (b.geometry, (b.translate_x, b.translate_y), b.parent))
        else {
            return (0.0, 0.0);
        };
        let Some(parent) = parent else { return (geometry.screen_x, geometry.screen_y) };
        let (px, py) = parent.screen_position();
        let pg = parent.geometry();
        let (ox, oy) = parent
            .with_dyn_kind(|k| k.child_frame(parent, pg.width, pg.height))
            .flatten()
            .map(|f| (f.offset_x, f.offset_y))
            .unwrap_or((0.0, 0.0));
        (px + ox + geometry.left as f64 + translate.0, py + oy + geometry.top as f64 + translate.1)
    }

    /// `x`: the screen column (`screenX`).
    pub fn x(self) -> i32 {
        self.screen_position().0 as i32
    }
    pub fn y(self) -> i32 {
        self.screen_position().1 as i32
    }
    /// `width`: the computed width.
    pub fn width(self) -> f32 {
        self.geometry().width
    }
    pub fn height(self) -> f32 {
        self.geometry().height
    }

    // -----------------------------------------------------------------------------------------------------------
    // Layout props (Renderable.ts setters)
    // -----------------------------------------------------------------------------------------------------------

    fn yoga_changed(self) {
        request_render();
    }

    pub fn set_visible(self, visible: bool) {
        let changed = self.with_base_mut(|b| {
            if b.visible == visible {
                return false;
            }
            b.visible = visible;
            true
        });
        if changed != Some(true) {
            return;
        }
        self.with_yoga(|y| y.set_enum(layout::ENUM_DISPLAY, if visible { 0 } else { 1 }));
        let (live, focused) = self.with_base(|b| (b.live, b.focused)).unwrap_or((false, false));
        if live {
            self.propagate_live_count(if visible { 1 } else { -1 });
        }
        self.update_animation_tick();
        if focused {
            self.blur();
        }
        request_render();
    }

    pub fn visible(self) -> bool {
        self.with_base(|b| b.visible).unwrap_or(false)
    }

    pub fn set_flex_grow(self, grow: Option<f32>) {
        self.with_yoga(|y| y.set_float(layout::FLOAT_FLEX_GROW, grow.unwrap_or(0.0)));
        self.yoga_changed();
    }

    pub fn set_flex_shrink(self, shrink: Option<f32>) {
        let value = shrink.unwrap_or(1.0);
        self.with_base_mut(|b| b.flex_shrink = value);
        self.with_yoga(|y| y.set_float(layout::FLOAT_FLEX_SHRINK, value));
        self.yoga_changed();
    }

    pub fn set_flex_direction(self, direction: FlexDirection) {
        self.with_yoga(|y| y.set_enum(layout::ENUM_FLEX_DIRECTION, direction as u32));
        self.yoga_changed();
    }

    pub fn set_flex_wrap(self, wrap: u32) {
        self.with_yoga(|y| y.set_enum(layout::ENUM_FLEX_WRAP, wrap));
        self.yoga_changed();
    }

    pub fn set_align_items(self, align: Align) {
        self.with_yoga(|y| y.set_enum(layout::ENUM_ALIGN_ITEMS, align as u32));
        self.yoga_changed();
    }

    pub fn set_align_self(self, align: Align) {
        self.with_yoga(|y| y.set_enum(layout::ENUM_ALIGN_SELF, align as u32));
        self.yoga_changed();
    }

    pub fn set_justify_content(self, justify: Justify) {
        self.with_yoga(|y| y.set_enum(layout::ENUM_JUSTIFY_CONTENT, justify as u32));
        self.yoga_changed();
    }

    pub fn set_flex_basis(self, basis: Value) {
        self.with_yoga(|y| y.set_value(layout::VALUE_FLEX_BASIS, 0, basis));
        self.yoga_changed();
    }

    /// `width = value` (`setDimension`): a number also disables flex shrink, as the bridge does it when the
    /// renderable's shrink was the default.
    pub fn set_width(self, value: Option<Value>) {
        self.set_dimension(layout::DIMENSION_WIDTH, value);
    }

    pub fn set_height(self, value: Option<Value>) {
        self.set_dimension(layout::DIMENSION_HEIGHT, value);
    }

    /// `setDimension`: a number while the shrink is the default 1 also disables shrinking.
    fn set_dimension(self, kind: u32, value: Option<Value>) {
        let value = value.unwrap_or(Value::Auto);
        let disable = self.with_base_mut(|b| {
            let spec = if kind == layout::DIMENSION_WIDTH { &mut b.width_spec } else { &mut b.height_spec };
            if *spec == value {
                return None;
            }
            *spec = value;
            let disable = matches!(value, Value::Point(_)) && b.flex_shrink == 1.0;
            if disable {
                b.flex_shrink = 0.0;
            }
            Some(disable)
        });
        let Some(Some(disable)) = disable else { return };
        self.with_yoga(|y| y.set_dimension(kind, value, disable));
        self.yoga_changed();
    }

    pub fn set_min_width(self, value: Option<Value>) {
        self.set_value(layout::VALUE_MIN_WIDTH, 0, value);
    }
    pub fn set_max_width(self, value: Option<Value>) {
        self.set_value(layout::VALUE_MAX_WIDTH, 0, value);
    }
    pub fn set_min_height(self, value: Option<Value>) {
        self.set_value(layout::VALUE_MIN_HEIGHT, 0, value);
    }
    pub fn set_max_height(self, value: Option<Value>) {
        self.set_value(layout::VALUE_MAX_HEIGHT, 0, value);
    }

    fn set_value(self, kind: u32, edge: u32, value: Option<Value>) {
        self.with_yoga(|y| y.set_value(kind, edge, value.unwrap_or(Value::Undefined)));
        self.yoga_changed();
    }

    pub fn set_margin(self, edge: Edge, value: Option<Value>) {
        self.set_value(layout::VALUE_MARGIN, edge as u32, value);
    }

    /// A scroll box's padding goes to its content node (ScrollBox.ts forwards the padding options there).
    pub fn set_padding(self, edge: Edge, value: Option<Value>) {
        let content = self.with_base(|b| b.children_yoga).flatten();
        if let Some(content) = content {
            unsafe { (*content).set_value(layout::VALUE_PADDING, edge as u32, value.unwrap_or(Value::Undefined)) };
            self.yoga_changed();
            return;
        }
        self.set_value(layout::VALUE_PADDING, edge as u32, value);
    }

    /// Sets the node the children attach to (see `Base::children_yoga`); only before it has children.
    pub(crate) fn set_children_yoga(self, node: Option<*const layout::Node>) {
        self.with_base_mut(|b| b.children_yoga = node);
    }

    pub fn set_gap(self, gutter: u32, value: Option<Value>) {
        self.set_value(layout::VALUE_GAP, gutter, value);
    }

    /// `position = "absolute" | "relative"`.
    pub fn set_position_type(self, absolute: bool) {
        self.with_base_mut(|b| b.position_absolute = absolute);
        self.with_yoga(|y| y.set_enum(layout::ENUM_POSITION_TYPE, if absolute { 2 } else { 1 }));
        self.yoga_changed();
    }

    /// `top`/`right`/`bottom`/`left` (`updateYogaPosition`): `Some(None)` unsets an edge.
    pub fn set_positions(self, left: Option<Option<Value>>, top: Option<Option<Value>>, right: Option<Option<Value>>, bottom: Option<Option<Value>>) {
        let mut mask = 0;
        let mut values = [Value::Undefined; 4];
        for (i, edge) in [left, top, right, bottom].into_iter().enumerate() {
            if let Some(value) = edge {
                mask |= 1 << i;
                values[i] = value.unwrap_or(Value::Undefined);
            }
        }
        self.with_yoga(|y| y.set_positions(mask, values));
        self.yoga_changed();
    }

    pub fn set_overflow(self, overflow: Overflow) {
        self.with_base_mut(|b| b.overflow = overflow);
        self.with_yoga(|y| {
            y.set_enum(
                layout::ENUM_OVERFLOW,
                match overflow {
                    Overflow::Visible => 0,
                    Overflow::Hidden => 1,
                    Overflow::Scroll => 2,
                },
            )
        });
        self.yoga_changed();
    }

    pub fn set_z_index(self, z: i32) {
        self.with_base_mut(|b| b.z_index = z);
        request_render();
    }

    pub fn set_opacity(self, opacity: f32) {
        self.with_base_mut(|b| b.opacity = opacity.clamp(0.0, 1.0));
        request_render();
    }

    pub fn set_translate(self, x: Option<f64>, y: Option<f64>) {
        self.with_base_mut(|b| {
            if let Some(x) = x {
                b.translate_x = x
            }
            if let Some(y) = y {
                b.translate_y = y
            }
        });
        request_render();
    }

    /// Yoga border widths of the painted sides (a box's border takes a cell on each side it draws).
    pub fn set_border_mask(self, mask: u32) {
        self.with_base_mut(|b| b.border_mask = mask);
        self.with_yoga(|y| {
            for (bit, edge) in [(1, layout::EDGE_LEFT), (2, layout::EDGE_TOP), (4, layout::EDGE_RIGHT), (8, layout::EDGE_BOTTOM)] {
                y.set_border(edge, if mask & bit != 0 { 1.0 } else { 0.0 });
            }
        });
        request_render();
    }

    pub fn set_selectable(self, selectable: bool) {
        self.with_base_mut(|b| b.selectable = selectable);
    }

    pub fn set_measure_func(self, measure: Option<layout::MeasureFn>) {
        // Text and editor views: the height cap only clamps their measurement (scene.zig).
        self.with_yoga(|y| y.set_measure_func(measure, true));
    }

    /// `invalidateIntrinsicSize` / native markDirty of a measured node.
    pub fn mark_layout_dirty(self) {
        self.with_yoga(|y| {
            if y.has_measure_func() {
                y.mark_dirty()
            }
        });
        request_render();
    }

    // -----------------------------------------------------------------------------------------------------------
    // Live count (requestLive / dropLive)
    // -----------------------------------------------------------------------------------------------------------

    /// `animationFps = value`: ticks [`Kind::on_animation_tick`] at this rate on the renderer's shared animation
    /// clock while the node is visible.
    pub fn set_animation_fps(self, fps: Option<f64>) {
        if let Some(fps) = fps {
            assert!(fps > 0.0 && fps.is_finite(), "animationFps must be positive and finite, got {fps}");
        }
        let changed = self.with_base_mut(|b| {
            if b.animation_fps == fps {
                return false;
            }
            b.animation_fps = fps;
            true
        });
        if changed == Some(true) {
            self.update_animation_tick();
        }
    }

    pub fn animation_fps(self) -> Option<f64> {
        self.with_base(|b| b.animation_fps).flatten()
    }

    /// Ticks only while the node is drawn. A hidden, culled or removed node gets no update, so the first tick after
    /// a frame without one stops the clock, and the node's next update starts it again.
    fn update_animation_tick(self) {
        let previous = self.with_base_mut(|b| b.animation_ticker.take()).flatten();
        if let Some(token) = previous {
            crate::renderer::off_animation_tick(token);
        }
        let Some((Some(fps), true, false)) = self.with_base(|b| (b.animation_fps, b.visible, b.destroyed)) else {
            return;
        };
        let el = self;
        let token = crate::renderer::on_animation_tick(
            fps,
            Rc::new(move |delta, ticks, time| {
                let drawn = el.with_base(|b| b.animation_drawn_frame).unwrap_or(-1);
                if drawn < crate::renderer::frame_id() as i64 - 1 {
                    let token = el.with_base_mut(|b| b.animation_ticker.take()).flatten();
                    if let Some(token) = token {
                        crate::renderer::off_animation_tick(token);
                    }
                    return;
                }
                el.with_dyn_kind(|k| k.on_animation_tick(el, delta, ticks, time));
            }),
        );
        self.with_base_mut(|b| b.animation_ticker = Some(token));
    }

    pub fn set_live(self, live: bool) {
        let delta = self.with_base_mut(|b| {
            if b.live == live {
                return 0;
            }
            b.live = live;
            if b.visible { if live { 1 } else { -1 } } else { 0 }
        });
        if let Some(delta) = delta.filter(|d| *d != 0) {
            self.propagate_live_count(delta);
        }
    }

    fn propagate_live_count(self, delta: i32) {
        let mut current = Some(self);
        while let Some(el) = current {
            let (before, after, parent) = match el.with_base_mut(|b| {
                let before = b.live_count;
                b.live_count = (b.live_count as i32 + delta).max(0) as u32;
                (before, b.live_count, b.parent)
            }) {
                Some(found) => found,
                None => return,
            };
            if parent.is_none() && Some(el) == with_tree(|t| t.root) {
                if before == 0 && after > 0 {
                    crate::renderer::request_live();
                } else if before > 0 && after == 0 {
                    crate::renderer::drop_live();
                }
            }
            current = parent;
        }
    }

    // -----------------------------------------------------------------------------------------------------------
    // Children (add / insertBefore / remove)
    // -----------------------------------------------------------------------------------------------------------

    /// `add(child, index)`: appends, or inserts before the child at `index`. Returns the index or -1.
    pub fn add(self, child: Element, index: Option<usize>) -> i32 {
        if self.is_destroyed() || child.is_destroyed() {
            return -1;
        }
        let anchor = index.and_then(|i| self.children().get(i).copied());
        self.place_child(child, anchor)
    }

    /// `insertBefore(child, anchor)`.
    pub fn insert_before(self, child: Element, anchor: Option<Element>) -> i32 {
        match anchor {
            None => self.add(child, None),
            Some(anchor) => {
                if self.is_destroyed() || child.is_destroyed() || anchor.is_destroyed() || anchor == child {
                    return -1;
                }
                if !self.children().contains(&anchor) {
                    return -1;
                }
                self.place_child(child, Some(anchor))
            }
        }
    }

    fn place_child(self, child: Element, anchor: Option<Element>) -> i32 {
        let previous = child.parent();
        let live = child.with_base(|b| b.live_count).unwrap_or(0);
        let index = with_tree(|t| {
            let previous_index = previous.and_then(|p| t.base(p)).and_then(|b| b.children.iter().position(|c| *c == child));
            let children = &t.base(self)?.children;
            let anchor_index = match anchor {
                Some(a) => children.iter().position(|c| *c == a)?,
                None => children.len(),
            };
            let index = anchor_index
                - if previous == Some(self) && previous_index.is_some_and(|p| p < anchor_index) { 1 } else { 0 };
            // Yoga: detach from the previous parent, attach here.
            if let (Some(p), Some(_)) = (previous, previous_index) {
                let removed = t.base_mut(p).map(|b| {
                    b.children.retain(|c| *c != child);
                });
                let _ = removed;
                let (parent_yoga, child_yoga) = (
                    t.base(p).and_then(|b| b.children_yoga.or(b.yoga.as_ref().map(|y| y as *const layout::Node))),
                    t.base(child).and_then(|b| b.yoga.as_ref()).map(|y| y as *const layout::Node),
                );
                if let (Some(py), Some(cy)) = (parent_yoga, child_yoga) {
                    unsafe { (*py).remove_child(&*cy) };
                }
            }
            t.next_attach += 1;
            let attach = t.next_attach;
            let base = t.base_mut(self)?;
            base.children.insert(index, child);
            let parent_yoga = base.children_yoga.or(base.yoga.as_ref().map(|y| y as *const layout::Node));
            let yoga_index = t
                .base(self)?
                .children
                .iter()
                .take(index)
                .filter(|c| t.base(**c).is_some_and(|b| b.yoga.is_some()))
                .count();
            let child_base = t.base_mut(child)?;
            child_base.parent = Some(self);
            child_base.attach_order = attach;
            let child_yoga = child_base.yoga.as_ref().map(|y| y as *const layout::Node);
            if let (Some(py), Some(cy)) = (parent_yoga, child_yoga) {
                unsafe { (*py).insert_child(&*cy, yoga_index as u32) };
            }
            t.pending_destroy.retain(|e| *e != child);
            Some(index as i32)
        });
        let Some(index) = index else { return -1 };
        if previous != Some(self) && live > 0 {
            if let Some(p) = previous {
                p.propagate_live_count(-(live as i32));
            }
            self.propagate_live_count(live as i32);
        }
        if let Some(p) = previous.filter(|p| *p != self) {
            p.with_dyn_kind(|k| k.children_changed(p));
        }
        self.with_dyn_kind(|k| k.children_changed(self));
        request_render();
        index
    }

    /// `remove(child)`.
    pub fn remove(self, child: Element) {
        let found = with_tree(|t| {
            let base = t.base_mut(self)?;
            let index = base.children.iter().position(|c| *c == child)?;
            base.children.remove(index);
            let parent_yoga = base.children_yoga.or(base.yoga.as_ref().map(|y| y as *const layout::Node));
            let child_base = t.base_mut(child)?;
            child_base.parent = None;
            let child_yoga = child_base.yoga.as_ref().map(|y| y as *const layout::Node);
            if let (Some(py), Some(cy)) = (parent_yoga, child_yoga) {
                unsafe { (*py).remove_child(&*cy) };
            }
            Some(child_base.live_count)
        });
        let Some(live) = found else { return };
        if child.has_focus_within() {
            request_render();
        }
        if live > 0 {
            self.propagate_live_count(-(live as i32));
        }
        self.with_dyn_kind(|k| k.children_changed(self));
        request_render();
    }

    /// The reconciler's removeNode: remove, then destroy recursively at the end of the tick unless the node was
    /// attached again by then.
    pub fn remove_and_schedule_destroy(self, child: Element) {
        self.remove(child);
        with_tree(|t| t.pending_destroy.push(child));
        crate::renderer::schedule_tick();
    }

    // -----------------------------------------------------------------------------------------------------------
    // Destruction
    // -----------------------------------------------------------------------------------------------------------

    /// `destroy()`.
    pub fn destroy(self) {
        if self.is_destroyed() {
            return;
        }
        let had_focus = self.with_base(|b| b.focused).unwrap_or(false) || self.has_focus_within();
        self.with_base_mut(|b| b.destroyed = true);
        self.update_animation_tick();
        self.emit("destroyed");
        if had_focus {
            request_render();
        }
        if let Some(parent) = self.parent() {
            parent.remove(self);
        }
        for child in self.children() {
            self.remove(child);
        }
        self.blur_internal(true);
        self.with_dyn_kind(|k| k.destroy(self));
        // Free the slot (and the Yoga node, now detached).
        with_tree(|t| {
            if let Some(slot) = t.slots.get_mut(self.index as usize).filter(|s| s.generation == self.generation) {
                if let Some(base) = slot.base.take() {
                    t.by_num.remove(&base.num);
                }
                slot.kind = None;
                t.free.push(self.index);
            }
            if t.focused == Some(self) {
                t.focused = None;
            }
        });
    }

    /// `destroyRecursively()`: children first.
    pub fn destroy_recursively(self) {
        if self.is_destroyed() {
            return;
        }
        for child in self.children() {
            child.destroy_recursively();
        }
        self.destroy();
    }

    // -----------------------------------------------------------------------------------------------------------
    // Focus
    // -----------------------------------------------------------------------------------------------------------

    pub fn set_focusable(self, focusable: bool) {
        let changed = self.with_base_mut(|b| {
            let changed = b.focusable != focusable;
            b.focusable = focusable;
            changed
        });
        if changed == Some(true) {
            if !focusable {
                self.blur();
            }
            request_render();
        }
    }

    pub fn focusable(self) -> bool {
        self.with_base(|b| b.focusable).unwrap_or(false)
    }

    pub fn focused(self) -> bool {
        self.with_base(|b| b.focused).unwrap_or(false)
    }

    /// `focus()`: blurs the focused node (renderer.focusRenderable), then takes keys and pastes.
    pub fn focus(self) {
        let ok = self.with_base(|b| !b.destroyed && !b.focused && b.focusable).unwrap_or(false);
        if !ok {
            return;
        }
        self.with_base_mut(|b| b.focused = true);
        // renderer.focusRenderable: blur the current one.
        let previous = with_tree(|t| t.focused);
        if let Some(previous) = previous.filter(|p| *p != self) {
            previous.blur();
        }
        with_tree(|t| t.focused = Some(self));
        request_render();
        if !self.focused() {
            return;
        }
        self.emit("focused");
        crate::renderer::focus_changed(Some(self));
    }

    /// `blur()`.
    pub fn blur(self) {
        self.blur_internal(false);
    }

    fn blur_internal(self, destroying: bool) {
        let was = self.with_base_mut(|b| std::mem::replace(&mut b.focused, false)).unwrap_or(false);
        if !was {
            return;
        }
        request_render();
        let cleared = with_tree(|t| {
            if t.focused == Some(self) {
                t.focused = None;
                true
            } else {
                false
            }
        });
        if !destroying {
            self.emit("blurred");
        }
        if cleared {
            crate::renderer::focus_changed(None);
        }
    }

    /// `hasFocusedDescendant`.
    pub fn has_focus_within(self) -> bool {
        let Some(focused) = with_tree(|t| t.focused) else { return false };
        let mut parent = focused.parent();
        while let Some(p) = parent {
            if p == self {
                return true;
            }
            parent = p.parent();
        }
        false
    }

    // -----------------------------------------------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------------------------------------------

    pub fn set_on_mouse(self, kind: Option<MouseEventType>, handler: Option<MouseHandler>) {
        self.with_base_mut(|b| match kind {
            None => b.handlers.mouse_any = handler,
            Some(kind) => b.handlers.mouse[kind.slot()] = handler,
        });
    }

    pub fn set_on_key_down(self, handler: Option<KeyHandler>) {
        self.with_base_mut(|b| b.handlers.key_down = handler);
    }

    pub fn set_on_paste(self, handler: Option<PasteHandler>) {
        self.with_base_mut(|b| b.handlers.paste = handler);
    }

    pub fn set_on_size_change(self, handler: Option<Rc<dyn Fn()>>) {
        self.with_base_mut(|b| b.handlers.size_change = handler);
    }

    pub fn set_render_before(self, hook: Option<PaintHook>) {
        self.with_base_mut(|b| b.handlers.render_before = hook);
        request_render();
    }

    pub fn set_render_after(self, hook: Option<PaintHook>) {
        self.with_base_mut(|b| b.handlers.render_after = hook);
        request_render();
    }

    /// `on(event, listener)`; returns a token for [`Element::off`].
    pub fn on(self, event: &'static str, listener: Rc<dyn Fn()>) -> u64 {
        with_tree(|t| {
            t.next_listener += 1;
            let token = t.next_listener;
            if let Some(b) = t.base_mut(self) {
                b.handlers.listeners.push((event, token, listener));
            }
            token
        })
    }

    pub fn off(self, token: u64) {
        self.with_base_mut(|b| b.handlers.listeners.retain(|(_, t, _)| *t != token));
    }

    /// `emit(event)` to the `on` listeners.
    pub fn emit(self, event: &str) {
        let listeners: Vec<Rc<dyn Fn()>> = self
            .with_base(|b| b.handlers.listeners.iter().filter(|(e, _, _)| *e == event).map(|(_, _, l)| l.clone()).collect())
            .unwrap_or_default();
        for listener in listeners {
            listener();
        }
    }

    /// `processMouseEvent(event)`: the node's handlers, its kind, then the parent unless propagation stopped.
    pub fn process_mouse_event(self, event: &mut MouseEvent) {
        let mut current = Some(self);
        while let Some(el) = current {
            event.current_target = Some(el);
            let (any, specific) = match el.with_base(|b| (b.handlers.mouse_any.clone(), b.handlers.mouse[event.kind.slot()].clone())) {
                Some(found) => found,
                None => return,
            };
            if let Some(handler) = any {
                handler(event);
            }
            if el.is_destroyed() {
                return;
            }
            if let Some(handler) = specific {
                handler(event);
            }
            if el.is_destroyed() {
                return;
            }
            el.with_dyn_kind(|k| k.on_mouse_event(el, event));
            if event.propagation_stopped {
                return;
            }
            current = el.parent();
        }
    }

    /// The focused node's key handling (the keypressHandler `focus()` registers): its onKeyDown, then
    /// handleKeyPress unless prevented.
    pub(crate) fn dispatch_key(self, key: &mut crate::input::KeyEvent) {
        let handler = self.with_base(|b| b.handlers.key_down.clone()).flatten();
        if let Some(handler) = handler {
            handler(key);
        }
        if self.is_destroyed() || !self.focused() {
            return;
        }
        if !key.default_prevented {
            self.with_dyn_kind(|k| {
                if k.handles_keys() {
                    k.handle_key_press(self, key);
                }
            });
        }
    }

    pub(crate) fn dispatch_paste(self, event: &mut crate::input::PasteEvent) {
        let handler = self.with_base(|b| b.handlers.paste.clone()).flatten();
        if let Some(handler) = handler {
            handler(event);
        }
        if self.is_destroyed() || !self.focused() {
            return;
        }
        if !event.default_prevented {
            self.with_dyn_kind(|k| {
                if k.handles_paste() {
                    k.handle_paste(self, event);
                }
            });
        }
    }

    /// `findDescendantById`.
    pub fn find_descendant_by_id(self, id: &str) -> Option<Element> {
        for child in self.children() {
            if child.id() == id {
                return Some(child);
            }
            if let Some(found) = child.find_descendant_by_id(id) {
                return Some(found);
            }
        }
        None
    }
}

/// Flex direction values (Yoga's).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlexDirection {
    Column = 0,
    ColumnReverse = 1,
    Row = 2,
    RowReverse = 3,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Auto = 0,
    FlexStart = 1,
    Center = 2,
    FlexEnd = 3,
    Stretch = 4,
    Baseline = 5,
    SpaceBetween = 6,
    SpaceAround = 7,
    SpaceEvenly = 8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Justify {
    FlexStart = 0,
    Center = 1,
    FlexEnd = 2,
    SpaceBetween = 3,
    SpaceAround = 4,
    SpaceEvenly = 5,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    Left = 0,
    Top = 1,
    Right = 2,
    Bottom = 3,
    Horizontal = 6,
    Vertical = 7,
    All = 8,
}

/// The node whose num is at a hit cell.
pub fn element_by_num(num: u32) -> Option<Element> {
    with_tree(|t| t.by_num.get(&num).copied())
}

pub fn root() -> Option<Element> {
    with_tree(|t| t.root)
}

pub fn focused() -> Option<Element> {
    with_tree(|t| t.focused)
}

/// Destroys the nodes removed this tick that were not attached again.
pub(crate) fn flush_pending_destroy() {
    let pending = with_tree(|t| std::mem::take(&mut t.pending_destroy));
    for el in pending {
        if el.exists() && el.parent().is_none() && Some(el) != root() {
            el.destroy_recursively();
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Frame: lifecycle passes, layout rounds, paint
// ---------------------------------------------------------------------------------------------------------------

/// Pre-order list of the visible nodes with a Yoga node.
fn visible_preorder(root: Element) -> Vec<Element> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(el) = stack.pop() {
        let Some((visible, text, children)) = el.with_base(|b| (b.visible, b.text_node, b.children.clone())) else {
            continue;
        };
        if !visible || text {
            continue;
        }
        out.push(el);
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    out
}

const MAX_LAYOUT_ROUNDS: usize = 8;

/// One frame's work before paint: lifecycle passes, update hooks, then layout rounds with resize feedback until
/// the layout settles (at most 8 rounds, as the scene's `maxLayoutRounds`).
pub(crate) fn prepare_frame(width: u32, height: u32, delta: f64) {
    let Some(root) = root() else { return };
    for el in visible_preorder(root) {
        let needs = el.with_dyn_kind(|k| k.needs_lifecycle_pass()).unwrap_or(false);
        if needs {
            el.with_dyn_kind(|k| k.lifecycle_pass(el));
        }
    }
    let frame = crate::renderer::frame_id() as i64;
    for el in visible_preorder(root) {
        let animated = el.with_base_mut(|b| {
            if b.animation_fps.is_none() {
                return false;
            }
            b.animation_drawn_frame = frame;
            b.animation_ticker.is_none()
        });
        if animated == Some(true) {
            el.update_animation_tick();
        }
        el.with_dyn_kind(|k| k.on_update(el, delta));
    }
    for _ in 0..MAX_LAYOUT_ROUNDS {
        // Measure functions read the tree: solve without holding it. The node stays put (no node is created or
        // freed while Yoga runs).
        let yoga = with_tree(|t| t.base(root).and_then(|b| b.yoga.as_ref()).map(|y| y as *const layout::Node));
        if let Some(yoga) = yoga {
            unsafe { (*yoga).calculate_layout(width as f32, height as f32) };
        }
        let mut resized = Vec::new();
        let mut resolvers = Vec::new();
        let order = visible_preorder(root);
        for &el in &order {
            let parent_screen = el.parent().map(|p| p.geometry()).map(|g| (g.screen_x, g.screen_y));
            let frame = el.parent().and_then(|p| {
                let g = p.geometry();
                p.with_dyn_kind(|k| k.child_frame(p, g.width, g.height)).flatten()
            });
            let computed = with_tree(|t| t.base(el).and_then(|b| b.yoga.as_ref()).map(|y| y.computed_layout()));
            let Some(computed) = computed else { continue };
            el.with_base_mut(|b| {
                let (px, py) = parent_screen.unwrap_or((0.0, 0.0));
                let (ox, oy) = frame.map(|f| (f.offset_x, f.offset_y)).unwrap_or((0.0, 0.0));
                b.geometry = Geometry {
                    left: computed.left,
                    top: computed.top,
                    width: computed.width,
                    height: computed.height,
                    screen_x: px + ox + computed.left as f64 + b.translate_x,
                    screen_y: py + oy + computed.top as f64 + b.translate_y,
                };
                if b.resized != Some((computed.width, computed.height)) {
                    b.resized = Some((computed.width, computed.height));
                    resized.push((el, computed.width, computed.height));
                }
            });
            if el.with_dyn_kind(|k| k.resolves_layout()).unwrap_or(false) {
                el.with_dyn_kind(|k| k.layout_resolved(el));
                resolvers.push(el);
            }
        }
        for (el, width, height) in resized {
            el.with_dyn_kind(|k| k.on_resize(el, width, height));
            let handler = el.with_base(|b| b.handlers.size_change.clone()).flatten();
            if let Some(handler) = handler {
                handler();
            }
            el.emit("resize");
        }
        for el in resolvers {
            if let Some(notify) = el.with_dyn_kind(|k| k.layout_notifications(el)).flatten() {
                notify();
            }
        }
        let dirty = with_tree(|t| t.base(root).and_then(|b| b.yoga.as_ref()).is_some_and(|y| y.is_dirty()));
        if !dirty {
            break;
        }
    }
}

/// A hit rectangle: cells that hit test to a node.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub num: u32,
}

#[derive(Clone, Copy)]
struct Clip {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Clip {
    fn intersect(self, other: Clip) -> Clip {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        Clip { x: x0, y: y0, w: (x1 - x0).max(0), h: (y1 - y0).max(0) }
    }
}

/// Paints the tree into `buffer` in paint order (pre-order, siblings by z-index then attachment) and returns the
/// hit rectangles in paint order.
pub(crate) fn paint(buffer: &mut OptimizedBuffer, delta: f64) -> Vec<Hit> {
    let mut hits = Vec::new();
    let Some(root) = root() else { return hits };
    let screen = Clip { x: 0, y: 0, w: buffer.width() as i32, h: buffer.height() as i32 };
    paint_node(root, buffer, screen, 1.0, delta, &mut hits);
    buffer.clear_scissor_rects();
    buffer.clear_opacity();
    hits
}

fn enter(buffer: &mut OptimizedBuffer, clip: Clip, opacity: f32) {
    buffer.clear_scissor_rects();
    buffer.clear_opacity();
    buffer.push_scissor_rect(clip.x, clip.y, clip.w, clip.h);
    buffer.push_opacity(opacity);
}

fn paint_node(el: Element, buffer: &mut OptimizedBuffer, clip: Clip, opacity: f32, delta: f64, hits: &mut Vec<Hit>) {
    let Some((visible, text, geometry, node_opacity, overflow, border, num, before, after, children)) = el.with_base(|b| {
        (
            b.visible,
            b.text_node,
            b.geometry,
            b.opacity,
            b.overflow,
            b.border_mask,
            b.num,
            b.handlers.render_before.clone(),
            b.handlers.render_after.clone(),
            b.children.clone(),
        )
    }) else {
        return;
    };
    if !visible || text {
        return;
    }
    let opacity = opacity * node_opacity;
    let x = geometry.screen_x.floor() as i32;
    let y = geometry.screen_y.floor() as i32;
    let w = geometry.width.max(0.0) as u32;
    let h = geometry.height.max(0.0) as u32;
    let rect = Clip { x, y, w: w as i32, h: h as i32 };
    let painted = rect.intersect(clip);
    if w > 0 && h > 0 {
        enter(buffer, clip, opacity);
        if let Some(hook) = before {
            hook(el, buffer, delta);
            enter(buffer, clip, opacity);
        }
        el.with_dyn_kind(|k| {
            if k.paints() {
                k.render(el, buffer, x, y, w, h)
            }
        });
        if let Some(hook) = after {
            enter(buffer, clip, opacity);
            hook(el, buffer, delta);
        }
    }
    if painted.w > 0 && painted.h > 0 && Some(el) != root() {
        hits.push(Hit { x: painted.x, y: painted.y, width: painted.w as u32, height: painted.h as u32, num });
    }
    if children.is_empty() && !el.with_dyn_kind(|k| k.has_after_children()).unwrap_or(false) {
        return;
    }
    let frame = el.with_dyn_kind(|k| k.child_frame(el, geometry.width, geometry.height)).flatten();
    let child_clip = if let Some(frame) = frame {
        clip.intersect(Clip { x: x + frame.clip.0, y: y + frame.clip.1, w: frame.clip.2, h: frame.clip.3 })
    } else if overflow != Overflow::Visible {
        let left = (border & 1 != 0) as i32;
        let top = (border & 2 != 0) as i32;
        let right = (border & 4 != 0) as i32;
        let bottom = (border & 8 != 0) as i32;
        clip.intersect(Clip { x: x + left, y: y + top, w: (w as i32 - left - right).max(0), h: (h as i32 - top - bottom).max(0) })
    } else {
        clip
    };
    // Siblings paint by z-index, equal ones in attachment order.
    let mut ordered: Vec<(i32, u64, Element)> =
        children.iter().filter_map(|c| c.with_base(|b| (b.z_index, b.attach_order, *c))).collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    for (_, _, child) in ordered {
        paint_node(child, buffer, child_clip, opacity, delta, hits);
    }
    if el.with_dyn_kind(|k| k.has_after_children()).unwrap_or(false) {
        enter(buffer, clip, opacity);
        el.with_dyn_kind(|k| k.render_after_children(el, buffer, x, y, w, h));
        for (hx, hy, hw, hh) in el.with_dyn_kind(|k| k.hits_after_children(el, x, y, w, h)).unwrap_or_default() {
            let hit = Clip { x: hx, y: hy, w: hw as i32, h: hh as i32 }.intersect(clip);
            if hit.w > 0 && hit.h > 0 {
                hits.push(Hit { x: hit.x, y: hit.y, width: hit.w as u32, height: hit.h as u32, num });
            }
        }
    }
}

/// The root (`RootRenderable`): a node sized to the terminal.
pub struct RootKind;

impl Kind for RootKind {
    fn name(&self) -> &'static str {
        "root"
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

/// Creates the root node (`__root__`) at the terminal size.
pub(crate) fn create_root(width: u32, height: u32) -> Element {
    let root = create(Box::new(RootKind), "__root__", false);
    root.set_id("__root__");
    root.set_width(Some(Value::Point(width as f32)));
    root.set_height(Some(Value::Point(height as f32)));
    with_tree(|t| t.root = Some(root));
    root
}

/// Test support: a root without a renderer.
#[doc(hidden)]
pub fn create_root_for_tests(width: u32, height: u32) -> Element {
    create_root(width, height)
}

/// Test support: paints the tree into `buffer` (a frame's paint without a renderer).
#[doc(hidden)]
pub fn paint_for_tests(buffer: &mut OptimizedBuffer) {
    paint(buffer, 0.0);
}

/// Test support: one frame's layout without a renderer.
#[doc(hidden)]
pub fn prepare_frame_for_tests(width: u32, height: u32) {
    prepare_frame(width, height, 0.0)
}

/// `RootRenderable.resize`.
pub(crate) fn resize_root(width: u32, height: u32) {
    if let Some(root) = root() {
        root.set_width(Some(Value::Point(width as f32)));
        root.set_height(Some(Value::Point(height as f32)));
    }
}
