//! Port of `text-buffer-iterators.zig`: line walks and coordinate conversions over a text buffer's rope.
//!
//! Offsets are rope weights: display cells plus one per line break. Rows and columns are logical lines and cells.

use crate::mem_registry::MemRegistry;
use crate::rope::{Metrics, Position, Tree};
use crate::segment::{Segment, TextChunk};
use crate::utf8::{self, ControlDisplay, WidthMethod};

/// One logical line of a walk (`LineInfo`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct LineInfo {
    pub line_idx: u32,
}

pub(crate) trait LineWalker {
    fn segment(&mut self, line_idx: u32, chunk: &TextChunk);
    fn line_end(&mut self, info: LineInfo);
}

/// `walkLineRange`: `count` lines from line `first`, whose line-start leaf and weight are `start`.
pub(crate) fn walk_line_range(tree: &Tree, first: u32, start: Position, count: u32, walker: &mut dyn LineWalker) {
    if tree.count() == 0 || count == 0 {
        return;
    }
    let mut line_idx = first;
    let mut col_offset = start.weight;
    let mut line_start_seg = start.leaf_index;
    let mut current_seg = start.leaf_index;
    let mut line_width = 0u32;
    let mut lines_left = count;
    tree.walk_from(start.leaf_index, &mut |segment, relative| {
        let idx = start.leaf_index + relative;
        match segment {
            Segment::Text(chunk) => {
                walker.segment(line_idx, chunk);
                line_width += chunk.width_cols();
            }
            Segment::Brk => {
                walker.line_end(LineInfo { line_idx });
                line_idx += 1;
                col_offset += line_width + 1;
                line_start_seg = idx + 1;
                line_width = 0;
                lines_left -= 1;
            }
            Segment::LineStart => {}
        }
        current_seg = idx + 1;
        lines_left > 0
    });
    if lines_left == 0 {
        return;
    }
    let had_breaks = line_idx > 0;
    let has_content_after_break = line_start_seg < current_seg;
    if has_content_after_break || had_breaks {
        walker.line_end(LineInfo { line_idx });
    }
}

pub(crate) fn walk_lines(tree: &Tree, walker: &mut dyn LineWalker) {
    walk_line_range(tree, 0, Position { leaf_index: 0, weight: 0 }, u32::MAX, walker);
}

pub(crate) fn line_count(tree: &Tree) -> u32 {
    tree.metrics().custom.linestart_count
}

pub(crate) fn max_line_width(tree: &Tree) -> u32 {
    tree.metrics().custom.line_width_max()
}

fn leaves(m: &Metrics) -> u32 {
    m.count
}
fn linestarts(m: &Metrics) -> u32 {
    m.custom.linestart_count
}
fn newlines(m: &Metrics) -> u32 {
    m.custom.newline_count
}
fn weight(m: &Metrics) -> u32 {
    m.weight()
}
fn cells(m: &Metrics) -> u32 {
    m.custom.total_width_cols
}

/// Line-start leaves among the first `leaf_index + 1` leaves.
#[allow(dead_code)]
pub(crate) fn line_starts_through_leaf(tree: &Tree, leaf_index: u32) -> u32 {
    tree.count_through(leaves, linestarts, leaf_index)
}

/// The line-start leaf of `row` and the weight before it.
pub(crate) fn line_start(tree: &Tree, row: u32) -> Option<Position> {
    tree.seek_count(linestarts, row)
}

fn line_width_from(tree: &Tree, row: u32, start: u32) -> u32 {
    match line_start(tree, row + 1) {
        Some(next) => next.weight - start - 1,
        None => tree.total_weight() - start,
    }
}

