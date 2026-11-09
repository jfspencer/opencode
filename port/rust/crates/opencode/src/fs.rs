//! A location's file system (packages/core/src/filesystem.ts, filesystem/search.ts, filesystem/file-watch.ts,
//! ripgrep.ts, ripgrep/binary.ts): reading files confined to the location, listing directories, writing files, fuzzy
//! file search (ripgrep's file list ranked by fuzzysort; the port omits fff) and file watches.
//!
//! Watched files announce `filesystem.changed` on their location. Without `rg` on `PATH` or in
//! `<XDG_CACHE_HOME>/opencode/bin`, the pinned ripgrep release is downloaded there (`RipgrepBinary`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use opencode_protocol::Json;

use crate::json::Obj;
use crate::location::Location;
use crate::project::contains;

#[path = "generated/mime.rs"]
mod mime;

/// `FSUtil.mimeType` (mime-types' `lookup`, else `application/octet-stream`).
pub fn mime_type(path: &str) -> &'static str {
    let extension = crate::text::extname(path).trim_start_matches('.').to_lowercase();
    if extension.is_empty() {
        return "application/octet-stream";
    }
    mime::TYPES
        .binary_search_by(|(ext, _)| (*ext).cmp(extension.as_str()))
        .map(|index| mime::TYPES[index].1)
        .unwrap_or("application/octet-stream")
}

/// An error the TS server lets die (`Effect.orDie`): an HTTP 500.
#[derive(Debug)]
pub struct Defect(pub std::io::Error);

/// `FileSystem.NotFoundError`: the path as the caller gave it.
#[derive(Debug)]
pub struct NotFound(pub String);

#[derive(Debug)]
pub enum Error {
    NotFound(NotFound),
    Defect(Defect),
}

impl From<NotFound> for Error {
    fn from(value: NotFound) -> Self {
        Error::NotFound(value)
    }
}

/// A file `read` found.
#[derive(Debug)]
pub struct File {
    /// Its real path.
    pub path: String,
    pub mime: &'static str,
    pub size: u64,
    /// Modification time in milliseconds.
    pub mtime: Option<i64>,
}

/// A path that does not exist, or that runs through a file (ENOTDIR), names nothing (D219); any other failure dies.
fn not_found_or_die(error: std::io::Error, path: &str) -> Error {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => Error::NotFound(NotFound(path.to_owned())),
        _ => Error::Defect(Defect(error)),
    }
}

/// `path.resolve(base, input)`.
pub fn resolve(base: &str, input: &str) -> String {
    if input.starts_with('/') { crate::project::normalize(input) } else { crate::project::normalize(&format!("{base}/{input}")) }
}

/// `FileSystem.read`: a file inside the location (after resolving symlinks).
pub fn read(location: &Location, input: &str) -> Result<File, Error> {
    if input.contains('\0') {
        return Err(NotFound(input.into()).into());
    }
    let absolute = resolve(&location.directory, input);
    if !contains(&location.directory, &absolute) {
        return Err(NotFound(input.into()).into());
    }
    let real = std::fs::canonicalize(&absolute).map_err(|e| not_found_or_die(e, input))?.to_string_lossy().into_owned();
    if !contains(&location.root, &real) {
        return Err(NotFound(input.into()).into());
    }
    let info = std::fs::metadata(&real).map_err(|e| not_found_or_die(e, input))?;
    if !info.is_file() {
        return Err(NotFound(input.into()).into());
    }
    let mtime = info.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64);
    Ok(File { mime: mime_type(&real), path: real, size: info.len(), mtime })
}

/// `FileSystem.Entry`.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: String,
    pub directory: bool,
}

impl Entry {
    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        o.str("path", &self.path).str("type", if self.directory { "directory" } else { "file" });
        o.done()
    }
}

