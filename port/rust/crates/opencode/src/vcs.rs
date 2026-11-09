//! Version control of a location (packages/core/src/vcs.ts, vcs/patch.ts) and its providers, the git and Mercurial
//! plugins (packages/core/src/plugin/vcs/git.ts, hg.ts). Both drive their tool's CLI.
//!
//! A location registers the provider of its repository (git or hg); a markerless location registers both, so it can
//! initialize its first repository, and selects neither. The branch information (`Vcs.Info`) is read when the
//! location boots and kept; `vcs.branch.updated` announces a change of the current branch.
//!
//! Snapshots (packages/core/src/snapshot.ts over git.ts's index, tree and object operations) are
//! [`crate::snapshot`].
//!
//! TODO(port): refreshing the branch when the repository's HEAD changes (the file watcher's `filesystem.changed`
//! events); providers registered by external plugins.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use opencode_protocol::Json;

use crate::collate;
use crate::json::Obj;
use crate::process::Command;
use crate::text::{self, lines, nuls, trim};

/// Effectively "full file" context; adapters use it when no context is requested.
pub const PATCH_CONTEXT_LINES: i64 = 2_147_483_647;
pub const MAX_PATCH_BYTES: usize = 10_000_000;
pub const MAX_TOTAL_PATCH_BYTES: usize = 10_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Git,
    Hg,
}

impl Provider {
    pub fn id(self) -> &'static str {
        match self {
            Provider::Git => "git",
            Provider::Hg => "hg",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Working,
    Branch,
    Committed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Untracked {
    Files,
    Folders,
}

#[derive(Debug, Clone, Default)]
pub struct DiffOptions {
    pub context: Option<i64>,
    pub base: Option<String>,
    pub untracked: Option<Untracked>,
}

/// `Vcs.DiffError`: its message is the reason the review cannot be produced.
#[derive(Debug)]
pub struct DiffError(pub String);

/// `Vcs.InitializeError` kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitializeError {
    Missing,
    Conflict,
    Unknown,
    /// A provider without `init`; the built-in ones both have it. TODO(port): VCS providers of external plugins.
    #[allow(dead_code)]
    Unsupported,
    Failed,
}

/// `FileDiff.Info`.
#[derive(Debug, Clone)]
pub struct FileDiff {
    pub file: String,
    pub patch: String,
    pub additions: i64,
    pub deletions: i64,
    pub status: &'static str,
    pub more: bool,
}

impl FileDiff {
    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        o.str("file", &self.file).str("patch", &self.patch).int("additions", self.additions).int("deletions", self.deletions);
        o.str("status", self.status);
        if self.more {
            o.set("more", Json::Bool(true));
        }
        o.done()
    }
}

/// `Vcs.FileStatus`.
#[derive(Debug, Clone)]
pub struct FileStatus {
    pub file: String,
    pub additions: i64,
    pub deletions: i64,
    pub status: &'static str,
}

impl FileStatus {
    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        o.str("file", &self.file).int("additions", self.additions).int("deletions", self.deletions).str("status", self.status);
        o.done()
    }
}

/// A location's VCS service (`Vcs.Service`).
#[derive(Debug)]
pub struct Service {
    /// Registered providers, in registration order.
    providers: Vec<Provider>,
    /// The location's repository type: the provider selected for it.
    selected: Option<Provider>,
    directory: String,
    worktree: String,
    current: Mutex<Info>,
}

/// `Vcs.Info`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
    pub current: Option<String>,
    pub default: Option<String>,
    pub provider: Option<Provider>,
}

impl Info {
    pub fn to_json(&self) -> Json {
        let mut branch = Obj::new();
        branch.opt_str("current", self.current.as_deref()).opt_str("default", self.default.as_deref());
        let mut o = Obj::new();
        o.set("branch", branch.done());
        o.opt_str("provider", self.provider.map(Provider::id));
        o.done()
    }
}

impl Service {
    /// The service of a location in `directory` whose project checkout is `worktree`, with the repository type the
    /// project resolved (`kind`: `"git"`, `"hg"` or none).
    pub fn new(kind: Option<&str>, directory: &str, worktree: &str) -> Self {
        let (providers, selected) = match kind {
            Some("git") => (vec![Provider::Git], Some(Provider::Git)),
            Some("hg") => (vec![Provider::Hg], Some(Provider::Hg)),
            // Markerless locations need both definitions to initialize their first repository.
            _ => (vec![Provider::Git, Provider::Hg], None),
        };
        Self { providers, selected, directory: directory.into(), worktree: worktree.into(), current: Mutex::new(Info::default()) }
    }

    /// Reads the branch information again (`Vcs.refresh`); the new current branch when it changed.
    pub async fn refresh(&self) -> Option<Option<String>> {
        let next = match self.selected {
            Some(provider) => {
                let mut info = match provider {
                    Provider::Git => git_info(&self.directory).await,
                    Provider::Hg => hg_info(&self.worktree).await,
                };
                info.provider = Some(provider);
                info
            }
            None => Info::default(),
        };
        let mut current = self.current.lock().unwrap();
        let changed = current.current != next.current;
        *current = next;
        changed.then(|| current.current.clone())
    }

    pub fn info(&self) -> Info {
        self.current.lock().unwrap().clone()
    }

    /// The review base (`Vcs.base`): `None` when the provider has none.
    pub async fn base(&self) -> Result<Option<Json>, DiffError> {
        match self.selected {
            Some(Provider::Git) => git_base(&self.directory).await,
            _ => Ok(None),
        }
    }

    pub async fn branches(&self, search: Option<&str>, limit: Option<usize>) -> Vec<String> {
        match self.selected {
            Some(Provider::Git) => git_branches(&self.directory, search, limit).await,
            _ => vec![],
        }
    }

    pub async fn status(&self) -> Vec<FileStatus> {
        let ctx = Ctx { directory: &self.directory, worktree: &self.worktree };
        match self.selected {
            Some(Provider::Git) => git_status(&ctx).await,
            Some(Provider::Hg) => hg_status(&ctx).await,
            None => vec![],
        }
    }

