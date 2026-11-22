//! Ports of the conformance, coverage and differential-suite driver scripts. These commands still invoke bun,
//! cargo and git as external tools; the argument parsing, sharding and report handling live here so macOS and Linux
//! take the same paths.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Instant;

use serde_json::Value;

use crate::process::{self, Ctx, Error, Result, TempDir, bail};
use crate::vendor::{vendor_grammars, vendor_opentui};

const CATEGORIES: [&str; 7] = ["A", "B", "C", "D", "E", "F", "G"];

pub fn upstream_tests(ctx: &Ctx, args: &[String]) -> Result<()> {
    let out = ctx.path("port/coverage/out");
    if args.first().map(String::as_str) == Some("compare") {
        let (Some(a), Some(b)) = (args.get(1), args.get(2)) else {
            return Err(Error::usage("usage: upstream-tests compare <a> <b>"));
        };
        let first = read_lines(out.join(format!("upstream-{a}/failures.txt")))?;
        let second = read_lines(out.join(format!("upstream-{b}/failures.txt")))?;
        let first: BTreeSet<&str> = first.iter().map(String::as_str).collect();
        let second: BTreeSet<&str> = second.iter().map(String::as_str).collect();
        for line in second.difference(&first) {
            println!("{line}");
        }
        return Ok(());
    }
    let label = args
        .first()
        .ok_or_else(|| Error::usage("usage: upstream-tests <label> [tree]"))?;
    let tree = process::canonical(
        args.get(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| ctx.path("port/.reference")),
    )?;
    let directory = out.join(format!("upstream-{label}"));
    process::remove_path(&directory)?;
    fs::create_dir_all(&directory)?;
    let lists = load_lists(ctx)?;
    let all = category_files(&lists, &CATEGORIES);
    let failures_path = directory.join("failures.txt");
    let mut failures = String::new();
    for package in packages(&all) {
        let files: Vec<String> = all
            .iter()
            .filter(|file| file.split('/').nth(1) == Some(package.as_str()))
            .filter_map(|file| file.splitn(3, '/').nth(2).map(|rest| format!("./{rest}")))
            .collect();
        let log_path = directory.join(format!("{package}.log"));
        let started = Instant::now();
        let status = run_test(
            &tree.join("packages").join(&package),
            &files,
            &[],
            &log_path,
            1800,
        )?;
        let log = process::read(&log_path).unwrap_or_default();
        for line in log.lines().filter(|line| line.starts_with("(fail)")) {
            failures.push_str(&format!("{package}: {}\n", strip_duration(line)));
        }
        println!(
            "{label} {package} exit={} {}s {}",
            status.code().unwrap_or(1),
            started.elapsed().as_secs(),
            summary(&log, &["pass", "fail"])
        );
    }
    process::write(&failures_path, failures)?;
    let count = read_lines(&failures_path)?.len();
    println!(
        "{count} failing tests in {} (list: {})",
        tree.display(),
        failures_path.display()
    );
    Ok(())
}

pub fn coverage_run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let (Some(label), Some(categories)) = (args.first(), args.get(1)) else {
        return Err(Error::new("usage: coverage-run <label> <category,...>"));
    };
    let out = ctx.path("port/coverage/out");
    fs::create_dir_all(&out)?;
    process::write(
        out.join(format!("{label}.ref")),
        format!("{}\n", ctx.git_output(&["rev-parse", "HEAD"])?),
    )?;
    let lists = load_lists(ctx)?;
    let categories: Vec<&str> = categories.split(',').collect();
    let files = category_files(&lists, &categories);
    for package in packages(&files) {
        let selected: Vec<String> = files
            .iter()
            .filter(|file| file.split('/').nth(1) == Some(package.as_str()))
            .filter_map(|file| file.splitn(3, '/').nth(2).map(|rest| format!("./{rest}")))
            .collect();
        let started = Instant::now();
        let log_path = out.join(format!("run-{label}-{package}.log"));
        let coverage = out.join(format!("{label}-{package}"));
        let mut command = Command::new("bun");
        command
            .current_dir(ctx.path("packages").join(&package))
            .args(["test", "--coverage", "--coverage-reporter=lcov"])
            .arg(format!("--coverage-dir={}", coverage.display()))
            .args(&selected);
        let status = run_logged(command, &log_path, 1200)?;
        let log = process::read(&log_path).unwrap_or_default();
        println!(
            "{label} {package} exit={} {}s {}",
            status.code().unwrap_or(1),
            started.elapsed().as_secs(),
            summary(&log, &["pass", "fail", "skip"])
        );
    }
    Ok(())
}

