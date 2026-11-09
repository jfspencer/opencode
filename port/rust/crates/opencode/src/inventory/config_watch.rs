//! What a location watches for config changes (packages/core/src/config/watch.ts `ConfigWatch.plan`, and the watch
//! reconciliation of config.ts): the config directories present (system, global, project `.opencode` roots) as
//! trees, minus the ignored directories (`FSUtil.ignoreGlobs`), and every other source file or root by name in its
//! parent directory, so that creating or deleting one is seen too. A change reloads the config 100 ms after the last
//! one of a burst ([`crate::inventory::Shared::reload`]).

use crate::inventory::config::{self, Sources};
use crate::project::contains;

/// `FSUtil.ignoredDirectories`, anywhere below a watched root (`ignoreGlobs`).
fn ignored(relative: &str) -> bool {
    relative.split('/').any(|c| matches!(c, "node_modules" | ".git" | ".venv" | "venv" | "__pycache__"))
}

/// `ConfigWatch.plan`: the watches of a location's config sources, sending each change to `send`.
pub fn plan(sources: &Sources, send: &crate::watch::Sender) -> Vec<crate::watch::Watch> {
    let mut directories: Vec<&str> = vec![];
    if sources.system.1 {
        directories.push(&sources.system.0);
    }
    directories.push(&sources.global);
    directories.extend(sources.project.iter().filter(|(_, present)| *present).map(|(path, _)| path.as_str()));
    let mut files: Vec<&str> = vec![&sources.system.0];
    files.extend(sources.direct.iter().map(String::as_str));
    files.extend(sources.project.iter().map(|(path, _)| path.as_str()));
    files.extend(sources.claude.iter().map(String::as_str));
    files.extend(sources.agents.iter().map(String::as_str));
    files.extend(sources.explicit.as_deref());
    // A parent watch for each root keeps its deletion and recreation observable.
    let mut parents: Vec<String> = files
        .into_iter()
        .filter(|file| !directories.iter().any(|directory| file != directory && contains(directory, file)))
        .map(str::to_owned)
        .collect();
    parents.sort();
    parents.dedup();
    let mut watches: Vec<crate::watch::Watch> =
        directories.iter().filter_map(|directory| crate::watch::directory(directory, ignored, send)).collect();
    watches.extend(parents.iter().filter(|file| config::dirname(file) != **file).filter_map(|file| crate::watch::file(file, send)));
    watches
}