    pub async fn diff(&self, mode: Mode, options: DiffOptions) -> Result<Vec<FileDiff>, DiffError> {
        let ctx = Ctx { directory: &self.directory, worktree: &self.worktree };
        let options = DiffOptions { context: Some(options.context.unwrap_or(PATCH_CONTEXT_LINES)), ..options };
        let rows = match self.selected {
            Some(Provider::Git) => git_diff(&ctx, mode, &options).await?,
            Some(Provider::Hg) => hg_diff(&ctx, mode, &options).await?,
            None => return Ok(vec![]),
        };
        let mut total = 0;
        Ok(rows
            .into_iter()
            .map(|row| {
                if total + row.patch.len() > MAX_TOTAL_PATCH_BYTES {
                    return FileDiff { patch: patch::empty(&row.file), ..row };
                }
                total += row.patch.len();
                row
            })
            .collect())
    }

    /// `Vcs.initialize`: creates a repository in the project directory with the named provider.
    pub async fn initialize(&self, provider: &str) -> Result<(), InitializeError> {
        if !std::path::Path::new(&self.worktree).is_dir() {
            return Err(InitializeError::Missing);
        }
        if crate::project::up(&[".git", ".hg"], &self.worktree, None).is_some() {
            return Err(InitializeError::Conflict);
        }
        let Some(provider) = self.providers.iter().find(|p| p.id() == provider) else {
            return Err(InitializeError::Unknown);
        };
        let result = match provider {
            Provider::Git => Command::new(git_executable(), &self.worktree).args(["init"]).run().await,
            Provider::Hg => Command::new("hg", &self.worktree).args(["init"]).env("HGPLAIN", "1").run().await,
        };
        match result {
            Ok(output) if output.code == 0 => Ok(()),
            _ => Err(InitializeError::Failed),
        }
    }
}

/// `gitExecutable()` (packages/core/src/util/git-executable.ts): `git` from `PATH` on Linux.
pub fn git_executable() -> &'static str {
    "git"
}

// --- git (plugin/vcs/git.ts) ---

struct Ctx<'a> {
    directory: &'a str,
    worktree: &'a str,
}

const CFG: &[&str] = &[
    "--no-optional-locks",
    "-c",
    "core.autocrlf=false",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.longpaths=true",
    "-c",
    "core.symlinks=true",
    "-c",
    "core.quotepath=false",
];

struct Run {
    code: i32,
    text: String,
    truncated: bool,
}

/// `makeGit(...).run`: git with the adapter's config flags; a spawn failure reads as exit 1 with no output.
async fn run(cwd: &str, args: &[&str], env: &[(&str, &str)], max: Option<usize>) -> Run {
    let mut command = Command::new(git_executable(), cwd).args(CFG.iter().copied()).args(args.iter().copied()).max_output(max);
    for (key, value) in env {
        command = command.env(key, value);
    }
    match command.run().await {
        Ok(output) => Run { code: output.code, truncated: output.truncated(), text: output.text() },
        Err(_) => Run { code: 1, text: String::new(), truncated: false },
    }
}

async fn git_text(cwd: &str, args: &[&str]) -> String {
    run(cwd, args, &[], None).await.text
}

async fn git_lines(cwd: &str, args: &[&str]) -> Vec<String> {
    lines(&git_text(cwd, args).await)
}

fn kind(code: &str) -> &'static str {
    if code == "??" {
        return "added";
    }
    if code.contains('U') {
        return "modified";
    }
    if code.contains('A') && !code.contains('D') {
        return "added";
    }
    if code.contains('D') && !code.contains('A') {
        return "deleted";
    }
    "modified"
}

