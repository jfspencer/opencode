//! Snapshots of a location's files (packages/core/src/snapshot.ts), kept in a git store of their own and driven through
//! the git CLI exactly as packages/core/src/git.ts drives it (`Git.repo.discover/create`, `Git.index.ignored`,
//! `Git.tree.capture/files/diff/restore`, `Git.objects.pack`).
//!
//! A git project's location captures its working tree as a content-addressed tree (`capture`), lists or diffs the
//! paths two trees differ in (`files`, `diff`), and restores selected paths from trees (`restore`). The store lives in
//! `$XDG_DATA_HOME/opencode/snapshot/<project>/<sha1 of the worktree>`, borrows the source repository's objects
//! (alternates) and starts from a copy of its index; captures run against a temporary index renamed over the store's,
//! so a capture that finds nothing changed returns the tree it wrote last. Snapshots track every scoped file; the
//! source repository's ignore rules decide what callers see. Snapshots are on unless the config says `snapshots:
//! false`, and only in git projects.
//!
//! TODO(port): callers queued behind a running capture of the same store share the next one (`pendingCaptures`);
//! here each runs its own, under the same per-store lock, with the same result.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::location::Location;
use crate::vcs::{FileDiff, patch};

const CONFIG_FILE: &str = "opencode.gitconfig";
const CONFIG_INCLUDE: &str = "[include]\n\tpath = opencode.gitconfig\n";
const CONFIG: &str = "[core]\n\tautocrlf = false\n\tlongpaths = true\n\tsymlinks = true\n\tfsmonitor = false\n\tuntrackedCache = true\n\t# A split index cannot name its shared file once manyFiles skips index checksums.\n\tsplitIndex = false\n[feature]\n\tmanyFiles = true\n[index]\n\tversion = 4\n\tthreads = true\n";
const TEMPORARY_INDEX_PREFIX: &str = "index.opencode-";
/// Room for paths on one git command line (Linux).
const ARGUMENT_BUDGET: usize = 128 * 1024;
const LOOSE_OBJECT_LIMIT: usize = 2048;
const PACK_COUNT_LIMIT: usize = 16;
const MAXIMUM_UNTRACKED_FILE_BYTES: u64 = 2 * 1024 * 1024;
const PACK_INTERVAL: Duration = Duration::from_secs(10 * 60);
const HOUR_MS: i64 = 60 * 60 * 1000;

/// `Snapshot.Error`: what a snapshot operation could not do.
#[derive(Debug, Clone)]
pub struct Error {
    pub operation: &'static str,
    pub message: String,
}

impl Error {
    fn new(operation: &'static str, message: impl Into<String>) -> Self {
        Self { operation, message: message.into() }
    }
}

/// `failure(operation, cause)`: the message of the cause under this operation.
fn failure(operation: &'static str, cause: impl Into<String>) -> Error {
    Error::new(operation, cause)
}

/// `Git.OperationError`; `cause` is set when git could not be started at all.
#[derive(Debug, Clone)]
struct OperationError {
    message: String,
    cause: bool,
}

impl From<OperationError> for String {
    fn from(error: OperationError) -> String {
        error.message
    }
}

/// `Git.Repository`.
#[derive(Debug, Clone)]
struct Repository {
    worktree: String,
    git_directory: String,
    common_directory: String,
}

impl Repository {
    fn store(worktree: &str, git_directory: &str) -> Self {
        Self { worktree: worktree.into(), git_directory: git_directory.into(), common_directory: git_directory.into() }
    }

    fn index_file(&self) -> String {
        format!("{}/index", self.git_directory)
    }
}

/// The location's source repository and its snapshot store.
#[derive(Debug, Clone)]
struct Repo {
    source: Repository,
    worktree: String,
    store: Repository,
}

/// A location's snapshot service (`Snapshot.Service`): its store, found or created on first use and kept.
#[derive(Debug, Default)]
pub struct Service {
    repository: tokio::sync::OnceCell<Result<Repo, Error>>,
    last_pack_check: Mutex<Option<Instant>>,
}

/// The store state git.ts keeps for the whole process, by store directory.
#[derive(Default)]
struct Stores {
    locks: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// A clean capture returns the tree last written from the exact index it scanned.
    last_captures: HashMap<String, (String, String)>,
    prepared: HashSet<String>,
    /// Untracked files skipped for size, by path, with the stamp they had when skipped.
    oversized: HashMap<String, HashMap<String, String>>,
}

fn stores() -> std::sync::MutexGuard<'static, Stores> {
    static STORES: OnceLock<Mutex<Stores>> = OnceLock::new();
    STORES.get_or_init(Default::default).lock().unwrap()
}

fn store_lock(repository: &Repository) -> Arc<tokio::sync::Mutex<()>> {
    stores().locks.entry(repository.git_directory.clone()).or_default().clone()
}

fn enabled(location: &Location) -> bool {
    location.vcs.as_ref().is_some_and(|vcs| vcs.kind == "git")
        && location.inventory().latest(|config| config.snapshots.as_ref()).copied().unwrap_or(true)
}

async fn repository(location: &Location) -> Result<Repo, Error> {
    location.snapshot.repository.get_or_init(|| open(location)).await.clone()
}

/// The store of the location's project, created (seeded from the source repository) when missing.
async fn open(location: &Location) -> Result<Repo, Error> {
    let source = discover(&location.project.directory)
        .await
        .ok_or_else(|| Error::new("capture", "Project is not a Git repository"))?;
    let worktree = real_path(&source.worktree).map_err(|e| failure("capture", e))?;
    let git_directory = format!("{}/snapshot/{}/{}", data_directory(), location.project.id, sha1(&worktree));
    let store = if exists(&format!("{git_directory}/HEAD")).await {
        Repository::store(&worktree, &git_directory)
    } else {
        create(&worktree, &git_directory, &source).await.map_err(|e| failure("capture", e))?
    };
    Ok(Repo { source, worktree, store })
}

/// `Global.Path.data`.
fn data_directory() -> String {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    format!("{base}/opencode")
}

/// `Hash.fast`.
fn sha1(text: &str) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn real_path(path: &str) -> Result<String, String> {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).map_err(|e| e.to_string())
}

/// The location's directory, relative to the store's worktree (`.` for the worktree itself). The worktree is a
/// real path, so the location must be one too.
fn scope(location: &Location, worktree: &str) -> Result<String, Error> {
    let directory = real_path(&location.directory).map_err(|e| failure("capture", e))?;
    if !crate::project::contains(worktree, &directory) {
        return Err(Error::new("capture", "Location is outside the project"));
    }
    let relative = crate::session::relative(worktree, &directory);
    Ok(if relative.is_empty() { ".".into() } else { relative })
}

/// `Snapshot.capture`: the location's files as a tree, or `None` when snapshots are off or the capture failed.
pub async fn capture(location: &Location) -> Option<String> {
    if !enabled(location) {
        return None;
    }
    let captured = async {
        let repo = repository(location).await?;
        let scope = scope(location, &repo.worktree)?;
        let tree = capture_tree(&repo.store, &[scope], Some(&repo.source), Some(MAXIMUM_UNTRACKED_FILE_BYTES))
            .await
            .map_err(|e| failure("capture", e))?;
        pack_when_due(location, &repo.store);
        Ok::<_, Error>(tree)
    }
    .await;
    match captured {
        Ok(tree) => Some(tree),
        Err(error) => {
            eprintln!("failed to capture snapshot: {}", error.message);
            None
        }
    }
}

