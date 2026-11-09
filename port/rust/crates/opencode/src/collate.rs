//! `String.prototype.localeCompare` as the TS reference runs it: JavaScriptCore's `IntlCollator` over Bun's ICU
//! (78.3, Unicode 17) in its default locale, en-US, which has no tailoring. That is `ucol_strcoll` with ICU's root
//! collation at tertiary strength, variable characters non-ignorable, normalization on, no case level or case-first,
//! no numeric ordering. TS sorts paths, names and IDs with it (packages/core/src/plugin/vcs/git.ts, filesystem.ts,
//! skill.ts, tool/read-filesystem.ts, ...): use [`compare`] wherever TS calls `localeCompare`.
//!
//! The data is ICU's own (`generated/collation.bin`, dumped from ICU 78 by `port/rust/gen/src/collation.ts`): CLDR
//! root, which differs from the DUCET in Han (radical-stroke order, Unihan kRSUnicode), U+FFFE/U+FFFF, the U+FDD1
//! script-boundary contractions and the `l·` prefix mapping. The algorithm is a port of the code ICU runs for
//! `ucol_strcoll` on UTF-16 (`RuleBasedCollator::doCompare`, `FCDUTF16CollationIterator`, `CollationIterator`,
//! `CollationCompare::compareUpToQuaternary`), because ICU's results are not those of the UCA on the NFD string:
//! - the identical prefix is skipped, backing up over code units of the unsafe-backward set;
//! - the text is checked for FCD incrementally with CollationFCD's code-unit bit sets, and only segments that fail
//!   are normalized; a FCD segment is looked up as it is, so a precomposed character meets the canonical closure
//!   and a contraction only matches what the closure lists (`U+FDD1 U+1E3A` does not match `U+FDD1 L`, `U+FDD1
//!   U+AC00` is a contraction that the decomposed jamo do not match);
//! - turning around in the text (a failed discontiguous contraction backing up, a prefix mapping looking behind)
//!   re-checks FCD in the other direction, which can leave a segment unnormalized (`U+064A U+1E01B U+108D` keeps
//!   its marks in text order, unlike `x U+1E01B U+108D`);
//! - contractions match the longest contiguous suffix, then unblocked non-starters (discontiguous contractions,
//!   UCA S2.1.1-S2.1.3) with ICU's skipped-marks state; prefix mappings look behind; Hangul syllables are their
//!   jamo's elements; code points without data get the unassigned implicit primary.
//!
//! JavaScriptCore compares ASCII-only strings on a fast path of its own and ICU compares Latin text on
//! CollationFastLatin; both give the results of the full algorithm, which is all this ports.
//! `port/rust/crates/opencode/harness/collate/run.sh` checks the collation elements of every code point,
//! contraction and prefix mapping and of generated strings against ICU's, and the order against Bun's
//! `localeCompare`. Lone surrogates, which a Rust string cannot hold, are the only JS strings left out.

use std::cmp::Ordering;

#[cfg(test)]
use opencode_protocol::Json;

static TABLE: &[u8] = include_bytes!("generated/collation.bin");

/// Primaries at or above this are the unassigned implicit ones (and U+FFFD's, U+FFFF's); the table ranks the rest.
const UNASSIGNED_LEAD: u32 = 0xFE;
const COMMON: u32 = 0x0500_0500;
/// The first of the 11,172 Hangul syllables.
const HANGUL: u32 = 0xAC00;
const SECTIONS: usize = 9;

struct Table {
    ranges: &'static [u8],
    pairs: &'static [u8],
    elements: &'static [u8],
    contractions: &'static [u8],
    prefixes: &'static [u8],
    ccc: &'static [u8],
    decompositions: &'static [u8],
    pool: &'static [u8],
    bits: &'static [u8],
}

fn word(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
}

fn table() -> &'static Table {
    static PARSED: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
    PARSED.get_or_init(|| {
        assert_eq!(word(TABLE, 0), 0x3341_4355, "collation table magic");
        let mut offset = (SECTIONS + 1) * 4;
        let mut section = |index: usize| {
            let words = word(TABLE, index) as usize;
            let slice = &TABLE[offset..offset + words * 4];
            offset += words * 4;
            slice
        };
        Table {
            ranges: section(1),
            pairs: section(2),
            elements: section(3),
            contractions: section(4),
            prefixes: section(5),
            ccc: section(6),
            decompositions: section(7),
            pool: section(8),
            bits: section(9),
        }
    })
}

/// A collation element: the primary, and the secondary (high half) and masked tertiary (low half).
type Element = (u32, u32);

/// The index of the last record (of `width` words) whose first word, masked, is at most `key`.
fn floor(records: &[u8], width: usize, mask: u32, key: u32) -> Option<usize> {
    let count = records.len() / (width * 4);
    let (mut lo, mut hi) = (0, count);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if word(records, mid * width) & mask <= key { lo = mid + 1 } else { hi = mid }
    }
    lo.checked_sub(1)
}

/// The index of the first record (of `width` words) whose first word is at least `key`.
fn lower_bound(records: &[u8], width: usize, key: u32) -> usize {
    let (mut lo, mut hi) = (0, records.len() / (width * 4));
    while lo < hi {
        let mid = (lo + hi) / 2;
        if word(records, mid * width) < key { lo = mid + 1 } else { hi = mid }
    }
    lo
}

/// What ICU's trie holds for a code point (its CE32), as far as the root collation uses it.
#[derive(Clone, Copy)]
enum Ce32 {
    /// The code point's own elements (the ranges, or the unassigned implicit element).
    Plain(u32),
    /// Elements in the table (`first << 8 | count`): a contraction's or a prefix mapping's.
    Elements(u32),
    Contraction(u32),
    Prefix(u32),
    Hangul(u32),
}

