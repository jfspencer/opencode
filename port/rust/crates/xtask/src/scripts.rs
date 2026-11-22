//! Ports of the setup, vendor, reference and release scripts under port/scripts.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Instant;

use crate::archive::extract_with_strip;
use crate::process::{self, Ctx, Error, Result, bail, capture_trim};
use crate::vendor::json_string_field;

pub fn check_upstream_clean(ctx: &Ctx) -> Result<()> {
    let base = ctx.git_output(&["merge-base", "HEAD", "upstream/dev"])?;
    let mut paths = Vec::new();
    for args in [
        vec!["diff", "--name-only", base.as_str(), "HEAD"],
        vec!["diff", "--name-only", "--cached"],
        vec!["diff", "--name-only"],
    ] {
        let text = ctx.git_output(&args)?;
        paths.extend(
            text.lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
        );
    }
    paths.sort();
    paths.dedup();
    let violations: Vec<&str> = paths
        .iter()
        .map(String::as_str)
        .filter(|path| !allowed_upstream_path(path))
        .collect();
    if !violations.is_empty() {
        eprintln!(
            "These changes touch upstream-owned paths and will eventually conflict with upstream:"
        );
        for path in &violations {
            eprintln!("  {path}");
        }
        eprintln!("Move the change under port/ instead (see port/README.md).");
        return Err(Error::silent(1));
    }
    let short = ctx.git_output(&["rev-parse", "--short", &base])?;
    println!("ok: every fork change since upstream {short} is under port/");
    Ok(())
}

fn allowed_upstream_path(path: &str) -> bool {
    if path.starts_with("port/") {
        return true;
    }
    let Some(rest) = path.strip_prefix(".github/workflows/port-") else {
        return false;
    };
    let Some(stem) = rest
        .strip_suffix(".yml")
        .or_else(|| rest.strip_suffix(".yaml"))
    else {
        return false;
    };
    !stem.is_empty() && !stem.contains('/')
}

pub fn worktree_setup(ctx: &Ctx) -> Result<()> {
    let common = ctx.git_output(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let main = Path::new(&common)
        .parent()
        .ok_or_else(|| Error::new(format!("cannot find the main checkout from {common}")))?;
    let root = process::canonical(&ctx.root)?;
    let main = process::canonical(main)?;
    if root == main {
        eprintln!("this is the main checkout; nothing to link");
        return Ok(());
    }
    if !main.join("port/.reference/packages").is_dir() {
        return bail(format!(
            "no TS reference in {}: run port/scripts/reference.sh build there",
            main.display()
        ));
    }
    link(&main.join("port/.reference"), &root.join("port/.reference"))?;
    link(
        &main.join("port/.reference"),
        &root.join("port/conformance/ref"),
    )?;
    link(
        &main.join("port/conformance/node_modules"),
        &root.join("port/conformance/node_modules"),
    )?;
    if main.join("port/rust/gen/node_modules").is_dir() {
        link(
            &main.join("port/rust/gen/node_modules"),
            &root.join("port/rust/gen/node_modules"),
        )?;
    }
    if main.join("port/.vendor").is_dir() {
        link(&main.join("port/.vendor"), &root.join("port/.vendor"))?;
    }
    let exclude = main.join(".git/info/exclude");
    if let Some(parent) = exclude.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut text = fs::read_to_string(&exclude).unwrap_or_default();
    for line in ["/port/.reference", "/port/.vendor"] {
        if !text.lines().any(|existing| existing == line) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(line);
            text.push('\n');
        }
    }
    fs::write(&exclude, text)?;
    println!(
        "linked {} to the reference and packages of {}",
        root.display(),
        main.display()
    );
    Ok(())
}

fn link(target: &Path, destination: &Path) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(destination)
        && !meta.file_type().is_symlink()
    {
        return bail(format!(
            "{} exists and is not a link",
            destination.display()
        ));
    }
    process::symlink_force(target, destination)
}

