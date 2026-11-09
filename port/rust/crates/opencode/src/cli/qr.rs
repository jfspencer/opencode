//! The QR Code encoder of `packages/util/src/qr.ts` (ISO/IEC 18004, byte mode, error correction level L, after
//! Project Nayuki's generator) and its `renderUnicodeCompact`: the same version, mask choice and modules.

const ECC_PER_BLOCK: [usize; 40] = [
    7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28, 30, 30, 26, 28, 30, 30, 30, 30,
    30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
];
const BLOCKS: [usize; 40] = [
    1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19,
    20, 21, 22, 24, 25,
];

fn mask(index: usize, x: usize, y: usize) -> bool {
    match index {
        0 => (x + y) % 2 == 0,
        1 => y % 2 == 0,
        2 => x % 3 == 0,
        3 => (x + y) % 3 == 0,
        4 => (x / 3 + y / 2) % 2 == 0,
        5 => (x * y) % 2 + (x * y) % 3 == 0,
        6 => ((x * y) % 2 + (x * y) % 3) % 2 == 0,
        _ => ((x + y) % 2 + (x * y) % 3) % 2 == 0,
    }
}

type Grid = Vec<Vec<bool>>;

/// `encode`: the smallest symbol that holds the UTF-8 text, as rows of modules (true is dark), or `None` when the
/// text is too long (TS throws a RangeError).
pub fn encode(text: &str) -> Option<Grid> {
    let bytes = text.as_bytes();
    let version = (1..=40).find(|&v| 4 + if v < 10 { 8 } else { 16 } + bytes.len() * 8 <= data_capacity(v) * 8)?;
    let size = version * 4 + 17;
    let mut modules = vec![vec![false; size]; size];
    let mut reserved = vec![vec![false; size]; size];
    draw_function_patterns(version, size, &mut |x, y, dark| {
        modules[y][x] = dark;
        reserved[y][x] = true;
    });
    draw_codewords(&mut modules, &reserved, &add_error_correction(&data_codewords(bytes, version), version));
    let candidates: Vec<Grid> = (0..8)
        .map(|index| {
            let mut masked: Grid = modules
                .iter()
                .enumerate()
                .map(|(y, row)| row.iter().enumerate().map(|(x, &dark)| dark != (!reserved[y][x] && mask(index, x, y))).collect())
                .collect();
            draw_format(index as u32, size, &mut |x, y, dark| masked[y][x] = dark);
            masked
        })
        .collect();
    let scores: Vec<i64> = candidates.iter().map(|c| penalty(c)).collect();
    let best = scores.iter().copied().min().unwrap_or(0);
    let chosen = scores.iter().position(|&s| s == best).unwrap_or(0);
    candidates.into_iter().nth(chosen)
}