/// `FileSystem.list`: a directory's files and directories, directories first, each by `localeCompare`. The
/// directory may be outside the location; paths stay relative to it.
pub fn list(location: &Location, input: Option<&str>) -> Result<Vec<Entry>, Error> {
    let given = input.unwrap_or(".");
    let directory = resolve(&location.directory, given);
    let info = std::fs::metadata(&directory).map_err(|e| not_found_or_die(e, given))?;
    if !info.is_dir() {
        return Err(NotFound(given.into()).into());
    }
    let read = std::fs::read_dir(&directory).map_err(|e| Error::Defect(Defect(e)))?;
    let mut entries: Vec<Entry> = read
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            if !kind.is_file() && !kind.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let absolute = crate::project::normalize(&format!("{directory}/{name}"));
            let relative = Some(crate::text::relative(&location.directory, &absolute)).filter(|r| !r.is_empty()).unwrap_or_else(|| ".".into());
            Some(Entry { path: if kind.is_dir() { format!("{relative}/") } else { relative }, directory: kind.is_dir() })
        })
        .collect();
    entries.sort_by(|a, b| match (a.directory, b.directory) {
        (x, y) if x == y => crate::collate::compare(&a.path, &b.path),
        (true, _) => std::cmp::Ordering::Less,
        _ => std::cmp::Ordering::Greater,
    });
    Ok(entries)
}

/// `FileSystem.write`: absolute, or relative to the location; not confined to it (clients stage files in the
/// server's temporary directory). The parent directories are created.
pub fn write(location: &Location, path: &str, data: &[u8]) -> Result<String, Defect> {
    let target = resolve(&location.directory, path);
    if let Some(parent) = Path::new(&target).parent() {
        std::fs::create_dir_all(parent).map_err(Defect)?;
    }
    std::fs::write(&target, data).map_err(Defect)?;
    Ok(target)
}

// --- search (filesystem/search.ts, ripgrep layer) ---

const REFRESH_INTERVAL_MS: u128 = 10_000;

#[derive(Default)]
struct Index {
    /// Files, then the directories they are in (with a trailing `/`), in the order ripgrep listed them.
    files: indexmap::IndexSet<String>,
    directories: indexmap::IndexSet<String>,
}

#[derive(Default)]
struct SearchState {
    index: Option<Arc<Index>>,
    settled: Option<Instant>,
    refreshing: bool,
}

/// A location's file search (`FileSystemSearch.Service`): ripgrep's file list, rescanned at most every 10 seconds
/// (in the background once there is an index).
#[derive(Default)]
pub struct Search {
    directory: String,
    state: Arc<Mutex<SearchState>>,
    /// Serializes the first scan.
    first: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Search {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Search").field("directory", &self.directory).finish()
    }
}

/// `FileSystem.FindInput`.
pub struct FindInput {
    pub query: String,
    /// `Some(true)` for directories, `Some(false)` for files, `None` for both.
    pub directories: Option<bool>,
    pub limit: Option<usize>,
}

/// A ripgrep failure (the TS server dies on it: HTTP 500).
#[derive(Debug)]
pub struct SearchError(pub String);

impl Search {
    pub fn new(directory: &str) -> Self {
        Self { directory: directory.into(), ..Default::default() }
    }

    pub async fn find(&self, location: &Location, input: &FindInput) -> Result<Vec<Entry>, SearchError> {
        let index = self.refresh(location).await?;
        let items: Vec<&str> = match input.directories {
            Some(false) => index.files.iter().map(String::as_str).collect(),
            Some(true) => index.directories.iter().map(String::as_str).collect(),
            None => index.files.iter().chain(index.directories.iter()).map(String::as_str).collect(),
        };
        Ok(crate::fuzzysort::go(&input.query, &items, Some(input.limit.unwrap_or(50)))
            .into_iter()
            .map(|i| Entry { path: items[i].to_owned(), directory: items[i].ends_with('/') })
            .collect())
    }

    async fn refresh(&self, location: &Location) -> Result<Arc<Index>, SearchError> {
        let limit = scan_limit(location);
        let stale = |state: &SearchState| state.settled.is_none_or(|at| at.elapsed().as_millis() >= REFRESH_INTERVAL_MS);
        {
            let mut state = self.state.lock().unwrap();
            if let Some(index) = state.index.clone() {
                if !state.refreshing && stale(&state) {
                    state.refreshing = true;
                    let (shared, directory) = (self.state.clone(), location.directory.clone());
                    tokio::spawn(async move {
                        let scanned = scan(&directory, limit).await;
                        let mut state = shared.lock().unwrap();
                        if let Ok(index) = scanned {
                            state.index = Some(Arc::new(index));
                        }
                        state.settled = Some(Instant::now());
                        state.refreshing = false;
                    });
                }
                return Ok(index);
            }
        }
        let _first = self.first.lock().await;
        if let Some(index) = self.state.lock().unwrap().index.clone() {
            return Ok(index);
        }
        let scanned = scan(&location.directory, limit).await;
        let mut state = self.state.lock().unwrap();
        state.settled = Some(Instant::now());
        let index = Arc::new(scanned?);
        state.index = Some(index.clone());
        Ok(index)
    }
}

