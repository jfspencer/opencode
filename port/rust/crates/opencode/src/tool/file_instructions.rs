//! Instructions next to what the read tool reads (packages/core/src/tool/plugin/read.ts, after a read, and
//! session/instructions.ts `SessionInstructions.load`): the `AGENTS.md` files found walking up from the read file's
//! directory (a listed directory's own included) to the location, below the location itself (whose instructions the
//! session already has), are injected once per session as a durable synthetic message. The synthetics' metadata is
//! the ledger of what was injected; an in-memory claim keeps concurrent reads of one step from injecting a path twice.
//!
//! TODO(port): the ledger from the latest compaction on (`SessionHistory.synthetics`); compaction is not ported, so
//! every synthetic counts.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use opencode_protocol::Json;

use super::Context;
use super::file_access::{contains, dirname, join};
use crate::json::{self, Obj};

const FILENAME: &str = "AGENTS.md";

use super::file_access::fs_resolve as resolve;

/// `FSUtil.up({targets: [AGENTS.md], start, stop, type: "file"})`: every match walking up, the stop included.
fn up(start: &str, stop: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = start.to_owned();
    loop {
        let candidate = join(&current, FILENAME);
        if std::fs::metadata(&candidate).is_ok_and(|m| m.is_file()) {
            found.push(candidate);
        }
        if crate::project::normalize(stop) == crate::project::normalize(&current) {
            break;
        }
        let parent = dirname(&current);
        if parent == current {
            break;
        }
        current = parent;
    }
    found
}

/// After a successful read of `absolute` (internal targets only; `directory` when it listed one): discovers and
/// loads the nearby instructions. Failures never fail the read.
pub fn discover(context: &Context, absolute: &str, directory: bool) {
    let resolved = resolve(absolute);
    let root = resolve(&context.location.directory);
    let start = if directory { resolved.clone() } else { dirname(&resolved) };
    let candidates: Vec<String> = up(&start, &root)
        .iter()
        .map(|file| resolve(file))
        .filter(|file| !contains(&dirname(file), &root) && *file != resolved)
        .collect();
    if candidates.is_empty() {
        return;
    }
    load(context, &candidates);
}

fn in_flight() -> &'static Mutex<HashMap<String, HashSet<String>>> {
    static IN_FLIGHT: std::sync::OnceLock<Mutex<HashMap<String, HashSet<String>>>> = std::sync::OnceLock::new();
    IN_FLIGHT.get_or_init(Default::default)
}

/// The paths the session's synthetic messages say were injected.
fn injected(db: &rusqlite::Connection, session: &str) -> rusqlite::Result<HashSet<String>> {
    let mut statement = db.prepare(
        "SELECT json_extract(data, '$.metadata.instruction.paths') FROM session_message WHERE session_id = ?1 AND type = 'synthetic' ORDER BY seq ASC",
    )?;
    let rows = statement.query_map([session], |row| row.get::<_, Option<String>>(0))?;
    let mut out = HashSet::new();
    for paths in rows.flatten().flatten() {
        if let Some(Json::Array(items)) = json::parse(&paths) {
            // `InjectedMetadata` decodes only an array of strings.
            if items.iter().all(|item| matches!(item, Json::String(_))) {
                out.extend(items.iter().filter_map(json::as_str).map(str::to_owned));
            }
        }
    }
    Ok(out)
}

/// `SessionInstructions.load`.
fn load(context: &Context, paths: &[String]) {
    let session = &context.session_id;
    let claimed: Vec<String> = {
        let mut map = in_flight().lock().unwrap();
        let existing = map.entry(session.clone()).or_default();
        let claimed: Vec<String> = paths.iter().filter(|path| !existing.contains(*path)).cloned().collect();
        existing.extend(claimed.iter().cloned());
        claimed
    };
    if claimed.is_empty() {
        return;
    }
    publish(context, &claimed);
    let mut map = in_flight().lock().unwrap();
    if let Some(existing) = map.get_mut(session) {
        existing.retain(|path| !claimed.contains(path));
        if existing.is_empty() {
            map.remove(session);
        }
    }
}

fn publish(context: &Context, claimed: &[String]) {
    let app = &context.app;
    let already = match injected(&app.db.lock().unwrap(), &context.session_id) {
        Ok(already) => already,
        Err(_) => return,
    };
    let readable: Vec<(String, String)> = claimed
        .iter()
        .filter(|path| !already.contains(*path))
        .filter_map(|path| std::fs::read(path).ok().map(|bytes| (path.clone(), String::from_utf8_lossy(&bytes).into_owned())))
        .collect();
    if readable.is_empty() {
        return;
    }
    // The description shows paths relative to the project root; one outside it stays absolute.
    let root = resolve(&context.location.project.directory);
    let describe = |path: &str| if contains(&root, path) { crate::text::relative(&root, path) } else { path.to_owned() };
    let text = readable.iter().map(|(path, content)| format!("Instructions from: {path}\n{content}")).collect::<Vec<_>>().join("\n\n");
    let description = format!("Loaded {}", readable.iter().map(|(path, _)| describe(path)).collect::<Vec<_>>().join(", "));
    let mut instruction = Obj::new();
    instruction.set("paths", Json::Array(readable.iter().map(|(path, _)| json::string(path)).collect()));
    let mut metadata = Obj::new();
    metadata.set("instruction", instruction.done());
    let mut data = Obj::new();
    data.str("sessionID", &context.session_id).str("text", &text).str("description", &description).set("metadata", metadata.done());
    let _ = crate::session::publish_event(app, "session.synthetic", data.done(), Some(context.location.reference()), None);
}