pub fn determinism(ctx: &Ctx, args: &[String]) -> Result<()> {
    let mut record = false;
    let mut positional = Vec::new();
    for argument in args {
        if argument == "--record" && positional.is_empty() {
            record = true;
        } else {
            positional.push(argument.as_str());
        }
    }
    let runs: usize = positional
        .first()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);
    let shards: usize = positional
        .get(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(6);
    if shards == 0 {
        return Err(Error::usage("shards must be greater than zero"));
    }
    let here = ctx.path("port/conformance");
    let log = TempDir::new("opencode-determinism")?.keep();
    let scenarios = test_files(&here.join("scenarios"))?;
    let tui = test_files(&here.join("tui"))?;
    if record {
        run_determinism_round(
            "record",
            &here,
            &scenarios,
            &tui,
            shards,
            &log,
            &[("UPDATE_GOLDENS", "1")],
        )?;
    }
    for run in 1..=runs {
        run_determinism_round(
            &format!("run{run}"),
            &here,
            &scenarios,
            &tui,
            shards,
            &log,
            &[("CONFORMANCE_STRICT", "1")],
        )?;
    }
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    for entry in fs::read_dir(&log)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("run") || !name.ends_with(".log") {
            continue;
        }
        for line in process::read(&path)?
            .lines()
            .filter(|line| line.starts_with("(fail)"))
        {
            *failures.entry(strip_duration(line)).or_default() += 1;
        }
    }
    if !failures.is_empty() {
        println!("scenarios that failed at least once (count of {runs} runs):");
        for (line, count) in failures {
            println!("{count:4} {line}");
        }
        println!("logs: {}", log.display());
        return bail("scenarios failed");
    }
    println!(
        "all scenarios reproduced on every run (logs: {})",
        log.display()
    );
    Ok(())
}

fn run_determinism_round(
    label: &str,
    here: &Path,
    scenarios: &[String],
    tui: &[String],
    shards: usize,
    log: &Path,
    environment: &[(&str, &str)],
) -> Result<()> {
    let mut children: Vec<Child> = Vec::new();
    for shard in 0..shards {
        let server = shard_files(scenarios, shards, shard);
        if !server.is_empty() {
            let path = log.join(format!("{label}-server-{shard}.log"));
            children.push(spawn_test(here, &server, environment, &path)?);
        }
        let tui_files = shard_files(tui, shards, shard);
        if !tui_files.is_empty() {
            let path = log.join(format!("{label}-tui-{shard}.log"));
            children.push(spawn_test(
                &here.join("tui"),
                &tui_files,
                environment,
                &path,
            )?);
        }
    }
    for mut child in children {
        let _ = child.wait()?;
    }
    let mut pass = 0u64;
    let mut fail = 0u64;
    for entry in fs::read_dir(log)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(&format!("{label}-")) || !name.ends_with(".log") {
            continue;
        }
        let text = process::read(&path)?;
        pass += count(&text, "pass");
        fail += count(&text, "fail");
    }
    println!("{label}: {pass} pass, {fail} fail");
    Ok(())
}

pub fn oracle(ctx: &Ctx, args: &[String]) -> Result<()> {
    let label = std::env::var("LABEL").unwrap_or_else(|_| "oracle".to_owned());
    let out = ctx.path("port/coverage/out");
    fs::create_dir_all(&out)?;
    process::write(
        out.join(format!("{label}.ref")),
        format!("{}\n", ctx.git_output(&["rev-parse", "HEAD"])?),
    )?;
    let here = ctx.path("port/conformance");
    let mut server = Vec::new();
    let mut tui = Vec::new();
    for file in args {
        if let Some(name) = file.strip_prefix("tui/") {
            tui.push(name.to_owned());
        } else {
            server.push(file.clone());
        }
    }
    let all = args.is_empty();
    if all || !server.is_empty() {
        run_oracle_suite(&here, &out, &label, "conformance", &server, all, 3600)?;
    }
    if all || !tui.is_empty() {
        run_oracle_suite(
            &here.join("tui"),
            &out,
            &label,
            "conformance-tui",
            &tui,
            all,
            3600,
        )?;
    }
    Ok(())
}