/// `Protected.isHome`: the user's home directory (whose scan is bounded and skips the platform's personal folders).
fn is_home(directory: &str) -> bool {
    std::env::var("HOME").is_ok_and(|home| crate::project::normalize(&home) == crate::project::normalize(directory))
}

fn scan_limit(location: &Location) -> usize {
    if location.vcs.is_some() && !is_home(&location.directory) { 9_007_199_254_740_991 } else { 100_000 }
}

/// Why ripgrep is unavailable: a failure its callers report in their own words (the download's HTTP request or a file
/// write failed), or a defect, whose message they pass on (verification, extraction, an unsupported platform).
#[derive(Debug, Clone)]
pub enum RipgrepUnavailable {
    Failed,
    Defect(String),
}

impl RipgrepUnavailable {
    /// The message a caller reports, given its own for a failure.
    pub fn message(&self, failed: &str) -> String {
        match self {
            RipgrepUnavailable::Failed => failed.to_owned(),
            RipgrepUnavailable::Defect(message) => message.clone(),
        }
    }
}

/// The ripgrep release downloaded when none is installed, and the SHA-256 of each platform's asset (from the
/// `<asset>.sha256` files published with it: an asset is only written and run when it matches).
const RIPGREP_VERSION: &str = "15.1.0";
const RIPGREP_PLATFORMS: [(&str, &str, &str); 4] = [
    ("arm64-darwin", "aarch64-apple-darwin", "378e973289176ca0c6054054ee7f631a065874a352bf43f0fa60ef079b6ba715"),
    ("arm64-linux", "aarch64-unknown-linux-gnu", "2b661c6ef508e902f388e9098d9c4c5aca72c87b55922d94abdba830b4dc885e"),
    ("x64-darwin", "x86_64-apple-darwin", "64811cb24e77cac3057d6c40b63ac9becf9082eedd54ca411b475b755d334882"),
    ("x64-linux", "x86_64-unknown-linux-musl", "1c9297be4a084eea7ecaedf93eb03d058d6faae29bbc57ecdaf5063921491599"),
];

/// `Global.Path.bin`.
fn global_bin() -> String {
    let cache = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.cache", std::env::var("HOME").unwrap_or_default()));
    format!("{cache}/opencode/bin")
}

/// `RipgrepBinary.filepath`: `rg` from `PATH`, else the copy in opencode's cache, else the pinned release downloaded
/// into the cache. Resolved once per process, failure included (`Effect.cached`).
pub async fn ripgrep() -> Result<String, RipgrepUnavailable> {
    static RESOLVED: tokio::sync::OnceCell<Result<String, RipgrepUnavailable>> = tokio::sync::OnceCell::const_new();
    RESOLVED.get_or_init(resolve_ripgrep).await.clone()
}

async fn resolve_ripgrep() -> Result<String, RipgrepUnavailable> {
    let bin = global_bin();
    if let Some(system) = crate::process::which("rg", Some(&bin)) {
        return Ok(system);
    }
    let target = format!("{bin}/rg");
    if Path::new(&target).is_file() {
        return Ok(target);
    }
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else if cfg!(target_arch = "x86_64") { "x64" } else { std::env::consts::ARCH };
    let os = if cfg!(target_os = "macos") { "darwin" } else { std::env::consts::OS };
    let key = format!("{arch}-{os}");
    let Some((_, platform, sha256)) = RIPGREP_PLATFORMS.iter().find(|(k, _, _)| *k == key) else {
        return Err(RipgrepUnavailable::Defect(format!("unsupported platform for ripgrep: {key}")));
    };
    let filename = format!("ripgrep-{RIPGREP_VERSION}-{platform}.tar.gz");
    let url = format!("https://github.com/BurntSushi/ripgrep/releases/download/{RIPGREP_VERSION}/{filename}");
    let archive = format!("{bin}/{filename}");
    std::fs::create_dir_all(&bin).map_err(|e| RipgrepUnavailable::Defect(e.to_string()))?;
    let bytes = download(&url).await.ok_or(RipgrepUnavailable::Failed)?;
    if bytes.is_empty() {
        return Err(RipgrepUnavailable::Defect(format!("failed to download ripgrep from {url}")));
    }
    let actual: String = <sha2::Sha256 as sha2::Digest>::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if actual != *sha256 {
        return Err(RipgrepUnavailable::Defect(format!(
            "ripgrep download {filename} failed SHA-256 verification: expected {sha256}, got {actual}"
        )));
    }
    std::fs::write(&archive, &bytes).map_err(|_| RipgrepUnavailable::Failed)?;
    let extracted = extract_ripgrep(&bin, &archive, platform, &target).await;
    let _ = std::fs::remove_file(&archive);
    extracted.map(|()| target)
}

