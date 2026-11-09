//! OpenTUI 0.8.1's layout engine: Yoga 3.2.1 as OpenTUI patches and builds it, behind a safe port of its two
//! wrappers.
//!
//! - build.rs compiles `packages/native/zig-deps/yoga` and `src/yoga-bridge.cpp` exactly as
//!   `packages/native/build.zig` does (see there). The bridge (`otYoga*`) validates arguments and turns C++
//!   exceptions into statuses; `sys` declares it and the Yoga C API.
//! - [`Node`] and [`Config`] port `packages/native/src/yoga.zig` (the checked `yoga*Checked` exports and the
//!   scene-only operations: `move_child`, `style_set_dimension`, `style_set_positions`, native measure functions,
//!   `invalidate_measure`, `forget_measurements`) and `packages/core/src/yoga.ts` (the public `Node`/`Config`
//!   classes: typed setters taking [`ValueInput`], defaults on a freed node, `YogaError`s). Methods named after
//!   yoga.zig take raw `u32` kinds, edges and units as the native ABI does and are what a port of `scene.zig`
//!   calls; the yoga.ts-named ones take the typed enums.
//! - [`options`] ports `lib/yoga.options.ts`, [`ValueInput::parse`] `parseYogaValue`.
//!
//! Layout configs as OpenTUI makes them: [`Config::create`] is yoga.zig's `Config.init` (no web defaults, point
//! scale factor 1). The renderer's context config (`context.zig`) also calls
//! [`Config::set_rounds_positions_from_absolute_edges`]; scene nodes then get flex shrink 1 and zero borders.
//!
//! # Divergences
//!
//! - Ownership. A [`Node`] is a reference-counted handle (cloning it clones the handle, as a JS reference would).
//!   The Yoga node is freed by [`Node::free`] or when the last handle goes; a node with a parent is kept alive by
//!   that parent, so dropping a root handle frees its tree, and children someone still holds become roots. A
//!   [`Config`] lives while any of its nodes does, so there is no `Config.free`.
//! - Callbacks are Rust closures. As in TS (`YogaHost`), a Yoga mutation made inside a measure, dirtied or log
//!   callback fails with [`Error::InCallback`], a panicking callback reports NaN to Yoga and the panic resumes
//!   when the outermost Yoga call returns (TS rethrows the callback's error there), and a node dropped inside a
//!   callback is freed after that call.
//! - yoga.zig's `busy` status (a mutation of a tree whose layout is running) and its config locks: a running
//!   layout can only be reached from one of its callbacks, where every mutation already fails with
//!   [`Error::InCallback`]; the crate is single-threaded (`!Send`).
//! - Native measure failures (`reportMeasureError`): a Rust measure closure cannot fail; it returns its size.
//!
//! # Licences
//!
//! Yoga is MIT-licensed, Copyright (c) Meta Platforms, Inc. and affiliates
//! (`packages/native/zig-deps/yoga/LICENSE`); OpenTUI, including its Yoga patches, `yoga-bridge.cpp` and the
//! wrappers ported here, is MIT-licensed, Copyright (c) 2025 opentui. See README.md.

mod options;
mod sys;
mod value;

pub use options::*;
pub use value::ValueInput;

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::ffi::{c_int, c_void};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::ptr;
use std::rc::Rc;

// ---------------------------------------------------------------------------------------------------------------
// Statuses and errors

/// The bridge's `OT_YOGA_*` statuses (`YogaStatus` in yoga.ts).
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Ok = 0,
    InvalidArgument = 1,
    OutOfMemory = 2,
    Exception = 3,
    Poisoned = 4,
    Busy = 5,
    DepthLimit = 6,
}

impl Status {
    fn from_raw(raw: u32) -> Status {
        match raw {
            0 => Status::Ok,
            1 => Status::InvalidArgument,
            2 => Status::OutOfMemory,
            3 => Status::Exception,
            4 => Status::Poisoned,
            5 => Status::Busy,
            6 => Status::DepthLimit,
            _ => Status::Exception,
        }
    }

    /// The `YogaStatus` member name.
    pub fn name(self) -> &'static str {
        match self {
            Status::Ok => "Ok",
            Status::InvalidArgument => "InvalidArgument",
            Status::OutOfMemory => "OutOfMemory",
            Status::Exception => "Exception",
            Status::Poisoned => "Poisoned",
            Status::Busy => "Busy",
            Status::DepthLimit => "DepthLimit",
        }
    }
}

