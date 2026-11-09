//! Ambient instructions (packages/core/src/instruction-discovery.ts, config/plugin/instruction.ts,
//! session/instructions.ts):
//!
//! - [`discover`]: what a location's model requests carry under `core/instructions`, in render order: the global
//!   `AGENTS.md` (`$XDG_CONFIG_HOME/opencode/AGENTS.md`), the project's `AGENTS.md` files from the location up to the
//!   project root (nearest first; up to the home directory when the location is under it), then config
//!   `instructions` entries: files and globs (absolute, `~/`, or relative and searched upward from the location to the
//!   project root), each sorted and resolved, and URLs (their last fetched copy). A project file that vanished
//!   between the walk and the read makes the whole set unavailable (the stored instructions stand); an unreadable
//!   one is skipped.
//! - [`fetch`]: the configured URLs, fetched when the config loads (5 s each, at most 4 at once); a failure keeps
//!   the previous copy, or skips the URL.
//! - [`nested`]: after a read, the `AGENTS.md` files between the read target and the location (exclusive) that the
//!   session has not been given yet, injected as one synthetic message (`SessionInstructions.load`). This is the
//!   read tool's entry point.
//!
//! TODO(port): the file watcher's rescans (on an `AGENTS.md` or configured file change, debounced 100 ms) and the
//! plugin-provided instruction files (OpenCode Console's).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use opencode_protocol::Json;

use crate::inventory::config::{self, Entry};
use crate::json::{self, Obj};

/// `InstructionDiscovery.File`: an absolute path (or a configured URL) and its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub content: String,
}

/// `InstructionDiscovery.list()`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Discovered {
    Available(Vec<File>),
    /// A source failed: the session keeps what it was given (and cannot start without it).
    #[default]
    Unavailable,
}

impl Discovered {
    /// The `core/instructions` value: the files (`[{path, content}]`), `None` (removed) when there are none.
    pub fn value(&self) -> Option<Option<Json>> {
        match self {
            Discovered::Unavailable => None,
            Discovered::Available(files) if files.is_empty() => Some(None),
            Discovered::Available(files) => Some(Some(Json::Array(
                files
                    .iter()
                    .map(|file| {
                        let mut o = Obj::new();
                        o.str("path", &file.path).str("content", &file.content);
                        o.done()
                    })
                    .collect(),
            ))),
        }
    }
}

/// The configured URLs' last good copies, in fetch order.
pub type Remote = Vec<File>;

/// `FSUtil.resolve`: absolute, with symlinks resolved where the path exists.
fn resolve(path: &str) -> String {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| crate::project::normalize(path))
}

/// `FSUtil.contains(parent, child)`.
fn contains(parent: &str, child: &str) -> bool {
    let parent = parent.trim_end_matches('/');
    child == parent || child.starts_with(&format!("{parent}/")) || parent.is_empty()
}