/// The release asset's bytes (redirects followed); `None` when the request fails or answers other than 2xx.
async fn download(url: &str) -> Option<Vec<u8>> {
    let mut url = url.to_owned();
    for _ in 0..20 {
        let response = crate::llm::http::send("GET", &url, &[], String::new()).await.ok()?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            url = response.headers.iter().find(|(k, _)| k == "location").map(|(_, v)| v.clone())?;
            continue;
        }
        if !(200..300).contains(&response.status) {
            return None;
        }
        let mut body = response.body;
        let mut out = Vec::new();
        while let Some(chunk) = body.next().await {
            out.extend_from_slice(&chunk.ok()?);
        }
        return Some(out);
    }
    None
}

/// Unpacks the archive in a temporary directory of the cache and installs its `rg` as `target`.
async fn extract_ripgrep(bin: &str, archive: &str, platform: &str, target: &str) -> Result<(), RipgrepUnavailable> {
    let suffix: String = crate::id::ascending().chars().rev().take(6).collect();
    let directory = format!("{bin}/ripgrep-{suffix}");
    std::fs::create_dir_all(&directory).map_err(|_| RipgrepUnavailable::Failed)?;
    let result = async {
        let output = tokio::process::Command::new("tar")
            .args(["-xzf", archive, "-C", &directory])
            .current_dir(&directory)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|_| RipgrepUnavailable::Failed)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            let code = output.status.code().unwrap_or(-1);
            let message = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                format!("ripgrep extraction failed with code {code}")
            };
            return Err(RipgrepUnavailable::Defect(message));
        }
        let extracted = format!("{directory}/ripgrep-{RIPGREP_VERSION}-{platform}/rg");
        if !Path::new(&extracted).is_file() {
            return Err(RipgrepUnavailable::Defect(format!("ripgrep archive did not contain executable: {extracted}")));
        }
        std::fs::copy(&extracted, target).map_err(|_| RipgrepUnavailable::Failed)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755)).map_err(|_| RipgrepUnavailable::Failed)
    }
    .await;
    let _ = std::fs::remove_dir_all(&directory);
    result
}

/// `Ripgrep.find` with pattern `*`: every file ripgrep lists (ignore rules apply, hidden files are skipped), up to
/// `limit`, and the directories containing them.
async fn scan(directory: &str, limit: usize) -> Result<Index, SearchError> {
    use tokio::io::AsyncBufReadExt;
    let binary = ripgrep().await.map_err(|e| SearchError(e.message("ripgrep is not installed")))?;
    // `Protected.names()` is empty on Linux, so the home directory excludes nothing.
    let mut child = tokio::process::Command::new(binary)
        .args(["--no-config", "--files", "--glob=!**/.git/**", "."])
        .current_dir(directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| SearchError(format!("ripgrep execution failed: {e}")))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let errors = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buffer = Vec::new();
        let _ = (&mut stderr).take(8 * 1024).read_to_end(&mut buffer).await;
        String::from_utf8_lossy(&buffer).into_owned()
    });
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut index = Index::default();
    let mut count = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).await.map_err(|e| SearchError(format!("ripgrep execution failed: {e}")))?;
        if read == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        if text.is_empty() {
            continue;
        }
        if count >= limit {
            // Truncated: the rest of the listing is not read.
            let _ = child.kill().await;
            return Ok(index);
        }
        count += 1;
        let relative = normalize_rg_path(text);
        let parts: Vec<&str> = relative.split('/').collect();
        for end in 1..parts.len() {
            index.directories.insert(format!("{}/", parts[..end].join("/")));
        }
        index.files.insert(relative);
    }
    let status = child.wait().await.map_err(|e| SearchError(format!("ripgrep execution failed: {e}")))?;
    let stderr = errors.await.unwrap_or_default();
    let code = status.code().unwrap_or(-1);
    if code != 0 && code != 1 && code != 2 {
        let message = crate::text::trim(&stderr);
        return Err(SearchError(if message.is_empty() { format!("ripgrep failed with code {code}") } else { message.to_owned() }));
    }
    Ok(if code == 1 { Index::default() } else { index })
}

