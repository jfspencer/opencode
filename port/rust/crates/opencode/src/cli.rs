//! The `opencode` command line (packages/cli/src/index.ts, commands/commands.ts, commands/handlers/*): effect/cli
//! 4.1's parsing, help and errors over opencode's command tree, then the command's handler. Commands talk to a
//! server over HTTP as the TS CLI does: an explicit `--server`, the background service, or a private
//! `opencode serve --stdio` child of this same executable.
//!
//! Exit codes follow `NodeRuntime.runMain`: a failure is 1 (its `Cause.pretty` on stderr), help with errors 1,
//! SIGINT 130, SIGTERM 143, otherwise what the handler sets.
//!
//! Not ported (they answer an error): `mini`, `mcp`, `plugin`, `upgrade`, `uninstall`, `pair`, the TUI (the
//! default command), `--wizard`; see each handler's TODO(port).

mod client;
mod commands;
mod connection;
mod help;
mod out;
mod parse;
mod paths;
mod serve;
mod service;
mod spec;

use out::{Fail, quote, stderr, stdout};
use parse::{Globals, Input, Outcome};

const BASH: &str = include_str!("cli/completions/bash.txt");
const ZSH: &str = include_str!("cli/completions/zsh.txt");
const FISH: &str = include_str!("cli/completions/fish.txt");
const POWERSHELL: &str = include_str!("cli/completions/powershell.txt");

/// `process.stdin.isTTY`.
pub fn stdin_tty() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(0) == 1 }
}

/// `process.stdout.isTTY`.
#[allow(dead_code)] // `run`, next
pub fn stdout_tty() -> bool {
    // SAFETY: as above.
    unsafe { libc::isatty(1) == 1 }
}

static STARTUP: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The directory the CLI started in (`process.cwd()` at load).
pub fn startup_directory() -> String {
    STARTUP.get().cloned().unwrap_or_else(|| crate::project::absolutize("."))
}

/// `PortOwner.describe`: the process listening on a TCP port, through `lsof` and `ps` (best effort).
pub async fn port_owner(port: u16, hostname: &str) -> Option<String> {
    let run = |program: &str, args: Vec<String>| {
        let mut command = tokio::process::Command::new(program);
        command.args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
        async move {
            let output = tokio::time::timeout(std::time::Duration::from_secs(1), command.output()).await.ok()?.ok()?;
            output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        }
    };
    let listing = run("lsof", vec!["-nP".into(), format!("-iTCP:{port}"), "-sTCP:LISTEN".into(), "-Fpn".into()]).await?;
    let host = hostname.trim_start_matches('[').trim_end_matches(']');
    let wildcard = host.parse::<std::net::IpAddr>().is_err() || host == "0.0.0.0" || host == "::";
    let mut pid: Option<String> = None;
    let mut found = None;
    for line in listing.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            pid = rest.bytes().all(|b| b.is_ascii_digit()).then(|| rest.to_owned());
        }
        if let (Some(rest), Some(pid)) = (line.strip_prefix('n'), &pid) {
            let address = rest.rsplit_once(':').map_or(rest, |(a, _)| a).trim_start_matches('[').trim_end_matches(']');
            if wildcard || address == "*" || address == host {
                found = Some(pid.clone());
                break;
            }
        }
    }
    let pid = found?;
    let command = run("ps", vec!["-o".into(), "command=".into(), "-p".into(), pid.clone()]).await.unwrap_or_default();
    let command = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if command.is_empty() {
        return Some(format!("PID {pid}"));
    }
    let shown = if command.chars().count() > 200 { format!("{}…", command.chars().take(199).collect::<String>()) } else { command };
    Some(format!("PID {pid} ({shown})"))
}

/// Keeps requests to local servers off any proxy: `NO_PROXY` and `no_proxy` get the same merged list.
fn merge_no_proxy() {
    let mut entries: Vec<String> = vec![];
    for value in [std::env::var("NO_PROXY").ok(), std::env::var("no_proxy").ok(), Some("127.0.0.1,localhost,::1".into())] {
        for entry in value.unwrap_or_default().split(',') {
            let entry = entry.trim();
            if !entry.is_empty() && !entries.iter().any(|e| e == entry) {
                entries.push(entry.to_owned());
            }
        }
    }
    let list = entries.join(",");
    // SAFETY: no other thread exists yet.
    unsafe {
        std::env::set_var("NO_PROXY", &list);
        std::env::set_var("no_proxy", &list);
    }
}

fn exit(code: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code)
}

pub fn main() -> ! {
    let argv: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let _ = STARTUP.set(crate::project::absolutize("."));
    merge_no_proxy();
    let root = spec::root();
    let code = match parse::run(&root, &argv) {
        Outcome::Help { path, errors } => {
            stdout(&format!("{}\n", help::render(&root, &path)));
            if errors.is_empty() {
                0
            } else {
                stderr(&format!("{}\n", help::errors(&errors)));
                1
            }
        }
        Outcome::Version => {
            stdout(&format!("{} v{}\n", root.name, crate::app::VERSION));
            0
        }
        Outcome::Completions(shell) => {
            stdout(match shell {
                "zsh" => ZSH,
                "fish" => FISH,
                "powershell" | "pwsh" => POWERSHELL,
                _ => BASH,
            });
            0
        }
        Outcome::Wizard => {
            stderr("Error: --wizard is not available in this build\n");
            1
        }
        Outcome::Run { path, input, globals } => {
            if globals.print_logs {
                // SAFETY: single-threaded so far.
                unsafe { std::env::set_var("OPENCODE_PRINT_LOGS", "1") };
            }
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
            match runtime.block_on(dispatch(&path, &input, &globals)) {
                Ok(code) => code,
                Err(fail) => {
                    stderr(&format!("{}: {}\n", fail.name, fail.message));
                    1
                }
            }
        }
    };
    exit(code)
}

fn unavailable(path: &[&str]) -> out::Result {
    Err(Fail::error(format!("opencode {} is not available in this build", path[1..].join(" "))))
}

async fn dispatch(path: &[&'static str], input: &Input, globals: &Globals) -> out::Result {
    paths::create();
    let names: Vec<&str> = path[1..].to_vec();
    match names.as_slice() {
        [] => default(input).await,
        ["serve"] => serve::run(input).await,
        ["service", name] => service::command(name, input).await,
        ["models"] => commands::models(globals).await,
        ["reload"] => commands::reload(globals).await,
        ["debug", "paths"] => commands::paths(input),
        ["debug", "agents"] => commands::agents(globals).await,
        ["debug", "config"] => commands::config(globals).await,
        ["session", "list"] => commands::session_list(globals, input).await,
        ["session", "delete"] => commands::session_delete(globals, input).await,
        ["session", "export"] => commands::session_export(globals, input).await,
        ["session", "import"] => commands::session_import(globals, input).await,
        ["api"] => commands::api(globals, input).await,
        // TODO(port): dropped (mini, mcp, plugin, the v1 migration) or not meaningful for this build (upgrade,
        // uninstall, pair, debug heap-dump).
        _ => unavailable(path),
    }
}

/// The default command: the TUI in a directory. An unknown command word arrives here as that directory.
async fn default(input: &Input) -> out::Result {
    if let Some(directory) = input.str("directory") {
        if std::env::set_current_dir(directory).is_err() {
            stderr(&format!("Unknown command or directory: {directory}\n"));
            return Ok(1);
        }
    }
    // TODO(port): the TUI (packages/tui) is another slice's.
    Err(Fail::error(format!("the interactive interface is not available in this build ({})", quote("opencode"))))
}