fn is_file(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// `ConfigInstructionPlugin.read`: `Ok(None)` for a missing file, `Err(())` for a denied one.
fn read(path: &str) -> Result<Option<File>, ()> {
    match std::fs::read(path) {
        Ok(_) => Ok(config::read(path).map(|content| File { path: path.to_owned(), content })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Err(()),
        // Other failures (a directory, I/O errors) surface as a failed source.
        Err(_) => Ok(None),
    }
}

/// `FSUtil.up({targets: [name], start, stop, type: "file"})`.
fn up(name: &str, start: &str, stop: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = start.to_owned();
    loop {
        let candidate = config::join(&current, name);
        if is_file(&candidate) {
            found.push(candidate);
        }
        if current == stop {
            break;
        }
        let parent = config::dirname(&current);
        if parent == current {
            break;
        }
        current = parent;
    }
    found
}

/// The configured `instructions` entries, every document's in order, without repeats.
fn configured(entries: &[Entry]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in config::documents(entries).flat_map(|d| d.instructions.iter().flatten()) {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

/// Where a location discovers from.
struct Scope {
    start: String,
    root: String,
    stop: String,
    project: bool,
    global_file: String,
}

impl Scope {
    fn new(directory: &str, project_directory: &str) -> Scope {
        let options = config::Options::get();
        let start = resolve(directory);
        let root = resolve(project_directory);
        let home = resolve(&options.home);
        let project = options.project && contains(&root, &start);
        let stop = if contains(&home, &start) { home } else { root.clone() };
        Scope { start, root, stop, project, global_file: resolve(&config::join(&options.global, "AGENTS.md")) }
    }

    /// `match(source)`: the files a configured path or glob names.
    fn matches(&self, source: &str) -> Vec<String> {
        let options = config::Options::get();
        let expanded = match source.strip_prefix("~/") {
            Some(rest) => config::join(&options.home, rest),
            None => source.to_owned(),
        };
        if expanded.starts_with('/') {
            let directory = config::dirname(&expanded);
            if config::is_dir(&directory) {
                return glob::scan(config::basename(&expanded), &directory);
            }
            return glob::scan(&expanded, "/");
        }
        if self.project {
            let mut out = Vec::new();
            let mut current = self.start.clone();
            loop {
                out.extend(glob::scan(&expanded, &current));
                if current == self.root {
                    break;
                }
                let parent = config::dirname(&current);
                if parent == current {
                    break;
                }
                current = parent;
            }
            return out;
        }
        glob::scan(&expanded, &options.global)
    }
}

/// The location's instructions (`refresh` of config/plugin/instruction.ts, as `InstructionDiscovery.list` sees it).
pub fn discover(directory: &str, project_directory: &str, entries: &[Entry], remote: &Remote) -> Discovered {
    let scope = Scope::new(directory, project_directory);
    let mut files = Vec::new();
    // Global.
    if is_file(&scope.global_file) {
        if let Ok(Some(file)) = read(&scope.global_file) {
            files.push(file);
        }
    }
    // Project: a file that disappears between the walk and the read makes the set unavailable.
    if scope.project {
        let mut seen = HashSet::new();
        for path in up("AGENTS.md", &scope.start, &scope.stop).iter().map(|p| resolve(p)) {
            if !seen.insert(path.clone()) {
                continue;
            }
            match read(&path) {
                Ok(Some(file)) => files.push(file),
                Ok(None) => return Discovered::Unavailable,
                Err(()) => {}
            }
        }
    }
    // Configured.
    let mut watched = HashSet::new();
    for source in configured(entries) {
        if crate::url::is_http(&source) {
            files.extend(remote.iter().find(|file| file.path == source).cloned());
            continue;
        }
        let mut matched = scope.matches(&source);
        matched.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        for item in matched {
            let resolved = resolve(&item);
            if !watched.insert(resolved.clone()) {
                continue;
            }
            if let Ok(Some(file)) = read(&resolved) {
                files.push(file);
            }
        }
    }
    Discovered::Available(files)
}

/// `reconfigure`: fetches the configured URLs (keeping a previous copy when a fetch fails).
pub async fn fetch(entries: &[Entry], previous: &[File]) -> Remote {
    let urls: Vec<String> = configured(entries).into_iter().filter(|s| crate::url::is_http(s)).collect();
    let mut fetched = Vec::new();
    for chunk in urls.chunks(4) {
        let results = futures_join(chunk.iter().map(|url| fetch_one(url.clone()))).await;
        for (url, result) in chunk.iter().zip(results) {
            match result {
                Some(content) => fetched.push(File { path: url.clone(), content }),
                None => fetched.extend(previous.iter().find(|file| &file.path == url).cloned()),
            }
        }
    }
    fetched
}

/// Runs the futures concurrently on the current task, in order.
async fn futures_join<F: std::future::Future>(futures: impl Iterator<Item = F>) -> Vec<F::Output> {
    let mut pinned: Vec<std::pin::Pin<Box<F>>> = futures.map(Box::pin).collect();
    let mut results: Vec<Option<F::Output>> = pinned.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (future, result) in pinned.iter_mut().zip(results.iter_mut()) {
            if result.is_some() {
                continue;
            }
            match future.as_mut().poll(cx) {
                std::task::Poll::Ready(value) => *result = Some(value),
                std::task::Poll::Pending => pending = true,
            }
        }
        if pending { std::task::Poll::Pending } else { std::task::Poll::Ready(()) }
    })
    .await;
    results.into_iter().map(|r| r.expect("every future finished")).collect()
}

/// One URL (`HttpClient.filterStatusOk`, `response.text`, 5 s).
async fn fetch_one(url: String) -> Option<String> {
    let target = crate::url::Url::parse(&url)?;
    let request = async {
        let response = crate::llm::http::send("GET", &target.href(), &[], String::new()).await.ok()?;
        if !(200..300).contains(&response.status) {
            return None;
        }
        response.text().await.ok()
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), request).await.ok().flatten()
}

// --- Nested instructions (session/instructions.ts) ----------------------------------------------------------------

/// Paths claimed by a load in flight, per session.
fn in_flight() -> &'static Mutex<HashMap<String, HashSet<String>>> {
    static CLAIMS: std::sync::OnceLock<Mutex<HashMap<String, HashSet<String>>>> = std::sync::OnceLock::new();
    CLAIMS.get_or_init(Default::default)
}

