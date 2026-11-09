//! Unicode data for the RegExp engine, as JavaScriptCore has it (see tables.rs, generated from JSC itself).

use super::charset::CharSet;
use super::tables;
use std::sync::OnceLock;

pub const MAX_CODE_POINT: u32 = 0x10FFFF;
pub const MAX_CODE_UNIT: u32 = 0xFFFF;

/// Reads the base-64 varints the table generator writes.
fn varints(text: &str) -> impl Iterator<Item = u32> + '_ {
    let mut bytes = text.bytes();
    std::iter::from_fn(move || {
        let mut value = 0u32;
        let mut shift = 0;
        loop {
            let b = bytes.next()?;
            let digit = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' => 62,
                _ => 63,
            } as u32;
            value |= (digit & 31) << shift;
            shift += 5;
            if digit & 32 == 0 {
                return Some(value);
            }
        }
    })
}

fn unzigzag(n: u32) -> i64 {
    if n & 1 == 0 { (n / 2) as i64 } else { -((n as i64 + 1) / 2) }
}

pub fn decode_set(text: &str) -> CharSet {
    let mut ranges = Vec::new();
    let mut values = varints(text);
    let mut previous: Option<u32> = None;
    while let Some(first) = values.next() {
        let length = values.next().unwrap_or(0);
        let a = match previous {
            None => first,
            Some(p) => p + first + 2,
        };
        let b = a + length;
        ranges.push((a, b));
        previous = Some(b);
    }
    CharSet::from_ranges(ranges)
}

fn decode_strings(text: &str) -> Vec<Vec<u32>> {
    let mut out = Vec::new();
    let mut values = varints(text);
    let mut previous = 0i64;
    while let Some(count) = values.next() {
        let mut s = Vec::with_capacity(count as usize);
        for _ in 0..count {
            previous += unzigzag(values.next().unwrap_or(0));
            s.push(previous as u32);
        }
        out.push(s);
    }
    out
}

fn decode_pairs(text: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut values = varints(text);
    let mut previous = 0u32;
    while let Some(delta) = values.next() {
        let c = previous + delta;
        let rep = (c as i64 + unzigzag(values.next().unwrap_or(0))) as u32;
        out.push((c, rep));
        previous = c;
    }
    out
}

/// A case-insensitive equivalence: every character's class representative, and every class's members.
pub struct Folding {
    /// (character, representative) for characters whose class has two or more members, sorted by character.
    pairs: Vec<(u32, u32)>,
    /// (representative, members including the representative), sorted by representative.
    classes: Vec<(u32, Vec<u32>)>,
}

impl Folding {
    fn new(text: &str) -> Folding {
        let pairs = decode_pairs(text);
        let mut classes: Vec<(u32, Vec<u32>)> = Vec::new();
        let mut by_rep: Vec<(u32, u32)> = pairs.iter().map(|&(c, rep)| (rep, c)).collect();
        by_rep.sort_unstable();
        for (rep, c) in by_rep {
            match classes.last_mut() {
                Some((r, members)) if *r == rep => members.push(c),
                _ => classes.push((rep, vec![rep, c])),
            }
        }
        let mut all: Vec<(u32, u32)> = pairs.clone();
        for (rep, _) in &classes {
            all.push((*rep, *rep));
        }
        all.sort_unstable();
        Folding { pairs: all, classes }
    }

    /// The representative of c's class (c itself when it has no case partner).
    pub fn canon(&self, c: u32) -> u32 {
        match self.pairs.binary_search_by_key(&c, |&(c, _)| c) {
            Ok(i) => self.pairs[i].1,
            Err(_) => c,
        }
    }

    /// The members of c's class (None when c has no case partner).
    pub fn class_of(&self, c: u32) -> Option<&[u32]> {
        let rep = self.canon(c);
        let index = self.classes.binary_search_by_key(&rep, |(r, _)| *r).ok()?;
        Some(&self.classes[index].1)
    }

    /// Every character with a case partner in `set`, added to it.
    pub fn close(&self, set: &CharSet) -> CharSet {
        let size: u64 = set.ranges().iter().map(|&(a, b)| (b - a) as u64 + 1).sum();
        if size <= 64 {
            let mut extra = Vec::new();
            for &(a, b) in set.ranges() {
                for c in a..=b {
                    if let Some(members) = self.class_of(c) {
                        extra.extend(members.iter().filter(|&&m| !set.contains(m)).map(|&m| (m, m)));
                    }
                }
            }
            if extra.is_empty() {
                return set.clone();
            }
            extra.extend_from_slice(set.ranges());
            return CharSet::from_ranges(extra);
        }
        let mut extra = Vec::new();
        for (_, members) in &self.classes {
            if members.iter().any(|&m| set.contains(m)) {
                for &m in members {
                    if !set.contains(m) {
                        extra.push((m, m));
                    }
                }
            }
        }
        if extra.is_empty() {
            return set.clone();
        }
        extra.extend_from_slice(set.ranges());
        CharSet::from_ranges(extra)
    }

