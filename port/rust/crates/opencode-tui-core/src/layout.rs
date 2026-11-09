//! Yoga 3.2.1 through OpenTUI's bridge (`native/src/yoga-bridge.cpp`), configured as `context.zig` configures the
//! scene's Yoga config: web defaults off, point scale factor 1, positions rounded from absolute edges.
//!
//! TODO(port-tui): opencode-yoga. A stand-in compiled by build.rs until the input slice's crate lands; the
//! renderables call only this module, so the swap stays here.
use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;

type NodeRef = *mut c_void;
type ConfigRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct ComputedLayout {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub width: f32,
    pub height: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct YGSize {
    width: f32,
    height: f32,
}

type MeasureCallback = extern "C" fn(NodeRef, f32, i32, f32, i32) -> YGSize;

unsafe extern "C" {
    fn otYogaConfigCreate(out: *mut ConfigRef) -> u32;
    fn otYogaConfigSetRoundsPositionsFromAbsoluteEdges(config: ConfigRef, enabled: u32);
    fn YGConfigSetUseWebDefaults(config: ConfigRef, enabled: bool);
    fn YGConfigSetPointScaleFactor(config: ConfigRef, factor: f32);
    fn otYogaNodeCreate(config: ConfigRef, out: *mut NodeRef) -> u32;
    fn otYogaNodeFree(node: NodeRef) -> u32;
    fn otYogaNodeInsertChild(node: NodeRef, child: NodeRef, index: u32) -> u32;
    fn otYogaNodeRemoveChild(node: NodeRef, child: NodeRef) -> u32;
    fn otYogaNodeCalculateLayout(node: NodeRef, width: f32, height: f32, direction: u32) -> u32;
    fn otYogaNodeMarkDirty(node: NodeRef) -> u32;
    fn otYogaNodeSetMeasureFunc(node: NodeRef, callback: Option<MeasureCallback>, height_cap_only_clamps: u32) -> u32;
    fn otYogaNodeGetComputedLayout(node: NodeRef, out: *mut ComputedLayout) -> u32;
    fn otYogaNodeLayoutGetEdge(node: NodeRef, kind: u32, edge: u32, out: *mut f32) -> u32;
    fn otYogaNodeStyleSetEnum(node: NodeRef, kind: u32, value: u32) -> u32;
    fn otYogaNodeStyleGetEnum(node: NodeRef, kind: u32, out: *mut u32) -> u32;
    fn otYogaNodeStyleSetFloat(node: NodeRef, kind: u32, value: f32) -> u32;
    fn otYogaNodeStyleSetBorder(node: NodeRef, edge: u32, value: f32) -> u32;
    fn otYogaNodeStyleSetValue(node: NodeRef, kind: u32, edge: u32, unit: u32, value: f32) -> u32;
    fn otYogaNodeStyleSetDimension(node: NodeRef, kind: u32, unit: u32, value: f32, disable_flex_shrink: u32) -> u32;
    fn otYogaNodeStyleSetPositions(node: NodeRef, edge_mask: u32, units: *const u32, values: *const f32) -> u32;
    fn YGNodeSetContext(node: NodeRef, context: *mut c_void);
    fn YGNodeGetContext(node: NodeRef) -> *mut c_void;
    fn YGNodeGetChildCount(node: NodeRef) -> usize;
    fn YGNodeIsDirty(node: NodeRef) -> bool;
}

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

/// `YGMeasureMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeasureMode {
    Undefined,
    Exactly,
    AtMost,
}

pub type MeasureFn = Box<dyn FnMut(f32, MeasureMode, f32, MeasureMode) -> (f32, f32)>;

struct Config(ConfigRef);

thread_local! {
    static CONFIG: RefCell<Option<Config>> = const { RefCell::new(None) };
}

fn config() -> ConfigRef {
    CONFIG.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            let mut config: ConfigRef = std::ptr::null_mut();
            unsafe {
                otYogaConfigCreate(&mut config);
                YGConfigSetUseWebDefaults(config, false);
                YGConfigSetPointScaleFactor(config, 1.0);
                otYogaConfigSetRoundsPositionsFromAbsoluteEdges(config, 1);
            }
            *cell = Some(Config(config));
        }
        cell.as_ref().unwrap().0
    })
}

/// One Yoga node, freed on drop (after being detached from its parent and children by the tree).
pub struct Node {
    raw: NonNull<c_void>,
}

extern "C" fn measure_trampoline(node: NodeRef, width: f32, wm: i32, height: f32, hm: i32) -> YGSize {
    let mode = |m: i32| match m {
        1 => MeasureMode::Exactly,
        2 => MeasureMode::AtMost,
        _ => MeasureMode::Undefined,
    };
    unsafe {
        let context = YGNodeGetContext(node) as *mut MeasureFn;
        if context.is_null() {
            return YGSize { width: 0.0, height: 0.0 };
        }
        let (w, h) = (*context)(width, mode(wm), height, mode(hm));
        YGSize { width: w, height: h }
    }
}