#[derive(Debug, Clone)]
struct Item {
    file: String,
    code: String,
    status: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct Stat {
    additions: i64,
    deletions: i64,
}

fn count(value: &str) -> i64 {
    if value == "-" {
        return 0;
    }
    text::parse_int(if value.is_empty() { "0" } else { value }).unwrap_or(0)
}

fn numstat(text: &str) -> Vec<(String, Stat)> {
    nuls(text)
        .into_iter()
        .filter_map(|item| {
            let a = item.find('\t')?;
            let b = a + 1 + item[a + 1..].find('\t')?;
            let file = &item[b + 1..];
            if file.is_empty() {
                return None;
            }
            Some((file.to_owned(), Stat { additions: count(&item[..a]), deletions: count(&item[a + 1..b]) }))
        })
        .collect()
}

fn nums(list: Vec<(String, Stat)>) -> HashMap<String, Stat> {
    // `new Map(entries)`: a later entry for the same file wins.
    list.into_iter().collect()
}

async fn configured(cwd: &str, list: &[String]) -> Option<(String, String)> {
    let result = run(cwd, &["config", "init.defaultBranch"], &[], None).await;
    let name = trim(&result.text).to_owned();
    (!name.is_empty() && list.contains(&name)).then(|| (name.clone(), name))
}

async fn primary(cwd: &str) -> Option<String> {
    let list = git_lines(cwd, &["remote"]).await;
    if list.iter().any(|r| r == "origin") {
        return Some("origin".into());
    }
    if list.len() == 1 {
        return list.into_iter().next();
    }
    if list.iter().any(|r| r == "upstream") {
        return Some("upstream".into());
    }
    list.into_iter().next()
}

pub async fn git_branch(cwd: &str) -> Option<String> {
    let result = run(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"], &[], None).await;
    if result.code != 0 {
        return None;
    }
    Some(trim(&result.text).to_owned()).filter(|b| !b.is_empty())
}

async fn git_branches(cwd: &str, search: Option<&str>, limit: Option<usize>) -> Vec<String> {
    let search = search.map(|s| trim(s).to_lowercase()).filter(|s| !s.is_empty());
    let items = git_lines(
        cwd,
        &["for-each-ref", "--sort=refname", "--sort=-committerdate", "--format=%(refname:lstrip=2)", "refs/heads", "refs/remotes"],
    )
    .await;
    let filtered = items
        .into_iter()
        .filter(|item| !item.ends_with("/HEAD") && search.as_ref().is_none_or(|s| item.to_lowercase().contains(s.as_str())));
    match limit {
        Some(limit) => filtered.take(limit).collect(),
        None => filtered.collect(),
    }
}

/// `defaultBranch`: `(name, ref)`.
async fn default_branch(cwd: &str) -> Option<(String, String)> {
    if let Some(remote) = primary(cwd).await {
        let head = run(cwd, &["symbolic-ref", &format!("refs/remotes/{remote}/HEAD")], &[], None).await;
        if head.code == 0 {
            let reference = trim(&head.text);
            let reference = reference.strip_prefix("refs/remotes/").unwrap_or(reference).to_owned();
            let name = reference.strip_prefix(&format!("{remote}/")).unwrap_or("").to_owned();
            if !name.is_empty() {
                return Some((name, reference));
            }
        }
    }
    let list = git_lines(cwd, &["for-each-ref", "--format=%(refname:lstrip=2)", "refs/heads"]).await;
    if let Some(next) = configured(cwd, &list).await {
        return Some(next);
    }
    for name in ["main", "master"] {
        if list.iter().any(|b| b == name) {
            return Some((name.into(), name.into()));
        }
    }
    None
}

async fn resolve(cwd: &str, reference: &str) -> Option<String> {
    let result = run(cwd, &["rev-parse", "--verify", "--end-of-options", &format!("{reference}^{{commit}}")], &[], None).await;
    if result.code != 0 {
        return None;
    }
    Some(trim(&result.text).to_owned()).filter(|r| !r.is_empty())
}

async fn ancestor(cwd: &str, commit: &str, reference: &str) -> bool {
    let hex = commit.len() >= 40 && commit.len() <= 64 && commit.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    hex && run(cwd, &["merge-base", "--is-ancestor", commit, reference], &[], None).await.code == 0
}

/// `namedRef`: a branch a creation hint names, `(name, ref)`.
async fn named_ref(cwd: &str, input: &str) -> Option<(String, String)> {
    // Creation hints must identify a branch, not HEAD, an object ID, or a revision expression.
    if input == "HEAD" || input.ends_with("/HEAD") || input.chars().any(|c| "~^:@{}".contains(c) || text::is_js_whitespace(c)) {
        return None;
    }
    let reference = trim(&git_text(cwd, &["rev-parse", "--symbolic-full-name", "--verify", "--end-of-options", input]).await).to_owned();
    let branch = reference.strip_prefix("refs/heads/").or_else(|| reference.strip_prefix("refs/remotes/"));
    if branch.is_none_or(str::is_empty) || reference.ends_with("/HEAD") || resolve(cwd, &reference).await.is_none() {
        return None;
    }
    let name = match reference.strip_prefix("refs/heads/") {
        Some(name) => name.to_owned(),
        None => {
            let rest = reference.strip_prefix("refs/remotes/").unwrap_or(&reference);
            // `.replace(/^refs\/remotes\/[^/]+\//, "")`
            match rest.find('/') {
                Some(index) if index > 0 => rest[index + 1..].to_owned(),
                _ => reference.clone(),
            }
        }
    };
    Some((name, reference))
}

async fn has_head(cwd: &str) -> bool {
    run(cwd, &["rev-parse", "--verify", "HEAD"], &[], None).await.code == 0
}

async fn merge_base(cwd: &str, base: &str) -> Option<String> {
    let reference = resolve(cwd, base).await?;
    let result = run(cwd, &["merge-base", &reference, "HEAD"], &[], None).await;
    if result.code != 0 {
        return None;
    }
    Some(trim(&result.text).to_owned()).filter(|r| !r.is_empty())
}

fn base_json(name: &str, reference: &str, source: &str) -> Json {
    let mut o = Obj::new();
    o.str("name", name).str("ref", reference).str("source", source);
    o.done()
}

async fn git_base(cwd: &str) -> Result<Option<Json>, DiffError> {
    if !has_head(cwd).await {
        return Ok(None);
    }
    let choose = || DiffError("Choose a review base".into());
    let Some(current) = git_branch(cwd).await else { return Err(choose()) };
    let history: Vec<(String, String)> = git_lines(
        cwd,
        &["reflog", "show", "--max-count=256", "--format=%H%x00%gs", &format!("refs/heads/{current}")],
    )
    .await
    .into_iter()
    .filter_map(|line| {
        let (commit, message) = line.split_once('\0')?;
        let hex = !commit.is_empty() && commit.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        (hex && !message.is_empty() && !message.contains(['\n', '\r', '\u{2028}', '\u{2029}']))
            .then(|| (commit.to_owned(), message.to_owned()))
    })
    .collect();
    let renamed = history.iter().any(|(_, message)| message.starts_with("Branch: renamed "));
    let creation =
        if renamed { None } else { history.iter().find(|(_, message)| message.starts_with("branch: Created from ")) };
    if let Some((commit, message)) = creation {
        let origin = &message["branch: Created from ".len()..];
        if !origin.is_empty() {
            if let Some((name, reference)) = named_ref(cwd, origin).await {
                if name != current && ancestor(cwd, commit, "HEAD").await && ancestor(cwd, commit, &reference).await {
                    return Ok(Some(base_json(&name, &reference, "reflog")));
                }
            }
        }
    }
    let Some((root_name, root_ref)) = default_branch(cwd).await.filter(|(name, _)| *name == current) else {
        return Err(choose());
    };
    let Some((_, reference)) = named_ref(cwd, &root_ref).await else {
        return Err(DiffError("The default review base is unavailable".into()));
    };
    Ok(Some(base_json(&root_name, &reference, "default")))
}

async fn git_info(cwd: &str) -> Info {
    let (current, root) = tokio::join!(git_branch(cwd), default_branch(cwd));
    Info { current, default: root.map(|(name, _)| name), provider: None }
}

/// `status` (git.ts `statusNames`): porcelain status of `.` or of the named entries.
async fn status_names(cwd: &str, untracked: &str, names: Option<&[String]>) -> Vec<Item> {
    let mut args = vec!["status".to_owned(), "--porcelain=v1".into(), format!("--untracked-files={untracked}"), "--no-renames".into(), "-z".into(), "--".into()];
    match names {
        Some(names) => args.extend(names.iter().map(|name| format!(":(literal){name}"))),
        None => args.push(".".into()),
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    nuls(&git_text(cwd, &args).await)
        .into_iter()
        .filter_map(|item| {
            let file = item.get(3..).filter(|f| !f.is_empty())?;
            let code = item.get(..2).unwrap_or(item);
            Some(Item { file: file.to_owned(), code: code.to_owned(), status: kind(code) })
        })
        .collect()
}

/// `diff` (git.ts `diffNames`).
async fn diff_names(cwd: &str, reference: &str, target: Option<&str>) -> Result<Vec<Item>, DiffError> {
    let mut args = vec!["diff", "--no-ext-diff", "--no-renames", "--name-status", "-z", reference];
    args.extend(target);
    args.extend(["--", "."]);
    let result = run(cwd, &args, &[], None).await;
    if result.code != 0 {
        return Err(DiffError("Unable to list Git changes".into()));
    }
    let list = nuls(&result.text);
    Ok(list
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 2 == 0)
        .filter_map(|(index, code)| {
            let file = list.get(index + 1)?;
            Some(Item { file: (*file).to_owned(), code: (*code).to_owned(), status: kind(code) })
        })
        .collect())
}

async fn stats(cwd: &str, reference: &str, target: Option<&str>) -> Vec<(String, Stat)> {
    let mut args = vec!["diff", "--no-ext-diff", "--no-renames", "--numstat", "-z", reference];
    args.extend(target);
    args.extend(["--", "."]);
    numstat(&git_text(cwd, &args).await)
}

struct Patch {
    text: String,
    truncated: bool,
}

struct PatchOptions<'a> {
    context: i64,
    max: Option<usize>,
    target: Option<&'a str>,
}

async fn diff_patch(cwd: &str, args: &[&str], options: &PatchOptions<'_>, drop_truncated: bool, no_index: bool, env: &[(&str, &str)]) -> Patch {
    let unified = format!("--unified={}", options.context);
    let mut all = vec!["diff"];
    if no_index {
        all.push("--no-index");
    }
    all.extend(["--patch", "--no-ext-diff", "--no-renames", &unified]);
    all.extend(args);
    let result = run(cwd, &all, env, options.max).await;
    Patch { text: if drop_truncated && result.truncated { String::new() } else { result.text }, truncated: result.truncated }
}

async fn patch_one(cwd: &str, reference: &str, file: &str, options: &PatchOptions<'_>) -> Patch {
    let mut args = vec![reference];
    args.extend(options.target);
    args.extend(["--", file]);
    diff_patch(cwd, &args, options, true, false, &[]).await
}

async fn patch_all(cwd: &str, reference: &str, options: &PatchOptions<'_>) -> Patch {
    let mut args = vec![reference];
    args.extend(options.target);
    args.extend(["--", "."]);
    diff_patch(cwd, &args, options, false, false, &[]).await
}

async fn patch_untracked(cwd: &str, file: &str, options: &PatchOptions<'_>) -> Patch {
    // A folder entry has no patch of its own.
    if file.ends_with('/') {
        return Patch { text: String::new(), truncated: false };
    }
    diff_patch(cwd, &["--", "/dev/null", file], options, true, true, &[]).await
}

async fn stat_untracked(cwd: &str, file: &str) -> Option<Stat> {
    if file.ends_with('/') {
        return None;
    }
    let result = run(cwd, &["diff", "--no-index", "--numstat", "--", "/dev/null", file], &[], Some(4096)).await;
    if result.truncated {
        return None;
    }
    let parts: Vec<&str> = result.text.split('\t').collect();
    if parts.len() < 2 {
        return None;
    }
    Some(Stat { additions: count(parts[0]), deletions: count(parts[1]) })
}

#[derive(Default)]
struct UntrackedBatch {
    stats: HashMap<String, Stat>,
    patches: HashMap<String, String>,
}

/// `untrackedBatch`: what `stat_untracked` and `patch_untracked` report for many files, in three git processes: the
/// files are recorded intent-to-add in a throwaway index, so a plain `git diff` against it shows them as new.
async fn untracked_batch(ctx: &Ctx<'_>, files: &[String], patches: Option<&DiffOptions>) -> UntrackedBatch {
    let eligible: Vec<&String> = files
        .iter()
        .filter(|file| {
            // Directories (nested repositories) and symlinks diff differently through an index.
            if file.ends_with('/') {
                return false;
            }
            let Ok(stat) = std::fs::symlink_metadata(std::path::Path::new(ctx.worktree).join(file)) else { return false };
            // A file this large could crowd every other file out of the batch's output budget.
            stat.is_file() && stat.len() as usize <= MAX_TOTAL_PATCH_BYTES / 2
        })
        .collect();
    // The batch costs up to three processes; for a single file the per-file commands are cheaper.
    if eligible.len() < 2 {
        return UntrackedBatch::default();
    }
    let base = std::env::temp_dir().join(format!("opencode-untracked-{}", crate::id::uuid())).to_string_lossy().into_owned();
    let list = format!("{base}.list");
    let index = format!("{base}.index");
    let body: String = eligible.iter().map(|file| format!("{file}\0")).collect();
    let result = if std::fs::write(&list, body).is_ok() {
        let env = [("GIT_INDEX_FILE", index.as_str())];
        let pathspec = format!("--pathspec-from-file={list}");
        run(
            ctx.worktree,
            &["--literal-pathspecs", "-c", "core.splitIndex=false", "add", "--intent-to-add", "--force", "--sparse", "--ignore-errors", &pathspec, "--pathspec-file-nul"],
            &env,
            None,
        )
        .await;
        let options = PatchOptions {
            context: patches.and_then(|p| p.context).unwrap_or(PATCH_CONTEXT_LINES),
            max: Some(MAX_TOTAL_PATCH_BYTES),
            target: None,
        };
        let (stats, patch) = tokio::join!(
            run(ctx.worktree, &["diff", "--no-ext-diff", "--no-renames", "--numstat", "-z"], &env, None),
            async {
                match patches {
                    Some(_) => diff_patch(ctx.worktree, &[], &options, false, false, &env).await,
                    None => Patch { text: String::new(), truncated: false },
                }
            }
        );
        let wanted: HashSet<&str> = eligible.iter().map(|f| f.as_str()).collect();
        let headers: HashMap<String, &String> = eligible.iter().map(|file| (format!("diff --git a/{file} b/{file}"), *file)).collect();
        UntrackedBatch {
            stats: numstat(&stats.text).into_iter().filter(|(file, _)| wanted.contains(file.as_str())).collect(),
            // Only chunks with the default `a/` and `b/` prefixes match the per-file output; configured prefixes and
            // quoted paths fall back to it.
            patches: patch::split_git(&patch.text)
                .into_iter()
                .filter_map(|chunk| {
                    let header = chunk.split('\n').next().unwrap_or(chunk);
                    headers.get(header).map(|file| ((*file).clone(), chunk.to_owned()))
                })
                .collect(),
        }
    } else {
        UntrackedBatch::default()
    };
    for file in [&list, &index, &format!("{index}.lock")] {
        let _ = std::fs::remove_file(file);
    }
    result
}

/// Entries a folder listing shows, and checks per git call.
const MAX_FOLDER_ENTRIES: usize = 500;
/// Entries, and time, a folder listing spends checking before it stops and reports that more were not checked.
const MAX_FOLDER_SCAN: usize = 5_000;
const MAX_FOLDER_SCAN_MS: u128 = 1_000;

/// `statusItems`: untracked files one by one unless the caller asks for folders, then the folder marked `more` when
/// a folder listing stopped early.
async fn status_items(ctx: &Ctx<'_>, untracked: Untracked) -> (Vec<Item>, Option<String>) {
    if untracked == Untracked::Files {
        return (status_names(ctx.directory, "all", None).await, None);
    }
    let list = status_names(ctx.directory, "normal", None).await;
    let prefix = text::relative(ctx.worktree, ctx.directory);
    let inside_untracked =
        list.iter().any(|item| item.code == "??" && item.file.ends_with('/') && format!("{prefix}/").starts_with(&item.file));
    if prefix.is_empty() || !inside_untracked {
        return (list, None);
    }
    let mut names: Vec<String> = std::fs::read_dir(ctx.directory)
        .map(|entries| entries.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    // Ignored entries do not list, so check entries a batch at a time until the listing fills, the folder runs out, or
    // the scan budget does.
    let mut entries = vec![];
    let started = std::time::Instant::now();
    let mut checked = 0;
    while checked < names.len()
        && entries.len() < MAX_FOLDER_ENTRIES
        && checked < MAX_FOLDER_SCAN
        && started.elapsed().as_millis() < MAX_FOLDER_SCAN_MS
    {
        let batch = &names[checked..(checked + MAX_FOLDER_ENTRIES).min(names.len())];
        checked += batch.len();
        entries.extend(status_names(ctx.directory, "normal", Some(batch)).await);
    }
    let more = (entries.len() > MAX_FOLDER_ENTRIES || checked < names.len()).then(|| format!("{prefix}/"));
    entries.truncate(MAX_FOLDER_ENTRIES);
    (entries, more)
}

/// A folder listing that stopped early ends with an entry for the folder itself, marked `more`.
fn with_more(mut rows: Vec<FileDiff>, folder: Option<String>) -> Vec<FileDiff> {
    if let Some(file) = folder {
        rows.push(FileDiff { file, patch: String::new(), additions: 0, deletions: 0, status: "added", more: true });
    }
    rows
}

fn merge(lists: Vec<Vec<Item>>) -> Vec<Item> {
    let mut seen = HashSet::new();
    lists.into_iter().flatten().filter(|item| seen.insert(item.file.clone())).collect()
}

#[derive(Default)]
struct Batch {
    patches: HashMap<String, String>,
    capped: bool,
}

async fn batch_patches(ctx: &Ctx<'_>, reference: &str, list: &[Item], options: &DiffOptions, target: Option<&str>) -> Batch {
    if list.is_empty() {
        return Batch::default();
    }
    let options = PatchOptions { context: options.context.unwrap_or(PATCH_CONTEXT_LINES), max: Some(MAX_TOTAL_PATCH_BYTES), target };
    let result = patch_all(ctx.directory, reference, &options).await;
    Batch { patches: patch::chunks_by_file(&result.text, result.truncated, |index| list.get(index).map(|i| i.file.clone())), capped: result.truncated }
}

async fn native_patch(ctx: &Ctx<'_>, reference: Option<&str>, item: &Item, options: &DiffOptions, target: Option<&str>) -> String {
    let context = options.context.unwrap_or(PATCH_CONTEXT_LINES);
    let result = match reference {
        Some(reference) if item.code != "??" => {
            patch_one(ctx.worktree, reference, &item.file, &PatchOptions { context, max: Some(MAX_PATCH_BYTES), target }).await
        }
        _ => patch_untracked(ctx.worktree, &item.file, &PatchOptions { context, max: Some(MAX_PATCH_BYTES), target: None }).await,
    };
    if !result.truncated && !result.text.is_empty() {
        return result.text;
    }
    patch::empty(&item.file)
}

fn sort_items(list: &mut [Item]) {
    list.sort_by(|a, b| collate::compare(&a.file, &b.file));
}

/// `files`: the rows of a review, with stats and patches within the byte budgets.
async fn files(
    ctx: &Ctx<'_>,
    reference: Option<&str>,
    mut list: Vec<Item>,
    map: &HashMap<String, Stat>,
    batch: &Batch,
    options: &DiffOptions,
    target: Option<&str>,
) -> Vec<FileDiff> {
    let candidates: Vec<String> = list
        .iter()
        .filter(|item| item.code == "??" || reference.is_none() || (target.is_none() && item.status == "added" && !map.contains_key(&item.file)))
        .map(|item| item.file.clone())
        .collect();
    let untracked = untracked_batch(ctx, &candidates, Some(options)).await;
    let mut next = Vec::with_capacity(list.len());
    let mut total = 0usize;
    let mut capped = false;
    sort_items(&mut list);
    for item in list {
        let stat = match map.get(&item.file) {
            Some(stat) => Some(*stat),
            None if target.is_none() && item.status == "added" => match untracked.stats.get(&item.file) {
                Some(stat) => Some(*stat),
                None => stat_untracked(ctx.worktree, &item.file).await,
            },
            None => None,
        };
        let patch = if capped {
            patch::empty(&item.file)
        } else if let Some(batched) = batch.patches.get(&item.file) {
            batched.clone()
        } else if item.code != "??" && batch.capped {
            patch::empty(&item.file)
        } else if let Some(prefetched) = (item.code == "??" || reference.is_none()).then(|| untracked.patches.get(&item.file)).flatten() {
            prefetched.clone()
        } else {
            native_patch(ctx, reference, &item, options, target).await
        };
        let (patch, was_capped) = if capped {
            (patch, true)
        } else if total + patch.len() <= MAX_TOTAL_PATCH_BYTES {
            (patch, false)
        } else {
            (patch::empty(&item.file), true)
        };
        capped = capped || was_capped;
        if !capped {
            total += patch.len();
            capped = total >= MAX_TOTAL_PATCH_BYTES;
        }
        next.push(FileDiff {
            file: item.file,
            patch,
            additions: stat.map(|s| s.additions).unwrap_or(0),
            deletions: stat.map(|s| s.deletions).unwrap_or(0),
            status: item.status,
            more: false,
        });
    }
    next
}

async fn diff_against_ref(ctx: &Ctx<'_>, reference: &str, options: &DiffOptions, target: Option<&str>) -> Result<Vec<FileDiff>, DiffError> {
    let (list, stats, extra) = tokio::join!(
        diff_names(ctx.directory, reference, target),
        stats(ctx.directory, reference, target),
        async {
            match target {
                Some(_) => (vec![], None),
                None => status_items(ctx, options.untracked.unwrap_or(Untracked::Files)).await,
            }
        }
    );
    let list = list?;
    let batch = batch_patches(ctx, reference, &list, options, target).await;
    let untracked: Vec<Item> = extra.0.into_iter().filter(|item| item.code == "??").collect();
    let rows = files(ctx, Some(reference), merge(vec![list, untracked]), &nums(stats), &batch, options, target).await;
    Ok(with_more(rows, extra.1))
}

async fn track(ctx: &Ctx<'_>, reference: Option<&str>, options: &DiffOptions) -> Result<Vec<FileDiff>, DiffError> {
    match reference {
        None => {
            let (list, more) = status_items(ctx, options.untracked.unwrap_or(Untracked::Files)).await;
            Ok(with_more(files(ctx, None, list, &HashMap::new(), &Batch::default(), options, None).await, more))
        }
        Some(reference) => diff_against_ref(ctx, reference, options, None).await,
    }
}

async fn git_status(ctx: &Ctx<'_>) -> Vec<FileStatus> {
    let head = has_head(ctx.directory).await;
    let (list, stats) = tokio::join!(status_items(ctx, Untracked::Files), async {
        if head { stats(ctx.directory, "HEAD", None).await } else { vec![] }
    });
    let mut list = list.0;
    let map = nums(stats);
    let candidates: Vec<String> =
        list.iter().filter(|item| item.status == "added" && !map.contains_key(&item.file)).map(|item| item.file.clone()).collect();
    let untracked = untracked_batch(ctx, &candidates, None).await;
    sort_items(&mut list);
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let stat = match map.get(&item.file) {
            Some(stat) => Some(*stat),
            None if item.status == "added" => match untracked.stats.get(&item.file) {
                Some(stat) => Some(*stat),
                None => stat_untracked(ctx.worktree, &item.file).await,
            },
            None => None,
        };
        out.push(FileStatus {
            file: item.file,
            additions: stat.map(|s| s.additions).unwrap_or(0),
            deletions: stat.map(|s| s.deletions).unwrap_or(0),
            status: item.status,
        });
    }
    out
}

async fn git_diff(ctx: &Ctx<'_>, mode: Mode, options: &DiffOptions) -> Result<Vec<FileDiff>, DiffError> {
    if mode == Mode::Working {
        let head = has_head(ctx.directory).await;
        return track(ctx, head.then_some("HEAD"), options).await;
    }
    if !has_head(ctx.directory).await {
        return if mode == Mode::Committed { Ok(vec![]) } else { track(ctx, None, options).await };
    }
    let base = match &options.base {
        Some(base) => Some(base.clone()),
        None => default_branch(ctx.directory).await.map(|(_, reference)| reference),
    };
    let reference = match &base {
        Some(base) => merge_base(ctx.directory, base).await,
        None => None,
    };
    let Some(reference) = reference else {
        return Err(DiffError(match base {
            Some(base) => format!("No merge base available for {base}"),
            None => "No review base available".into(),
        }));
    };
    diff_against_ref(ctx, &reference, options, (mode == Mode::Committed).then_some("HEAD")).await
}

// --- Mercurial (plugin/vcs/hg.ts) ---

async fn hg_run(worktree: &str, args: &[&str], max: Option<usize>) -> Run {
    match Command::new("hg", worktree).args(args.iter().copied()).env("HGPLAIN", "1").max_output(max).run().await {
        Ok(output) => Run { code: output.code, truncated: output.truncated(), text: output.text() },
        Err(_) => Run { code: 1, text: String::new(), truncated: false },
    }
}

async fn hg_branch(worktree: &str) -> Option<String> {
    let result = hg_run(worktree, &["branch"], None).await;
    if result.code != 0 {
        return None;
    }
    Some(trim(&result.text).to_owned()).filter(|b| !b.is_empty())
}

async fn hg_info(worktree: &str) -> Info {
    Info { current: hg_branch(worktree).await, default: Some("default".into()), provider: None }
}

fn hg_kind(code: &str) -> &'static str {
    match code {
        "A" | "?" => "added",
        "R" | "!" => "deleted",
        _ => "modified",
    }
}

