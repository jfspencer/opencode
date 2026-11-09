//! The pure part of `packages/core/src/shell/select.ts`: what a shell's path says about it. Finding shells on disk
//! (`resolve`, `list`, `which`) is the server's job.

use crate::js::{self, JsString};
use crate::path;

/// `ShellSelect.name(file)` off Windows: the lowercased base name.
pub fn name_units(file: &[u16]) -> JsString {
    js::to_lower(&path::basename(file))
}

/// `ShellSelect.name(file)`.
pub fn name(file: &str) -> String {
    js::text(&name_units(&js::units(file)))
}

struct Meta {
    deny: bool,
    login: bool,
    ps: bool,
}

// META in select.ts, looked up by own key only (`META[name]?.ps === true`).
fn meta(file: &[u16]) -> Option<Meta> {
    let name = name_units(file);
    let is = |text: &str| js::eq_ascii(&name, text);
    let login = Meta { deny: false, login: true, ps: false };
    if is("bash") || is("dash") || is("ksh") || is("sh") || is("zsh") {
        Some(login)
    } else if is("fish") {
        Some(Meta { deny: true, login: true, ps: false })
    } else if is("nu") {
        Some(Meta { deny: true, login: false, ps: false })
    } else if is("powershell") || is("pwsh") {
        Some(Meta { deny: false, login: false, ps: true })
    } else {
        None
    }
}

/// `ShellSelect.ps(file)`: PowerShell.
pub fn ps_units(file: &[u16]) -> bool {
    meta(file).is_some_and(|meta| meta.ps)
}

pub fn ps(file: &str) -> bool {
    ps_units(&js::units(file))
}

/// `ShellSelect.login(file)`: the shell takes `-l` for a login shell (the PTY passes it).
pub fn login(file: &str) -> bool {
    meta(&js::units(file)).is_some_and(|meta| meta.login)
}

/// Whether the shell tool may run commands with this shell (`compatible` in select.ts): fish and nu are denied.
pub fn compatible(file: &str) -> bool {
    meta(&js::units(file)).is_none_or(|meta| !meta.deny)
}

/// `ShellSelect.args(file, command)`: the arguments that run `command` with the shell.
pub fn args(file: &str, command: &str) -> Vec<String> {
    let units = js::units(file);
    if js::eq_ascii(&name_units(&units), "cmd") {
        return vec!["/c".into(), command.into()];
    }
    if ps_units(&units) {
        return ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", command].map(String::from).to_vec();
    }
    vec!["-c".into(), command.into()]
}
