//! `drawTextBuffer` / `drawEditorView`: `buffer.zig`'s `drawTextBufferInternal` over `opencode-tui-text`'s
//! [`DrawSource`] (the viewport's visual lines, chunk bytes and render clusters, style spans, selection,
//! alignment, truncation and tab indicator). Cells go through the buffer's blended write, so scissors, opacity
//! and alpha blending apply as in `setCellWithAlphaBlending`. `opencode-tui-text`'s
//! `examples/text-driver/draw.rs` is the same walk into a plain grid, checked cell for cell against libopentui.
//!
//! Divergence: a zero-width combining mark that follows a glyph this draw wrote is dropped instead of being
//! joined to it (`attachCombiningMark`); the text crate's views already cluster marks with their base, so only a
//! mark split from its base across chunks reaches this.
use opencode_tui_text::utf8;
use opencode_tui_text::{DrawSource, Rgba};

use crate::buffer::{OptimizedBuffer, is_printable_glyph, make_cell};
use crate::color::RGBA;
use crate::pool;

const WHITE: Rgba = [255, 255, 255, 255];

fn lower_bound<T>(items: &[T], key: impl Fn(&T) -> u32, value: u32) -> usize {
    items.partition_point(|item| key(item) < value)
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

impl OptimizedBuffer {
    /// One glyph of `width` cells, blended.
    fn blend_glyph(&mut self, x: i32, y: i32, glyph: &[u8], width: u32, fg: Rgba, bg: Rgba, attributes: u32) {
        if x < 0 || y < 0 {
            return;
        }
        let (fg, bg) = (RGBA(fg), RGBA(bg));
        let (x, y) = (x as u32, y as u32);
        if glyph.len() == 1 && width == 1 {
            self.set_cell_blended(x, y, make_cell(glyph[0] as u32, fg, bg, attributes));
            return;
        }
        if width == 1 {
            if let Ok(text) = std::str::from_utf8(glyph) {
                let mut chars = text.chars();
                if let (Some(c), None) = (chars.next(), chars.next()) {
                    self.set_cell_blended(x, y, make_cell(c as u32, fg, bg, attributes));
                    return;
                }
            }
        }
        if width > self.width().saturating_sub(x) {
            return;
        }
        let Some(id) = pool::acquire_grapheme(glyph) else { return };
        self.set_cell_blended(x, y, make_cell(pool::pack_grapheme_start(id, width), fg, bg, attributes));
        pool::release_grapheme(id);
    }

    /// `drawTextBuffer(view, x, y)` (and `drawEditorView`, whose source prefills the viewport background).
    pub fn draw_text_buffer(&mut self, src: &DrawSource, x: i32, y: i32) {
        if let Some(bg) = src.default_bg_fill()
            && let Some(vp) = src.viewport()
        {
            for row in 0..vp.height as i32 {
                for col in 0..vp.width as i32 {
                    let (cx, cy) = (x + col, y + row);
                    if let (true, Some(cell)) = (cx >= 0 && cy >= 0, self.get(cx.max(0) as u32, cy.max(0) as u32)) {
                        self.set_cell_blended(cx as u32, cy as u32, make_cell(32, cell.fg, RGBA(bg), 0));
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
            indicator_bytes = char::from_u32(cp).map(|c| c.to_string().into_bytes()).unwrap_or_default();
        }
        let indicator_width =
            if indicator_bytes.is_empty() { 0 } else { utf8::get_grapheme_width_at(&indicator_bytes, 0, tab_width, control, method) };
        let (dfg, dbg, dattr) = src.defaults();
        let prefill = src.default_bg_fill();
        let height = self.height() as i64;
        let grid_width = self.width() as i32;
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
            if cur_y >= self.height() as i32 {
                break;
            }
            let vline_idx = viewport.map_or(0, |vp| vp.y) as usize + first + slice_idx;
            let pad = src.line_alignment_pad(vline_idx, vline.width_cols) as i32;
            let mut cur_x = x + pad;
            let mut rendered = 0u32;
            let mut doc = vline.document_cell_offset;
            let info = src.virtual_line_spans(vline_idx);
            let spans = info.spans;
            let col_offset = info.source_col_start;
            let default_fg = dfg.unwrap_or(WHITE);
            let default_bg = dbg.unwrap_or([0, 0, 0, 0]);
            let default_attr = dattr.unwrap_or(0);
            let (mut line_fg, mut line_bg, mut line_attr) = (default_fg, default_bg, default_attr);
            let start_col = col_offset + hoff;
            let mut span_idx =
                lower_bound(&spans[..spans.len().min(start_col as usize)], |s| s.next_col, start_col.saturating_add(1));
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
                if cur_x >= grid_width {
                    doc += vchunk.width_cols;
                    cur_x += vchunk.width_cols as i32;
                    continue;
                }
                let col_end = vchunk.col_start_in_chunk + vchunk.width_cols;
                let mut col = vchunk.col_start_in_chunk;
                let mut byte_offset = vchunk.byte_start_in_chunk;
                let byte_end = vchunk.byte_start_in_chunk + vchunk.byte_len;
                let mut special =
                    lower_bound(&clusters[..clusters.len().min(byte_offset as usize)], |c| c.byte_start, byte_offset);
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
                        let ascii_len =
                            if chunk.is_ascii_only() { gap.len() } else { utf8::printable_ascii_prefix_len(gap) } as u32;
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
                        continue;
                    }
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
                    if cur_x >= grid_width {
                        doc += col_end - col;
                        break;
                    }
                    let is_tab = glyph == b"\t";
                    let mut is_blank = !ascii && !is_printable_glyph(glyph, width);
                    if width > 1 && !is_blank {
                        is_blank = rendered < hoff
                            || rendered as u64 + width as u64 > hoff as u64 + vpw as u64
                            || cur_x < 0
                            || cur_x + width as i32 > grid_width;
                    }
                    let mut selection_offset = doc;
                    if vline.is_truncated && doc >= line_col_offset {
                        let c = doc - line_col_offset;
                        if c >= vline.ellipsis_col && c < vline.ellipsis_col + ellipsis_width {
                            selection_offset = line_col_offset + vline.ellipsis_col;
                        } else if c >= vline.ellipsis_col + ellipsis_width {
                            selection_offset = line_col_offset
                                + vline.truncation_suffix_col_start
                                + (c - vline.ellipsis_col - ellipsis_width);
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
                            let suffix_col =
                                vline.truncation_suffix_col_start + (c - vline.ellipsis_col - ellipsis_width);
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
                            if tx >= grid_width {
                                break;
                            }
                            let mut text: Vec<u8> =
                                if notation { vec![utf8::control_notation_glyph(first_cp, tab_col)] } else { b" ".to_vec() };
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
                                } else if indicator_width <= grid_width as u32 - tx as u32 {
                                    let color = src.tab_indicator_color().unwrap_or(fg);
                                    self.blend_glyph(tx, cur_y, &indicator_bytes, indicator_width, color, bg, attr);
                                    tab_col += indicator_width;
                                    continue;
                                }
                            }
                            self.blend_glyph(tx, cur_y, &text, 1, cell_fg, bg, attr);
                            tab_col += 1;
                        }
                    } else {
                        self.blend_glyph(cur_x, cur_y, glyph, width, fg, bg, attr);
                    }
                    doc += width;
                    cur_x += width as i32;
                    rendered += width;
                    col += width;
                }
            }
            cur_y += 1;
        }
    }
}