fn run_oracle_suite(
    directory: &Path,
    out: &Path,
    label: &str,
    suite: &str,
    files: &[String],
    all: bool,
    timeout: u64,
) -> Result<()> {
    let coverage = out.join(format!("{label}-{suite}"));
    process::remove_path(&coverage)?;
    let log_path = out.join(format!("run-{label}-{suite}.log"));
    let started = Instant::now();
    let mut command = Command::new("bun");
    command
        .current_dir(directory)
        .args(["test", "--coverage", "--coverage-reporter=lcov"])
        .arg(format!("--coverage-dir={}", coverage.display()));
    if !all {
        command.args(files);
    }
    let status = run_logged(command, &log_path, timeout)?;
    let log = process::read(&log_path).unwrap_or_default();
    println!(
        "{label} {suite} exit={} {}s {}",
        status.code().unwrap_or(1),
        started.elapsed().as_secs(),
        summary(&log, &["pass", "fail"])
    );
    Ok(())
}

pub fn oracle_sharded(ctx: &Ctx, args: &[String]) -> Result<()> {
    let base = args.first().map(String::as_str).unwrap_or("oracle");
    let shards: usize = args
        .get(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(8);
    if shards == 0 {
        return Err(Error::usage("shards must be greater than zero"));
    }
    let here = ctx.path("port/conformance");
    let mut files = test_files(&here.join("scenarios"))?;
    files.extend(
        test_files(&here.join("tui"))?
            .into_iter()
            .map(|file| format!("tui/{file}")),
    );
    let executable = std::env::current_exe()?;
    let mut children = Vec::new();
    for shard in 0..shards {
        let selected = shard_files(&files, shards, shard);
        if selected.is_empty() {
            continue;
        }
        let mut command = Command::new(&executable);
        command
            .args(["oracle"])
            .args(&selected)
            .env("LABEL", format!("{base}{shard}"));
        children.push(command.spawn()?);
    }
    for mut child in children {
        let _ = child.wait()?;
    }
    let labels: Vec<String> = (0..shards).map(|shard| format!("{base}{shard}")).collect();
    println!("union label: {}", labels.join("+"));
    Ok(())
}

pub fn rust_conformance(ctx: &Ctx, args: &[String]) -> Result<()> {
    if args
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("usage: rust-conformance [--update] [--release] [shards] [scenario files...]");
        return Ok(());
    }
    let mut release = false;
    for argument in args {
        match argument.as_str() {
            "--update" => {}
            "--release" => release = true,
            _ => break,
        }
    }
    let rust = ctx.path("port/rust");
    if !ctx
        .path("port/.vendor/opentui/packages/native/zig-deps/yoga")
        .is_dir()
    {
        vendor_opentui(ctx)?;
    }
    if !ctx.path("port/.vendor/tree-sitter").is_dir() {
        vendor_grammars(ctx, None)?;
    }
    let pty = crate::scripts::opencode_pty(ctx, &[]).ok();
    let mut build = Command::new("cargo");
    build
        .current_dir(&rust)
        .args(["build", "--quiet"])
        .args(["-p", "opencode"])
        .env("OPENCODE_VERSION", "conformance")
        .env("OPENCODE_CHANNEL", "conformance");
    if release {
        build.arg("--release");
    }
    if let Some(pty) = pty {
        build.env("OPENCODE_PTY_EMBED", &pty);
        if let Some(version) = pty.parent().and_then(|path| path.file_name()) {
            build.env("OPENCODE_PTY_EMBED_VERSION", version);
        }
    }
    process::check(build, "cargo build -p opencode")?;

    use std::os::unix::process::CommandExt;
    let script = ctx.path("port/scripts/rust-conformance.sh");
    let error = Command::new("bash")
        .arg(&script)
        .args(args)
        .env("OPENCODE_XTASK_SKIP_BUILD", "1")
        .exec();
    Err(Error::new(format!("exec {}: {error}", script.display())))
}

fn load_lists(ctx: &Ctx) -> Result<Value> {
    let out = ctx.path("port/coverage/out");
    let path = out.join("lists.json");
    if !path.is_file() {
        process::check(
            {
                let mut command = Command::new("python3");
                command
                    .current_dir(&ctx.root)
                    .args(["-I", "port/coverage/classify.py"]);
                command
            },
            "classify.py",
        )?;
    }
    let text = process::read(&path)?;
    Ok(serde_json::from_str(&text)?)
}

fn category_files(lists: &Value, categories: &[&str]) -> Vec<String> {
    let mut files = Vec::new();
    for category in categories {
        if let Some(values) = lists.get(*category).and_then(Value::as_array) {
            files.extend(
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned)),
            );
        }
    }
    files
}

