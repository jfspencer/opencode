//! Feature flags (packages/core/src/flag.ts, packages/schema/src/config/flag.ts): `OPENCODE_FLAGS` (`-key`, then
//! `all`), then `experimental.<key>` from the last document that sets `experimental`, then `experimental.all_flags:
//! true` from the last document that sets `all_flags`, then on for local and dev builds.

use opencode_protocol::types as t;

use crate::inventory::config::{self, Entry};

fn configured(experimental: &t::ConfigExperimentalInfo, key: &str) -> Option<bool> {
    match key {
        "todo" => experimental.todo,
        "background_compaction" => experimental.background_compaction,
        "media_blob" => experimental.media_blob,
        "schedules" => experimental.schedules,
        "artifacts" => experimental.artifacts,
        "codemode_vm" => experimental.codemode_vm,
        _ => None,
    }
}

/// `Flag.resolve(...)[key].enabled` for this build's channel and the process's `OPENCODE_FLAGS`.
pub fn enabled(entries: &[Entry], key: &str) -> bool {
    let overrides = std::env::var("OPENCODE_FLAGS").unwrap_or_default();
    let mut env: Vec<(&str, bool)> = Vec::new();
    for item in overrides.split(',').map(str::trim).filter(|item| !item.is_empty()) {
        let (name, value) = match item.strip_prefix('-') {
            Some(name) => (name, false),
            None => (item, true),
        };
        // `new Map(entries)`: the last entry of a key wins.
        env.retain(|(existing, _)| *existing != name);
        env.push((name, value));
    }
    let lookup = |name: &str| env.iter().find(|(existing, _)| *existing == name).map(|(_, value)| *value);
    let all = lookup("all") == Some(true);
    let all_flags = config::documents(entries)
        .rev()
        .find_map(|info| info.experimental.as_ref().and_then(|e| e.all_flags))
        .unwrap_or(false);
    let experimental = config::latest(entries, |info| info.experimental.as_ref());
    if let Some(value) = lookup(key).or(all.then_some(true)) {
        return value;
    }
    if let Some(value) = experimental.and_then(|e| configured(e, key)).or(all_flags.then_some(true)) {
        return value;
    }
    matches!(crate::app::CHANNEL, "local" | "dev")
}

/// `experimental.part_rows`, which is a setting rather than a flag: on only when the last `experimental` says so.
pub fn part_rows(entries: &[Entry]) -> bool {
    config::latest(entries, |info| info.experimental.as_ref()).and_then(|e| e.part_rows) == Some(true)
}
