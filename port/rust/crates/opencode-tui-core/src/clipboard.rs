//! Writing the clipboard: `lib/clipboard.ts`'s `createClipboard` over the renderer's terminal adapter
//! (`createRendererClipboardAdapter`: OSC 52 unless the terminal is known not to support it) and a host
//! clipboard, with `writeText(text, { destination: "all-available", selection: "clipboard" })`'s rules: the host
//! is skipped on a remote terminal, and both run.
//!
//! TODO(port-tui): TS's host clipboard is OpenTUI's native clipboard service (Wayland data control, X11
//! selections, the macOS pasteboard); here the text goes to the platform's clipboard command (`pbcopy`,
//! `wl-copy`, `xclip`, `xsel`). Reading (`read`) is not ported.
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::renderer;
use crate::terminal::Osc52Support;

/// `HostClipboardWriteResult.status` (and `"not-attempted"`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostStatus {
    Written,
    Unsupported,
    Failed(String),
    NotAttempted,
}

/// `TerminalClipboardOperationResult.status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalStatus {
    Attempted,
    LocalFailure,
    NotAttempted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteResult {
    pub host: HostStatus,
    pub terminal: TerminalStatus,
}

/// `writeText(text, { destination: "all-available", selection: "clipboard" })`.
pub async fn write_text(text: &str) -> WriteResult {
    let (remote, osc52) = renderer::clipboard_capabilities();
    let host = (!remote).then(|| host_write(text.to_string()));
    let terminal = match osc52 {
        Osc52Support::Unsupported => TerminalStatus::NotAttempted,
        _ if renderer::copy_to_clipboard_osc52(text) => TerminalStatus::Attempted,
        _ => TerminalStatus::LocalFailure,
    };
    let host = match host {
        Some(write) => write.await,
        None => HostStatus::NotAttempted,
    };
    WriteResult { host, terminal }
}

/// The platform's clipboard commands, in the order they are tried.
fn host_commands() -> Vec<(&'static str, &'static [&'static str])> {
    if cfg!(target_os = "macos") {
        return vec![("pbcopy", &[])];
    }
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    let mut commands: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if set("WAYLAND_DISPLAY") {
        commands.push(("wl-copy", &[]));
    }
    if set("DISPLAY") {
        commands.push(("xclip", &["-selection", "clipboard"]));
        commands.push(("xsel", &["--clipboard", "--input"]));
    }
    commands
}

fn on_path(command: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(command).is_file()))
}

async fn host_write(text: String) -> HostStatus {
    let Some((command, args)) = host_commands().into_iter().find(|(command, _)| on_path(command)) else {
        return HostStatus::Unsupported;
    };
    let child = tokio::process::Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => return HostStatus::Failed(format!("{command}: {error}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(text.as_bytes()).await {
            return HostStatus::Failed(format!("{command}: {error}"));
        }
    }
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) if status.success() => HostStatus::Written,
        Ok(Ok(status)) => HostStatus::Failed(format!("{command} exited with {status}")),
        Ok(Err(error)) => HostStatus::Failed(format!("{command}: {error}")),
        Err(_) => HostStatus::Failed(format!("{command} timed out")),
    }
}