async fn hg_status_names(worktree: &str, rev: Option<&str>, scope: &str) -> Vec<Item> {
    let mut args = vec!["status", "-0"];
    if let Some(rev) = rev {
        args.extend(["--rev", rev]);
    }
    args.push(scope);
    let result = hg_run(worktree, &args, None).await;
    if result.code != 0 {
        return vec![];
    }
    nuls(&result.text)
        .into_iter()
        .filter_map(|entry| {
            let code = entry.get(..1)?;
            let file = entry.get(2..).filter(|f| !f.is_empty())?;
            Some(Item { file: file.to_owned(), code: code.to_owned(), status: hg_kind(code) })
        })
        .collect()
}

async fn hg_diff_against(ctx: &Ctx<'_>, rev: Option<&str>, context: i64) -> Vec<FileDiff> {
    let scope = Some(text::relative(ctx.worktree, ctx.directory)).filter(|s| !s.is_empty()).unwrap_or_else(|| ".".into());
    let context = context.to_string();
    let mut diff_args = vec!["diff", "--git", "--unified", &context];
    if let Some(rev) = rev {
        diff_args.extend(["-r", rev]);
    }
    diff_args.push(&scope);
    let (mut items, batch) =
        tokio::join!(hg_status_names(ctx.worktree, rev, &scope), hg_run(ctx.worktree, &diff_args, Some(MAX_TOTAL_PATCH_BYTES)));
    let (text, truncated) = if batch.code != 0 { (String::new(), false) } else { (batch.text, batch.truncated) };
    let chunks = patch::chunks_by_file(&text, truncated, |_| None);
    sort_items(&mut items);
    let mut list = vec![];
    for item in items {
        let patch = match chunks.get(&item.file) {
            Some(chunk) => chunk.clone(),
            None if item.code == "?" => match std::fs::read(std::path::Path::new(ctx.worktree).join(&item.file)) {
                Ok(bytes) if bytes.len() <= MAX_PATCH_BYTES => patch::add(&item.file, &String::from_utf8_lossy(&bytes)),
                _ => patch::empty(&item.file),
            },
            None if item.code == "!" => {
                let result = hg_run(ctx.worktree, &["cat", "-r", rev.unwrap_or("."), "--", &item.file], Some(MAX_PATCH_BYTES)).await;
                if result.code != 0 || result.truncated || result.text.len() > MAX_PATCH_BYTES {
                    patch::empty(&item.file)
                } else {
                    patch::delete(&item.file, &result.text)
                }
            }
            None => patch::empty(&item.file),
        };
        let (additions, deletions) = patch::count(&patch);
        list.push(FileDiff { file: item.file, patch, additions, deletions, status: item.status, more: false });
    }
    list
}