/// `packWhenDue`: at most every ten minutes, packing runs in the background (it takes a cross-process lock).
fn pack_when_due(location: &Location, store: &Repository) {
    {
        let mut last = location.snapshot.last_pack_check.lock().unwrap();
        if last.is_some_and(|at| at.elapsed() < PACK_INTERVAL) {
            return;
        }
        *last = Some(Instant::now());
    }
    let store = store.clone();
    tokio::spawn(async move {
        if let Err(error) = pack_objects(&store).await {
            eprintln!("failed to pack snapshot objects: {}", error.message);
        }
    });
}

/// `Snapshot.files`: the paths two trees differ in, without the ones the source repository ignores.
pub async fn files(location: &Location, from: &str, to: &str) -> Result<Vec<String>, Error> {
    let repo = repository(location).await.map_err(|e| failure("files", e.message))?;
    let output = operation(&repo.store, &["diff", "--name-only", "--no-renames", "-z", from, to], Options::default())
        .await
        .map_err(|e| failure("files", e))?;
    let changed = nuls(&output.text);
    if changed.is_empty() {
        return Ok(changed);
    }
    let skipped = ignored(&repo.source, &changed).await.map_err(|e| failure("files", e))?;
    Ok(changed.into_iter().filter(|file| !skipped.contains(file)).collect())
}

/// `Snapshot.diff`: per-file diffs between two trees (with `context` lines, 3 by default), of the selected `paths`
/// only when given, without the paths the source repository ignores.
pub async fn diff(
    location: &Location,
    from: &str,
    to: &str,
    context: Option<i64>,
    paths: Option<&[String]>,
) -> Result<Vec<FileDiff>, Error> {
    if paths.is_some_and(|p| p.is_empty()) {
        return Ok(vec![]);
    }
    let repo = repository(location).await.map_err(|e| failure("diff", e.message))?;
    let diffs = tree_diff(&repo.store, from, to, context, paths).await.map_err(|e| failure("diff", e))?;
    if diffs.is_empty() {
        return Ok(diffs);
    }
    let names: Vec<String> = diffs.iter().map(|d| d.file.clone()).collect();
    let skipped = ignored(&repo.source, &names).await.map_err(|e| failure("diff", e))?;
    Ok(diffs.into_iter().filter(|d| !skipped.contains(&d.file)).collect())
}

/// `Snapshot.restore`: each path from its tree (paths are relative to the project root); a path absent from its tree
/// is removed.
pub async fn restore(location: &Location, files: &IndexMap<String, String>) -> Result<(), Error> {
    if !enabled(location) {
        return Err(Error::new("restore", "Snapshots are disabled"));
    }
    let repo = repository(location).await.map_err(|e| failure("restore", e.message))?;
    for file in files.keys() {
        let joined = if file.starts_with('/') { file.clone() } else { format!("{}/{file}", repo.worktree) };
        let absolute = crate::project::normalize(&joined);
        if !crate::project::contains(&repo.worktree, &absolute) {
            return Err(Error::new("restore", format!("Path escapes the project: {file}")));
        }
    }
    restore_tree(&repo.store, files).await.map_err(|e| failure("restore", e))
}

/// The step's start snapshot, captured while the provider request is in flight (`pendingStartSnapshot`).
pub struct Pending {
    handle: Option<tokio::task::JoinHandle<Option<String>>>,
    value: Option<Option<String>>,
}

impl Pending {
    pub fn start(location: &Arc<Location>) -> Self {
        let location = location.clone();
        Self { handle: Some(tokio::spawn(async move { capture(&location).await })), value: None }
    }

    /// The captured tree, once it exists (cancel-safe).
    pub async fn join(&mut self) -> Option<String> {
        if let Some(value) = &self.value {
            return value.clone();
        }
        let value = match self.handle.as_mut() {
            Some(handle) => handle.await.ok().flatten(),
            None => None,
        };
        self.handle = None;
        self.value = Some(value.clone());
        value
    }

    pub fn finished(&self) -> bool {
        self.value.is_some() || self.handle.as_ref().is_some_and(|h| h.is_finished())
    }
}

/// What a settled step records about the files (`snapshot` and `files` of `session.step.ended` / `failed`).
pub struct Terminal {
    pub snapshot: Option<String>,
    pub files: Option<Vec<String>>,
}

impl Terminal {
    /// The end snapshot, and the paths changed since `start` (none when either is missing or listing failed).
    pub async fn capture(location: &Location, start: Option<&str>) -> Self {
        let snapshot = capture(location).await;
        let files = match (start, snapshot.as_deref()) {
            (Some(start), Some(end)) if start == end => Some(vec![]),
            (Some(start), Some(end)) => files(location, start, end).await.ok(),
            _ => None,
        };
        Self { snapshot, files }
    }

    pub fn write(&self, o: &mut crate::json::Obj) {
        o.opt_str("snapshot", self.snapshot.as_deref());
        o.opt("files", self.files.as_ref().map(|f| opencode_protocol::Json::Array(f.iter().map(|s| crate::json::string(s)).collect())));
    }
}

// --- git.ts ---

#[derive(Default)]
struct Options<'a> {
    index: Option<&'a str>,
    stdin: Option<Vec<u8>>,
    max_output: Option<usize>,
}

struct Run {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

struct Text {
    text: String,
    truncated: bool,
}

/// `repositoryRun`: git on the repository (`--git-dir`, `--work-tree`), from its worktree.
async fn run(repository: &Repository, args: &[&str], options: Options<'_>) -> Result<Run, OperationError> {
    let mut command = tokio::process::Command::new(crate::vcs::git_executable());
    command
        .args(["--git-dir", &repository.git_directory, "--work-tree", &repository.worktree])
        .args(args)
        .current_dir(&repository.worktree)
        .stdin(if options.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(index) = options.index {
        command.env("GIT_INDEX_FILE", index);
    }
    let mut child = command.spawn().map_err(|e| OperationError { message: e.to_string(), cause: true })?;
    let stdin = child.stdin.take();
    let input = options.stdin;
    let write = async move {
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
        }
    };
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (_, out, err, status) =
        tokio::join!(write, collect(stdout, options.max_output), collect(stderr, options.max_output), child.wait());
    let error = |e: std::io::Error| OperationError { message: e.to_string(), cause: true };
    let (stdout, truncated) = out.map_err(error)?;
    let (stderr, _) = err.map_err(error)?;
    let status = status.map_err(error)?;
    use std::os::unix::process::ExitStatusExt;
    let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
    Ok(Run { code, stdout, stderr, truncated })
}

async fn collect(mut stream: impl AsyncRead + Unpin, max: Option<usize>) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut total = 0usize;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        total += read;
        let room = max.map_or(read, |max| max.saturating_sub(kept.len()).min(read));
        kept.extend_from_slice(&buffer[..room]);
    }
    Ok((kept, max.is_some_and(|max| total > max)))
}