/// What a Yoga operation can fail with; `Display` is the TS error message.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// yoga.ts `YogaError`: `operation` is the native export's name.
    Yoga { operation: &'static str, status: Status },
    /// `YogaHost.assertMutable`: a mutation inside a Yoga callback.
    InCallback,
    /// `assertSameLibrary`: a freed node passed as the other node of an operation.
    NodeFreed,
    /// `parseYogaValue`'s errors.
    InvalidValue(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Yoga { operation, status } => {
                write!(f, "{operation} failed: {} (status {})", status.name(), *status as u32)
            }
            Error::InCallback => f.write_str("Cannot mutate Yoga during a callback"),
            Error::NodeFreed => f.write_str("Yoga node is freed"),
            Error::InvalidValue(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn failure(operation: &'static str, status: Status) -> Error {
    Error::Yoga { operation, status }
}

fn check(operation: &'static str, raw: u32) -> Result<()> {
    match Status::from_raw(raw) {
        Status::Ok => Ok(()),
        status => Err(failure(operation, status)),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Enums (yoga.ts; the values are Yoga's and `OT_*` alike)

macro_rules! yoga_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $value:expr),* $(,)? }) => {
        $(#[$meta])*
        #[repr(u32)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant = $value),* }

        impl $name {
            pub fn from_raw(raw: u32) -> Option<$name> {
                match raw {
                    $($value => Some($name::$variant),)*
                    _ => None,
                }
            }
        }

        impl From<$name> for u32 {
            fn from(value: $name) -> u32 {
                value as u32
            }
        }
    };
}

yoga_enum!(Align {
    Auto = 0, FlexStart = 1, Center = 2, FlexEnd = 3, Stretch = 4, Baseline = 5, SpaceBetween = 6,
    SpaceAround = 7, SpaceEvenly = 8,
});
yoga_enum!(BoxSizing { BorderBox = 0, ContentBox = 1 });
yoga_enum!(Dimension { Width = 0, Height = 1 });
yoga_enum!(Direction { Inherit = 0, LTR = 1, RTL = 2 });
yoga_enum!(Display { Flex = 0, None = 1, Contents = 2 });
yoga_enum!(Edge {
    Left = 0, Top = 1, Right = 2, Bottom = 3, Start = 4, End = 5, Horizontal = 6, Vertical = 7, All = 8,
});
yoga_enum!(ExperimentalFeature { WebFlexBasis = 0 });
yoga_enum!(FlexDirection { Column = 0, ColumnReverse = 1, Row = 2, RowReverse = 3 });
yoga_enum!(Gutter { Column = 0, Row = 1, All = 2 });
yoga_enum!(Justify { FlexStart = 0, Center = 1, FlexEnd = 2, SpaceBetween = 3, SpaceAround = 4, SpaceEvenly = 5 });
yoga_enum!(LogLevel { Error = 0, Warn = 1, Info = 2, Debug = 3, Verbose = 4, Fatal = 5 });
yoga_enum!(MeasureMode { Undefined = 0, Exactly = 1, AtMost = 2 });
yoga_enum!(NodeType { Default = 0, Text = 1 });
yoga_enum!(Overflow { Visible = 0, Hidden = 1, Scroll = 2 });
yoga_enum!(PositionType { Static = 0, Relative = 1, Absolute = 2 });
yoga_enum!(Unit { Undefined = 0, Point = 1, Percent = 2, Auto = 3 });
yoga_enum!(Wrap { NoWrap = 0, Wrap = 1, WrapReverse = 2 });

yoga_enum!(
    /// The style enum kinds of the native ABI (`OT_STYLE_ENUM_*`, `YogaEnumKind`).
    EnumKind {
    Direction = 0, FlexDirection = 1, JustifyContent = 2, AlignContent = 3, AlignItems = 4, AlignSelf = 5,
    PositionType = 6, FlexWrap = 7, Overflow = 8, Display = 9, BoxSizing = 10,
});
yoga_enum!(
    /// `OT_STYLE_FLOAT_*`, `YogaFloatKind`.
    FloatKind { Flex = 0, FlexGrow = 1, FlexShrink = 2, AspectRatio = 3 });
yoga_enum!(
    /// `OT_STYLE_VALUE_*`, `YogaValueKind`.
    ValueKind {
    Width = 0, Height = 1, MinWidth = 2, MinHeight = 3, MaxWidth = 4, MaxHeight = 5, FlexBasis = 6, Margin = 7,
    Padding = 8, Position = 9, Gap = 10,
});
yoga_enum!(
    /// The computed edges `layout_get_edge` reads (`YogaEdgeLayoutKind`).
    LayoutEdgeKind { Margin = 0, Padding = 1, Border = 2 });

/// `NATIVE_EDGE_NONE` (`OT_EDGE_NONE`): the edge argument of a value kind without edges.
pub const EDGE_NONE: u32 = 0;
/// `OT_YOGA_DEPTH_MAX`: the deepest tree placement accepts.
pub const DEPTH_MAX: u32 = sys::OT_YOGA_DEPTH_MAX;

/// Yoga's errata flags (`Errata`); any `u32` can be passed, as in TS, and the config validates it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Errata(pub u32);

impl Errata {
    pub const NONE: Errata = Errata(0);
    pub const STRETCH_FLEX_BASIS: Errata = Errata(1);
    pub const ABSOLUTE_POSITION_WITHOUT_INSETS_EXCLUDES_PADDING: Errata = Errata(2);
    pub const ABSOLUTE_PERCENT_AGAINST_INNER_SIZE: Errata = Errata(4);
    pub const ALL: Errata = Errata(sys::YG_ERRATA_ALL);
    pub const CLASSIC: Errata = Errata(sys::YG_ERRATA_CLASSIC);
}

impl std::ops::BitOr for Errata {
    type Output = Errata;
    fn bitor(self, other: Errata) -> Errata {
        Errata(self.0 | other.0)
    }
}

/// A style value (`Value`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Value {
    pub unit: Unit,
    pub value: f32,
}

/// `UNDEFINED_VALUE`.
pub const UNDEFINED_VALUE: Value = Value { unit: Unit::Undefined, value: f32::NAN };

/// A computed layout (`Layout`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Layout {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub width: f32,
    pub height: f32,
}

/// A measure function's result (`Size`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

/// `MeasureFunction`: `(width, widthMode, height, heightMode) -> Size`.
pub type MeasureFunction = Box<dyn FnMut(f32, MeasureMode, f32, MeasureMode) -> Size>;
/// `DirtiedFunction`: called with the node Yoga marked dirty.
pub type DirtiedFunction = Box<dyn FnMut(&Node)>;
/// A Yoga log sink: the level and the formatted message (at most 4095 bytes, as yoga.zig's buffer holds).
pub type Logger = Box<dyn FnMut(LogLevel, &str)>;

// ---------------------------------------------------------------------------------------------------------------
// The callback runtime: what YogaHost keeps per library, here per thread

#[derive(Default)]
struct Runtime {
    /// Wrapper calls into Yoga in progress that can run callbacks.
    calls: Cell<u32>,
    /// Callbacks running.
    callbacks: Cell<u32>,
    /// Nodes dropped during a call, freed when the outermost call returns.
    deferred: RefCell<Vec<Deferred>>,
    /// The first panic of a callback, resumed when the outermost call returns.
    panic: Cell<Option<Box<dyn Any + Send>>>,
}

thread_local! {
    static RUNTIME: Runtime = Runtime::default();
}

fn in_callback() -> bool {
    RUNTIME.try_with(|runtime| runtime.callbacks.get() != 0).unwrap_or(false)
}

/// `YogaHost.assertMutable`.
fn assert_mutable() -> Result<()> {
    if in_callback() { Err(Error::InCallback) } else { Ok(()) }
}

/// Runs a call into Yoga that can run callbacks; when the outermost one returns, frees the nodes dropped meanwhile
/// and resumes a callback's panic.
fn call<R>(operation: impl FnOnce() -> R) -> R {
    RUNTIME.with(|runtime| runtime.calls.set(runtime.calls.get() + 1));
    let result = operation();
    let outermost = RUNTIME.with(|runtime| {
        let depth = runtime.calls.get() - 1;
        runtime.calls.set(depth);
        depth == 0
    });
    if outermost {
        loop {
            let batch = RUNTIME.with(|runtime| std::mem::take(&mut *runtime.deferred.borrow_mut()));
            if batch.is_empty() {
                break;
            }
            for deferred in batch {
                deferred.free();
            }
        }
        if let Some(payload) = RUNTIME.with(|runtime| runtime.panic.take()) {
            resume_unwind(payload);
        }
    }
    result
}

/// Runs a callback: mutations inside fail, a panic is kept for the outermost call.
fn run_callback<R>(fallback: R, callback: impl FnOnce() -> R) -> R {
    RUNTIME.with(|runtime| runtime.callbacks.set(runtime.callbacks.get() + 1));
    let result = catch_unwind(AssertUnwindSafe(callback));
    RUNTIME.with(|runtime| runtime.callbacks.set(runtime.callbacks.get() - 1));
    match result {
        Ok(value) => value,
        Err(payload) => {
            RUNTIME.with(|runtime| {
                let first = runtime.panic.take().unwrap_or(payload);
                runtime.panic.set(Some(first));
            });
            fallback
        }
    }
}

/// Puts a callback back in its slot unless the callback replaced itself.
fn restore<T>(slot: &Cell<Option<T>>, callback: T) {
    let current = slot.take();
    slot.set(Some(current.unwrap_or(callback)));
}

// ---------------------------------------------------------------------------------------------------------------
// Config

struct ConfigCell {
    ptr: sys::YGConfigRef,
    logger: Cell<Option<Logger>>,
}

impl Drop for ConfigCell {
    fn drop(&mut self) {
        // Nodes hold their config, so none is left.
        unsafe {
            sys::YGConfigSetContext(self.ptr, ptr::null_mut());
            sys::YGConfigFree(self.ptr);
        }
    }
}

/// A Yoga config (yoga.ts `Config`, yoga.zig `Config`).
#[derive(Clone)]
pub struct Config(Rc<ConfigCell>);

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Config({:p})", self.0.ptr)
    }
}

impl PartialEq for Config {
    fn eq(&self, other: &Config) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Config {}

thread_local! {
    /// `YogaHost.getDefaultConfig()`: the config `Node::create(None)` uses.
    static HOST_DEFAULT: RefCell<Option<Config>> = const { RefCell::new(None) };
    /// yoga.zig's process `default_config`, for `Node::create_for_opentui`.
    static OPENTUI_DEFAULT: RefCell<Option<Config>> = const { RefCell::new(None) };
}

impl Config {
    /// `Config.create()` (`yogaConfigCreateChecked`): web defaults off, point scale factor 1.
    pub fn create() -> Result<Config> {
        const OPERATION: &str = "yogaConfigCreateChecked";
        assert_mutable()?;
        let mut raw: sys::YGConfigRef = ptr::null_mut();
        check(OPERATION, unsafe { sys::otYogaConfigCreate(&mut raw) })?;
        let cell = Rc::new(ConfigCell { ptr: raw, logger: Cell::new(None) });
        unsafe {
            sys::YGConfigSetUseWebDefaults(raw, false);
            sys::YGConfigSetPointScaleFactor(raw, 1.0);
            sys::YGConfigSetContext(raw, Rc::as_ptr(&cell) as *mut c_void);
        }
        Ok(Config(cell))
    }

    /// The config yoga.ts's `Node.create()` uses when none is given (one per thread, as one per library in TS).
    pub fn default_config() -> Config {
        Self::shared(&HOST_DEFAULT)
    }

    fn shared(key: &'static std::thread::LocalKey<RefCell<Option<Config>>>) -> Config {
        key.with(|slot| {
            slot.borrow_mut().get_or_insert_with(|| Config::create().expect("the default Yoga config")).clone()
        })
    }

    fn raw(&self) -> sys::YGConfigRef {
        self.0.ptr
    }

    /// `setUseWebDefaults` (`yogaConfigSetUseWebDefaultsChecked`).
    pub fn set_use_web_defaults(&self, enabled: bool) -> Result<()> {
        assert_mutable()?;
        unsafe { sys::YGConfigSetUseWebDefaults(self.raw(), enabled) };
        Ok(())
    }

    pub fn use_web_defaults(&self) -> bool {
        unsafe { sys::YGConfigGetUseWebDefaults(self.raw()) }
    }

    /// `setPointScaleFactor` (`yogaConfigSetPointScaleFactorChecked`): a finite factor of at least 0; the number
    /// narrows to f32 first, as the FFI call does.
    pub fn set_point_scale_factor(&self, point_scale_factor: f64) -> Result<()> {
        assert_mutable()?;
        let factor = point_scale_factor as f32;
        if !factor.is_finite() || factor < 0.0 {
            return Err(failure("yogaConfigSetPointScaleFactorChecked", Status::InvalidArgument));
        }
        unsafe { sys::YGConfigSetPointScaleFactor(self.raw(), factor) };
        Ok(())
    }

    pub fn get_point_scale_factor(&self) -> f32 {
        unsafe { sys::YGConfigGetPointScaleFactor(self.raw()) }
    }

