//! The parts of entities@4.5.0 (`lib/decode.js`, `lib/decode_codepoint.js`) that htmlparser2's tokenizer uses.

pub(crate) use crate::entities_data::HTML_DECODE_TREE;

/// `BinTrieFlags.VALUE_LENGTH`.
pub(crate) const VALUE_LENGTH: u16 = 0b1100_0000_0000_0000;
const BRANCH_LENGTH: u16 = 0b0011_1111_1000_0000;
const JUMP_TABLE: u16 = 0b0000_0000_0111_1111;

/// `determineBranch(decodeTree, current, nodeIdx, char)`: the index of the child of `current` for `char`, or -1.
pub(crate) fn determine_branch(tree: &[u16], current: u16, node_idx: i32, char: u32) -> i32 {
    let branch_count = ((current & BRANCH_LENGTH) >> 7) as i32;
    let jump_offset = (current & JUMP_TABLE) as i32;
    let char = char as i32;
    if branch_count == 0 {
        return if jump_offset != 0 && char == jump_offset { node_idx } else { -1 };
    }
    if jump_offset != 0 {
        let value = char - jump_offset;
        return if value < 0 || value >= branch_count { -1 } else { tree[(node_idx + value) as usize] as i32 - 1 };
    }
    let mut lo = node_idx;
    let mut hi = lo + branch_count - 1;
    while lo <= hi {
        let mid = ((lo + hi) as u32 >> 1) as i32;
        let mid_val = tree[mid as usize] as i32;
        if mid_val < char {
            lo = mid + 1;
        } else if mid_val > char {
            hi = mid - 1;
        } else {
            return tree[(mid + branch_count) as usize] as i32;
        }
    }
    -1
}

/// `replaceCodePoint`: surrogates and values past U+10FFFF become U+FFFD, C1 controls their Windows-1252 meaning.
pub(crate) fn replace_code_point(cp: u32) -> u32 {
    if (0xd800..=0xdfff).contains(&cp) || cp > 0x10ffff {
        return 0xfffd;
    }
    match cp {
        0 => 65533,
        128 => 8364,
        130 => 8218,
        131 => 402,
        132 => 8222,
        133 => 8230,
        134 => 8224,
        135 => 8225,
        136 => 710,
        137 => 8240,
        138 => 352,
        139 => 8249,
        140 => 338,
        142 => 381,
        145 => 8216,
        146 => 8217,
        147 => 8220,
        148 => 8221,
        149 => 8226,
        150 => 8211,
        151 => 8212,
        152 => 732,
        153 => 8482,
        154 => 353,
        155 => 8250,
        156 => 339,
        158 => 382,
        159 => 376,
        _ => cp,
    }
}
