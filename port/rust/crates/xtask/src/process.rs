//! Small, dependency-free process and filesystem helpers shared by the xtask commands.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error {
    message: String,
    exit_code: i32,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Error {
            message: message.into(),
            exit_code: 1,
        }
    }

    /// A command-line usage error, reported like the shell scripts' `exit 2`.
    pub fn usage(message: impl Into<String>) -> Self {
        Error {
            message: message.into(),
            exit_code: 2,
        }
    }

    /// An exit without an extra `xtask:` line, for commands that already printed their diagnosis.
    pub fn silent(exit_code: i32) -> Self {
        Error {
            message: String::new(),
            exit_code,
        }
    }

    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::new(error.to_string())
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Error::new(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Error::new(message.to_owned())
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Error::new(error.to_string())
    }
}

pub fn bail<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::new(message))
}

#[derive(Clone, Debug)]
pub struct Ctx {
    pub root: PathBuf,
}

impl Ctx {
    /// The repository root, so every command works from any directory (including `port/rust`).
    pub fn new() -> Result<Self> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|error| Error::new(format!("git rev-parse --show-toplevel: {error}")))?;
        if !output.status.success() {
            return bail("not inside a git checkout");
        }
        let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if root.is_empty() {
            return bail("git rev-parse --show-toplevel returned an empty path");
        }
        Ok(Ctx {
            root: PathBuf::from(root),
        })
    }

    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    pub fn git(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command.current_dir(&self.root).args(args);
        command
    }

    pub fn git_output(&self, args: &[&str]) -> Result<String> {
        let output = self.git(args).output()?;
        if !output.status.success() {
            return bail(format!("git {} failed ({})", args.join(" "), output.status));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    pub fn git_status(&self, args: &[&str]) -> Result<ExitStatus> {
        Ok(self.git(args).status()?)
    }
}

/// Runs a command with inherited stdio, failing if it cannot start or exits non-zero.
pub fn check(mut command: Command, what: &str) -> Result<()> {
    let status = command
        .status()
        .map_err(|error| Error::new(format!("{what}: {error}")))?;
    if !status.success() {
        return bail(format!("{what} failed ({status})"));
    }
    Ok(())
}

/// Runs a command with inherited stdio and returns its status.
pub fn status(mut command: Command) -> Result<ExitStatus> {
    command.status().map_err(Into::into)
}

/// Runs a command with inherited stdio and a wall-clock limit. Returns 124 when the limit is reached, as GNU
/// `timeout` does, so callers keep the same exit-code semantics on macOS and Linux.
pub fn timeout_status(mut command: Command, seconds: u64) -> Result<ExitStatus> {
    use std::os::unix::process::ExitStatusExt;
    let mut child = command.spawn().map_err(Error::from)?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= Duration::from_secs(seconds) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(ExitStatus::from_raw(124 << 8));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Captures and trims stdout; a failing command is an error.
pub fn capture_trim(mut command: Command, what: &str) -> Result<String> {
    let output = command
        .output()
        .map_err(|error| Error::new(format!("{what}: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return bail(format!(
            "{what} failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn read(path: impl AsRef<Path>) -> Result<String> {
    Ok(fs::read_to_string(path)?)
}

pub fn write(path: impl AsRef<Path>, text: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)?;
    Ok(())
}

pub fn copy_if_different(from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<bool> {
    let from = from.as_ref();
    let to = to.as_ref();
    let source = fs::read(from)?;
    if let Ok(existing) = fs::read(to)
        && existing == source
    {
        return Ok(false);
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(to, source)?;
    Ok(true)
}

pub fn remove_path(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Replaces a symlink (or file) with a new symlink. A real directory is an error, as for `ln -sfn` on a
/// non-link path in the scripts this replaces.
pub fn symlink_force(target: impl AsRef<Path>, link: impl AsRef<Path>) -> Result<()> {
    let link = link.as_ref();
    if let Ok(meta) = fs::symlink_metadata(link) {
        if meta.file_type().is_dir() && !meta.file_type().is_symlink() {
            return bail(format!("{} exists and is not a link", link.display()));
        }
        fs::remove_file(link)?;
    }
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(target, link).map_err(Into::into)
}

pub fn canonical(path: impl AsRef<Path>) -> Result<PathBuf> {
    Ok(fs::canonicalize(path)?)
}

/// A relative path from one absolute directory to another, independent of GNU `realpath --relative-to`.
pub fn relative(from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<PathBuf> {
    let from = canonical(from)?;
    let to = canonical(to)?;
    let from_parts: Vec<Component<'_>> = from.components().collect();
    let to_parts: Vec<Component<'_>> = to.components().collect();
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..from_parts.len() {
        relative.push("..");
    }
    for part in &to_parts[common..] {
        relative.push(part.as_os_str());
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Ok(relative)
}

pub fn sha256_file(path: impl AsRef<Path>) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn download(url: &str, destination: impl AsRef<Path>) -> Result<()> {
    let destination = destination.as_ref();
    let part = OsString::from(format!("{}.part", destination.display()));
    let part = PathBuf::from(part);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let status = Command::new("curl")
        .args(["-sSfL", "--retry", "3", "-o"])
        .arg(&part)
        .arg(url)
        .status()
        .map_err(|error| Error::new(format!("curl: {error}")))?;
    if !status.success() {
        let _ = fs::remove_file(&part);
        return bail(format!("download failed ({status}): {url}"));
    }
    fs::rename(&part, destination)?;
    Ok(())
}

pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    Command::new(program)
}

/// A temporary directory removed on drop.
pub struct TempDir {
    path: PathBuf,
    keep: bool,
}

impl TempDir {
    pub fn new(prefix: &str) -> Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir();
        for _ in 0..100 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("{prefix}-{}-{n}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(TempDir { path, keep: false }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        bail(format!(
            "could not create a temporary directory for {prefix}"
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn keep(mut self) -> PathBuf {
        self.keep = true;
        self.path.clone()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_do_not_need_gnu_realpath() {
        let temporary = TempDir::new("xtask-relative-test").unwrap();
        let from = temporary.path().join("a/b");
        let to = temporary.path().join("a/c/d");
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&to).unwrap();
        assert_eq!(relative(&from, &to).unwrap(), PathBuf::from("../c/d"));
        assert_eq!(relative(&to, &to).unwrap(), PathBuf::from("."));
    }
}