/// `repositoryOperation`: the output of a git command that must succeed.
async fn operation(repository: &Repository, args: &[&str], options: Options<'_>) -> Result<Text, OperationError> {
    let run = run(repository, args, options).await?;
    let text = String::from_utf8_lossy(&run.stdout).into_owned();
    if run.code == 0 {
        return Ok(Text { text, truncated: run.truncated });
    }
    let stderr = String::from_utf8_lossy(&run.stderr);
    let message = [stderr.trim(), text.trim()].into_iter().find(|m| !m.is_empty()).map(str::to_owned);
    Err(OperationError { message: message.unwrap_or_else(|| "Git command failed".into()), cause: false })
}

/// Split NUL-terminated git output into its records.
fn nuls(text: &str) -> Vec<String> {
    text.split('\0').filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

/// Pathspec magic that matches exactly one path.
fn literal(file: &str) -> String {
    format!(":(literal){file}")
}

fn nul_list<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        out.extend_from_slice(item.as_ref().as_bytes());
        out.push(0);
    }
    out
}

fn unique_suffix() -> String {
    let mut bytes = [0u8; 8];
    crate::id::random(&mut bytes);
    let n = u64::from_le_bytes(bytes);
    let mut digits = String::new();
    let mut rest = n;
    while rest > 0 {
        let d = (rest % 36) as u32;
        digits.push(char::from_digit(d, 36).unwrap_or('0'));
        rest /= 36;
    }
    format!("{}-{}", std::process::id(), digits)
}

async fn exists(path: &str) -> bool {
    afs::try_exists(path).await.unwrap_or(false)
}

async fn remove_file(path: &str) {
    let _ = afs::remove_file(path).await;
}

/// `fs.remove(path, {recursive: true, force: true})`.
async fn remove_all(path: &str) -> std::io::Result<()> {
    match afs::symlink_metadata(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(meta) if meta.is_dir() => afs::remove_dir_all(path).await,
        Ok(_) => afs::remove_file(path).await,
    }
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.mtime() * 1000 + meta.mtime_nsec() / 1_000_000
}

/// `resolvePath`: a path git printed, against the directory it ran in.
fn resolve_path(cwd: &str, value: &str) -> String {
    let trimmed = value.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return cwd.into();
    }
    if trimmed.starts_with('/') {
        return crate::project::normalize(trimmed);
    }
    crate::project::normalize(&format!("{cwd}/{trimmed}"))
}

/// `Git.repo.discover`: the repository whose `.git` is nearest above `start`.
async fn discover(start: &str) -> Option<Repository> {
    let mut directory = std::path::PathBuf::from(start);
    let cwd = loop {
        if afs::try_exists(directory.join(".git")).await.unwrap_or(false) {
            break directory.to_string_lossy().into_owned();
        }
        if !directory.pop() {
            return None;
        }
    };
    let output = tokio::process::Command::new(crate::vcs::git_executable())
        .args(["rev-parse", "--git-dir", "--git-common-dir", "--show-toplevel"])
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let lines: Vec<&str> = output.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let git_dir = lines.first().copied().filter(|l| !l.is_empty())?;
    let common_dir = lines.get(1).copied().filter(|l| !l.is_empty())?;
    let top_level = lines.get(2).copied().filter(|l| !l.is_empty());
    Some(Repository {
        worktree: top_level.map_or_else(|| cwd.clone(), |t| resolve_path(&cwd, t)),
        git_directory: resolve_path(&cwd, git_dir),
        common_directory: resolve_path(&cwd, common_dir),
    })
}

/// `Git.repo.create`: a new store is initialized in a temporary sibling directory and renamed into place, so
/// processes racing to create it never see a half-initialized one.
async fn create(worktree: &str, git_directory: &str, seed: &Repository) -> Result<Repository, String> {
    let repository = Repository::store(worktree, git_directory);
    let temporary = format!("{git_directory}.init-{}", unique_suffix());
    let result = async {
        init_repository(worktree, &temporary, seed).await?;
        let renamed = afs::rename(&temporary, git_directory).await;
        if renamed.is_ok() || exists(&format!("{git_directory}/HEAD")).await {
            return Ok(());
        }
        Err("Failed to move Git storage into place".to_owned())
    }
    .await;
    let _ = remove_all(&temporary).await;
    result.map(|_| repository)
}

async fn init_repository(worktree: &str, directory: &str, seed: &Repository) -> Result<(), String> {
    afs::create_dir_all(directory).await.map_err(|_| "Failed to create Git storage".to_owned())?;
    let store = Repository::store(worktree, directory);
    operation(&store, &["init"], Options::default()).await?;
    let configure = async {
        afs::write(format!("{directory}/{CONFIG_FILE}"), CONFIG).await?;
        let config = format!("{directory}/config");
        let current = afs::read_to_string(&config).await?;
        if current.contains(CONFIG_INCLUDE) {
            return Ok(());
        }
        let separator = if current.ends_with('\n') { "\n" } else { "\n\n" };
        afs::append(&config, format!("{separator}{CONFIG_INCLUDE}")).await
    };
    configure.await.map_err(|_: std::io::Error| "Failed to configure Git storage".to_owned())?;
    let info = format!("{directory}/objects/info");
    afs::create_dir_all(&info).await.map_err(|_| "Failed to configure shared Git objects".to_owned())?;
    afs::write(format!("{info}/alternates"), format!("{}/objects\n", seed.common_directory))
        .await
        .map_err(|_| "Failed to configure shared Git objects".to_owned())?;
    let index = format!("{directory}/index");
    if seed_index(&store, seed, &index).await.is_err() {
        remove_file(&index).await;
    }
    Ok(())
}

/// A copied index keeps the timestamp git uses to detect racily clean entries.
async fn copy_index(from: &str, to: &str) -> std::io::Result<()> {
    let (from, to) = (from.to_owned(), to.to_owned());
    tokio::task::spawn_blocking(move || {
        let meta = std::fs::metadata(&from)?;
        std::fs::copy(&from, &to)?;
        if let Ok(modified) = meta.modified() {
            let times = std::fs::FileTimes::new().set_modified(modified).set_accessed(meta.accessed().unwrap_or(modified));
            let _ = std::fs::OpenOptions::new().write(true).open(&to).and_then(|f| f.set_times(times));
        }
        Ok(())
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e)))
}

/// `seedIndex`: the source index copied into the store; a split index's shared files are linked in just long enough
/// to rewrite the copy as a complete index.
async fn seed_index(store: &Repository, source: &Repository, target: &str) -> Result<(), String> {
    copy_index(&source.index_file(), target).await.map_err(|e| e.to_string())?;
    let names = afs::read_names(&source.git_directory).await.map_err(|e| e.to_string())?;
    let shared: Vec<String> = names.into_iter().filter(|name| name.starts_with("sharedindex.")).collect();
    if shared.is_empty() {
        return Ok(());
    }
    let result = async {
        for entry in &shared {
            let (from, to) = (format!("{}/{entry}", source.git_directory), format!("{}/{entry}", store.git_directory));
            if afs::hard_link(&from, &to).await.is_err() {
                afs::copy(&from, &to).await.map_err(|e| e.to_string())?;
            }
        }
        operation(store, &["update-index", "--no-split-index"], Options { index: Some(target), ..Default::default() })
            .await
            .map(|_| ())
            .map_err(String::from)
    }
    .await;
    for entry in &shared {
        remove_file(&format!("{}/{entry}", store.git_directory)).await;
    }
    result
}

