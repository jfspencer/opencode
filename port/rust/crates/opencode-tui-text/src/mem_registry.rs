//! Port of `mem-registry.zig`: the byte slots text chunks point into, by `u8` id.
//!
//! Divergence: every slot owns its bytes (the Zig registry can also borrow caller memory, which the Context ABI
//! never does: it copies all input first). Slot ids, reuse and the 255-slot limit are as in Zig, since chunk merges
//! depend on two chunks sharing a slot.

#[derive(Debug)]
pub(crate) struct MemBuffer {
    pub data: Vec<u8>,
    pub active: bool,
}

#[derive(Debug, Default)]
pub(crate) struct MemRegistry {
    pub buffers: Vec<MemBuffer>,
    free_slots: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RegistryFull;

impl MemRegistry {
    pub fn register(&mut self, data: Vec<u8>) -> Result<u8, RegistryFull> {
        if let Some(id) = self.free_slots.pop() {
            self.buffers[id as usize] = MemBuffer { data, active: true };
            return Ok(id);
        }
        if self.buffers.len() >= 255 {
            return Err(RegistryFull);
        }
        self.buffers.push(MemBuffer { data, active: true });
        Ok((self.buffers.len() - 1) as u8)
    }

    pub fn get(&self, id: u8) -> Option<&[u8]> {
        let buffer = self.buffers.get(id as usize)?;
        buffer.active.then_some(&buffer.data[..])
    }

    pub fn get_mut(&mut self, id: u8) -> Option<&mut Vec<u8>> {
        let buffer = self.buffers.get_mut(id as usize)?;
        if buffer.active { Some(&mut buffer.data) } else { None }
    }

    pub fn replace(&mut self, id: u8, data: Vec<u8>) -> bool {
        match self.buffers.get_mut(id as usize) {
            Some(buffer) if buffer.active => {
                buffer.data = data;
                true
            }
            _ => false,
        }
    }

    pub fn clear(&mut self) {
        self.buffers.clear();
        self.free_slots.clear();
    }

    pub fn free_slots(&self) -> usize {
        255 - self.buffers.len() + self.free_slots.len()
    }

    /// `reclaimAppendBuffers`: keep only slot 0.
    pub fn truncate_to_first(&mut self) {
        self.buffers.truncate(1);
        self.free_slots.clear();
    }

    pub fn len(&self) -> usize {
        self.buffers.len()
    }

    /// Release the latest registration (`cancelLastRegistration`).
    pub fn cancel_last(&mut self, id: u8, previous_count: usize) {
        if self.buffers.len() > previous_count {
            self.buffers.pop();
        } else {
            self.buffers[id as usize] = MemBuffer { data: Vec::new(), active: false };
            self.free_slots.push(id);
        }
    }

    pub fn retained_bytes(&self) -> usize {
        self.buffers.iter().map(|b| b.data.capacity()).sum()
    }
}
