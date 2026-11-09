//! Stashed prompts (`prompt/stash.tsx`, `usePromptStash`): the last 50 in `<state>/prompt-stash.jsonl`.
use opencode_protocol::Json;
use opencode_tui_core::reactive::{Signal, create_signal, on_mount};

use crate::data::get;
use crate::prompt::history::{PromptInfo, parse_lines, parse_prompt_info, prompt_json};
use crate::prompt::persistence;

/// `StashEntry`.
#[derive(Clone, Debug, PartialEq)]
pub struct StashEntry {
    pub prompt: PromptInfo,
    pub timestamp: f64,
}

pub const MAX_STASH_ENTRIES: usize = 50;

fn entry_json(entry: &StashEntry) -> Json {
    crate::data::object(vec![("prompt", prompt_json(&entry.prompt)), ("timestamp", Json::Number(entry.timestamp))])
}

/// `parsePromptStash(text)`.
pub fn parse_prompt_stash(text: &str) -> Vec<StashEntry> {
    let entries: Vec<StashEntry> = parse_lines(text)
        .iter()
        .filter_map(|line| {
            let prompt = parse_prompt_info(get(line, "prompt")?)?;
            let Some(Json::Number(timestamp)) = get(line, "timestamp") else { return None };
            Some(StashEntry { prompt, timestamp: *timestamp })
        })
        .collect();
    let skip = entries.len().saturating_sub(MAX_STASH_ENTRIES);
    entries.into_iter().skip(skip).collect()
}

fn lines_of(entries: &[StashEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    entries.iter().map(|e| opencode_tui_sync::client::stringify(&entry_json(e))).collect::<Vec<_>>().join("\n") + "\n"
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

/// `usePromptStash()`.
#[derive(Clone)]
pub struct Stash {
    path: String,
    entries: Signal<Vec<StashEntry>>,
}

impl Stash {
    pub fn new(state_dir: &str) -> Stash {
        let path = format!("{state_dir}/prompt-stash.jsonl");
        let entries = create_signal(Vec::<StashEntry>::new()).0;
        let load = path.clone();
        on_mount(move || {
            tokio::task::spawn_local(async move {
                let lines = parse_prompt_stash(&persistence::read_text(&load).await.unwrap_or_default());
                let rewrite = (!lines.is_empty()).then(|| lines_of(&lines));
                entries.set(lines);
                if let Some(content) = rewrite {
                    persistence::write_text(&load, content);
                }
            });
        });
        Stash { path, entries }
    }

    /// `list()` (tracked).
    pub fn list(&self) -> Vec<StashEntry> {
        self.entries.get()
    }

    pub fn len(&self) -> usize {
        self.entries.with(|e| e.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `push({ prompt })`.
    pub fn push(&self, prompt: &PromptInfo) {
        let stash = StashEntry { prompt: prompt.clone(), timestamp: now_ms() };
        let mut trimmed = false;
        self.entries.update(|entries| {
            entries.push(stash.clone());
            if entries.len() > MAX_STASH_ENTRIES {
                let skip = entries.len() - MAX_STASH_ENTRIES;
                entries.drain(..skip);
                trimmed = true;
            }
        });
        if trimmed {
            persistence::write_text(&self.path, lines_of(&self.entries.get_untracked()));
            return;
        }
        persistence::append_text(&self.path, opencode_tui_sync::client::stringify(&entry_json(&stash)) + "\n");
    }

    /// `pop()`.
    pub fn pop(&self) -> Option<StashEntry> {
        let entry = self.entries.with_untracked(|e| e.last().cloned())?;
        self.entries.update(|e| {
            e.pop();
        });
        persistence::write_text(&self.path, lines_of(&self.entries.get_untracked()));
        Some(entry)
    }

    /// `remove(index)`.
    pub fn remove(&self, index: usize) {
        if index >= self.entries.with_untracked(|e| e.len()) {
            return;
        }
        self.entries.update(|e| {
            e.remove(index);
        });
        persistence::write_text(&self.path, lines_of(&self.entries.get_untracked()));
    }
}