/// The flags of a contraction CE32 (collation.h): every suffix starts with a non-starter, some suffix ends with one.
/// The root has no prefix with contractions, so CONTRACT_SINGLE_CP_NO_MATCH is never set.
#[derive(Clone, Copy)]
struct Flags {
    next_ccc: bool,
    trailing_ccc: bool,
}

const LCCC_BITS: usize = 0;
const TCCC_BITS: usize = 1;
const UNSAFE_BITS: usize = 2;

impl Table {
    fn push_elements(&self, packed: u32, out: &mut Vec<Element>) {
        let (start, len) = ((packed >> 8) as usize, (packed & 0xff) as usize);
        out.extend((start..start + len).map(|i| (word(self.elements, i * 2), word(self.elements, i * 2 + 1))));
    }

    /// The elements of a code point's own mapping.
    fn single(&self, cp: u32, out: &mut Vec<Element>) {
        if let Some(index) = floor(self.ranges, 3, 0xff_ffff, cp) {
            let (first, second, value) =
                (word(self.ranges, index * 3), word(self.ranges, index * 3 + 1), word(self.ranges, index * 3 + 2));
            if cp <= second & 0x1f_ffff {
                let pair = word(self.pairs, (second >> 21) as usize);
                match first >> 24 {
                    0 => out.push((value + (cp - (first & 0xff_ffff)), pair)),
                    1 => out.push((value, pair)),
                    _ => self.push_elements(value, out),
                }
                return;
            }
        }
        out.push((unassigned_primary(cp), COMMON));
    }

    fn data_ce32(&self, cp: u32) -> Ce32 {
        let (lo, hi) = self.contraction_entries(cp);
        if lo < hi {
            return Ce32::Contraction(cp);
        }
        let (lo, hi) = self.prefix_entries(cp);
        if lo < hi {
            return Ce32::Prefix(cp);
        }
        if (HANGUL..HANGUL + 11_172).contains(&cp) {
            return Ce32::Hangul(cp);
        }
        Ce32::Plain(cp)
    }

    fn contraction_entries(&self, cp: u32) -> (usize, usize) {
        let lo = lower_bound(self.contractions, 4, cp);
        (lo, lower_bound(self.contractions, 4, cp + 1))
    }

    fn prefix_entries(&self, cp: u32) -> (usize, usize) {
        let lo = lower_bound(self.prefixes, 3, cp);
        (lo, lower_bound(self.prefixes, 3, cp + 1))
    }

    /// The suffix (one or two code points) and the elements of a contraction or prefix entry.
    fn entry(&self, prefix: bool, index: usize) -> ([u32; 2], usize, u32) {
        if prefix {
            return ([word(self.prefixes, index * 3 + 1), 0], 1, word(self.prefixes, index * 3 + 2));
        }
        let (second, third) = (word(self.contractions, index * 4 + 1), word(self.contractions, index * 4 + 2));
        ([second, third], if third == 0 { 1 } else { 2 }, word(self.contractions, index * 4 + 3))
    }

    fn flags(&self, lo: usize, hi: usize) -> Flags {
        let suffixes = || (lo..hi).map(|i| self.entry(false, i));
        Flags {
            next_ccc: suffixes().all(|(s, _, _)| self.fcd16(s[0]) > 0xff),
            trailing_ccc: suffixes().any(|(s, len, _)| self.fcd16(s[len - 1]) > 0xff),
        }
    }

    fn combining_class(&self, cp: u32) -> u8 {
        match floor(self.ccc, 2, 0xff_ffff, cp) {
            Some(index) if cp <= word(self.ccc, index * 2 + 1) => (word(self.ccc, index * 2) >> 24) as u8,
            _ => 0,
        }
    }

    /// The full canonical decomposition of a code point other than a Hangul syllable, if it has one.
    fn decomposition(&self, cp: u32) -> Option<impl Iterator<Item = u32> + '_> {
        match floor(self.decompositions, 2, u32::MAX, cp) {
            Some(index) if word(self.decompositions, index * 2) == cp => {
                let packed = word(self.decompositions, index * 2 + 1);
                let (start, len) = ((packed >> 8) as usize, (packed & 0xff) as usize);
                Some((start..start + len).map(|i| word(self.pool, i)))
            }
            _ => None,
        }
    }

    /// `getFCD16`: the lead combining class (of the first code point of the NFD) and the trail one.
    fn fcd16(&self, cp: u32) -> u16 {
        if (HANGUL..HANGUL + 11_172).contains(&cp) {
            return 0;
        }
        let (first, last) = match self.decomposition(cp) {
            Some(mut mapping) => {
                let first = mapping.next().unwrap_or(cp);
                (first, mapping.last().unwrap_or(first))
            }
            None => (cp, cp),
        };
        (u16::from(self.combining_class(first)) << 8) | u16::from(self.combining_class(last))
    }

    fn bit(&self, set: usize, unit: u16) -> bool {
        word(self.bits, set * 2048 + (unit as usize >> 5)) >> (unit & 31) & 1 == 1
    }

    /// `CollationFCD::mayHaveLccc`.
    fn may_have_lccc(&self, cp: u32) -> bool {
        cp >= 0x300 && self.bit(LCCC_BITS, if cp > 0xffff { lead(cp) } else { cp as u16 })
    }

    /// NFD of UTF-16 text (`Normalizer2Impl::decompose`), as UTF-16.
    fn nfd(&self, text: &[u16]) -> Vec<u16> {
        let mut cps = Vec::with_capacity(text.len());
        for c in char::decode_utf16(text.iter().copied()) {
            let cp = c.map_or(0xFFFD, |c| c as u32);
            if (HANGUL..HANGUL + 11_172).contains(&cp) {
                let s = cp - HANGUL;
                cps.push(0x1100 + s / 588);
                cps.push(0x1161 + s % 588 / 28);
                if s % 28 != 0 {
                    cps.push(0x11A7 + s % 28);
                }
            } else {
                match self.decomposition(cp) {
                    Some(mapping) => cps.extend(mapping),
                    None => cps.push(cp),
                }
            }
        }
        let mut index = 0;
        while index < cps.len() {
            if self.combining_class(cps[index]) == 0 {
                index += 1;
                continue;
            }
            let mut end = index + 1;
            while end < cps.len() && self.combining_class(cps[end]) != 0 {
                end += 1;
            }
            cps[index..end].sort_by_key(|cp| self.combining_class(*cp));
            index = end;
        }
        let mut out = Vec::with_capacity(cps.len());
        for cp in cps {
            let mut units = [0u16; 2];
            out.extend_from_slice(char::from_u32(cp).unwrap_or('\u{FFFD}').encode_utf16(&mut units));
        }
        out
    }
}