async fn hg_status(ctx: &Ctx<'_>) -> Vec<FileStatus> {
    hg_diff_against(ctx, None, 0)
        .await
        .into_iter()
        .map(|row| FileStatus { file: row.file, additions: row.additions, deletions: row.deletions, status: row.status })
        .collect()
}

async fn hg_diff(ctx: &Ctx<'_>, mode: Mode, options: &DiffOptions) -> Result<Vec<FileDiff>, DiffError> {
    let context = options.context.unwrap_or(PATCH_CONTEXT_LINES);
    if mode == Mode::Working {
        return Ok(hg_diff_against(ctx, None, context).await);
    }
    if mode == Mode::Committed || options.base.is_some() {
        return Err(DiffError("The Mercurial provider does not support committed reviews or explicit bases".into()));
    }
    let branch = hg_branch(ctx.worktree).await;
    if branch.as_deref().is_none_or(|b| b == "default") {
        return Ok(vec![]);
    }
    let result = hg_run(ctx.worktree, &["log", "-r", "ancestor(., default)", "-T", "{node}"], None).await;
    let ancestor = (result.code == 0).then(|| trim(&result.text).to_owned()).filter(|a| !a.is_empty());
    match ancestor {
        Some(ancestor) => Ok(hg_diff_against(ctx, Some(&ancestor), context).await),
        None => Ok(vec![]),
    }
}

