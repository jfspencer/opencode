//! The hand-written runtime under the generated types: JSON with JS semantics, `JSON.stringify`-exact output, and
//! the decode/encode contract every generated type implements.
//!
//! A [`Wire`] value is the canonical wire form of a TS schema value: decoding accepts exactly the JSON that TS's
//! `Schema.decode` accepts, and `encode(decode(json))` writes the same bytes as TS's
//! `JSON.stringify(Schema.encode(Schema.decode(JSON.parse(json))))`. A rejection is an [`Issue`] whose text is
//! Effect's formatted message for the same input. See port/README.md for the mapping rules.
//!
//! Decoders follow Effect's parser step by step (effect/src/SchemaAST.ts), because the order of the steps decides
//! which problem is reported: a struct checks the input is an object, then its keys in declaration order (a
//! missing required key, or the value's first problem), then its index signature in JS key order; an array its
//! items in order; a refined value its checks in order, the first failing one reporting its message. The messages
//! themselves are computed by the generator from the TS AST and passed in (`te`: the type error, `Expected ...`).

pub mod check;
pub mod codec;
pub mod hook;
pub mod issue;
pub mod json;
pub mod number;
pub mod out;
pub mod union;

pub use codec::*;
pub use issue::{Entry, Issue, Segment};
pub use json::{Json, Object, ParseError, parse};
pub use out::Out;
pub use union::{Member, Site, any_of, one_of};

use indexmap::IndexMap;

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

// ---- primitives ---------------------------------------------------------------------------------------------

/// `Schema.String`; `te` is the type error (`Expected string`, or the schema's identifier).
pub fn string(v: &Json, te: &'static str) -> Result<String, Issue> {
    match v {
        Json::String(s) => Ok(s.clone()),
        _ => Err(Issue::new(te)),
    }
}

/// `Schema.Number`.
pub fn number(v: &Json, te: &'static str) -> Result<f64, Issue> {
    match v {
        Json::Number(n) => Ok(*n),
        _ => Err(Issue::new(te)),
    }
}

/// A number whose checks include `isInt` (a safe integer): `checks` run in declaration order on the number.
pub fn int(v: &Json, te: &'static str, checks: impl FnOnce(f64) -> Result<(), Issue>) -> Result<i64, Issue> {
    let n = number(v, te)?;
    checks(n)?;
    check::int(n, "Expected an integer")?;
    Ok(n as i64)
}

pub fn boolean(v: &Json, te: &'static str) -> Result<bool, Issue> {
    match v {
        Json::Bool(b) => Ok(*b),
        _ => Err(Issue::new(te)),
    }
}

pub fn null(v: &Json, te: &'static str) -> Result<Null, Issue> {
    match v {
        Json::Null => Ok(Null),
        _ => Err(Issue::new(te)),
    }
}

/// A literal, or the type error.
pub fn literal(v: &Json, lit: Lit, te: &'static str) -> Result<(), Issue> {
    if is_lit(v, lit) { Ok(()) } else { Err(Issue::new(te)) }
}

/// `Schema.Never` (and `Undefined`, which JSON cannot hold): always the type error.
pub fn never<T>(_: &Json, te: &'static str) -> Result<T, Issue> {
    Err(Issue::new(te))
}

impl Wire for String {
    fn decode(v: &Json) -> Result<Self, Issue> {
        string(v, "Expected string")
    }
    fn encode(&self, out: &mut Out) {
        out.string(self)
    }
}

impl Wire for bool {
    fn decode(v: &Json) -> Result<Self, Issue> {
        boolean(v, "Expected boolean")
    }
    fn encode(&self, out: &mut Out) {
        out.bool(*self)
    }
}

/// Any JSON number (`Schema.Number` outside the JSON codec, which also takes the `Infinity` of `1e400`).
/// Non-finite values are written as `null`, as `JSON.stringify` does.
impl Wire for f64 {
    fn decode(v: &Json) -> Result<Self, Issue> {
        number(v, "Expected number")
    }
    fn encode(&self, out: &mut Out) {
        out.number(*self)
    }
}

