//! Yoga for the renderables: `opencode-yoga` (Yoga 3.2.1 with OpenTUI's patches and bridge) configured as
//! `context.zig` configures the scene's config (upstream defaults, positions rounded from absolute edges, the log
//! routed away from the terminal), with the yoga.zig calls `scene.zig` makes. Errors of the checked calls are
//! dropped: the scene treats a failed style write as no change, and a failed solve leaves the last layout.
use std::cell::RefCell;

pub use opencode_yoga::MeasureMode;
use opencode_yoga::{Config, Layout, Size};

pub type ComputedLayout = Layout;

// OT_STYLE_ENUM_*
pub const ENUM_DIRECTION: u32 = 0;
pub const ENUM_FLEX_DIRECTION: u32 = 1;
pub const ENUM_JUSTIFY_CONTENT: u32 = 2;
pub const ENUM_ALIGN_CONTENT: u32 = 3;
pub const ENUM_ALIGN_ITEMS: u32 = 4;
pub const ENUM_ALIGN_SELF: u32 = 5;
pub const ENUM_POSITION_TYPE: u32 = 6;
pub const ENUM_FLEX_WRAP: u32 = 7;
pub const ENUM_OVERFLOW: u32 = 8;
pub const ENUM_DISPLAY: u32 = 9;
// OT_STYLE_FLOAT_*
pub const FLOAT_FLEX_GROW: u32 = 1;
pub const FLOAT_FLEX_SHRINK: u32 = 2;
// OT_STYLE_VALUE_*
pub const VALUE_WIDTH: u32 = 0;
pub const VALUE_HEIGHT: u32 = 1;
pub const VALUE_MIN_WIDTH: u32 = 2;
pub const VALUE_MIN_HEIGHT: u32 = 3;
pub const VALUE_MAX_WIDTH: u32 = 4;
pub const VALUE_MAX_HEIGHT: u32 = 5;
pub const VALUE_FLEX_BASIS: u32 = 6;
pub const VALUE_MARGIN: u32 = 7;
pub const VALUE_PADDING: u32 = 8;
pub const VALUE_GAP: u32 = 10;

pub const EDGE_LEFT: u32 = 0;
pub const EDGE_TOP: u32 = 1;
pub const EDGE_RIGHT: u32 = 2;
pub const EDGE_BOTTOM: u32 = 3;
pub const EDGE_HORIZONTAL: u32 = 6;
pub const EDGE_VERTICAL: u32 = 7;
pub const EDGE_ALL: u32 = 8;
pub const GUTTER_COLUMN: u32 = 0;
pub const GUTTER_ROW: u32 = 1;
pub const GUTTER_ALL: u32 = 2;

pub const DIMENSION_WIDTH: u32 = 0;
pub const DIMENSION_HEIGHT: u32 = 1;

/// A Yoga length: `number`, `"auto"`, `"<n>%"` or unset, as `parseYogaValue` reads them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Undefined,
    Point(f32),
    Percent(f32),
    Auto,
}

impl Value {
    fn unit(self) -> (u32, f32) {
        match self {
            Value::Undefined => (0, f32::NAN),
            Value::Point(v) => (1, v),
            Value::Percent(v) => (2, v),
            Value::Auto => (3, f32::NAN),
        }
    }
}

impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Point(v)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Point(v as f32)
    }
}

pub type MeasureFn = Box<dyn FnMut(f32, MeasureMode, f32, MeasureMode) -> (f32, f32)>;

thread_local! {
    static CONFIG: RefCell<Option<Config>> = const { RefCell::new(None) };
}

fn config() -> Config {
    CONFIG.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            let config = Config::create().expect("Yoga config");
            let _ = config.set_rounds_positions_from_absolute_edges(true);
            // context.zig routes Yoga's log to its diagnostics, never to the terminal.
            let _ = config.set_logger(Some(Box::new(|_, _| {})));
            *cell = Some(config);
        }
        cell.as_ref().unwrap().clone()
    })
}

/// One scene Yoga node.
pub struct Node {
    inner: opencode_yoga::Node,
}

impl Node {
    pub fn new() -> Node {
        let config = config();
        Node { inner: opencode_yoga::Node::create(Some(&config)).expect("Yoga node") }
    }

    pub fn set_enum(&self, kind: u32, value: u32) {
        let _ = self.inner.style_set_enum(kind, value);
    }

    pub fn get_enum(&self, kind: u32) -> u32 {
        self.inner.style_get_enum(kind).unwrap_or(0)
    }

