//! The grapheme and link pools that cells refer to (`grapheme.zig`'s `GraphemePool`, `GraphemeTracker` and
//! cell char encoding; `link.zig`'s `LinkPool` and `LinkTracker`).
//!
//! A cell's `char` is a code point, or a cluster char: a grapheme start (`0x8000_0000 | (width - 1) << 26 | id`)
//! or a continuation (`0xC000_0000 | left << 26 | id`), where `id` names the cluster's bytes in the grapheme
//! pool. A cell's attributes carry a link id in bits 8-31, naming a URL in the link pool. Each buffer tracks
//! which ids its cells use (a tracker per pool) and holds one pool reference per id it uses, so an id lives
//! while a buffer shows it, as in Zig.
//!
//! The pools are per thread (the TUI runs on one thread), like the one pool per native context in Zig.
//!
//! Divergences: the grapheme pool is a plain interning table rather than Zig's size-class slabs, so grapheme
//! ids differ from Zig's; they never reach the terminal. The link pool is ported exactly, since its ids are
//! written in OSC 8 (`id=`). TODO(port-tui): opencode-tui-text ports grapheme.zig; its pool replaces this one so
//! that text buffers and frame buffers share it.
use std::cell::RefCell;
use std::collections::HashMap;

pub const CHAR_FLAG_IMAGE: u32 = 0x4000_0000;
pub const CHAR_FLAG_GRAPHEME: u32 = 0x8000_0000;
pub const CHAR_FLAG_CONTINUATION: u32 = 0xC000_0000;
pub const CHAR_TYPE_MASK: u32 = 0xC000_0000;
pub const CHAR_EXT_SHIFT: u32 = 26;
pub const CHAR_EXT_MASK: u32 = 0xF;
/// The most cells one glyph can span (`glyph_width_max`).
pub const GLYPH_WIDTH_MAX: u32 = CHAR_EXT_MASK + 1;
pub const GRAPHEME_ID_MASK: u32 = 0x03FF_FFFF;
/// The grapheme pool stores at most this many bytes per cluster (`grapheme_bytes_max`).
pub const GRAPHEME_BYTES_MAX: usize = 128;

pub const fn is_grapheme_char(c: u32) -> bool {
    c & CHAR_TYPE_MASK == CHAR_FLAG_GRAPHEME
}
pub const fn is_continuation_char(c: u32) -> bool {
    c & CHAR_TYPE_MASK == CHAR_FLAG_CONTINUATION
}
pub const fn is_cluster_char(c: u32) -> bool {
    c & 0x8000_0000 == 0x8000_0000
}
pub const fn is_image_char(c: u32) -> bool {
    c & CHAR_TYPE_MASK == CHAR_FLAG_IMAGE
}
pub const fn grapheme_id_from_char(c: u32) -> u32 {
    c & GRAPHEME_ID_MASK
}
/// A start's right extent (width - 1).
pub const fn char_right_extent(c: u32) -> u32 {
    (c >> CHAR_EXT_SHIFT) & CHAR_EXT_MASK
}
/// Cells from a cluster cell back to its start (0 for a start).
pub const fn char_left_extent(c: u32) -> u32 {
    if is_continuation_char(c) { (c >> CHAR_EXT_SHIFT) & CHAR_EXT_MASK } else { 0 }
}
pub const fn pack_grapheme_start(gid: u32, total_width: u32) -> u32 {
    CHAR_FLAG_GRAPHEME | ((total_width - 1) << CHAR_EXT_SHIFT) | gid
}
pub const fn pack_continuation(left: u32, gid: u32) -> u32 {
    CHAR_FLAG_CONTINUATION | (left << CHAR_EXT_SHIFT) | gid
}
/// Cells of the glyph that starts in this cell: zero for a continuation (`encodedCharWidth`).
pub const fn encoded_char_width(c: u32) -> u32 {
    if is_continuation_char(c) {
        0
    } else if is_grapheme_char(c) {
        char_right_extent(c) + 1
    } else {
        1
    }
}

/// Interned cluster bytes with reference counts.
#[derive(Default)]
pub struct GraphemePool {
    slots: Vec<Slot>,
    free: Vec<u32>,
    live: HashMap<Box<[u8]>, u32>,
}

#[derive(Default)]
struct Slot {
    bytes: Box<[u8]>,
    refcount: u32,
    generation: u32,
}

const SLOT_BITS: u32 = 16;
const SLOT_MASK: u32 = (1 << SLOT_BITS) - 1;
/// Grapheme ids: 19 bits of slot index and 7 of generation.
const GRAPHEME_INDEX_BITS: u32 = 19;
const GRAPHEME_INDEX_MASK: u32 = (1 << GRAPHEME_INDEX_BITS) - 1;
const GENERATION_MASK: u32 = (1 << 7) - 1;

