//! JavaScript values as gray-matter hands them to opencode's frontmatter consumers (config/plugin/agent.ts,
//! command.ts, skill-file.ts), and the operations those consumers apply to them in TS.
//!
//! A frontmatter is not JSON: js-yaml makes `Date`s (`2001-12-14`), Node `Buffer`s (`!!binary`), arrays and objects
//! that aliases share or that contain themselves (`&a [*a]`); the `javascript` engine can make anything. What the
//! consumers do with such a value is exact JS and Effect Schema behaviour, reproduced here:
//! - [`keys`] and [`entries`]: `Object.keys` and the own enumerable properties a spread (`{ ...data }`) copies (a string
//!   contributes its UTF-16 code units, a `Buffer` its bytes, a `Date` nothing);
//! - [`get`]: a property read through the prototype chain (`Error.prototype.name` is `"Error"`), which is how Effect's
//!   `Struct` reads its fields;
//! - [`is_json`]: `Schema.Json`'s check (SchemaAST `isTree`): plain objects and arrays of finite numbers, strings,
//!   booleans and `null`, no holes, no cycles, shared nodes allowed;
//! - [`stringify`]: `JSON.parse(JSON.stringify(value))` (`toJSON` of `Date`, `Buffer` and `URL`, boxed primitives
//!   unwrapped, functions and `undefined` dropped, and JavaScriptCore's failures: a cycle or a BigInt is a
//!   `TypeError`, nesting past 40,000 levels or a result longer than 2^31 - 1 characters a `RangeError`);
//! - [`lower`]: the JSON a generated decoder sees for a value at a position of a known [`Shape`], or [`Reject`]
//!   where Effect rejects it for being what JSON cannot say (`undefined`, a function, a `Date` where a string
//!   belongs, a cycle under `Schema.Json`).
//!
//! Strings are Rust strings: a lone UTF-16 surrogate (a double-quoted YAML escape `"\uD800"`, half of a character a
//! string spread splits) is U+FFFD here. JS keeps the code unit, so its JSON output writes `\ud800` where the server's
//! writes the replacement character: carrying lone surrogates would need a UTF-16 string type through the server's
//! JSON, its wire types and every consumer (the wire layer's known gap, port/README.md), for strings that no
//! consumer accepts or rejects differently.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use indexmap::IndexMap;
use opencode_protocol::Json;
use opencode_protocol::wire::number::js_number_string;

/// A JavaScript value.
#[derive(Clone, Debug)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Rc<str>),
    /// A BigInt (only the `javascript` engine makes one).
    BigInt,
    /// A symbol (only the `javascript` engine makes one).
    Symbol,
    Object(Obj),
}

impl Value {
    pub fn string(text: &str) -> Value {
        Value::String(Rc::from(text))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Obj> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// `typeof value`.
    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::BigInt => "bigint",
            Value::Symbol => "symbol",
            Value::Object(o) if matches!(o.class(), Class::Function) => "function",
            Value::Object(_) => "object",
        }
    }

    /// `value || {}`'s test: whether the value is truthy.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0 && !n.is_nan(),
            Value::String(s) => !s.is_empty(),
            Value::BigInt => true,
            Value::Symbol | Value::Object(_) => true,
        }
    }
}

/// An object: shared by reference, as JS objects are, so aliases share nodes and cycles are possible.
#[derive(Clone)]
pub struct Obj(Rc<ObjData>);

struct ObjData {
    class: Class,
    /// Own string-keyed properties in creation order (array elements live in [`Class::Array`]).
    props: RefCell<Vec<Prop>>,
    /// The prototype when it is not the class's built-in one: another object (`Object.create(p)`), or `null`.
    proto: RefCell<Proto>,
    /// Set while a walk visits the object (cycle detection without a set).
    marked: Cell<bool>,
}

#[derive(Clone, Debug)]
enum Proto {
    Builtin,
    Null,
    Object(Obj),
}

#[derive(Clone, Debug)]
struct Prop {
    key: String,
    value: Value,
    enumerable: bool,
}

/// What kind of object a value is, as far as the consumers can tell.
#[derive(Debug)]
pub enum Class {
    /// An ordinary object.
    Plain,
    /// An array: its elements, `None` for a hole.
    Array(RefCell<Vec<Option<Value>>>),
    Function,
    /// A `Date` and its time value (NaN for an invalid date).
    Date(f64),
    /// A `Uint8Array`, or a Node `Buffer` (js-yaml's `!!binary`), whose `toJSON` is `{ type: "Buffer", data }`.
    Bytes { data: Vec<u8>, buffer: bool },
    /// A `String`, `Number` or `Boolean` object and its primitive.
    Boxed(Value),
    /// An `Error` and the `name` its prototype gives it (`Error`, `TypeError`, ...).
    Error(String),
    /// A `URL` and its `href` (its `toJSON`).
    Url(String),
    /// Any other built-in object (`Map`, `Set`, `RegExp`, a promise, ...): only its own properties show.
    Other,
}

