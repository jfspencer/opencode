//! The hand-written runtime under the generated types: JSON with JS semantics, `JSON.stringify`-exact output, and
//! the decode/encode contract every generated type implements.
//!
//! A [`Wire`] value is the canonical wire form of a TS schema value: decoding accepts exactly the JSON that TS's
//! `Schema.decode` accepts, and `encode(decode(json))` writes the same bytes as TS's
//! `JSON.stringify(Schema.encode(Schema.decode(JSON.parse(json))))`. See port/README.md for the mapping rules.

pub mod check;
pub mod codec;
pub mod hook;
pub mod json;
pub mod number;
pub mod out;

pub use codec::*;
pub use json::{Json, Object, ParseError, parse};
pub use out::Out;

use indexmap::IndexMap;

/// Why a value was rejected, with the path to the offending value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub path: Vec<String>,
    pub message: String,
}

impl Issue {
    pub fn new(message: impl Into<String>) -> Self {
        Self { path: Vec::new(), message: message.into() }
    }

    pub fn expected(what: &str, got: &Json) -> Self {
        Self::new(format!("expected {what}, got {}", got.kind()))
    }

    pub fn missing() -> Self {
        Self::new("missing key")
    }

    /// Prefixes the path with a key or index.
    pub fn at(mut self, segment: impl ToString) -> Self {
        self.path.insert(0, segment.to_string());
        self
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{} at {}", self.message, self.path.join("."))
        }
    }
}

impl std::error::Error for Issue {}

