//! Program values and objects (objects.ts): own properties plus a prototype link, with one `Kind` per built-in
//! object kind.

use crate::interp::collections::{OrderedMap, OrderedSet, WeakTable};
use crate::interp::cursor::Cursor;
use crate::interp::errors::{Fail, PendingThrow};
use crate::interp::runner::Reaction;
use crate::interp::{Ctx, Fut, Loc, R};
use crate::jsstr::JsStr;
use crate::parser::ast;
use indexmap::IndexMap;
use std::cell::{Cell, RefCell};
use std::hash::{Hash, Hasher};
use std::rc::{Rc, Weak};

/// The two confined symbols (`Symbol.iterator`, `Symbol.asyncIterator`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sym {
    Iterator,
    AsyncIterator,
}

impl Sym {
    pub fn description(self) -> &'static str {
        match self {
            Sym::Iterator => "Symbol.iterator",
            Sym::AsyncIterator => "Symbol.asyncIterator",
        }
    }
}

#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(JsStr),
    Sym(Sym),
    Obj(Obj),
    Tool(ToolRef),
}

impl Default for Value {
    fn default() -> Value {
        Value::Undefined
    }
}

impl Value {
    pub fn str(text: &str) -> Value {
        Value::Str(JsStr::from(text))
    }
    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }
    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }
    pub fn as_obj(&self) -> Option<&Obj> {
        match self {
            Value::Obj(obj) => Some(obj),
            _ => None,
        }
    }
    /// JS `===`.
    pub fn strict_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Num(a), Value::Num(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Sym(a), Value::Sym(b)) => a == b,
            (Value::Obj(a), Value::Obj(b)) => a.ptr_eq(b),
            (Value::Tool(a), Value::Tool(b)) => Rc::ptr_eq(&a.0, &b.0),
            _ => false,
        }
    }
    /// SameValueZero (`includes`, Map and Set keys).
    pub fn same_value_zero(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => a == b || (a.is_nan() && b.is_nan()),
            _ => self.strict_eq(other),
        }
    }
    /// SameValue (`Object.is`).
    pub fn same_value(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => {
                if a.is_nan() && b.is_nan() {
                    return true;
                }
                a == b && a.is_sign_negative() == b.is_sign_negative()
            }
            _ => self.strict_eq(other),
        }
    }
    /// JS truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => !(*n == 0.0 || n.is_nan()),
            Value::Str(s) => !s.is_empty(),
            Value::Sym(_) | Value::Obj(_) | Value::Tool(_) => true,
        }
    }
}

/// A key wrapper hashing by SameValueZero, for Map and Set.
#[derive(Clone)]
pub struct MapKey(pub Value);

impl PartialEq for MapKey {
    fn eq(&self, other: &MapKey) -> bool {
        self.0.same_value_zero(&other.0)
    }
}
impl Eq for MapKey {}
impl Hash for MapKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.0 {
            Value::Undefined => 0u8.hash(state),
            Value::Null => 1u8.hash(state),
            Value::Bool(b) => (2u8, *b).hash(state),
            Value::Num(n) => {
                3u8.hash(state);
                let n = if *n == 0.0 { 0.0 } else { *n };
                if n.is_nan() { u64::MAX.hash(state) } else { n.to_bits().hash(state) }
            }
            Value::Str(s) => (4u8, s).hash(state),
            Value::Sym(s) => (5u8, s).hash(state),
            Value::Obj(o) => (6u8, o.addr()).hash(state),
            Value::Tool(t) => (7u8, Rc::as_ptr(&t.0) as usize).hash(state),
        }
    }
}

/// A property key: an array index, a string, or one of the confined symbols. Canonical: a string that is an array
/// index (`"0"`, `"12"`, below 2^32 - 1) is always `Index`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PropKey {
    Index(u32),
    Str(JsStr),
    Sym(Sym),
}

pub const MAX_ARRAY_INDEX: u64 = 4_294_967_295;

