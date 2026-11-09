//! A reference draw of a text view into a plain cell grid, following `buffer.zig`'s `drawTextBufferInternal` for
//! a whole-buffer scissor, opacity 1 and colors that are opaque or fully transparent. It shows how a cell buffer
//! reads a view through [`DrawSource`], and lets the harness compare what views expose with what libopentui draws
//! (`OptimizedBuffer.getSpanLines()` after `drawTextBuffer`). Real drawing (blending, the glyph pool, links,
//! scissors) is `opencode-tui-core`'s.

use opencode_tui_text::utf8::{self, ControlDisplay};
use opencode_tui_text::{DrawSource, Rgba};

const WHITE: Rgba = [255, 255, 255, 255];
const SPACE: &[u8] = b" ";

#[derive(Clone)]
pub struct Cell {
    /// The glyph's bytes in its first cell; empty in the cells a wide glyph continues into.
    pub text: Vec<u8>,
    /// Cells of the glyph that starts here (0 for a continuation).
    pub width: u32,
    pub fg: Rgba,
    pub bg: Rgba,
    pub attributes: u32,
}

pub struct Grid {
    pub width: u32,
    pub height: u32,
    pub cells: Vec<Cell>,
}

impl Grid {
    /// `clear(bg)`.
    pub fn new(width: u32, height: u32, bg: Rgba) -> Grid {
        let cell = Cell { text: SPACE.to_vec(), width: 1, fg: WHITE, bg, attributes: 0 };
        Grid { width, height, cells: vec![cell; (width * height) as usize] }
    }

    fn index(&self, x: u32, y: u32) -> usize {
        (y * self.width + x) as usize
    }

    /// `set`, for a glyph of `width` cells: its first cell and the continuations.
    fn set(&mut self, x: u32, y: u32, text: &[u8], width: u32, fg: Rgba, bg: Rgba, attributes: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = self.index(x, y);
        self.cells[i] = Cell { text: text.to_vec(), width, fg, bg, attributes };
        for k in 1..width {
            if x + k >= self.width {
                break;
            }
            let j = self.index(x + k, y);
            self.cells[j] = Cell { text: Vec::new(), width: 0, fg, bg, attributes };
        }
    }

    /// A cell write with alpha blending, for opaque or fully transparent colors.
    fn blend_set(&mut self, x: u32, y: u32, text: &[u8], width: u32, fg: Rgba, bg: Rgba, attributes: u32) {
        if fg[3] & 0xff == 0 && bg[3] & 0xff == 0 {
            return;
        }
        if x >= self.width || y >= self.height {
            return;
        }
        let dest = self.cells[self.index(x, y)].clone();
        if bg[3] & 0xff == 0 {
            // A transparent space keeps a visible glyph under it.
            if text == SPACE && dest.width == 1 && dest.text != SPACE {
                return;
            }
            self.set(x, y, text, width, fg, dest.bg, attributes);
            return;
        }
        // A transparent foreground blends to the background it is drawn on.
        let fg = if fg[3] & 0xff == 0 { bg } else { fg };
        self.set(x, y, text, width, fg, bg, attributes);
    }

    /// `getSpanLines()`: per row, runs of cells with equal colors and base attributes.
    pub fn span_lines(&self) -> Vec<Vec<(String, Rgba, Rgba, u32, u32)>> {
        let mut lines = Vec::new();
        for y in 0..self.height {
            let mut spans: Vec<(String, Rgba, Rgba, u32, u32)> = Vec::new();
            for x in 0..self.width {
                let cell = &self.cells[self.index(x, y)];
                let text = String::from_utf8(cell.text.clone()).unwrap();
                let attributes = cell.attributes & 0xff;
                match spans.last_mut() {
                    Some(last) if last.1 == cell.fg && last.2 == cell.bg && last.3 == attributes => {
                        last.0.push_str(&text);
                        last.4 += 1;
                    }
                    _ => spans.push((text, cell.fg, cell.bg, attributes, 1)),
                }
            }
            lines.push(spans);
        }
        lines
    }
}

