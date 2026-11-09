//! The Unicode properties and grapheme break rules of uucode 0.2.0 (`zig-deps/uucode-opentui`), as OpenTUI's Zig
//! core configures it: `grapheme_break` (UAX #29 tailored as uucode's `GraphemeBreakDerived`), `east_asian_width`,
//! `general_category`, `is_default_ignorable` and `wcwidth_zero_in_grapheme`. The tables are generated from the
//! same UCD files by `scripts/gen-unicode.ts`; the break rules port `src/grapheme.zig` (`computeGraphemeBreak`,
//! precomputed into a table as `precomputedGraphemeBreak` does).

use crate::unicode_tables::{SHIFT, STAGE1, STAGE2, VALUES};

/// uucode `types.GraphemeBreak`, in declaration order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum GraphemeBreak {
    Other = 0,
    Control,
    Prepend,
    Cr,
    Lf,
    RegionalIndicator,
    SpacingMark,
    L,
    V,
    T,
    Lv,
    Lvt,
    Zwj,
    Zwnj,
    ExtendedPictographic,
    EmojiModifierBase,
    EmojiModifier,
    IndicConjunctBreakExtend,
    IndicConjunctBreakLinker,
    IndicConjunctBreakConsonant,
}

const GB_COUNT: usize = 20;

impl GraphemeBreak {
    const ALL: [GraphemeBreak; GB_COUNT] = [
        GraphemeBreak::Other,
        GraphemeBreak::Control,
        GraphemeBreak::Prepend,
        GraphemeBreak::Cr,
        GraphemeBreak::Lf,
        GraphemeBreak::RegionalIndicator,
        GraphemeBreak::SpacingMark,
        GraphemeBreak::L,
        GraphemeBreak::V,
        GraphemeBreak::T,
        GraphemeBreak::Lv,
        GraphemeBreak::Lvt,
        GraphemeBreak::Zwj,
        GraphemeBreak::Zwnj,
        GraphemeBreak::ExtendedPictographic,
        GraphemeBreak::EmojiModifierBase,
        GraphemeBreak::EmojiModifier,
        GraphemeBreak::IndicConjunctBreakExtend,
        GraphemeBreak::IndicConjunctBreakLinker,
        GraphemeBreak::IndicConjunctBreakConsonant,
    ];
}

/// uucode `grapheme.BreakState`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum BreakState {
    #[default]
    Default = 0,
    RegionalIndicator,
    ExtendedPictographic,
    IndicConjunctBreakConsonant,
    IndicConjunctBreakLinker,
}

const STATE_COUNT: usize = 5;

impl BreakState {
    pub const ALL: [BreakState; STATE_COUNT] = [
        BreakState::Default,
        BreakState::RegionalIndicator,
        BreakState::ExtendedPictographic,
        BreakState::IndicConjunctBreakConsonant,
        BreakState::IndicConjunctBreakLinker,
    ];
}

/// General category classes the core distinguishes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarkClass {
    None,
    Nonspacing,
    SpacingCombining,
    Enclosing,
}

#[inline]
fn value(cp: u32) -> u16 {
    if cp > 0x10FFFF {
        return 0;
    }
    let block = STAGE1[(cp >> SHIFT) as usize] as usize;
    VALUES[STAGE2[(block << SHIFT) + (cp & ((1 << SHIFT) - 1)) as usize] as usize]
}

#[inline]
pub fn grapheme_break(cp: u32) -> GraphemeBreak {
    GraphemeBreak::ALL[(value(cp) & 0x1F) as usize]
}

/// East Asian Width is Wide or Fullwidth.
#[inline]
pub fn is_wide(cp: u32) -> bool {
    value(cp) & (1 << 5) != 0
}

#[inline]
pub fn mark_class(cp: u32) -> MarkClass {
    match (value(cp) >> 6) & 3 {
        1 => MarkClass::Nonspacing,
        2 => MarkClass::SpacingCombining,
        3 => MarkClass::Enclosing,
        _ => MarkClass::None,
    }
}

#[inline]
pub fn is_mark(cp: u32) -> bool {
    (value(cp) >> 6) & 3 != 0
}

#[inline]
pub fn is_nonspacing_mark(cp: u32) -> bool {
    (value(cp) >> 6) & 3 == 1
}

#[inline]
pub fn is_default_ignorable(cp: u32) -> bool {
    value(cp) & (1 << 8) != 0
}

