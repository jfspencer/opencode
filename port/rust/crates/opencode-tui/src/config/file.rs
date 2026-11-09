//! The CLI config file (`Config.Service` of packages/cli/src/config/config.ts), which `opencode` hands the TUI as
//! `config` (`get`/`update`, packages/cli/src/commands/handlers/default.ts): `<global config>/cli.json`, merged
//! with `OPENCODE_CLI_CONFIG_CONTENT`, read and written under the file's lock (`Flock`, packages/util/src/flock.ts).
//! Updates edit the file's text in place, as jsonc-parser's `modify` does (comments and layout kept), and write it
//! atomically (`cli.json.tmp`, mode 0600, renamed).
//!
//! Blocking file I/O: call it off the event loop (`spawn_blocking`).
//!
//! Divergences: the legacy migrations `get`/`update` run first (`ConfigMigration.run`: `tui.json`/`kv.json` into
//! `cli.json`, `terminal.copy_on_select`, V1 keybind names) are not ported (legacy v1 config is dropped). The lock's
//! heartbeat (an mtime touch every 20 s while held) is not run: the config's critical sections are short. `update`
//! compares the draft with the current config by value where TS compares by identity (immer): the same edits for
//! every update the TUI makes (objects replaced by spreads of themselves with changed primitives).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

use super::jsonc::{self, FormattingOptions, Segment};
use super::schema::decode_info;

/// `SchemaURL`.
pub const SCHEMA_URL: &str = "https://opencode.ai/v2/cli.json";

/// The CLI config service.
#[derive(Clone, Debug)]
pub struct CliConfig {
    /// `<global config>/cli.json`.
    pub path: PathBuf,
    /// `<global state>/locks`.
    locks: PathBuf,
    /// `OPENCODE_CLI_CONFIG_CONTENT`, decoded.
    content: Option<Json>,
}

