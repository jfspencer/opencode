//! Port of `edit-buffer.zig`: a text buffer edited through a cursor, with inserted bytes in an append-only add
//! buffer, undo/redo through the rope's history, and grapheme-aware cursor motion and deletion.
//!
//! Divergences: the add buffer's slots grow as bytes arrive instead of being allocated at their full capacity
//! (the capacity still decides when a new slot is registered, which decides chunk merges). Cursor listeners are
//! passed to each call ([`EditListener`]) instead of being registered on the buffer.

use crate::iterators::{self, Coords};
use crate::mem_registry::MemRegistry;
use crate::rope::{Bias, JoinSpan, LeafJoin, LeafOps, RopeError, Tree};
use crate::segment::{find_junction, Segment, TextChunk, ASCII_ONLY, HAS_TAB};
use crate::text_buffer::TextBufferCore;
use crate::utf8::{self, ControlDisplay, CollectBreaks, LayoutWrapBreak, WidthMethod, WordClassEdges};
use crate::link::LinkPool;
use crate::TextError;
use std::cell::RefCell;
use std::rc::Rc;

/// A cursor (`Cursor`): row and column in cells, the column vertical moves aim for, and the offset.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Cursor {
    pub row: u32,
    pub col: u32,
    pub desired_col: u32,
    pub offset: u32,
}

/// The edit buffer's notifications for the Context (`NativeEvent`), in TS order of delivery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditEvent {
    /// `"cursor-changed"`
    CursorChanged,
    /// `"content-changed"`
    ContentChanged,
    /// `"cursorChanged"` (after undo/redo)
    HistoryCursorChanged,
}

impl EditEvent {
    /// The event name TS emits.
    pub fn name(self) -> &'static str {
        match self {
            EditEvent::CursorChanged => "cursor-changed",
            EditEvent::ContentChanged => "content-changed",
            EditEvent::HistoryCursorChanged => "cursorChanged",
        }
    }
}

/// The synchronous cursor listeners of an edit buffer (its editor views).
pub(crate) trait EditListener {
    fn cursor_changed(&mut self, eb: &EditBufferCore);
    /// Between the two steps of `replace_range`.
    fn after_delete(&mut self, _eb: &EditBufferCore) {}
}

#[derive(Clone, Copy, Debug)]
struct CursorMeta {
    row: u32,
    col: u32,
    desired_col: u32,
    tab_width: Option<u8>,
}

impl CursorMeta {
    fn encode(&self) -> String {
        match self.tab_width {
            Some(t) => format!("cursor:{}:{}:{}:{}", self.row, self.col, self.desired_col, t),
            None => format!("cursor:{}:{}:{}", self.row, self.col, self.desired_col),
        }
    }

    fn decode(bytes: &str) -> Option<CursorMeta> {
        let rest = bytes.strip_prefix("cursor:")?;
        let mut parts = rest.split(':');
        let row = parts.next()?.parse().ok()?;
        let col = parts.next()?.parse().ok()?;
        let desired_col = parts.next()?.parse().ok()?;
        let tab_width = match parts.next() {
            Some(p) => Some(p.parse::<u8>().ok()?),
            None => None,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(CursorMeta { row, col, desired_col, tab_width })
    }

    /// The metadata TS sees: without the tab width.
    fn public_bytes(bytes: &str) -> String {
        match CursorMeta::decode(bytes) {
            Some(m) if m.tab_width.is_some() => bytes[..bytes.rfind(':').unwrap()].to_owned(),
            _ => bytes.to_owned(),
        }
    }
}

/// `parseInt(u32)` accepts what Zig's `std.fmt.parseInt` does for the digits the encoder writes; the encoder
/// writes only decimal digits, so Rust's parse agrees.
#[derive(Clone, Copy, Debug)]
struct AddBuffer {
    mem_id: u8,
    len: usize,
    cap: usize,
    /// Slots registered by growth, for a failed edit to release (`grown`).
    grown: [(u8, usize); 32],
    grown_len: usize,
}

#[derive(Clone, Copy)]
struct Checkpoint {
    add: AddBuffer,
}

const ADD_BUFFER_CAPACITY: usize = 65536;

pub(crate) struct EditBufferCore {
    pub(crate) tb: TextBufferCore,
    add: AddBuffer,
    pub(crate) cursor: Cursor,
    pub(crate) events: Vec<EditEvent>,
}

/// The leaf callbacks of weight edits: split at a column, join across a grapheme.
struct SegmentOps<'a> {
    registry: &'a mut MemRegistry,
    add: &'a mut AddBuffer,
    tab_width: u8,
    control_display: ControlDisplay,
    method: WidthMethod,
}