pub fn link_deps(ctx: &Ctx, target: Option<PathBuf>) -> Result<()> {
    let target = process::canonical(target.unwrap_or_else(|| ctx.path("port/.reference")))?;
    if !target.join("packages/server/node_modules").is_dir() {
        return bail(format!(
            "{} has no installed packages/server",
            target.display()
        ));
    }
    let conformance = ctx.path("port/conformance");
    let relative = process::relative(&conformance, &target)?;
    process::symlink_force(&relative, conformance.join("ref"))?;

    let node_modules = conformance.join("node_modules");
    process::remove_path(&node_modules)?;
    fs::create_dir_all(node_modules.join("@opencode"))?;
    process::symlink_force(
        Path::new("../ref/packages/server/node_modules/effect"),
        node_modules.join("effect"),
    )?;
    let packages = target.join("packages/server/node_modules/@opencode");
    if let Ok(entries) = fs::read_dir(&packages) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            process::symlink_force(
                Path::new("../../ref/packages/server/node_modules/@opencode").join(&name),
                node_modules.join("@opencode").join(&name),
            )?;
        }
    }

    let tui_modules = target.join("packages/tui/node_modules");
    if let Ok(entries) = fs::read_dir(&tui_modules) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('@') {
                if name == "@opencode" || !entry.path().is_dir() {
                    continue;
                }
                if let Ok(scoped) = fs::read_dir(entry.path()) {
                    for child in scoped.flatten() {
                        let child = child.file_name().to_string_lossy().into_owned();
                        link_tui_dependency(&node_modules, &format!("{name}/{child}"))?;
                    }
                }
                continue;
            }
            link_tui_dependency(&node_modules, &name)?;
        }
    }

    let opencode = fs::read_dir(node_modules.join("@opencode"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    let opentui = fs::read_dir(node_modules.join("@opentui"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    println!(
        "conformance reference: {} ({opencode} @opencode packages, {opentui} @opentui)",
        target.display()
    );
    Ok(())
}

fn link_tui_dependency(node_modules: &Path, name: &str) -> Result<()> {
    if name == ".bin"
        || name.starts_with(".bin/")
        || name == "@opencode"
        || name.starts_with("@opencode/")
        || name == "effect"
        || (!name.contains('/') && name.starts_with('@'))
    {
        return Ok(());
    }
    let destination = node_modules.join(name);
    if fs::symlink_metadata(&destination).is_ok() {
        return Ok(());
    }
    let depth = name.bytes().filter(|byte| *byte == b'/').count();
    let mut prefix = PathBuf::new();
    for _ in 0..=depth {
        prefix.push("..");
    }
    let target = prefix.join("ref/packages/tui/node_modules").join(name);
    process::symlink_force(target, destination)
}

pub fn gen_types(ctx: &Ctx, args: &[String]) -> Result<()> {
    let generation = ctx.path("port/rust/gen");
    let reference = std::env::var_os("REFERENCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.path("port/.reference"));
    if !reference
        .join("packages/protocol/node_modules/effect")
        .is_dir()
    {
        return bail(format!(
            "no TS reference at {}: run port/scripts/reference.sh build",
            reference.display()
        ));
    }
    let git_dir: PathBuf = capture_trim(
        {
            let mut command = Command::new("git");
            command
                .arg("-C")
                .arg(&reference)
                .args(["rev-parse", "--absolute-git-dir"]);
            command
        },
        "git rev-parse --absolute-git-dir",
    )?
    .into();
    let base_file = git_dir.join("port-base");
    if base_file.is_file() {
        let base = process::read(&base_file)?.trim().to_owned();
        let status = ctx.git_status(&[
            "diff",
            "--quiet",
            &base,
            "HEAD",
            "--",
            ".",
            ":(exclude)port",
        ])?;
        if !status.success() {
            return bail(
                "the TS reference is older than this checkout's upstream code: run port/scripts/reference.sh build",
            );
        }
    }

    fs::create_dir_all(generation.join("node_modules/@opencode"))?;
    let effect = reference.join("packages/protocol/node_modules/effect");
    process::symlink_force(
        process::relative(generation.join("node_modules"), &effect)?,
        generation.join("node_modules/effect"),
    )?;
    for package in ["schema", "protocol"] {
        let target = reference.join("packages").join(package);
        let link = generation.join("node_modules/@opencode").join(package);
        process::symlink_force(
            process::relative(generation.join("node_modules/@opencode"), &target)?,
            link,
        )?;
    }

    let mode = args.first().map(String::as_str).unwrap_or("generate");
    match mode {
        "generate" => {
            run_bun(&generation, &["src/generate.ts"], None)?;
            run_bun(&generation, &["src/database.ts"], None)?;
            run_bun(&generation, &["src/mime.ts"], None)?;
            run_bun(&generation, &["src/openapi.ts"], Some(&reference))?;
            run_bun(&generation, &["src/tool-definitions.ts"], Some(&reference))?;
        }
        "check" => {
            let started = Instant::now();
            let check = ["--check"];
            run_bun(&generation, &["src/generate.ts", check[0]], None)?;
            run_bun(&generation, &["src/database.ts", check[0]], None)?;
            run_bun(&generation, &["src/mime.ts", check[0]], None)?;
            run_bun(&generation, &["src/openapi.ts", check[0]], Some(&reference))?;
            run_bun(
                &generation,
                &["src/tool-definitions.ts", check[0]],
                Some(&reference),
            )?;
            process::check(cargo(ctx)?, "cargo build")?;
            process::check(
                {
                    let mut command = cargo(ctx)?;
                    command.args(["test", "--locked", "--quiet", "-p", "opencode-protocol"]);
                    command
                },
                "cargo test -p opencode-protocol",
            )?;
            let mut command = Command::new("bun");
            command
                .current_dir(&generation)
                .args(["src/roundtrip.ts"])
                .args(&args[1..]);
            process::check(command, "gen round-trip")?;
            println!(
                "gen-types check: ok ({:.1}s)",
                started.elapsed().as_secs_f64()
            );
        }
        other => {
            return Err(Error::new(format!(
                "unknown gen-types mode {other}; usage: gen-types [generate|check [options]]"
            )));
        }
    }
    Ok(())
}

fn run_bun(directory: &Path, args: &[&str], reference: Option<&Path>) -> Result<()> {
    let mut command = Command::new("bun");
    command.current_dir(directory).args(args);
    if let Some(reference) = reference {
        command.env("REFERENCE", reference);
    }
    process::check(command, "bun")
}

fn cargo(ctx: &Ctx) -> Result<Command> {
    let mut command = Command::new("cargo");
    command.current_dir(ctx.path("port/rust"));
    Ok(command)
}

pub fn opencode_pty(ctx: &Ctx, args: &[String]) -> Result<PathBuf> {
    let mut source = false;
    let mut install = false;
    for argument in args {
        match argument.as_str() {
            "--source" => source = true,
            "--install" => install = true,
            other => {
                return Err(Error::usage(format!(
                    "unknown argument {other}; usage: opencode-pty [--source] [--install]"
                )));
            }
        }
    }
    let reference = ctx.path("port/.reference");
    if !reference.join("packages/core").is_dir() {
        return bail(format!(
            "no TS reference at {}: run port/scripts/reference.sh build (or worktree-setup.sh)",
            reference.display()
        ));
    }
    let core = process::read(reference.join("packages/core/package.json"))?;
    let version = json_string_field(&core, "@opencode/pty")
        .ok_or_else(|| Error::new("packages/core/package.json pins no @opencode/pty version"))?;
    let daemon = process::read(ctx.path("port/rust/crates/opencode/src/persistent_pty/daemon.rs"))?;
    let protocol = protocol_version(&daemon)
        .ok_or_else(|| Error::new("cannot read PROTOCOL_VERSION from persistent_pty/daemon.rs"))?;
    let out = ctx.path(format!("port/rust/target/opencode-pty/{version}"));
    fs::create_dir_all(&out)?;
    let binary = out.join("opencode-pty");

    if source {
        let tag = format!("process-daemon-v{version}");
        let status =
            ctx.git_status(&["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")])?;
        if !status.success() {
            return bail(format!("no tag {tag}: git fetch upstream --tags"));
        }
        let Some(zig) = process::which("zig") else {
            return bail(
                "the daemon's build.rs needs Zig 0.16 (libghostty-vt); without it, use the prebuilt daemon (no --source)",
            );
        };
        let work = out.join("source");
        process::remove_path(&work)?;
        fs::create_dir_all(&work)?;
        let archive = out.join("process-daemon.tar");
        process::check(
            {
                let mut command = ctx.git(&["archive", "--format=tar", "-o"]);
                command
                    .arg(&archive)
                    .args([tag.as_str(), "packages/process-daemon"]);
                command
            },
            "git archive process-daemon",
        )?;
        extract_with_strip(&archive, &work, false, 0)?;
        let daemon_dir = work.join("packages/process-daemon");
        let zig_wrapper = daemon_dir.join("script/release-bin/zig");
        let mut permissions = fs::metadata(&zig_wrapper)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&zig_wrapper, permissions)?;
        let path = format!(
            "{}:{}",
            daemon_dir.join("script/release-bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        process::check(
            {
                let mut command = Command::new("cargo");
                command
                    .current_dir(&daemon_dir)
                    .args(["build", "--release", "--locked"])
                    .env("OPENCODE_REAL_ZIG", zig)
                    .env("PATH", path);
                command
            },
            "cargo build process-daemon",
        )?;
        fs::copy(daemon_dir.join("target/release/opencode-pty"), &binary)?;
    } else {
        let prebuilt = capture_trim(
            {
                let mut command = Command::new("bun");
                command
                    .current_dir(reference.join("packages/core"))
                    .args(["-e", "const pty = await import(\"@opencode/pty\"); console.log(pty.binaryPath ?? \"\")"]);
                command
            },
            "bun import @opencode/pty",
        )?;
        let prebuilt = PathBuf::from(prebuilt);
        if prebuilt.as_os_str().is_empty() || !prebuilt.is_file() {
            return bail(
                "the reference has no prebuilt opencode-pty for this platform: use --source",
            );
        }
        process::copy_if_different(&prebuilt, &binary)?;
    }
    let mut permissions = fs::metadata(&binary)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&binary, permissions)?;

    let reported = capture_trim(
        {
            let mut command = Command::new(&binary);
            command.arg("--version");
            command
        },
        "opencode-pty --version",
    )?;
    let expected = format!("opencode-pty {version} (protocol {protocol})");
    if reported != expected {
        return bail(format!("unexpected daemon: {reported} (want {expected})"));
    }
    if install {
        process::check(
            {
                let mut command = cargo(ctx)?;
                command
                    .args(["build", "--release", "-p", "opencode"])
                    .env("OPENCODE_PTY_EMBED", &binary)
                    .env("OPENCODE_PTY_EMBED_VERSION", &version);
                command
            },
            "cargo build --release -p opencode",
        )?;
        eprintln!(
            "built {} with {reported} embedded",
            ctx.path("port/rust/target/release/opencode").display()
        );
    }
    Ok(binary)
}

fn protocol_version(daemon: &str) -> Option<String> {
    let marker = "const PROTOCOL_VERSION: f64 = ";
    let start = daemon.find(marker)? + marker.len();
    let rest = &daemon[start..];
    let end = rest.find(".0;")?;
    let digits = &rest[..end];
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        None
    } else {
        Some(digits.to_owned())
    }
}

pub fn check_macos(ctx: &Ctx) -> Result<()> {
    let rust = ctx.path("port/rust");
    let stub = rust.join("target/macos-stub-cc");
    let darwin = cfg!(target_os = "macos");
    if !darwin {
        fs::create_dir_all(stub.parent().unwrap())?;
        fs::write(
            &stub,
            r#"#!/bin/sh
for arg in "$@"; do
  if [ "$arg" = "-E" ]; then exec clang --target=arm64-apple-macos11 "$@"; fi
  if [ "$arg" = "-?" ]; then exit 1; fi
done
out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "-o" ]; then out="$arg"; fi
  prev="$arg"
done
if [ -n "$out" ]; then : >"$out"; fi
exit 0
"#,
        )?;
        let mut permissions = fs::metadata(&stub)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&stub, permissions)?;
    }
    let mut failed = false;
    for target in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
        println!("== {target}");
        let mut command = Command::new("cargo");
        command
            .current_dir(&rust)
            .args(["check", "--workspace", "--tests", "--target", target]);
        if !darwin {
            let variable = target.replace('-', "_");
            command
                .env(format!("CC_{variable}"), &stub)
                .env(format!("CXX_{variable}"), &stub)
                .env(format!("AR_{variable}"), "ar");
        }
        let status: ExitStatus = process::status(command)?;
        if !status.success() {
            failed = true;
        }
    }
    if failed {
        bail("cargo check failed for a macOS target")
    } else {
        Ok(())
    }
}

