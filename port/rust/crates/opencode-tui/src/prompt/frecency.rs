//! File mention frecency (`prompt/frecency.tsx`, `useFrecency`): how often and how recently each file was
//! mentioned, in `<state>/frecency.jsonl` (the last 1000 paths, newest line per path).
use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;
use opencode_protocol::Json;
use opencode_tui_core::reactive::on_mount;

use crate::data::{get, get_str};
use crate::prompt::history::parse_lines;
use crate::prompt::persistence;

pub const MAX_FRECENCY_ENTRIES: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Entry {
    frequency: f64,
    last_open: f64,
}

fn entry_line(path: &str, entry: &Entry) -> String {
    opencode_tui_sync::client::stringify(&crate::data::object(vec![
        ("path", Json::String(path.to_string())),
        ("frequency", Json::Number(entry.frequency)),
        ("lastOpen", Json::Number(entry.last_open)),
    ]))
}

/// `parseFrecency(text)`: the newest entry per path, most recent first, at most 1000.
fn parse_frecency(text: &str) -> Vec<(String, Entry)> {
    let mut latest: IndexMap<String, Entry> = IndexMap::new();
    for line in parse_lines(text) {
        let Some(path) = get_str(&line, "path") else { continue };
        let num = |key: &str| match get(&line, key) {
            Some(Json::Number(n)) => *n,
            _ => f64::NAN,
        };
        // An object key keeps its first position when reassigned.
        latest.insert(path.to_string(), Entry { frequency: num("frequency"), last_open: num("lastOpen") });
    }
    let mut entries: Vec<(String, Entry)> = latest.into_iter().collect();
    entries.sort_by(|a, b| b.1.last_open.partial_cmp(&a.1.last_open).unwrap_or(std::cmp::Ordering::Equal));
    entries.truncate(MAX_FRECENCY_ENTRIES);
    entries
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

/// `path.resolve(base, file)` for POSIX paths.
pub fn resolve_path(base: &str, file: &str) -> String {
    let joined = if file.starts_with('/') { file.to_string() } else { format!("{base}/{file}") };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

/// `useFrecency()`.
#[derive(Clone)]
pub struct Frecency {
    path: String,
    cwd: String,
    data: Rc<RefCell<IndexMap<String, Entry>>>,
}

impl Frecency {
    pub fn new(state_dir: &str, cwd: &str) -> Frecency {
        let frecency = Frecency { path: format!("{state_dir}/frecency.jsonl"), cwd: cwd.to_string(), data: Default::default() };
        let (path, data) = (frecency.path.clone(), frecency.data.clone());
        on_mount(move || {
            tokio::task::spawn_local(async move {
                let lines = parse_frecency(&persistence::read_text(&path).await.unwrap_or_default());
                let rewrite = (!lines.is_empty())
                    .then(|| lines.iter().map(|(p, e)| entry_line(p, e)).collect::<Vec<_>>().join("\n") + "\n");
                *data.borrow_mut() = lines.into_iter().collect();
                if let Some(content) = rewrite {
                    persistence::write_text(&path, content);
                }
            });
        });
        frecency
    }

    /// `getFrecency(filePath)`.
    pub fn get_frecency(&self, file: &str) -> f64 {
        let absolute = resolve_path(&self.cwd, file);
        match self.data.borrow().get(&absolute) {
            None => 0.0,
            Some(entry) => entry.frequency / (1.0 + (now_ms() - entry.last_open) / 86_400_000.0),
        }
    }

    /// `updateFrecency(filePath)`.
    pub fn update_frecency(&self, file: &str) {
        let absolute = resolve_path(&self.cwd, file);
        let entry = {
            let mut data = self.data.borrow_mut();
            let entry = Entry { frequency: data.get(&absolute).map_or(0.0, |e| e.frequency) + 1.0, last_open: now_ms() };
            data.insert(absolute.clone(), entry);
            entry
        };
        persistence::append_text(&self.path, entry_line(&absolute, &entry) + "\n");
        let rewrite = {
            let mut data = self.data.borrow_mut();
            if data.len() <= MAX_FRECENCY_ENTRIES {
                return;
            }
            let mut sorted: Vec<(String, Entry)> = data.drain(..).collect();
            sorted.sort_by(|a, b| b.1.last_open.partial_cmp(&a.1.last_open).unwrap_or(std::cmp::Ordering::Equal));
            sorted.truncate(MAX_FRECENCY_ENTRIES);
            let content = sorted.iter().map(|(p, e)| entry_line(p, e)).collect::<Vec<_>>().join("\n") + "\n";
            *data = sorted.into_iter().collect();
            content
        };
        persistence::write_text(&self.path, rewrite);
    }
}