pub(crate) fn coords_to_offset(tree: &Tree, row: u32, col: u32) -> Option<u32> {
    let start = line_start(tree, row)?;
    if col > line_width_from(tree, row, start.weight) {
        return None;
    }
    Some(start.weight + col)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Coords {
    pub row: u32,
    pub col: u32,
}

/// Valid offsets are `0..=total_weight`; a break's offset belongs to the line it ends.
pub(crate) fn offset_to_coords(tree: &Tree, offset: u32) -> Option<Coords> {
    if offset > tree.total_weight() {
        return None;
    }
    let starts = tree.count_through(weight, linestarts, offset);
    let row = starts - 1;
    Some(Coords { row, col: offset - line_start(tree, row)?.weight })
}

pub(crate) fn line_width_at(tree: &Tree, row: u32) -> u32 {
    match line_start(tree, row) {
        Some(start) => line_width_from(tree, row, start.weight),
        None => 0,
    }
}

/// The first line whose end, in cells without breaks, is past `cell`; the last line if none.
pub(crate) fn first_line_ending_after(tree: &Tree, cell: u32) -> u32 {
    tree.count_through(cells, newlines, cell)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Bounds {
    pub start: u32,
    pub end: u32,
}

pub(crate) struct TextSettings {
    pub tab_width: u8,
    pub control_display: ControlDisplay,
    pub method: WidthMethod,
}

fn text_unit_bounds_at(tree: &Tree, registry: &MemRegistry, row: u32, col: u32, s: &TextSettings, graphemes: bool) -> Option<Bounds> {
    let line_width = line_width_at(tree, row);
    if col >= line_width {
        return None;
    }
    let mut seg_idx = line_start(tree, row)?.leaf_index + 1;
    let mut cols_before = 0;
    let count = tree.count();
    while seg_idx < count {
        let segment = tree.get(seg_idx)?;
        if segment.is_break() || segment.is_line_start() {
            break;
        }
        if let Some(chunk) = segment.as_text() {
            let next_cols = cols_before + chunk.width_cols();
            if col < next_cols {
                let local = col - cols_before;
                let bytes = chunk.bytes(registry);
                let ascii = chunk.is_ascii_only();
                let pos = if graphemes {
                    utf8::find_grapheme_pos_by_width(bytes, local, s.tab_width, s.control_display, ascii, false, s.method)
                } else {
                    utf8::find_pos_by_width(bytes, local, s.tab_width, s.control_display, ascii, false, s.method)
                };
                if pos.byte_offset as usize >= bytes.len() {
                    return None;
                }
                let width = if graphemes {
                    utf8::get_grapheme_width_at(bytes, pos.byte_offset as usize, s.tab_width, s.control_display, s.method)
                } else {
                    utf8::get_width_at(bytes, pos.byte_offset as usize, s.tab_width, s.control_display, s.method)
                };
                let start = cols_before + pos.columns_used;
                return Some(Bounds { start, end: start + width });
            }
            cols_before = next_cols;
        }
        seg_idx += 1;
    }
    None
}

pub(crate) fn grapheme_bounds_at(tree: &Tree, registry: &MemRegistry, row: u32, col: u32, s: &TextSettings) -> Option<Bounds> {
    text_unit_bounds_at(tree, registry, row, col, s, true)
}

pub(crate) fn cursor_unit_bounds_at(tree: &Tree, registry: &MemRegistry, row: u32, col: u32, s: &TextSettings) -> Option<Bounds> {
    text_unit_bounds_at(tree, registry, row, col, s, false)
}

pub(crate) fn grapheme_width_at(tree: &Tree, registry: &MemRegistry, row: u32, col: u32, s: &TextSettings) -> u32 {
    match cursor_unit_bounds_at(tree, registry, row, col, s) {
        Some(b) => b.end.saturating_sub(col),
        None => 0,
    }
}

pub(crate) fn prev_grapheme_width(tree: &Tree, registry: &MemRegistry, row: u32, col: u32, s: &TextSettings) -> u32 {
    if col == 0 {
        return 0;
    }
    let line_width = line_width_at(tree, row);
    let col = col.min(line_width);
    let Some(start) = line_start(tree, row) else { return 0 };
    let mut seg_idx = start.leaf_index + 1;
    let mut cols_before = 0;
    let mut prev_chunk: Option<&TextChunk> = None;
    let count = tree.count();
    let prev_of = |chunk: &TextChunk, end: Option<usize>| {
        let bytes = chunk.bytes(registry);
        utf8::get_prev_grapheme_start(bytes, end.unwrap_or(bytes.len()), s.tab_width, s.control_display, s.method)
    };
    while seg_idx < count {
        let Some(segment) = tree.get(seg_idx) else { break };
        if segment.is_break() || segment.is_line_start() {
            break;
        }
        if let Some(chunk) = segment.as_text() {
            let next_cols = cols_before + chunk.width_cols();
            if col <= next_cols {
                if col == cols_before
                    && let Some(pc) = prev_chunk
                {
                    return prev_of(pc, None).map_or(0, |r| r.width);
                }
                let bytes = chunk.bytes(registry);
                let local = col - cols_before;
                let here = utf8::find_pos_by_width(bytes, local, s.tab_width, s.control_display, chunk.is_ascii_only(), false, s.method);
                let start_col = here.columns_used;
                if start_col > local {
                    if let Some(pc) = prev_chunk
                        && let Some(r) = prev_of(pc, None)
                    {
                        return r.width;
                    }
                    return 0;
                }
                let into = local - start_col;
                let prev = prev_of(chunk, Some(here.byte_offset as usize));
                if into > 0 {
                    return match prev {
                        Some(r) => into + r.width,
                        None => into,
                    };
                }
                return prev.map_or(0, |r| r.width);
            }
            prev_chunk = Some(chunk);
            cols_before = next_cols;
        }
        seg_idx += 1;
    }
    0
}

/// `extractTextBetweenOffsets`: the bytes between two offsets, the start snapped back and the end forward to
/// grapheme boundaries; with `out == None` only counts them.
pub(crate) fn extract_text_between_offsets(
    tree: &Tree,
    registry: &MemRegistry,
    s: &TextSettings,
    start: u32,
    end: u32,
    mut out: Option<&mut Vec<u8>>,
) -> usize {
    if start >= end {
        return 0;
    }
    let Some((_, first_weight, first_index)) = tree.find_by_weight(start) else { return 0 };
    let mut col_offset = first_weight;
    let mut count = 0usize;
    tree.walk_from(first_index, &mut |segment, _| {
        if col_offset >= end {
            return false;
        }
        match segment {
            Segment::Text(chunk) => {
                let chunk_start = col_offset;
                let width = chunk.width_cols();
                let chunk_end = chunk_start + width;
                col_offset = chunk_end;
                if chunk_end <= start {
                    return true;
                }
                let bytes = chunk.bytes(registry);
                let ascii = chunk.is_ascii_only();
                let local_start = start.saturating_sub(chunk_start);
                let local_end = (end - chunk_start).min(width);
                let mut byte_start = 0u32;
                let mut byte_end = bytes.len() as u32;
                if local_start > 0 {
                    byte_start =
                        utf8::find_grapheme_pos_by_width(bytes, local_start, s.tab_width, s.control_display, ascii, false, s.method)
                            .byte_offset;
                }
                if local_end < width {
                    byte_end =
                        utf8::find_grapheme_pos_by_width(bytes, local_end, s.tab_width, s.control_display, ascii, true, s.method)
                            .byte_offset;
                }
                if byte_start < byte_end && (byte_start as usize) < bytes.len() {
                    let piece = &bytes[byte_start as usize..(byte_end as usize).min(bytes.len())];
                    count += piece.len();
                    if let Some(out) = out.as_deref_mut() {
                        out.extend_from_slice(piece);
                    }
                }
            }
            Segment::Brk => {
                if col_offset >= start {
                    count += 1;
                    if let Some(out) = out.as_deref_mut() {
                        out.push(b'\n');
                    }
                }
                col_offset += 1;
            }
            Segment::LineStart => {}
        }
        true
    });
    count
}