/// `renderUnicodeCompact(text, { border })`: two modules per character in half blocks, light modules as blocks,
/// rows joined with "\n".
pub fn render_unicode_compact(text: &str, border: usize) -> Option<String> {
    let modules = add_border(&encode(text)?, border);
    let lines = modules.len().div_ceil(2);
    Some(
        (0..lines)
            .map(|line| {
                modules[line * 2]
                    .iter()
                    .enumerate()
                    .map(|(x, &top)| {
                        let bottom = modules.get(line * 2 + 1).map_or(true, |row| row[x]);
                        match (top, bottom) {
                            (true, true) => ' ',
                            (true, false) => '\u{2584}',
                            (false, true) => '\u{2580}',
                            (false, false) => '\u{2588}',
                        }
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn draw_function_patterns(version: usize, size: usize, set: &mut dyn FnMut(usize, usize, bool)) {
    for i in 0..size {
        set(6, i, i % 2 == 0);
        set(i, 6, i % 2 == 0);
    }
    for (cx, cy) in [(3i64, 3i64), (size as i64 - 4, 3), (3, size as i64 - 4)] {
        for dy in -4i64..=4 {
            for dx in -4i64..=4 {
                let (x, y) = (cx + dx, cy + dy);
                let distance = dx.abs().max(dy.abs());
                if x >= 0 && x < size as i64 && y >= 0 && y < size as i64 {
                    set(x as usize, y as usize, distance != 2 && distance != 4);
                }
            }
        }
    }
    let positions = alignment_positions(version, size);
    let last = positions.len().saturating_sub(1);
    for (i, &cx) in positions.iter().enumerate() {
        for (j, &cy) in positions.iter().enumerate() {
            // The three corners that hold finder patterns.
            if (i == 0 && j == 0) || (i == 0 && j == last) || (i == last && j == 0) {
                continue;
            }
            for dy in -2i64..=2 {
                for dx in -2i64..=2 {
                    set((cx as i64 + dx) as usize, (cy as i64 + dy) as usize, dx.abs().max(dy.abs()) != 1);
                }
            }
        }
    }
    // Reserves the format areas; the real format bits are drawn once the mask is chosen.
    draw_format(0, size, set);
    if version < 7 {
        return;
    }
    let bits = bch(version as u32, 12, 0x1f25);
    for i in 0..18 {
        let a = size - 11 + i % 3;
        let b = i / 3;
        let dark = (bits >> i) & 1 == 1;
        set(a, b, dark);
        set(b, a, dark);
    }
}

fn alignment_positions(version: usize, size: usize) -> Vec<usize> {
    if version == 1 {
        return vec![];
    }
    let count = version / 7 + 2;
    let step = if version == 32 { 26 } else { (version * 4 + 4).div_ceil(count * 2 - 2) * 2 };
    let mut out = vec![6];
    out.extend((0..count - 1).map(|i| size - 7 - (count - 2 - i) * step));
    out
}

fn draw_format(mask: u32, size: usize, set: &mut dyn FnMut(usize, usize, bool)) {
    // Level L is 0b01 in the format information.
    let bits = bch((1 << 3) | mask, 10, 0x537) ^ 0x5412;
    let bit = |i: usize| (bits >> i) & 1 == 1;
    for i in 0..=5 {
        set(8, i, bit(i));
    }
    set(8, 7, bit(6));
    set(8, 8, bit(7));
    set(7, 8, bit(8));
    for i in 9..15 {
        set(14 - i, 8, bit(i));
    }
    for i in 0..8 {
        set(size - 1 - i, 8, bit(i));
    }
    for i in 8..15 {
        set(8, size - 15 + i, bit(i));
    }
    set(8, size - 8, true);
}

/// The BCH remainder of `data` for a generator polynomial of the given degree, appended.
fn bch(data: u32, degree: u32, generator: u32) -> u32 {
    let mut remainder = data;
    for _ in 0..degree {
        remainder = (remainder << 1) ^ ((remainder >> (degree - 1)) * generator);
    }
    (data << degree) | remainder
}

fn data_codewords(bytes: &[u8], version: usize) -> Vec<u8> {
    let capacity = data_capacity(version);
    let mut bits: Vec<u8> = vec![];
    let append = |bits: &mut Vec<u8>, value: usize, length: usize| {
        for i in (0..length).rev() {
            bits.push(((value >> i) & 1) as u8);
        }
    };
    append(&mut bits, 0b0100, 4);
    append(&mut bits, bytes.len(), if version < 10 { 8 } else { 16 });
    for &byte in bytes {
        append(&mut bits, byte as usize, 8);
    }
    let terminator = 4.min(capacity * 8 - bits.len());
    append(&mut bits, 0, terminator);
    let pad = (8 - bits.len() % 8) % 8;
    append(&mut bits, 0, pad);
    let mut data: Vec<u8> = bits.chunks(8).map(|chunk| chunk.iter().fold(0u8, |byte, &bit| (byte << 1) | bit)).collect();
    let length = data.len();
    data.extend((0..capacity - length).map(|i| if i % 2 == 0 { 0xec } else { 0x11 }));
    data
}

fn add_error_correction(data: &[u8], version: usize) -> Vec<u8> {
    let blocks = BLOCKS[version - 1];
    let ecc = ECC_PER_BLOCK[version - 1];
    let raw = raw_modules(version) / 8;
    // The first `short` blocks hold one data codeword fewer than the rest.
    let short = blocks - raw % blocks;
    let short_length = raw / blocks - ecc;
    let divisor = reed_solomon_divisor(ecc);
    let split: Vec<&[u8]> = (0..blocks)
        .map(|i| {
            let start = i * short_length + i.saturating_sub(short);
            let end = (start + short_length + if i < short { 0 } else { 1 }).min(data.len());
            &data[start.min(data.len())..end]
        })
        .collect();
    let interleave = |rows: &[Vec<u8>], length: usize| -> Vec<u8> {
        (0..length).flat_map(|i| rows.iter().filter_map(move |row| row.get(i).copied())).collect()
    };
    let owned: Vec<Vec<u8>> = split.iter().map(|block| block.to_vec()).collect();
    let remainders: Vec<Vec<u8>> = split.iter().map(|block| reed_solomon_remainder(block, &divisor)).collect();
    let mut out = interleave(&owned, short_length + 1);
    out.extend(interleave(&remainders, ecc));
    out
}

fn draw_codewords(modules: &mut Grid, reserved: &Grid, codewords: &[u8]) {
    let size = modules.len();
    let mut i = 0usize;
    let mut right = size as i64 - 1;
    while right >= 1 {
        // The vertical timing pattern takes column 6, so the column pair shifts left past it.
        if right == 6 {
            right = 5;
        }
        for vertical in 0..size {
            for j in 0..2 {
                let x = (right - j) as usize;
                let y = if ((right + 1) & 2) == 0 { size - 1 - vertical } else { vertical };
                if reserved[y][x] || i >= codewords.len() * 8 {
                    continue;
                }
                modules[y][x] = (codewords[i >> 3] >> (7 - (i & 7))) & 1 == 1;
                i += 1;
            }
        }
        right -= 2;
    }
}

fn penalty(modules: &Grid) -> i64 {
    let size = modules.len();
    let columns: Grid = (0..size).map(|x| modules.iter().map(|row| row[x]).collect()).collect();
    let lines: i64 = modules.iter().chain(columns.iter()).map(|line| line_penalty(line)).sum();
    // Each 2x2 block of one colour, found from its bottom-right module.
    let mut boxes = 0i64;
    for y in 0..size - 1 {
        let row = &modules[y + 1];
        for x in 0..size - 1 {
            let dark = row[x + 1];
            if dark == row[x] && dark == modules[y][x] && dark == modules[y][x + 1] {
                boxes += 1;
            }
        }
    }
    let dark = modules.iter().flatten().filter(|&&d| d).count() as i64;
    let total = (size * size) as i64;
    let deviation = (dark * 20 - total * 10).abs();
    lines + boxes * 3 + ((deviation + total - 1) / total - 1) * 10
}

/// Runs of five or more same-coloured modules, and 1:1:3:1:1 finder-like patterns with 4 light modules on a side.
fn line_penalty(line: &[bool]) -> i64 {
    let size = line.len() as i64;
    let mut history = [0i64; 7];
    let push = |history: &mut [i64; 7], length: i64| {
        let first = history[0];
        history.copy_within(0..6, 1);
        history[0] = if first == 0 { length + size } else { length };
    };
    let finders = |history: &[i64; 7]| -> i64 {
        let n = history[1];
        let core = n > 0 && history[2] == n && history[3] == n * 3 && history[4] == n && history[5] == n;
        i64::from(core && history[0] >= n * 4 && history[6] >= n) + i64::from(core && history[6] >= n * 4 && history[0] >= n)
    };
    let mut result = 0i64;
    let mut color = false;
    let mut run = 0i64;
    for &dark in line {
        if dark == color {
            run += 1;
            if run == 5 {
                result += 3;
            }
            if run > 5 {
                result += 1;
            }
            continue;
        }
        push(&mut history, run);
        if !color {
            result += finders(&history) * 40;
        }
        color = dark;
        run = 1;
    }
    if color {
        push(&mut history, run);
        run = 0;
    }
    push(&mut history, run + size);
    result + finders(&history) * 40
}

fn reed_solomon_divisor(degree: usize) -> Vec<u8> {
    let mut result = vec![0u8; degree];
    result[degree - 1] = 1;
    let mut root = 1u8;
    for _ in 0..degree {
        for j in 0..degree {
            result[j] = multiply(result[j], root);
            if j + 1 < degree {
                result[j] ^= result[j + 1];
            }
        }
        root = multiply(root, 2);
    }
    result
}

fn reed_solomon_remainder(data: &[u8], divisor: &[u8]) -> Vec<u8> {
    let mut remainder = vec![0u8; divisor.len()];
    for &byte in data {
        let factor = byte ^ remainder[0];
        remainder.remove(0);
        remainder.push(0);
        for (i, value) in remainder.iter_mut().enumerate() {
            *value ^= multiply(divisor[i], factor);
        }
    }
    remainder
}

/// Multiplication in GF(2^8) modulo x^8 + x^4 + x^3 + x^2 + 1.
fn multiply(x: u8, y: u8) -> u8 {
    let mut z: u32 = 0;
    for i in (0..8).rev() {
        z = (z << 1) ^ ((z >> 7) * 0x11d);
        z ^= ((u32::from(y) >> i) & 1) * u32::from(x);
    }
    z as u8
}

fn raw_modules(version: usize) -> usize {
    let base = (16 * version + 128) * version + 64;
    if version < 2 {
        return base;
    }
    let count = version / 7 + 2;
    base - ((25 * count - 10) * count - 55) - if version >= 7 { 36 } else { 0 }
}

fn data_capacity(version: usize) -> usize {
    raw_modules(version) / 8 - ECC_PER_BLOCK[version - 1] * BLOCKS[version - 1]
}

fn add_border(modules: &Grid, border: usize) -> Grid {
    let width = modules.len() + border * 2;
    let mut out: Grid = (0..border).map(|_| vec![false; width]).collect();
    for row in modules {
        let mut padded = vec![false; border];
        padded.extend_from_slice(row);
        padded.extend(std::iter::repeat_n(false, border));
        out.push(padded);
    }
    out.extend((0..border).map(|_| vec![false; width]));
    out
}

#[cfg(test)]
mod tests {
    use super::render_unicode_compact;

    fn digest(text: &str) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The SHA-256 of the reference's `renderUnicodeCompact(text, { border })`, over versions 1 to 26.
    #[test]
    fn renders_as_the_reference() {
        let cases = [
            ("hello".to_owned(), 0, "a54a5b6d5fecac91c5c4ae879ffccdef53c08cb5a747afb6f0f8bef22dd0a532"),
            (
                r#"{"code":"AAAAAAAAAAAAAAAAAAAAAA","urls":["https://opencode.example.test"]}"#.to_owned(),
                1,
                "ce0d3f703c627bb5c7590aaf20e557d9b71dc0b99b936e883b87dd1eec47a725",
            ),
            ("é€😀 unicode".to_owned(), 2, "258e3a1b4138e6d72ccbd2685d33d47fca40da80fa470e04396e4ab1e5f8786f"),
            ("x".repeat(150), 0, "d84c4938dbc93cf2792a9f577c1356f0be4d09d52ab2a0786a62e2c9ab39283c"),
            ("y".repeat(400), 1, "382aade0856d4b4751527f7d1de98855d96d498f36bebbc23ee02e01ccac6dfc"),
            ("0123456789".repeat(120), 2, "64607080c29f0d5477d45931b5ec2faaa9e1d6341eeb75fa15ac69ca8de68fe2"),
        ];
        for (text, border, expected) in cases {
            assert_eq!(digest(&render_unicode_compact(&text, border).unwrap()), expected, "{text}");
        }
    }
}