impl Node {
    pub fn new() -> Node {
        let mut raw: NodeRef = std::ptr::null_mut();
        unsafe { otYogaNodeCreate(config(), &mut raw) };
        Node { raw: NonNull::new(raw).expect("Yoga node allocation failed") }
    }

    fn ptr(&self) -> NodeRef {
        self.raw.as_ptr()
    }

    pub fn set_enum(&self, kind: u32, value: u32) {
        unsafe { otYogaNodeStyleSetEnum(self.ptr(), kind, value) };
    }

    pub fn get_enum(&self, kind: u32) -> u32 {
        let mut out = 0;
        unsafe { otYogaNodeStyleGetEnum(self.ptr(), kind, &mut out) };
        out
    }

    pub fn set_float(&self, kind: u32, value: f32) {
        unsafe { otYogaNodeStyleSetFloat(self.ptr(), kind, value) };
    }

    pub fn set_value(&self, kind: u32, edge: u32, value: Value) {
        let (unit, v) = value.unit();
        unsafe { otYogaNodeStyleSetValue(self.ptr(), kind, edge, unit, v) };
    }

    pub fn set_dimension(&self, kind: u32, value: Value, disable_flex_shrink: bool) {
        let (unit, v) = value.unit();
        unsafe { otYogaNodeStyleSetDimension(self.ptr(), kind, unit, v, disable_flex_shrink as u32) };
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
        unsafe { otYogaNodeStyleSetPositions(self.ptr(), mask, units.as_ptr(), floats.as_ptr()) };
    }

    pub fn set_border(&self, edge: u32, value: f32) {
        unsafe { otYogaNodeStyleSetBorder(self.ptr(), edge, value) };
    }

    pub fn insert_child(&self, child: &Node, index: u32) {
        unsafe { otYogaNodeInsertChild(self.ptr(), child.ptr(), index) };
    }

    pub fn remove_child(&self, child: &Node) {
        unsafe { otYogaNodeRemoveChild(self.ptr(), child.ptr()) };
    }

    pub fn child_count(&self) -> usize {
        unsafe { YGNodeGetChildCount(self.ptr()) }
    }

    pub fn calculate_layout(&self, width: f32, height: f32) {
        unsafe { otYogaNodeCalculateLayout(self.ptr(), width, height, 1) };
    }

    pub fn computed_layout(&self) -> ComputedLayout {
        let mut out = ComputedLayout::default();
        unsafe { otYogaNodeGetComputedLayout(self.ptr(), &mut out) };
        out
    }

    /// `kind`: 0 margin, 1 padding, 2 border.
    pub fn layout_edge(&self, kind: u32, edge: u32) -> f32 {
        let mut out = 0.0;
        unsafe { otYogaNodeLayoutGetEdge(self.ptr(), kind, edge, &mut out) };
        out
    }

    pub fn is_dirty(&self) -> bool {
        unsafe { YGNodeIsDirty(self.ptr()) }
    }

    pub fn mark_dirty(&self) {
        unsafe { otYogaNodeMarkDirty(self.ptr()) };
    }

    pub fn set_measure_func(&self, measure: Option<MeasureFn>, height_cap_only_clamps: bool) {
        unsafe {
            let old = YGNodeGetContext(self.ptr()) as *mut MeasureFn;
            if !old.is_null() {
                drop(Box::from_raw(old));
            }
            match measure {
                Some(f) => {
                    YGNodeSetContext(self.ptr(), Box::into_raw(Box::new(f)) as *mut c_void);
                    otYogaNodeSetMeasureFunc(self.ptr(), Some(measure_trampoline), height_cap_only_clamps as u32);
                }
                None => {
                    YGNodeSetContext(self.ptr(), std::ptr::null_mut());
                    otYogaNodeSetMeasureFunc(self.ptr(), None, 0);
                }
            }
        }
    }

    pub fn has_measure_func(&self) -> bool {
        unsafe { !YGNodeGetContext(self.ptr()).is_null() }
    }
}

impl Default for Node {
    fn default() -> Self {
        Node::new()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        unsafe {
            let old = YGNodeGetContext(self.ptr()) as *mut MeasureFn;
            if !old.is_null() {
                drop(Box::from_raw(old));
            }
            otYogaNodeFree(self.ptr());
        }
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
        measured.set_measure_func(Some(Box::new(|_, _, _, _| (7.0, 2.0))), false);
        a.insert_child(&measured, 1);
        root.calculate_layout(80.0, 24.0);
        assert_eq!(measured.computed_layout().width, 7.0);
        assert_eq!(measured.computed_layout().top, 3.0);
        a.remove_child(&measured);
        a.remove_child(&b);
        root.remove_child(&a);
    }
}