/// Patch text helpers (packages/core/src/vcs/patch.ts); `add`, `delete` and `empty` write what jsdiff's
/// `formatPatch(structuredPatch(...))` writes for them.
pub mod patch {
    use std::collections::HashMap;

    fn header(file: &str) -> String {
        format!("Index: {file}\n===================================================================\n--- {file}\t\n+++ {file}\t\n")
    }

    fn lines(content: &str, sign: char) -> (usize, String) {
        let mut parts: Vec<&str> = content.split('\n').collect();
        let newline = content.ends_with('\n');
        if newline {
            parts.pop();
        }
        let mut out = String::new();
        for part in &parts {
            out.push(sign);
            out.push_str(part);
            out.push('\n');
        }
        if !newline {
            out.push_str("\\ No newline at end of file\n");
        }
        (parts.len(), out)
    }

    pub fn empty(file: &str) -> String {
        header(file)
    }

    pub fn add(file: &str, content: &str) -> String {
        if content.is_empty() {
            return header(file);
        }
        let (count, body) = lines(content, '+');
        format!("{}@@ -0,0 +1,{count} @@\n{body}", header(file))
    }

    pub fn delete(file: &str, content: &str) -> String {
        if content.is_empty() {
            return header(file);
        }
        let (count, body) = lines(content, '-');
        format!("{}@@ -1,{count} +0,0 @@\n{body}", header(file))
    }

