//! Which shell runs commands (packages/core/src/shell/select.ts, its Unix half): the configured shell, else `$SHELL`,
//! else bash on `PATH`, else `/bin/sh` (`/bin/zsh` on macOS). The shell tool asks for a compatible shell (fish and
//! nu are skipped); terminals and user commands take the configured one as is. The pure part (names, PowerShell,
//! arguments) is `opencode_shell::select`.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::inventory::Inventory;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Priority {
    /// `{ priority: "config" }`: any shell the user configured.
    Config,
    /// `{ priority: "compat" }`: a shell the tool's command scanner understands.
    Compat,
}

/// `Global.Path.bin`: `$XDG_CACHE_HOME/opencode/bin`, searched after `PATH`.
fn bin() -> String {
    format!("{}/bin", crate::inventory::config::Options::get().cache)
}

/// The `which` package on Unix: `name` itself when it holds a slash, else the first `PATH` (then `bin`) entry that has
/// an executable file of that name.
fn which(name: &str, bin: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let runnable =
        |file: &str| std::fs::metadata(file).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if name.contains('/') {
        return runnable(name).then(|| name.to_owned());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let search = match (path.is_empty(), bin.is_empty()) {
        (false, false) => format!("{path}:{bin}"),
        (false, true) => path,
        (true, _) => bin.to_owned(),
    };
    search
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") })
        .find(|candidate| runnable(candidate))
}

/// `executable`: a rooted path that is a file, or a name `which` finds.
fn executable(file: &str, bin: &str) -> Option<String> {
    if file.starts_with('/') {
        return std::fs::metadata(file).is_ok_and(|m| m.is_file()).then(|| file.to_owned());
    }
    which(file, bin)
}

fn fallback(bin: &str) -> String {
    if cfg!(target_os = "macos") {
        return "/bin/zsh".into();
    }
    which("bash", bin).unwrap_or_else(|| "/bin/sh".into())
}

/// `select`: the shell itself when it qualifies and exists, else the fallback.
fn select(file: Option<&str>, priority: Priority, bin: &str) -> String {
    if let Some(file) = file.filter(|f| !f.is_empty())
        && (priority == Priority::Config || opencode_shell::select::compatible(file))
        && let Some(shell) = executable(file, bin)
    {
        return shell;
    }
    fallback(bin)
}

/// `ShellSelect.environment`: `$SHELL` when it exists, else the fallback, resolved afresh (persistent terminals).
pub fn environment() -> String {
    select(std::env::var("SHELL").ok().as_deref(), Priority::Config, &bin())
}

/// `ShellSelect.resolve`: the configured shell when there is one, else the environment's, which is looked up once per
/// process and priority (as TS caches it).
pub fn resolve(priority: Priority, inventory: &Inventory) -> String {
    let bin = bin();
    let configured = inventory.latest(|c| c.shell.as_deref()).filter(|s| !s.is_empty());
    if let Some(shell) = configured {
        return select(Some(shell), priority, &bin);
    }
    static CACHE: Mutex<Option<HashMap<(Priority, String), String>>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    cache
        .entry((priority, bin.clone()))
        .or_insert_with(|| select(std::env::var("SHELL").ok().as_deref(), priority, &bin))
        .clone()
}