/// `Collation::unassignedPrimaryFromCodePoint`: code point order after every assigned character.
fn unassigned_primary(cp: u32) -> u32 {
    let mut c = cp + 1;
    let mut primary = 2 + (c % 18) * 14;
    c /= 18;
    primary |= (2 + c % 254) << 8;
    c /= 254;
    primary |= (4 + c % 251) << 16;
    primary | (UNASSIGNED_LEAD << 24)
}

fn lead(cp: u32) -> u16 {
    (0xD7C0 + (cp >> 10)) as u16
}

fn is_lead(unit: u16) -> bool {
    (0xD800..0xDC00).contains(&unit)
}

fn is_trail(unit: u16) -> bool {
    (0xDC00..0xE000).contains(&unit)
}

fn supplementary(lead: u16, trail: u16) -> u32 {
    ((u32::from(lead) - 0xD800) << 10) + (u32::from(trail) - 0xDC00) + 0x10000
}

/// `CollationFCD::maybeTibetanCompositeVowel`.
fn maybe_tibetan_composite_vowel(unit: u16) -> bool {
    u32::from(unit) & 0x1f_ff01 == 0xf01
}

/// `CollationFCD::isFCD16OfTibetanCompositeVowel` (U+0F73, U+0F75, U+0F81).
fn is_tibetan_composite_vowel(fcd16: u16) -> bool {
    fcd16 == 0x8182 || fcd16 == 0x8184
}

/// A UCharsTrie over the suffixes of one code point's contractions, or over its prefixes (one code point each).
#[derive(Clone, Copy)]
struct Trie {
    prefix: bool,
    lo: usize,
    hi: usize,
    matched: [u32; 2],
    len: usize,
    stopped: bool,
}

/// A `UStringTrieResult` and the value of a match.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    NoMatch,
    NoValue,
    Value { packed: u32, more: bool },
}

impl Step {
    fn value(self) -> Option<u32> {
        match self {
            Step::Value { packed, .. } => Some(packed),
            _ => None,
        }
    }

    fn has_next(self) -> bool {
        matches!(self, Step::NoValue | Step::Value { more: true, .. })
    }
}

impl Trie {
    fn new(prefix: bool, (lo, hi): (usize, usize)) -> Trie {
        Trie { prefix, lo, hi, matched: [0; 2], len: 0, stopped: false }
    }

    fn reset(&mut self) {
        self.len = 0;
        self.stopped = false;
    }

    /// `nextForCodePoint` (`firstForCodePoint` from the root).
    fn next(&mut self, table: &Table, cp: u32) -> Step {
        if self.stopped || self.len == 2 {
            self.stopped = true;
            return Step::NoMatch;
        }
        self.matched[self.len] = cp;
        self.len += 1;
        let (mut value, mut more) = (None, false);
        for index in self.lo..self.hi {
            let (suffix, len, packed) = table.entry(self.prefix, index);
            if len >= self.len && suffix[..self.len] == self.matched[..self.len] {
                if len == self.len {
                    value = Some(packed);
                } else {
                    more = true;
                }
            }
        }
        match value {
            Some(packed) => Step::Value { packed, more },
            None if more => Step::NoValue,
            None => {
                self.stopped = true;
                Step::NoMatch
            }
        }
    }
}

/// `SkippedState`: the combining marks a discontiguous contraction skipped, read again after it.
#[derive(Default)]
struct Skipped {
    old: Vec<u32>,
    new: Vec<u32>,
    /// Index in `old`, or `old.len()` plus the code points read beyond it.
    pos: usize,
    skip_length_at_match: usize,
    state: Option<Trie>,
}

impl Skipped {
    fn is_empty(&self) -> bool {
        self.old.is_empty()
    }

    fn has_next(&self) -> bool {
        self.pos < self.old.len()
    }

    fn next(&mut self) -> u32 {
        self.pos += 1;
        self.old[self.pos - 1]
    }

    /// Returns how many of the `n` code points must be backed out of the normal input.
    fn backward_num_code_points(&mut self, n: usize) -> usize {
        let length = self.old.len();
        if self.pos > length {
            let beyond = self.pos - length;
            if beyond >= n {
                self.pos -= n;
                return n;
            }
            self.pos = length.saturating_sub(n - beyond);
            return beyond;
        }
        self.pos = self.pos.saturating_sub(n);
        0
    }