pub fn sync_upstream(ctx: &Ctx) -> Result<()> {
    let branch = ctx.git_output(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch != "dev" {
        return bail("switch to dev first");
    }
    for args in [vec!["diff", "--quiet"], vec!["diff", "--cached", "--quiet"]] {
        if !ctx.git_status(&args)?.success() {
            return bail("commit or stash your changes first");
        }
    }
    check_upstream_clean(ctx)?;
    let before = ctx.git_output(&["merge-base", "HEAD", "upstream/dev"])?;
    process::check(
        ctx.git(&["fetch", "--quiet", "upstream", "dev"]),
        "git fetch upstream dev",
    )?;
    let after = ctx.git_output(&["rev-parse", "upstream/dev"])?;
    if before == after {
        println!(
            "already up to date with upstream {}",
            &after[..after.len().min(7)]
        );
        return Ok(());
    }
    process::check(
        ctx.git(&["merge", "--no-edit", "upstream/dev"]),
        "git merge upstream/dev",
    )?;
    let count = ctx.git_output(&["rev-list", "--count", &format!("{before}..{after}")])?;
    println!();
    println!(
        "merged upstream {}..{} ({count} commits)",
        &before[..before.len().min(7)],
        &after[..after.len().min(7)]
    );
    println!(
        "next: bun install, port/scripts/link-deps.sh, run the conformance loop for that range, then: git push origin dev"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_port_and_new_workflow_files_are_allowed() {
        assert!(allowed_upstream_path(
            "port/scripts/check-upstream-clean.sh"
        ));
        assert!(allowed_upstream_path(".github/workflows/port-check.yml"));
        assert!(allowed_upstream_path(".github/workflows/port-check.yaml"));
        assert!(!allowed_upstream_path("packages/core/src/foo.ts"));
        assert!(!allowed_upstream_path(
            ".github/workflows/port-check.yml/extra"
        ));
        assert!(!allowed_upstream_path(".github/workflows/ci.yml"));
        assert!(!allowed_upstream_path(".github/workflows/port-.yml"));
    }
}
