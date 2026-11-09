//! Port of `link.zig`: the context-wide pool of hyperlink URLs (OSC 8) and a per-owner tracker of the links it
//! holds. Link ids reach the terminal (`ESC ] 8 ; id=<n> ; <url>`), so slot order, growth and generations are as in
//! Zig: pages of 64 slots whose free list pops the highest index first, 8-bit generations, a slot retired at the
//! last generation.

use std::collections::HashMap;

pub const MAX_URL_LENGTH: usize = 512;
const GEN_BITS: u32 = 8;
const SLOT_BITS: u32 = 16;
const GEN_MASK: u32 = (1 << GEN_BITS) - 1;
const SLOT_MASK: u32 = (1 << SLOT_BITS) - 1;
const RETIRED_GENERATION: u32 = GEN_MASK + 1;
const SLOTS_PER_PAGE: u32 = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkError {
    OutOfMemory,
    InvalidId,
    WrongGeneration,
    UrlTooLong,
    RefcountOverflow,
}

#[derive(Default, Debug)]
struct Slot {
    url: Box<str>,
    refcount: u32,
    generation: u32,
}

#[derive(Default, Debug)]
pub struct LinkPool {
    slots: Vec<Slot>,
    free_list: Vec<u32>,
    retired: u32,
    live: HashMap<Box<str>, u32>,
}

fn unpack(id: u32) -> (u32, u32) {
    (id & SLOT_MASK, (id >> SLOT_BITS) & GEN_MASK)
}

impl LinkPool {
    pub fn new() -> Self {
        Self::default()
    }

    fn grow(&mut self) -> Result<(), LinkError> {
        let count = self.slots.len() as u32;
        if count > SLOT_MASK + 1 - SLOTS_PER_PAGE {
            return Err(LinkError::OutOfMemory);
        }
        for i in 0..SLOTS_PER_PAGE {
            self.slots.push(Slot::default());
            self.free_list.push(count + i);
        }
        Ok(())
    }

    pub fn lookup_live(&self, url: &str) -> Option<u32> {
        self.live.get(url).copied()
    }

    /// One owned live reference to `url`.
    pub fn acquire(&mut self, url: &str) -> Result<u32, LinkError> {
        if url.len() > MAX_URL_LENGTH {
            return Err(LinkError::UrlTooLong);
        }
        if let Some(id) = self.lookup_live(url) {
            self.incref(id)?;
            return Ok(id);
        }
        if self.free_list.is_empty() {
            self.grow()?;
        }
        let index = self.free_list.pop().unwrap();
        let slot = &mut self.slots[index as usize];
        slot.generation += 1;
        slot.url = url.into();
        slot.refcount = 1;
        let id = ((slot.generation & GEN_MASK) << SLOT_BITS) | index;
        self.live.insert(url.into(), id);
        Ok(id)
    }

    pub fn incref(&mut self, id: u32) -> Result<(), LinkError> {
        let (index, generation) = unpack(id);
        let slot = self.slots.get_mut(index as usize).ok_or(LinkError::InvalidId)?;
        if slot.generation != generation {
            return Err(LinkError::WrongGeneration);
        }
        if slot.refcount == 0 {
            return Err(LinkError::InvalidId);
        }
        if slot.refcount == u32::MAX {
            return Err(LinkError::RefcountOverflow);
        }
        slot.refcount += 1;
        Ok(())
    }

    pub fn decref(&mut self, id: u32) -> Result<(), LinkError> {
        let (index, generation) = unpack(id);
        let slot = self.slots.get_mut(index as usize).ok_or(LinkError::InvalidId)?;
        if slot.refcount == 0 {
            return Err(LinkError::InvalidId);
        }
        if slot.generation != generation {
            return Err(LinkError::WrongGeneration);
        }
        slot.refcount -= 1;
        if slot.refcount == 0 {
            let url = std::mem::take(&mut slot.url);
            self.live.remove(&url);
            if slot.generation == GEN_MASK {
                slot.generation = RETIRED_GENERATION;
                self.retired += 1;
            } else {
                self.free_list.push(index);
            }
        }
        Ok(())
    }

    pub fn get(&self, id: u32) -> Result<&str, LinkError> {
        let (index, generation) = unpack(id);
        let slot = self.slots.get(index as usize).ok_or(LinkError::InvalidId)?;
        if slot.generation != generation {
            return Err(LinkError::WrongGeneration);
        }
        Ok(&slot.url)
    }

    pub fn refcount(&self, id: u32) -> Result<u32, LinkError> {
        let (index, generation) = unpack(id);
        let slot = self.slots.get(index as usize).ok_or(LinkError::InvalidId)?;
        if slot.generation != generation {
            return Err(LinkError::WrongGeneration);
        }
        Ok(slot.refcount)
    }

    pub fn live_slot_count(&self) -> usize {
        self.slots.len() - self.free_list.len() - self.retired as usize
    }
}

/// The links one owner (a text buffer, a cell buffer) holds, with a use count each (`LinkTracker`).
#[derive(Default, Debug)]
pub struct LinkTracker {
    used: HashMap<u32, u32>,
}

impl LinkTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self, pool: &mut LinkPool) {
        for (&id, _) in self.used.iter() {
            let _ = pool.decref(id);
        }
        self.used.clear();
    }

    /// Track document membership once per URL (`trackUrl`).
    pub fn track_url(&mut self, pool: &mut LinkPool, url: &str) -> Result<u32, LinkError> {
        if let Some(id) = pool.lookup_live(url)
            && self.used.contains_key(&id)
        {
            return Ok(id);
        }
        let id = pool.acquire(url)?;
        self.used.insert(id, 1);
        Ok(id)
    }

    pub fn add_cell_ref(&mut self, pool: &mut LinkPool, id: u32) {
        match self.used.get_mut(&id) {
            Some(count) => *count += 1,
            None => {
                if pool.incref(id).is_ok() {
                    self.used.insert(id, 1);
                }
            }
        }
    }

    pub fn remove_cell_ref(&mut self, pool: &mut LinkPool, id: u32) {
        if let Some(count) = self.used.get_mut(&id)
            && *count > 0
        {
            *count -= 1;
            if *count == 0 {
                self.used.remove(&id);
                let _ = pool.decref(id);
            }
        }
    }

    pub fn contains(&self, id: u32) -> bool {
        self.used.contains_key(&id)
    }

    pub fn link_count(&self) -> u32 {
        self.used.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.used.is_empty()
    }
}

/// Text attribute bits (`ansi.TextAttributes`): the low byte holds the base attributes, the next 24 bits a link id.
pub mod attributes {
    pub const BASE_MASK: u32 = 0xFF;
    pub const LINK_ID_SHIFT: u32 = 8;
    pub const LINK_ID_PAYLOAD_MASK: u32 = (1 << 24) - 1;
    pub const LINK_ID_MASK: u32 = LINK_ID_PAYLOAD_MASK << LINK_ID_SHIFT;

    pub fn link_id(attributes: u32) -> u32 {
        (attributes & LINK_ID_MASK) >> LINK_ID_SHIFT
    }

    pub fn set_link_id(attributes: u32, id: u32) -> u32 {
        (attributes & BASE_MASK) | ((id & LINK_ID_PAYLOAD_MASK) << LINK_ID_SHIFT)
    }
}