#[derive(Debug)]
pub enum ConfigError {
    /// `Failed to update CLI config` (its cause).
    Update(String),
    /// What TS throws as a defect, which rejects with the error itself: jsonc-parser refusing an edit (a document
    /// whose root is not an object) or the lock failing.
    Defect(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Update(_) => f.write_str("Failed to update CLI config"),
            ConfigError::Defect(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ConfigError {}

/// `parseRecord(text)`: the JSONC text's object, if it parses without error and is an object.
pub fn parse_record(text: &str) -> Option<Json> {
    let (value, errors) = jsonc::parse(text);
    if errors {
        return None;
    }
    value.filter(|v| matches!(v, Json::Object(_)))
}

/// `mergeRecords(base, overlay)`.
fn merge_records(base: &Object, overlay: &Object) -> Object {
    let mut result = base.clone();
    for (key, value) in crate::theme::js::entries(overlay) {
        let merged = match (result.get(key), value) {
            (Some(Json::Object(a)), Json::Object(b)) => Json::Object(merge_records(a, b)),
            _ => value.clone(),
        };
        result.insert(key.to_owned(), merged);
    }
    result
}

/// `normalizeLegacyTabs(info)`: `tabs.enabled` becomes `tabs.mode`.
fn normalize_legacy_tabs(info: &Json) -> Json {
    let Json::Object(object) = info else { return info.clone() };
    let Some(Json::Object(tabs)) = object.get("tabs") else { return info.clone() };
    let Some(enabled) = tabs.get("enabled") else { return info.clone() };
    let mut tabs = tabs.clone();
    if matches!(tabs.get("mode"), None | Some(Json::Null)) {
        let on = matches!(enabled, Json::Bool(true));
        tabs.insert("mode".into(), Json::String(if on { "on" } else { "off" }.into()));
    }
    tabs.shift_remove("enabled");
    let mut out = object.clone();
    out.insert("tabs".into(), Json::Object(tabs));
    Json::Object(out)
}

/// `merge(...values)`: the values deep-merged, then decoded (`{}` when that fails).
fn merge(values: &[Option<&Json>]) -> Json {
    let mut result = Object::new();
    for value in values.iter().flatten() {
        let normalized = normalize_legacy_tabs(value);
        if let Json::Object(overlay) = &normalized {
            result = merge_records(&result, overlay);
        }
    }
    decode_info(&Json::Object(result)).unwrap_or_else(|| Json::Object(Object::new()))
}

/// An edit of `changes(before, after)`: a path, and the new value (`None`: removed).
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub path: Vec<String>,
    pub value: Option<Json>,
}

/// `Object.is` for JSON values (deep, since the port has no object identity; see the module doc).
fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => (x.is_nan() && y.is_nan()) || (x == y && x.is_sign_negative() == y.is_sign_negative()),
        (Json::Array(x), Json::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
        (Json::Object(x), Json::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

/// `changes(before, after)`: the leaf edits that turn one config into the other.
pub fn changes(before: &Json, after: &Json, path: &[String]) -> Vec<Change> {
    if same(before, after) {
        return Vec::new();
    }
    if let (Json::Object(b), Json::Object(a)) = (before, after) {
        let mut keys: Vec<&str> = crate::theme::js::keys(b);
        for key in crate::theme::js::keys(a) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        let mut out = Vec::new();
        for key in keys {
            let mut next = path.to_vec();
            next.push(key.to_owned());
            match (b.get(key), a.get(key)) {
                (_, None) => out.push(Change { path: next, value: None }),
                (None, Some(value)) => out.push(Change { path: next, value: Some(value.clone()) }),
                (Some(x), Some(y)) => out.extend(changes(x, y, &next)),
            }
        }
        return out;
    }
    vec![Change { path: path.to_vec(), value: Some(after.clone()) }]
}

impl CliConfig {
    /// The service for a global config directory and state directory (`Global.config`, `Global.state`), with
    /// `OPENCODE_CLI_CONFIG_CONTENT` from the environment.
    pub fn new(config_dir: &Path, state_dir: &Path) -> CliConfig {
        let content = std::env::var("OPENCODE_CLI_CONFIG_CONTENT")
            .ok()
            .filter(|c| !c.is_empty())
            .and_then(|c| parse_record(&c))
            .and_then(|record| decode_info(&record));
        CliConfig::with_content(config_dir, state_dir, content)
    }

    pub fn with_content(config_dir: &Path, state_dir: &Path, content: Option<Json>) -> CliConfig {
        CliConfig { path: config_dir.join("cli.json"), locks: state_dir.join("locks"), content }
    }

    fn read_json(&self) -> Option<Json> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        parse_record(&text)
    }

    fn load(&self) -> Json {
        let file = self.read_json().and_then(|record| decode_info(&record));
        merge(&[file.as_ref(), self.content.as_ref()])
    }

    /// `get()`: the decoded config (the file merged with `OPENCODE_CLI_CONFIG_CONTENT`).
    pub fn get(&self) -> Json {
        match flock::acquire(&self.path.to_string_lossy(), &self.locks) {
            Ok(lock) => {
                let config = self.load();
                let _ = lock.release();
                config
            }
            // TS: `Effect.promise` of the lock rejects, a defect.
            Err(_) => self.load(),
        }
    }

    /// `update(update)`: applies the mutation to the current config, writes the changed properties into the file's
    /// text and returns the new config.
    pub fn update(&self, update: impl FnOnce(&mut Json)) -> Result<Json, ConfigError> {
        let lock = flock::acquire(&self.path.to_string_lossy(), &self.locks).map_err(ConfigError::Defect)?;
        let result = self.update_locked(update);
        let released = lock.release();
        let config = result?;
        released.map_err(ConfigError::Defect)?;
        Ok(config)
    }

    fn update_locked(&self, update: impl FnOnce(&mut Json)) -> Result<Json, ConfigError> {
        let current = self.load();
        let mut next = current.clone();
        update(&mut next);
        let edits = changes(&current, &next, &[]);
        if edits.is_empty() {
            return Ok(current);
        }
        let text = std::fs::read_to_string(&self.path).unwrap_or_else(|_| {
            let mut object = Object::new();
            object.insert("$schema".into(), Json::String(SCHEMA_URL.into()));
            stringify_pretty(&Json::Object(object))
        });
        let options = FormattingOptions { tab_size: 2, insert_spaces: true };
        let mut updated = text;
        for edit in edits {
            let path: Vec<Segment> = edit.path.into_iter().map(Segment::Key).collect();
            let edits = jsonc::modify(&updated, &path, edit.value.as_ref(), options).map_err(ConfigError::Defect)?;
            updated = jsonc::apply_edits(&updated, &edits).map_err(ConfigError::Defect)?;
        }
        let (value, errors) = jsonc::parse(&updated);
        let config = if errors { None } else { value.and_then(|v| decode_info(&v)) };
        let Some(config) = config else { return Err(ConfigError::Update("Invalid CLI config update".into())) };
        let text = if updated.ends_with('\n') { updated } else { updated + "\n" };
        self.write(&text).map_err(|e| ConfigError::Update(e.to_string()))?;
        Ok(merge(&[Some(&config), self.content.as_ref()]))
    }

    fn write(&self, text: &str) -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let temp = PathBuf::from(format!("{}.tmp", self.path.to_string_lossy()));
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temp)?;
            file.write_all(text.as_bytes())?;
        }
        std::fs::rename(&temp, &self.path)
    }
}

