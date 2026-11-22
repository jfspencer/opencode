//! tar/tar.gz extraction without depending on the host's tar implementation. The tree-sitter release archives
//! contain hardlink entries (`./src/foo` plus `src/foo` linking to it) that bsdtar 3.5.3 cannot unpack; this
//! extractor treats a hardlink whose target is the same path as already extracted content, as GNU tar does.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use tar::Archive;

use crate::process::{Result, bail};

/// Extracts an archive with one top-level directory when it has one, and bare files when it does not.
pub fn extract_auto(
    archive: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    gzip: bool,
) -> Result<()> {
    let archive = archive.as_ref();
    let destination = destination.as_ref();
    fs::create_dir_all(destination)?;
    let first = first_entry(archive, gzip)?;
    let strip =
        if first.starts_with("./") || first.starts_with("src/") || first.starts_with("LICENSE") {
            0
        } else {
            1
        };
    extract_with_strip(archive, destination, gzip, strip)
}

pub fn extract_with_strip(
    archive: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    gzip: bool,
    strip: usize,
) -> Result<()> {
    let archive = archive.as_ref();
    let destination = destination.as_ref();
    fs::create_dir_all(destination)?;
    let file = fs::File::open(archive)?;
    if gzip {
        extract_reader(GzDecoder::new(file), destination, strip)
    } else {
        extract_reader(file, destination, strip)
    }
}

fn first_entry(archive: &Path, gzip: bool) -> Result<String> {
    let file = fs::File::open(archive)?;
    if gzip {
        first_from_entries(Archive::new(GzDecoder::new(file)).entries()?)
    } else {
        first_from_entries(Archive::new(file).entries()?)
    }
}

fn first_from_entries<R: Read>(mut entries: tar::Entries<'_, R>) -> Result<String> {
    match entries.next() {
        Some(Ok(entry)) => Ok(entry.path()?.to_string_lossy().into_owned()),
        Some(Err(error)) => Err(error.into()),
        None => Ok(String::new()),
    }
}

fn extract_reader<R: Read>(reader: R, destination: &Path, strip: usize) -> Result<()> {
    let mut archive = Archive::new(reader);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_type = entry.header().entry_type();
        let path = entry.path()?.into_owned();
        let Some(relative) = strip_components(&path, strip)? else {
            continue;
        };
        let target = destination.join(&relative);
        if entry_type.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if entry_type.is_symlink() {
            let Some(link) = entry.link_name()?.map(|link| link.into_owned()) else {
                continue;
            };
            let link = strip_symlink_components(&link, strip)?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let _ = fs::remove_file(&target);
            std::os::unix::fs::symlink(&link, &target)?;
            continue;
        }
        if entry_type.is_hard_link() {
            let Some(link) = entry.link_name()?.map(|link| link.into_owned()) else {
                continue;
            };
            let Some(link_relative) = strip_components(&link, strip)? else {
                continue;
            };
            let link_target = destination.join(&link_relative);
            if link_target == target {
                // The release archive names the same file as `./src/foo` and as the hardlink `src/foo`.
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let _ = fs::remove_file(&target);
            if fs::hard_link(&link_target, &target).is_err() {
                // A hardlink target that is not a regular file cannot be linked; copying preserves the archive's
                // visible content and is what a fresh extraction needs.
                if link_target.is_file() {
                    fs::copy(&link_target, &target)?;
                } else {
                    return bail(format!(
                        "{}: hardlink target {} is missing",
                        archive_name(destination),
                        link_target.display()
                    ));
                }
            }
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let _ = fs::remove_file(&target);
        entry.unpack(&target)?;
    }
    Ok(())
}

fn archive_name(destination: &Path) -> String {
    destination.display().to_string()
}

/// `./src/foo` and `tree-sitter-x-1.0/src/foo` become `src/foo`; any `..` or absolute component rejects the
/// archive entry.
fn strip_components(path: &Path, strip: usize) -> Result<Option<PathBuf>> {
    let mut components = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir));
    for _ in 0..strip {
        if components.next().is_none() {
            return Ok(None);
        }
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return bail(format!(
                    "archive entry escapes the destination: {}",
                    path.display()
                ));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        Ok(None)
    } else {
        Ok(Some(relative))
    }
}

/// Symlink targets may legitimately contain `..`; only the top-level directory component is stripped.
fn strip_symlink_components(path: &Path, strip: usize) -> Result<PathBuf> {
    let mut components = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir));
    for _ in 0..strip {
        if components.next().is_none() {
            return Ok(PathBuf::new());
        }
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir => relative.push(".."),
            Component::RootDir | Component::Prefix(_) => {
                return bail(format!(
                    "symlink target escapes the destination: {}",
                    path.display()
                ));
            }
        }
    }
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tar::{Builder, EntryType, Header};

    use super::*;
    use crate::process::TempDir;

    fn header(kind: EntryType, size: u64) -> Header {
        let mut header = Header::new_gnu();
        header.set_entry_type(kind);
        header.set_mode(0o644);
        header.set_size(size);
        header
    }

    #[test]
    fn extracts_release_archives_with_duplicate_hardlinks() {
        let mut builder = Builder::new(Vec::new());
        let mut file = header(EntryType::Regular, 4);
        file.set_cksum();
        builder
            .append_data(&mut file, "./src/a.c", Cursor::new(b"main"))
            .unwrap();
        let mut link = header(EntryType::Link, 0);
        link.set_link_name("./src/a.c").unwrap();
        link.set_cksum();
        builder
            .append_data(&mut link, "src/a.c", Cursor::new(Vec::<u8>::new()))
            .unwrap();
        let bytes = builder.into_inner().unwrap();

        let temporary = TempDir::new("xtask-archive-test").unwrap();
        let archive = temporary.path().join("release.tar");
        fs::write(&archive, bytes).unwrap();
        let destination = temporary.path().join("out");
        extract_with_strip(&archive, &destination, false, 0).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("src/a.c")).unwrap(),
            "main"
        );
    }

    #[test]
    fn strips_a_single_top_directory() {
        let mut builder = Builder::new(Vec::new());
        let mut file = header(EntryType::Regular, 4);
        file.set_cksum();
        builder
            .append_data(&mut file, "tree-sitter-x-1.0/src/a.c", Cursor::new(b"main"))
            .unwrap();
        let bytes = builder.into_inner().unwrap();

        let temporary = TempDir::new("xtask-archive-test").unwrap();
        let archive = temporary.path().join("release.tar");
        fs::write(&archive, bytes).unwrap();
        let destination = temporary.path().join("out");
        extract_auto(&archive, &destination, false).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("src/a.c")).unwrap(),
            "main"
        );
    }
}

#[cfg(test)]
mod opentui_tests {
    use std::path::Path;

    use super::*;
    use crate::process::TempDir;

    #[test]
    fn opentui_zig_deps_are_not_stripped() {
        // The vendored checkout is gitignored; skip on a checkout that has not run vendor-opentui yet.
        let archive = Path::new("../.vendor/opentui/packages/native/src/vendor/zig-deps.tar.gz");
        if !archive.is_file() {
            return;
        }
        let temporary = TempDir::new("xtask-zig-deps-test").unwrap();
        let destination = temporary.path().join("zig-deps");
        extract_with_strip(archive, &destination, true, 0).unwrap();
        assert!(destination.join("ghostty/build.zig").is_file());
        assert!(!destination.join("build.zig").exists());
    }
}