/// Reading JSON text into a wire type failed.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Parse(ParseError),
    Decode(Issue),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(e) => e.fmt(f),
            Error::Decode(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

/// A type with a JSON wire form.
pub trait Wire: Sized {
    fn decode(v: &Json) -> Result<Self, Issue>;
    fn encode(&self, out: &mut Out);
}

/// Parses and decodes JSON text.
pub fn from_str<T: Wire>(text: &str) -> Result<T, Error> {
    T::decode(&parse(text).map_err(Error::Parse)?).map_err(Error::Decode)
}

/// Writes a value as `JSON.stringify` would write its TS counterpart.
pub fn to_string<T: Wire>(v: &T) -> String {
    let mut out = Out::new();
    v.encode(&mut out);
    out.finish()
}

/// Decodes and re-encodes: the canonical wire form of `v`.
pub fn roundtrip<T: Wire>(v: &Json) -> Result<String, Issue> {
    T::decode(v).map(|t| to_string(&t))
}

impl Wire for String {
    fn decode(v: &Json) -> Result<Self, Issue> {
        match v {
            Json::String(s) => Ok(s.clone()),
            _ => Err(Issue::expected("string", v)),
        }
    }
    fn encode(&self, out: &mut Out) {
        out.string(self)
    }
}

impl Wire for bool {
    fn decode(v: &Json) -> Result<Self, Issue> {
        match v {
            Json::Bool(b) => Ok(*b),
            _ => Err(Issue::expected("boolean", v)),
        }
    }
    fn encode(&self, out: &mut Out) {
        out.bool(*self)
    }
}

/// Any JSON number (`Schema.Number` outside the JSON codec, which also takes the `Infinity` of `1e400`).
/// Non-finite values are written as `null`, as `JSON.stringify` does.
impl Wire for f64 {
    fn decode(v: &Json) -> Result<Self, Issue> {
        match v {
            Json::Number(n) => Ok(*n),
            _ => Err(Issue::expected("number", v)),
        }
    }
    fn encode(&self, out: &mut Out) {
        out.number(*self)
    }
}

/// `Schema.Int`: a safe integer. `1.0` and `1e2` are integers; `-0` reads as `0`.
impl Wire for i64 {
    fn decode(v: &Json) -> Result<Self, Issue> {
        let n = f64::decode(v)?;
        check::int(n)?;
        Ok(n as i64)
    }
    fn encode(&self, out: &mut Out) {
        out.int(*self)
    }
}

impl Wire for Json {
    fn decode(v: &Json) -> Result<Self, Issue> {
        Ok(v.clone())
    }
    fn encode(&self, out: &mut Out) {
        out.json(self)
    }
}

/// `null` (`Schema.Null`, and `Schema.Undefined`/`Void` under the JSON codec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Null;

impl Wire for Null {
    fn decode(v: &Json) -> Result<Self, Issue> {
        match v {
            Json::Null => Ok(Null),
            _ => Err(Issue::expected("null", v)),
        }
    }
    fn encode(&self, out: &mut Out) {
        out.null()
    }
}

/// `X | null`.
impl<T: Wire> Wire for Option<T> {
    fn decode(v: &Json) -> Result<Self, Issue> {
        match v {
            Json::Null => Ok(None),
            _ => T::decode(v).map(Some),
        }
    }
    fn encode(&self, out: &mut Out) {
        match self {
            None => out.null(),
            Some(v) => v.encode(out),
        }
    }
}

impl<T: Wire> Wire for Box<T> {
    fn decode(v: &Json) -> Result<Self, Issue> {
        T::decode(v).map(Box::new)
    }
    fn encode(&self, out: &mut Out) {
        (**self).encode(out)
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn decode(v: &Json) -> Result<Self, Issue> {
        array(v, T::decode)
    }
    fn encode(&self, out: &mut Out) {
        let mut a = out.array();
        for item in self {
            item.encode(a.item());
        }
    }
}

/// Decodes an array with an item decoder.
pub fn array<T>(v: &Json, item: impl Fn(&Json) -> Result<T, Issue>) -> Result<Vec<T>, Issue> {
    match v {
        Json::Array(items) => items.iter().enumerate().map(|(i, v)| item(v).map_err(|e| e.at(i))).collect(),
        _ => Err(Issue::expected("array", v)),
    }
}

/// `Schema.ArrayEnsure`: one item or an array of items. TS's encoder tries the single-item form first, so a
/// one-item array is written as the bare item.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArrayEnsure<T>(pub Vec<T>);

impl<T: Wire> Wire for ArrayEnsure<T> {
    fn decode(v: &Json) -> Result<Self, Issue> {
        array_ensure(v, T::decode)
    }
    fn encode(&self, out: &mut Out) {
        match self.0.as_slice() {
            [one] => one.encode(out),
            items => write_array(out, items, |out, v| v.encode(out)),
        }
    }
}

pub fn array_ensure<T>(v: &Json, item: impl Fn(&Json) -> Result<T, Issue>) -> Result<ArrayEnsure<T>, Issue> {
    match item(v) {
        Ok(one) => Ok(ArrayEnsure(vec![one])),
        Err(e) => match v {
            Json::Array(_) => array(v, item).map(ArrayEnsure),
            _ => Err(e),
        },
    }
}

/// Encodes a sequence with an item encoder.
pub fn write_array<T>(out: &mut Out, items: &[T], item: impl Fn(&mut Out, &T)) {
    let mut a = out.array();
    for v in items {
        item(a.item(), v);
    }
}

/// A record key: a string, or a branded string whose checks select which keys belong to the record.
pub trait Key: Sized {
    /// `None` when the key does not match the key schema; TS skips such keys rather than rejecting the object.
    fn from_key(key: &str) -> Option<Self>;
    fn as_key(&self) -> &str;
}

impl Key for String {
    fn from_key(key: &str) -> Option<Self> {
        Some(key.to_owned())
    }
    fn as_key(&self) -> &str {
        self
    }
}

/// `Schema.Record(K, V)`. Keeps input order; written in JS property order (array-index keys first).
#[derive(Debug, Clone, PartialEq)]
pub struct Record<K: Key + std::hash::Hash + Eq, V>(pub IndexMap<K, V>);

impl<K: Key + std::hash::Hash + Eq, V> Default for Record<K, V> {
    fn default() -> Self {
        Self(IndexMap::new())
    }
}

impl<K: Key + std::hash::Hash + Eq, V: Wire> Wire for Record<K, V> {
    fn decode(v: &Json) -> Result<Self, Issue> {
        record(v, V::decode)
    }
    fn encode(&self, out: &mut Out) {
        write_record(out, self, |out, v| v.encode(out))
    }
}

/// Decodes a record with a value decoder. Keys the key type rejects are skipped.
pub fn record<K: Key + std::hash::Hash + Eq, V>(
    v: &Json,
    value: impl Fn(&Json) -> Result<V, Issue>,
) -> Result<Record<K, V>, Issue> {
    let o = v.as_object().ok_or_else(|| Issue::expected("object", v))?;
    let mut map = IndexMap::with_capacity(o.len());
    for (k, v) in o {
        let Some(key) = K::from_key(k) else { continue };
        map.insert(key, value(v).map_err(|e| e.at(k))?);
    }
    Ok(Record(map))
}

pub fn write_record<K: Key + std::hash::Hash + Eq, V>(out: &mut Out, r: &Record<K, V>, value: impl Fn(&mut Out, &V)) {
    let by_key: IndexMap<&str, &V> = r.0.iter().map(|(k, v)| (k.as_key(), v)).collect();
    let mut o = out.object();
    for key in out::js_key_order(by_key.keys().copied()) {
        value(o.key(key), by_key[key]);
    }
}

/// Reads a struct's input object.
pub fn object(v: &Json) -> Result<&Object, Issue> {
    v.as_object().ok_or_else(|| Issue::expected("object", v))
}

/// A required key.
pub fn required<T>(o: &Object, key: &str, f: impl FnOnce(&Json) -> Result<T, Issue>) -> Result<T, Issue> {
    match o.get(key) {
        Some(v) => f(v).map_err(|e| e.at(key)),
        None => Err(Issue::missing().at(key)),
    }
}

/// An optional key: absent is `None`; a present value (even `null`) must decode.
pub fn optional<T>(o: &Object, key: &str, f: impl FnOnce(&Json) -> Result<T, Issue>) -> Result<Option<T>, Issue> {
    match o.get(key) {
        Some(v) => f(v).map(Some).map_err(|e| e.at(key)),
        None => Ok(None),
    }
}

/// `key?: never`: the key must be absent.
pub fn absent(o: &Object, key: &str) -> Result<(), Issue> {
    match o.get(key) {
        Some(v) => Err(Issue::expected("never", v).at(key)),
        None => Ok(()),
    }
}

/// An optional key under `Schema.catchDecoding(() => none)`: a value that does not decode counts as absent.
pub fn optional_lenient<T>(o: &Object, key: &str, f: impl FnOnce(&Json) -> Result<T, Issue>) -> Option<T> {
    o.get(key).and_then(|v| f(v).ok())
}

/// An optional key with a decoding default (JSON text of the wire value used when the key is absent).
pub fn defaulted<T>(o: &Object, key: &str, default: &str, f: impl Fn(&Json) -> Result<T, Issue>) -> Result<T, Issue> {
    match o.get(key) {
        Some(v) => f(v).map_err(|e| e.at(key)),
        None => f(&parse(default).expect("generated default is valid JSON")).map_err(|e| e.at(key)),
    }
}

/// The extra keys of a struct with an index signature. Like Effect: every key the key type accepts must have a
/// valid value, declared keys included; declared keys are then left to their fields.
pub fn rest<K: Key + std::hash::Hash + Eq, V>(
    o: &Object,
    declared: &[&str],
    value: impl Fn(&Json) -> Result<V, Issue>,
) -> Result<Record<K, V>, Issue> {
    let mut map = IndexMap::new();
    for (k, v) in o {
        let Some(key) = K::from_key(k) else { continue };
        let v = value(v).map_err(|e| e.at(k))?;
        if !declared.contains(&k.as_str()) {
            map.insert(key, v);
        }
    }
    Ok(Record(map))
}

/// Writes pre-rendered entries in JS property order.
pub fn write_entries(out: &mut Out, entries: Vec<(&str, String)>) {
    let by_key: IndexMap<&str, String> = entries.into_iter().collect();
    let mut o = out.object();
    for key in out::js_key_order(by_key.keys().copied()) {
        o.key(key).raw(&by_key[key]);
    }
}

/// A custom transformation reproduced by a hand-written [`hook`]: the input must decode as the wire type, then the
/// hook's output is decoded as the target type.
pub fn hooked<W, T>(
    v: &Json,
    wire: impl FnOnce(&Json) -> Result<W, Issue>,
    hook: fn(&Json) -> Result<Json, Issue>,
    target: impl FnOnce(&Json) -> Result<T, Issue>,
) -> Result<T, Issue> {
    wire(v)?;
    target(&hook(v)?)
}

/// Runs checks on a decoded value.
pub fn checked<T>(r: Result<T, Issue>, f: impl FnOnce(&T) -> Result<(), Issue>) -> Result<T, Issue> {
    let v = r?;
    f(&v)?;
    Ok(v)
}

/// `X | null` with an inline decoder for `X`.
pub fn nullable<T>(v: &Json, f: impl FnOnce(&Json) -> Result<T, Issue>) -> Result<Option<T>, Issue> {
    match v {
        Json::Null => Ok(None),
        _ => f(v).map(Some),
    }
}

/// `Schema.NumberFromString` with checks on the text and on the number.
pub fn number_string(
    v: &Json,
    wire: impl Fn(&str) -> Result<(), Issue>,
    num: impl Fn(&f64) -> Result<(), Issue>,
) -> Result<f64, Issue> {
    let n = NumberString::decode_with(v, wire)?;
    num(&n)?;
    Ok(n)
}

/// A literal value, for union pre-checks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lit {
    Str(&'static str),
    Num(f64),
    Bool(bool),
}

/// Whether `v` is an object whose `key` holds the literal (JS `===`, so `-0` matches `0`).
pub fn field_is(v: &Json, key: &str, lit: Lit) -> bool {
    match (v.as_object().and_then(|o| o.get(key)), lit) {
        (Some(Json::String(s)), Lit::Str(l)) => s == l,
        (Some(Json::Number(n)), Lit::Num(l)) => *n == l,
        (Some(Json::Bool(b)), Lit::Bool(l)) => *b == l,
        _ => false,
    }
}

/// Union decoding: members are tried in declaration order and the first that decodes wins (`anyOf`).
pub fn no_member(v: &Json, expected: &str) -> Issue {
    Issue::new(format!("expected {expected}, got {}", v.kind()))
}