/// `normalizePath` (ripgrep.ts): without leading `./` and `/`, with forward slashes.
fn normalize_rg_path(value: &str) -> String {
    let mut rest = value;
    while let Some(stripped) = rest.strip_prefix("./").or_else(|| rest.strip_prefix(".\\")) {
        rest = stripped;
    }
    rest.trim_start_matches(['/', '\\']).replace('\\', "/")
}

// --- file watches (filesystem/file-watch.ts) ---

/// A watch lasts this long unless renewed.
const WATCH_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// How often expired watches are stopped.
const WATCH_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const MAX_PER_LOCATION: usize = 256;

/// `FileWatch.DisabledError`.
#[derive(Debug)]
pub struct WatchDisabled;

impl WatchDisabled {
    pub const MESSAGE: &'static str = "File watching is disabled";
}

/// One watched file: when it expires, and its watch (the subscription and the task announcing its changes; dropping
/// it stops both).
struct Watched {
    expires: i64,
    generation: u64,
    _watch: Option<crate::watch::Watch>,
    _task: crate::watch::Watch,
}

#[derive(Default)]
struct FileWatches {
    /// Location key → watched file → its watch. Global rather than location-scoped, so watches outlive idle eviction.
    locations: HashMap<String, HashMap<String, Watched>>,
    next: u64,
    sweeping: bool,
}

fn watches() -> &'static Mutex<FileWatches> {
    static WATCHES: std::sync::OnceLock<Mutex<FileWatches>> = std::sync::OnceLock::new();
    WATCHES.get_or_init(Default::default)
}

/// The server's file watcher is off (`OPENCODE_FILEWATCHER_DISABLE`, `OPENCODE_DISABLE_FILEWATCHER`).
pub fn watcher_disabled() -> bool {
    !crate::watch::enabled()
}

