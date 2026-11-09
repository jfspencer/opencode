//! Port of `grapheme.zig`: the char encoding of cell buffers and the context-wide pool of multi-byte glyphs they
//! reference.
//!
//! A cell's `u32` char is a Unicode scalar (bits 31-30 `00`), an image cell (`01`), the start cell of a pooled
//! glyph (`10`: its right extent in bits 29-26 and pool id in bits 25-0) or a continuation cell (`11`: its left
//! extent and the pool id). Pool ids are `[class 3 | generation 7 | slot 16]`, as in Zig.
//!
//! Divergence: the pool keeps each glyph in its own boxed slice instead of size-class pages; ids, generations,
//! interning and reference counts are as in Zig.

use std::collections::HashMap;

pub const CHAR_FLAG_IMAGE: u32 = 0x4000_0000;
pub const CHAR_FLAG_GRAPHEME: u32 = 0x8000_0000;
pub const CHAR_FLAG_CONTINUATION: u32 = 0xC000_0000;
pub const CHAR_TYPE_MASK: u32 = 0xC000_0000;
pub const IMAGE_ID_MASK: u32 = 0x03FF_FFFF;
pub const CHAR_EXT_SHIFT: u32 = 26;
pub const CHAR_EXT_MASK: u32 = 0xF;
/// The most cells one glyph can span.
pub const GLYPH_WIDTH_MAX: u32 = CHAR_EXT_MASK + 1;
pub const GRAPHEME_ID_MASK: u32 = 0x03FF_FFFF;
/// The most bytes one pooled glyph holds.
pub const GRAPHEME_BYTES_MAX: usize = 128;

const CLASS_SIZES: [usize; 5] = [8, 16, 32, 64, 128];
const SLOTS_PER_PAGE: [u32; 5] = [256, 128, 64, 16, 8];
const GENERATION_BITS: u32 = 7;
const SLOT_BITS: u32 = 16;
const GENERATION_MASK: u32 = (1 << GENERATION_BITS) - 1;
const SLOT_MASK: u32 = (1 << SLOT_BITS) - 1;

pub fn pack_image_cell(id: u32, fallback: u8) -> u32 {
    CHAR_FLAG_IMAGE | (id << 4) | (fallback as u32 & 0xF)
}

pub fn is_image_char(c: u32) -> bool {
    c & CHAR_TYPE_MASK == CHAR_FLAG_IMAGE
}

pub fn image_id_from_char(c: u32) -> u32 {
    (c >> 4) & IMAGE_ID_MASK
}

pub fn image_fallback_from_char(c: u32) -> u8 {
    (c & 0xF) as u8
}

pub fn is_grapheme_char(c: u32) -> bool {
    c & CHAR_TYPE_MASK == CHAR_FLAG_GRAPHEME
}

pub fn is_continuation_char(c: u32) -> bool {
    c & CHAR_TYPE_MASK == CHAR_FLAG_CONTINUATION
}

pub fn is_cluster_char(c: u32) -> bool {
    c & 0x8000_0000 != 0
}

pub fn grapheme_id_from_char(c: u32) -> u32 {
    c & GRAPHEME_ID_MASK
}

pub fn char_right_extent(c: u32) -> u32 {
    (c >> CHAR_EXT_SHIFT) & CHAR_EXT_MASK
}

/// Cells from a cluster cell back to its start cell.
pub fn char_left_extent(c: u32) -> u32 {
    if is_continuation_char(c) { (c >> CHAR_EXT_SHIFT) & CHAR_EXT_MASK } else { 0 }
}

pub fn pack_grapheme_start(gid: u32, total_width: u32) -> u32 {
    debug_assert!(total_width > 0 && total_width <= GLYPH_WIDTH_MAX);
    CHAR_FLAG_GRAPHEME | ((total_width - 1) << CHAR_EXT_SHIFT) | (gid & GRAPHEME_ID_MASK)
}

pub fn pack_continuation(left: u32, gid: u32) -> u32 {
    debug_assert!(left > 0 && left <= CHAR_EXT_MASK);
    CHAR_FLAG_CONTINUATION | (left << CHAR_EXT_SHIFT) | (gid & GRAPHEME_ID_MASK)
}

