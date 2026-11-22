//! `cargo xtask reference`: the TS reference worktree (port/scripts/reference.sh).
//!
//! The reference is a sparse git worktree of this checkout at HEAD, without `port/`, whose commits are the
//! deviation patches in port/patches. `port-base` records the upstream commit it was built from and
//! `port-exported` records the patches last exported, so the commands can tell hand-made commits from
//! exported ones. Everything here mirrors the shell script's ordering, messages and safety checks.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::process::{self, Ctx, Error, Result, TempDir};

const PROGRAM: &str = "cargo xtask reference";
const GIT_IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "opencode-port"),
    ("GIT_AUTHOR_EMAIL", "port@localhost"),
    ("GIT_COMMITTER_NAME", "opencode-port"),
    ("GIT_COMMITTER_EMAIL", "port@localhost"),
];

pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let reference = Reference {
        ctx,
        path: std::env::var_os("REFERENCE")
            .map(PathBuf::from)
            .unwrap_or_else(|| ctx.path("port/.reference")),
        patches: ctx.path("port/patches"),
    };
    match args.first().map(String::as_str) {
        Some("build") => reference.build(),
        Some("save") => reference.save(&args[1..]),
        Some("amend") => reference.amend(&args[1..]),
        Some("export") => reference.export(),
        Some("status") => reference.status(),
        _ => reference.usage(),
    }
}

struct Reference<'a> {
    ctx: &'a Ctx,
    path: PathBuf,
    patches: PathBuf,
}

impl Reference<'_> {
    fn usage(&self) -> Result<()> {
        let text = "The TS reference that the conformance oracle records: upstream (this fork's HEAD; upstream paths are never
modified here) plus the deviation patches in port/patches/, which fix upstream bugs the Rust port must not
reproduce (see port/conformance/DEVIATIONS.md). It is materialized as a sparse git worktree without port/,
with its own `bun install` (seconds: bun hardlinks its cache), and the conformance drivers load it through
port/conformance/ref (set by link-deps.sh).