fn make_chunk(registry: &MemRegistry, tab_width: u8, control: ControlDisplay, method: WidthMethod, mem_id: u8, start: u32, end: u32) -> TextChunk {
    let bytes = &registry.get(mem_id).expect("live slot")[start as usize..end as usize];
    let ascii = utf8::is_ascii_only(bytes);
    let mut flags = 0;
    if !bytes.is_empty() && ascii {
        flags |= ASCII_ONLY;
    }
    if !ascii && bytes.contains(&b'\t') {
        flags |= HAS_TAB;
    }
    TextChunk::new(mem_id, flags, start, end, utf8::calculate_text_width(bytes, tab_width, control, ascii, method))
}

impl AddBuffer {
    fn ensure_capacity(&mut self, registry: &mut MemRegistry, need: usize) -> Result<(), RopeError> {
        if self.len + need <= self.cap {
            return Ok(());
        }
        let new_cap = (self.cap * 2).max(need);
        let count = registry.len();
        let id = registry.register(Vec::new()).map_err(|_| RopeError::OutOfMemory)?;
        self.grown[self.grown_len] = (id, count);
        self.grown_len += 1;
        self.mem_id = id;
        self.len = 0;
        self.cap = new_cap;
        Ok(())
    }

    /// Appends `bytes`, which must fit; returns the slot and range.
    fn append(&mut self, registry: &mut MemRegistry, bytes: &[u8]) -> (u8, u32, u32) {
        debug_assert!(self.len + bytes.len() <= self.cap);
        let data = registry.get_mut(self.mem_id).expect("live add buffer");
        data.truncate(self.len);
        data.extend_from_slice(bytes);
        let start = self.len as u32;
        self.len += bytes.len();
        (self.mem_id, start, self.len as u32)
    }
}

impl SegmentOps<'_> {
    fn chunk(&self, mem_id: u8, start: u32, end: u32) -> TextChunk {
        make_chunk(self.registry, self.tab_width, self.control_display, self.method, mem_id, start, end)
    }

    fn measure(&self, bytes: &[u8]) -> u32 {
        utf8::calculate_text_width(bytes, self.tab_width, self.control_display, utf8::is_ascii_only(bytes), self.method)
    }
}

impl LeafOps for SegmentOps<'_> {
    /// `splitChunkAtWeight`.
    fn split(&mut self, leaf: &Segment, weight: u32) -> Result<(Segment, Segment), RopeError> {
        let Segment::Text(chunk) = leaf else { return Ok((Segment::Brk, Segment::Brk)) };
        if weight == 0 {
            return Ok((Segment::empty(), leaf.clone()));
        }
        if weight >= chunk.width_cols() {
            return Ok((leaf.clone(), Segment::empty()));
        }
        let bytes = chunk.bytes(self.registry);
        let split = utf8::find_pos_by_width(bytes, weight, self.tab_width, self.control_display, chunk.is_ascii_only(), false, self.method)
            .byte_offset;
        let left = self.chunk(chunk.mem_id, chunk.byte_start, chunk.byte_start + split);
        let right = self.chunk(chunk.mem_id, chunk.byte_start + split, chunk.byte_end);
        Ok((Segment::Text(left), Segment::Text(right)))
    }

    /// `joinSegmentCallback`: keeps chunk boundaries on grapheme boundaries.
    fn join(&mut self, left: &Segment, right: &Segment) -> Result<Option<LeafJoin>, RopeError> {
        let (Some(l), Some(r)) = (left.as_text(), right.as_text()) else { return Ok(None) };
        let l_bytes = l.bytes(self.registry).to_vec();
        let r_bytes = r.bytes(self.registry).to_vec();
        if Segment::can_merge(left, right) {
            let span = find_junction(&l_bytes, &r_bytes, self.method).map(|(tail, head)| {
                let start = l.width_cols() - self.measure(&l_bytes[tail as usize..]);
                let slot = self.registry.get(l.mem_id).expect("live slot");
                let across = &slot[(l.byte_start + tail) as usize..(l.byte_end + head) as usize];
                [start, start + self.measure(across)]
            });
            let joined = self.chunk(l.mem_id, l.byte_start, r.byte_end);
            return Ok(Some(LeafJoin { left: None, joined: Segment::Text(joined), right: None, span, more: true }));
        }
        let Some((tail, head)) = find_junction(&l_bytes, &r_bytes, self.method) else {
            let cp = utf8::decode(&r_bytes, 0).0;
            if (0x1F1E6..=0x1F1FF).contains(&cp) {
                return Ok(Some(LeafJoin { left: Some(left.clone()), joined: right.clone(), right: None, span: None, more: true }));
            }
            return Ok(None);
        };
        let joined = if l.mem_id == r.mem_id && l.byte_end == r.byte_start {
            self.chunk(l.mem_id, l.byte_start + tail, r.byte_start + head)
        } else {
            let len = l_bytes.len() - tail as usize + head as usize;
            self.add.ensure_capacity(self.registry, len)?;
            let (mem_id, start, _) = self.add.append(self.registry, &l_bytes[tail as usize..]);
            self.add.append(self.registry, &r_bytes[..head as usize]);
            self.chunk(mem_id, start, start + len as u32)
        };
        let width = joined.width_cols();
        Ok(Some(LeafJoin {
            left: (tail > 0).then(|| Segment::Text(self.chunk(l.mem_id, l.byte_start, l.byte_start + tail))),
            joined: Segment::Text(joined),
            right: ((head as usize) < r_bytes.len()).then(|| Segment::Text(self.chunk(r.mem_id, r.byte_start + head, r.byte_end))),
            span: Some([0, width]),
            more: true,
        }))
    }
}