/// Cells of the glyph that starts in this cell: zero for a continuation.
pub fn encoded_char_width(c: u32) -> u32 {
    if is_continuation_char(c) {
        return 0;
    }
    if is_grapheme_char(c) { char_right_extent(c) + 1 } else { 1 }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GraphemePoolError {
    OutOfMemory,
    GraphemeTooLong,
    InvalidId,
    WrongGeneration,
    RefcountOverflow,
}

#[derive(Default, Debug)]
struct Slot {
    bytes: Box<[u8]>,
    refcount: u32,
    generation: u32,
    allocated: bool,
}

#[derive(Default, Debug)]
struct ClassPool {
    slots: Vec<Slot>,
    free_list: Vec<u32>,
}

/// The pool of multi-byte glyphs (`GraphemePool`).
#[derive(Default, Debug)]
pub struct GraphemePool {
    classes: [ClassPool; 5],
    live: HashMap<Box<[u8]>, u32>,
}

fn class_for_size(size: usize) -> usize {
    CLASS_SIZES.iter().position(|&s| size <= s).unwrap_or(4)
}

fn unpack(id: u32) -> (usize, u32, u32) {
    (((id >> (GENERATION_BITS + SLOT_BITS)) & 7) as usize, id & SLOT_MASK, (id >> SLOT_BITS) & GENERATION_MASK)
}

impl GraphemePool {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&self, id: u32) -> Result<&Slot, GraphemePoolError> {
        let (class, index, generation) = unpack(id);
        let pool = self.classes.get(class).ok_or(GraphemePoolError::InvalidId)?;
        let slot = pool.slots.get(index as usize).ok_or(GraphemePoolError::InvalidId)?;
        if slot.generation != generation {
            return Err(GraphemePoolError::WrongGeneration);
        }
        if !slot.allocated {
            return Err(GraphemePoolError::InvalidId);
        }
        Ok(slot)
    }

    /// One owned live reference to `bytes`.
    pub fn acquire(&mut self, bytes: &[u8]) -> Result<u32, GraphemePoolError> {
        if bytes.len() > GRAPHEME_BYTES_MAX {
            return Err(GraphemePoolError::GraphemeTooLong);
        }
        if let Some(&id) = self.live.get(bytes) {
            self.incref(id)?;
            return Ok(id);
        }
        let class = class_for_size(bytes.len());
        let pool = &mut self.classes[class];
        if pool.free_list.is_empty() {
            let count = pool.slots.len() as u32;
            let per_page = SLOTS_PER_PAGE[class];
            if count > SLOT_MASK + 1 - per_page {
                return Err(GraphemePoolError::OutOfMemory);
            }
            for i in 0..per_page {
                pool.slots.push(Slot::default());
                pool.free_list.push(count + i);
            }
        }
        let index = pool.free_list.pop().unwrap();
        let slot = &mut pool.slots[index as usize];
        slot.generation = (slot.generation + 1) & GENERATION_MASK;
        slot.bytes = bytes.into();
        slot.refcount = 1;
        slot.allocated = true;
        let id = ((class as u32) << (GENERATION_BITS + SLOT_BITS)) | (slot.generation << SLOT_BITS) | index;
        self.live.insert(bytes.into(), id);
        Ok(id)
    }

    pub fn incref(&mut self, id: u32) -> Result<(), GraphemePoolError> {
        self.slot(id)?;
        let (class, index, _) = unpack(id);
        let slot = &mut self.classes[class].slots[index as usize];
        if slot.refcount == u32::MAX {
            return Err(GraphemePoolError::RefcountOverflow);
        }
        slot.refcount += 1;
        Ok(())
    }

    pub fn decref(&mut self, id: u32) -> Result<(), GraphemePoolError> {
        self.slot(id)?;
        let (class, index, _) = unpack(id);
        let pool = &mut self.classes[class];
        let slot = &mut pool.slots[index as usize];
        if slot.refcount == 0 {
            return Err(GraphemePoolError::InvalidId);
        }
        slot.refcount -= 1;
        if slot.refcount == 0 {
            slot.allocated = false;
            let bytes = std::mem::take(&mut slot.bytes);
            self.live.remove(&bytes);
            pool.free_list.push(index);
        }
        Ok(())
    }

    pub fn get(&self, id: u32) -> Result<&[u8], GraphemePoolError> {
        Ok(&self.slot(id)?.bytes)
    }

    pub fn refcount(&self, id: u32) -> Result<u32, GraphemePoolError> {
        Ok(self.slot(id)?.refcount)
    }
}

/// The glyphs one cell buffer references, with a cell count each (`GraphemeTracker`).
#[derive(Default, Debug)]
pub struct GraphemeTracker {
    used: HashMap<u32, u32>,
}

impl GraphemeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self, pool: &mut GraphemePool) {
        for &id in self.used.keys() {
            let _ = pool.decref(id);
        }
        self.used.clear();
    }

    pub fn add(&mut self, pool: &mut GraphemePool, id: u32) {
        match self.used.get_mut(&id) {
            Some(count) => *count += 1,
            None => {
                self.used.insert(id, 1);
                let _ = pool.incref(id);
            }
        }
    }

    pub fn remove(&mut self, pool: &mut GraphemePool, id: u32) {
        let Some(count) = self.used.get_mut(&id) else { return };
        if *count > 1 {
            *count -= 1;
            return;
        }
        self.used.remove(&id);
        let _ = pool.decref(id);
    }

    pub fn replace(&mut self, pool: &mut GraphemePool, old: Option<u32>, new: Option<u32>) {
        if old.is_some() && old == new {
            return;
        }
        if let Some(id) = new {
            self.add(pool, id);
        }
        if let Some(id) = old {
            self.remove(pool, id);
        }
    }

    pub fn contains(&self, id: u32) -> bool {
        self.used.contains_key(&id)
    }

    pub fn has_any(&self) -> bool {
        !self.used.is_empty()
    }

    pub fn grapheme_count(&self) -> u32 {
        self.used.len() as u32
    }

    pub fn grapheme_cell_count(&self) -> u32 {
        self.used.values().sum()
    }
}