impl std::fmt::Debug for Obj {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Obj({:?} @{:p})", self.0.class, Rc::as_ptr(&self.0))
    }
}

/// The objects of one value graph (a loaded frontmatter), emptied when it is dropped so that objects containing
/// themselves are freed.
#[derive(Default)]
pub struct Arena(RefCell<Vec<Obj>>);

impl Arena {
    pub fn object(&self, class: Class) -> Obj {
        let o = Obj::new(class);
        self.0.borrow_mut().push(o.clone());
        o
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        for o in self.0.borrow().iter() {
            o.0.props.borrow_mut().clear();
            *o.0.proto.borrow_mut() = Proto::Builtin;
            if let Class::Array(items) = &o.0.class {
                items.borrow_mut().clear();
            }
        }
    }
}

/// A value and the arena of its objects.
pub struct Graph {
    pub root: Value,
    _arena: Arena,
}

impl Graph {
    pub fn new(root: Value, arena: Arena) -> Graph {
        Graph { root, _arena: arena }
    }

    /// A value without objects of its own (`{}` included).
    pub fn of(root: Value) -> Graph {
        Graph { root, _arena: Arena::default() }
    }
}

/// Whether a key is an array index (a canonical numeric string below 2^32 - 1), which JS enumerates first.
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 || (key.len() > 1 && key.starts_with('0')) || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u64>().ok().filter(|n| *n < u32::MAX as u64).map(|n| n as u32)
}

impl Obj {
    pub fn new(class: Class) -> Obj {
        Obj(Rc::new(ObjData { class, props: RefCell::new(Vec::new()), proto: RefCell::new(Proto::Builtin), marked: Cell::new(false) }))
    }

    pub fn plain() -> Obj {
        Obj::new(Class::Plain)
    }

    pub fn array(items: Vec<Option<Value>>) -> Obj {
        Obj::new(Class::Array(RefCell::new(items)))
    }

    pub fn class(&self) -> &Class {
        &self.0.class
    }

    pub fn ptr(&self) -> *const () {
        Rc::as_ptr(&self.0) as *const ()
    }

    pub fn same(&self, other: &Obj) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    pub fn is_array(&self) -> bool {
        matches!(self.0.class, Class::Array(_))
    }

    /// `CreateDataProperty`: an own enumerable data property (replacing one of the same key in place).
    pub fn define(&self, key: &str, value: Value) {
        self.define_with(key, value, true);
    }

    pub fn define_with(&self, key: &str, value: Value, enumerable: bool) {
        if let (Class::Array(items), Some(index)) = (&self.0.class, array_index(key)) {
            let mut items = items.borrow_mut();
            let index = index as usize;
            if index >= items.len() {
                items.resize(index + 1, None);
            }
            items[index] = Some(value);
            return;
        }
        let mut props = self.0.props.borrow_mut();
        match props.iter_mut().find(|p| p.key == key) {
            Some(prop) => prop.value = value,
            None => props.push(Prop { key: key.to_owned(), value, enumerable }),
        }
    }

    /// An ordinary `object[key] = value` on a plain object: `__proto__` sets the prototype (an object or `null`) or
    /// is ignored, any other key defines a property.
    pub fn assign(&self, key: &str, value: Value) {
        if key == "__proto__" && matches!(self.0.class, Class::Plain) && self.own_prop(key).is_none() {
            match value {
                Value::Object(proto) => self.set_proto(Some(proto)),
                Value::Null => self.set_proto(None),
                _ => {}
            }
            return;
        }
        self.define(key, value);
    }

    /// `Object.setPrototypeOf(object, proto)` (`None` is `null`).
    pub fn set_proto(&self, proto: Option<Obj>) {
        *self.0.proto.borrow_mut() = match proto {
            Some(p) => Proto::Object(p),
            None => Proto::Null,
        };
    }

    fn own_prop(&self, key: &str) -> Option<Prop> {
        self.0.props.borrow().iter().find(|p| p.key == key).cloned()
    }