impl GraphemePool {
    /// One owned reference to the bytes' id.
    pub fn acquire(&mut self, bytes: &[u8]) -> u32 {
        if let Some(&id) = self.live.get(bytes) {
            self.incref(id);
            return id;
        }
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                self.slots.push(Slot::default());
                (self.slots.len() - 1) as u32
            }
        };
        let slot = &mut self.slots[index as usize];
        slot.generation = (slot.generation + 1) & GENERATION_MASK;
        slot.bytes = bytes.into();
        slot.refcount = 1;
        let id = (slot.generation << GRAPHEME_INDEX_BITS) | (index & GRAPHEME_INDEX_MASK);
        self.live.insert(bytes.into(), id);
        id
    }

    fn slot_index(id: u32) -> usize {
        (id & GRAPHEME_INDEX_MASK) as usize
    }

    fn slot(&self, id: u32) -> Option<&Slot> {
        let slot = self.slots.get(Self::slot_index(id))?;
        (slot.refcount > 0 && slot.generation == (id >> GRAPHEME_INDEX_BITS) & GENERATION_MASK).then_some(slot)
    }

    pub fn incref(&mut self, id: u32) {
        let index = Self::slot_index(id);
        if self.slot(id).is_some() {
            self.slots[index].refcount += 1;
        }
    }

    pub fn decref(&mut self, id: u32) {
        let index = Self::slot_index(id);
        if self.slot(id).is_none() {
            return;
        }
        let slot = &mut self.slots[index];
        slot.refcount -= 1;
        if slot.refcount == 0 {
            self.live.remove(&slot.bytes);
            slot.bytes = Box::default();
            self.free.push(index as u32);
        }
    }

    /// The cluster's bytes, or `None` for a dead id.
    pub fn get(&self, id: u32) -> Option<&[u8]> {
        self.slot(id).map(|slot| &*slot.bytes)
    }

    pub fn refcount(&self, id: u32) -> u32 {
        self.slot(id).map_or(0, |slot| slot.refcount)
    }

    /// Live ids (for tests and memory accounting).
    pub fn live_count(&self) -> usize {
        self.live.len()
    }
}

/// The URL pool (`LinkPool`), with Zig's slot and generation numbering: ids reach the terminal in OSC 8.
#[derive(Default)]
pub struct LinkPool {
    slots: Vec<LinkSlot>,
    free: Vec<u32>,
    retired: u32,
    live: HashMap<Box<str>, u32>,
}

#[derive(Default)]
struct LinkSlot {
    url: Box<str>,
    refcount: u32,
    generation: u32,
}

const LINK_GEN_BITS: u32 = 8;
const LINK_GEN_MASK: u32 = (1 << LINK_GEN_BITS) - 1;
const LINK_SLOTS_PER_PAGE: u32 = 64;
const LINK_RETIRED_GENERATION: u32 = LINK_GEN_MASK + 1;
/// `MAX_URL_LENGTH`.
pub const MAX_URL_LENGTH: usize = 512;

impl LinkPool {
    fn grow(&mut self) -> bool {
        let count = self.slots.len() as u32;
        if count > (SLOT_MASK + 1) - LINK_SLOTS_PER_PAGE {
            return false;
        }
        for i in 0..LINK_SLOTS_PER_PAGE {
            self.slots.push(LinkSlot::default());
            self.free.push(count + i);
        }
        true
    }

    fn unpack(id: u32) -> (usize, u32) {
        ((id & SLOT_MASK) as usize, (id >> SLOT_BITS) & LINK_GEN_MASK)
    }

    /// One owned reference to the URL's id; `None` for a URL over 512 bytes or a full pool.
    pub fn acquire(&mut self, url: &str) -> Option<u32> {
        if url.len() > MAX_URL_LENGTH {
            return None;
        }
        if let Some(&id) = self.live.get(url) {
            self.incref(id);
            return Some(id);
        }
        if self.free.is_empty() && !self.grow() {
            return None;
        }
        let index = self.free.pop()?;
        let slot = &mut self.slots[index as usize];
        slot.generation += 1;
        slot.url = url.into();
        slot.refcount = 1;
        let id = ((slot.generation & LINK_GEN_MASK) << SLOT_BITS) | (index & SLOT_MASK);
        self.live.insert(url.into(), id);
        Some(id)
    }

    fn live_slot(&self, id: u32) -> Option<usize> {
        let (index, generation) = Self::unpack(id);
        let slot = self.slots.get(index)?;
        (slot.generation == generation && slot.refcount > 0).then_some(index)
    }

    pub fn incref(&mut self, id: u32) -> bool {
        match self.live_slot(id) {
            Some(index) => {
                self.slots[index].refcount += 1;
                true
            }
            None => false,
        }
    }

    pub fn decref(&mut self, id: u32) {
        let Some(index) = self.live_slot(id) else { return };
        let slot = &mut self.slots[index];
        if slot.refcount == 1 {
            self.live.remove(&slot.url);
        }
        slot.refcount -= 1;
        if slot.refcount == 0 {
            if slot.generation == LINK_GEN_MASK {
                slot.generation = LINK_RETIRED_GENERATION;
                self.retired += 1;
            } else {
                self.free.push(index as u32);
            }
        }
    }