    /// The representatives of the members of `set` (MaybeSimpleCaseFolding of the v flag, with
    /// representatives standing in for simple case folding: both name one member per class).
    pub fn fold_set(&self, set: &CharSet) -> CharSet {
        let mut removed = Vec::new();
        let mut added = Vec::new();
        for (rep, members) in &self.classes {
            for &m in members {
                if m != *rep && set.contains(m) {
                    removed.push((m, m));
                    added.push((*rep, *rep));
                }
            }
        }
        if removed.is_empty() {
            return set.clone();
        }
        let mut out = set.subtract(&CharSet::from_ranges(removed));
        out.add_set(&CharSet::from_ranges(added));
        out
    }

    /// Every character that is not its class's representative.
    pub fn non_representatives(&self) -> CharSet {
        CharSet::from_ranges(
            self.classes
                .iter()
                .flat_map(|(rep, members)| members.iter().filter(move |&&m| m != *rep).map(|&m| (m, m)))
                .collect(),
        )
    }
}

pub fn ucs2_folding() -> &'static Folding {
    static FOLDING: OnceLock<Folding> = OnceLock::new();
    FOLDING.get_or_init(|| Folding::new(tables::UCS2_FOLD))
}

pub fn unicode_folding() -> &'static Folding {
    static FOLDING: OnceLock<Folding> = OnceLock::new();
    FOLDING.get_or_init(|| Folding::new(tables::UNICODE_FOLD))
}

pub fn space() -> &'static CharSet {
    static SET: OnceLock<CharSet> = OnceLock::new();
    SET.get_or_init(|| decode_set(tables::SPACE))
}

pub fn group_name_start() -> &'static CharSet {
    static SET: OnceLock<CharSet> = OnceLock::new();
    SET.get_or_init(|| decode_set(tables::GROUP_NAME_START))
}

pub fn group_name_part() -> &'static CharSet {
    static SET: OnceLock<CharSet> = OnceLock::new();
    SET.get_or_init(|| decode_set(tables::GROUP_NAME_PART))
}

pub fn digits() -> CharSet {
    CharSet::range('0' as u32, '9' as u32)
}

pub fn word() -> CharSet {
    CharSet::from_ranges(vec![(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)])
}

pub fn line_terminators() -> CharSet {
    CharSet::from_ranges(vec![(0x0A, 0x0A), (0x0D, 0x0D), (0x2028, 0x2029)])
}

pub fn is_line_terminator(c: u32) -> bool {
    c == 0x0A || c == 0x0D || c == 0x2028 || c == 0x2029
}

/// A property of strings: its single code points and its longer strings.
pub struct StringProperty {
    pub singles: CharSet,
    pub strings: Vec<Vec<u32>>,
}

pub enum Property {
    Set(CharSet),
    Strings(StringProperty),
}

fn lookup(table: &[(&str, u16)], name: &str) -> Option<CharSet> {
    let index = table.binary_search_by(|(n, _)| (*n).cmp(name)).ok()?;
    Some(decode_set(tables::SETS[table[index].1 as usize]))
}

/// `\p{name=value}` (name given) or `\p{value}`; `sets` allows properties of strings (v flag).
pub fn property(name: Option<&str>, value: &str, sets: bool) -> Option<Property> {
    match name {
        Some("General_Category" | "gc") => lookup(tables::GENERAL_CATEGORY, value).map(Property::Set),
        Some("Script" | "sc") => lookup(tables::SCRIPT, value).map(Property::Set),
        Some("Script_Extensions" | "scx") => lookup(tables::SCRIPT_EXTENSIONS, value).map(Property::Set),
        Some(_) => None,
        None => {
            if let Some(set) = lookup(tables::LONE, value) {
                return Some(Property::Set(set));
            }
            if !sets {
                return None;
            }
            let (_, single, strings) = tables::STRING_PROPERTIES.iter().find(|(n, _, _)| *n == value)?;
            Some(Property::Strings(StringProperty {
                singles: decode_set(tables::SETS[*single as usize]),
                strings: decode_strings(strings),
            }))
        }
    }
}