fn rope_error(_: RopeError) -> TextError {
    TextError::OutOfMemory
}

/// `endCursor`: the cursor at `offset` in `after`, an edit at `start` that added `added_lines` lines.
fn end_cursor(start: Cursor, added_lines: u32, after: &Tree, offset: u32) -> Cursor {
    let row = start.row + added_lines;
    let col = if added_lines == 0 {
        offset - (start.offset - start.col)
    } else {
        offset - iterators::line_start(after, row).expect("a line").weight
    };
    Cursor { row, col, desired_col: col, offset }
}

/// `insertBias`: before zero-width text at a line start, after it elsewhere.
fn insert_bias(cursor: Cursor) -> Bias {
    if cursor.col == 0 { Bias::Right } else { Bias::Left }
}

impl EditBufferCore {
    pub fn new(width_method: WidthMethod, link_pool: Rc<RefCell<LinkPool>>) -> Self {
        let mut tb = TextBufferCore::new(width_method, ControlDisplay::Notation, link_pool);
        let mem_id = tb.mem_registry.register(Vec::new()).expect("an empty registry");
        EditBufferCore {
            tb,
            add: AddBuffer { mem_id, len: 0, cap: ADD_BUFFER_CAPACITY, grown: [(0, 0); 32], grown_len: 0 },
            cursor: Cursor::default(),
            events: Vec::new(),
        }
    }