    /// The own property `key`: an array's or a byte array's element and `length`, a boxed string's characters,
    /// a function's `name` and `length` are own properties too.
    pub fn own(&self, key: &str) -> Option<Value> {
        match &self.0.class {
            Class::Array(items) => {
                if let Some(index) = array_index(key) {
                    return items.borrow().get(index as usize).cloned().flatten();
                }
                if key == "length" {
                    return Some(Value::Number(items.borrow().len() as f64));
                }
            }
            Class::Bytes { data, .. } => {
                if let Some(index) = array_index(key) {
                    return data.get(index as usize).map(|b| Value::Number(*b as f64));
                }
            }
            Class::Boxed(Value::String(s)) => {
                let units = utf16_units(s);
                if let Some(index) = array_index(key) {
                    return units.get(index as usize).cloned().map(|u| Value::string(&u));
                }
                if key == "length" {
                    return Some(Value::Number(units.len() as f64));
                }
            }
            _ => {}
        }
        self.own_prop(key).map(|p| p.value)
    }

    /// The own enumerable string-keyed properties in JS order (array indexes ascending, then creation order).
    pub fn entries(&self) -> Vec<(String, Value)> {
        let mut out: Vec<(String, Value)> = Vec::new();
        match &self.0.class {
            Class::Array(items) => {
                for (i, item) in items.borrow().iter().enumerate() {
                    if let Some(item) = item {
                        out.push((i.to_string(), item.clone()));
                    }
                }
            }
            Class::Bytes { data, .. } => out.extend(data.iter().enumerate().map(|(i, b)| (i.to_string(), Value::Number(*b as f64)))),
            Class::Boxed(Value::String(s)) => {
                out.extend(utf16_units(s).into_iter().enumerate().map(|(i, u)| (i.to_string(), Value::string(&u))))
            }
            _ => {}
        }
        let props = self.0.props.borrow();
        let mut indexed: Vec<(u32, &Prop)> = props.iter().filter(|p| p.enumerable).filter_map(|p| array_index(&p.key).map(|i| (i, p))).collect();
        indexed.sort_by_key(|(i, _)| *i);
        out.extend(indexed.into_iter().map(|(_, p)| (p.key.clone(), p.value.clone())));
        out.extend(props.iter().filter(|p| p.enumerable && array_index(&p.key).is_none()).map(|p| (p.key.clone(), p.value.clone())));
        out
    }