    fn replace_match(&mut self) {
        let consumed = self.pos.min(self.old.len());
        self.old.splice(0..consumed, self.new[..self.skip_length_at_match].iter().copied());
        self.pos = 0;
    }
}

/// `FCDUTF16CollationIterator` with `CollationIterator`'s forward iteration, over `raw[pos..]` (with the text before
/// `pos` visible to prefix matching). `start`, `pos` and `limit` index `normalized` while `in_normalized`.
struct Iter<'a> {
    table: &'static Table,
    raw: &'a [u16],
    normalized: Vec<u16>,
    in_normalized: bool,
    start: usize,
    pos: usize,
    limit: usize,
    segment_start: usize,
    segment_limit: usize,
    check_dir: i8,
    ces: Vec<Element>,
    index: usize,
    skipped: Option<Skipped>,
}

impl<'a> Iter<'a> {
    fn new(raw: &'a [u16], pos: usize) -> Iter<'a> {
        Iter {
            table: table(),
            raw,
            normalized: Vec::new(),
            in_normalized: false,
            start: 0,
            pos,
            limit: raw.len(),
            segment_start: pos,
            segment_limit: 0,
            check_dir: 1,
            ces: Vec::with_capacity(raw.len() + 4),
            index: 0,
            skipped: None,
        }
    }

    fn unit(&self, index: usize) -> u16 {
        if self.in_normalized { self.normalized[index] } else { self.raw[index] }
    }

    /// `start == segmentStart` (a pointer into the normalized buffer is never a pointer into the text).
    fn start_is_segment_start(&self) -> bool {
        !self.in_normalized && self.start == self.segment_start
    }

    fn has_lccc(&self, unit: u16) -> bool {
        unit >= 0x300 && self.table.bit(LCCC_BITS, unit)
    }

    fn has_tccc(&self, unit: u16) -> bool {
        unit >= 0xc0 && self.table.bit(TCCC_BITS, unit)
    }

    fn next_ce(&mut self) -> Option<Element> {
        if self.index == self.ces.len() {
            let (_, ce32) = self.handle_next_ce32()?;
            self.append(ce32, true);
        }
        self.index += 1;
        Some(self.ces[self.index - 1])
    }

    /// `handleNextCE32`, with the lead surrogate's trail read as `LEAD_SURROGATE_TAG` does.
    fn handle_next_ce32(&mut self) -> Option<(u32, Ce32)> {
        let unit = loop {
            if self.check_dir > 0 {
                if self.pos == self.limit {
                    return None;
                }
                let mut unit = self.unit(self.pos);
                self.pos += 1;
                if self.has_tccc(unit)
                    && (maybe_tibetan_composite_vowel(unit) || (self.pos != self.limit && self.has_lccc(self.unit(self.pos))))
                {
                    self.pos -= 1;
                    self.next_segment();
                    unit = self.unit(self.pos);
                    self.pos += 1;
                }
                break unit;
            } else if self.check_dir == 0 && self.pos != self.limit {
                self.pos += 1;
                break self.unit(self.pos - 1);
            } else {
                self.switch_to_forward();
            }
        };
        let mut cp = u32::from(unit);
        if is_lead(unit) && self.pos != self.limit && is_trail(self.unit(self.pos)) {
            cp = supplementary(unit, self.unit(self.pos));
            self.pos += 1;
        }
        Some((cp, self.table.data_ce32(cp)))
    }

    fn next_code_point(&mut self) -> Option<u32> {
        let unit = loop {
            if self.check_dir > 0 {
                if self.pos == self.limit {
                    return None;
                }
                let mut unit = self.unit(self.pos);
                self.pos += 1;
                if self.has_tccc(unit)
                    && (maybe_tibetan_composite_vowel(unit) || (self.pos != self.limit && self.has_lccc(self.unit(self.pos))))
                {
                    self.pos -= 1;
                    self.next_segment();
                    unit = self.unit(self.pos);
                    self.pos += 1;
                }
                break unit;
            } else if self.check_dir == 0 && self.pos != self.limit {
                self.pos += 1;
                break self.unit(self.pos - 1);
            } else {
                self.switch_to_forward();
            }
        };
        if is_lead(unit) && self.pos != self.limit && is_trail(self.unit(self.pos)) {
            self.pos += 1;
            return Some(supplementary(unit, self.unit(self.pos - 1)));
        }
        Some(u32::from(unit))
    }

    fn previous_code_point(&mut self) -> Option<u32> {
        let unit = loop {
            if self.check_dir < 0 {
                if self.pos == self.start {
                    return None;
                }
                self.pos -= 1;
                let mut unit = self.unit(self.pos);
                if self.has_lccc(unit)
                    && (maybe_tibetan_composite_vowel(unit) || (self.pos != self.start && self.has_tccc(self.unit(self.pos - 1))))
                {
                    self.pos += 1;
                    self.previous_segment();
                    self.pos -= 1;
                    unit = self.unit(self.pos);
                }
                break unit;
            } else if self.check_dir == 0 && self.pos != self.start {
                self.pos -= 1;
                break self.unit(self.pos);
            } else {
                self.switch_to_backward();
            }
        };
        if is_trail(unit) && self.pos != self.start && is_lead(self.unit(self.pos - 1)) {
            self.pos -= 1;
            return Some(supplementary(self.unit(self.pos), unit));
        }
        Some(u32::from(unit))
    }

    fn forward_num_code_points(&mut self, mut n: usize) {
        while n > 0 && self.next_code_point().is_some() {
            n -= 1;
        }
    }

    fn backward_num_code_points(&mut self, mut n: usize) {
        while n > 0 && self.previous_code_point().is_some() {
            n -= 1;
        }
    }

    fn switch_to_forward(&mut self) {
        if self.check_dir < 0 {
            // Turn around from backward checking.
            self.start = self.pos;
            self.segment_start = self.pos;
            if self.pos == self.segment_limit {
                self.limit = self.raw.len();
                self.check_dir = 1;
            } else {
                self.check_dir = 0;
            }
        } else {
            // Reached the end of the FCD segment; a normalized one continues in the text after it.
            if !self.start_is_segment_start() {
                self.in_normalized = false;
                self.pos = self.segment_limit;
                self.start = self.segment_limit;
                self.segment_start = self.segment_limit;
            }
            self.limit = self.raw.len();
            self.check_dir = 1;
        }
    }

    fn switch_to_backward(&mut self) {
        if self.check_dir > 0 {
            // Turn around from forward checking.
            self.limit = self.pos;
            self.segment_limit = self.pos;
            if self.pos == self.segment_start {
                self.start = 0;
                self.check_dir = -1;
            } else {
                self.check_dir = 0;
            }
        } else {
            // Reached the start of the FCD segment; a normalized one continues in the text before it.
            if !self.start_is_segment_start() {
                self.in_normalized = false;
                self.pos = self.segment_start;
                self.limit = self.segment_start;
                self.segment_limit = self.segment_start;
            }
            self.start = 0;
            self.check_dir = -1;
        }
    }

    /// `nfcImpl.nextFCD16(p, rawLimit)` over the text.
    fn next_fcd16(&self, p: &mut usize) -> u16 {
        let unit = self.raw[*p];
        *p += 1;
        let mut cp = u32::from(unit);
        if is_lead(unit) && *p != self.raw.len() && is_trail(self.raw[*p]) {
            cp = supplementary(unit, self.raw[*p]);
            *p += 1;
        }
        self.table.fcd16(cp)
    }

    /// `nfcImpl.previousFCD16(rawStart, p)` over the text.
    fn previous_fcd16(&self, p: &mut usize) -> u16 {
        *p -= 1;
        let unit = self.raw[*p];
        let mut cp = u32::from(unit);
        if is_trail(unit) && *p > 0 && is_lead(self.raw[*p - 1]) {
            cp = supplementary(self.raw[*p - 1], unit);
            *p -= 1;
        }
        self.table.fcd16(cp)
    }

    fn next_segment(&mut self) {
        // The text [segmentStart..pos[ passes the FCD check.
        let mut p = self.pos;
        let mut prev_cc = 0u8;
        loop {
            let q = p;
            let fcd16 = self.next_fcd16(&mut p);
            let lead_cc = (fcd16 >> 8) as u8;
            if lead_cc == 0 && q != self.pos {
                // FCD boundary before the [q, p[ character.
                self.limit = q;
                self.segment_limit = q;
                break;
            }
            if lead_cc != 0 && (prev_cc > lead_cc || is_tibetan_composite_vowel(fcd16)) {
                // Fails the FCD check: normalize up to the next FCD boundary.
                let mut q = p;
                while p != self.raw.len() && self.next_fcd16(&mut p) > 0xff {
                    q = p;
                }
                self.normalize(self.pos, q);
                self.pos = self.start;
                break;
            }
            prev_cc = fcd16 as u8;
            if p == self.raw.len() || prev_cc == 0 {
                // FCD boundary after the last character.
                self.limit = p;
                self.segment_limit = p;
                break;
            }
        }
        self.check_dir = 0;
    }

    fn previous_segment(&mut self) {
        // The text [pos..segmentLimit[ passes the FCD check.
        let mut p = self.pos;
        let mut next_cc = 0u8;
        loop {
            let q = p;
            let mut fcd16 = self.previous_fcd16(&mut p);
            let trail_cc = fcd16 as u8;
            if trail_cc == 0 && q != self.pos {
                // FCD boundary after the [p, q[ character.
                self.start = q;
                self.segment_start = q;
                break;
            }
            if trail_cc != 0 && ((next_cc != 0 && trail_cc > next_cc) || is_tibetan_composite_vowel(fcd16)) {
                // Fails the FCD check: normalize back to the previous FCD boundary.
                let mut q;
                loop {
                    q = p;
                    if !(fcd16 > 0xff && p != 0) {
                        break;
                    }
                    fcd16 = self.previous_fcd16(&mut p);
                    if fcd16 == 0 {
                        break;
                    }
                }
                self.normalize(q, self.pos);
                self.pos = self.limit;
                break;
            }
            next_cc = (fcd16 >> 8) as u8;
            if p == 0 || next_cc == 0 {
                // FCD boundary before the following character.
                self.start = p;
                self.segment_start = p;
                break;
            }
        }
        self.check_dir = 0;
    }

    fn normalize(&mut self, from: usize, to: usize) {
        self.normalized = self.table.nfd(&self.raw[from..to]);
        self.segment_start = from;
        self.segment_limit = to;
        self.in_normalized = true;
        self.start = 0;
        self.limit = self.normalized.len();
    }

    fn next_skipped_code_point(&mut self) -> Option<u32> {
        if let Some(skipped) = self.skipped.as_mut().filter(|s| s.has_next()) {
            return Some(skipped.next());
        }
        let cp = self.next_code_point();
        if let Some(skipped) = self.skipped.as_mut().filter(|s| !s.is_empty() && cp.is_some()) {
            skipped.pos += 1;
        }
        cp
    }

    fn backward_num_skipped(&mut self, n: usize) {
        let n = match self.skipped.as_mut().filter(|s| !s.is_empty()) {
            Some(skipped) => skipped.backward_num_code_points(n),
            None => n,
        };
        self.backward_num_code_points(n);
    }

    fn skipped(&mut self) -> &mut Skipped {
        self.skipped.get_or_insert_with(Skipped::default)
    }

    /// `appendCEsFromCE32` (forward).
    fn append(&mut self, mut ce32: Ce32, forward: bool) {
        let table = self.table;
        loop {
            match ce32 {
                Ce32::Plain(cp) => return table.single(cp, &mut self.ces),
                Ce32::Elements(packed) => return table.push_elements(packed, &mut self.ces),
                Ce32::Hangul(cp) => {
                    let s = cp - HANGUL;
                    table.single(0x1100 + s / 588, &mut self.ces);
                    table.single(0x1161 + s % 588 / 28, &mut self.ces);
                    if s % 28 != 0 {
                        table.single(0x11A7 + s % 28, &mut self.ces);
                    }
                    return;
                }
                Ce32::Prefix(cp) => {
                    if forward {
                        self.backward_num_code_points(1);
                    }
                    ce32 = self.from_prefix(cp);
                    if forward {
                        self.forward_num_code_points(1);
                    }
                }
                Ce32::Contraction(cp) => {
                    let default = Ce32::Plain(cp);
                    if !forward {
                        ce32 = default;
                        continue;
                    }
                    let entries = table.contraction_entries(cp);
                    let flags = table.flags(entries.0, entries.1);
                    let fast = self.skipped.is_none();
                    let next = if fast { self.next_code_point() } else { self.next_skipped_code_point() };
                    let Some(next) = next else {
                        ce32 = default;
                        continue;
                    };
                    if flags.next_ccc && !table.may_have_lccc(next) {
                        // Every suffix starts with a non-starter, and this code point has lccc 0.
                        if fast { self.backward_num_code_points(1) } else { self.backward_num_skipped(1) }
                        ce32 = default;
                        continue;
                    }
                    match self.from_contraction(Trie::new(false, entries), flags, default, next) {
                        Some(found) => ce32 = found,
                        None => return,
                    }
                }
            }
        }
    }

    /// `getCE32FromPrefix`: the mapping of `cp` after the longest prefix that matches the text before it.
    fn from_prefix(&mut self, cp: u32) -> Ce32 {
        let mut ce32 = Ce32::Plain(cp);
        let mut look_behind = 0;
        let mut prefixes = Trie::new(true, self.table.prefix_entries(cp));
        while let Some(before) = self.previous_code_point() {
            look_behind += 1;
            let step = prefixes.next(self.table, before);
            if let Some(packed) = step.value() {
                ce32 = Ce32::Elements(packed);
            }
            if !step.has_next() {
                break;
            }
        }
        self.forward_num_code_points(look_behind);
        ce32
    }

    /// `nextCE32FromContraction`; `None` when the elements (of a discontiguous contraction) are appended already.
    fn from_contraction(&mut self, mut suffixes: Trie, flags: Flags, mut ce32: Ce32, mut cp: u32) -> Option<Ce32> {
        let table = self.table;
        // Code points read beyond the original one, and since the last match.
        let mut look_ahead = 1;
        let mut since_match = 1;
        if let Some(skipped) = self.skipped.as_mut().filter(|s| !s.is_empty()) {
            skipped.state = Some(suffixes);
        }
        let mut step = suffixes.next(table, cp);
        loop {
            if let Some(packed) = step.value() {
                ce32 = Ce32::Elements(packed);
                if !step.has_next() {
                    return Some(ce32);
                }
                match self.next_skipped_code_point() {
                    Some(next) => cp = next,
                    None => return Some(ce32),
                }
                let saved = suffixes;
                if let Some(skipped) = self.skipped.as_mut().filter(|s| !s.is_empty()) {
                    skipped.state = Some(saved);
                }
                since_match = 1;
            } else {
                let next = if step == Step::NoMatch { None } else { self.next_skipped_code_point() };
                match next {
                    Some(next) => {
                        // A partial match: not itself "a match in the table".
                        cp = next;
                        since_match += 1;
                    }
                    None => {
                        if flags.trailing_ccc {
                            if since_match > 1 {
                                // Back to the state after the last match.
                                self.backward_num_skipped(since_match);
                                cp = self.next_skipped_code_point().unwrap_or(cp);
                                look_ahead -= since_match - 1;
                                since_match = 1;
                            }
                            if table.fcd16(cp) > 0xff {
                                return self.from_discontiguous_contraction(suffixes, ce32, look_ahead, cp);
                            }
                        }
                        break;
                    }
                }
            }
            look_ahead += 1;
            step = suffixes.next(table, cp);
        }
        self.backward_num_skipped(since_match);
        Some(ce32)
    }

    /// `nextCE32FromDiscontiguousContraction`.
    fn from_discontiguous_contraction(&mut self, mut suffixes: Trie, mut ce32: Ce32, mut look_ahead: usize, cp: u32) -> Option<Ce32> {
        let table = self.table;
        let mut fcd16 = table.fcd16(cp);
        let Some(next) = self.next_skipped_code_point() else {
            self.backward_num_skipped(1);
            return Some(ce32);
        };
        look_ahead += 1;
        let mut prev_cc = fcd16 as u8;
        fcd16 = table.fcd16(next);
        if fcd16 <= 0xff {
            // The code point after cp is a starter.
            self.backward_num_skipped(2);
            return Some(ce32);
        }
        if self.skipped.as_ref().is_none_or(Skipped::is_empty) {
            self.skipped();
            suffixes.reset();
            if look_ahead > 2 {
                // Replay the partial match so far.
                self.backward_num_code_points(look_ahead);
                if let Some(first) = self.next_code_point() {
                    suffixes.next(table, first);
                }
                for _ in 3..look_ahead {
                    if let Some(again) = self.next_code_point() {
                        suffixes.next(table, again);
                    }
                }
                // Skip cp (which did not match) and next (which is tried now).
                self.forward_num_code_points(2);
            }
            self.skipped().state = Some(suffixes);
        } else if let Some(state) = self.skipped().state {
            suffixes = state;
        }
        let skipped = self.skipped();
        skipped.skip_length_at_match = 0;
        skipped.new = vec![cp];
        let mut since_match = 2;
        let mut cp = next;
        loop {
            // "If C is not blocked from S, find if S + C has a match in the table." (S2.1.2)
            let step = if prev_cc < (fcd16 >> 8) as u8 { suffixes.next(table, cp) } else { Step::NoMatch };
            if let Some(packed) = step.value() {
                // "If there is a match, replace S by S + C, and remove C." (S2.1.3)
                ce32 = Ce32::Elements(packed);
                since_match = 0;
                let skipped = self.skipped();
                skipped.skip_length_at_match = skipped.new.len();
                if !step.has_next() {
                    break;
                }
                skipped.state = Some(suffixes);
            } else {
                let skipped = self.skipped();
                skipped.new.push(cp);
                if let Some(state) = skipped.state {
                    suffixes = state;
                }
                prev_cc = fcd16 as u8;
            }
            match self.next_skipped_code_point() {
                Some(next) => cp = next,
                None => break,
            }
            since_match += 1;
            fcd16 = table.fcd16(cp);
            if fcd16 <= 0xff {
                break;
            }
        }
        self.backward_num_skipped(since_match);
        let top = self.skipped().is_empty();
        self.skipped().replace_match();
        if top && !self.skipped().is_empty() {
            // A match after skipping marks, not within another discontiguous contraction: the contraction's elements,
            // then those of the marks skipped before the match.
            loop {
                self.append(ce32, true);
                let skipped = self.skipped();
                if !skipped.has_next() {
                    break;
                }
                let mark = skipped.next();
                ce32 = table.data_ce32(mark);
            }
            let skipped = self.skipped();
            skipped.old.clear();
            skipped.pos = 0;
            return None;
        }
        Some(ce32)
    }
}

fn level(element: &Element, which: u8) -> u32 {
    match which {
        1 => element.0,
        2 => element.1 >> 16,
        _ => element.1 & 0xffff,
    }
}

/// A mapping has at most 31 elements (`Collation::MAX_EXPANSION_LENGTH`) and covers at least one code point, so an
/// iteration that gives more than this per code unit is ICU's endless loop (see [`compare`]).
const ELEMENTS_PER_UNIT: usize = 31;
/// How far past the longest finite iteration [`compare`] follows two endless ones before it gives up.
const ENDLESS_SLACK: usize = 10_000;

/// `a.localeCompare(b)` (as an ordering; equal at tertiary strength is `Equal`).
///
/// ICU's iteration loops forever on some strings, giving one element again and again (a discontiguous contraction
/// that fails after a normalized segment backs up across it, and FCDUTF16CollationIterator's backward check then
/// resumes before the contraction's starter). The comparison pulls elements lazily, as
/// `CollationCompare::compareUpToQuaternary` does, so such a string compares as in Bun wherever Bun answers: until
/// the other string, which is finite, ends or differs. Where both loop alike, Bun never returns (it spins and grows
/// its element buffer); here they compare equal once both have run [`ENDLESS_SLACK`] elements past any finite
/// iteration. TODO(port): a deviation from the reference, which hangs there (Bun 1.4.2, ICU 78:
/// `"ె\u{16B31}\u{10AE6}̈́".localeCompare("ె\u{16B30}\u{10AE6}̈́")` never returns, so a directory
/// holding two such names hangs `/api/fs/list`).
pub fn compare(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (left, right): (Vec<u16>, Vec<u16>) = (a.encode_utf16().collect(), b.encode_utf16().collect());
    // The identical prefix, backed up to a safe code unit (`RuleBasedCollator::doCompare`).
    let mut equal = left.iter().zip(&right).take_while(|(l, r)| l == r).count();
    let table = table();
    let unsafe_at = |text: &[u16], at: usize| at != text.len() && table.bit(UNSAFE_BITS, text[at]);
    if equal > 0 && (unsafe_at(&left, equal) || unsafe_at(&right, equal)) {
        equal -= 1;
        while equal > 0 && table.bit(UNSAFE_BITS, left[equal]) {
            equal -= 1;
        }
    }
    let mut budget = ELEMENTS_PER_UNIT * (left.len() + right.len() + 2) + ENDLESS_SLACK;
    let (mut left, mut right) = (Iter::new(&left, equal), Iter::new(&right, equal));
    // The primaries, fetched as they are compared; the elements stay in the iterators for the other levels.
    let mut primary = |iter: &mut Iter| -> Result<Option<u32>, ()> {
        loop {
            budget = budget.checked_sub(1).ok_or(())?;
            match iter.next_ce() {
                Some((0, _)) => {}
                found => return Ok(found.map(|element| element.0)),
            }
        }
    };
    loop {
        let (Ok(l), Ok(r)) = (primary(&mut left), primary(&mut right)) else { return Ordering::Equal };
        match (l, r) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x != y => return x.cmp(&y),
            _ => {}
        }
    }
    for which in 2..=3u8 {
        let mut l = left.ces.iter().map(|e| level(e, which)).filter(|w| *w != 0);
        let mut r = right.ces.iter().map(|e| level(e, which)).filter(|w| *w != 0);
        loop {
            match (l.next(), r.next()) {
                (None, None) => break,
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (Some(x), Some(y)) if x != y => return x.cmp(&y),
                _ => {}
            }
        }
    }
    Ordering::Equal
}