/// `listChanges`: tracked paths that changed and untracked paths, against `index`.
async fn list_changes(
    repository: &Repository,
    scope: &str,
    index: Option<&str>,
    exclude_from: Option<&str>,
) -> Result<(Vec<String>, Vec<String>), OperationError> {
    let pathspec = literal(scope);
    let exclude = exclude_from.map(|file| format!("--exclude-from={file}"));
    let mut others = vec!["ls-files", "--others"];
    if let Some(exclude) = &exclude {
        others.push(exclude);
    }
    others.extend(["--exclude-standard", "-z", "--", &pathspec]);
    let changed = ["diff-files", "--name-only", "-z", "--", &pathspec];
    let (tracked, untracked) = tokio::join!(
        operation(repository, &changed, Options { index, ..Default::default() }),
        operation(repository, &others, Options { index, ..Default::default() }),
    );
    // Embedded repositories are listed as `dir/`; update-index records them as gitlinks.
    let strip = |text: Text| nuls(&text.text).into_iter().map(|f| f.strip_suffix('/').map(str::to_owned).unwrap_or(f)).collect();
    Ok((strip(tracked?), strip(untracked?)))
}

/// `updateIndex`: the listed changes written to `index`, without ignored paths and oversized untracked files.
/// Returns the untracked files skipped for size.
async fn update_index(
    repository: &Repository,
    tracked: &[String],
    untracked: &[String],
    ignores: Option<&Repository>,
    maximum: Option<u64>,
    index: Option<&str>,
    excluded: Option<&HashSet<String>>,
) -> Result<Vec<String>, OperationError> {
    let candidates: Vec<String> = tracked.iter().chain(untracked).cloned().collect();
    if candidates.is_empty() {
        return Ok(vec![]);
    }
    let computed;
    let excluded = match (excluded, ignores) {
        (Some(excluded), _) => excluded,
        (None, Some(ignores)) => {
            computed = ignored(ignores, &candidates).await?;
            &computed
        }
        (None, None) => {
            computed = HashSet::new();
            &computed
        }
    };
    let mut skipped = Vec::new();
    if let Some(maximum) = maximum.filter(|m| *m > 0) {
        for item in untracked.iter().filter(|item| !excluded.contains(*item)) {
            let meta = afs::metadata(format!("{}/{item}", repository.worktree)).await;
            if meta.is_ok_and(|m| m.is_file() && m.len() > maximum) {
                skipped.push(item.clone());
            }
        }
    }
    let added: Vec<&String> = candidates.iter().filter(|item| !excluded.contains(*item) && !skipped.contains(item)).collect();
    // Untracked paths are not in the index, so only tracked paths that are now ignored need removing.
    let removed: Vec<&String> = tracked.iter().filter(|item| excluded.contains(*item)).collect();
    if !removed.is_empty() {
        let stdin = nul_list(&removed);
        operation(repository, &["update-index", "--force-remove", "-z", "--stdin"], Options { index, stdin: Some(stdin), max_output: None })
            .await?;
    }
    // update-index drops a tracked file replaced by an embedded repository; a second line for the same path, in the
    // same call, records its gitlink.
    let mut embedded = Vec::new();
    for item in tracked.iter().filter(|item| !excluded.contains(*item)) {
        if exists(&format!("{}/{item}/.git", repository.worktree)).await {
            embedded.push(item);
        }
    }
    if !added.is_empty() {
        let stdin = nul_list(added.iter().chain(embedded.iter()));
        operation(
            repository,
            &["update-index", "--add", "--remove", "--replace", "-z", "--stdin"],
            Options { index, stdin: Some(stdin), max_output: None },
        )
        .await?;
    }
    Ok(skipped)
}

/// `Git.index.ignored`: the paths the repository's ignore rules match.
async fn ignored(repository: &Repository, paths: &[String]) -> Result<HashSet<String>, OperationError> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }
    let run = run(repository, &["check-ignore", "--no-index", "--stdin", "-z"], Options { stdin: Some(nul_list(paths)), ..Default::default() })
        .await?;
    // Exit 1: none of the paths are ignored.
    if run.code != 0 && run.code != 1 {
        let stderr = String::from_utf8_lossy(&run.stderr).trim().to_owned();
        let message = if stderr.is_empty() { "Failed to check ignored paths".to_owned() } else { stderr };
        return Err(OperationError { message, cause: false });
    }
    Ok(nuls(&String::from_utf8_lossy(&run.stdout)).into_iter().collect())
}

/// `Git.tree.write`.
async fn write_tree(repository: &Repository, index: Option<&str>) -> Result<String, OperationError> {
    let text = operation(repository, &["write-tree"], Options { index, ..Default::default() }).await?.text;
    let tree = text.trim();
    if (40..=64).contains(&tree.len()) && tree.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Ok(tree.to_owned());
    }
    Err(OperationError { message: format!("Invalid tree ID: {tree}"), cause: false })
}

/// Git only replaces an index by renaming a new file into place, so every write changes its inode.
async fn index_fingerprint(file: &str) -> Option<String> {
    let meta = afs::metadata(file).await.ok()?;
    Some(format!("{}:{}:{}", meta.ino(), meta.len(), mtime_ms(&meta)))
}

fn temporary_index(repository: &Repository) -> String {
    format!("{}/{TEMPORARY_INDEX_PREFIX}{}-{}", repository.git_directory, crate::id::now(), unique_suffix())
}

/// A temporary index for `use`: a hard link to (or a copy of) the store's index, renamed over it afterwards.
/// Returns the fingerprint of the index installed, if the rename succeeded.
async fn prepare_temporary_index(repository: &Repository) -> Result<String, OperationError> {
    let current = repository.index_file();
    let index = temporary_index(repository);
    if !exists(&current).await || afs::hard_link(&current, &index).await.is_ok() {
        return Ok(index);
    }
    copy_index(&current, &index)
        .await
        .map_err(|_| OperationError { message: "Failed to prepare a temporary index".into(), cause: true })?;
    Ok(index)
}

async fn install_temporary_index(repository: &Repository, index: &str) -> Option<String> {
    // Fingerprint before the rename, so a concurrent writer's index can never be paired with this tree.
    let fingerprint = index_fingerprint(index).await;
    let renamed = afs::rename(index, repository.index_file()).await.is_ok();
    if renamed { fingerprint } else { None }
}