    /// Changed lines in a unified diff, ignoring `+++`/`---` file headers.
    pub fn count(patch: &str) -> (i64, i64) {
        let (mut additions, mut deletions) = (0, 0);
        for line in patch.split('\n') {
            if line.starts_with('+') && !line.starts_with("+++") {
                additions += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                deletions += 1;
            }
        }
        (additions, deletions)
    }

    fn parse_quoted(value: &str) -> Option<(String, usize)> {
        let chars: Vec<(usize, char)> = value.char_indices().collect();
        let mut out = String::new();
        let mut index = 1;
        while index < chars.len() {
            let (offset, c) = chars[index];
            if c == '"' {
                return Some((out, offset + 1));
            }
            if c != '\\' {
                out.push(c);
                index += 1;
                continue;
            }
            index += 1;
            match chars.get(index).map(|(_, c)| *c) {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => {}
            }
            index += 1;
        }
        None
    }

    fn path_token(value: &str) -> String {
        if !value.starts_with('"') {
            return value.split('\t').next().unwrap_or("").to_owned();
        }
        parse_quoted(value).map(|(v, _)| v).unwrap_or_else(|| value.to_owned())
    }

    fn from_diff_path(value: Option<&str>) -> Option<String> {
        let value = value.filter(|v| !v.is_empty() && *v != "/dev/null")?;
        let file = path_token(value);
        if file.starts_with("a/") || file.starts_with("b/") {
            return Some(file[2..].to_owned());
        }
        Some(file).filter(|f| !f.is_empty())
    }