/// parseArrayIndex: canonical decimal digits below 2^32 - 1.
pub fn parse_array_index(units: &[u16]) -> Option<u32> {
    if units.is_empty() || units.len() > 10 {
        return None;
    }
    if units[0] == b'0' as u16 {
        return if units.len() == 1 { Some(0) } else { None };
    }
    let mut value: u64 = 0;
    for &unit in units {
        if !(b'0' as u16..=b'9' as u16).contains(&unit) {
            return None;
        }
        value = value * 10 + (unit - b'0' as u16) as u64;
    }
    if value < MAX_ARRAY_INDEX { Some(value as u32) } else { None }
}

impl PropKey {
    pub fn from_js(text: JsStr) -> PropKey {
        match parse_array_index(text.units()) {
            Some(index) => PropKey::Index(index),
            None => PropKey::Str(text),
        }
    }
    pub fn from_str(text: &str) -> PropKey {
        PropKey::from_js(JsStr::from(text))
    }
    pub fn from_number(n: f64) -> PropKey {
        if n >= 0.0 && n < MAX_ARRAY_INDEX as f64 && n.fract() == 0.0 {
            // -0 is "0" as well.
            return PropKey::Index(n as u32);
        }
        PropKey::Str(JsStr::from(crate::numconv::to_string(n)))
    }
    /// String(key) for display.
    pub fn to_js(&self) -> JsStr {
        match self {
            PropKey::Index(index) => JsStr::from(index.to_string()),
            PropKey::Str(text) => text.clone(),
            PropKey::Sym(sym) => JsStr::from(format!("Symbol({})", sym.description())),
        }
    }
    pub fn is_symbol(&self) -> bool {
        matches!(self, PropKey::Sym(_))
    }
    pub fn eq_str(&self, text: &str) -> bool {
        match self {
            PropKey::Str(s) => s.eq_str(text),
            PropKey::Index(i) => i.to_string() == text,
            PropKey::Sym(_) => false,
        }
    }
}