/// `Git.tree.capture`.
async fn capture_tree(
    repository: &Repository,
    scopes: &[String],
    ignores: Option<&Repository>,
    maximum: Option<u64>,
) -> Result<String, OperationError> {
    let lock = store_lock(repository);
    let _guard = lock.lock().await;
    let prepared = !stores().prepared.insert(repository.git_directory.clone());
    if !prepared {
        sweep_temporary_indexes(repository).await;
        ensure_snapshot_config(repository).await;
    }
    match attempt_capture(repository, scopes, ignores, maximum).await {
        Ok(tree) => Ok(tree),
        Err(error) => {
            if !index_unreadable(repository).await {
                return Err(error);
            }
            eprintln!("rebuilding unreadable snapshot index {}: {}", repository.git_directory, error.message);
            rebuild_index(repository, ignores).await;
            attempt_capture(repository, scopes, ignores, maximum).await
        }
    }
}

/// `attemptCapture`: changes are listed against the temporary index they are written to, never the store's index,
/// which another process may replace at any moment.
async fn attempt_capture(
    repository: &Repository,
    scopes: &[String],
    ignores: Option<&Repository>,
    maximum: Option<u64>,
) -> Result<String, OperationError> {
    let source_exclude = ignores.map(|r| format!("{}/info/exclude", r.common_directory));
    let exclude_from = match &source_exclude {
        Some(file) if exists(file).await => Some(file.as_str()),
        _ => None,
    };
    let index = prepare_temporary_index(repository).await?;
    let value = capture_with_index(repository, scopes, ignores, maximum, &index, exclude_from).await;
    let installed = match &value {
        Ok(_) => install_temporary_index(repository, &index).await,
        Err(_) => None,
    };
    remove_file(&index).await;
    let tree = value?;
    let mut stores = stores();
    match installed {
        Some(fingerprint) => {
            stores.last_captures.insert(repository.git_directory.clone(), (fingerprint, tree.clone()));
        }
        None => {
            stores.last_captures.remove(&repository.git_directory);
        }
    }
    Ok(tree)
}

async fn capture_with_index(
    repository: &Repository,
    scopes: &[String],
    ignores: Option<&Repository>,
    maximum: Option<u64>,
    index: &str,
    exclude_from: Option<&str>,
) -> Result<String, OperationError> {
    let scanned = index_fingerprint(index).await;
    let mut changes = Vec::new();
    for scope in scopes {
        changes.push(list_changes(repository, scope, Some(index), exclude_from).await?);
    }
    let (last, oversized) = {
        let stores = stores();
        (stores.last_captures.get(&repository.git_directory).cloned(), stores.oversized.get(&repository.git_directory).cloned())
    };
    let untracked: Vec<String> = changes.iter().flat_map(|(_, u)| u.iter().cloned()).collect();
    let mut still_oversized = HashSet::new();
    if let Some(oversized) = &oversized {
        for file in untracked.iter().filter(|f| oversized.contains_key(*f)) {
            if oversized_stamp(repository, file, maximum).await.is_some_and(|stamp| Some(&stamp) == oversized.get(file)) {
                still_oversized.insert(file.clone());
            }
        }
    }
    // Paths the source ignores are listed as untracked on every scan; they leave the index unchanged.
    let clean = scanned.is_some()
        && last.as_ref().is_some_and(|(fingerprint, _)| Some(fingerprint) == scanned.as_ref())
        && changes.iter().all(|(tracked, _)| tracked.is_empty());
    let excluded = if clean {
        Some(match ignores {
            Some(ignores) => {
                let candidates: Vec<String> = untracked.iter().filter(|f| !still_oversized.contains(*f)).cloned().collect();
                ignored(ignores, &candidates).await?
            }
            None => HashSet::new(),
        })
    } else {
        None
    };
    if let (Some((_, tree)), Some(excluded)) = (&last, &excluded) {
        if untracked.iter().all(|file| excluded.contains(file) || still_oversized.contains(file)) {
            return Ok(tree.clone());
        }
    }
    let mut skipped = Vec::new();
    for (tracked, untracked) in &changes {
        skipped.extend(update_index(repository, tracked, untracked, ignores, maximum, Some(index), excluded.as_ref()).await?);
    }
    let mut stamps = HashMap::new();
    for file in skipped {
        if let Some(stamp) = oversized_stamp(repository, &file, maximum).await {
            stamps.insert(file, stamp);
        }
    }
    stores().oversized.insert(repository.git_directory.clone(), stamps);
    write_tree(repository, Some(index)).await
}

async fn oversized_stamp(repository: &Repository, file: &str, maximum: Option<u64>) -> Option<String> {
    let meta = afs::metadata(format!("{}/{file}", repository.worktree)).await.ok()?;
    let maximum = maximum.filter(|m| *m > 0)?;
    (meta.is_file() && meta.len() > maximum).then(|| format!("{}:{}", meta.len(), mtime_ms(&meta)))
}

/// Only an index git itself cannot read is rebuilt, decided by exit status.
async fn index_unreadable(repository: &Repository) -> bool {
    if !exists(&repository.index_file()).await {
        return false;
    }
    let probe = literal(".opencode-probe");
    match operation(repository, &["ls-files", "-z", "--", &probe], Options::default()).await {
        Ok(_) => false,
        Err(error) => !error.cause,
    }
}

/// The source's stat cache and cache-tree spare recovery from rehashing every tracked file.
async fn rebuild_index(repository: &Repository, source: Option<&Repository>) {
    stores().last_captures.remove(&repository.git_directory);
    let temporary = temporary_index(repository);
    let copied = match source {
        Some(source) => seed_index(repository, source, &temporary).await.is_ok(),
        None => false,
    };
    if copied {
        let _ = afs::rename(&temporary, repository.index_file()).await;
    } else {
        remove_file(&repository.index_file()).await;
    }
    remove_file(&temporary).await;
}

/// Stores created by earlier releases keep their settings otherwise; only opencode's include file is rewritten.
async fn ensure_snapshot_config(repository: &Repository) {
    let file = format!("{}/{CONFIG_FILE}", repository.git_directory);
    match afs::read_to_string(&file).await {
        Ok(current) if current != CONFIG => {}
        _ => return,
    }
    let temporary = format!("{file}.{}", unique_suffix());
    if afs::write(&temporary, CONFIG).await.is_ok() {
        let _ = afs::rename(&temporary, &file).await;
    }
    remove_file(&temporary).await;
}

/// Temporary indexes left by killed processes, older than an hour by the creation time in their name.
async fn sweep_temporary_indexes(repository: &Repository) {
    let cutoff = crate::id::now() - HOUR_MS;
    let Ok(names) = afs::read_names(&repository.git_directory).await else { return };
    for name in names {
        let Some(rest) = name.strip_prefix(TEMPORARY_INDEX_PREFIX) else { continue };
        let created = rest.split('-').next().and_then(|n| n.parse::<f64>().ok());
        if created.is_some_and(|c| c < cutoff as f64) {
            remove_file(&format!("{}/{name}", repository.git_directory)).await;
        }
    }
}

/// `Git.tree.diff`: path selections are split into groups that fit a command line.
async fn tree_diff(
    repository: &Repository,
    from: &str,
    to: &str,
    context: Option<i64>,
    paths: Option<&[String]>,
) -> Result<Vec<FileDiff>, OperationError> {
    let groups: Vec<Option<Vec<String>>> = match paths {
        Some(paths) => path_groups(paths).into_iter().map(Some).collect(),
        None => vec![None],
    };
    let mut diffs = Vec::new();
    let mut budget = crate::vcs::MAX_TOTAL_PATCH_BYTES;
    for paths in groups {
        let (group, bytes, truncated) = diff_group(repository, from, to, context, paths.as_deref(), budget).await?;
        diffs.extend(group);
        budget = if truncated { 0 } else { budget.saturating_sub(bytes) };
    }
    Ok(diffs)
}