fn lower_bound<T>(items: &[T], key: impl Fn(&T) -> u32, value: u32) -> usize {
    items.partition_point(|item| key(item) < value)
}

fn is_printable_glyph(bytes: &[u8], width: u32) -> bool {
    if bytes.len() > 128 || width > 16 {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        if b < 0x20 || b == 0x7f {
            return false;
        }
        if b == 0xc2 && i + 1 < bytes.len() && bytes[i + 1] < 0xa0 {
            return false;
        }
    }
    true
}

fn sequence_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn alpha(c: Rgba) -> u16 {
    c[3] & 0xff
}

/// `drawTextBufferInternal` into `grid` at (`x`, `y`).
pub fn draw(src: &DrawSource, grid: &mut Grid, x: i32, y: i32) {
    if let Some(bg) = src.default_bg_fill()
        && let Some(vp) = src.viewport()
    {
        for row in 0..vp.height as i32 {
            for col in 0..vp.width as i32 {
                let (cx, cy) = (x + col, y + row);
                if cx >= 0 && cy >= 0 && (cx as u32) < grid.width && (cy as u32) < grid.height {
                    let cell = grid.cells[grid.index(cx as u32, cy as u32)].clone();
                    grid.set(cx as u32, cy as u32, SPACE, 1, cell.fg, bg, 0);
                }
            }
        }
    }
    let lines = src.virtual_lines();
    if lines.is_empty() {
        return;
    }
    let viewport = src.viewport();
    let (tab_width, control, method) = (src.tab_width(), src.control_display(), src.width_method());
    let indicator = src.tab_indicator();
    let mut indicator_bytes = Vec::new();
    if let Some(cp) = indicator {
        indicator_bytes = char::from_u32(cp).unwrap().to_string().into_bytes();
    }
    let indicator_width = if indicator_bytes.is_empty() {
        0
    } else {
        utf8::get_grapheme_width_at(&indicator_bytes, 0, tab_width, control, method)
    };
    let (dfg, dbg, dattr) = src.defaults();
    let prefill = src.default_bg_fill();
    let height = grid.height as i64;
    let count = lines.len() as i64;
    let first = (-(y as i64)).clamp(0, count) as usize;
    let last = (height - y as i64).clamp(0, count) as usize;
    if first >= last {
        return;
    }
    let hoff = viewport.map_or(0, |vp| vp.x);
    let vpw = viewport.map_or(u32::MAX, |vp| vp.width);
    let mut cur_y = y + first as i32;
    let selection = src.selection();
    let ellipsis_width = 3u32;
    for (slice_idx, vline) in lines[first..last].iter().enumerate() {
        if cur_y >= grid.height as i32 {
            break;
        }
        let vline_idx = viewport.map_or(0, |vp| vp.y) as usize + first + slice_idx;
        let pad = src.line_alignment_pad(vline_idx, vline.width_cols) as i32;
        let mut cur_x = x + pad;
        let mut rendered = 0u32;
        let mut doc = vline.document_cell_offset;
        let mut mark_base: Option<(u32, Vec<u8>, u32)> = None;
        let info = src.virtual_line_spans(vline_idx);
        let spans = info.spans;
        let col_offset = info.source_col_start;
        let default_fg = dfg.unwrap_or(WHITE);
        let default_bg = dbg.unwrap_or([0, 0, 0, 0]);
        let default_attr = dattr.unwrap_or(0);
        let (mut line_fg, mut line_bg, mut line_attr) = (default_fg, default_bg, default_attr);
        let start_col = col_offset + hoff;
        let mut span_idx = lower_bound(&spans[..spans.len().min(start_col as usize)], |s| s.next_col, start_col.saturating_add(1));
        let mut next_change = spans.get(span_idx).map_or(u32::MAX, |s| s.next_col);
        let apply = |id: u32, fg: &mut Rgba, bg: &mut Rgba, attr: &mut u32| {
            if let Some(style) = src.resolve_style(id) {
                if let Some(c) = style.fg {
                    *fg = c;
                }
                if let Some(c) = style.bg {
                    *bg = c;
                }
                *attr |= style.attributes;
            }
        };
        if span_idx < spans.len() && spans[span_idx].col <= start_col {
            apply(spans[span_idx].style_id, &mut line_fg, &mut line_bg, &mut line_attr);
        }
        for vchunk in &vline.chunks {
            let chunk = vchunk.chunk();
            let bytes = src.chunk_bytes(vchunk);
            let clusters = src.with_render_clusters(chunk, |c| c.to_vec());
            let line_col_offset = vline.document_cell_offset;
            if cur_x >= grid.width as i32 && mark_base.is_none() {
                doc += vchunk.width_cols;
                cur_x += vchunk.width_cols as i32;
                continue;
            }
            let col_end = vchunk.col_start_in_chunk + vchunk.width_cols;
            let mut col = vchunk.col_start_in_chunk;
            let mut byte_offset = vchunk.byte_start_in_chunk;
            let byte_end = vchunk.byte_start_in_chunk + vchunk.byte_len;
            let mut special = lower_bound(&clusters[..clusters.len().min(byte_offset as usize)], |c| c.byte_start, byte_offset);
            if rendered < hoff {
                if hoff - rendered >= vchunk.width_cols {
                    doc += vchunk.width_cols;
                    rendered += vchunk.width_cols;
                    continue;
                }
                let target = col + (hoff - rendered);
                let lo = special + lower_bound(&clusters[special..], |c| c.col_start, target);
                if lo > special {
                    byte_offset = clusters[lo - 1].byte_start;
                    col = clusters[lo - 1].col_start;
                    special = lo - 1;
                }
                doc += col - vchunk.col_start_in_chunk;
                rendered += col - vchunk.col_start_in_chunk;
            }
            while byte_offset < byte_end {
                if rendered < hoff {
                    let gap_end = clusters.get(special).map_or(byte_end, |c| c.byte_start.min(byte_end));
                    let take = (gap_end.saturating_sub(byte_offset)).min(hoff - rendered) as usize;
                    let gap = &bytes[byte_offset as usize..byte_offset as usize + take];
                    let ascii_len = if chunk.is_ascii_only() { gap.len() } else { utf8::printable_ascii_prefix_len(gap) } as u32;
                    byte_offset += ascii_len;
                    col += ascii_len;
                    doc += ascii_len;
                    rendered += ascii_len;
                }
                let at_special = special < clusters.len() && clusters[special].byte_start == byte_offset;
                let (glyph, width, ascii): (&[u8], u32, bool);
                if at_special {
                    let g = clusters[special];
                    if g.byte_start + g.byte_len > byte_end {
                        break;
                    }
                    glyph = &bytes[g.byte_start as usize..(g.byte_start + g.byte_len) as usize];
                    width = g.width_cols;
                    ascii = false;
                    byte_offset = g.byte_start + g.byte_len;
                    special += 1;
                } else {
                    if byte_offset >= byte_end || byte_offset as usize >= bytes.len() {
                        break;
                    }
                    let len = sequence_len(bytes[byte_offset as usize]) as u32;
                    let next = (byte_offset + len).min(byte_end);
                    glyph = &bytes[byte_offset as usize..next as usize];
                    ascii = glyph.len() == 1 && glyph[0] >= 0x20 && glyph[0] < 0x7f;
                    width = if ascii { 1 } else { utf8::get_width_at(glyph, 0, tab_width, control, method) };
                    byte_offset = next;
                }
                if width == 0 {
                    if let Some(base) = mark_base.clone()
                        && utf8::is_combining_mark(glyph)
                    {
                        mark_base = attach_mark(grid, glyph, base, cur_y as u32, method);
                    }
                    continue;
                }
                mark_base = None;
                if rendered + width <= hoff {
                    doc += width;
                    rendered += width;
                    col += width;
                    continue;
                }
                if rendered < hoff {
                    cur_x -= (hoff - rendered) as i32;
                }
                if rendered as u64 >= hoff as u64 + vpw as u64 {
                    doc += col_end - col;
                    break;
                }
                if cur_x + width as i32 <= 0 {
                    doc += width;
                    cur_x += width as i32;
                    rendered += width;
                    col += width;
                    continue;
                }
                if cur_x >= grid.width as i32 {
                    doc += col_end - col;
                    break;
                }
                let is_tab = glyph == b"\t";
                let mut is_blank = !ascii && !is_printable_glyph(glyph, width);
                if width > 1 && !is_blank {
                    is_blank = rendered < hoff
                        || rendered as u64 + width as u64 > hoff as u64 + vpw as u64
                        || cur_x < 0
                        || cur_x + width as i32 > grid.width as i32;
                }
                let mut selection_offset = doc;
                if vline.is_truncated && doc >= line_col_offset {
                    let c = doc - line_col_offset;
                    if c >= vline.ellipsis_col && c < vline.ellipsis_col + ellipsis_width {
                        selection_offset = line_col_offset + vline.ellipsis_col;
                    } else if c >= vline.ellipsis_col + ellipsis_width {
                        selection_offset = line_col_offset + vline.truncation_suffix_col_start + (c - vline.ellipsis_col - ellipsis_width);
                    } else {
                        selection_offset = line_col_offset + c;
                    }
                }
                let mut source_col = col_offset + rendered;
                if vline.is_truncated {
                    let c = doc - line_col_offset;
                    if c >= vline.ellipsis_col && c < vline.ellipsis_col + ellipsis_width {
                        source_col = u32::MAX;
                    } else if c >= vline.ellipsis_col + ellipsis_width {
                        source_col = vline.truncation_suffix_col_start + (c - vline.ellipsis_col - ellipsis_width);
                    }
                }
                while source_col >= next_change && span_idx + 1 < spans.len() {
                    span_idx += 1;
                    line_fg = default_fg;
                    line_bg = default_bg;
                    line_attr = default_attr;
                    apply(spans[span_idx].style_id, &mut line_fg, &mut line_bg, &mut line_attr);
                    next_change = spans[span_idx].next_col;
                }
                if vline.is_truncated {
                    let c = doc - line_col_offset;
                    if c >= vline.ellipsis_col && c < vline.ellipsis_col + ellipsis_width {
                        line_fg = default_fg;
                        line_bg = default_bg;
                        line_attr = default_attr;
                    } else if c >= vline.ellipsis_col + ellipsis_width {
                        let suffix_col = vline.truncation_suffix_col_start + (c - vline.ellipsis_col - ellipsis_width);
                        if spans.is_empty() {
                            line_fg = default_fg;
                            line_bg = default_bg;
                            line_attr = default_attr;
                            next_change = u32::MAX;
                        } else {
                            let idx = lower_bound(spans, |s| s.next_col, suffix_col.saturating_add(1));
                            if idx < spans.len() {
                                span_idx = idx;
                            }
                            line_fg = default_fg;
                            line_bg = default_bg;
                            line_attr = default_attr;
                            apply(spans[span_idx].style_id, &mut line_fg, &mut line_bg, &mut line_attr);
                            next_change = spans[span_idx].next_col;
                        }
                    }
                }
                let (mut fg, mut bg) = (line_fg, line_bg);
                if let Some(sel) = selection {
                    for k in 0..width {
                        if selection_offset + k >= sel.start && selection_offset + k < sel.end {
                            if let Some(sel_bg) = sel.bg_color {
                                bg = sel_bg;
                                if let Some(sel_fg) = sel.fg_color {
                                    fg = sel_fg;
                                }
                            } else {
                                fg = if alpha(line_bg) > 0 { line_bg } else { [0, 0, 0, 255] };
                                bg = line_fg;
                            }
                            break;
                        }
                    }
                }
                let attr = line_attr;
                if attr & (1 << 5) != 0 {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if let Some(p) = prefill
                    && bg == p
                {
                    bg[3] &= 0xff00;
                }
                if is_blank {
                    let (first_cp, _) = utf8::decode(glyph, 0);
                    let notation = utf8::control_notation_width(first_cp) == width;
                    let mut tab_col = 0;
                    while tab_col < width {
                        if rendered as u64 + tab_col as u64 >= hoff as u64 + vpw as u64 {
                            break;
                        }
                        let tx = cur_x + tab_col as i32;
                        if tx < 0 || rendered + tab_col < hoff {
                            tab_col += 1;
                            continue;
                        }
                        if tx >= grid.width as i32 {
                            break;
                        }
                        let mut text: Vec<u8> = if notation { vec![utf8::control_notation_glyph(first_cp, tab_col)] } else { SPACE.to_vec() };
                        let mut cell_fg = fg;
                        if is_tab
                            && tab_col == 0
                            && indicator_width > 0
                            && indicator_width <= width
                            && rendered as u64 + indicator_width as u64 <= hoff as u64 + vpw as u64
                        {
                            if indicator_width == 1 {
                                text = indicator_bytes.clone();
                                cell_fg = src.tab_indicator_color().unwrap_or(fg);
                            } else if indicator_width <= grid.width - tx as u32 {
                                grid.blend_set(tx as u32, cur_y as u32, &indicator_bytes, indicator_width, src.tab_indicator_color().unwrap_or(fg), bg, attr);
                                tab_col += indicator_width;
                                continue;
                            }
                        }
                        grid.blend_set(tx as u32, cur_y as u32, &text, 1, cell_fg, bg, attr);
                        tab_col += 1;
                    }
                } else if glyph.len() == 1 && width == 1 && glyph[0] >= 32 {
                    grid.blend_set(cur_x as u32, cur_y as u32, glyph, 1, fg, bg, attr);
                    if !(fg[3] & 0xff == 0 && bg[3] & 0xff == 0) {
                        mark_base = Some((cur_x as u32, glyph.to_vec(), 1));
                    }
                } else if !(fg[3] & 0xff == 0 && bg[3] & 0xff == 0) {
                    grid.blend_set(cur_x as u32, cur_y as u32, glyph, width, fg, bg, attr);
                    mark_base = Some((cur_x as u32, glyph.to_vec(), width));
                }
                doc += width;
                cur_x += width as i32;
                rendered += width;
                col += width;
            }
            if chunk.is_ellipsis() {
                mark_base = None;
            }
        }
        cur_y += 1;
    }
    let _ = ControlDisplay::Hidden;
}

/// `attachCombiningMark`: joins a zero-width mark to the glyph this draw just wrote.
fn attach_mark(grid: &mut Grid, mark: &[u8], base: (u32, Vec<u8>, u32), y: u32, method: utf8::WidthMethod) -> Option<(u32, Vec<u8>, u32)> {
    let (x, bytes, width) = base;
    let i = grid.index(x, y);
    let cell = grid.cells[i].clone();
    if cell.text != bytes || cell.width != width {
        return None;
    }
    let mut combined = bytes.clone();
    combined.extend_from_slice(mark);
    if combined.len() > 128 {
        return None;
    }
    if utf8::get_grapheme_width_at(&combined, 0, 1, ControlDisplay::Hidden, method) != width {
        return None;
    }
    grid.set(x, y, &combined, width, cell.fg, cell.bg, cell.attributes);
    Some((x, combined, width))
}
