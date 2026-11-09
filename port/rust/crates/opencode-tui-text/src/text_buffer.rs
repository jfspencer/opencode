//! Port of `text-buffer.zig` (`UnifiedTextBuffer`): a document as a rope of line-start markers, text chunks and
//! line breaks, with per-line highlights and their precomputed style spans.
//!
//! Divergences: the arena and the per-view dirty flags are gone (reference counting frees replaced ropes; views
//! compare roots to tell that the text changed). Every registered slot owns a copy of its bytes.

use crate::iterators::{self, Bounds, LineInfo, LineWalker, TextSettings};
use crate::link::{self, LinkPool, LinkTracker};
use crate::mem_registry::{MemRegistry, RegistryFull};
use crate::rope::{Rope, Tree};
use crate::segment::{Highlight, Segment, StyleSpan, TextChunk, ASCII_ONLY, HAS_TAB};
use crate::syntax_style::{StyleDefinition, SyntaxStyle, ID_LIMIT};
use crate::utf8::{self, ControlDisplay, TextWidthCursor, WidthMethod};
use crate::{Rgba, TextError};
use indexmap::IndexSet;
use std::cell::RefCell;
use std::rc::Rc;

/// Style ids with this bit index the buffer's run styles (`run_style_flag`).
pub const RUN_STYLE_FLAG: u32 = ID_LIMIT;

/// A run of styled text: a byte count and its style (`OwnedStyledChunk`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct OwnedStyledChunk {
    pub byte_count: u32,
    pub style: StyleDefinition,
}

pub(crate) struct TextBufferCore {
    /// Tells buffers apart for views that switch between them (an editor's placeholder).
    pub(crate) id: u64,
    pub(crate) mem_registry: MemRegistry,
    pub(crate) default_fg: Option<Rgba>,
    pub(crate) default_bg: Option<Rgba>,
    pub(crate) default_attributes: Option<u32>,
    pub(crate) rope: Rope,
    style: Option<SyntaxStyle>,
    link_pool: Rc<RefCell<LinkPool>>,
    link_tracker: Option<LinkTracker>,
    pub(crate) width_method: WidthMethod,
    pub(crate) control_display: ControlDisplay,
    content_epoch: u64,
    line_highlights: Vec<Vec<Highlight>>,
    line_spans: Vec<Vec<StyleSpan>>,
    internal_highlight_count: usize,
    run_styles: IndexSet<StyleDefinition>,
    run_links_retired: bool,
    pub(crate) tab_width: u8,
    pub(crate) tab_metrics_generation: u64,
}

impl Drop for TextBufferCore {
    fn drop(&mut self) {
        if let Some(mut tracker) = self.link_tracker.take() {
            tracker.clear(&mut self.link_pool.borrow_mut());
        }
    }
}

