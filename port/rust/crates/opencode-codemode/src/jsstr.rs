//! JavaScript strings: immutable sequences of UTF-16 code units, which may hold lone surrogates.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[derive(Clone, Default)]
pub struct JsStr(Rc<[u16]>);

impl JsStr {
    pub fn new() -> JsStr {
        JsStr(Rc::from([] as [u16; 0]))
    }
    pub fn from_units(units: Vec<u16>) -> JsStr {
        JsStr(Rc::from(units))
    }
    pub fn from_slice(units: &[u16]) -> JsStr {
        JsStr(Rc::from(units))
    }
    pub fn units(&self) -> &[u16] {
        &self.0
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn ptr_eq(&self, other: &JsStr) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
    /// Lossy conversion: a lone surrogate becomes U+FFFD.
    pub fn to_rust_lossy(&self) -> String {
        String::from_utf16_lossy(&self.0)
    }
    /// Exact conversion when the string is well formed.
    pub fn to_rust(&self) -> Option<String> {
        String::from_utf16(&self.0).ok()
    }
    pub fn is_well_formed(&self) -> bool {
        let units = &self.0;
        let mut index = 0;
        while index < units.len() {
            let unit = units[index];
            if (0xD800..0xDC00).contains(&unit) {
                if index + 1 < units.len() && (0xDC00..0xE000).contains(&units[index + 1]) {
                    index += 2;
                    continue;
                }
                return false;
            }
            if (0xDC00..0xE000).contains(&unit) {
                return false;
            }
            index += 1;
        }
        true
    }
    pub fn slice(&self, start: usize, end: usize) -> JsStr {
        if start == 0 && end == self.len() {
            return self.clone();
        }
        JsStr::from_slice(&self.0[start..end])
    }
    pub fn concat(&self, other: &JsStr) -> JsStr {
        if self.is_empty() {
            return other.clone();
        }
        if other.is_empty() {
            return self.clone();
        }
        let mut units = Vec::with_capacity(self.len() + other.len());
        units.extend_from_slice(&self.0);
        units.extend_from_slice(&other.0);
        JsStr::from_units(units)
    }
    pub fn eq_str(&self, text: &str) -> bool {
        let mut units = text.encode_utf16();
        for unit in self.0.iter() {
            if units.next() != Some(*unit) {
                return false;
            }
        }
        units.next().is_none()
    }
    pub fn starts_with_str(&self, text: &str) -> bool {
        let mut index = 0;
        for unit in text.encode_utf16() {
            if self.0.get(index) != Some(&unit) {
                return false;
            }
            index += 1;
        }
        true
    }
    /// Code points, with lone surrogates as themselves (as `String.prototype[Symbol.iterator]` yields them).
    pub fn code_points(&self) -> CodePoints<'_> {
        CodePoints { units: &self.0, index: 0 }
    }
    pub fn index_of(&self, search: &[u16], from: usize) -> Option<usize> {
        let units = &self.0;
        if search.is_empty() {
            return if from <= units.len() { Some(from) } else { None };
        }
        if search.len() > units.len() {
            return None;
        }
        (from..=units.len() - search.len()).find(|&start| &units[start..start + search.len()] == search)
    }
}

pub struct CodePoints<'a> {
    units: &'a [u16],
    index: usize,
}

impl Iterator for CodePoints<'_> {
    /// (code point, start index, length in units)
    type Item = (u32, usize, usize);
    fn next(&mut self) -> Option<Self::Item> {
        let start = self.index;
        let first = *self.units.get(start)?;
        if (0xD800..0xDC00).contains(&first)
            && let Some(&second) = self.units.get(start + 1)
            && (0xDC00..0xE000).contains(&second)
        {
            self.index += 2;
            let point = 0x10000 + (((first as u32) - 0xD800) << 10) + ((second as u32) - 0xDC00);
            return Some((point, start, 2));
        }
        self.index += 1;
        Some((first as u32, start, 1))
    }
}

impl From<&str> for JsStr {
    fn from(text: &str) -> JsStr {
        JsStr::from_units(text.encode_utf16().collect())
    }
}

impl From<String> for JsStr {
    fn from(text: String) -> JsStr {
        JsStr::from(text.as_str())
    }
}

impl PartialEq for JsStr {
    fn eq(&self, other: &JsStr) -> bool {
        Rc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}
impl Eq for JsStr {}

impl Hash for JsStr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}

impl PartialOrd for JsStr {
    fn partial_cmp(&self, other: &JsStr) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
/// Code unit order, as JS `<` compares strings.
impl Ord for JsStr {
    fn cmp(&self, other: &JsStr) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl fmt::Display for JsStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rust_lossy())
    }
}

impl fmt::Debug for JsStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.to_rust_lossy())
    }
}

/// Builds a string from pieces without intermediate allocations.
#[derive(Default)]
pub struct JsStrBuf(pub Vec<u16>);

impl JsStrBuf {
    pub fn new() -> JsStrBuf {
        JsStrBuf(Vec::new())
    }
    pub fn push_str(&mut self, text: &str) {
        self.0.extend(text.encode_utf16())
    }
    pub fn push_js(&mut self, text: &JsStr) {
        self.0.extend_from_slice(text.units())
    }
    pub fn push_units(&mut self, units: &[u16]) {
        self.0.extend_from_slice(units)
    }
    pub fn push_unit(&mut self, unit: u16) {
        self.0.push(unit)
    }
    pub fn push_char(&mut self, ch: char) {
        let mut buf = [0u16; 2];
        self.0.extend_from_slice(ch.encode_utf16(&mut buf))
    }
    pub fn push_code_point(&mut self, point: u32) {
        if point >= 0x10000 {
            let value = point - 0x10000;
            self.0.push(0xD800 + (value >> 10) as u16);
            self.0.push(0xDC00 + (value & 0x3FF) as u16);
        } else {
            self.0.push(point as u16);
        }
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn finish(self) -> JsStr {
        JsStr::from_units(self.0)
    }
}