#[inline]
pub fn wcwidth_zero_in_grapheme(cp: u32) -> bool {
    value(cp) & (1 << 9) != 0
}

const fn is_indic_conjunct_break_extend(gb: GraphemeBreak) -> bool {
    matches!(gb, GraphemeBreak::IndicConjunctBreakExtend | GraphemeBreak::Zwj)
}

const fn is_extend(gb: GraphemeBreak) -> bool {
    matches!(gb, GraphemeBreak::Zwnj | GraphemeBreak::IndicConjunctBreakExtend | GraphemeBreak::IndicConjunctBreakLinker)
}

const fn is_extended_pictographic(gb: GraphemeBreak) -> bool {
    matches!(gb, GraphemeBreak::ExtendedPictographic | GraphemeBreak::EmojiModifierBase)
}

/// `computeGraphemeBreak`: whether there is a break between `gb1` and `gb2`, updating `state`.
const fn compute_grapheme_break(gb1: GraphemeBreak, gb2: GraphemeBreak, state: &mut BreakState) -> bool {
    use GraphemeBreak as G;
    match *state {
        BreakState::RegionalIndicator => {
            if !matches!(gb1, G::RegionalIndicator) || !matches!(gb2, G::RegionalIndicator) {
                *state = BreakState::Default;
            }
        }
        BreakState::ExtendedPictographic => {
            if !matches!(
                gb1,
                G::IndicConjunctBreakExtend
                    | G::IndicConjunctBreakLinker
                    | G::Zwnj
                    | G::Zwj
                    | G::ExtendedPictographic
                    | G::EmojiModifierBase
                    | G::EmojiModifier
            ) {
                *state = BreakState::Default;
            }
            if !matches!(
                gb2,
                G::IndicConjunctBreakExtend
                    | G::IndicConjunctBreakLinker
                    | G::Zwnj
                    | G::Zwj
                    | G::ExtendedPictographic
                    | G::EmojiModifierBase
                    | G::EmojiModifier
            ) {
                *state = BreakState::Default;
            }
        }
        BreakState::IndicConjunctBreakConsonant | BreakState::IndicConjunctBreakLinker => {
            if !matches!(
                gb1,
                G::IndicConjunctBreakConsonant | G::IndicConjunctBreakLinker | G::IndicConjunctBreakExtend | G::Zwj
            ) {
                *state = BreakState::Default;
            }
            if !matches!(
                gb2,
                G::IndicConjunctBreakConsonant | G::IndicConjunctBreakLinker | G::IndicConjunctBreakExtend | G::Zwj
            ) {
                *state = BreakState::Default;
            }
        }
        BreakState::Default => {}
    }

    // GB3
    if matches!(gb1, G::Cr) && matches!(gb2, G::Lf) {
        return false;
    }
    // GB4
    if matches!(gb1, G::Control | G::Cr | G::Lf) {
        return true;
    }
    // GB5
    if matches!(gb2, G::Control | G::Cr | G::Lf) {
        return true;
    }
    // GB6
    if matches!(gb1, G::L) && matches!(gb2, G::L | G::V | G::Lv | G::Lvt) {
        return false;
    }
    // GB7
    if matches!(gb1, G::Lv | G::V) && matches!(gb2, G::V | G::T) {
        return false;
    }
    // GB8
    if matches!(gb1, G::Lvt | G::T) && matches!(gb2, G::T) {
        return false;
    }
    // GB9a
    if matches!(gb2, G::SpacingMark) {
        return false;
    }
    // GB9b
    if matches!(gb1, G::Prepend) {
        return false;
    }
    // GB9c
    if matches!(gb1, G::IndicConjunctBreakConsonant) {
        if is_indic_conjunct_break_extend(gb2) {
            *state = BreakState::IndicConjunctBreakConsonant;
            return false;
        } else if matches!(gb2, G::IndicConjunctBreakLinker) {
            *state = BreakState::IndicConjunctBreakLinker;
            return false;
        }
    } else if matches!(*state, BreakState::IndicConjunctBreakConsonant) {
        if matches!(gb2, G::IndicConjunctBreakLinker) {
            *state = BreakState::IndicConjunctBreakLinker;
            return false;
        } else if is_indic_conjunct_break_extend(gb2) {
            return false;
        } else {
            *state = BreakState::Default;
        }
    } else if matches!(*state, BreakState::IndicConjunctBreakLinker) {
        if matches!(gb2, G::IndicConjunctBreakLinker) || is_indic_conjunct_break_extend(gb2) {
            return false;
        } else if matches!(gb2, G::IndicConjunctBreakConsonant) {
            *state = BreakState::Default;
            return false;
        } else {
            *state = BreakState::Default;
        }
    }
    // GB11
    if is_extended_pictographic(gb1) {
        if is_extend(gb2) || matches!(gb2, G::Zwj) {
            *state = BreakState::ExtendedPictographic;
            return false;
        }
        if matches!(gb1, G::EmojiModifierBase) && matches!(gb2, G::EmojiModifier) {
            *state = BreakState::ExtendedPictographic;
            return false;
        }
    } else if matches!(*state, BreakState::ExtendedPictographic) {
        if (is_extend(gb1) || matches!(gb1, G::EmojiModifier)) && (is_extend(gb2) || matches!(gb2, G::Zwj)) {
            return false;
        } else if matches!(gb1, G::Zwj) && is_extended_pictographic(gb2) {
            *state = BreakState::Default;
            return false;
        } else {
            *state = BreakState::Default;
        }
    }
    // GB12 and GB13
    if matches!(gb1, G::RegionalIndicator) && matches!(gb2, G::RegionalIndicator) {
        if matches!(*state, BreakState::Default) {
            *state = BreakState::RegionalIndicator;
            return false;
        } else {
            *state = BreakState::Default;
            return true;
        }
    }
    // GB9
    if is_extend(gb2) || matches!(gb2, G::Zwj) {
        return false;
    }
    // GB999
    true
}