    /// The prototype a property read continues with (`None` at the end of the chain).
    fn next_proto(&self) -> Option<Obj> {
        match &*self.0.proto.borrow() {
            Proto::Object(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// Whether the chain ends in the class's built-in prototype (and so in `Object.prototype`).
    fn builtin_proto(&self) -> bool {
        matches!(&*self.0.proto.borrow(), Proto::Builtin)
    }

    /// Whether the object is a plain object for `Schema.Json`: its prototype is `Object.prototype` or `null`.
    fn json_plain(&self) -> bool {
        match &*self.0.proto.borrow() {
            Proto::Builtin => matches!(self.0.class, Class::Plain),
            Proto::Null => true,
            // A prototype object whose own prototype is null passes (another realm's Object.prototype).
            Proto::Object(p) => matches!(&*p.0.proto.borrow(), Proto::Null),
        }
    }
}

/// A string's UTF-16 code units, each as a string (a lone surrogate half is U+FFFD, see the module docs).
fn utf16_units(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in s.chars() {
        if c.len_utf16() == 2 {
            out.push("\u{FFFD}".to_owned());
            out.push("\u{FFFD}".to_owned());
        } else {
            out.push(c.to_string());
        }
    }
    out
}

/// The members of `Object.prototype`, which every ordinary object inherits.
pub const OBJECT_PROTOTYPE: [&str; 12] = [
    "toString",
    "toLocaleString",
    "valueOf",
    "hasOwnProperty",
    "propertyIsEnumerable",
    "isPrototypeOf",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
    "__proto__",
    "constructor",
];

/// `Object.keys(value)` (of a non-nullish value).
pub fn keys(value: &Value) -> Vec<String> {
    entries(value).into_iter().map(|(k, _)| k).collect()
}

/// The own enumerable properties of a value as `{ ...value }` copies them: a string's code units, an object's own
/// enumerable properties, nothing for other primitives.
pub fn entries(value: &Value) -> Vec<(String, Value)> {
    match value {
        Value::String(s) => utf16_units(s).into_iter().enumerate().map(|(i, u)| (i.to_string(), Value::string(&u))).collect(),
        Value::Object(o) => o.entries(),
        _ => Vec::new(),
    }
}

/// `{ ...value }` as a new plain object.
pub fn spread(value: &Value) -> Obj {
    let out = Obj::plain();
    for (key, value) in entries(value) {
        out.define(&key, value);
    }
    out
}

/// `value[key]` through the prototype chain; `None` when no object on the chain has the property (`key in value` is
/// false), `Some(Undefined)` when one holds `undefined`.
pub fn get(value: &Value, key: &str) -> Option<Value> {
    let Value::Object(object) = value else {
        return match value {
            Value::String(s) if key == "length" => Some(Value::Number(utf16_units(s).len() as f64)),
            Value::String(s) => array_index(key).and_then(|i| utf16_units(s).get(i as usize).map(|u| Value::string(u))),
            _ => None,
        };
    };
    let mut current = Some(object.clone());
    let mut depth = 0;
    while let Some(o) = current {
        if let Some(found) = o.own(key) {
            return Some(found);
        }
        if o.builtin_proto() {
            return builtin_member(&o.0.class, key);
        }
        current = o.next_proto();
        depth += 1;
        if depth > 10_000 {
            return None;
        }
    }
    None
}

/// A property a built-in prototype provides (only those a consumer could read: names of fields).
fn builtin_member(class: &Class, key: &str) -> Option<Value> {
    match (class, key) {
        (Class::Error(name), "name") => return Some(Value::string(name)),
        (Class::Error(_), "message") => return Some(Value::string("")),
        (Class::Bytes { data, .. }, "length") => return Some(Value::Number(data.len() as f64)),
        _ => {}
    }
    OBJECT_PROTOTYPE.contains(&key).then(|| Value::Object(Obj::new(Class::Function)))
}

/// How `ToPrimitive` finds a method: a built-in one, a property that is no function (skipped), a program's function.
enum Method {
    Builtin,
    NotCallable,
    Function,
}

fn method(object: &Obj, key: &str) -> Method {
    let mut current = object.clone();
    for _ in 0..10_000 {
        if let Some(prop) = current.own_prop(key) {
            return match prop.value {
                Value::Object(f) if matches!(f.class(), Class::Function) => Method::Function,
                _ => Method::NotCallable,
            };
        }
        let next = match &*current.0.proto.borrow() {
            Proto::Builtin => return Method::Builtin,
            Proto::Null => return Method::NotCallable,
            Proto::Object(p) => p.clone(),
        };
        current = next;
    }
    Method::NotCallable
}

/// Whether `String(value)` throws: a symbol, or an object whose `toString` and `valueOf` give no primitive (an own
/// `toString` that is no function and `Object.prototype.valueOf`, which returns the object; an array whose elements
/// throw, a cycle joining as `""`). A program's own `toString` is not called (see frontmatter_js.rs).
pub fn to_string_throws(value: &Value) -> bool {
    to_string_throws_in(value, &mut Vec::new())
}

fn to_string_throws_in(value: &Value, joining: &mut Vec<*const ()>) -> bool {
    let Value::Object(object) = value else { return matches!(value, Value::Symbol) };
    match method(object, "toString") {
        Method::Builtin => {
            let Class::Array(items) = object.class() else { return false };
            if !matches!(method(object, "join"), Method::Builtin) || joining.contains(&object.ptr()) {
                return false;
            }
            joining.push(object.ptr());
            let items = items.borrow().clone();
            let throws = items
                .iter()
                .flatten()
                .any(|item| stacker::maybe_grow(64 * 1024, 1024 * 1024, || to_string_throws_in(item, joining)));
            joining.pop();
            return throws;
        }
        Method::Function => return false,
        Method::NotCallable => {}
    }
    match method(object, "valueOf") {
        Method::Builtin => !matches!(object.class(), Class::Boxed(_) | Class::Date(_)),
        Method::Function => false,
        Method::NotCallable => true,
    }
}

/// Whether Effect's `Struct` and `Record` take the value: an object that is neither an array nor a function.
pub fn is_record(value: &Value) -> bool {
    matches!(value, Value::Object(o) if !matches!(o.class(), Class::Array(_) | Class::Function))
}

/// `Schema.Json`'s check: SchemaAST `isTree(value, isJsonLeaf)`, walking with an explicit stack.
pub fn is_json(value: &Value) -> bool {
    let leaf = |v: &Value| match v {
        Value::Null | Value::String(_) | Value::Bool(_) => true,
        Value::Number(n) => n.is_finite(),
        _ => false,
    };
    // `false` while on the current path, `true` once validated (a shared node is checked once).
    let mut cache: HashMap<*const (), bool> = HashMap::new();
    let mut stack: Vec<(Obj, Vec<Value>, usize)> = Vec::new();
    let mut current = value.clone();
    loop {
        match &current {
            Value::Object(o) => match cache.get(&o.ptr()) {
                Some(false) => return false,
                Some(true) => {}
                None => {
                    if !o.is_array() && !o.json_plain() {
                        return false;
                    }
                    cache.insert(o.ptr(), false);
                    let children = match o.class() {
                        // A hole reads as `undefined`.
                        Class::Array(items) => items.borrow().iter().map(|i| i.clone().unwrap_or(Value::Undefined)).collect(),
                        _ => o.entries().into_iter().map(|(_, v)| v).collect(),
                    };
                    stack.push((o.clone(), children, 0));
                }
            },
            other => {
                if !leaf(other) {
                    return false;
                }
            }
        }
        loop {
            let Some((object, children, index)) = stack.last_mut() else { return true };
            if *index < children.len() {
                current = children[*index].clone();
                *index += 1;
                break;
            }
            cache.insert(object.ptr(), true);
            stack.pop();
        }
    }
}

/// An exception JavaScript throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thrown {
    pub name: &'static str,
    pub message: String,
}

impl std::fmt::Display for Thrown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name, self.message)
    }
}