impl TextBufferCore {
    pub fn new(width_method: WidthMethod, control_display: ControlDisplay, link_pool: Rc<RefCell<LinkPool>>) -> Self {
        thread_local!(static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) });
        let id = NEXT_ID.with(|next| {
            let id = next.get();
            next.set(id + 1);
            id
        });
        TextBufferCore {
            id,
            mem_registry: MemRegistry::default(),
            default_fg: None,
            default_bg: None,
            default_attributes: None,
            rope: Rope::new(),
            style: None,
            link_pool,
            link_tracker: None,
            width_method,
            control_display,
            content_epoch: 0,
            line_highlights: Vec::new(),
            line_spans: Vec::new(),
            internal_highlight_count: 0,
            run_styles: IndexSet::new(),
            run_links_retired: false,
            tab_width: 2,
            tab_metrics_generation: 1,
        }
    }

    pub fn tree(&self) -> &Tree {
        &self.rope.tree
    }

    pub fn settings(&self) -> TextSettings {
        TextSettings { tab_width: self.tab_width, control_display: self.control_display, method: self.width_method }
    }

    pub fn content_epoch(&self) -> u64 {
        self.content_epoch
    }

    /// Restore an undo root with metrics valid for the current tab width.
    pub fn undo(&mut self, meta: &str) -> Option<String> {
        let previous = self.rope.undo(meta)?;
        self.refresh_tab_width_metrics();
        Some(previous)
    }

    pub fn redo(&mut self) -> Option<String> {
        let next = self.rope.redo()?;
        self.refresh_tab_width_metrics();
        Some(next)
    }

    pub fn line_width_at(&self, row: u32) -> u32 {
        iterators::line_width_at(self.tree(), row)
    }

    pub fn line_width_cols_max(&self) -> u32 {
        iterators::max_line_width(self.tree())
    }

    pub fn grapheme_width_at(&self, row: u32, col: u32) -> u32 {
        iterators::grapheme_width_at(self.tree(), &self.mem_registry, row, col, &self.settings())
    }

    pub fn prev_grapheme_width(&self, row: u32, col: u32) -> u32 {
        iterators::prev_grapheme_width(self.tree(), &self.mem_registry, row, col, &self.settings())
    }

    pub fn text_end_offset(&self) -> u32 {
        self.tree().total_weight()
    }

    fn bounds_at_offset(&self, offset: u32, graphemes: bool) -> Option<Bounds> {
        let tree = self.tree();
        let coords = iterators::offset_to_coords(tree, offset)?;
        let s = self.settings();
        let bounds = if graphemes {
            iterators::grapheme_bounds_at(tree, &self.mem_registry, coords.row, coords.col, &s)?
        } else {
            iterators::cursor_unit_bounds_at(tree, &self.mem_registry, coords.row, coords.col, &s)?
        };
        let line_start = offset - coords.col;
        Some(Bounds { start: line_start + bounds.start, end: line_start + bounds.end })
    }

    pub fn grapheme_bounds_at_offset(&self, offset: u32) -> Option<Bounds> {
        self.bounds_at_offset(offset, true)
    }

    pub fn cursor_unit_bounds_at_offset(&self, offset: u32) -> Option<Bounds> {
        self.bounds_at_offset(offset, false)
    }

    pub fn mark_all_views_dirty(&mut self) {
        self.rope.set_metrics_generation(self.tab_metrics_generation);
        self.content_epoch = self.content_epoch.wrapping_add(1);
    }

    pub fn length(&self) -> u32 {
        self.tree().metrics().custom.total_width_cols
    }

    pub fn byte_size(&self) -> u32 {
        let total = self.tree().metrics().custom.total_bytes;
        let lines = iterators::line_count(self.tree());
        if lines > 0 { total + (lines - 1) } else { total }
    }

    pub fn measure_text(&self, text: &[u8]) -> u32 {
        utf8::calculate_text_width(text, self.tab_width, self.control_display, utf8::is_ascii_only(text), self.width_method)
    }

    /// Clear the text, keeping highlights, registered bytes and history.
    pub fn clear(&mut self) {
        self.clear_with_undo(None);
    }

    pub fn clear_with_undo(&mut self, meta: Option<&str>) {
        self.rope.clear_with_undo(meta);
        self.clear_link_refs();
        self.mark_all_views_dirty();
    }

    pub fn reset(&mut self) {
        self.clear_link_refs();
        self.line_highlights.clear();
        self.internal_highlight_count = 0;
        self.drop_run_styles();
        self.line_spans.clear();
        self.rope = Rope::new();
        self.mem_registry.clear();
        self.mark_all_views_dirty();
    }

    pub fn set_syntax_style(&mut self, style: Option<SyntaxStyle>) {
        self.style = style;
    }

    fn clear_link_refs(&mut self) {
        if let Some(tracker) = self.link_tracker.as_mut() {
            tracker.clear(&mut self.link_pool.borrow_mut());
        }
        self.run_links_retired = !self.run_styles.is_empty();
    }

    fn drop_run_styles(&mut self) {
        self.run_styles = IndexSet::new();
        self.run_links_retired = false;
    }

    /// `resolveStyle`: run refs through the run styles, other ids through the attached syntax style.
    pub fn resolve_style(&self, id: u32) -> Option<StyleDefinition> {
        if id == 0 {
            return None;
        }
        if id & RUN_STYLE_FLAG == 0 {
            return self.style.as_ref()?.core.borrow().resolve_by_id(id);
        }
        let index = id & !RUN_STYLE_FLAG;
        let mut definition = *self.run_styles.get_index(index as usize - 1)?;
        if self.run_links_retired {
            definition.attributes = link::attributes::set_link_id(definition.attributes, 0);
        }
        Some(definition)
    }

    fn create_chunk_from_bytes(&self, mem_id: u8, bytes: &[u8], byte_start: u32) -> TextChunk {
        let ascii = utf8::is_ascii_only(bytes);
        let mut flags = 0;
        if !bytes.is_empty() && ascii {
            flags |= ASCII_ONLY;
        }
        if !ascii && bytes.contains(&b'\t') {
            flags |= HAS_TAB;
        }
        let width = utf8::calculate_text_width(bytes, self.tab_width, self.control_display, ascii, self.width_method);
        TextChunk::new(mem_id, flags, byte_start, byte_start + bytes.len() as u32, width)
    }

    /// `createChunk`.
    pub fn create_chunk(&self, mem_id: u8, byte_start: u32, byte_end: u32) -> TextChunk {
        let data = self.mem_registry.get(mem_id).expect("live slot");
        self.create_chunk_from_bytes(mem_id, &data[byte_start as usize..byte_end as usize], byte_start)
    }

    /// `textToSegments`: one chunk per line, with breaks and line-start markers between.
    pub fn text_to_segments(&self, text: &[u8], mem_id: u8, byte_offset: u32, prepend_linestart: bool) -> Vec<Segment> {
        let mut breaks = Vec::new();
        utf8::find_line_breaks(text, &mut breaks);
        let mut segments = Vec::with_capacity(breaks.len() * 3 + 2);
        if prepend_linestart {
            segments.push(Segment::LineStart);
        }
        let mut local_start = 0usize;
        for line_break in &breaks {
            let local_end = match line_break.kind {
                utf8::LineBreakKind::CrLf => line_break.pos - 1,
                _ => line_break.pos,
            };
            if local_end > local_start {
                segments.push(Segment::Text(self.create_chunk_from_bytes(
                    mem_id,
                    &text[local_start..local_end],
                    byte_offset + local_start as u32,
                )));
            }
            segments.push(Segment::Brk);
            segments.push(Segment::LineStart);
            local_start = line_break.pos + 1;
        }
        if local_start < text.len() {
            segments.push(Segment::Text(self.create_chunk_from_bytes(mem_id, &text[local_start..], byte_offset + local_start as u32)));
        }
        segments
    }

    pub fn line_count(&self) -> u32 {
        if self.tree().count() == 0 {
            return 0;
        }
        iterators::line_count(self.tree())
    }

    /// `replaceText` / `replaceOwnedText`: the whole text in slot `mem_id` (registered when not live); clears
    /// history. Returns the slot.
    pub fn replace_text(&mut self, text: Vec<u8>, mem_id: Option<u8>) -> Result<u8, TextError> {
        let reuse = mem_id.is_some_and(|id| self.mem_registry.get(id).is_some());
        let id = if reuse { mem_id.unwrap() } else { self.mem_registry.register(Vec::new()).map_err(|_| TextError::OutOfMemory)? };
        let segments = self.text_to_segments(&text, id, 0, true);
        self.set_plain_text_segments(segments, None, true);
        self.mem_registry.replace(id, text);
        self.clear_link_refs();
        Ok(id)
    }

    /// `setTextFromMemRangeWithUndo`: the text becomes `start..end` of slot `mem_id`, storing an undo step.
    pub fn set_text_from_mem_range_with_undo(&mut self, mem_id: u8, start: u32, end: u32, meta: &str) {
        let bytes = self.mem_registry.get(mem_id).expect("live slot");
        let bytes = bytes[start as usize..end as usize].to_vec();
        let segments = self.text_to_segments(&bytes, mem_id, start, true);
        self.set_plain_text_segments(segments, Some(meta), false);
        self.clear_link_refs();
    }

    /// The common part of `setPlainText`.
    fn set_plain_text_segments(&mut self, segments: Vec<Segment>, meta: Option<&str>, replace_rope: bool) {
        let candidate = if replace_rope || (meta.is_none() && !self.rope.has_history()) {
            None
        } else {
            Some(if segments.is_empty() { self.rope.tree.cleared() } else { Tree::from_segments(&segments) })
        };
        let new_tree = Tree::from_segments(&segments);
        let line_total = new_tree.metrics().custom.linestart_count;
        let line_count = if self.highlight_count() == 0 { 0 } else { self.line_highlights.len().min(line_total as usize) };
        let mut highlights: Vec<Vec<Highlight>> = Vec::with_capacity(line_count);
        let mut spans: Vec<Vec<StyleSpan>> = Vec::with_capacity(line_count);
        for line in &self.line_highlights[..line_count] {
            let kept: Vec<Highlight> = line.iter().filter(|h| !h.internal).copied().collect();
            spans.push(build_line_spans(&kept));
            highlights.push(kept);
        }
        if let Some(meta) = meta {
            self.rope.store_undo(meta);
        }
        match candidate {
            None => {
                // A new rope: the replacement drops the history.
                self.rope = Rope::with_tree(new_tree);
            }
            Some(tree) => {
                self.rope.tree.root = tree.root;
            }
        }
        self.internal_highlight_count = 0;
        self.drop_run_styles();
        self.line_highlights = highlights;
        self.line_spans = spans;
        self.mark_all_views_dirty();
    }

    /// `prepareOwnedStyledText` + `commit`: the text with one run style per chunk; drops user highlights.
    pub fn replace_owned_styled_text(
        &mut self,
        text: Vec<u8>,
        mem_id: Option<u8>,
        chunks: &[OwnedStyledChunk],
        links: Option<LinkTracker>,
    ) -> Result<u8, TextError> {
        let reuse = mem_id.is_some_and(|id| self.mem_registry.get(id).is_some());
        let id = if reuse { mem_id.unwrap() } else { self.mem_registry.register(Vec::new()).map_err(|_| TextError::OutOfMemory)? };
        let segments = self.text_to_segments(&text, id, 0, true);
        let tree = Tree::from_segments(&segments);
        let line_count = tree.metrics().custom.linestart_count as usize;
        let mut highlights: Vec<Vec<Highlight>> = Vec::with_capacity(line_count);
        let mut spans: Vec<Vec<StyleSpan>> = Vec::with_capacity(line_count);
        let mut run_styles = IndexSet::new();
        let mut cursor = TextWidthCursor::new(&text, self.tab_width, self.control_display, self.width_method);
        let mut chunk_index = 0;
        let mut byte_offset = 0usize;
        let mut remaining = 0u32;
        let mut style_id = 0u32;
        let mut internal = 0usize;
        for segment in &segments {
            if segment.is_line_start() {
                highlights.push(Vec::new());
                spans.push(Vec::new());
            }
            let Some(line) = segment.as_text() else { continue };
            let line_index = highlights.len() - 1;
            let width = line.width_cols();
            let mut col = 0u32;
            while col < width {
                while remaining == 0 && chunk_index < chunks.len() {
                    let chunk = chunks[chunk_index];
                    let byte_end = byte_offset + chunk.byte_count as usize;
                    let before = cursor.columns;
                    remaining = cursor.advance_to(byte_end) - before;
                    style_id = if remaining == 0 { 0 } else { intern_run_style(&mut run_styles, chunk.style) };
                    byte_offset = byte_end;
                    chunk_index += 1;
                }
                if remaining == 0 {
                    break;
                }
                let w = remaining.min(width - col);
                highlights[line_index].push(Highlight {
                    col_start: col,
                    col_end: col + w,
                    style_id,
                    priority: 1,
                    hl_ref: 0,
                    internal: true,
                });
                spans[line_index].push(StyleSpan { col, next_col: col + w, style_id });
                internal += 1;
                remaining -= w;
                col += w;
            }
            if col > 0 {
                spans[line_index].push(StyleSpan { col, next_col: u32::MAX, style_id: 0 });
            }
        }
        self.run_styles = run_styles;
        self.run_links_retired = false;
        self.rope = Rope::with_tree(tree);
        self.line_highlights = highlights;
        self.line_spans = spans;
        self.internal_highlight_count = internal;
        self.mark_all_views_dirty();
        let retired = std::mem::replace(&mut self.link_tracker, links);
        self.mem_registry.replace(id, text);
        if let Some(mut tracker) = retired {
            tracker.clear(&mut self.link_pool.borrow_mut());
        }
        Ok(id)
    }

    pub fn link_pool(&self) -> &Rc<RefCell<LinkPool>> {
        &self.link_pool
    }

    /// `appendBytes(.copy)`: a grapheme that the last chunk and `text` split moves whole into the appended copy.
    pub fn append_copy(&mut self, text: &[u8]) -> Result<(), TextError> {
        if text.is_empty() {
            return Ok(());
        }
        let mut candidate = self.rope.tree.clone();
        let mut tail: Vec<u8> = Vec::new();
        let last_index = candidate.count().saturating_sub(1);
        if let Some(Segment::Text(chunk)) = candidate.get(last_index) {
            let chunk = chunk.clone();
            let bytes = chunk.bytes(&self.mem_registry);
            if let Some((at_tail, _)) = crate::segment::find_junction(bytes, text, self.width_method) {
                tail = bytes[at_tail as usize..].to_vec();
                if at_tail == 0 {
                    candidate.delete(last_index);
                } else {
                    let left = self.create_chunk(chunk.mem_id, chunk.byte_start, chunk.byte_start + at_tail);
                    candidate.replace(last_index, Segment::Text(left));
                }
            }
        }
        let mut data = tail;
        data.extend_from_slice(text);
        let mem_id = self.mem_registry.register(data).map_err(|_: RegistryFull| TextError::OutOfMemory)?;
        let bytes = self.mem_registry.get(mem_id).unwrap().to_vec();
        let segments = self.text_to_segments(&bytes, mem_id, 0, false);
        let count = candidate.count();
        candidate.insert_slice(count, &segments);
        self.rope.tree.root = candidate.root;
        self.mark_all_views_dirty();
        Ok(())
    }

    /// `flattenMemRegistry`: every chunk's bytes, in order, in one slot 0; chunk boundaries and widths are kept.
    pub fn flatten_mem_registry(&mut self) {
        let byte_count = self.tree().metrics().custom.total_bytes as usize;
        let mut bytes = Vec::with_capacity(byte_count);
        let mut segments = self.tree().to_vec();
        for segment in segments.iter_mut() {
            if let Segment::Text(chunk) = segment {
                let start = bytes.len() as u32;
                bytes.extend_from_slice(chunk.bytes(&self.mem_registry));
                *chunk = TextChunk::new(0, chunk.flags, start, bytes.len() as u32, chunk.width_cols());
            }
        }
        self.rope = Rope::with_tree(Tree::from_segments(&segments));
        self.mem_registry.clear();
        self.mem_registry.register(bytes).expect("an empty registry");
        self.mark_all_views_dirty();
    }

    pub fn plain_text(&self) -> Vec<u8> {
        struct Collect<'a> {
            registry: &'a MemRegistry,
            out: Vec<u8>,
            line_count: u32,
        }
        impl LineWalker for Collect<'_> {
            fn segment(&mut self, _line: u32, chunk: &TextChunk) {
                self.out.extend_from_slice(chunk.bytes(self.registry));
            }
            fn line_end(&mut self, info: LineInfo) {
                if self.line_count > 0 && info.line_idx < self.line_count - 1 {
                    self.out.push(b'\n');
                }
            }
        }
        let mut walker =
            Collect { registry: &self.mem_registry, out: Vec::with_capacity(self.byte_size() as usize), line_count: self.line_count() };
        iterators::walk_lines(self.tree(), &mut walker);
        walker.out
    }

    /// `commitHighlights` for one highlight.
    fn commit_highlight(&mut self, line: usize, highlight: Highlight) {
        while self.line_highlights.len() <= line {
            self.line_highlights.push(Vec::new());
        }
        while self.line_spans.len() <= line {
            self.line_spans.push(Vec::new());
        }
        self.line_highlights[line].push(highlight);
        self.line_spans[line] = build_line_spans(&self.line_highlights[line]);
    }

    pub fn add_highlight(&mut self, line: usize, col_start: u32, col_end: u32, style_id: u32, priority: u8, hl_ref: u16) -> Result<(), TextError> {
        if line >= self.line_count() as usize {
            return Err(TextError::InvalidIndex);
        }
        if col_start >= col_end {
            return Ok(());
        }
        self.commit_highlight(line, Highlight { col_start, col_end, style_id, priority, hl_ref, internal: false });
        Ok(())
    }

    /// `addHighlightByCharRange`: offsets in cells that exclude line breaks.
    pub fn add_highlight_by_char_range(&mut self, char_start: u32, char_end: u32, style_id: u32, priority: u8, hl_ref: u16) -> Result<(), TextError> {
        if char_start >= char_end || self.line_count() == 0 {
            return Ok(());
        }
        let tree = self.tree();
        let first_row = iterators::first_line_ending_after(tree, char_start);
        let first = iterators::line_start(tree, first_row).expect("a line");
        let mut additions: Vec<(usize, Highlight)> = Vec::new();
        let mut row = first_row;
        let mut line_weight = first.weight;
        let mut weight = first.weight;
        let mut done = false;
        let end_line = |row: u32, line_weight: u32, weight: u32, additions: &mut Vec<(usize, Highlight)>| -> bool {
            let line_start = line_weight - row;
            let width = weight - line_weight;
            let done = line_start + width >= char_end;
            if line_start >= char_end {
                return done;
            }
            let col_start = char_start.saturating_sub(line_start);
            let col_end = (char_end - line_start).min(width);
            if col_start < col_end {
                additions.push((row as usize, Highlight { col_start, col_end, style_id, priority, hl_ref, internal: false }));
            }
            done
        };
        tree.walk_from(first.leaf_index, &mut |segment, _| {
            if !segment.is_break() {
                weight += segment.measure().weight();
                return true;
            }
            done = end_line(row, line_weight, weight, &mut additions);
            row += 1;
            weight += 1;
            line_weight = weight;
            !done
        });
        if !done {
            end_line(row, line_weight, weight, &mut additions);
        }
        for (line, highlight) in additions {
            self.commit_highlight(line, highlight);
        }
        Ok(())
    }

    pub fn remove_highlights_by_ref(&mut self, hl_ref: u16) {
        for line in 0..self.line_highlights.len() {
            if !self.line_highlights[line].iter().any(|h| h.hl_ref == hl_ref) {
                continue;
            }
            let mut removed_internal = 0;
            self.line_highlights[line].retain(|h| {
                if h.hl_ref == hl_ref {
                    if h.internal {
                        removed_internal += 1;
                    }
                    false
                } else {
                    true
                }
            });
            self.internal_highlight_count = self.internal_highlight_count.saturating_sub(removed_internal);
            self.line_spans[line] = build_line_spans(&self.line_highlights[line]);
        }
    }

    pub fn clear_line_highlights(&mut self, line: usize) {
        if let Some(list) = self.line_highlights.get_mut(line) {
            let internal = list.iter().filter(|h| h.internal).count();
            self.internal_highlight_count = self.internal_highlight_count.saturating_sub(internal);
            list.clear();
        }
        if let Some(spans) = self.line_spans.get_mut(line) {
            spans.clear();
        }
    }

    pub fn clear_all_highlights(&mut self) {
        for list in &mut self.line_highlights {
            list.clear();
        }
        self.internal_highlight_count = 0;
        for spans in &mut self.line_spans {
            spans.clear();
        }
    }

    pub fn line_highlights(&self, line: usize) -> &[Highlight] {
        self.line_highlights.get(line).map_or(&[], |l| &l[..])
    }

    pub fn line_spans(&self, line: usize) -> &[StyleSpan] {
        self.line_spans.get(line).map_or(&[], |l| &l[..])
    }

    pub fn highlight_count(&self) -> u32 {
        self.line_highlights.iter().map(|l| l.len() as u32).sum()
    }

    /// Set the tab width, rounded up to an even value in `[2, 254]`.
    pub fn set_tab_width(&mut self, width: u8) {
        let width = effective_tab_width(width);
        if self.tab_width == width {
            return;
        }
        self.tab_width = width;
        self.tab_metrics_generation = self.tab_metrics_generation.wrapping_add(1);
        self.refresh_tab_width_metrics();
        self.mark_all_views_dirty();
    }

    /// `checkTabWidth`: reject a width under which the text could exceed the u32 cell bound.
    pub fn check_tab_width(&self, width: u8) -> Result<(), TextError> {
        if self.byte_size() > (u32::MAX - 1) / effective_tab_width(width) as u32 {
            return Err(TextError::TextLimit);
        }
        Ok(())
    }

    /// Widths of chunks with tabs are derived state: update them in the shared nodes.
    fn refresh_tab_width_metrics(&mut self) {
        if self.rope.metrics_generation() == self.tab_metrics_generation {
            return;
        }
        if !self.tree().metrics().custom.has_tabs {
            self.rope.set_metrics_generation(self.tab_metrics_generation);
            return;
        }
        let registry = &self.mem_registry;
        let (tab_width, control_display, method) = (self.tab_width, self.control_display, self.width_method);
        self.rope.tree.walk(&mut |segment, _| {
            if let Segment::Text(chunk) = segment
                && chunk.has_tab()
            {
                let bytes = chunk.bytes(registry);
                chunk.set_width_cols(utf8::calculate_text_width(bytes, tab_width, control_display, chunk.is_ascii_only(), method));
            }
            true
        });
        self.rope.tree.remeasure();
        self.rope.set_metrics_generation(self.tab_metrics_generation);
    }

    /// `getTextRange`: the text between two offsets, snapped to grapheme boundaries.
    pub fn text_range(&self, start: u32, end: u32) -> Vec<u8> {
        let mut out = Vec::new();
        if start >= end {
            return out;
        }
        let total = self.tree().total_weight();
        if start >= total {
            return out;
        }
        iterators::extract_text_between_offsets(self.tree(), &self.mem_registry, &self.settings(), start, end.min(total), Some(&mut out));
        out
    }

    /// `copyTextRange`: as `text_range`.
    pub fn copy_text_range(&self, start: u32, end: u32) -> Vec<u8> {
        self.text_range(start, end)
    }

}

