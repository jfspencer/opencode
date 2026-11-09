//! The grapheme and link pools that cells refer to: the renderer's one native context (`Context.graphemes`,
//! `Context.links`), shared by text buffers and frame buffers as in Zig. The pools, the cell char encoding and
//! the trackers' rules are `opencode-tui-text`'s ports of `grapheme.zig` and `link.zig`; this module holds the
//! thread's context and a per-buffer [`Tracker`] over it.
//!
//! A cell's `char` is a code point, or a cluster char: a grapheme start (`0x8000_0000 | (width - 1) << 26 | id`)
//! or a continuation (`0xC000_0000 | left << 26 | id`), where `id` names the cluster's bytes in the grapheme
//! pool. A cell's attributes carry a link id in bits 8-31, naming a URL in the link pool. Each buffer tracks
//! which ids its cells use and holds one pool reference per id it uses, so an id lives while a buffer shows it.
//! Text buffers acquire link ids in the same pool, so the ids their draw sources carry are the ids output
//! writes in OSC 8.
//!
//! The context is per thread (the TUI runs on one thread).
use std::collections::HashMap;

pub use opencode_tui_text::grapheme::{
    CHAR_EXT_MASK, CHAR_EXT_SHIFT, CHAR_FLAG_CONTINUATION, CHAR_FLAG_GRAPHEME, CHAR_FLAG_IMAGE, CHAR_TYPE_MASK,
    GLYPH_WIDTH_MAX, GRAPHEME_BYTES_MAX, GRAPHEME_ID_MASK, GraphemePool, char_left_extent, char_right_extent,
    encoded_char_width, grapheme_id_from_char, is_cluster_char, is_continuation_char, is_grapheme_char,
    is_image_char, pack_continuation, pack_grapheme_start,
};
pub use opencode_tui_text::link::{LinkPool, MAX_URL_LENGTH};
use opencode_tui_text::TextContext;

thread_local! {
    static CONTEXT: TextContext = TextContext::new();
}

/// The renderer's context: the pools every text buffer and frame buffer of this thread shares.
pub fn context() -> TextContext {
    CONTEXT.with(|c| c.clone())
}

/// Runs `f` with this thread's grapheme pool.
pub fn with_graphemes<R>(f: impl FnOnce(&mut GraphemePool) -> R) -> R {
    CONTEXT.with(|c| f(&mut c.graphemes.borrow_mut()))
}

/// Runs `f` with this thread's link pool.
pub fn with_links<R>(f: impl FnOnce(&mut LinkPool) -> R) -> R {
    CONTEXT.with(|c| f(&mut c.links.borrow_mut()))
}

/// One owned reference to the glyph's id (`pool.alloc`), or `None` for a glyph over 128 bytes or a full pool.
pub fn acquire_grapheme(bytes: &[u8]) -> Option<u32> {
    with_graphemes(|pool| pool.acquire(bytes).ok())
}

/// Drops one owned reference to a grapheme id.
pub fn release_grapheme(id: u32) {
    let _ = with_graphemes(|pool| pool.decref(id));
}

/// The bytes of a grapheme id, copied.
pub fn grapheme_bytes(id: u32) -> Option<Vec<u8>> {
    with_graphemes(|pool| pool.get(id).ok().map(<[u8]>::to_vec))
}

/// The URL of a link id (`getLinkUrl`).
pub fn link_url(id: u32) -> Option<String> {
    with_links(|pool| pool.get(id).ok().map(str::to_owned))
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

    // GraphemeTracker.add keeps an id whose incref failed; LinkTracker.addCellRef drops it.
    fn pool_incref(&self, id: u32) -> bool {
        match self.kind {
            Kind::Grapheme => {
                let _ = with_graphemes(|pool| pool.incref(id));
                true
            }
            Kind::Link => with_links(|pool| pool.incref(id).is_ok()),
        }
    }

    fn pool_decref(&self, id: u32) {
        // A buffer dropped while the thread exits may outlive the context.
        let _ = CONTEXT.try_with(|c| match self.kind {
            Kind::Grapheme => c.graphemes.try_borrow_mut().map(|mut pool| pool.decref(id).is_ok()).ok(),
            Kind::Link => c.links.try_borrow_mut().map(|mut pool| pool.decref(id).is_ok()).ok(),
        });
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
        if let Some(id) = with_links(|pool| pool.lookup_live(url))
            && self.used.contains_key(&id)
        {
            return Some(id);
        }
        let id = with_links(|pool| pool.acquire(url)).ok()?;
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