    /// `setErrata` (`yogaConfigSetErrataChecked`): flags up to 7, or `ALL` or `CLASSIC`.
    pub fn set_errata(&self, errata: Errata) -> Result<()> {
        assert_mutable()?;
        let raw = errata.0;
        if raw > 7 && raw != sys::YG_ERRATA_ALL && raw != sys::YG_ERRATA_CLASSIC {
            return Err(failure("yogaConfigSetErrataChecked", Status::InvalidArgument));
        }
        unsafe { sys::YGConfigSetErrata(self.raw(), raw as c_int) };
        Ok(())
    }

    pub fn get_errata(&self) -> Errata {
        Errata(unsafe { sys::YGConfigGetErrata(self.raw()) } as u32)
    }

    /// `setExperimentalFeatureEnabled` (`yogaConfigSetExperimentalFeatureEnabledChecked`).
    pub fn set_experimental_feature_enabled(&self, feature: ExperimentalFeature, enabled: bool) -> Result<()> {
        assert_mutable()?;
        unsafe { sys::YGConfigSetExperimentalFeatureEnabled(self.raw(), feature as c_int, enabled) };
        Ok(())
    }

    /// `isExperimentalFeatureEnabled` (`yogaConfigIsExperimentalFeatureEnabledChecked`).
    pub fn is_experimental_feature_enabled(&self, feature: ExperimentalFeature) -> bool {
        unsafe { sys::YGConfigIsExperimentalFeatureEnabled(self.raw(), feature as c_int) }
    }

    /// `otYogaConfigSetRoundsPositionsFromAbsoluteEdges` (OpenTUI's Yoga patch): round each position from the
    /// absolute edges, so a child rounds inside its parent. The renderer's config sets it (`context.zig`).
    pub fn set_rounds_positions_from_absolute_edges(&self, enabled: bool) -> Result<()> {
        assert_mutable()?;
        unsafe { sys::otYogaConfigSetRoundsPositionsFromAbsoluteEdges(self.raw(), enabled as u32) };
        Ok(())
    }

    /// Where Yoga's log messages go (yoga.zig `Config.setLogger`); `None` restores Yoga's default logger, which
    /// prints to stdout.
    pub fn set_logger(&self, logger: Option<Logger>) -> Result<()> {
        assert_mutable()?;
        let installed = logger.is_some();
        self.0.logger.set(logger);
        let callback: sys::YGLogger = if installed { Some(sys::opencodeYogaLogger) } else { None };
        unsafe { sys::YGConfigSetLogger(self.raw(), callback) };
        Ok(())
    }
}

/// src/logger.cpp hands each formatted message here.
#[unsafe(no_mangle)]
extern "C" fn opencodeYogaLogMessage(
    config: sys::YGConfigConstRef,
    level: c_int,
    message: *const u8,
    length: c_int,
) -> c_int {
    let cell = unsafe { sys::YGConfigGetContext(config) } as *const ConfigCell;
    if cell.is_null() {
        return 0;
    }
    // Hold the config while its logger runs.
    let cell = unsafe {
        Rc::increment_strong_count(cell);
        Rc::from_raw(cell)
    };
    let Some(mut logger) = cell.logger.take() else { return 0 };
    let level = LogLevel::from_raw(level as u32).unwrap_or(LogLevel::Error);
    let text = if length < 0 {
        std::borrow::Cow::Borrowed("Log formatting failed")
    } else {
        let copied = (length as usize).min(4095);
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(message, copied) })
    };
    run_callback((), || logger(level, &text));
    restore(&cell.logger, logger);
    length
}

// ---------------------------------------------------------------------------------------------------------------
// Node

struct NodeCell {
    /// Null once freed.
    ptr: Cell<sys::YGNodeRef>,
    config: Config,
    measure: Cell<Option<MeasureFunction>>,
    dirtied: Cell<Option<DirtiedFunction>>,
    /// yoga.zig `CallbackContext.poisoned`: a layout of this tree failed; only teardown is accepted.
    poisoned: Cell<bool>,
    /// This node's own handle while it has a parent.
    keepalive: Cell<Option<Node>>,
}

/// A Yoga node whose last handle went: freed when no Yoga call is running.
struct Deferred {
    ptr: sys::YGNodeRef,
    _config: Config,
    _measure: Option<MeasureFunction>,
    _dirtied: Option<DirtiedFunction>,
}

impl Deferred {
    fn free(self) {
        unsafe { free_node(self.ptr) };
    }
}

/// Frees a Yoga node (`otYogaNodeFree`, which detaches it from its parent and its children) and releases the
/// children it held.
unsafe fn free_node(raw: sys::YGNodeRef) -> u32 {
    let children = unsafe { children_of(raw) };
    let status = unsafe { sys::otYogaNodeFree(raw) };
    if status == 0 {
        for child in &children {
            child.release_if_orphaned();
        }
    }
    status
}

/// Handles to a Yoga node's children.
unsafe fn children_of(raw: sys::YGNodeRef) -> Vec<Node> {
    let count = unsafe { sys::YGNodeGetChildCount(raw) };
    (0..count).filter_map(|index| unsafe { handle(sys::YGNodeGetChild(raw, index)) }).collect()
}

/// The handle of a live Yoga node made by this crate.
unsafe fn handle(raw: sys::YGNodeRef) -> Option<Node> {
    if raw.is_null() {
        return None;
    }
    let cell = unsafe { sys::YGNodeGetContext(raw) } as *const NodeCell;
    if cell.is_null() {
        return None;
    }
    // A node's context is cleared when its cell goes, so the cell is alive here.
    unsafe {
        Rc::increment_strong_count(cell);
        Some(Node(Rc::from_raw(cell)))
    }
}

impl Drop for NodeCell {
    fn drop(&mut self) {
        let raw = self.ptr.get();
        if raw.is_null() {
            return;
        }
        unsafe { sys::YGNodeSetContext(raw, ptr::null_mut()) };
        let deferred = Deferred {
            ptr: raw,
            _config: self.config.clone(),
            _measure: self.measure.take(),
            _dirtied: self.dirtied.take(),
        };
        let deferred = match RUNTIME.try_with(|runtime| runtime.calls.get() != 0) {
            Ok(true) => match RUNTIME.try_with(|runtime| runtime.deferred.borrow_mut().push(deferred)) {
                Ok(()) => return,
                Err(_) => unreachable!("the runtime outlives a running call"),
            },
            _ => deferred,
        };
        deferred.free();
    }
}

/// A Yoga node (yoga.ts `Node`); see the crate docs for ownership. Equality is identity.
#[derive(Clone)]
pub struct Node(Rc<NodeCell>);

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Node({:p})", Rc::as_ptr(&self.0))
    }
}

impl PartialEq for Node {
    fn eq(&self, other: &Node) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Node {}

impl std::hash::Hash for Node {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Rc::as_ptr(&self.0).hash(state);
    }
}

unsafe extern "C" fn measure_trampoline(
    node: sys::YGNodeConstRef,
    width: f32,
    width_mode: c_int,
    height: f32,
    height_mode: c_int,
) -> sys::YGSize {
    let unspecified = sys::YGSize { width: f32::NAN, height: f32::NAN };
    let Some(node) = (unsafe { handle(node as sys::YGNodeRef) }) else { return unspecified };
    let Some(mut measure) = node.0.measure.take() else { return unspecified };
    let width_mode = MeasureMode::from_raw(width_mode as u32).unwrap_or(MeasureMode::Undefined);
    let height_mode = MeasureMode::from_raw(height_mode as u32).unwrap_or(MeasureMode::Undefined);
    let size =
        run_callback(Size { width: f32::NAN, height: f32::NAN }, || measure(width, width_mode, height, height_mode));
    restore(&node.0.measure, measure);
    sys::YGSize { width: size.width, height: size.height }
}

unsafe extern "C" fn dirtied_trampoline(node: sys::YGNodeConstRef) {
    let Some(node) = (unsafe { handle(node as sys::YGNodeRef) }) else { return };
    let Some(mut dirtied) = node.0.dirtied.take() else { return };
    run_callback((), || dirtied(&node));
    restore(&node.0.dirtied, dirtied);
}

/// yoga.zig's `@sizeOf(CallbackContext)`, which `nodeStorageBytes` adds to the bridge's count.
const CALLBACK_CONTEXT_BYTES: u64 = 80;

impl Node {
    // ----- creation

    /// `Node.create(config?)` (`yogaNodeCreateWithConfigChecked`); without a config, [`Config::default_config`].
    pub fn create(config: Option<&Config>) -> Result<Node> {
        match config {
            Some(config) => Node::create_with_config(config),
            None => Node::create_with_config(&Config::default_config()),
        }
    }

    /// `Node.createWithConfig(config)`.
    pub fn create_with_config(config: &Config) -> Result<Node> {
        Node::make(config, "yogaNodeCreateWithConfigChecked")
    }

    /// `Node.createDefault()`.
    pub fn create_default() -> Result<Node> {
        Node::create(None)
    }

