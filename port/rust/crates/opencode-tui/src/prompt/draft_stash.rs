//! `component/prompt/draft-stash.ts`: one in-progress draft per tab across composer remounts (taken when restored,
//! so a stale copy never shadows newer input), and the mounted composer of each tab.
use std::cell::RefCell;
use std::collections::HashMap;

use opencode_tui_text::WidthMethod;

use crate::prompt::PromptRef;
use crate::prompt::history::{Mode, PromptInfo, append_prompt};

/// `DraftEntry`.
#[derive(Clone, Debug, PartialEq)]
pub struct DraftEntry {
    pub prompt: PromptInfo,
    /// The cursor offset (`u32::MAX`: the end, TS's `Infinity`).
    pub cursor: u32,
}

thread_local! {
    static BY_TAB: RefCell<HashMap<Option<String>, DraftEntry>> = RefCell::new(HashMap::new());
    static COMPOSERS: RefCell<Vec<(Option<String>, PromptRef)>> = const { RefCell::new(Vec::new()) };
}

/// `takeDraft(sessionID)`.
pub fn take_draft(session_id: Option<&str>) -> Option<DraftEntry> {
    BY_TAB.with(|t| t.borrow_mut().remove(&session_id.map(str::to_string)))
}

/// `saveDraft(sessionID, entry)`.
pub fn save_draft(session_id: Option<&str>, entry: DraftEntry) {
    BY_TAB.with(|t| t.borrow_mut().insert(session_id.map(str::to_string), entry));
}

/// `registerComposer(sessionID, composer)`: the tab's mounted composer; returns its unregister.
pub fn register_composer(session_id: Option<String>, composer: PromptRef) -> impl FnOnce() + 'static {
    COMPOSERS.with(|c| {
        let mut c = c.borrow_mut();
        c.retain(|(k, _)| *k != session_id);
        c.push((session_id.clone(), composer.clone()));
    });
    move || {
        COMPOSERS.with(|c| c.borrow_mut().retain(|(k, existing)| !(*k == session_id && *existing == composer)));
    }
}

/// `appendToSessionPrompt(sessionID, prompt, widthMethod)`: a prompt that comes back after an await (an undo or a
/// revert) goes after anything already typed in the session's mounted composer, else in its stashed draft; returns
/// the composer it went to.
pub fn append_to_session_prompt(session_id: &str, prompt: PromptInfo) -> Option<PromptRef> {
    let key = Some(session_id.to_string());
    let composer = COMPOSERS.with(|c| c.borrow().iter().find(|(k, _)| *k == key).map(|(_, c)| c.clone()));
    let Some(composer) = composer else {
        let draft = take_draft(Some(session_id)).map(|d| d.prompt);
        let prompt = match draft {
            // The renderer's width method (the textarea's).
            Some(draft) => append_prompt(&draft, &prompt, WidthMethod::Unicode),
            None => prompt,
        };
        save_draft(Some(session_id), DraftEntry { prompt, cursor: u32::MAX });
        return None;
    };
    composer.set_mode(Mode::Normal);
    let width_method = crate::prompt::input::Textarea::of(composer.input()).map_or(WidthMethod::Unicode, |t| t.width_method());
    composer.set(&append_prompt(&composer.current(), &prompt, width_method));
    Some(composer)
}