async fn diff_group(
    repository: &Repository,
    from: &str,
    to: &str,
    context: Option<i64>,
    paths: Option<&[String]>,
    budget: usize,
) -> Result<(Vec<FileDiff>, usize, bool), OperationError> {
    let mut args = vec!["--no-renames", from, to, "--"];
    args.extend(paths.unwrap_or_default().iter().map(String::as_str));
    let with = |head: &[&'static str]| -> Vec<&str> { head.iter().copied().chain(args.iter().copied()).collect() };
    let unified = format!("--unified={}", context.unwrap_or(3));
    let names_args = with(&["--literal-pathspecs", "diff", "--name-status", "-z"]);
    let numbers_args = with(&["--literal-pathspecs", "diff", "--numstat", "-z"]);
    let mut patch_args = vec!["--literal-pathspecs", "-c", "core.quotepath=false", "diff", "--no-ext-diff", &unified];
    patch_args.extend(args.iter().copied());
    // Patch headers have no -z form: unquoted paths keep chunksByFile matching non-ASCII names.
    let patch_run = async {
        if budget == 0 {
            return Ok(Text { text: String::new(), truncated: true });
        }
        operation(repository, &patch_args, Options { max_output: Some(budget), ..Default::default() }).await
    };
    let (names, numbers, patch_text) = tokio::join!(
        operation(repository, &names_args, Options::default()),
        operation(repository, &numbers_args, Options::default()),
        patch_run,
    );
    let (names, numbers, patch_text) = (names?, numbers?, patch_text?);
    let statuses = nuls(&names.text);
    let files: Vec<(String, &'static str)> = statuses
        .chunks(2)
        .filter(|pair| pair.len() == 2)
        .map(|pair| {
            let status = if pair[0].starts_with('A') {
                "added"
            } else if pair[0].starts_with('D') {
                "deleted"
            } else {
                "modified"
            };
            (pair[1].clone(), status)
        })
        .collect();
    let mut stats: HashMap<String, Option<(i64, i64)>> = HashMap::new();
    for line in nuls(&numbers.text) {
        let mut parts = line.split('\t');
        let (Some(additions), Some(deletions)) = (parts.next(), parts.next()) else { continue };
        let file: Vec<&str> = parts.collect();
        if additions.is_empty() || deletions.is_empty() || file.is_empty() {
            continue;
        }
        let stat = if additions == "-" || deletions == "-" {
            None
        } else {
            Some((js_number(additions), js_number(deletions)))
        };
        stats.insert(file.join("\t"), stat);
    }
    let patches = patch::chunks_by_file(&patch_text.text, patch_text.truncated, |index| files.get(index).map(|f| f.0.clone()));
    let diffs = files
        .iter()
        .map(|(file, status)| {
            let stat = stats.get(file);
            let (additions, deletions) = stat.copied().flatten().unwrap_or((0, 0));
            let binary = matches!(stat, Some(None));
            FileDiff {
                file: file.clone(),
                patch: if binary { String::new() } else { patches.get(file).cloned().unwrap_or_else(|| patch::empty(file)) },
                additions,
                deletions,
                status,
                more: false,
            }
        })
        .collect();
    Ok((diffs, patch_text.text.len(), patch_text.truncated))
}

/// `Number(text)` of a count git printed.
fn js_number(text: &str) -> i64 {
    text.parse().unwrap_or(0)
}

/// UTF-16 length, as JS counts a string.
fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `pathGroups`: paths sorted as git sorts them, grouped to fit a command line (with room for quotes).
fn path_groups(paths: &[String]) -> Vec<Vec<String>> {
    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut length = 0;
    for file in sorted {
        let cost = js_length(file) + 3;
        match groups.last_mut() {
            Some(current) if length + cost <= ARGUMENT_BUDGET => {
                current.push(file.clone());
                length += cost;
            }
            _ => {
                groups.push(vec![file.clone()]);
                length = cost;
            }
        }
    }
    groups
}

/// Paths whose restore operations commute: none inside another, and every segment printable ASCII that no platform
/// rewrites.
fn can_batch_restore(files: &[&String]) -> bool {
    let canonical = files.iter().all(|file| {
        file.split('/').all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| (0x20..=0x7e).contains(&b))
                && !part.contains(['\\', ':'])
                && !part.ends_with(['.', ' '])
        })
    });
    if !canonical {
        return false;
    }
    let folded: HashSet<String> = files.iter().map(|f| f.to_lowercase()).collect();
    if folded.len() != files.len() {
        return false;
    }
    files.iter().all(|file| {
        let lower = file.to_lowercase();
        let parts: Vec<&str> = lower.split('/').collect();
        (1..parts.len()).all(|index| !folded.contains(&parts[..index].join("/")))
    })
}

/// `hasEntry`: whether the tree has the path.
async fn has_entry(repository: &Repository, tree: &str, file: &str) -> Result<bool, OperationError> {
    let pathspec = literal(file);
    let text = operation(repository, &["ls-tree", "-z", tree, "--", &pathspec], Options::default()).await?.text;
    let text = text.strip_suffix('\0').unwrap_or(&text);
    if text.is_empty() {
        return Ok(false);
    }
    if tree_entry(text, false).is_none() {
        return Err(OperationError { message: format!("Invalid tree entry for {file}"), cause: false });
    }
    Ok(true)
}

/// The path of an `ls-tree` record (`<mode> <type> <object>\t<path>`).
fn tree_entry(record: &str, strict: bool) -> Option<&str> {
    let (head, path) = record.split_once('\t')?;
    let mut fields = head.split(' ').filter(|f| strict || !f.is_empty());
    let mode = fields.next()?;
    let kind = fields.next()?;
    let object = fields.next()?;
    let valid = !mode.is_empty()
        && mode.bytes().all(|b| b.is_ascii_digit())
        && !kind.is_empty()
        && kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !object.is_empty()
        && object.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    (valid && fields.next().is_none()).then_some(path)
}

async fn remove_path(repository: &Repository, file: &str) -> Result<(), OperationError> {
    remove_all(&format!("{}/{file}", repository.worktree))
        .await
        .map_err(|_| OperationError { message: format!("Failed to remove {file}"), cause: true })
}

/// `pathsInTree`: which of the paths the tree has (one `ls-tree`, or a full listing when they do not fit).
async fn paths_in_tree(repository: &Repository, tree: &str, files: &[&String]) -> Result<HashSet<String>, OperationError> {
    let pathspecs: Vec<String> = files.iter().map(|f| literal(f)).collect();
    let fits = pathspecs.iter().map(|p| js_length(p) + 1).sum::<usize>() <= ARGUMENT_BUDGET;
    let mut args = vec!["ls-tree"];
    if fits {
        args.extend(["-z", tree, "--"]);
        args.extend(pathspecs.iter().map(String::as_str));
    } else {
        args.extend(["-r", "-t", "-z", "--full-tree", tree]);
    }
    let text = operation(repository, &args, Options::default()).await?.text;
    let mut listed = HashSet::new();
    for record in nuls(&text) {
        match tree_entry(&record, true) {
            Some(path) => listed.insert(path.to_owned()),
            None => return Err(OperationError { message: format!("Invalid tree entry: {record}"), cause: false }),
        };
    }
    Ok(listed)
}

