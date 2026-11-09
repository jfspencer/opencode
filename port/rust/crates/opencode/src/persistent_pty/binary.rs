//! Where the `opencode-pty` daemon comes from (packages/core/src/persistent-pty/binary.bun.ts, the CLI build's
//! `opencode-pty-binary` plugin): `OPENCODE_PTY_BIN` when set; else the daemon embedded in this executable (a build
//! with `OPENCODE_PTY_EMBED`, see build.rs and port/scripts/opencode-pty.sh), installed on first use into
//! `<cache>/opencode/bin/opencode-pty/<version>-<sha256 prefix>/opencode-pty` (private directories, the file checked
//! against the embedded checksum each time it is used); else `opencode-pty` on `PATH`. TS ships the daemon the same
//! way: inside the single-file executable, extracted to the same place.

/// The embedded daemon: its bytes, version and SHA-256.
#[cfg(opencode_pty_embedded)]
const ASSET: Option<(&[u8], &str, &str)> = Some((
    include_bytes!(env!("OPENCODE_PTY_EMBED_PATH")),
    env!("OPENCODE_PTY_EMBED_VERSION"),
    env!("OPENCODE_PTY_EMBED_SHA256"),
));
#[cfg(not(opencode_pty_embedded))]
const ASSET: Option<(&[u8], &str, &str)> = None;

/// `resolveBinary(global.bin)`: the daemon executable to start. Blocking (file checks); run off the runtime thread.
pub fn resolve(bin: &str) -> Result<String, String> {
    if let Some(binary) = std::env::var("OPENCODE_PTY_BIN").ok().filter(|b| !b.is_empty()) {
        return Ok(binary);
    }
    match ASSET {
        None => Ok("opencode-pty".into()),
        Some((bytes, version, sha256)) => install(bin, bytes, version, sha256),
    }
}

fn install(bin: &str, bytes: &[u8], version: &str, sha256: &str) -> Result<String, String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let root = format!("{bin}/opencode-pty");
    private_directory(&root)?;
    let directory = format!("{root}/{version}-{}", &sha256[..16]);
    private_directory(&directory)?;
    let destination = format!("{directory}/opencode-pty");
    if validate_if_present(&destination, sha256)? {
        return Ok(destination);
    }
    if hex(bytes) != sha256 {
        return Err("Embedded opencode-pty checksum mismatch".into());
    }
    let temporary = format!("{directory}/opencode-pty.{}.{}.tmp", std::process::id(), crate::id::uuid());
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .and_then(|()| std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755)))
        .map_err(|e| e.to_string())
        .and_then(|()| match std::fs::rename(&temporary, &destination) {
            Ok(()) => Ok(()),
            // Another server installed it first.
            Err(error) => match validate_if_present(&destination, sha256) {
                Ok(true) => Ok(()),
                _ => Err(error.to_string()),
            },
        });
    let _ = std::fs::remove_file(&temporary);
    written?;
    validate(&destination, sha256)
}

/// `privateDirectory`: created mode 0700, a real directory of this user.
fn private_directory(directory: &str) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(directory).map_err(|e| e.to_string())?;
    let info = std::fs::symlink_metadata(directory).map_err(|e| e.to_string())?;
    if !info.is_dir() || info.file_type().is_symlink() {
        return Err(format!("Unsafe opencode-pty directory: {directory}"));
    }
    // SAFETY: getuid has no preconditions.
    if info.uid() != unsafe { libc::getuid() } {
        return Err(format!("opencode-pty directory is owned by another user: {directory}"));
    }
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}

fn validate_if_present(file: &str, expected: &str) -> Result<bool, String> {
    match std::fs::symlink_metadata(file) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        _ => validate(file, expected).map(|_| true),
    }
}

/// `validate`: a regular file of this user with the expected checksum, made executable.
fn validate(file: &str, expected: &str) -> Result<String, String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let info = std::fs::symlink_metadata(file).map_err(|e| e.to_string())?;
    if !info.is_file() || info.file_type().is_symlink() {
        return Err(format!("Unsafe opencode-pty executable: {file}"));
    }
    // SAFETY: getuid has no preconditions.
    if info.uid() != unsafe { libc::getuid() } {
        return Err(format!("opencode-pty executable is owned by another user: {file}"));
    }
    // Streamed: the daemon is megabytes, and nothing keeps it in memory.
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    let mut handle = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match handle.read(&mut chunk).map_err(|e| e.to_string())? {
            0 => break,
            n => hasher.update(&chunk[..n]),
        }
    }
    let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if digest != expected {
        return Err(format!("Cached opencode-pty checksum mismatch: {file}"));
    }
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    Ok(file.to_owned())
}

fn hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