fn watch_key(location: &crate::bus::LocationRef) -> String {
    crate::json::text(&Json::Array(vec![
        Json::String(location.directory.clone()),
        location.workspace_id.clone().map(Json::String).unwrap_or(Json::Null),
    ]))
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Forgets a watched file; the caller drops what it returns (which stops the watch) after releasing the lock.
fn forget(all: &mut FileWatches, key: &str, file: &str) -> Option<Watched> {
    let files = all.locations.get_mut(key)?;
    let removed = files.remove(file);
    if files.is_empty() {
        all.locations.remove(key);
    }
    removed
}

/// Starts the watch of one file: each change announces `filesystem.changed` on the watch's location, `change` while
/// the file exists and `unlink` once it is gone. A watch that ends (its directory vanished) is forgotten, so the next
/// `watch` starts a new one.
fn start_watch(app: &crate::app::App, reference: &crate::bus::LocationRef, file: &str, generation: u64, expires: i64) -> Watched {
    let (send, mut changes) = tokio::sync::mpsc::unbounded_channel::<String>();
    let watch = crate::watch::file(file, &send);
    drop(send);
    let (app, reference, file) = (app.arc(), reference.clone(), file.to_owned());
    let task = tokio::spawn(async move {
        while changes.recv().await.is_some() {
            let exists = Path::new(&file).exists();
            let mut data = Obj::new();
            data.str("file", &file).str("event", if exists { "change" } else { "unlink" });
            let options = crate::bus::PublishOptions { id: None, location: Some(reference.clone()) };
            app.bus.emit("filesystem.changed", data.done(), options);
            crate::vcs::changed(&app, &reference, &file);
        }
        let key = watch_key(&reference);
        let mut all = watches().lock().unwrap();
        let current = all.locations.get(&key).and_then(|files| files.get(&file)).is_some_and(|w| w.generation == generation);
        let ended = if current { forget(&mut all, &key, &file) } else { None };
        drop(all);
        // Dropping it aborts this task, which has nothing left to await.
        drop(ended);
    });
    Watched { expires, generation, _watch: watch, _task: crate::watch::Watch::task(task) }
}

/// Every five minutes, expired watches stop.
fn sweep_watches() {
    tokio::spawn(async {
        loop {
            tokio::time::sleep(WATCH_SWEEP_INTERVAL).await;
            let now = now_ms();
            let expired: Vec<Watched> = {
                let mut all = watches().lock().unwrap();
                let mut expired = vec![];
                for files in all.locations.values_mut() {
                    let stale: Vec<String> = files.iter().filter(|(_, w)| w.expires <= now).map(|(f, _)| f.clone()).collect();
                    expired.extend(stale.iter().filter_map(|file| files.remove(file)));
                }
                all.locations.retain(|_, files| !files.is_empty());
                expired
            };
            drop(expired);
        }
    });
}

/// `FileWatch.watch`: starts watching a file, or renews its expiry; a location watches at most 256 files (the one
/// expiring soonest makes room).
pub fn watch(app: &crate::app::App, location: &Location, file: &str) -> Result<(), WatchDisabled> {
    if watcher_disabled() {
        return Err(WatchDisabled);
    }
    let reference = location.reference();
    let key = watch_key(&reference);
    let expires = now_ms() + WATCH_TTL_MS;
    let mut all = watches().lock().unwrap();
    if !all.sweeping {
        all.sweeping = true;
        sweep_watches();
    }
    if let Some(existing) = all.locations.get_mut(&key).and_then(|files| files.get_mut(file)) {
        existing.expires = expires;
        return Ok(());
    }
    let mut evicted = None;
    if let Some(files) = all.locations.get_mut(&key).filter(|files| files.len() >= MAX_PER_LOCATION) {
        if let Some(soonest) = files.iter().min_by_key(|(_, w)| w.expires).map(|(f, _)| f.clone()) {
            evicted = files.remove(&soonest);
        }
    }
    all.next += 1;
    let watched = start_watch(app, &reference, file, all.next, expires);
    all.locations.entry(key).or_default().insert(file.to_owned(), watched);
    drop(all);
    drop(evicted);
    Ok(())
}

/// `FileWatch.unwatch`.
pub fn unwatch(location: &Location, file: &str) {
    if watcher_disabled() {
        return;
    }
    let removed = forget(&mut watches().lock().unwrap(), &watch_key(&location.reference()), file);
    drop(removed);
}

// --- Node-style errors and HTTP dates ---

/// The message Node gives a failed file system call: `ENOTDIR: not a directory, mkdir '<path>'`.
pub fn node_error(error: &std::io::Error, syscall: &str, path: &str) -> String {
    match node_code(error) {
        Some((code, description)) => format!("{code}: {description}, {syscall} '{path}'"),
        None => format!("{error}, {syscall} '{path}'"),
    }
}

/// The message of a failed call Node reports without a path (`write` on an open file): `ENOSPC: no space left on
/// device, write`.
pub fn node_call_error(error: &std::io::Error, syscall: &str) -> String {
    match node_code(error) {
        Some((code, description)) => format!("{code}: {description}, {syscall}"),
        None => format!("{error}, {syscall}"),
    }
}

/// libuv's code and description of an OS error.
fn node_code(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    Some(match error.raw_os_error()? {
        1 => ("EPERM", "operation not permitted"),
        2 => ("ENOENT", "no such file or directory"),
        5 => ("EIO", "i/o error"),
        13 => ("EACCES", "permission denied"),
        16 => ("EBUSY", "resource busy or locked"),
        17 => ("EEXIST", "file already exists"),
        18 => ("EXDEV", "cross-device link not permitted"),
        20 => ("ENOTDIR", "not a directory"),
        21 => ("EISDIR", "illegal operation on a directory"),
        22 => ("EINVAL", "invalid argument"),
        24 => ("EMFILE", "too many open files"),
        26 => ("ETXTBSY", "text file is busy"),
        27 => ("EFBIG", "file too large"),
        28 => ("ENOSPC", "no space left on device"),
        30 => ("EROFS", "read-only file system"),
        36 => ("ENAMETOOLONG", "name too long"),
        39 => ("ENOTEMPTY", "directory not empty"),
        40 => ("ELOOP", "too many symbolic links encountered"),
        122 => ("EDQUOT", "disk quota exceeded"),
        _ => return None,
    })
}

/// `fs.mkdir(dir, { recursive: true })` as Bun runs it, failing with Node's message: a missing parent is created
/// first, an existing directory is fine, and the message names the directory whose creation failed (`mkdir
/// '/p/ro/a'` for `/p/ro/a/b/c` under a read-only `/p/ro`).
pub fn mkdir_recursive(dir: &str) -> Result<(), String> {
    let exists = |error: &std::io::Error| error.kind() == std::io::ErrorKind::AlreadyExists && Path::new(dir).is_dir();
    match std::fs::create_dir(dir) {
        Ok(()) => Ok(()),
        Err(error) if exists(&error) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match Path::new(dir).parent().and_then(Path::to_str) {
                Some(parent) if !parent.is_empty() && parent != dir => mkdir_recursive(parent)?,
                _ => return Err(node_error(&error, "mkdir", dir)),
            }
            match std::fs::create_dir(dir) {
                Err(error) if !exists(&error) => Err(node_error(&error, "mkdir", dir)),
                _ => Ok(()),
            }
        }
        Err(error) => Err(node_error(&error, "mkdir", dir)),
    }
}