/// `Git.tree.restore`: batched paths cost one ls-tree and one checkout per source tree.
async fn restore_tree(repository: &Repository, files: &IndexMap<String, String>) -> Result<(), OperationError> {
    let lock = store_lock(repository);
    let _guard = lock.lock().await;
    if files.is_empty() {
        return Ok(());
    }
    let keys: Vec<&String> = files.keys().collect();
    if !can_batch_restore(&keys) {
        for (file, tree) in files {
            if !has_entry(repository, tree, file).await? {
                remove_path(repository, file).await?;
                continue;
            }
            let index = prepare_temporary_index(repository).await?;
            let pathspec = literal(file);
            let result = operation(repository, &["checkout", tree, "--", &pathspec], Options { index: Some(&index), ..Default::default() }).await;
            if result.is_ok() {
                install_temporary_index(repository, &index).await;
            }
            remove_file(&index).await;
            result?;
        }
        return Ok(());
    }
    let mut groups: IndexMap<&String, Vec<&String>> = IndexMap::new();
    for (file, tree) in files {
        groups.entry(tree).or_default().push(file);
    }
    let mut plan = Vec::new();
    for (tree, files) in &groups {
        let present = paths_in_tree(repository, tree, files).await?;
        let (found, absent): (Vec<&String>, Vec<&String>) = files.iter().partition(|f| present.contains(f.as_str()));
        plan.push((*tree, found, absent));
    }
    // Checkouts go first, so one failing removal cannot stop the other files from being restored.
    if plan.iter().any(|(_, present, _)| !present.is_empty()) {
        let index = prepare_temporary_index(repository).await?;
        let mut result = Ok(());
        for (tree, present, _) in plan.iter().filter(|(_, present, _)| !present.is_empty()) {
            let stdin = nul_list(present.iter().map(|f| literal(f)));
            let options = Options { index: Some(&index), stdin: Some(stdin), max_output: None };
            if let Err(error) = operation(repository, &["checkout", tree, "--pathspec-from-file=-", "--pathspec-file-nul"], options).await {
                result = Err(error);
                break;
            }
        }
        if result.is_ok() {
            install_temporary_index(repository, &index).await;
        }
        remove_file(&index).await;
        result?;
    }
    for (_, _, absent) in &plan {
        for file in absent {
            remove_path(repository, file).await?;
        }
    }
    Ok(())
}

// --- Git.objects.pack ---

/// `Git.objects.pack`: snapshot trees have no refs, so git's own repack and prune would treat every snapshot as
/// garbage. Packing hands pack-objects every loose object (and, when combining, every object in the old packs),
/// checks the new index lists all of them, and only then deletes the loose copies and the combined packs.
async fn pack_objects(repository: &Repository) -> Result<(), Error> {
    let objects = format!("{}/objects", repository.git_directory);
    let pack_directory = format!("{objects}/pack");
    let loose = loose_objects(&objects).await;
    let packs = local_packs(&pack_directory).await;
    let combine = packs.len() >= PACK_COUNT_LIMIT;
    if loose.len() < LOOSE_OBJECT_LIMIT && !combine {
        return Ok(());
    }
    let lock = format!("{}/opencode-pack.lock", repository.git_directory);
    let stale = afs::metadata(&lock).await.is_ok_and(|m| mtime_ms(&m) < crate::id::now() - HOUR_MS);
    if stale {
        remove_file(&lock).await;
    }
    let acquired = afs::create_new(&lock, std::process::id().to_string()).await.is_ok();
    if !acquired {
        return Ok(());
    }
    let result = pack_locked(repository, &pack_directory, &loose, if combine { &packs } else { &[] }).await;
    remove_file(&lock).await;
    result
}

async fn pack_locked(repository: &Repository, pack_directory: &str, loose: &[(String, String)], packs: &[String]) -> Result<(), Error> {
    let fail = |message: String| Error::new("pack", message);
    sweep_temporary_packs(pack_directory).await;
    let mut combined = Vec::new();
    for pack in packs {
        combined.push((pack.clone(), pack_index(repository, &format!("{pack_directory}/{pack}.idx")).await?));
    }
    let mut seen = HashSet::new();
    let object_ids: Vec<String> = loose
        .iter()
        .map(|(oid, _)| oid.clone())
        .chain(combined.iter().flat_map(|(_, oids)| oids.iter().cloned()))
        .filter(|oid| seen.insert(oid.clone()))
        .collect();
    if object_ids.is_empty() {
        return Ok(());
    }
    let base = format!("{pack_directory}/pack");
    let stdin = format!("{}\n", object_ids.join("\n")).into_bytes();
    let output = operation(
        repository,
        &["-c", "pack.threads=2", "-c", "pack.windowMemory=64m", "pack-objects", "-q", "--non-empty", &base],
        Options { stdin: Some(stdin), ..Default::default() },
    )
    .await
    .map_err(|e| fail(e.message))?;
    let new_packs: Vec<String> = output.text.split('\n').map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
    let mut packed = HashSet::new();
    for name in &new_packs {
        packed.extend(pack_index(repository, &format!("{pack_directory}/pack-{name}.idx")).await?);
    }
    let missing = object_ids.iter().filter(|oid| !packed.contains(*oid)).count();
    if missing > 0 {
        return Err(fail(format!("Packed {} objects but {missing} are missing; nothing was removed", packed.len())));
    }
    for (_, file) in loose {
        remove_file(file).await;
    }
    let names: HashSet<String> = new_packs.iter().map(|n| format!("pack-{n}")).collect();
    for (pack, _) in combined.iter().filter(|(pack, _)| !names.contains(pack)) {
        // The index goes first so readers never see an index without its pack.
        for extension in [".idx", ".pack", ".rev", ".bitmap", ".mtimes"] {
            remove_file(&format!("{pack_directory}/{pack}{extension}")).await;
        }
    }
    Ok(())
}

async fn read_names(directory: &str) -> Vec<String> {
    afs::read_names(directory).await.unwrap_or_default()
}

