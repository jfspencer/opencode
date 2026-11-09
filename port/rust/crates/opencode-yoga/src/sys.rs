//! The C surface the crate is built on: Yoga 3.2.1's C API (`yoga/YG*.h`) and OpenTUI's bridge
//! (`packages/native/src/yoga-bridge.h`), both compiled by build.rs. Every bridge function returns an
//! `OT_YOGA_*` status and validates its arguments before entering Yoga.
#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct YGNode {
    _private: [u8; 0],
}
#[repr(C)]
pub struct YGConfig {
    _private: [u8; 0],
}
pub type YGNodeRef = *mut YGNode;
pub type YGNodeConstRef = *const YGNode;
pub type YGConfigRef = *mut YGConfig;
pub type YGConfigConstRef = *const YGConfig;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct YGSize {
    pub width: f32,
    pub height: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct OTYogaLayout {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub width: f32,
    pub height: f32,
}

pub type YGMeasureFunc = Option<unsafe extern "C" fn(YGNodeConstRef, f32, c_int, f32, c_int) -> YGSize>;
pub type YGDirtiedFunc = Option<unsafe extern "C" fn(YGNodeConstRef)>;
/// `YGLogger`; its `va_list` is only passed through (to src/logger.cpp's formatter), never read in Rust.
pub type YGLogger = Option<unsafe extern "C" fn()>;

pub const OT_YOGA_DEPTH_MAX: u32 = 256;
pub const YG_DIRECTION_RTL: u32 = 2;
pub const YG_EXPERIMENTAL_FEATURE_WEB_FLEX_BASIS: u32 = 0;
pub const YG_ERRATA_ALL: u32 = 2147483647;
pub const YG_ERRATA_CLASSIC: u32 = 2147483646;
pub const YG_NODE_TYPE_DEFAULT: c_int = 0;

unsafe extern "C" {
    // yoga/YGConfig.h
    pub fn YGConfigFree(config: YGConfigRef);
    pub fn YGConfigSetUseWebDefaults(config: YGConfigRef, enabled: bool);
    pub fn YGConfigGetUseWebDefaults(config: YGConfigConstRef) -> bool;
    pub fn YGConfigSetPointScaleFactor(config: YGConfigRef, pixels_in_point: f32);
    pub fn YGConfigGetPointScaleFactor(config: YGConfigConstRef) -> f32;
    pub fn YGConfigSetErrata(config: YGConfigRef, errata: c_int);
    pub fn YGConfigGetErrata(config: YGConfigConstRef) -> c_int;
    pub fn YGConfigSetExperimentalFeatureEnabled(config: YGConfigRef, feature: c_int, enabled: bool);
    pub fn YGConfigIsExperimentalFeatureEnabled(config: YGConfigConstRef, feature: c_int) -> bool;
    pub fn YGConfigSetContext(config: YGConfigRef, context: *mut c_void);
    pub fn YGConfigGetContext(config: YGConfigConstRef) -> *mut c_void;
    pub fn YGConfigSetLogger(config: YGConfigRef, logger: YGLogger);

    // yoga/YGNode.h
    pub fn YGNodeGetHasNewLayout(node: YGNodeConstRef) -> bool;
    pub fn YGNodeIsDirty(node: YGNodeConstRef) -> bool;
    pub fn YGNodeSetDirtiedFunc(node: YGNodeRef, dirtied: YGDirtiedFunc);
    pub fn YGNodeGetChild(node: YGNodeRef, index: usize) -> YGNodeRef;
    pub fn YGNodeGetChildCount(node: YGNodeConstRef) -> usize;
    pub fn YGNodeGetOwner(node: YGNodeRef) -> YGNodeRef;
    pub fn YGNodeGetParent(node: YGNodeRef) -> YGNodeRef;
    pub fn YGNodeGetConfig(node: YGNodeRef) -> YGConfigConstRef;
    pub fn YGNodeSetContext(node: YGNodeRef, context: *mut c_void);
    pub fn YGNodeGetContext(node: YGNodeConstRef) -> *mut c_void;
    pub fn YGNodeHasMeasureFunc(node: YGNodeConstRef) -> bool;
    pub fn YGNodeIsReferenceBaseline(node: YGNodeConstRef) -> bool;
    pub fn YGNodeSetNodeType(node: YGNodeRef, node_type: c_int);
    pub fn YGNodeGetNodeType(node: YGNodeConstRef) -> c_int;
    pub fn YGNodeGetAlwaysFormsContainingBlock(node: YGNodeConstRef) -> bool;

    // packages/native/src/yoga-bridge.h
    pub fn otYogaConfigCreate(out: *mut YGConfigRef) -> u32;
    pub fn otYogaConfigSetRoundsPositionsFromAbsoluteEdges(config: YGConfigRef, enabled: u32);
    pub fn otYogaNodeCreate(config: YGConfigConstRef, out: *mut YGNodeRef) -> u32;
    pub fn otYogaNodeFree(node: YGNodeRef) -> u32;
    pub fn otYogaNodeReset(node: YGNodeRef) -> u32;
    pub fn otYogaNodeStorageBytes(node: YGNodeConstRef) -> u64;
    pub fn otYogaNodeCopyStyle(node: YGNodeRef, source: YGNodeConstRef) -> u32;
    pub fn otYogaNodeInsertChild(node: YGNodeRef, child: YGNodeRef, index: u32) -> u32;
    pub fn otYogaNodeMoveChild(parent: YGNodeRef, child: YGNodeRef, final_index: u32) -> u32;
    pub fn otYogaNodeRemoveChild(node: YGNodeRef, child: YGNodeRef) -> u32;
    pub fn otYogaNodeRemoveAllChildren(node: YGNodeRef) -> u32;
    pub fn otYogaNodeCalculateLayout(node: YGNodeRef, width: f32, height: f32, direction: u32) -> u32;
    pub fn otYogaNodeMarkDirty(node: YGNodeRef) -> u32;
    pub fn otYogaNodeSetMeasureFunc(node: YGNodeRef, callback: YGMeasureFunc, height_cap_only_clamps: u32) -> u32;
    pub fn otYogaNodeInvalidateMeasure(node: YGNodeRef);
    pub fn otYogaNodeForgetMeasurements(node: YGNodeRef);
    pub fn otYogaNodeGetComputedLayout(node: YGNodeConstRef, out: *mut OTYogaLayout) -> u32;
    pub fn otYogaNodeLayoutGetEdge(node: YGNodeConstRef, kind: u32, edge: u32, out: *mut f32) -> u32;
    pub fn otYogaNodeStyleSetEnum(node: YGNodeRef, kind: u32, value: u32) -> u32;
    pub fn otYogaNodeStyleGetEnum(node: YGNodeConstRef, kind: u32, out: *mut u32) -> u32;
    pub fn otYogaNodeStyleSetFloat(node: YGNodeRef, kind: u32, value: f32) -> u32;
    pub fn otYogaNodeStyleGetFloat(node: YGNodeConstRef, kind: u32, out: *mut f32) -> u32;
    pub fn otYogaNodeStyleSetBorder(node: YGNodeRef, edge: u32, value: f32) -> u32;
    pub fn otYogaNodeStyleGetBorder(node: YGNodeConstRef, edge: u32, out: *mut f32) -> u32;
    pub fn otYogaNodeStyleSetValue(node: YGNodeRef, kind: u32, edge: u32, unit: u32, value: f32) -> u32;
    pub fn otYogaNodeStyleSetDimension(
        node: YGNodeRef,
        kind: u32,
        unit: u32,
        value: f32,
        disable_flex_shrink: u32,
    ) -> u32;
    pub fn otYogaNodeStyleSetPositions(
        node: YGNodeRef,
        edge_mask: u32,
        units: *const [u32; 4],
        values: *const [f32; 4],
    ) -> u32;
    pub fn otYogaNodeStyleGetValue(node: YGNodeConstRef, kind: u32, edge: u32, out: *mut u64) -> u32;
    pub fn otYogaNodeSetFlag(node: YGNodeRef, kind: u32, value: u32) -> u32;

    // src/logger.cpp
    pub fn opencodeYogaLogger();
}
