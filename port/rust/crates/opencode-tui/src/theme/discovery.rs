//! Custom theme files: `discoverThemes` (`packages/tui/src/theme/discovery.ts`) and `configDirectories`
//! (`packages/tui/src/util/config-directories.ts`), which `createThemeSource(config).discover()` combines.

use std::path::{Component, Path, PathBuf};

use opencode_protocol::Json;

/// `configDirectories(config, cwd)`: the global config directory, then `.opencode` in every ancestor of `cwd`
/// from the root down, without repeats.
pub fn config_directories(config: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![normalize(config)];
    for directory in ancestors(cwd) {
        let candidate = normalize(&directory.join(".opencode"));
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// `path.resolve(cwd)` and each parent up to the root, root first.
fn ancestors(cwd: &Path) -> Vec<PathBuf> {
    let start = if cwd.is_absolute() {
        normalize(cwd)
    } else {
        normalize(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")).join(cwd))
    };
    let mut out = Vec::new();
    let mut current = Some(start.as_path());
    while let Some(path) = current {
        out.push(path.to_path_buf());
        current = path.parent();
    }
    out.reverse();
    out
}

/// `path.normalize` for an absolute path (`.` and `..` resolved lexically, no trailing slash).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() { PathBuf::from(".") } else { out }
}

/// Why discovery failed (TS rejects; the theme context then falls back to `opencode`).
#[derive(Debug)]
pub enum DiscoverError {
    Io(std::io::Error),
    /// `JSON.parse` failed on this file.
    Json(PathBuf),
}

/// `discoverThemes(directories)`: every `<directory>/themes/*.json` (files and symlinks), in sorted path order per
/// directory, parsed as JSON and keyed by file name without `.json`. Later directories replace earlier names (which
/// keep their position).
pub fn discover_themes(directories: &[PathBuf]) -> Result<Vec<(String, Json)>, DiscoverError> {
    let mut result: Vec<(String, Json)> = Vec::new();
    for directory in directories {
        let theme_directory = directory.join("themes");
        let entries = match std::fs::read_dir(&theme_directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(DiscoverError::Io(error)),
        };
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(DiscoverError::Io)?;
            let kind = entry.file_type().map_err(DiscoverError::Io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if (kind.is_file() || kind.is_symlink()) && extname_is_json(&name) {
                files.push(theme_directory.join(entry.file_name()));
            }
        }
        files.sort_by(|a, b| utf16_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
        for file in files {
            let text = std::fs::read_to_string(&file).map_err(DiscoverError::Io)?;
            let value = opencode_protocol::wire::json::parse(&text).map_err(|_| DiscoverError::Json(file.clone()))?;
            let name = file.file_name().unwrap().to_string_lossy();
            let name = name.strip_suffix(".json").unwrap_or(&name).to_owned();
            match result.iter_mut().find(|(n, _)| *n == name) {
                Some(slot) => slot.1 = value,
                None => result.push((name, value)),
            }
        }
    }
    Ok(result)
}

/// `path.extname(name) === ".json"`: a file named `.json` has no extension (`..json` has).
fn extname_is_json(name: &str) -> bool {
    name.ends_with(".json") && name != ".json"
}

/// JS string comparison (UTF-16 code units).
pub(crate) fn utf16_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}