fn hex(text: &str) -> bool {
    text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

async fn loose_objects(objects: &str) -> Vec<(String, String)> {
    let mut listed = Vec::new();
    for prefix in read_names(objects).await.into_iter().filter(|e| e.len() == 2 && hex(e)) {
        let directory = format!("{objects}/{prefix}");
        for entry in read_names(&directory).await.into_iter().filter(|e| (e.len() == 38 || e.len() == 62) && hex(e)) {
            listed.push((format!("{prefix}{entry}"), format!("{directory}/{entry}")));
        }
    }
    listed
}

/// Packs with a .keep marker or a promisor file belong to someone else's policy and are left alone.
async fn local_packs(directory: &str) -> Vec<String> {
    let entries: HashSet<String> = read_names(directory).await.into_iter().collect();
    entries
        .iter()
        .filter(|e| e.starts_with("pack-") && e.ends_with(".pack"))
        .map(|e| e[..e.len() - ".pack".len()].to_owned())
        .filter(|pack| {
            entries.contains(&format!("{pack}.idx"))
                && !entries.contains(&format!("{pack}.keep"))
                && !entries.contains(&format!("{pack}.promisor"))
        })
        .collect()
}

async fn pack_index(repository: &Repository, file: &str) -> Result<Vec<String>, Error> {
    let fail = |message: String| Error::new("pack", message);
    let bytes = afs::read(file).await.map_err(|_| fail(format!("Failed to read {file}")))?;
    if let Some(ids) = pack_index_object_ids(&bytes) {
        return Ok(ids);
    }
    let output = operation(repository, &["show-index"], Options { stdin: Some(bytes), ..Default::default() })
        .await
        .map_err(|e| fail(e.message))?;
    Ok(output.text.split('\n').filter_map(|line| line.split(' ').nth(1).filter(|o| !o.is_empty()).map(str::to_owned)).collect())
}

/// Object IDs from a version 2 pack index; `None` for version 1 and for packs with 64-bit offsets.
fn pack_index_object_ids(bytes: &[u8]) -> Option<Vec<String>> {
    let word = |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
    if bytes.len() < 1032 || word(0) != 0xff74_4f63 || word(4) != 2 {
        return None;
    }
    let count = word(8 + 255 * 4);
    let rest = bytes.len().checked_sub(1032 + 8 * count)?;
    if rest % (count + 2) != 0 {
        return None;
    }
    let hash = rest / (count + 2);
    if hash != 20 && hash != 32 {
        return None;
    }
    Some(
        (0..count)
            .map(|index| bytes[1032 + index * hash..1032 + (index + 1) * hash].iter().map(|b| format!("{b:02x}")).collect())
            .collect(),
    )
}

/// pack-objects writes tmp_* files and renames them; leftovers older than an hour mean a killed process.
async fn sweep_temporary_packs(directory: &str) {
    let cutoff = crate::id::now() - HOUR_MS;
    let entries = read_names(directory).await;
    let indexed: HashSet<&str> = entries.iter().filter_map(|e| e.strip_suffix(".idx")).collect();
    for entry in &entries {
        let orphan = entry.starts_with("pack-") && entry.ends_with(".pack") && !indexed.contains(&entry[..entry.len() - 5]);
        if !entry.starts_with("tmp_") && !orphan {
            continue;
        }
        let path = format!("{directory}/{entry}");
        if afs::metadata(&path).await.is_ok_and(|m| mtime_ms(&m) < cutoff) {
            remove_file(&path).await;
        }
    }
}

/// File operations off the runtime's thread (`tokio::fs` without its feature).
mod afs {
    use std::io::Result;
    use std::path::PathBuf;

    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        tokio::task::spawn_blocking(f).await.expect("file operation")
    }

    pub async fn try_exists(path: impl Into<PathBuf>) -> Result<bool> {
        let path = path.into();
        blocking(move || path.try_exists()).await
    }

    pub async fn metadata(path: impl Into<PathBuf>) -> Result<std::fs::Metadata> {
        let path = path.into();
        blocking(move || std::fs::metadata(path)).await
    }

    pub async fn symlink_metadata(path: impl Into<PathBuf>) -> Result<std::fs::Metadata> {
        let path = path.into();
        blocking(move || std::fs::symlink_metadata(path)).await
    }

    pub async fn remove_file(path: impl Into<PathBuf>) -> Result<()> {
        let path = path.into();
        blocking(move || std::fs::remove_file(path)).await
    }

    pub async fn remove_dir_all(path: impl Into<PathBuf>) -> Result<()> {
        let path = path.into();
        blocking(move || std::fs::remove_dir_all(path)).await
    }

    pub async fn create_dir_all(path: impl Into<PathBuf>) -> Result<()> {
        let path = path.into();
        blocking(move || std::fs::create_dir_all(path)).await
    }

    pub async fn rename(from: impl Into<PathBuf>, to: impl Into<PathBuf>) -> Result<()> {
        let (from, to) = (from.into(), to.into());
        blocking(move || std::fs::rename(from, to)).await
    }

    pub async fn hard_link(from: impl Into<PathBuf>, to: impl Into<PathBuf>) -> Result<()> {
        let (from, to) = (from.into(), to.into());
        blocking(move || std::fs::hard_link(from, to)).await
    }

    pub async fn copy(from: impl Into<PathBuf>, to: impl Into<PathBuf>) -> Result<u64> {
        let (from, to) = (from.into(), to.into());
        blocking(move || std::fs::copy(from, to)).await
    }

    pub async fn read(path: impl Into<PathBuf>) -> Result<Vec<u8>> {
        let path = path.into();
        blocking(move || std::fs::read(path)).await
    }

    pub async fn read_to_string(path: impl Into<PathBuf>) -> Result<String> {
        let path = path.into();
        blocking(move || std::fs::read_to_string(path)).await
    }

    pub async fn write(path: impl Into<PathBuf>, data: impl AsRef<[u8]> + Send + 'static) -> Result<()> {
        let path = path.into();
        blocking(move || std::fs::write(path, data)).await
    }

    pub async fn append(path: impl Into<PathBuf>, data: String) -> Result<()> {
        let path = path.into();
        blocking(move || {
            use std::io::Write;
            std::fs::OpenOptions::new().append(true).open(path)?.write_all(data.as_bytes())
        })
        .await
    }

    /// Creates the file, failing when it exists (`flag: "wx"`).
    pub async fn create_new(path: impl Into<PathBuf>, data: String) -> Result<()> {
        let path = path.into();
        blocking(move || {
            use std::io::Write;
            std::fs::OpenOptions::new().write(true).create_new(true).open(path)?.write_all(data.as_bytes())
        })
        .await
    }

    pub async fn read_names(path: impl Into<PathBuf>) -> Result<Vec<String>> {
        let path = path.into();
        blocking(move || {
            std::fs::read_dir(path)?.map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned())).collect()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_only_canonical_disjoint_paths() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let check = |list: &[&str]| {
            let owned = names(list);
            let refs: Vec<&String> = owned.iter().collect();
            can_batch_restore(&refs)
        };
        assert!(check(&["README.md", "src/new.ts"]));
        assert!(!check(&["ünïcode.txt"]));
        assert!(!check(&["src", "src/a.ts"]));
        assert!(!check(&["A.txt", "a.txt"]));
        assert!(!check(&["dir/file."]));
    }

    #[test]
    fn groups_sort_bytewise() {
        let groups = path_groups(&["b".into(), "a/c".into(), "a".into()]);
        assert_eq!(groups, vec![vec!["a".to_string(), "a/c".into(), "b".into()]]);
    }

    #[test]
    fn tree_entries() {
        assert_eq!(tree_entry("100644 blob 0123abcd\tsrc/a.ts", true), Some("src/a.ts"));
        assert_eq!(tree_entry("100644 blob xyz\tsrc/a.ts", true), None);
    }
}
