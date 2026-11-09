//! Shell command analysis, ported from the TS reference: what a command line runs, the permission it asks for, and
//! how the shell tool reports what ran. Pure functions only; spawning, files and asking are the server's.
//!
//! | Module | TS |
//! |---|---|
//! | [`scan`] | `core/src/shell/scan.ts`: commands and opaque reasons (Bash, Zsh, POSIX sh, PowerShell) |
//! | [`parse`] | `core/src/shell/parse.ts`: resources, saved prefixes, `cd` directories; `FileAccess.resolvePath` |
//! | [`wildcard`] | `core/src/util/wildcard.ts` |
//! | [`permission`] | `Permission.evaluate` and the rule decisions of `core/src/permission.ts`, `whollyDisabled` |
//! | [`select`] | the pure part of `core/src/shell/select.ts`: shell names, PowerShell, arguments |
//! | [`result`] | `core/src/shell/result.ts`, the output capture of `Shell.result`, the shell tool's results |
//! | [`path`] | Node's `path.posix` |
//!
//! Strings are UTF-16 inside, as in TS, so every offset counts UTF-16 units; a `?` wildcard matches one code point,
//! as the TS regex does in Unicode mode. The `_units` functions take and return them; the others take `&str` and
//! return `String`, replacing a lone surrogate (which only a PowerShell `` `u{d800} `` escape can produce) with
//! U+FFFD.
//!
//! Verified against the TS reference by `harness/check.sh` (see README.md).

pub mod js;
pub mod parse;
pub mod path;
pub mod permission;
pub mod result;
pub mod scan;
pub mod select;
mod unicode;
pub mod wildcard;
mod word;

pub use parse::Host;
pub use scan::{Dialect, Reason};

/// The permission request a shell tool call makes (TS `ShellParse.scan`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// `Permission.Request.resources` for the `shell` action, one per command found, in order.
    pub resources: Vec<String>,
    /// `Permission.Request.save`: the pattern an "always" reply saves for each resource.
    pub save: Vec<String>,
    /// Directories the command changes to, unresolved (relative ones are relative to `cwd`); the tool resolves
    /// each with [`resolve_path`] and asks for `external_directory` access outside the project.
    pub directories: Vec<String>,
}

/// Analyzes `command` as the shell tool does before it runs it in `cwd` (absolute) with `shell` (the compatible
/// shell's path; its name picks the dialect). `Err` is TS's error message; the tool reports it as the call's failure.
/// An empty `resources` asks nothing (the TS tool skips the `shell` permission then).
pub fn analyze(command: &str, shell: &str, cwd: &str, host: &dyn Host) -> Result<Request, String> {
    let analysis = parse::analyze_units(&js::units(command), &js::units(shell), &js::units(cwd), host)
        .map_err(parse::error_message)?;
    Ok(Request {
        resources: analysis.commands.iter().map(|c| js::text(&c.resource)).collect(),
        save: analysis.commands.iter().map(|c| js::text(&c.save)).collect(),
        directories: analysis.directories.iter().map(|d| js::text(d)).collect(),
    })
}

/// `FileAccess.resolvePath(directory, input, home)`: an absolute path for a directory a command named.
pub fn resolve_path(directory: &str, input: &str, home: &str) -> String {
    js::text(&parse::resolve_path_units(&js::units(directory), &js::units(input), &js::units(home)))
}

/// The process's view, as `os.homedir()`, `os.userInfo()`, `/etc/passwd` and `process.env` give it on Linux and
/// macOS. Build it once: like TS, it reads the password database on first use and keeps it.
pub struct SystemHost {
    home: String,
    passwd: std::sync::OnceLock<Passwd>,
}

struct Passwd {
    current: Option<(String, String)>,
    homes: Vec<(String, String)>,
}

impl SystemHost {
    pub fn new() -> Self {
        let home = std::env::var("HOME").ok().filter(|home| !home.is_empty());
        let host = SystemHost { home: String::new(), passwd: std::sync::OnceLock::new() };
        let home = home.or_else(|| host.passwd().current.as_ref().map(|(_, home)| home.clone())).unwrap_or_default();
        SystemHost { home, ..host }
    }

    fn passwd(&self) -> &Passwd {
        self.passwd.get_or_init(|| {
            let text = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
            let entries: Vec<(String, String, String)> = text
                .split('\n')
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .filter_map(|line| {
                    let fields: Vec<&str> = line.split(':').collect();
                    let name = fields.first().copied().unwrap_or("");
                    let home = fields.get(5).copied().unwrap_or("");
                    let uid = fields.get(2).copied().unwrap_or("");
                    (!name.is_empty() && !home.is_empty()).then(|| (name.into(), home.into(), uid.into()))
                })
                .collect();
            // os.userInfo() is the effective user, which owns /proc/self on Linux.
            #[cfg(target_os = "linux")]
            let uid = {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata("/proc/self").ok().map(|meta| meta.uid().to_string())
            };
            #[cfg(not(target_os = "linux"))]
            let uid: Option<String> = None;
            let current = uid.and_then(|uid| {
                entries.iter().find(|(_, _, id)| *id == uid).map(|(name, home, _)| (name.clone(), home.clone()))
            });
            Passwd { current, homes: entries.into_iter().map(|(name, home, _)| (name, home)).collect() }
        })
    }
}

impl Default for SystemHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Host for SystemHost {
    fn home(&self) -> String {
        self.home.clone()
    }

    fn user_home(&self, user: &str) -> Option<String> {
        let passwd = self.passwd();
        if let Some((name, home)) = &passwd.current
            && name == user
            && !home.is_empty()
        {
            return Some(home.clone());
        }
        // `new Map(entries)`: the last entry of a name wins.
        if let Some((_, home)) = passwd.homes.iter().rev().find(|(name, _)| name == user) {
            return Some(home.clone());
        }
        if cfg!(target_os = "macos") {
            let candidate = format!("/Users/{user}");
            if std::fs::metadata(&candidate).is_ok_and(|meta| meta.is_dir()) {
                return Some(candidate);
            }
        }
        None
    }

    fn env(&self, name: &str) -> Option<String> {
        // `process.env`'s keys end at the first `=` of an entry, while getenv("a=b") would read the variable `a`.
        if name.contains('=') {
            return None;
        }
        std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
    }
}