/// The paths the session's synthetic messages (since its latest compaction) already carry.
fn injected(db: &rusqlite::Connection, session: &str) -> rusqlite::Result<HashSet<String>> {
    let from: i64 = db.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM session_message WHERE session_id = ?1 AND type = 'compaction'",
        [session],
        |row| row.get(0),
    )?;
    let mut statement =
        db.prepare("SELECT data FROM session_message WHERE session_id = ?1 AND type = 'synthetic' AND seq >= ?2 ORDER BY seq")?;
    let rows = statement.query_map(rusqlite::params![session, from], |row| row.get::<_, String>(0))?;
    let mut out = HashSet::new();
    for data in rows {
        let Some(data) = json::parse(&data?) else { continue };
        let paths = json::get(&data, "metadata").and_then(|m| json::get(m, "instruction")).and_then(|i| json::get(i, "paths"));
        if let Some(Json::Array(paths)) = paths {
            out.extend(paths.iter().filter_map(json::as_str).map(str::to_owned));
        }
    }
    Ok(out)
}

/// `SessionInstructions.load`: injects the files the session has not been given yet.
pub fn load(app: &crate::app::App, location: &crate::location::Location, session: &str, paths: &[String]) {
    let claimed: Vec<String> = {
        let mut claims = in_flight().lock().unwrap();
        let existing = claims.entry(session.to_owned()).or_default();
        let fresh: Vec<String> = paths.iter().filter(|p| !existing.contains(*p)).cloned().collect();
        existing.extend(fresh.iter().cloned());
        fresh
    };
    if claimed.is_empty() {
        return;
    }
    let _ = inject(app, location, session, &claimed);
    let mut claims = in_flight().lock().unwrap();
    if let Some(existing) = claims.get_mut(session) {
        existing.retain(|p| !claimed.contains(p));
        if existing.is_empty() {
            claims.remove(session);
        }
    }
}

fn inject(app: &crate::app::App, location: &crate::location::Location, session: &str, claimed: &[String]) -> rusqlite::Result<()> {
    let already = injected(&app.db.lock().unwrap(), session)?;
    let readable: Vec<File> = claimed
        .iter()
        .filter(|path| !already.contains(*path))
        .filter_map(|path| config::read(path).map(|content| File { path: path.clone(), content }))
        .collect();
    if readable.is_empty() {
        return Ok(());
    }
    // Paths are shown relative to the project root when under it.
    let root = resolve(&location.project.directory);
    let describe = |path: &str| {
        if contains(&root, path) {
            path.strip_prefix(&root).map(|rest| rest.trim_start_matches('/')).unwrap_or(path).to_owned()
        } else {
            path.to_owned()
        }
    };
    let text = readable.iter().map(|f| format!("Instructions from: {}\n{}", f.path, f.content)).collect::<Vec<_>>().join("\n\n");
    let description = format!("Loaded {}", readable.iter().map(|f| describe(&f.path)).collect::<Vec<_>>().join(", "));
    let mut instruction = Obj::new();
    instruction.set("paths", Json::Array(readable.iter().map(|f| json::string(&f.path)).collect()));
    let mut metadata = Obj::new();
    metadata.set("instruction", instruction.done());
    let mut data = Obj::new();
    data.str("sessionID", session).str("text", &text).str("description", &description).set("metadata", metadata.done());
    crate::session::publish_event(app, "session.synthetic", data.done(), Some(location.reference()), None).map(drop)
}