/// `Schema.Int`: a safe integer. `1.0` and `1e2` are integers; `-0` reads as `0`.
impl Wire for i64 {
    fn decode(v: &Json) -> Result<Self, Issue> {
        int(v, "Expected number", |_| Ok(()))
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
        null(v, "Expected null")
    }
    fn encode(&self, out: &mut Out) {
        out.null()
    }
}

/// `X | null`, for hand-written use: the generated code decodes nullable values through their union's [`Site`].
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

// ---- arrays -------------------------------------------------------------------------------------------------

impl<T: Wire> Wire for Vec<T> {
    fn decode(v: &Json) -> Result<Self, Issue> {
        array(v, "Expected array", T::decode)
    }
    fn encode(&self, out: &mut Out) {
        let mut a = out.array();
        for item in self {
            item.encode(a.item());
        }
    }
}

/// Decodes an array with an item decoder; the first failing item is reported.
pub fn array<T>(v: &Json, te: &'static str, item: impl Fn(&Json) -> Result<T, Issue>) -> Result<Vec<T>, Issue> {
    match v {
        Json::Array(items) => items.iter().enumerate().map(|(i, v)| item(v).map_err(|e| e.at(i))).collect(),
        _ => Err(Issue::new(te)),
    }
}

/// A tuple of `n` required elements followed by a rest of the same item type. Items are checked in order; the
/// first absent element is a missing key.
pub fn tuple<T>(
    v: &Json,
    te: &'static str,
    n: usize,
    missing: &'static str,
    item: impl Fn(&Json) -> Result<T, Issue>,
) -> Result<Vec<T>, Issue> {
    let items = array(v, te, item)?;
    if items.len() < n {
        return Err(Issue::new(missing).at(items.len()));
    }
    Ok(items)
}

/// `Schema.ArrayEnsure`: one item or an array of items. TS's encoder tries the single-item form first, so a
/// one-item array is written as the bare item. Decoding goes through the union's [`Site`] (generated).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArrayEnsure<T>(pub Vec<T>);

impl<T: Wire> Wire for ArrayEnsure<T> {
    fn decode(v: &Json) -> Result<Self, Issue> {
        match T::decode(v) {
            Ok(one) => Ok(ArrayEnsure(vec![one])),
            Err(e) => match v {
                Json::Array(_) => <Vec<T>>::decode(v).map(ArrayEnsure),
                _ => Err(e),
            },
        }
    }
    fn encode(&self, out: &mut Out) {
        match self.0.as_slice() {
            [one] => one.encode(out),
            items => write_array(out, items, |out, v| v.encode(out)),
        }
    }
}

/// HttpApi's `toCodecArrayFromSingle` around an array in a query: a single string is read as a one-item array;
/// anything that is neither a string nor an array fails with `te` (`Expected array | string`).
pub fn array_from_single<T>(
    v: &Json,
    te: &'static str,
    array: impl FnOnce(&Json) -> Result<T, Issue>,
) -> Result<T, Issue> {
    match v {
        Json::String(_) => array(&Json::Array(vec![v.clone()])),
        Json::Array(_) => array(v),
        _ => Err(Issue::new(te)),
    }
}

/// Encodes a sequence with an item encoder.
pub fn write_array<T>(out: &mut Out, items: &[T], item: impl Fn(&mut Out, &T)) {
    let mut a = out.array();
    for v in items {
        item(a.item(), v);
    }
}

// ---- records and structs -------------------------------------------------------------------------------------

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

/// `Schema.Record(K, V)`. Keys are kept in JS property order (array-index keys first), as TS decodes them.
#[derive(Debug, Clone, PartialEq)]
pub struct Record<K: Key + std::hash::Hash + Eq, V>(pub IndexMap<K, V>);

impl<K: Key + std::hash::Hash + Eq, V> Default for Record<K, V> {
    fn default() -> Self {
        Self(IndexMap::new())
    }
}

impl<K: Key + std::hash::Hash + Eq, V: Wire> Wire for Record<K, V> {
    fn decode(v: &Json) -> Result<Self, Issue> {
        record(v, "Expected object", V::decode)
    }
    fn encode(&self, out: &mut Out) {
        write_record(out, self, |out, v| v.encode(out))
    }
}