/// JavaScriptCore's `JSON.stringify` nesting limit (`maximumSideStackRecursion`).
const STRINGIFY_DEPTH: usize = 40_000;
/// JavaScriptCore's longest string (`String::MaxLength`).
const MAX_STRING_LENGTH: u64 = i32::MAX as u64;

fn cyclic() -> Thrown {
    Thrown { name: "TypeError", message: "JSON.stringify cannot serialize cyclic structures.".into() }
}

/// What `JSON.stringify` serializes for an object after `toJSON` and unboxing: a primitive, an array, or an object's
/// properties.
enum Serialized {
    Primitive(Value),
    Array(Obj),
    Object(Obj),
}

fn serialize_as(value: &Value) -> Result<Option<Serialized>, Thrown> {
    Ok(Some(match value {
        Value::Undefined | Value::Symbol => return Ok(None),
        Value::BigInt => return Err(Thrown { name: "TypeError", message: "JSON.stringify cannot serialize BigInt.".into() }),
        Value::Object(o) => match o.class() {
            Class::Function => return Ok(None),
            Class::Date(t) => match opencode_codemode::jsdate::to_iso_string(*t) {
                Some(iso) => Serialized::Primitive(Value::string(&iso)),
                None => Serialized::Primitive(Value::Null),
            },
            Class::Url(href) => Serialized::Primitive(Value::string(href)),
            Class::Bytes { data, buffer: true } => {
                let json = Obj::plain();
                json.define("type", Value::string("Buffer"));
                json.define("data", Value::Object(Obj::array(data.iter().map(|b| Some(Value::Number(*b as f64))).collect())));
                Serialized::Object(json)
            }
            Class::Boxed(Value::BigInt) => return serialize_as(&Value::BigInt),
            Class::Boxed(primitive) => Serialized::Primitive(primitive.clone()),
            Class::Array(_) => Serialized::Array(o.clone()),
            _ => Serialized::Object(o.clone()),
        },
        other => Serialized::Primitive(other.clone()),
    }))
}

/// The text length of a JSON string literal of `s`.
fn quoted_length(s: &str) -> u64 {
    2 + s
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf16() as u64,
        })
        .sum::<u64>()
}

/// `JSON.parse(JSON.stringify(value))`: `None` when `JSON.stringify` gives `undefined`.
pub fn stringify(value: &Value) -> Result<Option<Json>, Thrown> {
    stringify_at(value, 0)
}

/// [`stringify`] of a value `depth` objects deep in what `JSON.stringify` is given (its nesting limit counts them).
pub fn stringify_at(value: &Value, depth: usize) -> Result<Option<Json>, Thrown> {
    // The length first (shared nodes once), so an alias bomb fails as JavaScriptCore does instead of expanding.
    let mut memo = HashMap::new();
    if let Some((length, _)) = text_length(value, depth, &mut memo)? {
        if length > MAX_STRING_LENGTH {
            return Err(Thrown { name: "RangeError", message: "Out of memory".into() });
        }
    }
    to_json(value, depth)
}

fn too_deep() -> Thrown {
    Thrown { name: "RangeError", message: "Maximum call stack size exceeded.".into() }
}