impl From<&str> for PropKey {
    fn from(text: &str) -> PropKey {
        PropKey::from_str(text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attrs {
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
}

/// Ordinary assignment.
pub const DATA: Attrs = Attrs { writable: true, enumerable: true, configurable: true };
/// Built-in methods and `constructor`.
pub const HIDDEN: Attrs = Attrs { writable: true, enumerable: false, configurable: true };
/// Function `name` and `length`.
pub const READONLY: Attrs = Attrs { writable: false, enumerable: false, configurable: true };
/// Constants such as `Math.PI` and a constructor's `prototype`.
pub const FROZEN: Attrs = Attrs { writable: false, enumerable: false, configurable: false };

pub type Getter = Rc<dyn Fn(&Ctx, &Value) -> R<Value>>;
pub type Setter = Rc<dyn Fn(&Ctx, &Value, Value) -> R<()>>;

#[derive(Clone)]
pub enum Slot {
    Data { value: Value, writable: bool, enumerable: bool, configurable: bool },
    Accessor { get: Option<Getter>, set: Option<Setter>, enumerable: bool, configurable: bool },
}

impl Slot {
    pub fn data(value: Value, attrs: Attrs) -> Slot {
        Slot::Data { value, writable: attrs.writable, enumerable: attrs.enumerable, configurable: attrs.configurable }
    }
    pub fn enumerable(&self) -> bool {
        match self {
            Slot::Data { enumerable, .. } | Slot::Accessor { enumerable, .. } => *enumerable,
        }
    }
    pub fn configurable(&self) -> bool {
        match self {
            Slot::Data { configurable, .. } | Slot::Accessor { configurable, .. } => *configurable,
        }
    }
}

/// The result of a native function: a value at once, or interpreter work that may run program code.
pub enum NOut {
    Val(Value),
    Work(Fut<R<Value>>),
}

impl From<Value> for NOut {
    fn from(value: Value) -> NOut {
        NOut::Val(value)
    }
}

pub type NativeCall = Rc<dyn Fn(&Ctx, Value, Vec<Value>, Option<Loc>) -> R<NOut>>;
pub type NativeConstruct = Rc<dyn Fn(&Ctx, Vec<Value>, Obj, Option<Loc>) -> R<NOut>>;

pub struct NativeFn {
    pub call: std::cell::RefCell<NativeCall>,
    pub construct: std::cell::RefCell<Option<NativeConstruct>>,
    /// Whether callback sites admit this function.
    pub callback: bool,
}

/// A program function: the node it was created from and the scopes it closes over.
pub struct ProgramFn {
    pub node: Rc<ast::Function>,
    pub captured: RefCell<Vec<crate::interp::scope::Scope>>,
    pub arrow: bool,
}

pub enum Callable {
    Program(ProgramFn),
    Native(NativeFn),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

pub struct PromiseData {
    pub state: Cell<PromiseState>,
    /// The value, or the failure as thrown.
    pub result: RefCell<Option<Result<Value, Fail>>>,
    pub reactions: RefCell<Vec<Rc<Reaction>>>,
    pub handled: Cell<bool>,
    pub id: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GenState {
    SuspendedStart,
    SuspendedYield,
    Executing,
    AwaitingReturn,
    Draining,
    Completed,
}

#[derive(Clone)]
pub enum GenRequestKind {
    Next,
    Return,
    Throw,
}

impl GenRequestKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GenRequestKind::Next => "next",
            GenRequestKind::Return => "return",
            GenRequestKind::Throw => "throw",
        }
    }
}

#[derive(Clone)]
pub struct GenRequest {
    pub kind: GenRequestKind,
    pub value: Value,
}

pub struct GenData {
    pub asynchronous: bool,
    pub state: Cell<GenState>,
    /// The suspended (or not yet started) body.
    pub body: RefCell<Option<Fut<R<Value>>>>,
    /// An async generator's requests, oldest first, each with the functions that settle its promise.
    pub queue: RefCell<std::collections::VecDeque<(GenRequest, crate::interp::runner::Resolving)>>,
}

pub struct ArrData {
    /// Elements; `None` is a hole. Indexes from `items.len()` up to `len` are holes too.
    pub items: RefCell<Vec<Option<Value>>>,
    pub len: Cell<usize>,
    /// The attributes every live element shares; `seal` and `freeze` narrow them.
    pub elements: Cell<Attrs>,
}

impl ArrData {
    pub fn new(items: Vec<Option<Value>>) -> ArrData {
        let len = items.len();
        ArrData { items: RefCell::new(items), len: Cell::new(len), elements: Cell::new(DATA) }
    }
    pub fn len(&self) -> usize {
        self.len.get()
    }
    pub fn get(&self, index: usize) -> Option<Value> {
        self.items.borrow().get(index).cloned().flatten()
    }
    pub fn has(&self, index: usize) -> bool {
        matches!(self.items.borrow().get(index), Some(Some(_)))
    }
    /// The element or undefined (a hole reads as undefined).
    pub fn at(&self, index: usize) -> Value {
        self.get(index).unwrap_or(Value::Undefined)
    }
    pub fn set(&self, index: usize, value: Value) {
        let mut items = self.items.borrow_mut();
        if index >= items.len() {
            items.resize(index + 1, None);
        }
        items[index] = Some(value);
        drop(items);
        if index >= self.len.get() {
            self.len.set(index + 1);
        }
    }
    pub fn delete(&self, index: usize) {
        let mut items = self.items.borrow_mut();
        if index < items.len() {
            items[index] = None;
        }
    }
    pub fn set_len(&self, len: usize) {
        let mut items = self.items.borrow_mut();
        if len < items.len() {
            items.truncate(len);
        }
        self.len.set(len);
    }
    pub fn push(&self, value: Value) {
        let len = self.len.get();
        self.set(len, value);
    }
    /// The elements with holes kept, as a dense vector of options covering the full length.
    pub fn snapshot(&self) -> Vec<Option<Value>> {
        let mut items = self.items.borrow().clone();
        items.resize(self.len.get(), None);
        items
    }
    /// Appends the elements, holes kept, to `out`.
    pub fn extend_into(&self, out: &mut Vec<Option<Value>>) {
        let items = self.items.borrow();
        out.extend(items.iter().cloned());
        out.resize(out.len() + (self.len.get() - items.len()), None);
    }
    /// The elements with holes read as undefined (`[...items]` on a host array).
    pub fn values(&self) -> Vec<Value> {
        let items = self.items.borrow();
        let mut out: Vec<Value> = items.iter().map(|item| item.clone().unwrap_or(Value::Undefined)).collect();
        out.resize(self.len.get(), Value::Undefined);
        out
    }
    pub fn replace(&self, items: Vec<Option<Value>>) {
        self.len.set(items.len());
        *self.items.borrow_mut() = items;
    }
}

/// The bytes of a Uint8Array, shared by its subarray views.
pub struct BytesData {
    pub buffer: Rc<RefCell<Vec<u8>>>,
    pub offset: usize,
    pub length: usize,
}

impl BytesData {
    pub fn to_vec(&self) -> Vec<u8> {
        self.buffer.borrow()[self.offset..self.offset + self.length].to_vec()
    }
    pub fn get(&self, index: usize) -> Option<u8> {
        if index < self.length { Some(self.buffer.borrow()[self.offset + index]) } else { None }
    }
    pub fn set(&self, index: usize, value: u8) {
        if index < self.length {
            self.buffer.borrow_mut()[self.offset + index] = value;
        }
    }
}

pub enum Kind {
    Ordinary,
    Array(ArrData),
    /// [[ErrorData]]; `host` is the failure this error materialized from.
    Error(RefCell<Option<Fail>>),
    Function(Callable),
    /// A built-in namespace (`Math`, `JSON`, `console`) with its toStringTag.
    Namespace(&'static str),
    Arguments,
    Promise(PromiseData),
    Generator(GenData),
    Iterator(Rc<Cursor>),
    Date(Cell<f64>),
    RegExp(Rc<crate::jsregex::JsRegExp>),
    Map(RefCell<OrderedMap>),
    Set(RefCell<OrderedSet>),
    WeakMap(RefCell<WeakTable>),
    WeakSet(RefCell<WeakTable>),
    URLSearchParams(Rc<RefCell<crate::stdlib::url::SearchParams>>),
    Headers(RefCell<crate::stdlib::headers::HeaderList>),
    URL(Rc<RefCell<crate::stdlib::url::UrlState>>, Obj),
    Bytes(BytesData),
    TextDecoder { fatal: bool, ignore_bom: bool },
}

pub struct ObjInner {
    pub proto: RefCell<Option<Obj>>,
    pub props: RefCell<IndexMap<PropKey, Slot>>,
    /// [[Extensible]].
    pub extensible: Cell<bool>,
    pub kind: Kind,
}

#[derive(Clone)]
pub struct Obj(pub Rc<ObjInner>);

impl Obj {
    pub fn ptr_eq(&self, other: &Obj) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
    pub fn addr(&self) -> usize {
        Rc::as_ptr(&self.0) as *const u8 as usize
    }
    pub fn kind(&self) -> &Kind {
        &self.0.kind
    }
    pub fn proto(&self) -> Option<Obj> {
        self.0.proto.borrow().clone()
    }
    pub fn set_proto(&self, proto: Option<Obj>) {
        *self.0.proto.borrow_mut() = proto;
    }
    pub fn extensible(&self) -> bool {
        self.0.extensible.get()
    }
    pub fn downgrade(&self) -> Weak<ObjInner> {
        Rc::downgrade(&self.0)
    }
    pub fn array(&self) -> Option<&ArrData> {
        match &self.0.kind {
            Kind::Array(data) => Some(data),
            _ => None,
        }
    }
    pub fn is_array(&self) -> bool {
        matches!(self.0.kind, Kind::Array(_))
    }
    pub fn callable(&self) -> Option<&Callable> {
        match &self.0.kind {
            Kind::Function(callable) => Some(callable),
            _ => None,
        }
    }
    pub fn is_callable(&self) -> bool {
        matches!(self.0.kind, Kind::Function(_))
    }
    pub fn program_fn(&self) -> Option<&ProgramFn> {
        match &self.0.kind {
            Kind::Function(Callable::Program(program)) => Some(program),
            _ => None,
        }
    }
    pub fn native_fn(&self) -> Option<&NativeFn> {
        match &self.0.kind {
            Kind::Function(Callable::Native(native)) => Some(native),
            _ => None,
        }
    }
    pub fn promise(&self) -> Option<&PromiseData> {
        match &self.0.kind {
            Kind::Promise(data) => Some(data),
            _ => None,
        }
    }
    pub fn generator(&self) -> Option<&GenData> {
        match &self.0.kind {
            Kind::Generator(data) => Some(data),
            _ => None,
        }
    }
    pub fn bytes(&self) -> Option<&BytesData> {
        match &self.0.kind {
            Kind::Bytes(data) => Some(data),
            _ => None,
        }
    }
    pub fn date(&self) -> Option<&Cell<f64>> {
        match &self.0.kind {
            Kind::Date(time) => Some(time),
            _ => None,
        }
    }
    pub fn is_error(&self) -> bool {
        matches!(self.0.kind, Kind::Error(_))
    }
    pub fn is_promise(&self) -> bool {
        matches!(self.0.kind, Kind::Promise(_))
    }

    /// The class name `Object.prototype.toString` reports.
    pub fn tag(&self) -> &'static str {
        match &self.0.kind {
            Kind::Ordinary => "Object",
            Kind::Array(_) => "Array",
            Kind::Error(_) => "Error",
            Kind::Function(_) => "Function",
            Kind::Namespace(tag) => tag,
            Kind::Arguments => "Arguments",
            Kind::Promise(_) => "Promise",
            Kind::Generator(data) => {
                if data.asynchronous {
                    "AsyncGenerator"
                } else {
                    "Generator"
                }
            }
            Kind::Iterator(_) => "Iterator",
            Kind::Date(_) => "Date",
            Kind::RegExp(_) => "RegExp",
            Kind::Map(_) => "Map",
            Kind::Set(_) => "Set",
            Kind::WeakMap(_) => "WeakMap",
            Kind::WeakSet(_) => "WeakSet",
            Kind::URLSearchParams(_) => "URLSearchParams",
            Kind::Headers(_) => "Headers",
            Kind::URL(..) => "URL",
            Kind::Bytes(_) => "Uint8Array",
            Kind::TextDecoder { .. } => "Object",
        }
    }

    /// Interpreter machinery a program can hold but never inspect (Opaque in TS).
    pub fn is_opaque(&self) -> bool {
        matches!(self.0.kind, Kind::Function(_) | Kind::Promise(_) | Kind::Generator(_) | Kind::Iterator(_))
    }

    /// A built-in object around a host value (Wrapper in TS).
    pub fn is_wrapper(&self) -> bool {
        matches!(
            self.0.kind,
            Kind::Date(_)
                | Kind::RegExp(_)
                | Kind::Map(_)
                | Kind::Set(_)
                | Kind::WeakMap(_)
                | Kind::WeakSet(_)
                | Kind::URLSearchParams(_)
                | Kind::Headers(_)
                | Kind::URL(..)
                | Kind::Bytes(_)
        )
    }

    /// How diagnostics refer to a value of this kind.
    pub fn describe(&self) -> String {
        match &self.0.kind {
            Kind::Array(_) => "an array".into(),
            Kind::Function(_) => "a function".into(),
            Kind::Namespace(_) => "a data object".into(),
            Kind::Promise(_) => "an un-awaited Promise".into(),
            Kind::Generator(_) => "a generator".into(),
            Kind::Iterator(_) => "an iterator".into(),
            _ => {
                let tag = self.tag();
                if tag == "Object" {
                    return "a data object".into();
                }
                let article = if tag.starts_with(['A', 'E', 'I', 'O']) { "an" } else { "a" };
                format!("{article} {tag}")
            }
        }
    }
}

/// A reference into the tool tree (`tools`, `tools.ns`, `tools.ns.tool`): the same path gives the same value.
#[derive(Clone)]
pub struct ToolRef(pub Rc<ToolRefInner>);

pub struct ToolRefInner {
    pub path: Vec<JsStr>,
    pub children: RefCell<std::collections::HashMap<JsStr, ToolRef>>,
}

impl ToolRef {
    pub fn root() -> ToolRef {
        ToolRef(Rc::new(ToolRefInner { path: Vec::new(), children: RefCell::new(Default::default()) }))
    }
    pub fn path(&self) -> &[JsStr] {
        &self.0.path
    }
    pub fn child(&self, key: &JsStr) -> ToolRef {
        if let Some(known) = self.0.children.borrow().get(key) {
            return known.clone();
        }
        let mut path = self.0.path.clone();
        path.push(key.clone());
        let made = ToolRef(Rc::new(ToolRefInner { path, children: RefCell::new(Default::default()) }));
        self.0.children.borrow_mut().insert(key.clone(), made.clone());
        made
    }
}

/// Keeps a weak handle on every object of a run, so the cyclic garbage left at the end can be broken apart.
#[derive(Default)]
pub struct Registry {
    objects: RefCell<Vec<Weak<ObjInner>>>,
    prune_at: Cell<usize>,
}

impl Registry {
    pub fn add(&self, obj: &Obj) {
        let mut objects = self.objects.borrow_mut();
        objects.push(obj.downgrade());
        if objects.len() >= self.prune_at.get().max(1024) {
            objects.retain(|weak| weak.strong_count() > 0);
            self.prune_at.set(objects.len() * 2);
        }
    }
    /// Drops every live object's contents: properties, prototype, and kind state that holds other values.
    pub fn break_cycles(&self) {
        let objects = std::mem::take(&mut *self.objects.borrow_mut());
        for weak in objects {
            if let Some(inner) = weak.upgrade() {
                let props = std::mem::take(&mut *inner.props.borrow_mut());
                let proto = inner.proto.borrow_mut().take();
                let state = clear_kind(&inner.kind, true);
                drop((props, proto, state));
            }
        }
    }
}

/// Takes out whatever values a kind holds, so dropping them breaks cycles.
fn clear_kind(kind: &Kind, deep: bool) -> Box<dyn std::any::Any> {
    match kind {
        Kind::Array(data) => Box::new(std::mem::take(&mut *data.items.borrow_mut())),
        Kind::Error(host) => Box::new(host.borrow_mut().take()),
        Kind::Function(Callable::Program(program)) => Box::new(std::mem::take(&mut *program.captured.borrow_mut())),
        Kind::Function(Callable::Native(native)) => {
            let call = std::mem::replace(&mut *native.call.borrow_mut(), Rc::new(|_, _, _, _| Ok(NOut::Val(Value::Undefined))));
            let construct = native.construct.borrow_mut().take();
            Box::new((call, construct))
        }
        Kind::Promise(data) => {
            let result = data.result.borrow_mut().take();
            let reactions = std::mem::take(&mut *data.reactions.borrow_mut());
            Box::new((result, reactions))
        }
        Kind::Generator(data) => {
            let body = data.body.borrow_mut().take();
            let queue = std::mem::take(&mut *data.queue.borrow_mut());
            Box::new((body, queue))
        }
        Kind::Iterator(cursor) => {
            if deep {
                cursor.clear();
            }
            Box::new(())
        }
        Kind::Map(map) => Box::new(std::mem::take(&mut *map.borrow_mut())),
        Kind::Set(set) => Box::new(std::mem::take(&mut *set.borrow_mut())),
        Kind::WeakMap(table) | Kind::WeakSet(table) => Box::new(std::mem::take(&mut *table.borrow_mut())),
        _ => Box::new(()),
    }
}

thread_local! {
    /// Contents of dropped objects waiting to be dropped in turn, so freeing a deeply nested value never recurses.
    static DROPS: RefCell<Vec<Box<dyn std::any::Any>>> = const { RefCell::new(Vec::new()) };
    static DRAINING: Cell<bool> = const { Cell::new(false) };
}

impl Drop for ObjInner {
    fn drop(&mut self) {
        let props = std::mem::take(self.props.get_mut());
        let proto = self.proto.get_mut().take();
        let state = clear_kind(&self.kind, false);
        if props.is_empty() && proto.is_none() && state.is::<()>() {
            return;
        }
        let _ = DROPS.try_with(|drops| drops.borrow_mut().push(Box::new((props, proto, state))));
        if DRAINING.try_with(|draining| draining.replace(true)).unwrap_or(true) {
            return;
        }
        loop {
            let next = DROPS.try_with(|drops| drops.borrow_mut().pop()).ok().flatten();
            match next {
                Some(item) => drop(item),
                None => break,
            }
        }
        let _ = DRAINING.try_with(|draining| draining.set(false));
    }
}

pub fn pending_value(fail: &Fail) -> Option<Rc<PendingThrow>> {
    match fail {
        Fail::Pending(p) => Some(p.clone()),
        _ => None,
    }
}