/// Decodes a record with a value decoder, in JS key order (`Object.keys`), so the first failing value is the one
/// TS reports. Keys the key type rejects are skipped.
pub fn record<K: Key + std::hash::Hash + Eq, V>(
    v: &Json,
    te: &'static str,
    value: impl Fn(&Json) -> Result<V, Issue>,
) -> Result<Record<K, V>, Issue> {
    let o = object(v, te)?;
    let mut map = IndexMap::with_capacity(o.len());
    for k in out::js_key_order(o.keys().map(String::as_str)) {
        let Some(key) = K::from_key(k) else { continue };
        map.insert(key, value(&o[k]).map_err(|e| e.at(k))?);
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
pub fn object<'a>(v: &'a Json, te: &'static str) -> Result<&'a Object, Issue> {
    v.as_object().ok_or_else(|| Issue::new(te))
}

/// A required key; `missing` is the missing-key message (`Missing key`, or the key's `messageMissingKey`).
pub fn required<T>(
    o: &Object,
    key: &str,
    missing: &'static str,
    f: impl FnOnce(&Json) -> Result<T, Issue>,
) -> Result<T, Issue> {
    match o.get(key) {
        Some(v) => f(v).map_err(|e| e.at(key)),
        None => Err(Issue::new(missing).at(key)),
    }
}

/// An optional key: absent is `None`; a present value (even `null`) must decode.
pub fn optional<T>(o: &Object, key: &str, f: impl FnOnce(&Json) -> Result<T, Issue>) -> Result<Option<T>, Issue> {
    match o.get(key) {
        Some(v) => f(v).map(Some).map_err(|e| e.at(key)),
        None => Ok(None),
    }
}

/// `key?: never`: the key must be absent (`te` is `Expected never`).
pub fn absent(o: &Object, key: &str, te: &'static str) -> Result<(), Issue> {
    match o.get(key) {
        Some(_) => Err(Issue::new(te).at(key)),
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

/// The extra keys of a struct with an index signature. Like Effect: after the declared fields, every key the key
/// type accepts must have a valid value (in JS key order), declared keys included; declared keys are then left to
/// their fields.
pub fn rest<K: Key + std::hash::Hash + Eq, V>(
    o: &Object,
    declared: &[&str],
    value: impl Fn(&Json) -> Result<V, Issue>,
) -> Result<Record<K, V>, Issue> {
    let mut map = IndexMap::new();
    for k in out::js_key_order(o.keys().map(String::as_str)) {
        let Some(key) = K::from_key(k) else { continue };
        let v = value(&o[k]).map_err(|e| e.at(k))?;
        if !declared.contains(&k) {
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

// ---- refinements and transformations --------------------------------------------------------------------------

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

/// HttpApi's flattened wire shape of a query or header property: the input must decode as the wire shape first
/// (its failures are the ones reported), then the value is decoded from the same input.
pub fn pre<W, T>(
    v: &Json,
    wire: impl FnOnce(&Json) -> Result<W, Issue>,
    value: impl FnOnce(&Json) -> Result<T, Issue>,
) -> Result<T, Issue> {
    wire(v)?;
    value(v)
}

/// Runs checks on a decoded value.
pub fn checked<T>(r: Result<T, Issue>, f: impl FnOnce(&T) -> Result<(), Issue>) -> Result<T, Issue> {
    let v = r?;
    f(&v)?;
    Ok(v)
}

/// `Schema.NumberFromString` with checks on the text and on the number.
pub fn number_string(
    v: &Json,
    te: &'static str,
    wire: impl Fn(&str) -> Result<(), Issue>,
    num: impl Fn(&f64) -> Result<(), Issue>,
) -> Result<f64, Issue> {
    let n = NumberString::decode_with(v, te, wire)?;
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

/// Whether `v` is the literal (JS `===`, so `-0` matches `0`).
pub fn is_lit(v: &Json, lit: Lit) -> bool {
    match (v, lit) {
        (Json::String(s), Lit::Str(l)) => s == l,
        (Json::Number(n), Lit::Num(l)) => *n == l,
        (Json::Bool(b), Lit::Bool(l)) => *b == l,
        _ => false,
    }
}