pub(crate) fn effective_tab_width(width: u8) -> u8 {
    let clamped = width.clamp(2, 254);
    clamped + clamped % 2
}

/// `internRunStyle`: equal values share one ref; a style that sets nothing is no style (0).
fn intern_run_style(run_styles: &mut IndexSet<StyleDefinition>, style: StyleDefinition) -> u32 {
    if style == StyleDefinition::default() {
        return 0;
    }
    let (index, _) = run_styles.insert_full(style);
    RUN_STYLE_FLAG | (index as u32 + 1)
}

/// `buildLineSpans`: the highest-priority style over each column range.
pub(crate) fn build_line_spans(highlights: &[Highlight]) -> Vec<StyleSpan> {
    let mut spans = Vec::new();
    #[derive(Clone, Copy)]
    struct Event {
        col: u32,
        is_start: bool,
        idx: usize,
    }
    let mut events: Vec<Event> = Vec::with_capacity(highlights.len() * 2);
    for (idx, h) in highlights.iter().enumerate() {
        events.push(Event { col: h.col_start, is_start: true, idx });
        events.push(Event { col: h.col_end, is_start: false, idx });
    }
    events.sort_by(|a, b| {
        a.col.cmp(&b.col).then_with(|| a.is_start.cmp(&b.is_start)).then_with(|| a.idx.cmp(&b.idx))
    });
    // The active highlights iterate in Zig's hash map order: among equal priorities the first one iterated wins.
    let mut active = ZigUsizeSet::new();
    let mut current_col = 0u32;
    for event in &events {
        let mut priority: i16 = -1;
        let mut style = 0u32;
        for idx in active.iter() {
            let h = &highlights[idx];
            if h.priority as i16 > priority {
                priority = h.priority as i16;
                style = h.style_id;
            }
        }
        if event.col > current_col {
            spans.push(StyleSpan { col: current_col, style_id: style, next_col: event.col });
            current_col = event.col;
        }
        if event.is_start {
            active.insert(event.idx);
        } else {
            active.remove(event.idx);
        }
    }
    if !events.is_empty() {
        spans.push(StyleSpan { col: current_col, style_id: 0, next_col: u32::MAX });
    }
    spans
}