    pub fn set_float(&self, kind: u32, value: f32) {
        let _ = self.inner.style_set_float(kind, value);
    }

    pub fn set_value(&self, kind: u32, edge: u32, value: Value) {
        let (unit, v) = value.unit();
        let _ = self.inner.style_set_value(kind, edge, unit, v);
    }

    pub fn set_dimension(&self, kind: u32, value: Value, disable_flex_shrink: bool) {
        let (unit, v) = value.unit();
        let _ = self.inner.style_set_dimension(kind, unit, v, disable_flex_shrink as u32);
    }

    /// Edges in mask order left, top, right, bottom.
    pub fn set_positions(&self, mask: u32, values: [Value; 4]) {
        let mut units = [0u32; 4];
        let mut floats = [0f32; 4];
        for (i, value) in values.iter().enumerate() {
            let (u, v) = value.unit();
            units[i] = u;
            floats[i] = if v.is_nan() { 0.0 } else { v };
        }
        let _ = self.inner.style_set_positions(mask, units, floats);
    }

    pub fn set_border(&self, edge: u32, value: f32) {
        let _ = self.inner.style_set_border(edge, value);
    }

    pub fn insert_child(&self, child: &Node, index: u32) {
        let _ = self.inner.insert_child(&child.inner, index);
    }

    pub fn remove_child(&self, child: &Node) {
        let _ = self.inner.remove_child(&child.inner);
    }

    pub fn child_count(&self) -> usize {
        self.inner.get_child_count() as usize
    }

    pub fn calculate_layout(&self, width: f32, height: f32) {
        let _ = self.inner.calculate_layout_checked(width, height, 1);
    }

    pub fn computed_layout(&self) -> ComputedLayout {
        self.inner.get_computed_layout().unwrap_or_default()
    }

    /// `kind`: 0 margin, 1 padding, 2 border.
    pub fn layout_edge(&self, kind: u32, edge: u32) -> f32 {
        self.inner.layout_get_edge(kind, edge).unwrap_or(0.0)
    }

    pub fn is_dirty(&self) -> bool {
        self.inner.is_dirty().unwrap_or(false)
    }

    /// yoga.zig `yogaNodeInvalidateMeasure`: the node's measured content changed.
    pub fn mark_dirty(&self) {
        let _ = self.inner.invalidate_measure();
    }

    /// A native measure function, as the scene sets one for text and editor views.
    pub fn set_measure_func(&self, measure: Option<MeasureFn>, height_cap_only_clamps: bool) {
        let measure = measure.map(|mut f| {
            Box::new(move |w: f32, wm: MeasureMode, h: f32, hm: MeasureMode| {
                let (width, height) = f(w, wm, h, hm);
                Size { width, height }
            }) as opencode_yoga::MeasureFunction
        });
        let _ = self.inner.set_native_measure_func(measure, height_cap_only_clamps);
    }

    pub fn has_measure_func(&self) -> bool {
        self.inner.has_measure_func()
    }
}

impl Default for Node {
    fn default() -> Self {
        Node::new()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.inner.free();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_with_grow_and_centered_child() {
        let root = Node::new();
        root.set_dimension(DIMENSION_WIDTH, Value::Point(80.0), false);
        root.set_dimension(DIMENSION_HEIGHT, Value::Point(24.0), false);
        let a = Node::new();
        a.set_float(FLOAT_FLEX_GROW, 1.0);
        a.set_enum(ENUM_ALIGN_ITEMS, 2);
        let b = Node::new();
        b.set_dimension(DIMENSION_WIDTH, Value::Point(10.0), false);
        b.set_dimension(DIMENSION_HEIGHT, Value::Point(3.0), false);
        a.insert_child(&b, 0);
        root.insert_child(&a, 0);
        root.calculate_layout(80.0, 24.0);
        assert_eq!(a.computed_layout().height, 24.0);
        assert_eq!(b.computed_layout().left, 35.0);
        let measured = Node::new();
        measured.set_measure_func(Some(Box::new(|_, _, _, _| (7.0, 2.0))), true);
        a.insert_child(&measured, 1);
        root.calculate_layout(80.0, 24.0);
        assert_eq!(measured.computed_layout().width, 7.0);
        assert_eq!(measured.computed_layout().top, 3.0);
        a.remove_child(&measured);
        a.remove_child(&b);
        root.remove_child(&a);
    }
}