/// The read tool's nested instructions: after a successful read of `target` (absolute; `listing` when it is a
/// directory), the `AGENTS.md` files from it (a directory) or its directory up to the location, exclusive, that are
/// not the target itself. External reads are the caller's to skip; failures never fail the read.
#[allow(dead_code)] // Called by the read tool (the file tools).
pub fn nested(app: &crate::app::App, location: &crate::location::Location, session: &str, target: &str, listing: bool) {
    let resolved = resolve(target);
    let root = resolve(&location.directory);
    let start = if listing { resolved.clone() } else { config::dirname(&resolved) };
    let candidates: Vec<String> = up("AGENTS.md", &start, &root)
        .iter()
        .map(|p| resolve(p))
        .filter(|file| !contains(&config::dirname(file), &root) && *file != resolved)
        .collect();
    if !candidates.is_empty() {
        load(app, location, session, &candidates);
    }
}

/// The `glob` package's `glob(pattern, { cwd, absolute: true, nodir: true, dot: true })` for the patterns
/// instructions use: `*`, `?`, `[...]`, `{a,b}` and `**` segments; symlinked directories are not descended by `**`.
/// TODO(port): extglobs (`+(a|b)`) and minimatch's escapes inside classes.
mod glob {
    use std::collections::HashSet;

    enum Segment {
        Literal(String),
        Pattern(regex::Regex),
        Globstar,
    }

    /// Brace expansion (`a{b,c}d` → `abd`, `acd`), nested braces included.
    fn expand(pattern: &str) -> Vec<String> {
        let bytes = pattern.as_bytes();
        let Some(open) = pattern.find('{') else { return vec![pattern.to_owned()] };
        let mut depth = 0;
        let mut parts = Vec::new();
        let mut start = open + 1;
        for (i, &b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        parts.push(&pattern[start..i]);
                        if parts.len() < 2 {
                            // `{x}` is not a set: keep it literal and expand what follows.
                            return expand(&pattern[i + 1..])
                                .into_iter()
                                .map(|rest| format!("{}{rest}", &pattern[..=i]))
                                .collect();
                        }
                        let (prefix, suffix) = (&pattern[..open], &pattern[i + 1..]);
                        return parts
                            .iter()
                            .flat_map(|part| expand(&format!("{prefix}{part}{suffix}")))
                            .collect();
                    }
                }
                b',' if depth == 1 => {
                    parts.push(&pattern[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        vec![pattern.to_owned()]
    }

    fn segment(text: &str) -> Segment {
        if text == "**" {
            return Segment::Globstar;
        }
        if !text.contains(['*', '?', '[']) {
            return Segment::Literal(text.to_owned());
        }
        let mut out = String::from("^");
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '*' => out.push_str("[^/]*"),
                '?' => out.push_str("[^/]"),
                '[' => match chars[i + 1..].iter().position(|c| *c == ']').map(|p| p + i + 1) {
                    Some(end) if end > i + 1 => {
                        let mut class: String = chars[i + 1..end].iter().collect();
                        if let Some(rest) = class.strip_prefix('!') {
                            class = format!("^{rest}");
                        }
                        out.push('[');
                        out.push_str(&class.replace('\\', "\\\\"));
                        out.push(']');
                        i = end;
                    }
                    _ => out.push_str("\\["),
                },
                '\\' if i + 1 < chars.len() => {
                    i += 1;
                    out.push_str(&regex::escape(&chars[i].to_string()));
                }
                c => out.push_str(&regex::escape(&c.to_string())),
            }
            i += 1;
        }
        out.push('$');
        regex::Regex::new(&out).map(Segment::Pattern).unwrap_or_else(|_| Segment::Literal(text.to_owned()))
    }