/// The collation elements of a string as ICU's CollationElementIterator gives them (for the harness,
/// `port/rust/crates/opencode/harness/collate`): at most 31 per code unit, plus 64, and whether there are more.
#[doc(hidden)]
#[allow(dead_code)]
pub fn debug_elements(text: &str) -> (Vec<(u32, u32)>, bool) {
    let text: Vec<u16> = text.encode_utf16().collect();
    let limit = ELEMENTS_PER_UNIT * text.len() + 64;
    let mut iter = Iter::new(&text, 0);
    let mut out = Vec::new();
    while let Some(element) = iter.next_ce() {
        if out.len() == limit {
            return (out, true);
        }
        out.push(element);
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sample sorted by Bun: `COLLATE_SAMPLE=<file> cargo test -p opencode collate -- --ignored`, the file holding
    /// `{ items, sorted }` (items in random order, then `items.toSorted((a, b) => a.localeCompare(b))`).
    #[test]
    #[ignore]
    fn sorts_sample_like_bun() {
        let path = std::env::var("COLLATE_SAMPLE").expect("COLLATE_SAMPLE");
        let sample = crate::json::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        let strings = |key: &str| -> Vec<String> {
            crate::json::array(crate::json::get(&sample, key).unwrap())
                .unwrap()
                .iter()
                .map(|v| match v {
                    Json::String(s) => s.clone(),
                    _ => panic!("not a string"),
                })
                .collect()
        };
        let mut items = strings("items");
        items.sort_by(|a, b| compare(a, b));
        let expected = strings("sorted");
        let wrong: Vec<String> = items
            .iter()
            .zip(&expected)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .take(20)
            .map(|(i, (a, b))| format!("{i}: rust {a:?} bun {b:?}"))
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// Orders recorded from Bun (`items.toSorted((a, b) => a.localeCompare(b))`).
    #[test]
    fn sorts_like_bun() {
        let mut items: Vec<&str> = vec![
            "b", "A", "a", "B", "_x", "-x", ".x", "ü", "u", "v", "Z", "z", "10", "9", "a b", "ab", "a-b", "a_b", "a.b", "a/b",
            "a/", "a", "README.md", "src/", "é", "e", "f", "~", "Ä", "tab\tname", "with space", "quote\"d", "a1", "a10", "a2",
            "ß", "ss", "Æ", "ae", "ǅ", "x\u{301}", "#", "$", "!", "@", "^", "`", "|", "+", "=", "<", ">", "*", "&", "%", "(",
            ")", "[", "]", "{", "}", "'", ",", ";", ":", "?", "\\", "0", "½", "²", "€", "α", "я", "中", "😀",
        ];
        items.sort_by(|a, b| compare(a, b));
        let expected = [
            "_x", "-x", ",", ";", ":", "!", "?", ".x", "'", "(", ")", "[", "]", "{", "}", "@", "*", "\\", "&", "#", "%", "`",
            "^", "+", "<", "=", ">", "|", "~", "😀", "$", "€", "0", "½", "10", "²", "9", "a", "a", "A", "Ä", "a b", "a_b",
            "a-b", "a.b", "a/", "a/b", "a1", "a10", "a2", "ab", "ae", "Æ", "b", "B", "ǅ", "e", "é", "f", "quote\"d",
            "README.md", "src/", "ss", "ß", "tab\tname", "u", "ü", "v", "with space", "x\u{301}", "z", "Z", "α", "я", "中",
        ];
        assert_eq!(items, expected);
    }

    /// ICU's own quirks, recorded from Bun: canonically equivalent strings it orders apart.
    #[test]
    fn keeps_icu_quirks() {
        assert_eq!(compare("\u{fdd1}\u{ac00}", "\u{fdd1}\u{1100}\u{1161}"), Ordering::Less);
        assert_eq!(compare("\u{64a}\u{1e01b}\u{108d}", "\u{64a}\u{108d}\u{1e01b}"), Ordering::Less);
        assert_eq!(compare("x\u{1e01b}\u{108d}", "x\u{108d}\u{1e01b}"), Ordering::Equal);
        assert_eq!(compare("\u{fdd1}\u{1e3a}", "\u{fdd1}L\u{331}"), Ordering::Greater);
    }

    /// ICU's endless iteration: against a finite string it compares as in Bun; two endless ones return equal where
    /// Bun never returns.
    #[test]
    fn ends_where_icu_loops() {
        let endless = "\u{c46}\u{16b31}\u{10ae6}\u{344}";
        assert!(debug_elements(endless).1);
        assert_eq!(compare(endless, "a"), Ordering::Greater);
        assert_eq!(compare(endless, "\u{c46}\u{c46}\u{c46}b"), Ordering::Greater);
        assert_eq!(compare(endless, "\u{c46}\u{16b30}\u{10ae6}\u{344}"), Ordering::Equal);
    }
}