/// Zig's `std.AutoHashMap(usize, void)`, as far as its iteration order goes: Wyhash of the key's 8 bytes, linear
/// probing in a power-of-two table (at least 8 slots, 80% load), tombstones recycled on insert, and growth that
/// re-inserts in slot order. `buildLineSpans` breaks priority ties by this order.
struct ZigUsizeSet {
    slots: Vec<Slot>,
    available: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Free,
    Tombstone,
    Used(usize),
}

const WYHASH_SECRET: [u64; 4] = [0xa0761d6478bd642f, 0xe7037ed1a0b428db, 0x8ebc6af09c88c6e3, 0x589965cc75374cc3];

fn wy_mum(a: u64, b: u64) -> (u64, u64) {
    let x = a as u128 * b as u128;
    (x as u64, (x >> 64) as u64)
}

fn wy_mix(a: u64, b: u64) -> u64 {
    let (a, b) = wy_mum(a, b);
    a ^ b
}

/// `Wyhash.hash(0, std.mem.asBytes(&key))` for a 64-bit key.
fn wyhash_u64(key: u64) -> u64 {
    let bytes = key.to_le_bytes();
    let read4 = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as u64;
    let state = wy_mix(WYHASH_SECRET[0], WYHASH_SECRET[1]);
    let a = (read4(0) << 32) | read4(4);
    let b = (read4(4) << 32) | read4(0);
    let (a, b) = wy_mum(a ^ WYHASH_SECRET[1], b ^ state);
    wy_mix(a ^ WYHASH_SECRET[0] ^ 8, b ^ WYHASH_SECRET[1])
}