    fn ops(&mut self) -> SegmentOps<'_> {
        SegmentOps {
            registry: &mut self.tb.mem_registry,
            add: &mut self.add,
            tab_width: self.tb.tab_width,
            control_display: self.tb.control_display,
            method: self.tb.width_method,
        }
    }

    fn checkpoint(&self) -> Checkpoint {
        Checkpoint { add: self.add }
    }

    fn restore(&mut self, cp: Checkpoint) {
        while self.add.grown_len > cp.add.grown_len {
            self.add.grown_len -= 1;
            let (id, count) = self.add.grown[self.add.grown_len];
            self.tb.mem_registry.cancel_last(id, count);
        }
        self.add = cp.add;
        if let Some(data) = self.tb.mem_registry.get_mut(self.add.mem_id) {
            data.truncate(self.add.len);
        }
    }

    /// `publishCursor`: the synchronous listeners, then the Context's notifications.
    fn publish_cursor(&mut self, content_changed: bool, listener: &mut dyn EditListener) {
        listener.cursor_changed(self);
        self.events.push(EditEvent::CursorChanged);
        if content_changed {
            self.events.push(EditEvent::ContentChanged);
        }
    }

    pub fn tree(&self) -> &Tree {
        self.tb.tree()
    }

    pub fn set_cursor(&mut self, row: u32, col: u32, listener: &mut dyn EditListener) {
        let line_count = self.tb.line_count();
        let row = row.min(line_count.saturating_sub(1));
        let col = col.min(iterators::line_width_at(self.tree(), row));
        let offset = iterators::coords_to_offset(self.tree(), row, col).unwrap_or(0);
        self.cursor = Cursor { row, col, desired_col: col, offset };
        self.publish_cursor(false, listener);
    }

    pub fn set_cursor_by_offset(&mut self, offset: u32, listener: &mut dyn EditListener) {
        let coords = iterators::offset_to_coords(self.tree(), offset).unwrap_or(Coords { row: 0, col: 0 });
        self.set_cursor(coords.row, coords.col, listener);
    }

    /// `remapCol`: a column measured under `old_tab_width`, measured again under the current one.
    fn remap_col(&self, row: u32, target: u32, old_tab_width: u8) -> Option<u32> {
        let tree = self.tree();
        let mut seg_idx = iterators::line_start(tree, row)?.leaf_index + 1;
        let mut old_col = 0u32;
        let mut new_col = 0u32;
        let (control, method) = (self.tb.control_display, self.tb.width_method);
        while seg_idx < tree.count() {
            let Some(segment) = tree.get(seg_idx) else { break };
            if segment.is_break() || segment.is_line_start() {
                break;
            }
            seg_idx += 1;
            let Some(chunk) = segment.as_text() else { continue };
            let bytes = chunk.bytes(&self.tb.mem_registry);
            let next_col = old_col.saturating_add(utf8::calculate_text_width(bytes, old_tab_width, control, chunk.is_ascii_only(), method));
            if target <= next_col {
                let pos = utf8::find_pos_by_width(bytes, target.saturating_sub(old_col), old_tab_width, control, chunk.is_ascii_only(), false, method);
                return Some(new_col.saturating_add(utf8::calculate_text_width(
                    &bytes[..pos.byte_offset as usize],
                    self.tb.tab_width,
                    control,
                    chunk.is_ascii_only(),
                    method,
                )));
            }
            old_col = next_col;
            new_col = new_col.saturating_add(chunk.width_cols());
        }
        if target == old_col { Some(new_col) } else { None }
    }

    pub fn set_tab_width(&mut self, width: u8, listener: &mut dyn EditListener) {
        let old = self.tb.tab_width;
        self.tb.set_tab_width(width);
        if old == self.tb.tab_width {
            return;
        }
        let cursor = self.cursor;
        let new_col = self
            .remap_col(cursor.row, cursor.col, old)
            .unwrap_or_else(|| cursor.col.min(iterators::line_width_at(self.tree(), cursor.row)));
        if self.cursor.desired_col == self.cursor.col {
            self.cursor.desired_col = new_col;
        }
        self.cursor.col = new_col;
        self.cursor.offset = iterators::coords_to_offset(self.tree(), cursor.row, new_col).unwrap_or(0);
        self.publish_cursor(false, listener);
    }

    fn encode_cursor_meta(&self) -> String {
        CursorMeta { row: self.cursor.row, col: self.cursor.col, desired_col: self.cursor.desired_col, tab_width: Some(self.tb.tab_width) }.encode()
    }

    fn auto_store_undo(&mut self) {
        let meta = self.encode_cursor_meta();
        self.tb.rope.store_undo(&meta);
    }

    pub fn insert_text(&mut self, bytes: &[u8], listener: &mut dyn EditListener) -> Result<(), TextError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let cursor = self.cursor;
        let insert_offset = iterators::coords_to_offset(self.tree(), cursor.row, cursor.col).ok_or(TextError::InvalidCursor)?;
        let cp = self.checkpoint();
        let result = (|| {
            self.add.ensure_capacity(&mut self.tb.mem_registry, bytes.len())?;
            let (mem_id, start, _) = self.add.append(&mut self.tb.mem_registry, bytes);
            let segments = self.tb.text_to_segments(bytes, mem_id, start, false);
            let mut candidate = self.tb.rope.tree.clone();
            let span = candidate.insert_slice_by_weight(insert_offset, insert_bias(cursor), &segments, &mut self.ops())?;
            Ok::<_, RopeError>((candidate, span))
        })();
        let (candidate, span) = match result {
            Ok(v) => v,
            Err(e) => {
                self.restore(cp);
                return Err(rope_error(e));
            }
        };
        let added_lines = iterators::line_count(&candidate) - self.tb.line_count();
        let end = end_cursor(Cursor { row: cursor.row, col: cursor.col, desired_col: 0, offset: insert_offset }, added_lines, &candidate, span.end);
        self.auto_store_undo();
        self.tb.rope.tree.root = candidate.root;
        self.cursor = end;
        self.tb.mark_all_views_dirty();
        self.publish_cursor(true, listener);
        Ok(())
    }

    /// `replaceByWeight`.
    fn replace_by_weight(&mut self, candidate: &mut Tree, start_offset: u32, end_offset: u32, segments: &[Segment]) -> Result<JoinSpan, TextError> {
        let tree = self.tree();
        let start = iterators::offset_to_coords(tree, start_offset).ok_or(TextError::InvalidCursor)?;
        let end = iterators::offset_to_coords(tree, end_offset).ok_or(TextError::InvalidCursor)?;
        let end_bias = if end.col == 0 && end_offset < tree.total_weight() { Bias::Right } else { Bias::Left };
        let start_bias = if start.col == 0 { Bias::Right } else { Bias::Left };
        candidate.replace_range_by_weight(start_offset, end_offset, start_bias, end_bias, segments, &mut self.ops()).map_err(rope_error)
    }

    pub fn delete_range(&mut self, start: Cursor, end: Cursor, listener: &mut dyn EditListener) -> Result<(), TextError> {
        let (mut start, mut end) = (start, end);
        if start.row > end.row || (start.row == end.row && start.col > end.col) {
            std::mem::swap(&mut start, &mut end);
        }
        let empty = start.row == end.row && start.col == end.col;
        let tree = self.tree();
        let Some(start_offset) = iterators::coords_to_offset(tree, start.row, start.col) else {
            return if empty { Ok(()) } else { Err(TextError::InvalidCursor) };
        };
        let end_offset = iterators::coords_to_offset(tree, end.row, end.col).ok_or(TextError::InvalidCursor)?;
        if empty {
            let Some(line) = iterators::line_start(tree, start.row) else { return Ok(()) };
            if start.col > 0 || end_offset < tree.total_weight() || tree.count() == line.leaf_index + 1 {
                return Ok(());
            }
        }
        let cp = self.checkpoint();
        let mut candidate = self.tb.rope.tree.clone();
        let span = match self.replace_by_weight(&mut candidate, start_offset, end_offset, &[]) {
            Ok(span) => span,
            Err(e) => {
                self.restore(cp);
                return Err(e);
            }
        };
        self.auto_store_undo();
        self.tb.rope.tree.root = candidate.root.clone();
        self.tb.mark_all_views_dirty();
        self.cursor = end_cursor(Cursor { row: start.row, col: start.col, desired_col: 0, offset: start_offset }, 0, &candidate, span.end);
        self.publish_cursor(true, listener);
        Ok(())
    }

    /// `replaceRange`: an exclusive cell range replaced as a deletion step and an insertion step; the listener's
    /// `after_delete` runs between them.
    pub fn replace_range(&mut self, start_offset: u32, end_offset: u32, bytes: &[u8], listener: &mut dyn EditListener) -> Result<(), TextError> {
        if start_offset > end_offset || end_offset > self.tree().total_weight() {
            return Err(TextError::InvalidCursor);
        }
        let has_deletion = start_offset != end_offset;
        let cp = self.checkpoint();
        let prepared = (|| -> Result<_, TextError> {
            self.add.ensure_capacity(&mut self.tb.mem_registry, bytes.len()).map_err(rope_error)?;
            let (mem_id, start_ref, _) = self.add.append(&mut self.tb.mem_registry, bytes);
            let segments = self.tb.text_to_segments(bytes, mem_id, start_ref, false);
            let mut deleted = self.tb.rope.tree.clone();
            let mut inserted = deleted.clone();
            let mut start = self.cursor;
            let mut deleted_span = JoinSpan::default();
            let mut inserted_span = JoinSpan::default();
            if has_deletion {
                let coords = iterators::offset_to_coords(self.tree(), start_offset).unwrap();
                start = Cursor { row: coords.row, col: coords.col, desired_col: 0, offset: start_offset };
                deleted_span = self.replace_by_weight(&mut deleted, start_offset, end_offset, &[])?;
                if !bytes.is_empty() {
                    inserted_span = self.replace_by_weight(&mut inserted, start_offset, end_offset, &segments)?;
                }
            } else if !bytes.is_empty() {
                start.offset = iterators::coords_to_offset(self.tree(), start.row, start.col).ok_or(TextError::InvalidCursor)?;
                inserted_span = inserted
                    .insert_slice_by_weight(start.offset, insert_bias(start), &segments, &mut self.ops())
                    .map_err(rope_error)?;
            }
            Ok((deleted, inserted, start, deleted_span, inserted_span))
        })();
        let (deleted, inserted, start, deleted_span, inserted_span) = match prepared {
            Ok(v) => v,
            Err(e) => {
                self.restore(cp);
                return Err(e);
            }
        };
        let deleted_cursor = if has_deletion { end_cursor(start, 0, &deleted, deleted_span.end) } else { start };
        let inserted_cursor = if !bytes.is_empty() {
            end_cursor(start, iterators::line_count(&inserted) - iterators::line_count(&deleted), &inserted, inserted_span.end)
        } else {
            start
        };
        if has_deletion {
            self.publish_replacement_step(&deleted, deleted_cursor, listener);
        }
        listener.after_delete(self);
        if !bytes.is_empty() {
            self.publish_replacement_step(&inserted, inserted_cursor, listener);
        }
        Ok(())
    }

    fn publish_replacement_step(&mut self, candidate: &Tree, cursor: Cursor, listener: &mut dyn EditListener) {
        self.auto_store_undo();
        self.tb.rope.tree.root = candidate.root.clone();
        self.cursor = cursor;
        self.tb.mark_all_views_dirty();
        self.publish_cursor(true, listener);
    }

    pub fn backspace(&mut self, listener: &mut dyn EditListener) -> Result<(), TextError> {
        let cursor = self.cursor;
        if cursor.col == 0 {
            let mut start = cursor;
            if cursor.row > 0 {
                start = Cursor { row: cursor.row - 1, col: iterators::line_width_at(self.tree(), cursor.row - 1), ..Cursor::default() };
            }
            return self.delete_range(start, cursor, listener);
        }
        let width = self.tb.prev_grapheme_width(cursor.row, cursor.col);
        if width == 0 {
            return Ok(());
        }
        let target = cursor.col - width;
        self.delete_range(
            Cursor { row: cursor.row, col: target, ..Cursor::default() },
            Cursor { row: cursor.row, col: cursor.col, ..Cursor::default() },
            listener,
        )
    }

    pub fn delete_forward(&mut self, listener: &mut dyn EditListener) -> Result<(), TextError> {
        let cursor = self.cursor;
        let line_width = iterators::line_width_at(self.tree(), cursor.row);
        let line_count = self.tb.line_count();
        if cursor.col >= line_width {
            let mut end = Cursor { row: cursor.row, col: line_width, ..Cursor::default() };
            if cursor.row + 1 < line_count {
                end = Cursor { row: cursor.row + 1, col: 0, ..Cursor::default() };
            }
            return self.delete_range(Cursor { row: cursor.row, col: line_width, ..Cursor::default() }, end, listener);
        }
        let width = self.tb.grapheme_width_at(cursor.row, cursor.col);
        if width > 0 {
            return self.delete_range(
                Cursor { row: cursor.row, col: cursor.col, ..Cursor::default() },
                Cursor { row: cursor.row, col: cursor.col + width, ..Cursor::default() },
                listener,
            );
        }
        Ok(())
    }

    pub fn move_left(&mut self, listener: &mut dyn EditListener) {
        self.cursor = self.prev_char_boundary();
        self.publish_cursor(false, listener);
    }

    /// `getPrevCharBoundary`: where `move_left` puts the cursor.
    pub fn prev_char_boundary(&self) -> Cursor {
        let mut cursor = self.cursor;
        if cursor.col > 0 {
            cursor.col -= self.tb.prev_grapheme_width(cursor.row, cursor.col);
        } else if cursor.row > 0 {
            cursor.row -= 1;
            cursor.col = iterators::line_width_at(self.tree(), cursor.row);
        }
        cursor.desired_col = cursor.col;
        cursor.offset = iterators::coords_to_offset(self.tree(), cursor.row, cursor.col).unwrap_or(0);
        cursor
    }

    pub fn move_right(&mut self, listener: &mut dyn EditListener) {
        let mut cursor = self.cursor;
        let line_width = iterators::line_width_at(self.tree(), cursor.row);
        let line_count = self.tb.line_count();
        if cursor.col < line_width {
            cursor.col += self.tb.grapheme_width_at(cursor.row, cursor.col);
        } else if cursor.row + 1 < line_count {
            cursor.row += 1;
            cursor.col = 0;
        }
        cursor.desired_col = cursor.col;
        cursor.offset = iterators::coords_to_offset(self.tree(), cursor.row, cursor.col).unwrap_or(0);
        self.cursor = cursor;
        self.publish_cursor(false, listener);
    }

    pub fn move_up(&mut self, listener: &mut dyn EditListener) {
        if self.cursor.row > 0 {
            self.move_to_row(self.cursor.row - 1);
        }
        self.publish_cursor(false, listener);
    }

    pub fn move_down(&mut self, listener: &mut dyn EditListener) {
        if self.cursor.row + 1 < self.tb.line_count() {
            self.move_to_row(self.cursor.row + 1);
        }
        self.publish_cursor(false, listener);
    }

    fn move_to_row(&mut self, row: u32) {
        if self.cursor.desired_col == 0 {
            self.cursor.desired_col = self.cursor.col;
        }
        self.cursor.row = row;
        let col = self.snap_column(row, self.cursor.desired_col.min(iterators::line_width_at(self.tree(), row)));
        self.cursor.col = col;
        self.cursor.offset = iterators::coords_to_offset(self.tree(), row, col).unwrap_or(0);
    }

    /// `snapColumn`: the start of the cursor unit that covers `col`.
    pub fn snap_column(&self, row: u32, col: u32) -> u32 {
        match iterators::cursor_unit_bounds_at(self.tree(), &self.tb.mem_registry, row, col, &self.tb.settings()) {
            Some(b) => b.start,
            None => col,
        }
    }

    /// `unsnappedColumn`.
    pub fn unsnapped_column(&self) -> u32 {
        let c = self.cursor;
        if self.snap_column(c.row, c.desired_col) == c.col { c.desired_col } else { c.col }
    }

    /// `setTextOwned`: replaces the text, clears history; reuses `preferred_id`'s slot when live.
    pub fn set_text_owned(&mut self, text: &[u8], preferred_id: Option<u8>, listener: &mut dyn EditListener) -> Result<u8, TextError> {
        if let Some(id) = preferred_id
            && (id == self.add.mem_id || id == 255)
        {
            return Err(TextError::InvalidOptions);
        }
        let mem_id = self.tb.replace_text(text.to_vec(), preferred_id)?;
        self.add.len = 0;
        if let Some(data) = self.tb.mem_registry.get_mut(self.add.mem_id) {
            data.clear();
        }
        self.finish_text_replacement(listener);
        Ok(mem_id)
    }

    /// `replaceText`: replaces the text as an undo step; the bytes go to the add buffer.
    pub fn replace_text(&mut self, text: &[u8], listener: &mut dyn EditListener) -> Result<(), TextError> {
        let meta = self.encode_cursor_meta();
        let cp = self.checkpoint();
        if let Err(e) = self.add.ensure_capacity(&mut self.tb.mem_registry, text.len()) {
            self.restore(cp);
            return Err(rope_error(e));
        }
        let (mem_id, start, end) = self.add.append(&mut self.tb.mem_registry, text);
        self.tb.set_text_from_mem_range_with_undo(mem_id, start, end, &meta);
        self.finish_text_replacement(listener);
        Ok(())
    }

    fn finish_text_replacement(&mut self, listener: &mut dyn EditListener) {
        self.cursor = Cursor::default();
        self.publish_cursor(true, listener);
    }

    pub fn delete_line(&mut self, listener: &mut dyn EditListener) -> Result<(), TextError> {
        let cursor = self.cursor;
        let line_count = self.tb.line_count();
        if cursor.row >= line_count {
            return Ok(());
        }
        let at = |row, col| Cursor { row, col, ..Cursor::default() };
        if cursor.row + 1 < line_count {
            self.delete_range(at(cursor.row, 0), at(cursor.row + 1, 0), listener)
        } else if cursor.row > 0 {
            let prev = iterators::line_width_at(self.tree(), cursor.row - 1);
            let curr = iterators::line_width_at(self.tree(), cursor.row);
            self.delete_range(at(cursor.row - 1, prev), at(cursor.row, curr), listener)
        } else {
            let width = iterators::line_width_at(self.tree(), cursor.row);
            self.delete_range(at(cursor.row, 0), at(cursor.row, width), listener)
        }
    }

    pub fn goto_line(&mut self, line: u32, listener: &mut dyn EditListener) {
        let line_count = self.tb.line_count();
        let target = line.min(line_count.saturating_sub(1));
        if line >= line_count {
            let width = iterators::line_width_at(self.tree(), target);
            self.set_cursor(target, width, listener);
        } else {
            self.set_cursor(target, 0, listener);
        }
    }

    pub fn undo(&mut self, listener: &mut dyn EditListener) -> Option<String> {
        let meta = self.encode_cursor_meta();
        let previous = self.tb.undo(&meta)?;
        Some(self.restore_history_step(&previous, listener))
    }

    pub fn redo(&mut self, listener: &mut dyn EditListener) -> Option<String> {
        let next = self.tb.redo()?;
        Some(self.restore_history_step(&next, listener))
    }

    fn restore_history_step(&mut self, meta: &str, listener: &mut dyn EditListener) -> String {
        let current = self.cursor;
        let decoded = CursorMeta::decode(meta).unwrap_or(CursorMeta { row: current.row, col: current.col, desired_col: current.col, tab_width: None });
        let width_changed = decoded.tab_width.is_some_and(|w| w != self.tb.tab_width);
        let col = if width_changed {
            self.remap_col(decoded.row, decoded.col, decoded.tab_width.unwrap()).unwrap_or(decoded.col)
        } else {
            decoded.col
        };
        self.set_cursor(decoded.row, col, listener);
        self.cursor.desired_col = if width_changed && decoded.desired_col == decoded.col { col } else { decoded.desired_col };
        self.tb.mark_all_views_dirty();
        self.events.push(EditEvent::ContentChanged);
        self.events.push(EditEvent::HistoryCursorChanged);
        CursorMeta::public_bytes(meta)
    }

    pub fn can_undo(&self) -> bool {
        self.tb.rope.can_undo()
    }

    /// `canRedo`: `clear` replaces the root without an undo point, which leaves the redo branch unusable.
    pub fn can_redo(&self) -> bool {
        self.tb.rope.can_redo() && self.tb.rope.curr_is_root()
    }

    pub fn clear_history(&mut self) {
        self.tb.rope.clear_history();
    }

    pub fn clear(&mut self, listener: &mut dyn EditListener) {
        self.tb.clear();
        self.set_cursor(0, 0, listener);
        self.events.push(EditEvent::ContentChanged);
    }

    /// `getWordLayoutInfoFor`: a chunk's word boundaries and edge classes.
    fn word_layout(&self, chunk: &TextChunk) -> (Vec<LayoutWrapBreak>, WordClassEdges) {
        let mut breaks = Vec::new();
        let s = self.tb.settings();
        let edges = utf8::walk_chunk_layout_info(
            chunk.bytes(&self.tb.mem_registry),
            s.tab_width,
            s.control_display,
            chunk.is_ascii_only(),
            s.method,
            &mut CollectBreaks::<true>(&mut breaks),
        );
        (breaks, edges)
    }

    pub fn next_word_boundary(&self) -> Cursor {
        let cursor = self.cursor;
        let tree = self.tree();
        let line_count = self.tb.line_count();
        if cursor.row >= line_count {
            return cursor;
        }
        let line_width = iterators::line_width_at(tree, cursor.row);
        let Some(start) = iterators::line_start(tree, cursor.row) else { return cursor };
        let mut seg_idx = start.leaf_index + 1;
        let mut cols_before = 0u32;
        let mut passed = false;
        let mut previous_class = utf8::WordClass::Other;
        let at = |col: u32| {
            let offset = iterators::coords_to_offset(tree, cursor.row, col).unwrap_or(cursor.offset);
            Cursor { row: cursor.row, col, desired_col: col, offset }
        };
        while seg_idx < tree.count() {
            let Some(segment) = tree.get(seg_idx) else { break };
            if segment.is_break() || segment.is_line_start() {
                break;
            }
            seg_idx += 1;
            let Some(chunk) = segment.as_text() else { continue };
            let next_cols = cols_before + chunk.width_cols();
            let (breaks, edges) = self.word_layout(chunk);
            if utf8::is_cjk_ascii_transition(previous_class, edges.first) && cols_before > cursor.col && cols_before <= line_width {
                return at(cols_before);
            }
            if cursor.col < next_cols || passed {
                let local = cursor.col.saturating_sub(cols_before);
                for b in &breaks {
                    let break_col = b.col_start;
                    let target = cols_before + b.col_end();
                    if (passed || break_col > local) && target > cursor.col && target <= line_width {
                        return at(target);
                    }
                    if !passed && break_col == local {
                        let bytes = chunk.bytes(&self.tb.mem_registry);
                        if (b.byte_start as usize) < bytes.len()
                            && utf8::is_word_codepoint(utf8::decode(bytes, b.byte_start as usize).0)
                            && target > cursor.col
                            && target <= line_width
                        {
                            return at(target);
                        }
                    }
                }
                passed = true;
            }
            previous_class = edges.last;
            cols_before = next_cols;
        }
        if cursor.row + 1 < line_count {
            let offset = iterators::coords_to_offset(tree, cursor.row + 1, 0).unwrap_or(cursor.offset);
            return Cursor { row: cursor.row + 1, col: 0, desired_col: 0, offset };
        }
        at(line_width)
    }

    pub fn prev_word_boundary(&self) -> Cursor {
        let cursor = self.cursor;
        if cursor.row == 0 && cursor.col == 0 {
            return cursor;
        }
        let tree = self.tree();
        let Some(start) = iterators::line_start(tree, cursor.row) else { return cursor };
        let mut seg_idx = start.leaf_index + 1;
        let mut cols_before = 0u32;
        let mut last_boundary: Option<u32> = None;
        let mut previous_class = utf8::WordClass::Other;
        while seg_idx < tree.count() {
            let Some(segment) = tree.get(seg_idx) else { break };
            if segment.is_break() || segment.is_line_start() {
                break;
            }
            seg_idx += 1;
            let Some(chunk) = segment.as_text() else { continue };
            let next_cols = cols_before + chunk.width_cols();
            let (breaks, edges) = self.word_layout(chunk);
            if utf8::is_cjk_ascii_transition(previous_class, edges.first) && cols_before < cursor.col {
                last_boundary = Some(cols_before);
            }
            for b in &breaks {
                let col = cols_before + b.col_end();
                if col < cursor.col {
                    last_boundary = Some(col);
                }
            }
            previous_class = edges.last;
            cols_before = next_cols;
            if cursor.col <= cols_before {
                break;
            }
        }
        if let Some(col) = last_boundary {
            let offset = iterators::coords_to_offset(tree, cursor.row, col).unwrap_or(cursor.offset);
            return Cursor { row: cursor.row, col, desired_col: col, offset };
        }
        if cursor.row > 0 {
            let width = iterators::line_width_at(tree, cursor.row - 1);
            let offset = iterators::coords_to_offset(tree, cursor.row - 1, width).unwrap_or(cursor.offset);
            return Cursor { row: cursor.row - 1, col: width, desired_col: width, offset };
        }
        Cursor::default()
    }

    pub fn eol(&self) -> Cursor {
        let cursor = self.cursor;
        if cursor.row >= self.tb.line_count() {
            return cursor;
        }
        let width = iterators::line_width_at(self.tree(), cursor.row);
        let offset = iterators::coords_to_offset(self.tree(), cursor.row, width).unwrap_or(cursor.offset);
        Cursor { row: cursor.row, col: width, desired_col: width, offset }
    }

    pub fn take_events(&mut self) -> Vec<EditEvent> {
        std::mem::take(&mut self.events)
    }
}

/// `measureEditText`: the offset at the end of `bytes` laid out as an edit buffer's text.
pub(crate) fn measure_edit_text(bytes: &[u8], tab_width: u8, control: ControlDisplay, method: WidthMethod) -> u32 {
    let mut offset = 0u32;
    let mut rest = bytes;
    loop {
        let end = rest.iter().position(|&b| b == b'\r' || b == b'\n').unwrap_or(rest.len());
        let line = &rest[..end];
        offset += utf8::calculate_text_width(line, tab_width, control, utf8::is_ascii_only(line), method);
        if end == rest.len() {
            return offset;
        }
        let crlf = rest[end..].starts_with(b"\r\n");
        offset += 1;
        rest = &rest[end + 1 + crlf as usize..];
    }
}