    /// `Node.createForOpenTUI()` (`yogaNodeCreateForOpenTUIChecked`): a node of OpenTUI's own default config.
    pub fn create_for_opentui() -> Result<Node> {
        Node::make(&Config::shared(&OPENTUI_DEFAULT), "yogaNodeCreateForOpenTUIChecked")
    }

    fn make(config: &Config, operation: &'static str) -> Result<Node> {
        assert_mutable()?;
        let mut raw: sys::YGNodeRef = ptr::null_mut();
        check(operation, unsafe { sys::otYogaNodeCreate(config.raw(), &mut raw) })?;
        let cell = Rc::new(NodeCell {
            ptr: Cell::new(raw),
            config: config.clone(),
            measure: Cell::new(None),
            dirtied: Cell::new(None),
            poisoned: Cell::new(false),
            keepalive: Cell::new(None),
        });
        unsafe { sys::YGNodeSetContext(raw, Rc::as_ptr(&cell) as *mut c_void) };
        Ok(Node(cell))
    }

    /// The config the node was made with.
    pub fn config(&self) -> &Config {
        &self.0.config
    }

    // ----- state

    /// `isFreed()`.
    pub fn is_freed(&self) -> bool {
        self.0.ptr.get().is_null()
    }

    /// Whether a failed layout poisoned the node's tree (yoga.zig): only teardown works on it then.
    pub fn is_poisoned(&self) -> bool {
        self.0.poisoned.get()
    }

    fn raw(&self) -> Option<sys::YGNodeRef> {
        let raw = self.0.ptr.get();
        (!raw.is_null()).then_some(raw)
    }

    /// yoga.zig `nodeStatus` for a live node (its `busy` check is unreachable, see the crate docs).
    fn live(&self, operation: &'static str, teardown: bool) -> Result<sys::YGNodeRef> {
        let raw = self.raw().ok_or(failure(operation, Status::InvalidArgument))?;
        if !teardown && self.0.poisoned.get() {
            return Err(failure(operation, Status::Poisoned));
        }
        Ok(raw)
    }

    fn release_if_orphaned(&self) {
        if let Some(raw) = self.raw()
            && !unsafe { sys::YGNodeGetOwner(raw) }.is_null()
        {
            return;
        }
        drop(self.0.keepalive.take());
    }

    fn adopted(&self) {
        let held = self.0.keepalive.take();
        self.0.keepalive.set(Some(held.unwrap_or_else(|| self.clone())));
    }

    fn mark_freed(&self) {
        self.0.ptr.set(ptr::null_mut());
        drop(self.0.measure.take());
        drop(self.0.dirtied.take());
        drop(self.0.keepalive.take());
    }

    // ----- teardown

