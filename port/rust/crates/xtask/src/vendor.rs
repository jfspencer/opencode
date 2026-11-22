//! Vendored sources: the tree-sitter grammars `opencode-tui-markup` compiles and the OpenTUI checkout its
//! harnesses compare against.

use std::fs;
use std::path::{Path, PathBuf};

use crate::archive::{extract_auto, extract_with_strip};
use crate::process::{
    self, Ctx, Error, Result, TempDir, bail, capture_trim, download, sha256_file,
};

const SOURCES: &str = "port/rust/crates/opencode-tui-markup/grammars/SOURCES.txt";

#[derive(Debug)]
struct Grammar {
    name: String,
    archive: String,
    sha256: String,
    source: String,
}

pub fn vendor_grammars(ctx: &Ctx, destination: Option<PathBuf>) -> Result<()> {
    let sources_path = ctx.path(SOURCES);
    let sources = process::read(&sources_path)?;
    let destination = destination.unwrap_or_else(|| ctx.path("port/.vendor/tree-sitter"));
    let archives = destination.join(".archives");
    fs::create_dir_all(&archives)?;
    let temporary = TempDir::new("opencode-grammars")?;

    let grammars = parse_sources(&sources)?;
    for grammar in &grammars {
        let directory = grammar.source.split('/').next().unwrap_or("");
        if directory.is_empty() {
            return bail(format!(
                "{}: no source directory for {}",
                SOURCES, grammar.name
            ));
        }
        let archive = archives.join(format!("{directory}.tar.gz"));
        if !archive.is_file() || !has_sha256(&archive, &grammar.sha256) {
            let part = PathBuf::from(format!("{}.part", archive.display()));
            download(&grammar.archive, &part)?;
            fs::rename(&part, &archive)?;
        }
        if !has_sha256(&archive, &grammar.sha256) {
            return bail(format!(
                "{}: sha256 is not {}",
                archive.display(),
                grammar.sha256
            ));
        }
        let extracted = temporary.path().join(directory);
        if !extracted.is_dir() {
            fs::create_dir_all(&extracted)?;
            extract_auto(&archive, &extracted, true)?;
        }
        copy_grammar(temporary.path(), &destination, grammar)?;
        println!("{}: {}", grammar.name, grammar.source);
    }
    fs::copy(&sources_path, destination.join("SOURCES.txt"))?;
    println!("tree-sitter grammars in {}", destination.display());
    Ok(())
}

fn parse_sources(text: &str) -> Result<Vec<Grammar>> {
    let mut grammars = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() < 6 {
            return bail(format!("{SOURCES}: malformed line: {line}"));
        }
        grammars.push(Grammar {
            name: fields[0].to_owned(),
            archive: fields[3].to_owned(),
            sha256: fields[4].to_owned(),
            source: fields[5].to_owned(),
        });
    }
    Ok(grammars)
}

fn has_sha256(path: &Path, expected: &str) -> bool {
    sha256_file(path)
        .map(|actual| actual == expected)
        .unwrap_or(false)
}

fn copy_grammar(extracted: &Path, destination: &Path, grammar: &Grammar) -> Result<()> {
    let directory = grammar.source.split('/').next().unwrap_or("");
    let source = extracted.join(&grammar.source);
    let out = destination.join(&grammar.source);
    process::remove_path(&out)?;
    fs::create_dir_all(out.join("tree_sitter"))?;
    copy_files_with_extension(&source, &out, "c")?;
    copy_files_with_extension(&source, &out, "h")?;
    copy_files_with_extension(&source.join("tree_sitter"), &out.join("tree_sitter"), "h")?;
    let repository = extracted.join(directory);
    if let Ok(entries) = fs::read_dir(&repository) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if (name.starts_with("LICENSE") || name.starts_with("COPYING"))
                && entry.path().is_file()
            {
                fs::copy(
                    entry.path(),
                    destination.join(directory).join(name.as_ref()),
                )?;
            }
        }
    }
    let common = repository.join("common");
    if common.is_dir() {
        fs::create_dir_all(destination.join(directory).join("common"))?;
        copy_files_with_extension(&common, &destination.join(directory).join("common"), "h")?;
    }
    Ok(())
}

fn copy_files_with_extension(from: &Path, to: &Path, extension: &str) -> Result<()> {
    if !from.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|value| value == extension) {
            fs::copy(&path, to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

pub fn vendor_opentui(ctx: &Ctx) -> Result<()> {
    let package = process::read(ctx.path("package.json"))?;
    let version = json_string_field(&package, "@opentui/core")
        .ok_or_else(|| Error::new("no @opentui/core version in the root package.json"))?;
    let destination = ctx.path("port/.vendor/opentui");
    if destination.join(".git").is_dir() {
        let described = capture_trim(
            {
                let mut command = process::command("git");
                command
                    .arg("-C")
                    .arg(&destination)
                    .args(["describe", "--tags"]);
                command
            },
            "git describe --tags",
        )
        .unwrap_or_default();
        if described == format!("v{version}") {
            println!("opentui v{version} already in {}", destination.display());
            return Ok(());
        }
    }
    process::remove_path(&destination)?;
    fs::create_dir_all(ctx.path("port/.vendor"))?;
    process::check(
        {
            let mut command = process::command("git");
            command
                .args([
                    "-c",
                    "advice.detachedHead=false",
                    "clone",
                    "--quiet",
                    "--depth",
                    "1",
                    "--branch",
                ])
                .arg(format!("v{version}"))
                .arg("https://github.com/anomalyco/opentui")
                .arg(&destination);
            command
        },
        "git clone opentui",
    )?;
    let zig_deps = destination.join("packages/native/zig-deps");
    fs::create_dir_all(&zig_deps)?;
    // The archive holds `ghostty/`, `yoga/`, ... directly, with no single top-level directory to strip.
    extract_with_strip(
        destination.join("packages/native/src/vendor/zig-deps.tar.gz"),
        &zig_deps,
        true,
        0,
    )?;
    println!("opentui v{version} in {}", destination.display());
    Ok(())
}

/// The first string value for `key` in a small JSON document (`package.json` and similar files).
pub fn json_string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    let quote = rest.find('"')?;
    let rest = &rest[quote + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}