/// `buildGraphemeBreakTable`: bit 7 is the result, bits 0-2 the next state.
static BREAK_TABLE: [u8; STATE_COUNT * GB_COUNT * GB_COUNT] = build_break_table();

const fn build_break_table() -> [u8; STATE_COUNT * GB_COUNT * GB_COUNT] {
    let mut table = [0u8; STATE_COUNT * GB_COUNT * GB_COUNT];
    let mut s = 0;
    while s < STATE_COUNT {
        let mut a = 0;
        while a < GB_COUNT {
            let mut b = 0;
            while b < GB_COUNT {
                let mut state = BreakState::ALL[s];
                let result = compute_grapheme_break(GraphemeBreak::ALL[a], GraphemeBreak::ALL[b], &mut state);
                table[s * GB_COUNT * GB_COUNT + a * GB_COUNT + b] = (if result { 0x80 } else { 0 }) | state as u8;
                b += 1;
            }
            a += 1;
        }
        s += 1;
    }
    table
}

/// uucode `grapheme.isBreak` (through the precomputed table).
#[inline]
pub fn is_break(cp1: u32, cp2: u32, state: &mut BreakState) -> bool {
    let gb1 = grapheme_break(cp1) as usize;
    let gb2 = grapheme_break(cp2) as usize;
    let entry = BREAK_TABLE[*state as usize * GB_COUNT * GB_COUNT + gb1 * GB_COUNT + gb2];
    *state = BreakState::ALL[(entry & 7) as usize];
    entry & 0x80 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// uucode's own test: GraphemeBreakTest.txt, with emoji modifiers that follow no base breaking.
    #[test]
    fn grapheme_break_test() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../.vendor/opentui/packages/native/zig-deps/uucode-opentui/ucd/auxiliary/GraphemeBreakTest.txt"
        );
        let Ok(content) = std::fs::read_to_string(path) else {
            eprintln!("skipped: no {path}");
            return;
        };
        let mut failures = 0;
        for line in content.lines() {
            let line = line.split('#').next().unwrap().trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split(' ').collect();
            assert_eq!(parts[0], "÷");
            let mut state = BreakState::Default;
            let mut i = 1;
            while i + 2 < parts.len() {
                let cp1 = u32::from_str_radix(parts[i], 16).unwrap();
                let cp2 = u32::from_str_radix(parts[i + 2], 16).unwrap();
                let mut expected = parts[i + 1] == "÷";
                if grapheme_break(cp2) == GraphemeBreak::EmojiModifier && grapheme_break(cp1) != GraphemeBreak::EmojiModifierBase {
                    expected = true;
                }
                if is_break(cp1, cp2, &mut state) != expected {
                    failures += 1;
                }
                i += 2;
            }
        }
        assert_eq!(failures, 0);
    }
}
