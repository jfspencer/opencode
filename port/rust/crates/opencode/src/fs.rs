//! A location's file system (packages/core/src/filesystem.ts, filesystem/search.ts, filesystem/file-watch.ts,
//! ripgrep.ts, ripgrep/binary.ts): reading files confined to the location, listing directories, writing files, fuzzy
//! file search (ripgrep's file list ranked by fuzzysort; the port omits fff) and file watches.
//!
//! TODO(port): the file watcher itself (watched files announce `filesystem.changed`); downloading ripgrep when none is
//! installed (`RipgrepBinary`: the port uses `rg` from `PATH` or `<XDG_CACHE_HOME>/opencode/bin`).

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

fn not_found_or_die(error: std::io::Error, path: &str) -> Error {
    if error.kind() == std::io::ErrorKind::NotFound { Error::NotFound(NotFound(path.to_owned())) } else { Error::Defect(Defect(error)) }
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

/// `RipgrepBinary.filepath`: `rg` from `PATH`, else the copy in opencode's cache.
pub fn ripgrep() -> Option<String> {
    let cache = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.cache", std::env::var("HOME").unwrap_or_default()));
    crate::process::which("rg", Some(&format!("{cache}/opencode/bin")))
}

/// `Ripgrep.find` with pattern `*`: every file ripgrep lists (ignore rules apply, hidden files are skipped), up to
/// `limit`, and the directories containing them.
async fn scan(directory: &str, limit: usize) -> Result<Index, SearchError> {
    use tokio::io::AsyncBufReadExt;
    let binary = ripgrep().ok_or_else(|| SearchError("ripgrep is not installed".into()))?;
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
const MAX_PER_LOCATION: usize = 256;

/// `FileWatch.DisabledError`.
#[derive(Debug)]
pub struct WatchDisabled;

impl WatchDisabled {
    pub const MESSAGE: &'static str = "File watching is disabled";
}

fn watches() -> &'static Mutex<HashMap<String, HashMap<String, i64>>> {
    static WATCHES: std::sync::OnceLock<Mutex<HashMap<String, HashMap<String, i64>>>> = std::sync::OnceLock::new();
    WATCHES.get_or_init(Default::default)
}

/// The server's file watcher is off (`OPENCODE_FILEWATCHER_DISABLE`, `OPENCODE_DISABLE_FILEWATCHER`).
pub fn watcher_disabled() -> bool {
    let value = std::env::var("OPENCODE_FILEWATCHER_DISABLE").or_else(|_| std::env::var("OPENCODE_DISABLE_FILEWATCHER")).ok();
    value.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn watch_key(location: &Location) -> String {
    crate::json::text(&Json::Array(vec![
        Json::String(location.directory.clone()),
        location.workspace_id.clone().map(Json::String).unwrap_or(Json::Null),
    ]))
}

/// `FileWatch.watch`: starts watching a file, or renews its expiry; a location watches at most 256 files (the one
/// expiring soonest makes room).
pub fn watch(location: &Location, file: &str) -> Result<(), WatchDisabled> {
    if watcher_disabled() {
        return Err(WatchDisabled);
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let mut all = watches().lock().unwrap();
    all.retain(|_, files| {
        files.retain(|_, expires| *expires > now);
        !files.is_empty()
    });
    let files = all.entry(watch_key(location)).or_default();
    if !files.contains_key(file) && files.len() >= MAX_PER_LOCATION {
        if let Some(soonest) = files.iter().min_by_key(|(_, expires)| **expires).map(|(f, _)| f.clone()) {
            files.remove(&soonest);
        }
    }
    files.insert(file.to_owned(), now + WATCH_TTL_MS);
    Ok(())
}

/// `FileWatch.unwatch`.
pub fn unwatch(location: &Location, file: &str) {
    if watcher_disabled() {
        return;
    }
    let mut all = watches().lock().unwrap();
    let key = watch_key(location);
    if let Some(files) = all.get_mut(&key) {
        files.remove(file);
        if files.is_empty() {
            all.remove(&key);
        }
    }
}

// --- Node-style errors and HTTP dates ---

/// The message Node gives a failed file system call: `ENOTDIR: not a directory, mkdir '<path>'`.
pub fn node_error(error: &std::io::Error, syscall: &str, path: &str) -> String {
    let (code, description) = match error.raw_os_error() {
        Some(1) => ("EPERM", "operation not permitted"),
        Some(2) => ("ENOENT", "no such file or directory"),
        Some(5) => ("EIO", "i/o error"),
        Some(13) => ("EACCES", "permission denied"),
        Some(16) => ("EBUSY", "resource busy or locked"),
        Some(17) => ("EEXIST", "file already exists"),
        Some(18) => ("EXDEV", "cross-device link not permitted"),
        Some(20) => ("ENOTDIR", "not a directory"),
        Some(21) => ("EISDIR", "illegal operation on a directory"),
        Some(22) => ("EINVAL", "invalid argument"),
        Some(24) => ("EMFILE", "too many open files"),
        Some(28) => ("ENOSPC", "no space left on device"),
        Some(30) => ("EROFS", "read-only file system"),
        Some(36) => ("ENAMETOOLONG", "name too long"),
        Some(39) => ("ENOTEMPTY", "directory not empty"),
        Some(40) => ("ELOOP", "too many symbolic links encountered"),
        Some(122) => ("EDQUOT", "disk quota exceeded"),
        _ => return format!("{error}, {syscall} '{path}'"),
    };
    format!("{code}: {description}, {syscall} '{path}'")
}

const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Days since the epoch of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

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

/// `Date.parse` for the forms HTTP clients send (IMF-fixdate, RFC 850, asctime) and ISO 8601; `None` is `NaN`.
/// TODO(port): the rest of JavaScriptCore's lenient date grammar.
pub fn parse_date(input: &str) -> Option<i64> {
    let text = crate::text::trim(input);
    if let Some(ms) = parse_iso(text) {
        return Some(ms);
    }
    let tokens: Vec<&str> = text.split(|c: char| c == ' ' || c == ',' || c == '-').filter(|t| !t.is_empty()).collect();
    let month_of = |t: &str| MONTHS.iter().position(|m| m.eq_ignore_ascii_case(t.get(..3).unwrap_or(""))).map(|i| i as i64 + 1);
    let mut day = None;
    let mut month = None;
    let mut year = None;
    let mut time = None;
    let mut offset = 0i64;
    const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    for token in tokens {
        let alphabetic = token.chars().all(|c| c.is_ascii_alphabetic());
        if token.contains(':') {
            let parts: Vec<i64> = token.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
            time = Some((parts[0], *parts.get(1)?, parts.get(2).copied().unwrap_or(0)));
        } else if let Ok(number) = token.parse::<i64>() {
            if day.is_none() && token.len() <= 2 {
                day = Some(number);
            } else if year.is_none() {
                year = Some(if token.len() <= 2 { if number < 50 { 2000 + number } else { 1900 + number } } else { number });
            } else {
                return None;
            }
        } else if let Some(m) = month_of(token).filter(|_| alphabetic && token.len() >= 3) {
            month = Some(m);
        } else if ["GMT", "UTC", "UT", "Z"].iter().any(|z| z.eq_ignore_ascii_case(token)) {
        } else if let Some((sign, digits)) = token.strip_prefix('+').map(|r| (1, r)).or_else(|| token.strip_prefix('-').map(|r| (-1, r))) {
            let digits: i64 = digits.parse().ok()?;
            offset = sign * ((digits / 100) * 60 + digits % 100);
        } else if !(alphabetic && token.len() >= 3 && WEEKDAYS.iter().any(|d| d.eq_ignore_ascii_case(&token[..3]))) {
            return None;
        }
    }
    let (hour, minute, second) = time.unwrap_or((0, 0, 0));
    let (day, month, year) = (day?, month?, year?);
    if !(1..=31).contains(&day) || hour > 24 || minute > 59 || second > 59 {
        return None;
    }
    Some((days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset * 60) * 1000)
}

fn parse_iso(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| text.get(range).and_then(|t| t.parse::<i64>().ok());
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let mut ms = days_from_civil(year, month, day) * 86_400_000;
    let rest = &text[10..];
    if rest.is_empty() {
        return Some(ms);
    }
    let rest = rest.strip_prefix('T')?;
    let (clock, zone) = match rest.find(['Z', '+', '-']) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let (hms, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let parts: Vec<i64> = hms.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    ms += parts.first()? * 3_600_000 + parts.get(1)? * 60_000 + parts.get(2).copied().unwrap_or(0) * 1000;
    if !fraction.is_empty() {
        ms += format!("{:0<3}", &fraction[..fraction.len().min(3)]).parse::<i64>().ok()?;
    }
    if let Some(offset) = zone.strip_prefix('+').map(|r| (-1, r)).or_else(|| zone.strip_prefix('-').map(|r| (1, r))) {
        let (h, m) = offset.1.split_once(':').unwrap_or((offset.1, "0"));
        ms += offset.0 * (h.parse::<i64>().ok()? * 3_600_000 + m.parse::<i64>().ok()? * 60_000);
    }
    Some(ms)
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
    }

    #[test]
    fn mime() {
        assert_eq!(mime_type("/a/b.SVG"), "image/svg+xml");
        assert_eq!(mime_type("/a/README.md"), "text/markdown");
        assert_eq!(mime_type("/a/.env"), "application/octet-stream");
        assert_eq!(mime_type("/a/Makefile"), "application/octet-stream");
    }
}
