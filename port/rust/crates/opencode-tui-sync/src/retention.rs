//! `createSessionRetention` (packages/client/src/solid/retention.ts): keeps the transcripts of the viewed session's
//! family, the kept ones and the most recently viewed `limit` others, and evicts the rest.
//!
//! TS creates the effect itself; here [`SessionRetention::run`] is the effect's body, which the app runs in an effect
//! of its reactive layer. It tracks what TS tracks (the viewed and kept sessions as the app's accessors read them,
//! the session order and parent links), and reads message counts and evicts untracked.

use std::cell::RefCell;
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};

use crate::data::Data;
use crate::json::JsonExt;
use crate::reactive::Key;

pub struct RetentionInput {
    pub data: Data,
    /// The viewed session.
    pub current: Rc<dyn Fn() -> Option<String>>,
    /// Sessions whose families stay loaded.
    pub keep: Rc<dyn Fn() -> Vec<String>>,
    pub limit: usize,
}

#[derive(Default)]
struct State {
    previous: Option<String>,
    recent: Vec<String>,
    evicted: IndexMap<String, String>,
}

pub struct SessionRetention {
    input: RetentionInput,
    state: RefCell<State>,
}

impl SessionRetention {
    pub fn new(input: RetentionInput) -> Self {
        Self { input, state: RefCell::new(State::default()) }
    }

    /// The effect's body.
    pub fn run(&self) {
        let data = &self.input.data;
        let session = &data.session;
        let viewed = (self.input.current)();
        let current = viewed.as_ref().map(|id| session.root(id));
        let keep: IndexSet<String> = (self.input.keep)().iter().map(|id| session.root(id)).collect();
        let mut state = self.state.borrow_mut();
        // Resolve again when metadata arrives, but only navigation advances recency.
        let mut recent: IndexSet<String> = IndexSet::new();
        if viewed != state.previous {
            if let Some(current) = &current {
                recent.insert(current.clone());
            }
        }
        for id in &state.recent {
            recent.insert(session.root(id));
        }
        state.previous = viewed;
        let mut retained: IndexSet<String> = keep.clone();
        retained.extend(recent.iter().filter(|id| !keep.contains(*id)).take(self.input.limit).cloned());
        if let Some(current) = &current {
            retained.insert(current.clone());
        }
        state.recent = recent.into_iter().filter(|id| retained.contains(id)).collect();

        let count = |id: &str| data.untrack(|| session.message.list(id).len());
        data.track(Key::SessionOrder);
        let sessions = data.untrack(|| session.list());
        let excluded: IndexMap<String, String> = sessions
            .iter()
            .filter_map(|info| {
                let id = info.str("id").unwrap_or("").to_owned();
                let root = session.root(&id);
                (!retained.contains(&root)).then_some((id, root))
            })
            .collect();
        // New descendants and non-retained sessions repopulated by background turns need eviction even when their
        // root was already evicted.
        let mut roots: IndexSet<String> = IndexSet::new();
        for (id, root) in &excluded {
            if state.evicted.get(id) != Some(&format!("{root}:{}", count(id))) {
                roots.insert(root.clone());
            }
        }
        drop(state);
        data.untrack(|| roots.iter().for_each(|root| session.evict(root)));
        let evicted: IndexMap<String, String> =
            excluded.iter().map(|(id, root)| (id.clone(), format!("{root}:{}", count(id)))).collect();
        self.state.borrow_mut().evicted = evicted;
    }
}