    fn join(directory: &str, name: &str) -> String {
        if directory.ends_with('/') { format!("{directory}{name}") } else { format!("{directory}/{name}") }
    }

    fn walk(directory: &str, segments: &[Segment], out: &mut Vec<String>, seen: &mut HashSet<String>) {
        let Some((first, rest)) = segments.split_first() else {
            if std::fs::metadata(directory).is_ok_and(|m| !m.is_dir()) && seen.insert(directory.to_owned()) {
                out.push(directory.to_owned());
            }
            return;
        };
        match first {
            Segment::Literal(name) => {
                let path = match name.as_str() {
                    "" | "." => directory.to_owned(),
                    ".." => crate::inventory::config::dirname(directory),
                    _ => join(directory, name),
                };
                if rest.is_empty() || std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
                    walk(&path, rest, out, seen);
                }
            }
            Segment::Pattern(pattern) => {
                let Ok(entries) = std::fs::read_dir(directory) else { return };
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !pattern.is_match(&name) {
                        continue;
                    }
                    let path = join(directory, &name);
                    if rest.is_empty() || std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
                        walk(&path, rest, out, seen);
                    }
                }
            }
            Segment::Globstar => {
                walk(directory, rest, out, seen);
                let Ok(entries) = std::fs::read_dir(directory) else { return };
                for entry in entries.flatten() {
                    // `follow: false`: symlinked directories are not descended.
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        let path = join(directory, &entry.file_name().to_string_lossy());
                        walk(&path, segments, out, seen);
                    }
                }
            }
        }
    }

    /// The files `pattern` matches under `cwd` (or from the root for an absolute pattern), as absolute paths.
    pub fn scan(pattern: &str, cwd: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for pattern in expand(pattern) {
            let (base, body) = match pattern.strip_prefix('/') {
                Some(body) => ("/".to_owned(), body.to_owned()),
                None => (cwd.to_owned(), pattern.clone()),
            };
            let segments: Vec<Segment> = body.split('/').filter(|s| !s.is_empty()).map(segment).collect();
            if segments.is_empty() {
                continue;
            }
            walk(&base, &segments, &mut out, &mut seen);
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn braces() {
            assert_eq!(expand("a{b,c}d"), ["abd", "acd"]);
            assert_eq!(expand("{x}y"), ["{x}y"]);
            assert_eq!(expand("{a,{b,c}}"), ["a", "b", "c"]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_up_and_globs() {
        let root = std::env::temp_dir().join(format!("oc-discovery-{}", std::process::id()));
        let root = root.to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(format!("{root}/a/b/docs/deep")).unwrap();
        for file in ["AGENTS.md", "a/AGENTS.md", "a/b/docs/x.md", "a/b/docs/y.txt", "a/b/docs/deep/z.md"] {
            std::fs::write(format!("{root}/{file}"), file).unwrap();
        }
        let found = up("AGENTS.md", &format!("{root}/a/b"), &root);
        assert_eq!(found, [format!("{root}/a/AGENTS.md"), format!("{root}/AGENTS.md")]);
        let mut docs = glob::scan("docs/*.md", &format!("{root}/a/b"));
        docs.sort();
        assert_eq!(docs, [format!("{root}/a/b/docs/x.md")]);
        let mut deep = glob::scan("**/*.md", &format!("{root}/a/b"));
        deep.sort();
        assert_eq!(deep, [format!("{root}/a/b/docs/deep/z.md"), format!("{root}/a/b/docs/x.md")]);
        assert!(glob::scan("no-such/**/*.md", &root).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