/// `JSON.stringify(value, null, 2)`.
pub fn stringify_pretty(value: &Json) -> String {
    fn write(out: &mut String, value: &Json, depth: usize) {
        let pad = |n: usize| "  ".repeat(n);
        match value {
            Json::Array(items) if !items.is_empty() => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    write(out, item, depth + 1);
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            Json::Object(object) if !object.is_empty() => {
                out.push_str("{\n");
                let entries = crate::theme::js::entries(object);
                let n = entries.len();
                for (i, (key, item)) in entries.into_iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    out.push_str(&crate::theme::js::quote(key));
                    out.push_str(": ");
                    write(out, item, depth + 1);
                    if i + 1 < n {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
            other => out.push_str(&opencode_protocol::to_string(other)),
        }
    }
    let mut out = String::new();
    write(&mut out, value, 0);
    out
}

/// `Flock` (packages/util/src/flock.ts): a lock directory `<dir>/<sha1(key)>.lock` holding `meta.json` (owner token)
/// and `heartbeat`; contenders back off (100 ms growing by 1.7 up to 2 s, ±30% jitter) for up to 5 minutes, and a
/// lock whose heartbeat, metadata or directory is older than 60 s is broken (under a `.breaker` directory).
pub mod flock {
    use super::*;

    const STALE: Duration = Duration::from_secs(60);
    const TIMEOUT: Duration = Duration::from_secs(5 * 60);
    const BASE_DELAY_MS: u64 = 100;
    const MAX_DELAY_MS: u64 = 2_000;

    /// A held lock.
    pub struct Lease {
        dir: PathBuf,
        token: String,
    }

    fn random_bytes<const N: usize>() -> [u8; N] {
        let mut bytes = [0u8; N];
        getrandom::fill(&mut bytes).expect("random bytes");
        bytes
    }

    /// `crypto.randomUUID()`.
    fn random_uuid() -> String {
        let mut b = random_bytes::<16>();
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
        format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
    }

    fn sha1_hex(text: &str) -> String {
        use sha1::Digest;
        sha1::Sha1::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn hostname() -> String {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; gethostname NUL-terminates within it on success.
        let ok = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } == 0;
        if !ok {
            return String::new();
        }
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }

    /// `new Date().toISOString()`.
    fn iso_now() -> String {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
        let millis = now.as_millis() as i64;
        let days = millis.div_euclid(86_400_000);
        let ms_of_day = millis.rem_euclid(86_400_000);
        // Civil from days (Howard Hinnant's algorithm).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
            ms_of_day / 3_600_000,
            (ms_of_day / 60_000) % 60,
            (ms_of_day / 1000) % 60,
            ms_of_day % 1000
        )
    }

    fn age(path: &Path) -> Option<Duration> {
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;
        Some(SystemTime::now().duration_since(modified).unwrap_or_default())
    }

    fn stale(dir: &Path) -> bool {
        if let Some(age) = age(&dir.join("heartbeat")) {
            return age > STALE;
        }
        if let Some(age) = age(&dir.join("meta.json")) {
            return age > STALE;
        }
        age(dir).is_some_and(|a| a > STALE)
    }

    fn try_acquire(dir: &Path) -> Result<Option<Lease>, String> {
        use std::os::unix::fs::DirBuilderExt;
        let token = random_uuid();
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !stale(dir) {
                    return Ok(None);
                }
                let breaker = PathBuf::from(format!("{}.breaker", dir.to_string_lossy()));
                match std::fs::DirBuilder::new().mode(0o700).create(&breaker) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        if age(&breaker).is_some_and(|a| a > STALE) {
                            let _ = std::fs::remove_dir_all(&breaker);
                        }
                        return Ok(None);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error.to_string()),
                }
                let result = (|| {
                    if !stale(dir) {
                        return Ok(false);
                    }
                    let _ = std::fs::remove_dir_all(dir);
                    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
                        Ok(()) => Ok(true),
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                        Err(error) => Err(error.to_string()),
                    }
                })();
                let _ = std::fs::remove_dir_all(&breaker);
                if !result? {
                    return Ok(None);
                }
            }
            Err(error) => return Err(error.to_string()),
        }
        let create_new = |path: &Path, text: &str| -> std::io::Result<()> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
            file.write_all(text.as_bytes())
        };
        if create_new(&dir.join("heartbeat"), "").is_err() {
            let _ = std::fs::remove_dir_all(dir);
            return Err("Lock acquired but heartbeat already existed (possible compromise).".into());
        }
        let mut meta = Object::new();
        meta.insert("token".into(), Json::String(token.clone()));
        meta.insert("pid".into(), Json::Number(std::process::id() as f64));
        meta.insert("hostname".into(), Json::String(hostname()));
        meta.insert("createdAt".into(), Json::String(iso_now()));
        if create_new(&dir.join("meta.json"), &stringify_pretty(&Json::Object(meta))).is_err() {
            let _ = std::fs::remove_dir_all(dir);
            return Err("Lock acquired but meta.json already existed (possible compromise).".into());
        }
        Ok(Some(Lease { dir: dir.to_path_buf(), token }))
    }

    /// `Flock.acquire(key, { dir })`.
    pub fn acquire(key: &str, dir: &Path) -> Result<Lease, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let lock = dir.join(format!("{}.lock", sha1_hex(key)));
        let stop = Instant::now() + TIMEOUT;
        let mut delay = BASE_DELAY_MS;
        loop {
            if let Some(lease) = try_acquire(&lock)? {
                return Ok(lease);
            }
            if Instant::now() > stop {
                return Err(format!("Timed out waiting for lock: {key}"));
            }
            std::thread::sleep(Duration::from_millis(jitter(delay)));
            delay = MAX_DELAY_MS.min((delay as f64 * 1.7).floor() as u64);
        }
    }

    fn jitter(ms: u64) -> u64 {
        let j = (ms as f64 * 0.3).floor() as i64;
        let random = u32::from_le_bytes(random_bytes::<4>()) as f64 / 4_294_967_296.0;
        let d = (random * (2 * j + 1) as f64).floor() as i64 - j;
        (ms as i64 + d).max(0) as u64
    }

    impl Lease {
        /// `release()`: removes the lock if this lease still owns it.
        pub fn release(self) -> Result<(), String> {
            let meta = match std::fs::read_to_string(self.dir.join("meta.json")) {
                Ok(raw) => raw,
                Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound) => {
                    return Err("Refusing to release: lock is compromised (metadata missing).".into());
                }
                Err(error) => return Err(error.to_string()),
            };
            let parsed = opencode_protocol::wire::json::parse(&meta)
                .map_err(|_| "Refusing to release: lock is compromised (metadata invalid).".to_string())?;
            let token = match &parsed {
                Json::Object(o) => match o.get("token") {
                    Some(Json::String(t)) => Some(t.as_str()),
                    _ => None,
                },
                _ => None,
            };
            if token != Some(self.token.as_str()) {
                return Err("Refusing to release: lock token mismatch (not the owner).".into());
            }
            let _ = std::fs::remove_dir_all(&self.dir);
            Ok(())
        }
    }
}
