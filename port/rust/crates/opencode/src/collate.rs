//! `String.prototype.localeCompare` as the TS reference runs it: Bun's ICU in its default locale (en-US, which has no
//! tailoring), so the Unicode Collation Algorithm over the root order at tertiary strength, variable characters
//! non-ignorable, no identical level. TS sorts paths, names and IDs with it (packages/core/src/plugin/vcs/git.ts,
//! filesystem.ts, skill.ts, tool/read-filesystem.ts, ...): use [`compare`] wherever TS calls `localeCompare`.
//!
//! The weights are the DUCET (`generated/collation.bin`, by `port/rust/gen/src/collation.ts`); input is normalized
//! to NFD first, as ICU's canonical closure makes equivalent strings compare equal.
//! TODO(port): discontiguous contractions (UCA S2.1.1-S2.1.3, combining marks between the characters of a
//! contraction); CLDR root's few differences from the DUCET (Tibetan, a handful of symbols); characters added after
//! Unicode 13 weigh as unassigned.

use std::cmp::Ordering;

use unicode_normalization::UnicodeNormalization;

static TABLE: &[u8] = include_bytes!("generated/collation.bin");

struct Table {
    singles: &'static [u8],
    contractions: &'static [u8],
    elements: &'static [u8],
    implicit: &'static [u8],
}

fn word(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
}

fn table() -> &'static Table {
    static PARSED: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
    PARSED.get_or_init(|| {
        assert_eq!(word(TABLE, 0), 0x3141_4355, "collation table magic");
        let (singles, contractions, elements, implicit) =
            (word(TABLE, 1) as usize, word(TABLE, 2) as usize, word(TABLE, 3) as usize, word(TABLE, 4) as usize);
        let mut offset = 20;
        let mut take = |words: usize| {
            let slice = &TABLE[offset..offset + words * 4];
            offset += words * 4;
            slice
        };
        Table {
            singles: take(singles * 2),
            contractions: take(contractions * 4),
            elements: take(elements),
            implicit: take(implicit * 3),
        }
    })
}

impl Table {
    fn elements(&self, packed: u32) -> impl Iterator<Item = u32> + '_ {
        let (start, len) = ((packed >> 8) as usize, (packed & 0xff) as usize);
        (start..start + len).map(move |i| word(self.elements, i))
    }

    fn single(&self, cp: u32) -> Option<u32> {
        let count = self.singles.len() / 8;
        let (mut lo, mut hi) = (0, count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            match word(self.singles, mid * 2).cmp(&cp) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => return Some(word(self.singles, mid * 2 + 1)),
            }
        }
        None
    }

    /// The longest contraction starting at `chars[0]`: its length and elements.
    fn contraction(&self, chars: &[u32]) -> Option<(usize, u32)> {
        let count = self.contractions.len() / 16;
        let key = |i: usize| (word(self.contractions, i * 4), word(self.contractions, i * 4 + 1), word(self.contractions, i * 4 + 2));
        // The first entry starting with chars[0].
        let (mut lo, mut hi) = (0, count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if key(mid).0 < chars[0] { lo = mid + 1 } else { hi = mid }
        }
        let mut best = None;
        let mut index = lo;
        while index < count && key(index).0 == chars[0] {
            let (_, second, third) = key(index);
            let len = if third == 0 { 2 } else { 3 };
            if chars.len() >= len && chars[1] == second && (len == 2 || chars[2] == third) {
                if best.is_none_or(|(l, _)| len > l) {
                    best = Some((len, word(self.contractions, index * 4 + 3)));
                }
            }
            index += 1;
        }
        best
    }

    /// The two implicit weights (`AAAA`, `BBBB`) of a code point without an entry (UTS #10 §10.1.3).
    fn implicit(&self, cp: u32) -> (u32, u32) {
        let ranges = self.implicit.len() / 12;
        let range = |r: usize| (word(self.implicit, r * 3), word(self.implicit, r * 3 + 1), word(self.implicit, r * 3 + 2));
        if let Some((_, _, base)) = (0..ranges).map(range).find(|(first, last, _)| (*first..=*last).contains(&cp)) {
            // Siniform scripts count from the first code point of their script (Tangut's two blocks share one base).
            let first = (0..ranges).map(range).find(|(_, _, b)| *b == base).map(|(f, _, _)| f).unwrap_or(cp);
            return (base, (cp - first) | 0x8000);
        }
        let base = match cp {
            0x4E00..=0x9FFF | 0xFA0E | 0xFA0F | 0xFA11 | 0xFA13 | 0xFA14 | 0xFA1F | 0xFA21 | 0xFA23 | 0xFA24 | 0xFA27..=0xFA29 => 0xFB40,
            0x3400..=0x4DBF | 0x20000..=0x2A6DF | 0x2A700..=0x2EBEF | 0x30000..=0x3134F => 0xFB80,
            _ => 0xFBC0,
        };
        (base + (cp >> 15), (cp & 0x7FFF) | 0x8000)
    }
}

/// The collation elements of a string (UCA S1-S2, non-ignorable).
fn elements(text: &str) -> Vec<u32> {
    let table = table();
    let chars: Vec<u32> = text.nfd().map(|c| c as u32).collect();
    let mut out = Vec::with_capacity(chars.len() + 2);
    let mut index = 0;
    while index < chars.len() {
        if let Some((len, packed)) = table.contraction(&chars[index..]) {
            out.extend(table.elements(packed));
            index += len;
            continue;
        }
        let cp = chars[index];
        match table.single(cp) {
            Some(packed) => out.extend(table.elements(packed)),
            None => {
                let (aaaa, bbbb) = table.implicit(cp);
                out.push((aaaa << 14) | (0x20 << 5) | 0x02);
                out.push(bbbb << 14);
            }
        }
        index += 1;
    }
    out
}

fn level(ce: u32, which: u8) -> u32 {
    match which {
        1 => ce >> 14,
        2 => (ce >> 5) & 0x1ff,
        _ => ce & 0x1f,
    }
}

/// `a.localeCompare(b)` (as an ordering; equal at tertiary strength is `Equal`).
pub fn compare(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (left, right) = (elements(a), elements(b));
    for which in 1..=3u8 {
        let mut l = left.iter().map(|ce| level(*ce, which)).filter(|w| *w != 0);
        let mut r = right.iter().map(|ce| level(*ce, which)).filter(|w| *w != 0);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
