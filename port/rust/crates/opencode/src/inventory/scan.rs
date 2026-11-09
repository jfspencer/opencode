//! The markdown file scans of config directories (`FSUtil.scan`, packages/util/src/glob.ts over npm `glob`): patterns
//! such as `{agent,agents}/**/*.md` with `dot: true` and symlinks followed, files only, results sorted as JS sorts
//! strings. The agent scan also skips `FSUtil.ignoreGlobs` (`node_modules`, `.git`, `.venv`, `venv`, `__pycache__`
//! anywhere).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Directories `FSUtil.ignoreGlobs` skips at any depth.
const IGNORED: [&str; 5] = ["node_modules", ".git", ".venv", "venv", "__pycache__"];

/// `<root>/{names}/**/*.md` (`recursive`) or `<root>/{names}/*.md`, skipping ignored directories: absolute, sorted.
pub fn markdown(root: &str, names: &[&str], recursive: bool) -> Vec<String> {
    collect(root, names, recursive, true)
}

/// `<root>/{names}/**/*.md` without ignores (the command scan).
pub fn markdown_all(root: &str, names: &[&str]) -> Vec<String> {
    collect(root, names, true, false)
}

fn collect(root: &str, names: &[&str], recursive: bool, ignore: bool) -> Vec<String> {
    let mut out = vec![];
    for name in names {
        if ignore && IGNORED.contains(name) {
            continue;
        }
        let mut seen = HashSet::new();
        walk(&Path::new(root).join(name), recursive, ignore, &mut seen, &mut out);
    }
    out.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    out.dedup();
    out
}

fn walk(dir: &Path, recursive: bool, ignore: bool, seen: &mut HashSet<PathBuf>, out: &mut Vec<String>) {
    // Symlinked directories are followed, each real directory once (a cycle ends at the first repeat).
    let Ok(real) = std::fs::canonicalize(dir) else { return };
    if !seen.insert(real) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        // `stat` follows symlinks; a dangling link is neither a file nor a directory.
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            if recursive && !(ignore && IGNORED.contains(&name.as_str())) {
                walk(&path, recursive, ignore, seen, out);
            }
        } else if meta.is_file() && name.ends_with(".md") {
            out.push(path.to_string_lossy().into_owned());
        }
    }
}
