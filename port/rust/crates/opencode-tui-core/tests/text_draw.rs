//! `OptimizedBuffer::draw_text_buffer` cell for cell against `opencode-tui-text`'s reference draw
//! (`examples/text-driver/draw.rs`, which the text crate's harness checks against libopentui's drawTextBuffer), on
//! generated views: mixed scripts, wide and zero-width clusters, controls, tabs and indicators, styles and links,
//! defaults, wrapping, viewports, truncation, alignment, selections and origins partly off the buffer. The
//! reference covers a whole-buffer scissor, opacity 1 and colors that are opaque or fully transparent, so the
//! cases stay there.
#[path = "../../opencode-tui-text/examples/text-driver/draw.rs"]
#[allow(dead_code)]
mod draw;

use opencode_tui_core::buffer::OptimizedBuffer;
use opencode_tui_core::color::RGBA;
use opencode_tui_core::pool;
use opencode_tui_text::{Rgba, StyledChunk, TextAlign, TextBuffer, TextBufferView, WidthMethod, WrapMode};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

const PIECES: &[&str] = &[
    "hello", "a", "word ", "  ", "\t", "中文", "字", "😀", "👩\u{200d}💻", "🇫🇷", "e\u{301}", "\u{301}", "\u{1}", "\u{7f}",
    "☺\u{fe0f}", "─", "é", "x\u{fe0f}", "ﾊ", "\u{200b}", "\u{0915}\u{094d}\u{0937}", "abc\u{0301}\u{0302}", "\n", "ab\ncd",
];

fn color(rng: &mut Rng) -> Option<Rgba> {
    match rng.below(4) {
        0 => None,
        1 => Some([0, 0, 0, 0]),
        _ => Some([rng.below(256) as u16, rng.below(256) as u16, rng.below(256) as u16, 255]),
    }
}

fn core_cell(buffer: &OptimizedBuffer, x: u32, y: u32) -> (Vec<u8>, u32, Rgba, Rgba, u32) {
    let cell = buffer.get(x, y).unwrap();
    let (text, width) = if pool::is_continuation_char(cell.char) {
        (Vec::new(), 0)
    } else if pool::is_grapheme_char(cell.char) {
        (pool::grapheme_bytes(pool::grapheme_id_from_char(cell.char)).unwrap_or_default(), pool::encoded_char_width(cell.char))
    } else {
        (char::from_u32(cell.char).unwrap().to_string().into_bytes(), 1)
    };
    (text, width, cell.fg.0, cell.bg.0, cell.attributes)
}

/// Draws one case; the number of cells it changed (to see that cases draw).
fn run_case(seed: u64) -> (u32, u32) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let method = *rng.pick(&[WidthMethod::Unicode, WidthMethod::Wcwidth, WidthMethod::NoZwj, WidthMethod::UnicodeWide]);
    let ctx = pool::context();
    let tb = TextBuffer::create(method, &ctx);
    let texts: Vec<String> = (0..1 + rng.below(4))
        .map(|_| (0..1 + rng.below(6)).map(|_| *rng.pick(PIECES)).collect::<String>())
        .collect();
    let links = ["https://example.com/a", "https://example.com/b"];
    let chunks: Vec<StyledChunk> = texts
        .iter()
        .map(|text| StyledChunk {
            text,
            fg: color(&mut rng),
            bg: color(&mut rng),
            attributes: rng.below(64) as u32,
            link: if rng.chance(15) { Some(*rng.pick(&links)) } else { None },
        })
        .collect();
    tb.set_styled_text(&chunks).unwrap();
    if rng.chance(40) {
        let attributes = if rng.chance(50) { Some(rng.below(64) as u32) } else { None };
        tb.set_all_defaults(color(&mut rng), color(&mut rng), attributes).unwrap();
    }
    let view = TextBufferView::create(&tb);
    view.set_wrap_mode(*rng.pick(&[WrapMode::None, WrapMode::Char, WrapMode::Word]));
    if rng.chance(60) {
        view.set_wrap_width(Some(1 + rng.below(20) as u32)).unwrap();
    }
    if rng.chance(40) {
        let (vx, vy) = (rng.below(4) as u32, rng.below(3) as u32);
        let (vw, vh) = (1 + rng.below(20) as u32, 1 + rng.below(6) as u32);
        view.set_viewport(vx, vy, vw, vh).unwrap();
    }
    if rng.chance(25) {
        view.set_truncate(true);
    }
    view.set_text_align(*rng.pick(&[TextAlign::Left, TextAlign::Center, TextAlign::Right]));
    if rng.chance(30) {
        view.set_tab_indicator(*rng.pick(&['→' as u32, '·' as u32, '中' as u32])).unwrap();
        if rng.chance(50) {
            view.set_tab_indicator_color(color(&mut rng).filter(|c| c[3] == 255));
        }
    }
    if rng.chance(40) {
        let start = rng.below(12) as u32;
        let end = start + rng.below(12) as u32;
        let bg = color(&mut rng).filter(|c| c[3] == 255);
        let fg = bg.and_then(|_| color(&mut rng).filter(|c| c[3] == 255));
        view.set_selection(start, end, bg, fg);
    }
    let (bw, bh) = (1 + rng.below(24) as u32, 1 + rng.below(8) as u32);
    let (x, y) = (rng.below(bw as u64 + 4) as i32 - 3, rng.below(bh as u64 + 3) as i32 - 2);

    let mut grid = draw::Grid::new(bw, bh, [0, 0, 0, 255]);
    let mut buffer = OptimizedBuffer::new(bw, bh, false, method);
    buffer.clear(RGBA([0, 0, 0, 255]));
    if x < bw as i32 && y < bh as i32 {
        view.draw(y, |src| draw::draw(src, &mut grid, x, y));
        view.draw(y, |src| buffer.draw_text_buffer(src, x, y));
    }
    let (mut changed, mut wide) = (0, 0);
    for row in 0..bh {
        for col in 0..bw {
            let want = &grid.cells[(row * bw + col) as usize];
            let want = (want.text.clone(), want.width, want.fg, want.bg, want.attributes);
            let got = core_cell(&buffer, col, row);
            if want.0 != b" " || want.3 != [0, 0, 0, 255] {
                changed += 1;
            }
            if want.1 != 1 || want.0.len() > 1 {
                wide += 1;
            }
            assert_eq!(
                got,
                want,
                "seed {seed}: cell ({col}, {row}) of {bw}x{bh} drawn at ({x}, {y}), {method:?}, texts {texts:?}",
            );
        }
    }
    (changed, wide)
}

#[test]
fn draws_text_views_as_the_reference_does() {
    let count = std::env::var("TEXT_DRAW_CASES").ok().and_then(|n| n.parse().ok()).unwrap_or(3000u64);
    let (mut changed, mut wide) = (0, 0);
    for seed in 1..=count {
        let (c, w) = run_case(seed);
        changed += c;
        wide += w;
    }
    eprintln!("{count} cases: {changed} cells drawn, {wide} of them wide, continued or multibyte");
    assert!(changed > count as u32 && wide > count as u32 / 10);
}