fn packages(files: &[String]) -> Vec<String> {
    files
        .iter()
        .filter_map(|file| file.split('/').nth(1).map(str::to_owned))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn read_lines(path: impl AsRef<Path>) -> Result<Vec<String>> {
    if !path.as_ref().is_file() {
        return Ok(Vec::new());
    }
    Ok(process::read(path)?.lines().map(str::to_owned).collect())
}

fn test_files(directory: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".test.ts") && entry.path().is_file() {
            files.push(name);
        }
    }
    files.sort();
    Ok(files)
}

fn shard_files(files: &[String], shards: usize, shard: usize) -> Vec<String> {
    files
        .iter()
        .enumerate()
        .filter(|(index, _)| index % shards == shard)
        .map(|(_, file)| file.clone())
        .collect()
}

fn run_test(
    directory: &Path,
    files: &[String],
    environment: &[(&str, &str)],
    log: &Path,
    timeout: u64,
) -> Result<ExitStatus> {
    let mut command = Command::new("bun");
    command.current_dir(directory).arg("test").args(files);
    for (key, value) in environment {
        command.env(key, value);
    }
    run_logged(command, log, timeout)
}

fn run_logged(mut command: Command, log: &Path, timeout: u64) -> Result<ExitStatus> {
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(log)?;
    command.stdout(file.try_clone()?).stderr(file);
    process::timeout_status(command, timeout)
}

fn spawn_test(
    directory: &Path,
    files: &[String],
    environment: &[(&str, &str)],
    log: &Path,
) -> Result<Child> {
    let file = File::create(log)?;
    let mut command = Command::new("bun");
    command.current_dir(directory).arg("test").args(files);
    for (key, value) in environment {
        command.env(key, value);
    }
    command
        .stdout(Stdio::from(file.try_clone()?))
        .stderr(Stdio::from(file));
    Ok(command.spawn()?)
}

fn count(log: &str, word: &str) -> u64 {
    log.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let number = parts.next()?.parse::<u64>().ok()?;
            (parts.next()? == word).then_some(number)
        })
        .sum()
}

fn summary(log: &str, words: &[&str]) -> String {
    let mut parts = Vec::new();
    for line in log.lines() {
        let trimmed = line.trim_start();
        let mut split = trimmed.split_whitespace();
        if let (Some(number), Some(word)) = (split.next(), split.next())
            && number.parse::<u64>().is_ok()
            && words.contains(&word)
        {
            parts.push(format!("{number} {word}"));
        }
        if trimmed.starts_with("Ran ") {
            parts.push(trimmed.to_owned());
        }
    }
    parts.join(" ")
}

fn strip_duration(line: &str) -> String {
    let Some(start) = line.rfind(" [") else {
        return line.to_owned();
    };
    let duration = &line[start + 2..];
    let duration = duration.strip_suffix(']').unwrap_or(duration);
    let number = duration.trim_end_matches("ms").trim_end_matches('s');
    if number.parse::<f64>().is_ok() {
        line[..start].to_owned()
    } else {
        line.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn shards_are_round_robin() {
        let files = vec![
            "a".to_owned(),
            "b".to_owned(),
            "c".to_owned(),
            "d".to_owned(),
        ];
        assert_eq!(shard_files(&files, 2, 0), vec!["a", "c"]);
        assert_eq!(shard_files(&files, 2, 1), vec!["b", "d"]);
    }

    #[test]
    fn bun_durations_are_stripped_from_failures() {
        assert_eq!(strip_duration("(fail) one [1.2ms]"), "(fail) one");
        assert_eq!(strip_duration("(fail) two [2s]"), "(fail) two");
        assert_eq!(strip_duration("(fail) three"), "(fail) three");
    }

    #[test]
    fn summaries_follow_the_shell_grep() {
        let log = " 3 pass\n 1 fail\nRan 4 tests across 1 file.\n";
        assert_eq!(
            summary(log, &["pass", "fail"]),
            "3 pass 1 fail Ran 4 tests across 1 file."
        );
    }

    #[test]
    fn categories_flatten_in_order() {
        let lists = json!({"A": ["packages/core/a.test.ts"], "D": ["packages/cli/b.test.ts"]});
        assert_eq!(
            category_files(&lists, &["A", "D"]),
            vec!["packages/core/a.test.ts", "packages/cli/b.test.ts"]
        );
    }
}
