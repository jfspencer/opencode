//! Embeds the `opencode-pty` daemon when `OPENCODE_PTY_EMBED` names its executable (as the TS build embeds the
//! `@opencode/pty` binary in the single-file executable), with its version (`OPENCODE_PTY_EMBED_VERSION`, "local" by
//! default, as TS names a daemon embedded from `OPENCODE_PTY_BIN`) and SHA-256. `port/scripts/opencode-pty.sh`
//! builds the daemon and an executable that embeds it. Without it, the server starts `OPENCODE_PTY_BIN` or
//! `opencode-pty` from `PATH` (src/persistent_pty/binary.rs).

use sha2::Digest;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(opencode_pty_embedded)");
    println!("cargo:rerun-if-env-changed=OPENCODE_PTY_EMBED");
    println!("cargo:rerun-if-env-changed=OPENCODE_PTY_EMBED_VERSION");
    let Some(path) = std::env::var_os("OPENCODE_PTY_EMBED").filter(|path| !path.is_empty()) else { return };
    let path = std::fs::canonicalize(&path).unwrap_or_else(|e| panic!("OPENCODE_PTY_EMBED={path:?}: {e}"));
    println!("cargo:rerun-if-changed={}", path.display());
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let sha256: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    let version = std::env::var("OPENCODE_PTY_EMBED_VERSION").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "local".into());
    println!("cargo:rustc-cfg=opencode_pty_embedded");
    println!("cargo:rustc-env=OPENCODE_PTY_EMBED_PATH={}", path.display());
    println!("cargo:rustc-env=OPENCODE_PTY_EMBED_SHA256={sha256}");
    println!("cargo:rustc-env=OPENCODE_PTY_EMBED_VERSION={version}");
}