impl ZigUsizeSet {
    fn new() -> Self {
        ZigUsizeSet { slots: Vec::new(), available: 0 }
    }

    fn max_load(capacity: usize) -> usize {
        capacity * 80 / 100
    }

    fn grow(&mut self, wanted: usize) {
        let capacity = (wanted * 100 / 80 + 1).next_power_of_two().max(8);
        let old = std::mem::replace(&mut self.slots, vec![Slot::Free; capacity]);
        self.available = Self::max_load(capacity);
        for slot in old {
            if let Slot::Used(key) = slot {
                let mask = capacity - 1;
                let mut idx = wyhash_u64(key as u64) as usize & mask;
                while self.slots[idx] != Slot::Free {
                    idx = (idx + 1) & mask;
                }
                self.slots[idx] = Slot::Used(key);
                self.available -= 1;
            }
        }
    }

    fn insert(&mut self, key: usize) {
        if self.available < 1 {
            let load = Self::max_load(self.slots.len()) - self.available;
            self.grow(load + 1);
        }
        let capacity = self.slots.len();
        let mask = capacity - 1;
        let mut idx = wyhash_u64(key as u64) as usize & mask;
        let mut limit = capacity;
        let mut tombstone = None;
        while self.slots[idx] != Slot::Free && limit != 0 {
            match self.slots[idx] {
                Slot::Used(k) if k == key => return,
                Slot::Tombstone if tombstone.is_none() => tombstone = Some(idx),
                _ => {}
            }
            limit -= 1;
            idx = (idx + 1) & mask;
        }
        if let Some(t) = tombstone {
            idx = t;
        }
        self.available -= 1;
        self.slots[idx] = Slot::Used(key);
    }

    fn remove(&mut self, key: usize) {
        if self.slots.is_empty() {
            return;
        }
        let mask = self.slots.len() - 1;
        let mut idx = wyhash_u64(key as u64) as usize & mask;
        let mut limit = self.slots.len();
        while self.slots[idx] != Slot::Free && limit != 0 {
            if self.slots[idx] == Slot::Used(key) {
                self.slots[idx] = Slot::Tombstone;
                self.available += 1;
                return;
            }
            limit -= 1;
            idx = (idx + 1) & mask;
        }
    }

    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots.iter().filter_map(|s| if let Slot::Used(k) = s { Some(*k) } else { None })
    }
}