    /// The URL of a live id.
    pub fn get(&self, id: u32) -> Option<&str> {
        let (index, generation) = Self::unpack(id);
        let slot = self.slots.get(index)?;
        // Zig's `get` checks only the generation.
        (slot.generation == generation).then_some(&*slot.url)
    }

    /// `lookupLive`.
    pub fn lookup_live(&self, url: &str) -> Option<u32> {
        self.live.get(url).copied()
    }

    pub fn refcount(&self, id: u32) -> u32 {
        self.live_slot(id).map_or(0, |index| self.slots[index].refcount)
    }
}

thread_local! {
    static GRAPHEMES: RefCell<GraphemePool> = RefCell::new(GraphemePool::default());
    static LINKS: RefCell<LinkPool> = RefCell::new(LinkPool::default());
}

/// Runs `f` with this thread's grapheme pool.
pub fn with_graphemes<R>(f: impl FnOnce(&mut GraphemePool) -> R) -> R {
    GRAPHEMES.with(|pool| f(&mut pool.borrow_mut()))
}

/// Runs `f` with this thread's link pool.
pub fn with_links<R>(f: impl FnOnce(&mut LinkPool) -> R) -> R {
    LINKS.with(|pool| f(&mut pool.borrow_mut()))
}

/// The bytes of a grapheme id, copied.
pub fn grapheme_bytes(id: u32) -> Option<Vec<u8>> {
    with_graphemes(|pool| pool.get(id).map(<[u8]>::to_vec))
}

/// Which pool a tracker refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Grapheme,
    Link,
}

/// Per-buffer usage of pool ids: id -> number of cells (`GraphemeTracker`, `LinkTracker`). The buffer holds one
/// pool reference per tracked id.
pub struct Tracker {
    kind: Kind,
    used: HashMap<u32, u32>,
}

impl Tracker {
    pub fn graphemes() -> Self {
        Tracker { kind: Kind::Grapheme, used: HashMap::new() }
    }

    pub fn links() -> Self {
        Tracker { kind: Kind::Link, used: HashMap::new() }
    }

    fn pool_incref(&self, id: u32) -> bool {
        match self.kind {
            Kind::Grapheme => {
                with_graphemes(|pool| pool.incref(id));
                true
            }
            Kind::Link => with_links(|pool| pool.incref(id)),
        }
    }

    fn pool_decref(&self, id: u32) {
        // A buffer dropped while the thread exits may outlive the pools.
        let _ = match self.kind {
            Kind::Grapheme => GRAPHEMES.try_with(|pool| pool.borrow_mut().decref(id)),
            Kind::Link => LINKS.try_with(|pool| pool.borrow_mut().decref(id)),
        };
    }

    /// `add` / `addCellRef`.
    pub fn add(&mut self, id: u32) {
        if let Some(count) = self.used.get_mut(&id) {
            *count += 1;
            return;
        }
        if self.pool_incref(id) {
            self.used.insert(id, 1);
        }
    }

    /// `remove` / `removeCellRef`.
    pub fn remove(&mut self, id: u32) {
        let Some(count) = self.used.get_mut(&id) else { return };
        if *count > 1 {
            *count -= 1;
            return;
        }
        self.used.remove(&id);
        self.pool_decref(id);
    }

    /// `replace`.
    pub fn replace(&mut self, old: Option<u32>, new: Option<u32>) {
        if old.is_some() && old == new {
            return;
        }
        if let Some(id) = new {
            self.add(id);
        }
        if let Some(id) = old {
            self.remove(id);
        }
    }

    /// `trackUrl`: document membership once per URL (link trackers only).
    pub fn track_url(&mut self, url: &str) -> Option<u32> {
        if let Some(id) = with_links(|pool| pool.lookup_live(url)) {
            if self.used.contains_key(&id) {
                return Some(id);
            }
        }
        let id = with_links(|pool| pool.acquire(url))?;
        self.used.insert(id, 1);
        Some(id)
    }

    pub fn has_any(&self) -> bool {
        !self.used.is_empty()
    }

    pub fn count(&self) -> usize {
        self.used.len()
    }

    pub fn contains(&self, id: u32) -> bool {
        self.used.contains_key(&id)
    }

    /// Releases every id (`clear`, keeping capacity).
    pub fn clear(&mut self) {
        for id in std::mem::take(&mut self.used).into_keys() {
            self.pool_decref(id);
        }
    }

    /// The tracked ids and their cell counts.
    pub fn entries(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.used.iter().map(|(id, count)| (*id, *count))
    }

    /// Adopts `source`'s membership (`copyFrom`): every id it holds, with its count.
    pub fn copy_from(&mut self, source: &Tracker) {
        self.clear();
        for (&id, &count) in &source.used {
            if self.pool_incref(id) {
                self.used.insert(id, count);
            }
        }
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        self.clear();
    }
}