usage: port/scripts/reference.sh build                 (re)create it from HEAD + port/patches/*.patch
       port/scripts/reference.sh save \"D123 area: fix\" [body]   commit the reference's uncommitted changes
                                                        as a new deviation patch (exported to port/patches/);
                                                        the body is the deviation record (see patches/README.md)
       port/scripts/reference.sh amend D123            fold the uncommitted changes into D123's patch
       port/scripts/reference.sh export                rewrite port/patches/ from the reference's commits
       port/scripts/reference.sh status                is it current? what is uncommitted?
Patches apply in file-name order. A patch that stops applying after an upstream merge is the signal to
re-check the bug against the new upstream code: re-derive the fix, or drop it if upstream fixed it.
REFERENCE=<dir> overrides the location (default port/.reference).";
        println!("{}", text.replace("port/scripts/reference.sh", PROGRAM));
        Err(Error::silent(1))
    }

    fn git(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command.arg("-C").arg(&self.path).args(args);
        for (key, value) in GIT_IDENTITY {
            command.env(key, value);
        }
        command
    }

    fn git_output(&self, args: &[&str]) -> Result<String> {
        let output = self.git(args).output()?;
        if !output.status.success() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            return Err(Error::silent(1));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned())
    }

    fn git_status(&self, args: &[&str]) -> Result<std::process::ExitStatus> {
        Ok(self.git(args).status()?)
    }

    fn root_git(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command.current_dir(&self.ctx.root).args(args);
        command
    }

    fn root_git_output(&self, args: &[&str]) -> Result<String> {
        let output = self.root_git(args).output()?;
        if !output.status.success() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            return Err(Error::silent(1));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned())
    }

    fn fail(&self, message: impl std::fmt::Display) -> Result<()> {
        eprintln!("{message}");
        Err(Error::silent(1))
    }

    fn base_file(&self) -> Result<PathBuf> {
        let git_dir = self.git_output(&["rev-parse", "--absolute-git-dir"])?;
        Ok(PathBuf::from(git_dir).join("port-base"))
    }

    fn base(&self) -> Result<String> {
        Ok(process::read(self.base_file()?)?.trim().to_owned())
    }

    fn snapshot(&self) -> Result<PathBuf> {
        let git_dir = self.git_output(&["rev-parse", "--absolute-git-dir"])?;
        Ok(PathBuf::from(git_dir).join("port-exported"))
    }

    fn need(&self) -> Result<()> {
        if !self.path.join(".git").is_file() || !self.base_file()?.is_file() {
            return self.fail(format!(
                "no reference at {}: run {PROGRAM} build",
                self.path.display()
            ));
        }
        Ok(())
    }

    fn remember(&self) -> Result<()> {
        let snapshot = self.snapshot()?;
        process::remove_path(&snapshot)?;
        self.export_patches(&snapshot)
    }

    /// Writes `<dir>/D<id>-<slug>.patch` for every commit in `port-base..HEAD`, in commit order.
    fn export_patches(&self, directory: &Path) -> Result<()> {
        fs::create_dir_all(directory)?;
        for path in d_patches(directory)? {
            let _ = fs::remove_file(path);
        }
        let base = self.base()?;
        let log = self.git_output(&["rev-list", "--reverse", &format!("{base}..HEAD")])?;
        for sha in log.lines().filter(|line| !line.is_empty()) {
            let subject = self.git_output(&["log", "-1", "--format=%s", sha])?;
            let id = subject.split(' ').next().unwrap_or("");
            if !is_deviation_id(id) {
                return self.fail(format!(
                    "commit {sha}: subject must start with a deviation ID (D123): {subject}"
                ));
            }
            let slug = slugify(&subject);
            let file = directory.join(format!("{id}-{slug}.patch"));
            let status = self
                .git(&[
                    "format-patch",
                    "-1",
                    sha,
                    "--zero-commit",
                    "--no-signature",
                    "--no-numbered",
                    "--no-stat",
                    "--stdout",
                ])
                .stdout(Stdio::from(fs::File::create(&file)?))
                .status()?;
            if !status.success() {
                return Err(Error::silent(1));
            }
        }
        Ok(())
    }

    fn build(&self) -> Result<()> {
        if self.path.join(".git").is_file() {
            let status = self.git_output(&["status", "--porcelain"])?;
            if !status.is_empty() {
                eprintln!(
                    "the reference has uncommitted changes: save, amend or discard them first"
                );
                if let Ok(short) = self.git_output(&["status", "--short"])
                    && !short.is_empty()
                {
                    eprintln!("{short}");
                }
                return Err(Error::silent(1));
            }
            let base_file = self.base_file()?;
            if base_file.is_file() {
                let snapshot = self.snapshot()?;
                if snapshot.is_dir() {
                    let temporary = TempDir::new("opencode-reference-export")?;
                    self.export_patches(temporary.path())?;
                    if !diff_quiet(temporary.path(), &snapshot, None)? {
                        return self.fail(format!(
                            "the reference has commits made by hand, not saved to port/patches: run {PROGRAM} export first"
                        ));
                    }
                }
            }
        } else {
            process::check(
                {
                    let mut command =
                        self.root_git(&["worktree", "add", "-q", "--no-checkout", "--detach"]);
                    command.arg(&self.path).arg("HEAD");
                    command
                },
                "git worktree add",
            )?;
            process::check(
                self.git(&["sparse-checkout", "set", "--no-cone", "/*", "!/port/"]),
                "git sparse-checkout set",
            )?;
        }
        let head = self.root_git_output(&["rev-parse", "HEAD"])?;
        process::check(
            self.git(&["checkout", "-q", "--force", "--detach", &head]),
            "git checkout",
        )?;
        process::write(self.base_file()?, format!("{head}\n"))?;

        for patch in d_patches(&self.patches)? {
            let status = self.git(&["am", "-q", "--3way"]).arg(&patch).status()?;
            if !status.success() {
                let _ = self.git_status(&["am", "--abort"]);
                let name = patch
                    .file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default();
                let short = self.root_git_output(&["rev-parse", "--short", "HEAD"])?;
                return self.fail(format!(
                    "{name} no longer applies to upstream {short}: re-check the bug, then re-derive the fix or drop the patch"
                ));
            }
        }
        self.remember()?;

        let mut install = Command::new("bun");
        install
            .current_dir(&self.path)
            .args(["install", "--frozen-lockfile"])
            .stdout(Stdio::null());
        process::check(install, "bun install --frozen-lockfile")?;
        crate::scripts::link_deps(self.ctx, Some(self.path.clone()))?;

        let short = self.root_git_output(&["rev-parse", "--short", "HEAD"])?;
        let count =
            self.git_output(&["rev-list", "--count", &format!("{}..HEAD", self.base()?)])?;
        println!("reference: upstream {short} + {count} deviation patches");
        Ok(())
    }

    fn save(&self, args: &[String]) -> Result<()> {
        self.need()?;
        let Some(subject) = args.first() else {
            return self.fail(format!(
                "usage: {PROGRAM} save \"D123 area: what is fixed\""
            ));
        };
        let Some(id) = subject.split(' ').next() else {
            return self.fail(format!(
                "usage: {PROGRAM} save \"D123 area: what is fixed\""
            ));
        };
        if !is_deviation_id(id) || !subject.starts_with(&format!("{id} ")) {
            return self.fail(format!(
                "usage: {PROGRAM} save \"D123 area: what is fixed\""
            ));
        }
        let base = self.base()?;
        let subjects = self.git_output(&["log", "--format=%s", &format!("{base}..HEAD")])?;
        if subjects
            .lines()
            .any(|line| line.starts_with(&format!("{id} ")))
        {
            return self.fail(format!("{id} exists: use amend"));
        }
        if self.git_output(&["status", "--porcelain"])?.is_empty() {
            return self.fail("nothing to save");
        }
        process::check(self.git(&["add", "-A"]), "git add")?;
        let mut commit = self.git(&["commit", "-q", "-m", subject]);
        if let Some(body) = args.get(1).filter(|body| !body.is_empty()) {
            commit.args(["-m", body]);
        }
        process::check(commit, "git commit")?;
        self.export_patches(&self.patches)?;
        self.remember()?;
        let saved = d_patches(&self.patches)?
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&format!("{id}-")))
            })
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        println!("saved {saved}");
        Ok(())
    }

    fn amend(&self, args: &[String]) -> Result<()> {
        self.need()?;
        let id = args.first().map(String::as_str).unwrap_or("");
        let base = self.base()?;
        let log = self.git_output(&["log", "--format=%H %s", &format!("{base}..HEAD")])?;
        let sha = log
            .lines()
            .find_map(|line| {
                let (sha, subject) = line.split_once(' ')?;

                (subject.split_whitespace().next() == Some(id)).then(|| sha.to_owned())
            })
            .unwrap_or_default();
        if sha.is_empty() {
            return self.fail(format!("no deviation {id} in the reference"));
        }
        if self.git_output(&["status", "--porcelain"])?.is_empty() {
            return self.fail("nothing to amend");
        }
        process::check(self.git(&["add", "-A"]), "git add")?;
        process::check(
            self.git(&["commit", "-q", &format!("--fixup={sha}")]),
            "git commit --fixup",
        )?;
        let mut rebase = self.git(&["rebase", "-q", "-i", "--autosquash", &base]);
        rebase.env("GIT_SEQUENCE_EDITOR", "true");
        process::check(rebase, "git rebase --autosquash")?;
        self.export_patches(&self.patches)?;
        self.remember()?;
        println!("amended {id}");
        Ok(())
    }

    fn export(&self) -> Result<()> {
        self.need()?;
        self.export_patches(&self.patches)?;
        self.remember()?;
        for name in entry_names(&self.patches)? {
            println!("{name}");
        }
        Ok(())
    }

    fn status(&self) -> Result<()> {
        self.need()?;
        let base = self.base()?;
        let current = self
            .root_git(&[
                "diff",
                "--quiet",
                &base,
                "HEAD",
                "--",
                ".",
                ":(exclude)port",
            ])
            .status()?
            .success();
        if current {
            println!("upstream: current");
        } else {
            println!("upstream: changed since the reference was built: run {PROGRAM} build");
        }

        let temporary = TempDir::new("opencode-reference-status")?;
        self.export_patches(temporary.path())?;
        let in_sync = diff_quiet(temporary.path(), &self.patches, Some("*.md"))?
            || (entry_names(temporary.path())?.is_empty() && d_patches(&self.patches)?.is_empty());
        if in_sync {
            let count = entry_names(temporary.path())?.len();
            println!("patches: in sync ({})", process::wc_count(count));
        } else {
            println!("patches: the reference and port/patches differ");
            let _ = Command::new("diff")
                .arg("-rq")
                .arg(temporary.path())
                .arg(&self.patches)
                .args(["-x", "*.md"])
                .status();
        }

        let changes = self.git_output(&["status", "--short"])?;
        if changes.is_empty() {
            println!("uncommitted: none");
        } else {
            println!("uncommitted:");
            println!("{changes}");
        }
        Ok(())
    }
}

fn is_deviation_id(id: &str) -> bool {
    id.len() == 4 && id.starts_with('D') && id[1..].bytes().all(|byte| byte.is_ascii_digit())
}

fn slugify(subject: &str) -> String {
    let rest = subject
        .split_once(' ')
        .map(|(_, rest)| rest)
        .unwrap_or(subject);
    let mut slug = String::new();
    for character in rest.to_ascii_lowercase().chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let mut slug: String = slug.chars().take(60).collect();
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

fn d_patches(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut patches = Vec::new();
    if let Ok(entries) = fs::read_dir(directory) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('D') && name.ends_with(".patch") && entry.path().is_file() {
                patches.push(entry.path());
            }
        }
    }
    patches.sort();
    Ok(patches)
}

fn entry_names(directory: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if let Ok(entries) = fs::read_dir(directory) {
        for entry in entries.flatten() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

fn diff_quiet(left: &Path, right: &Path, exclude: Option<&str>) -> Result<bool> {
    let mut command = Command::new("diff");
    command.args(["-rq"]).arg(left).arg(right);
    if let Some(exclude) = exclude {
        command.args(["-x", exclude]);
    }
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deviation_ids_are_d_plus_three_digits() {
        assert!(is_deviation_id("D001"));
        assert!(is_deviation_id("D999"));
        assert!(!is_deviation_id("D01"));
        assert!(!is_deviation_id("D0001"));
        assert!(!is_deviation_id("d001"));
        assert!(!is_deviation_id("D00a"));
    }

    #[test]
    fn patch_slugs_match_the_shell_script() {
        assert_eq!(
            slugify("D109 Permissions: wildcard characters from a command or path are literal"),
            "permissions-wildcard-characters-from-a-command-or-path-are-l"
        );
        assert_eq!(slugify("D123"), "d123");
        assert_eq!(slugify("D123  A  B "), "a-b");
        assert_eq!(
            slugify("D123 a very long subject that goes on and on and on and on and on and on"),
            "a-very-long-subject-that-goes-on-and-on-and-on-and-on-and-on"
        );
    }
}