/// The length of a value's JSON text and the height of its nesting (objects and arrays, itself included); `None` for
/// what `JSON.stringify` leaves out. A shared object is measured once: its text does not depend on where it appears.
fn text_length(value: &Value, depth: usize, memo: &mut HashMap<*const (), (u64, usize)>) -> Result<Option<(u64, usize)>, Thrown> {
    let Some(serialized) = serialize_as(value)? else { return Ok(None) };
    let primitive = |v: &Value| -> u64 {
        match v {
            Value::Null => 4,
            Value::Bool(true) => 4,
            Value::Bool(false) => 5,
            Value::Number(n) if n.is_finite() => js_number_string(*n).len() as u64,
            Value::Number(_) => 4,
            Value::String(s) => quoted_length(s),
            _ => 0,
        }
    };
    let object = match serialized {
        Serialized::Primitive(p) => return Ok(Some((primitive(&p), 0))),
        Serialized::Array(o) | Serialized::Object(o) => o,
    };
    if let Some(&(length, height)) = memo.get(&object.ptr()) {
        if depth + height > STRINGIFY_DEPTH {
            return Err(too_deep());
        }
        return Ok(Some((length, height)));
    }
    if object.0.marked.get() {
        return Err(cyclic());
    }
    if depth >= STRINGIFY_DEPTH {
        return Err(too_deep());
    }
    object.0.marked.set(true);
    let result = stacker::maybe_grow(64 * 1024, 1024 * 1024, || -> Result<(u64, usize), Thrown> {
        let mut total = 2u64;
        let mut height = 1usize;
        match object.class() {
            Class::Array(items) => {
                let items = items.borrow().clone();
                total += items.len().saturating_sub(1) as u64;
                for item in items {
                    match text_length(&item.unwrap_or(Value::Undefined), depth + 1, memo)? {
                        Some((length, h)) => {
                            total += length;
                            height = height.max(h + 1);
                        }
                        None => total += 4,
                    }
                }
            }
            _ => {
                let mut first = true;
                for (key, value) in object.entries() {
                    if let Some((length, h)) = text_length(&value, depth + 1, memo)? {
                        total += quoted_length(&key) + 1 + length + if first { 0 } else { 1 };
                        height = height.max(h + 1);
                        first = false;
                    }
                }
            }
        }
        Ok((total, height))
    });
    object.0.marked.set(false);
    let measured = result?;
    memo.insert(object.ptr(), measured);
    Ok(Some(measured))
}

fn to_json(value: &Value, depth: usize) -> Result<Option<Json>, Thrown> {
    let Some(serialized) = serialize_as(value)? else { return Ok(None) };
    let object = match serialized {
        Serialized::Primitive(p) => {
            return Ok(Some(match p {
                Value::Bool(b) => Json::Bool(b),
                // `-0` is written `0`; a non-finite number `null`.
                Value::Number(n) if n.is_finite() => Json::Number(if n == 0.0 { 0.0 } else { n }),
                Value::String(s) => Json::String(s.to_string()),
                _ => Json::Null,
            }));
        }
        Serialized::Array(o) | Serialized::Object(o) => o,
    };
    if object.0.marked.get() {
        return Err(cyclic());
    }
    if depth >= STRINGIFY_DEPTH {
        return Err(too_deep());
    }
    object.0.marked.set(true);
    let result = stacker::maybe_grow(64 * 1024, 1024 * 1024, || -> Result<Json, Thrown> {
        Ok(match object.class() {
            Class::Array(items) => {
                let items = items.borrow().clone();
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(to_json(&item.unwrap_or(Value::Undefined), depth + 1)?.unwrap_or(Json::Null));
                }
                Json::Array(out)
            }
            _ => {
                let mut out = IndexMap::new();
                for (key, value) in object.entries() {
                    if let Some(json) = to_json(&value, depth + 1)? {
                        out.insert(key, json);
                    }
                }
                Json::Object(out)
            }
        })
    });
    object.0.marked.set(false);
    result.map(Some)
}

/// A value converted from JSON (`JSON.parse`'s result).
pub fn from_json(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Number(*n),
        Json::String(s) => Value::string(s),
        Json::Array(items) => Value::Object(Obj::array(items.iter().map(|i| Some(from_json(i))).collect())),
        Json::Object(entries) => {
            let o = Obj::plain();
            for (k, v) in entries {
                // JSON.parse defines `__proto__` as an own property.
                o.define(k, from_json(v));
            }
            Value::Object(o)
        }
    }
}

/// Where Effect rejects a value for what it is (see [`lower`]); the decode fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reject;