    fn from_git_header(header: &str) -> Option<String> {
        if header.starts_with('"') {
            let (_, end) = parse_quoted(header)?;
            let second = header[end..].trim_start_matches(crate::text::is_js_whitespace);
            if second.is_empty() {
                return None;
            }
            if !second.starts_with('"') {
                return from_diff_path(Some(second));
            }
            return from_diff_path(parse_quoted(second).map(|(v, _)| v).as_deref());
        }
        let separator = header.find(" b/")?;
        from_diff_path(Some(&header[separator + 1..]))
    }

    /// The value of the first line starting with `prefix` (`/^<prefix>(.+)$/m`: JS line terminators delimit lines).
    fn line_value<'a>(chunk: &'a str, prefix: &str) -> Option<&'a str> {
        chunk
            .split(['\n', '\r', '\u{2028}', '\u{2029}'])
            .find_map(|line| line.strip_prefix(prefix).filter(|rest| !rest.is_empty()))
    }

    pub fn file_from_chunk(chunk: &str) -> Option<String> {
        let next = line_value(chunk, "+++ ");
        let before = line_value(chunk, "--- ");
        if let Some(file) = from_diff_path(next).or_else(|| from_diff_path(before)) {
            return Some(file);
        }
        from_git_header(line_value(chunk, "diff --git ").unwrap_or(""))
    }

    /// Splits `git diff`-format output into per-file chunks, dropping a trailing partial chunk when truncated.
    pub fn split_git(text: &str) -> Vec<&str> {
        let mut starts = vec![];
        if text.starts_with("diff --git ") {
            starts.push(0);
        }
        let mut from = 0;
        while let Some(found) = text[from..].find("\ndiff --git ") {
            starts.push(from + found + 1);
            from += found + 1;
        }
        starts.iter().enumerate().map(|(index, start)| &text[*start..starts.get(index + 1).copied().unwrap_or(text.len())]).collect()
    }

    /// Per-file chunks by the file they touch; chunks for the same file are concatenated.
    pub fn chunks_by_file(text: &str, truncated: bool, fallback: impl Fn(usize) -> Option<String>) -> HashMap<String, String> {
        let mut chunks = split_git(text);
        if truncated {
            chunks.pop();
        }
        let mut out: HashMap<String, String> = HashMap::new();
        for (index, chunk) in chunks.into_iter().enumerate() {
            let Some(file) = file_from_chunk(chunk).or_else(|| fallback(index)) else { continue };
            out.entry(file).or_default().push_str(chunk);
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn jsdiff_shapes() {
            assert_eq!(empty("a b"), "Index: a b\n===================================================================\n--- a b\t\n+++ a b\t\n");
            assert_eq!(
                add("f", "one"),
                "Index: f\n===================================================================\n--- f\t\n+++ f\t\n@@ -0,0 +1,1 @@\n+one\n\\ No newline at end of file\n"
            );
            assert_eq!(
                delete("f", "\n\n"),
                "Index: f\n===================================================================\n--- f\t\n+++ f\t\n@@ -1,2 +0,0 @@\n-\n-\n"
            );
        }

        #[test]
        fn chunk_files() {
            let text = "diff --git \"a/tab\\tname.txt\" \"b/tab\\tname.txt\"\nnew file mode 100644\n--- /dev/null\n+++ \"b/tab\\tname.txt\"\n@@ -0,0 +1 @@\n+x\ndiff --git a/with space.txt b/with space.txt\n--- a/with space.txt\t\n+++ b/with space.txt\t\n";
            let chunks = chunks_by_file(text, false, |_| None);
            assert!(chunks.contains_key("tab\tname.txt"));
            assert!(chunks.contains_key("with space.txt"));
            let binary = "diff --git a/blob.bin b/blob.bin\nnew file mode 100644\nBinary files /dev/null and b/blob.bin differ\n";
            assert_eq!(file_from_chunk(binary).as_deref(), Some("blob.bin"));
        }
    }
}
