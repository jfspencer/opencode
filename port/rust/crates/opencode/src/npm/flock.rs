//! `EffectFlock.acquire(key)` (packages/util/src/effect-flock.ts), the cross-process lock `Npm.add` holds while it
//! installs into a package's cache directory, so a Rust and a TS server sharing the cache never install the same
//! package at once: an atomic `mkdir` of `<XDG_STATE_HOME>/opencode/locks/<sha1(key)>.lock`, its `heartbeat` touched
//! every 20 s and its `meta.json` (`{"token","pid","hostname","createdAt"}`) written exclusively; a lock whose
//! heartbeat (else meta, else directory) is older than 60 s is stale and broken under a `.breaker` directory; a
//! held lock is retried with jittered backoff (100 ms × 1.7ⁿ, at most 2 s) for 5 minutes. Released by removing the
//! directory after checking the token.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use sha1::Digest;

const STALE: Duration = Duration::from_secs(60);
const TIMEOUT: Duration = Duration::from_secs(300);
const BASE_DELAY_MS: f64 = 100.0;
const MAX_DELAY_MS: f64 = 2_000.0;

fn state_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let state = std::env::var("XDG_STATE_HOME").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| format!("{home}/.local/state"));
    PathBuf::from(crate::project::normalize(&format!("{state}/opencode")))
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

fn older_than_stale(time: SystemTime) -> bool {
    SystemTime::now().duration_since(time).is_ok_and(|age| age > STALE)
}

/// `isStale`: the heartbeat's age, else the meta file's, else the directory's.
fn is_stale(dir: &Path) -> bool {
    for path in [dir.join("heartbeat"), dir.join("meta.json"), dir.to_path_buf()] {
        if let Some(time) = mtime(&path) {
            return older_than_stale(time);
        }
    }
    false
}

fn mkdir(path: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

fn write_new(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(content.as_bytes())
}

/// A held lock; dropping it releases it.
pub struct Lock {
    dir: PathBuf,
    token: String,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

enum Attempt {
    Acquired(Lock),
    Held,
}

fn try_acquire(dir: &Path) -> Result<Attempt, String> {
    let token = super::uuid();
    let created = mkdir(dir).map_err(|e| e.to_string())?;
    if !created {
        if !is_stale(dir) {
            return Ok(Attempt::Held);
        }
        let breaker = PathBuf::from(format!("{}.breaker", dir.display()));
        let claimed = match mkdir(&breaker) {
            Ok(true) => true,
            Ok(false) => {
                if mtime(&breaker).is_some_and(older_than_stale) {
                    let _ = std::fs::remove_dir_all(&breaker);
                }
                false
            }
            Err(_) => false,
        };
        if !claimed {
            return Ok(Attempt::Held);
        }
        let recreated = (|| {
            if !is_stale(dir) {
                return Ok(false);
            }
            let _ = std::fs::remove_dir_all(dir);
            mkdir(dir)
        })();
        let _ = std::fs::remove_dir_all(&breaker);
        if !recreated.map_err(|e| e.to_string())? {
            return Ok(Attempt::Held);
        }
    }
    let compromised = |detail: &str| {
        let _ = std::fs::remove_dir_all(dir);
        detail.to_owned()
    };
    write_new(&dir.join("heartbeat"), "").map_err(|_| compromised("heartbeat already existed"))?;
    let meta = crate::json::Obj::new()
        .str("token", &token)
        .set("pid", opencode_protocol::Json::Number(f64::from(std::process::id())))
        .str("hostname", &hostname())
        .str("createdAt", &crate::fs::utc_iso(crate::id::now()))
        .done();
    write_new(&dir.join("meta.json"), &crate::json::text(&meta)).map_err(|_| compromised("meta.json already existed"))?;
    Ok(Attempt::Acquired(Lock { dir: dir.to_path_buf(), token, heartbeat: None }))
}

fn hostname() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: gethostname writes at most `len` bytes into the buffer.
    let ok = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0;
    if !ok {
        return String::new();
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// `acquire(key)`: waits for the lock (for up to 5 minutes).
pub async fn acquire(key: &str) -> Result<Lock, String> {
    let root = state_dir().join("locks");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let digest: String = sha1::Sha1::digest(key.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let dir = root.join(format!("{digest}.lock"));
    let started = Instant::now();
    let mut attempt = 0i32;
    loop {
        if let Attempt::Acquired(mut lock) = try_acquire(&dir)? {
            let heartbeat = dir.join("heartbeat");
            lock.heartbeat = Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(20_000)).await;
                    let now = std::fs::FileTimes::new().set_accessed(SystemTime::now()).set_modified(SystemTime::now());
                    if let Ok(file) = std::fs::File::options().write(true).open(&heartbeat) {
                        let _ = file.set_times(now);
                    }
                }
            }));
            return Ok(lock);
        }
        let delay = (BASE_DELAY_MS * 1.7f64.powi(attempt)).min(MAX_DELAY_MS);
        let mut jitter = [0u8; 1];
        let _ = getrandom::fill(&mut jitter);
        let delay = delay * (0.8 + 0.4 * f64::from(jitter[0]) / 255.0);
        if started.elapsed() + Duration::from_millis(delay as u64) >= TIMEOUT {
            return Err(format!("LockTimeoutError: {key}"));
        }
        tokio::time::sleep(Duration::from_millis(delay as u64)).await;
        attempt += 1;
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        let meta = std::fs::read_to_string(self.dir.join("meta.json")).ok().and_then(|t| crate::json::parse(&t));
        if meta.as_ref().and_then(|m| crate::json::get_str(m, "token")) == Some(self.token.as_str()) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}
