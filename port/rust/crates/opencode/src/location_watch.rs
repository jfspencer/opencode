//! A location's watcher (packages/core/src/filesystem/location-watcher.ts, location-watcher-policy.ts,
//! config/plugin/location-watcher.ts): the repository's branch metadata is watched (git: `HEAD` in the location's git
//! directory; Mercurial: the store's `branch`), and each change is announced as `filesystem.changed` (`change`) on the
//! location, which makes its VCS service read the branch again ([`crate::vcs::changed`]).
//!
//! Config `watcher.ignore` lists (every document's, in order) turn it off when they name `.git` / `.hg` or the
//! metadata's directory. The watch follows them: it is reconciled when the location has booted and after each
//! config reload, and stops when the location shuts down.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::app::App;
use crate::inventory::Inventory;
use crate::inventory::config::Entry;
use crate::json::Obj;
use crate::location::Location;

/// What to watch, and the names that turn it off.
#[derive(Debug, Clone)]
struct Target {
    path: String,
    aliases: Vec<String>,
}

/// The watch in effect: its path, the subscription, and the task announcing its changes (dropped together).
struct Active {
    path: String,
    _watch: Option<crate::watch::Watch>,
    _task: crate::watch::Watch,
}

#[derive(Default)]
pub struct Watcher {
    /// The target, resolved on the first reconciliation (`Effect.cached`); held while reconciling.
    target: tokio::sync::Mutex<Option<Option<Target>>>,
    active: Mutex<Option<Active>>,
    stopped: AtomicBool,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher").field("stopped", &self.stopped.load(Ordering::Relaxed)).finish()
    }
}

/// `LocationWatcherPolicy.current()`: every document's `watcher.ignore`, in order.
fn ignored(inventory: &Inventory) -> Vec<String> {
    inventory
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Document { info, .. } => info.watcher.as_ref().and_then(|w| w.ignore.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn real_path(path: &str) -> String {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_owned())
}

/// The location's branch metadata (`target`).
async fn resolve(location: &Location) -> Option<Target> {
    let vcs = location.vcs.as_ref()?;
    match vcs.kind {
        "git" => {
            let directory = location.directory.clone();
            let repository = tokio::task::spawn_blocking(move || crate::project::repository(&directory)).await.ok()??;
            let resolved = repository.git_directory;
            let vcs = real_path(&resolved);
            Some(Target { path: format!("{vcs}/HEAD"), aliases: vec![".git".into(), vcs, resolved] })
        }
        _ => {
            let vcs = real_path(&vcs.store);
            Some(Target { path: format!("{vcs}/branch"), aliases: vec![".hg".into(), vcs] })
        }
    }
}

impl Watcher {
    /// `reconcile`: the watch follows the target and the current ignore lists.
    pub async fn reconcile(&self, app: &Arc<App>, location: &Location) {
        let mut target = self.target.lock().await;
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        if target.is_none() {
            *target = Some(resolve(location).await);
        }
        let ignore = ignored(&location.inventory());
        let next = target
            .as_ref()
            .and_then(Option::as_ref)
            .filter(|t| !t.aliases.iter().any(|alias| ignore.contains(alias)))
            .map(|t| t.path.clone());
        let mut active = self.active.lock().unwrap();
        if active.as_ref().map(|a| &a.path) == next.as_ref() {
            return;
        }
        let previous = active.take();
        *active = next.map(|path| start(app, location, path));
        drop(active);
        drop(previous);
    }

    /// The location shut down: the watch stops, and no reconciliation starts another.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let active = self.active.lock().unwrap().take();
        drop(active);
    }
}

fn start(app: &Arc<App>, location: &Location, path: String) -> Active {
    let (send, mut changes) = tokio::sync::mpsc::unbounded_channel::<String>();
    let watch = crate::watch::file(&path, &send);
    drop(send);
    let (app, reference) = (app.clone(), location.reference());
    let task = tokio::spawn(async move {
        while let Some(file) = changes.recv().await {
            let mut data = Obj::new();
            data.str("file", &file).str("event", "change");
            let options = crate::bus::PublishOptions { id: None, location: Some(reference.clone()) };
            app.bus.emit("filesystem.changed", data.done(), options);
            crate::vcs::changed(&app, &reference, &file);
        }
    });
    Active { path, _watch: watch, _task: crate::watch::Watch::task(task) }
}