    /// `free()` (`yogaNodeFreeChecked`): detaches the node from its parent and its children, which stay alive.
    pub fn free(&self) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        assert_mutable()?;
        self.free_checked()
    }

    fn free_checked(&self) -> Result<()> {
        const OPERATION: &str = "yogaNodeFreeChecked";
        let raw = self.live(OPERATION, true)?;
        let children = unsafe { children_of(raw) };
        unsafe { sys::YGNodeSetContext(raw, ptr::null_mut()) };
        let status = unsafe { sys::otYogaNodeFree(raw) };
        if status != 0 {
            unsafe { sys::YGNodeSetContext(raw, Rc::as_ptr(&self.0) as *mut c_void) };
            return check(OPERATION, status);
        }
        self.mark_freed();
        for child in &children {
            child.release_if_orphaned();
        }
        Ok(())
    }

    /// `freeRecursive()` (`yogaNodeFreeRecursiveChecked`): removes every child (which dirties and calls dirtied
    /// functions), then frees the subtree.
    pub fn free_recursive(&self) -> Result<()> {
        const OPERATION: &str = "yogaNodeFreeRecursiveChecked";
        if self.is_freed() {
            return Ok(());
        }
        assert_mutable()?;
        self.live(OPERATION, true)?;
        // Yoga's first-child order; every visited node is held until all are freed.
        let mut retired: Vec<Node> = Vec::new();
        let mut stack: Vec<(Node, usize)> = vec![(self.clone(), 0)];
        while let Some((node, next)) = stack.last_mut() {
            let raw = node.0.ptr.get();
            if unsafe { sys::YGNodeGetChildCount(raw) } > *next {
                let child_raw = unsafe { sys::YGNodeGetChild(raw, *next) };
                if unsafe { sys::YGNodeGetOwner(child_raw) } != raw {
                    *next += 1;
                    continue;
                }
                let child = unsafe { handle(child_raw) }.ok_or(failure(OPERATION, Status::InvalidArgument))?;
                let parent = node.clone();
                parent.remove_child_checked(&child).map_err(|error| match error {
                    Error::Yoga { status, .. } => failure(OPERATION, status),
                    other => other,
                })?;
                stack.push((child, 0));
            } else {
                let (node, _) = stack.pop().unwrap();
                retired.push(node);
            }
        }
        for node in retired.iter().rev() {
            node.free_checked()?;
        }
        Ok(())
    }

    /// `reset()` (`yogaNodeResetChecked`): back to a new node's style; only a node without parent or children.
    pub fn reset(&self) -> Result<()> {
        const OPERATION: &str = "yogaNodeResetChecked";
        if self.is_freed() {
            return Ok(());
        }
        assert_mutable()?;
        let raw = self.live(OPERATION, false)?;
        let status = call(|| unsafe { sys::otYogaNodeReset(raw) });
        // Yoga clears the context on reset.
        unsafe { sys::YGNodeSetContext(raw, Rc::as_ptr(&self.0) as *mut c_void) };
        check(OPERATION, status)?;
        self.0.poisoned.set(false);
        drop(self.0.measure.take());
        drop(self.0.dirtied.take());
        Ok(())
    }

    /// yoga.zig `nodeStorageBytes`: the Yoga node, its child vector and the wrapper's callback context.
    pub fn storage_bytes(&self) -> u64 {
        self.raw().map_or(0, |raw| unsafe { sys::otYogaNodeStorageBytes(raw) } + CALLBACK_CONTEXT_BYTES)
    }

    // ----- tree

    fn same_library(&self, other: &Node) -> Result<()> {
        if other.is_freed() { Err(Error::NodeFreed) } else { Ok(()) }
    }

    /// `copyStyle(node)`.
    pub fn copy_style(&self, node: &Node) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        self.same_library(node)?;
        self.copy_style_checked(node)
    }

    /// `yogaNodeCopyStyleChecked`.
    pub fn copy_style_checked(&self, source: &Node) -> Result<()> {
        const OPERATION: &str = "yogaNodeCopyStyleChecked";
        assert_mutable()?;
        let raw = self.live(OPERATION, false)?;
        let source = source.live(OPERATION, false)?;
        check(OPERATION, call(|| unsafe { sys::otYogaNodeCopyStyle(raw, source) }))
    }

    /// yoga.zig `placementStatus`.
    fn placement(
        &self,
        operation: &'static str,
        child: &Node,
        index: u32,
        moving: bool,
    ) -> Result<(sys::YGNodeRef, sys::YGNodeRef)> {
        let raw = self.live(operation, false)?;
        let child_raw = child.live(operation, false)?;
        let previous = unsafe { sys::YGNodeGetOwner(child_raw) };
        if !moving && !previous.is_null() {
            return Err(failure(operation, Status::InvalidArgument));
        }
        if !previous.is_null() {
            let owner = unsafe { handle(previous) }.ok_or(failure(operation, Status::InvalidArgument))?;
            owner.live(operation, false)?;
        }
        let count = unsafe { sys::YGNodeGetChildCount(raw) } as u32;
        let own = previous == raw;
        if own && count == 0 {
            return Err(failure(operation, Status::InvalidArgument));
        }
        if index > count - own as u32 || unsafe { sys::YGNodeHasMeasureFunc(raw) } {
            return Err(failure(operation, Status::InvalidArgument));
        }
        let mut parent = raw;
        let mut depth = 0u32;
        while !parent.is_null() {
            if parent == child_raw {
                return Err(failure(operation, Status::InvalidArgument));
            }
            depth += 1;
            if depth >= DEPTH_MAX {
                return Err(failure(operation, Status::DepthLimit));
            }
            parent = unsafe { sys::YGNodeGetOwner(parent) };
        }
        // The child's subtree, breadth first, with each node's depth in it (the child is 1).
        let mut queue = vec![(child_raw, 1u32)];
        let mut cursor = 0;
        while let Some(&(node, level)) = queue.get(cursor) {
            if depth + level > DEPTH_MAX {
                return Err(failure(operation, Status::DepthLimit));
            }
            for index in 0..unsafe { sys::YGNodeGetChildCount(node) } {
                queue.push((unsafe { sys::YGNodeGetChild(node, index) }, level + 1));
            }
            cursor += 1;
        }
        Ok((raw, child_raw))
    }

    /// `insertChild(child, index)`.
    pub fn insert_child(&self, child: &Node, index: u32) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        self.same_library(child)?;
        self.insert_child_checked(child, index)
    }

    /// `yogaNodeInsertChildChecked`: a child without a parent, at an index up to the child count.
    pub fn insert_child_checked(&self, child: &Node, index: u32) -> Result<()> {
        const OPERATION: &str = "yogaNodeInsertChildChecked";
        assert_mutable()?;
        let (raw, child_raw) = self.placement(OPERATION, child, index, false)?;
        check(OPERATION, call(|| unsafe { sys::otYogaNodeInsertChild(raw, child_raw, index) }))?;
        child.adopted();
        Ok(())
    }

    /// `yogaNodeMoveChildChecked` (the scene's placement): inserts the child at `final_index`, moving it from its
    /// current parent (this one or another), and dirties both ancestor paths before any dirtied function runs.
    pub fn move_child(&self, child: &Node, final_index: u32) -> Result<()> {
        const OPERATION: &str = "yogaNodeMoveChildChecked";
        assert_mutable()?;
        let (raw, child_raw) = self.placement(OPERATION, child, final_index, true)?;
        let previous = unsafe { sys::YGNodeGetOwner(child_raw) };
        let previous = unsafe { handle(previous) };
        let status = call(|| unsafe { sys::otYogaNodeMoveChild(raw, child_raw, final_index) });
        check(OPERATION, status)?;
        child.adopted();
        drop(previous);
        Ok(())
    }

    /// `removeChild(child)`.
    pub fn remove_child(&self, child: &Node) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        self.same_library(child)?;
        self.remove_child_checked(child)
    }

    /// `yogaNodeRemoveChildChecked`: removes the child if it is one, dirtying this node.
    pub fn remove_child_checked(&self, child: &Node) -> Result<()> {
        const OPERATION: &str = "yogaNodeRemoveChildChecked";
        assert_mutable()?;
        let raw = self.live(OPERATION, true)?;
        let child_raw = child.live(OPERATION, true)?;
        check(OPERATION, call(|| unsafe { sys::otYogaNodeRemoveChild(raw, child_raw) }))?;
        child.release_if_orphaned();
        Ok(())
    }

    /// `removeAllChildren()` (`yogaNodeRemoveAllChildrenChecked`).
    pub fn remove_all_children(&self) -> Result<()> {
        const OPERATION: &str = "yogaNodeRemoveAllChildrenChecked";
        if self.is_freed() {
            return Ok(());
        }
        assert_mutable()?;
        let raw = self.live(OPERATION, true)?;
        let children = unsafe { children_of(raw) };
        check(OPERATION, call(|| unsafe { sys::otYogaNodeRemoveAllChildren(raw) }))?;
        for child in &children {
            child.release_if_orphaned();
        }
        Ok(())
    }

    /// `getChild(index)` (`yogaNodeGetChildChecked`).
    pub fn get_child(&self, index: u32) -> Result<Option<Node>> {
        let Some(raw) = self.raw() else { return Ok(None) };
        if (index as usize) < unsafe { sys::YGNodeGetChildCount(raw) } {
            Ok(unsafe { handle(sys::YGNodeGetChild(raw, index as usize)) })
        } else {
            Ok(None)
        }
    }

    /// `getChildCount()`.
    pub fn get_child_count(&self) -> u32 {
        self.raw().map_or(0, |raw| unsafe { sys::YGNodeGetChildCount(raw) } as u32)
    }

    /// `getParent()`.
    pub fn get_parent(&self) -> Option<Node> {
        let raw = self.raw()?;
        unsafe { handle(sys::YGNodeGetParent(raw)) }
    }

    // ----- layout

    /// `calculateLayout(width?, height?, direction = LTR)`: `None` is `undefined` or `"auto"` (no constraint).
    pub fn calculate_layout(&self, width: Option<f64>, height: Option<f64>, direction: Direction) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        let narrow = |value: Option<f64>| value.map_or(f32::NAN, |value| value as f32);
        self.calculate_layout_checked(narrow(width), narrow(height), direction as u32)
    }

    /// `yogaNodeCalculateLayoutChecked`: NaN or a finite size of at least 0 per axis. A failed layout poisons
    /// the whole tree.
    pub fn calculate_layout_checked(&self, width: f32, height: f32, direction: u32) -> Result<()> {
        const OPERATION: &str = "yogaNodeCalculateLayoutChecked";
        assert_mutable()?;
        let raw = self.live(OPERATION, false)?;
        if direction > sys::YG_DIRECTION_RTL {
            return Err(failure(OPERATION, Status::InvalidArgument));
        }
        for dimension in [width, height] {
            if !dimension.is_nan() && (!dimension.is_finite() || dimension < 0.0) {
                return Err(failure(OPERATION, Status::InvalidArgument));
            }
        }
        let status = call(|| unsafe { sys::otYogaNodeCalculateLayout(raw, width, height, direction) });
        if status != 0 {
            // Interrupted solves can leave partially updated caches: only teardown is safe.
            let mut top = raw;
            loop {
                let owner = unsafe { sys::YGNodeGetOwner(top) };
                if owner.is_null() {
                    break;
                }
                top = owner;
            }
            let mut queue = vec![top];
            while let Some(node) = queue.pop() {
                if let Some(handle) = unsafe { handle(node) } {
                    handle.0.poisoned.set(true);
                }
                for index in 0..unsafe { sys::YGNodeGetChildCount(node) } {
                    queue.push(unsafe { sys::YGNodeGetChild(node, index) });
                }
            }
        }
        check(OPERATION, status)
    }

    /// `hasNewLayout()` (`yogaNodeGetHasNewLayoutChecked`).
    pub fn has_new_layout(&self) -> Result<bool> {
        let Some(_) = self.raw() else { return Ok(false) };
        let raw = self.live("yogaNodeGetHasNewLayoutChecked", false)?;
        Ok(unsafe { sys::YGNodeGetHasNewLayout(raw) })
    }

    /// `markLayoutSeen()`: `setHasNewLayout(false)`.
    pub fn mark_layout_seen(&self) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        self.set_has_new_layout(false)
    }

    /// `yogaNodeSetHasNewLayoutChecked`.
    pub fn set_has_new_layout(&self, has_new_layout: bool) -> Result<()> {
        self.set_flag("yogaNodeSetHasNewLayoutChecked", 0, has_new_layout as u32)
    }

    fn set_flag(&self, operation: &'static str, kind: u32, value: u32) -> Result<()> {
        assert_mutable()?;
        let raw = self.live(operation, false)?;
        check(operation, call(|| unsafe { sys::otYogaNodeSetFlag(raw, kind, value) }))
    }

    /// `markDirty()` (`yogaNodeMarkDirtyChecked`): only a node with a measure function.
    pub fn mark_dirty(&self) -> Result<()> {
        const OPERATION: &str = "yogaNodeMarkDirtyChecked";
        if self.is_freed() {
            return Ok(());
        }
        assert_mutable()?;
        let raw = self.live(OPERATION, false)?;
        check(OPERATION, call(|| unsafe { sys::otYogaNodeMarkDirty(raw) }))
    }

    /// `isDirty()` (`yogaNodeIsDirtyChecked`); a freed node is dirty.
    pub fn is_dirty(&self) -> Result<bool> {
        if self.is_freed() {
            return Ok(true);
        }
        let raw = self.live("yogaNodeIsDirtyChecked", false)?;
        Ok(unsafe { sys::YGNodeIsDirty(raw) })
    }

    /// `getComputedLayout()` (`yogaNodeGetComputedLayoutChecked`); zeros for a freed node.
    pub fn get_computed_layout(&self) -> Result<Layout> {
        const OPERATION: &str = "yogaNodeGetComputedLayoutChecked";
        if self.is_freed() {
            return Ok(Layout::default());
        }
        let raw = self.live(OPERATION, false)?;
        let mut out = sys::OTYogaLayout::default();
        check(OPERATION, unsafe { sys::otYogaNodeGetComputedLayout(raw, &mut out) })?;
        Ok(Layout {
            left: out.left,
            top: out.top,
            right: out.right,
            bottom: out.bottom,
            width: out.width,
            height: out.height,
        })
    }

    pub fn get_computed_left(&self) -> Result<f32> {
        Ok(self.get_computed_layout()?.left)
    }

    pub fn get_computed_top(&self) -> Result<f32> {
        Ok(self.get_computed_layout()?.top)
    }

    pub fn get_computed_right(&self) -> Result<f32> {
        Ok(self.get_computed_layout()?.right)
    }

    pub fn get_computed_bottom(&self) -> Result<f32> {
        Ok(self.get_computed_layout()?.bottom)
    }

    pub fn get_computed_width(&self) -> Result<f32> {
        Ok(self.get_computed_layout()?.width)
    }

    pub fn get_computed_height(&self) -> Result<f32> {
        Ok(self.get_computed_layout()?.height)
    }

    /// `yogaNodeLayoutGetEdgeChecked`: a computed margin, padding or border (`LayoutEdgeKind`) of an edge up to
    /// `End`.
    pub fn layout_get_edge(&self, kind: u32, edge: u32) -> Result<f32> {
        const OPERATION: &str = "yogaNodeLayoutGetEdgeChecked";
        let raw = self.live(OPERATION, false)?;
        let mut out = f32::NAN;
        check(OPERATION, unsafe { sys::otYogaNodeLayoutGetEdge(raw, kind, edge, &mut out) })?;
        Ok(out)
    }

    fn computed_edge(&self, kind: LayoutEdgeKind, edge: Edge) -> Result<f32> {
        if self.is_freed() {
            return Ok(0.0);
        }
        self.layout_get_edge(kind as u32, edge as u32)
    }

    /// `getComputedMargin(edge)`.
    pub fn get_computed_margin(&self, edge: Edge) -> Result<f32> {
        self.computed_edge(LayoutEdgeKind::Margin, edge)
    }

    /// `getComputedPadding(edge)`.
    pub fn get_computed_padding(&self, edge: Edge) -> Result<f32> {
        self.computed_edge(LayoutEdgeKind::Padding, edge)
    }

    /// `getComputedBorder(edge)`.
    pub fn get_computed_border(&self, edge: Edge) -> Result<f32> {
        self.computed_edge(LayoutEdgeKind::Border, edge)
    }

    // ----- style, as yoga.zig takes it

    /// `yogaNodeStyleSetEnumChecked`: an [`EnumKind`] and a value up to that enum's last.
    pub fn style_set_enum(&self, kind: u32, value: u32) -> Result<()> {
        const OPERATION: &str = "yogaNodeStyleSetEnumChecked";
        assert_mutable()?;
        let raw = self.live(OPERATION, false)?;
        check(OPERATION, call(|| unsafe { sys::otYogaNodeStyleSetEnum(raw, kind, value) }))
    }

    /// `yogaNodeStyleGetEnumChecked`.
    pub fn style_get_enum(&self, kind: u32) -> Result<u32> {
        const OPERATION: &str = "yogaNodeStyleGetEnumChecked";
        let raw = self.live(OPERATION, false)?;
        let mut out = 0;
        check(OPERATION, unsafe { sys::otYogaNodeStyleGetEnum(raw, kind, &mut out) })?;
        Ok(out)
    }

    /// `yogaNodeStyleSetFloatChecked`: a [`FloatKind`]; NaN is undefined, an infinity invalid.
    pub fn style_set_float(&self, kind: u32, value: f32) -> Result<()> {
        const OPERATION: &str = "yogaNodeStyleSetFloatChecked";
        assert_mutable()?;
        let raw = self.live(OPERATION, false)?;
        check(OPERATION, call(|| unsafe { sys::otYogaNodeStyleSetFloat(raw, kind, value) }))
    }

    /// `yogaNodeStyleGetFloatChecked`.
    pub fn style_get_float(&self, kind: u32) -> Result<f32> {
        const OPERATION: &str = "yogaNodeStyleGetFloatChecked";
        let raw = self.live(OPERATION, false)?;
        let mut out = f32::NAN;
        check(OPERATION, unsafe { sys::otYogaNodeStyleGetFloat(raw, kind, &mut out) })?;
        Ok(out)
    }

    /// `yogaNodeStyleSetBorderChecked`: an edge up to `All`.
    pub fn style_set_border(&self, edge: u32, value: f32) -> Result<()> {
        const OPERATION: &str = "yogaNodeStyleSetBorderChecked";
        assert_mutable()?;
        let raw = self.live(OPERATION, false)?;
        check(OPERATION, call(|| unsafe { sys::otYogaNodeStyleSetBorder(raw, edge, value) }))
    }

    /// `yogaNodeStyleGetBorderChecked`.
    pub fn style_get_border(&self, edge: u32) -> Result<f32> {
        const OPERATION: &str = "yogaNodeStyleGetBorderChecked";
        let raw = self.live(OPERATION, false)?;
        let mut out = f32::NAN;
        check(OPERATION, unsafe { sys::otYogaNodeStyleGetBorder(raw, edge, &mut out) })?;
        Ok(out)
    }

    /// `yogaNodeStyleSetValueChecked`: a [`ValueKind`], an edge (margin, padding, position: up to `All`) or
    /// gutter (gap), a [`Unit`] (`Auto` only for width, height, flex basis, margin and position) and a value.
    pub fn style_set_value(&self, kind: u32, edge_or_gutter: u32, unit: u32, value: f32) -> Result<()> {
        const OPERATION: &str = "yogaNodeStyleSetValueChecked";
        assert_mutable()?;
        let raw = self.live(OPERATION, false)?;
        check(OPERATION, call(|| unsafe { sys::otYogaNodeStyleSetValue(raw, kind, edge_or_gutter, unit, value) }))
    }

    /// `yogaNodeStyleSetDimensionChecked` (the scene's width and height): a no-op when nothing changes; with
    /// `disable_flex_shrink` 1 also sets flex shrink 0, in one style update.
    pub fn style_set_dimension(&self, kind: u32, unit: u32, value: f32, disable_flex_shrink: u32) -> Result<()> {
        const OPERATION: &str = "yogaNodeStyleSetDimensionChecked";
        assert_mutable()?;
        let raw = self.live(OPERATION, false)?;
        let status = call(|| unsafe { sys::otYogaNodeStyleSetDimension(raw, kind, unit, value, disable_flex_shrink) });
        check(OPERATION, status)
    }

    /// `yogaNodeStyleSetPositionsChecked` (the scene's insets): the masked edges of left, top, right and bottom
    /// (bits 0 to 3) in one style update.
    pub fn style_set_positions(&self, edge_mask: u32, units: [u32; 4], values: [f32; 4]) -> Result<()> {
        const OPERATION: &str = "yogaNodeStyleSetPositionsChecked";
        assert_mutable()?;
        let raw = self.live(OPERATION, false)?;
        check(OPERATION, call(|| unsafe { sys::otYogaNodeStyleSetPositions(raw, edge_mask, &units, &values) }))
    }

    /// `yogaNodeStyleGetValueChecked`.
    pub fn style_get_value(&self, kind: u32, edge_or_gutter: u32) -> Result<Value> {
        const OPERATION: &str = "yogaNodeStyleGetValueChecked";
        let raw = self.live(OPERATION, false)?;
        let mut out: u64 = 0;
        check(OPERATION, unsafe { sys::otYogaNodeStyleGetValue(raw, kind, edge_or_gutter, &mut out) })?;
        let unit = Unit::from_raw((out & 0xffff_ffff) as u32).unwrap_or(Unit::Undefined);
        Ok(Value { unit, value: f32::from_bits((out >> 32) as u32) })
    }

    // ----- style, as yoga.ts takes it

    fn set_enum(&self, kind: EnumKind, value: u32) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        self.style_set_enum(kind as u32, value)
    }

    fn get_enum(&self, kind: EnumKind, fallback: u32) -> Result<u32> {
        if self.is_freed() {
            return Ok(fallback);
        }
        self.style_get_enum(kind as u32)
    }

    fn set_float(&self, kind: FloatKind, value: Option<f64>) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        self.style_set_float(kind as u32, value.map_or(f32::NAN, |value| value as f32))
    }

    fn get_float(&self, kind: FloatKind) -> Result<f32> {
        if self.is_freed() {
            return Ok(f32::NAN);
        }
        self.style_get_float(kind as u32)
    }

    fn set_value(&self, kind: ValueKind, edge_or_gutter: u32, input: ValueInput) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        let (unit, value) = input.resolve()?;
        self.style_set_value(kind as u32, edge_or_gutter, unit as u32, value)
    }

    fn get_value(&self, kind: ValueKind, edge_or_gutter: u32) -> Result<Value> {
        if self.is_freed() {
            return Ok(UNDEFINED_VALUE);
        }
        self.style_get_value(kind as u32, edge_or_gutter)
    }

    fn percent(value: Option<f64>) -> ValueInput {
        value.map_or(ValueInput::Undefined, ValueInput::Percent)
    }

    pub fn set_direction(&self, direction: Direction) -> Result<()> {
        self.set_enum(EnumKind::Direction, direction as u32)
    }

    pub fn get_direction(&self) -> Result<Direction> {
        let raw = self.get_enum(EnumKind::Direction, Direction::Inherit as u32)?;
        Ok(Direction::from_raw(raw).unwrap_or(Direction::Inherit))
    }

    pub fn set_flex_direction(&self, flex_direction: FlexDirection) -> Result<()> {
        self.set_enum(EnumKind::FlexDirection, flex_direction as u32)
    }

    pub fn get_flex_direction(&self) -> Result<FlexDirection> {
        let raw = self.get_enum(EnumKind::FlexDirection, FlexDirection::Column as u32)?;
        Ok(FlexDirection::from_raw(raw).unwrap_or(FlexDirection::Column))
    }

    pub fn set_justify_content(&self, justify_content: Justify) -> Result<()> {
        self.set_enum(EnumKind::JustifyContent, justify_content as u32)
    }

    pub fn get_justify_content(&self) -> Result<Justify> {
        let raw = self.get_enum(EnumKind::JustifyContent, Justify::FlexStart as u32)?;
        Ok(Justify::from_raw(raw).unwrap_or(Justify::FlexStart))
    }

    pub fn set_align_content(&self, align_content: Align) -> Result<()> {
        self.set_enum(EnumKind::AlignContent, align_content as u32)
    }

    pub fn get_align_content(&self) -> Result<Align> {
        let raw = self.get_enum(EnumKind::AlignContent, Align::FlexStart as u32)?;
        Ok(Align::from_raw(raw).unwrap_or(Align::FlexStart))
    }

    pub fn set_align_items(&self, align_items: Align) -> Result<()> {
        self.set_enum(EnumKind::AlignItems, align_items as u32)
    }

    pub fn get_align_items(&self) -> Result<Align> {
        let raw = self.get_enum(EnumKind::AlignItems, Align::Stretch as u32)?;
        Ok(Align::from_raw(raw).unwrap_or(Align::Stretch))
    }

    pub fn set_align_self(&self, align_self: Align) -> Result<()> {
        self.set_enum(EnumKind::AlignSelf, align_self as u32)
    }

    pub fn get_align_self(&self) -> Result<Align> {
        let raw = self.get_enum(EnumKind::AlignSelf, Align::Auto as u32)?;
        Ok(Align::from_raw(raw).unwrap_or(Align::Auto))
    }

    pub fn set_position_type(&self, position_type: PositionType) -> Result<()> {
        self.set_enum(EnumKind::PositionType, position_type as u32)
    }

    pub fn get_position_type(&self) -> Result<PositionType> {
        let raw = self.get_enum(EnumKind::PositionType, PositionType::Relative as u32)?;
        Ok(PositionType::from_raw(raw).unwrap_or(PositionType::Relative))
    }

    pub fn set_flex_wrap(&self, flex_wrap: Wrap) -> Result<()> {
        self.set_enum(EnumKind::FlexWrap, flex_wrap as u32)
    }

    pub fn get_flex_wrap(&self) -> Result<Wrap> {
        let raw = self.get_enum(EnumKind::FlexWrap, Wrap::NoWrap as u32)?;
        Ok(Wrap::from_raw(raw).unwrap_or(Wrap::NoWrap))
    }

    pub fn set_overflow(&self, overflow: Overflow) -> Result<()> {
        self.set_enum(EnumKind::Overflow, overflow as u32)
    }

    pub fn get_overflow(&self) -> Result<Overflow> {
        let raw = self.get_enum(EnumKind::Overflow, Overflow::Visible as u32)?;
        Ok(Overflow::from_raw(raw).unwrap_or(Overflow::Visible))
    }

    pub fn set_display(&self, display: Display) -> Result<()> {
        self.set_enum(EnumKind::Display, display as u32)
    }

    pub fn get_display(&self) -> Result<Display> {
        let raw = self.get_enum(EnumKind::Display, Display::Flex as u32)?;
        Ok(Display::from_raw(raw).unwrap_or(Display::Flex))
    }

    pub fn set_box_sizing(&self, box_sizing: BoxSizing) -> Result<()> {
        self.set_enum(EnumKind::BoxSizing, box_sizing as u32)
    }

    pub fn get_box_sizing(&self) -> Result<BoxSizing> {
        let raw = self.get_enum(EnumKind::BoxSizing, BoxSizing::BorderBox as u32)?;
        Ok(BoxSizing::from_raw(raw).unwrap_or(BoxSizing::BorderBox))
    }

    /// `setFlex(flex)`: `None` is undefined.
    pub fn set_flex(&self, flex: Option<f64>) -> Result<()> {
        self.set_float(FloatKind::Flex, flex)
    }

    pub fn get_flex(&self) -> Result<f32> {
        self.get_float(FloatKind::Flex)
    }

    pub fn set_flex_grow(&self, flex_grow: Option<f64>) -> Result<()> {
        self.set_float(FloatKind::FlexGrow, flex_grow)
    }

    pub fn get_flex_grow(&self) -> Result<f32> {
        self.get_float(FloatKind::FlexGrow)
    }

    pub fn set_flex_shrink(&self, flex_shrink: Option<f64>) -> Result<()> {
        self.set_float(FloatKind::FlexShrink, flex_shrink)
    }

    pub fn get_flex_shrink(&self) -> Result<f32> {
        self.get_float(FloatKind::FlexShrink)
    }

    pub fn set_aspect_ratio(&self, aspect_ratio: Option<f64>) -> Result<()> {
        self.set_float(FloatKind::AspectRatio, aspect_ratio)
    }

    pub fn get_aspect_ratio(&self) -> Result<f32> {
        self.get_float(FloatKind::AspectRatio)
    }

    pub fn set_flex_basis(&self, flex_basis: impl Into<ValueInput>) -> Result<()> {
        self.set_value(ValueKind::FlexBasis, EDGE_NONE, flex_basis.into())
    }

    pub fn set_flex_basis_percent(&self, flex_basis: Option<f64>) -> Result<()> {
        self.set_value(ValueKind::FlexBasis, EDGE_NONE, Self::percent(flex_basis))
    }

    pub fn set_flex_basis_auto(&self) -> Result<()> {
        self.set_value(ValueKind::FlexBasis, EDGE_NONE, ValueInput::Auto)
    }

    pub fn get_flex_basis(&self) -> Result<Value> {
        self.get_value(ValueKind::FlexBasis, EDGE_NONE)
    }

    pub fn set_width(&self, width: impl Into<ValueInput>) -> Result<()> {
        self.set_value(ValueKind::Width, EDGE_NONE, width.into())
    }

    pub fn set_width_percent(&self, width: Option<f64>) -> Result<()> {
        self.set_value(ValueKind::Width, EDGE_NONE, Self::percent(width))
    }

    pub fn set_width_auto(&self) -> Result<()> {
        self.set_value(ValueKind::Width, EDGE_NONE, ValueInput::Auto)
    }

    pub fn get_width(&self) -> Result<Value> {
        self.get_value(ValueKind::Width, EDGE_NONE)
    }

    pub fn set_height(&self, height: impl Into<ValueInput>) -> Result<()> {
        self.set_value(ValueKind::Height, EDGE_NONE, height.into())
    }

    pub fn set_height_percent(&self, height: Option<f64>) -> Result<()> {
        self.set_value(ValueKind::Height, EDGE_NONE, Self::percent(height))
    }

    pub fn set_height_auto(&self) -> Result<()> {
        self.set_value(ValueKind::Height, EDGE_NONE, ValueInput::Auto)
    }

    pub fn get_height(&self) -> Result<Value> {
        self.get_value(ValueKind::Height, EDGE_NONE)
    }

    pub fn set_min_width(&self, min_width: impl Into<ValueInput>) -> Result<()> {
        self.set_value(ValueKind::MinWidth, EDGE_NONE, min_width.into())
    }

    pub fn set_min_width_percent(&self, min_width: Option<f64>) -> Result<()> {
        self.set_value(ValueKind::MinWidth, EDGE_NONE, Self::percent(min_width))
    }

    pub fn get_min_width(&self) -> Result<Value> {
        self.get_value(ValueKind::MinWidth, EDGE_NONE)
    }

    pub fn set_min_height(&self, min_height: impl Into<ValueInput>) -> Result<()> {
        self.set_value(ValueKind::MinHeight, EDGE_NONE, min_height.into())
    }

    pub fn set_min_height_percent(&self, min_height: Option<f64>) -> Result<()> {
        self.set_value(ValueKind::MinHeight, EDGE_NONE, Self::percent(min_height))
    }

    pub fn get_min_height(&self) -> Result<Value> {
        self.get_value(ValueKind::MinHeight, EDGE_NONE)
    }

    pub fn set_max_width(&self, max_width: impl Into<ValueInput>) -> Result<()> {
        self.set_value(ValueKind::MaxWidth, EDGE_NONE, max_width.into())
    }

    pub fn set_max_width_percent(&self, max_width: Option<f64>) -> Result<()> {
        self.set_value(ValueKind::MaxWidth, EDGE_NONE, Self::percent(max_width))
    }

    pub fn get_max_width(&self) -> Result<Value> {
        self.get_value(ValueKind::MaxWidth, EDGE_NONE)
    }

    pub fn set_max_height(&self, max_height: impl Into<ValueInput>) -> Result<()> {
        self.set_value(ValueKind::MaxHeight, EDGE_NONE, max_height.into())
    }

    pub fn set_max_height_percent(&self, max_height: Option<f64>) -> Result<()> {
        self.set_value(ValueKind::MaxHeight, EDGE_NONE, Self::percent(max_height))
    }

    pub fn get_max_height(&self) -> Result<Value> {
        self.get_value(ValueKind::MaxHeight, EDGE_NONE)
    }

    pub fn set_margin(&self, edge: Edge, margin: impl Into<ValueInput>) -> Result<()> {
        self.set_value(ValueKind::Margin, edge as u32, margin.into())
    }

    pub fn set_margin_percent(&self, edge: Edge, margin: Option<f64>) -> Result<()> {
        self.set_value(ValueKind::Margin, edge as u32, Self::percent(margin))
    }

    pub fn set_margin_auto(&self, edge: Edge) -> Result<()> {
        self.set_value(ValueKind::Margin, edge as u32, ValueInput::Auto)
    }

    pub fn get_margin(&self, edge: Edge) -> Result<Value> {
        self.get_value(ValueKind::Margin, edge as u32)
    }

    pub fn set_padding(&self, edge: Edge, padding: impl Into<ValueInput>) -> Result<()> {
        self.set_value(ValueKind::Padding, edge as u32, padding.into())
    }

    pub fn set_padding_percent(&self, edge: Edge, padding: Option<f64>) -> Result<()> {
        self.set_value(ValueKind::Padding, edge as u32, Self::percent(padding))
    }

    pub fn get_padding(&self, edge: Edge) -> Result<Value> {
        self.get_value(ValueKind::Padding, edge as u32)
    }

    pub fn set_position(&self, edge: Edge, position: impl Into<ValueInput>) -> Result<()> {
        self.set_value(ValueKind::Position, edge as u32, position.into())
    }

    pub fn set_position_percent(&self, edge: Edge, position: Option<f64>) -> Result<()> {
        self.set_value(ValueKind::Position, edge as u32, Self::percent(position))
    }

    pub fn set_position_auto(&self, edge: Edge) -> Result<()> {
        self.set_value(ValueKind::Position, edge as u32, ValueInput::Auto)
    }

    pub fn get_position(&self, edge: Edge) -> Result<Value> {
        self.get_value(ValueKind::Position, edge as u32)
    }

    pub fn set_gap(&self, gutter: Gutter, gap: impl Into<ValueInput>) -> Result<()> {
        self.set_value(ValueKind::Gap, gutter as u32, gap.into())
    }

    pub fn set_gap_percent(&self, gutter: Gutter, gap: Option<f64>) -> Result<()> {
        self.set_value(ValueKind::Gap, gutter as u32, Self::percent(gap))
    }

    pub fn get_gap(&self, gutter: Gutter) -> Result<Value> {
        self.get_value(ValueKind::Gap, gutter as u32)
    }

    /// `setBorder(edge, border)`: `None` is undefined.
    pub fn set_border(&self, edge: Edge, border: Option<f64>) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        self.style_set_border(edge as u32, border.map_or(f32::NAN, |border| border as f32))
    }

    pub fn get_border(&self, edge: Edge) -> Result<f32> {
        if self.is_freed() {
            return Ok(f32::NAN);
        }
        self.style_get_border(edge as u32)
    }

    /// `setIsReferenceBaseline` (`yogaNodeSetIsReferenceBaselineChecked`).
    pub fn set_is_reference_baseline(&self, is_reference_baseline: bool) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        self.set_flag("yogaNodeSetIsReferenceBaselineChecked", 1, is_reference_baseline as u32)
    }

    pub fn is_reference_baseline(&self) -> Result<bool> {
        if self.is_freed() {
            return Ok(false);
        }
        let raw = self.live("yogaNodeIsReferenceBaselineChecked", false)?;
        Ok(unsafe { sys::YGNodeIsReferenceBaseline(raw) })
    }

    /// `setAlwaysFormsContainingBlock` (`yogaNodeSetAlwaysFormsContainingBlockChecked`).
    pub fn set_always_forms_containing_block(&self, always_forms_containing_block: bool) -> Result<()> {
        if self.is_freed() {
            return Ok(());
        }
        self.set_flag("yogaNodeSetAlwaysFormsContainingBlockChecked", 2, always_forms_containing_block as u32)
    }

    pub fn get_always_forms_containing_block(&self) -> Result<bool> {
        if self.is_freed() {
            return Ok(false);
        }
        let raw = self.live("yogaNodeGetAlwaysFormsContainingBlockChecked", false)?;
        Ok(unsafe { sys::YGNodeGetAlwaysFormsContainingBlock(raw) })
    }

    /// The node's type: `Text` while a [`Node::set_measure_func`] function is set, which makes Yoga round its
    /// size up to the pixel grid.
    pub fn get_node_type(&self) -> NodeType {
        self.raw().map_or(NodeType::Default, |raw| {
            NodeType::from_raw(unsafe { sys::YGNodeGetNodeType(raw) } as u32).unwrap_or(NodeType::Default)
        })
    }

    // ----- callbacks

    /// `setMeasureFunc(measureFunc)` (`yogaNodeSetMeasureFuncChecked`): only a node without children; the node
    /// becomes a text node. `None` unsets it.
    pub fn set_measure_func(&self, measure: Option<MeasureFunction>) -> Result<()> {
        const OPERATION: &str = "yogaNodeSetMeasureFuncChecked";
        if self.is_freed() {
            return Ok(());
        }
        assert_mutable()?;
        let enabled = measure.is_some();
        let raw = self.live(OPERATION, !enabled)?;
        let callback: sys::YGMeasureFunc = if enabled { Some(measure_trampoline) } else { None };
        check(OPERATION, call(|| unsafe { sys::otYogaNodeSetMeasureFunc(raw, callback, 0) }))?;
        drop(self.0.measure.replace(measure));
        Ok(())
    }

    /// `unsetMeasureFunc()`.
    pub fn unset_measure_func(&self) -> Result<()> {
        self.set_measure_func(None)
    }

    /// yoga.zig `yogaNodeSetNativeMeasureFunc`, the scene's text and editor measurement: the node stays a default
    /// node; `height_cap_only_clamps` tells Yoga's cache that an "at most" height only clamps the result, so a
    /// larger cap reuses a smaller result.
    pub fn set_native_measure_func(
        &self,
        measure: Option<MeasureFunction>,
        height_cap_only_clamps: bool,
    ) -> Result<()> {
        const OPERATION: &str = "yogaNodeSetNativeMeasureFunc";
        assert_mutable()?;
        let enabled = measure.is_some();
        let raw = self.live(OPERATION, !enabled)?;
        let callback: sys::YGMeasureFunc = if enabled { Some(measure_trampoline) } else { None };
        let status = call(|| unsafe { sys::otYogaNodeSetMeasureFunc(raw, callback, height_cap_only_clamps as u32) });
        check(OPERATION, status)?;
        unsafe { sys::YGNodeSetNodeType(raw, sys::YG_NODE_TYPE_DEFAULT) };
        drop(self.0.measure.replace(measure));
        Ok(())
    }

    /// `hasMeasureFunc()`.
    pub fn has_measure_func(&self) -> bool {
        self.raw().is_some_and(|raw| unsafe { sys::YGNodeHasMeasureFunc(raw) })
    }

    /// yoga.zig `yogaNodeInvalidateMeasure`: marks the node and its ancestors dirty after its measured content
    /// changed (no measure function precondition). Nothing on a poisoned node.
    pub fn invalidate_measure(&self) -> Result<()> {
        assert_mutable()?;
        let raw = self.live("yogaNodeInvalidateMeasure", true)?;
        if !self.0.poisoned.get() {
            call(|| unsafe { sys::otYogaNodeInvalidateMeasure(raw) });
        }
        Ok(())
    }

    /// yoga.zig `yogaNodeForgetMeasurements`: drops the node's cached sizes without dirtying anything.
    pub fn forget_measurements(&self) -> Result<()> {
        assert_mutable()?;
        let raw = self.live("yogaNodeForgetMeasurements", true)?;
        if !self.0.poisoned.get() {
            unsafe { sys::otYogaNodeForgetMeasurements(raw) };
        }
        Ok(())
    }

    /// `setDirtiedFunc(dirtiedFunc)` (`yogaNodeSetDirtiedFuncChecked`): called when Yoga marks the node dirty.
    /// `None` unsets it.
    pub fn set_dirtied_func(&self, dirtied: Option<DirtiedFunction>) -> Result<()> {
        const OPERATION: &str = "yogaNodeSetDirtiedFuncChecked";
        if self.is_freed() {
            return Ok(());
        }
        assert_mutable()?;
        let enabled = dirtied.is_some();
        let raw = self.live(OPERATION, !enabled)?;
        let callback: sys::YGDirtiedFunc = if enabled { Some(dirtied_trampoline) } else { None };
        unsafe { sys::YGNodeSetDirtiedFunc(raw, callback) };
        drop(self.0.dirtied.replace(dirtied));
        Ok(())
    }

    /// `unsetDirtiedFunc()`.
    pub fn unset_dirtied_func(&self) -> Result<()> {
        self.set_dirtied_func(None)
    }
}

#[cfg(test)]
mod tests;