/// A position in a schema whose decoding [`lower`] reproduces for values JSON cannot say.
pub enum Shape {
    /// A string, number, boolean or literal: an object, a function, `undefined`, a symbol or a BigInt fails it.
    Leaf,
    /// A string that `catchDecoding` drops when it does not decode (an agent's `color`).
    Caught,
    /// `Schema.Struct` (with opencode's `optional` keys: present but `undefined` fails): fields read through the
    /// prototype chain, other keys ignored.
    Struct(&'static [(&'static str, Shape)]),
    /// `Schema.Record(Schema.String, value)`: own enumerable properties.
    Record(&'static Shape),
    /// `Schema.Array(item)`: a hole reads as `undefined`.
    Array(&'static Shape),
    /// `Schema.Json`.
    Json,
    /// `ConfigModel.Selection`: a `provider/model` string or `{ providerID, model, variant }`.
    Selection,
}

const SELECTION: Shape = Shape::Struct(&[("providerID", Shape::Leaf), ("model", Shape::Leaf), ("variant", Shape::Leaf)]);

/// The JSON a generated decoder sees for `value` at a position of `shape`: `Ok(None)` for a field to leave out,
/// [`Reject`] where Effect fails on what JSON cannot say. Values JSON can say pass through for the decoder to check.
pub fn lower(value: &Value, shape: &Shape) -> Result<Option<Json>, Reject> {
    let mut budget = 1_000_000usize;
    lower_in(value, shape, &mut budget)
}

fn lower_in(value: &Value, shape: &Shape, budget: &mut usize) -> Result<Option<Json>, Reject> {
    if *budget == 0 {
        return Err(Reject);
    }
    *budget -= 1;
    let leaf = |v: &Value| -> Result<Option<Json>, Reject> {
        match v {
            Value::Null => Ok(Some(Json::Null)),
            Value::Bool(b) => Ok(Some(Json::Bool(*b))),
            Value::Number(n) => Ok(Some(Json::Number(*n))),
            Value::String(s) => Ok(Some(Json::String(s.to_string()))),
            _ => Err(Reject),
        }
    };
    match shape {
        Shape::Leaf => leaf(value),
        Shape::Caught => Ok(value.as_str().map(|s| Json::String(s.to_owned()))),
        Shape::Selection => match value {
            Value::String(_) => leaf(value),
            _ => lower_in(value, &SELECTION, budget),
        },
        Shape::Struct(fields) => {
            if !is_record(value) {
                return Err(Reject);
            }
            let mut out = IndexMap::new();
            for (name, field) in fields.iter() {
                let Some(found) = get(value, name) else { continue };
                if matches!(found, Value::Undefined) && !matches!(field, Shape::Caught) {
                    return Err(Reject);
                }
                if let Some(json) = stacker::maybe_grow(64 * 1024, 1024 * 1024, || lower_in(&found, field, budget))? {
                    out.insert((*name).to_owned(), json);
                }
            }
            Ok(Some(Json::Object(out)))
        }
        Shape::Record(item) => {
            if !is_record(value) {
                return Err(Reject);
            }
            let mut out = IndexMap::new();
            for (key, entry) in entries(value) {
                match lower_in(&entry, item, budget)? {
                    Some(json) => {
                        out.insert(key, json);
                    }
                    None => return Err(Reject),
                }
            }
            Ok(Some(Json::Object(out)))
        }
        Shape::Array(item) => {
            let Value::Object(o) = value else { return Err(Reject) };
            let Class::Array(items) = o.class() else { return Err(Reject) };
            let items = items.borrow().clone();
            let mut out = Vec::with_capacity(items.len());
            for entry in items {
                out.push(lower_in(&entry.unwrap_or(Value::Undefined), item, budget)?.ok_or(Reject)?);
            }
            Ok(Some(Json::Array(out)))
        }
        Shape::Json => {
            if !is_json(value) {
                return Err(Reject);
            }
            plain_json(value, budget).map(Some)
        }
    }
}

/// A value [`is_json`] accepts, as JSON (shared nodes copied, within the budget).
fn plain_json(value: &Value, budget: &mut usize) -> Result<Json, Reject> {
    if *budget == 0 {
        return Err(Reject);
    }
    *budget -= 1;
    Ok(match value {
        Value::Object(o) => stacker::maybe_grow(64 * 1024, 1024 * 1024, || -> Result<Json, Reject> {
            Ok(match o.class() {
                Class::Array(items) => {
                    let items = items.borrow().clone();
                    let mut out = Vec::with_capacity(items.len());
                    for item in items {
                        out.push(plain_json(&item.unwrap_or(Value::Undefined), budget)?);
                    }
                    Json::Array(out)
                }
                _ => {
                    let mut out = IndexMap::new();
                    for (key, entry) in o.entries() {
                        out.insert(key, plain_json(&entry, budget)?);
                    }
                    Json::Object(out)
                }
            })
        })?,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => Json::Number(*n),
        Value::String(s) => Json::String(s.to_string()),
        _ => Json::Null,
    })
}

/// A description of a value for the differential harness (`harness/frontmatter`): the graph with each object's class,
/// prototype and own enumerable properties, a repeated object as `{"ref": n}` (n counts objects in first-visit order).
pub fn describe(value: &Value) -> Json {
    let mut seen: HashMap<*const (), usize> = HashMap::new();
    describe_in(value, &mut seen)
}

fn tagged(tag: &str, rest: Vec<(&str, Json)>) -> Json {
    let mut out = IndexMap::new();
    out.insert("t".to_owned(), Json::String(tag.to_owned()));
    for (k, v) in rest {
        out.insert(k.to_owned(), v);
    }
    Json::Object(out)
}

fn describe_in(value: &Value, seen: &mut HashMap<*const (), usize>) -> Json {
    match value {
        Value::Undefined => tagged("undefined", vec![]),
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) if n.is_finite() && !(*n == 0.0 && n.is_sign_negative()) => Json::Number(*n),
        Value::Number(n) => tagged("number", vec![("v", Json::String(if *n == 0.0 { "-0".into() } else { js_number_string(*n) }))]),
        Value::String(s) => Json::String(s.to_string()),
        Value::BigInt => tagged("bigint", vec![]),
        Value::Symbol => tagged("symbol", vec![]),
        Value::Object(o) => {
            if let Some(id) = seen.get(&o.ptr()) {
                let mut out = IndexMap::new();
                out.insert("ref".to_owned(), Json::Number(*id as f64));
                return Json::Object(out);
            }
            let id = seen.len();
            seen.insert(o.ptr(), id);
            stacker::maybe_grow(64 * 1024, 1024 * 1024, || {
                let mut rest: Vec<(&str, Json)> = vec![("id", Json::Number(id as f64))];
                let class = match o.class() {
                    Class::Plain => "Plain",
                    Class::Array(items) => {
                        rest.push(("length", Json::Number(items.borrow().len() as f64)));
                        "Array"
                    }
                    Class::Function => "Function",
                    Class::Date(t) => {
                        rest.push(("time", describe_in(&Value::Number(*t), seen)));
                        "Date"
                    }
                    Class::Bytes { buffer: true, .. } => "Buffer",
                    Class::Bytes { buffer: false, .. } => "Uint8Array",
                    Class::Boxed(p) => {
                        rest.push(("value", describe_in(p, seen)));
                        "Boxed"
                    }
                    Class::Error(name) => {
                        rest.push(("name", Json::String(name.clone())));
                        "Error"
                    }
                    Class::Url(href) => {
                        rest.push(("href", Json::String(href.clone())));
                        "URL"
                    }
                    Class::Other => "Other",
                };
                let proto = match &*o.0.proto.borrow() {
                    Proto::Builtin => None,
                    Proto::Null => Some(Json::Null),
                    Proto::Object(p) => Some(describe_in(&Value::Object(p.clone()), seen)),
                };
                if let Some(proto) = proto {
                    rest.push(("proto", proto));
                }
                let props = o.entries().into_iter().map(|(k, v)| Json::Array(vec![Json::String(k), describe_in(&v, seen)])).collect();
                rest.push(("props", Json::Array(props)));
                tagged(class, rest)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_and_keys_follow_js() {
        let o = Obj::plain();
        o.define("b", Value::Number(1.0));
        o.define("2", Value::Number(2.0));
        o.define("a", Value::Number(3.0));
        o.define("1", Value::Number(4.0));
        assert_eq!(keys(&Value::Object(o)), ["1", "2", "b", "a"]);
        assert_eq!(keys(&Value::string("ab")), ["0", "1"]);
        assert!(keys(&Value::Number(1.0)).is_empty());
    }

    #[test]
    fn stringify_fails_like_javascriptcore() {
        let a = Obj::array(vec![]);
        if let Class::Array(items) = a.class() {
            items.borrow_mut().push(Some(Value::Object(a.clone())));
        }
        assert_eq!(stringify(&Value::Object(a)).unwrap_err().name, "TypeError");
        // Nested JSON values drop recursively: a thread with room for 40,000 levels.
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(|| {
                let arena = Arena::default();
                let mut deep = Value::Number(1.0);
                for _ in 0..STRINGIFY_DEPTH {
                    let array = arena.object(Class::Array(RefCell::new(vec![Some(deep)])));
                    deep = Value::Object(array);
                }
                assert!(stringify(&deep).is_ok());
                let deeper = Value::Object(arena.object(Class::Array(RefCell::new(vec![Some(deep)]))));
                assert_eq!(stringify(&deeper).unwrap_err().name, "RangeError");
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn json_check_accepts_shared_nodes_only() {
        let shared = Value::Object(Obj::plain());
        let o = Obj::plain();
        o.define("a", shared.clone());
        o.define("b", shared);
        assert!(is_json(&Value::Object(o.clone())));
        o.define("c", Value::Object(o.clone()));
        assert!(!is_json(&Value::Object(o)));
        assert!(!is_json(&Value::Object(Obj::array(vec![None]))));
        assert!(!is_json(&Value::Object(Obj::new(Class::Date(0.0)))));
    }
}