/// `fs.writeFile(path, bytes)`, failing with Node's message (`open '<path>'`, or `write` once it is open).
pub fn write_file(path: &str, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::File::create(path).map_err(|error| node_error(&error, "open", path))?;
    file.write_all(bytes).map_err(|error| node_call_error(&error, "write"))
}

/// `fs.rm(path, { recursive: true, force: true })`, failing with Node's message (`rm '<path>'`): a missing path is
/// fine, a directory goes with its contents.
pub fn rm_force(path: &str) -> Result<(), String> {
    let missing = |error: &std::io::Error| error.kind() == std::io::ErrorKind::NotFound;
    let removed = match std::fs::symlink_metadata(path) {
        Err(error) if missing(&error) => return Ok(()),
        Err(error) => Err(error),
        Ok(info) if info.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    };
    match removed {
        Err(error) if !missing(&error) => Err(node_error(&error, "rm", path)),
        _ => Ok(()),
    }
}

const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if month <= 2 { 1 } else { 0 }, month, day)
}

/// `new Date(ms).toUTCString()`.
pub fn utc_string(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let seconds = ms.div_euclid(1000).rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{}, {day:02} {} {year:04} {:02}:{:02}:{:02} GMT",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(month - 1) as usize],
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// `new Date(ms).toISOString()`.
pub fn utc_iso(ms: i64) -> String {
    let (year, month, day) = civil_from_days(ms.div_euclid(86_400_000));
    let within = ms.rem_euclid(86_400_000);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        within / 3_600_000,
        within / 60_000 % 60,
        within / 1000 % 60,
        within % 1000
    )
}

/// `Date.parse(input)`: JavaScriptCore's whole lenient grammar (ISO 8601, then the legacy forms HTTP clients and
/// people write), through the Code Mode interpreter's port of it (`opencode_codemode::jsdate`, checked against Bun
/// by its `harness/jsdate`). A date without a zone is local time, as in TS. `None` is `NaN`.
pub fn parse_date(input: &str) -> Option<i64> {
    let utf16: Vec<u16> = input.encode_utf16().collect();
    let ms = opencode_codemode::jsdate::parse(&utf16);
    (!ms.is_nan()).then_some(ms as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(utc_string(1_767_225_600_000), "Thu, 01 Jan 2026 00:00:00 GMT");
        assert_eq!(parse_date("Thu, 01 Jan 2026 00:00:00 GMT"), Some(1_767_225_600_000));
        assert_eq!(parse_date("Fri, 02 Jan 2026 00:00:00 GMT"), Some(1_767_312_000_000));
        assert_eq!(parse_date("not a date"), None);
        assert_eq!(parse_date("2026-01-01T00:00:00Z"), Some(1_767_225_600_000));
        // The legacy grammar: asctime, RFC 850, a bare month name with a zone.
        assert_eq!(parse_date("Thu Jan  1 00:00:00 UTC 2026"), Some(1_767_225_600_000));
        assert_eq!(parse_date("Thursday, 01-Jan-26 00:00:00 GMT"), Some(1_767_225_600_000));
        assert_eq!(parse_date("Jan 1 2026 01:00 +0100"), Some(1_767_225_600_000));
    }

    #[test]
    fn mime() {
        assert_eq!(mime_type("/a/b.SVG"), "image/svg+xml");
        assert_eq!(mime_type("/a/README.md"), "text/markdown");
        assert_eq!(mime_type("/a/.env"), "application/octet-stream");
        assert_eq!(mime_type("/a/Makefile"), "application/octet-stream");
    }
}
