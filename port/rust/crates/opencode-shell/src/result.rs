//! The pure output handling of the shell: `packages/core/src/shell/result.ts`, the capture `Shell.result` builds from
//! the output file (`core/src/shell.ts`), and the shell tool's result shapes (`core/src/tool/plugin/shell.ts`).
//!
//! Structs list their fields in the key order TS writes them; an `Option` field that is `None` is a key TS leaves
//! out.

/// `Shell.Status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Exited,
    Timeout,
    Killed,
}

/// What `ShellResult.output` reads from a terminal `Shell.Info`.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub status: Status,
    pub exit: Option<i64>,
    pub signal: Option<String>,
}

/// The bounded tail of a command's output (`ShellResult.Result.capture`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub output: String,
    pub truncated: bool,
}

/// TS `Output` of result.ts: `{ output, truncated, exit?, signal?, timeout? }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub output: String,
    pub truncated: bool,
    pub exit: Option<i64>,
    pub signal: Option<String>,
    pub timeout: Option<bool>,
}

/// `ShellResult.metadata(output)`: `{ truncated, exit?, signal?, timeout? }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Metadata {
    pub truncated: bool,
    pub exit: Option<i64>,
    pub signal: Option<String>,
    pub timeout: Option<bool>,
}

/// Text of `ShellResult.unavailable`, the output of a shell whose capture is gone.
pub const UNAVAILABLE: &str = "Shell command output is no longer available.";

/// `Shell.Output`: one page of a command's output file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub output: String,
    pub cursor: u64,
    pub size: u64,
    pub truncated: bool,
}

/// `ShellResult.unavailable`: the page a session gets for a shell whose output is gone.
pub fn unavailable() -> Page {
    let size = UNAVAILABLE.len() as u64;
    Page { output: UNAVAILABLE.into(), cursor: size, size, truncated: false }
}

/// The page size `Shell.output` reads when the request names none.
pub const DEFAULT_PAGE_LIMIT: u64 = 65536;

/// `Shell.output(id, { cursor, limit })` over an output file of `size` bytes; `read(start, length)` returns the
/// bytes the file holds there (fewer when it is shorter, none on a read error).
pub fn page(size: u64, cursor: Option<u64>, limit: Option<u64>, read: impl FnOnce(u64, u64) -> Vec<u8>) -> Page {
    let cursor = cursor.unwrap_or(0);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    // An empty page reads nothing; a zero-length read stream would be out of range.
    if cursor >= size || limit == 0 {
        return Page { output: String::new(), cursor: cursor.min(size), size, truncated: false };
    }
    let length = limit.min(size - cursor);
    let mut bytes = read(cursor, length);
    bytes.truncate(length as usize);
    Page {
        output: String::from_utf8_lossy(&bytes).into_owned(),
        cursor: cursor + bytes.len() as u64,
        size,
        truncated: false,
    }
}

/// `Shell.result`'s capture: `tail` is the last `min(max_bytes, size)` bytes of the output file (what
/// `output(id, { cursor: size - maxBytes, limit: maxBytes })` reads), `size` its length, `newlines` the newlines
/// counted while it streamed, `file` its path. `max_lines` and `max_bytes` are the `tool_output` limits
/// (`ToolOutput.MAX_LINES` = 2000 and `MAX_BYTES` = 51200 by default).
pub fn capture(tail: &[u8], size: u64, newlines: u64, max_lines: u64, max_bytes: u64, file: &str) -> Capture {
    // An empty page reads nothing (`cursor >= size || limit === 0`).
    let page = if size == 0 || max_bytes == 0 { String::new() } else { String::from_utf8_lossy(tail).into_owned() };
    let ends_with_newline = page.ends_with('\n');
    let mut lines: Vec<&str> = page.split('\n').collect();
    if ends_with_newline {
        lines.pop();
    }
    let count = lines.len() as u64;
    let truncated = size > max_bytes || count > max_lines;
    let text = if count > max_lines {
        // `lines.slice(-maxLines)`, where `-0` is the whole list.
        let from = if max_lines == 0 { 0 } else { lines.len() - max_lines as usize };
        lines[from..].join("\n")
    } else {
        page.clone()
    };
    let total = newlines as i128 + if ends_with_newline { 0 } else { 1 };
    let shown = count.min(max_lines) as i128;
    let notice = if truncated {
        format!("\n\n[showing lines {}-{} of {}; full output saved to {}]", total - shown + 1, total, total, file)
    } else {
        String::new()
    };
    let text = if text.is_empty() { "(no output)".to_string() } else { text };
    Capture { output: format!("{text}{notice}"), truncated }
}

/// `ShellResult.output(result)`.
pub fn output(info: &Info, capture: Option<&Capture>) -> Output {
    Output {
        output: capture.map_or(UNAVAILABLE.to_string(), |c| c.output.clone()),
        truncated: capture.is_some_and(|c| c.truncated),
        exit: info.exit,
        signal: info.signal.clone(),
        timeout: (info.status == Status::Timeout).then_some(true),
    }
}

/// `ShellResult.notice(output)`: why a command did not end normally.
pub fn notice(exit: Option<i64>, signal: Option<&str>, timeout: Option<bool>) -> Option<String> {
    if timeout == Some(true) {
        return Some("Timed out before completion".into());
    }
    if let Some(signal) = signal {
        return Some(format!("Killed by {signal}"));
    }
    match exit {
        Some(exit) if exit != 0 => Some(format!("Exited with code {exit}")),
        _ => None,
    }
}

/// `ShellResult.metadata(output)`.
pub fn metadata(output: &Output) -> Metadata {
    Metadata { truncated: output.truncated, exit: output.exit, signal: output.signal.clone(), timeout: output.timeout }
}

/// `ShellResult.notification(...).state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationState {
    Completed,
    Cancelled,
    Error,
}

impl NotificationState {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationState::Completed => "completed",
            NotificationState::Cancelled => "cancelled",
            NotificationState::Error => "error",
        }
    }
}

/// `ShellResult.notification(...)`: `{ text, metadata: { source: "shell", shellID, jobID?, state, ...metadata(output) } }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    pub text: String,
    pub shell_id: String,
    pub job_id: Option<String>,
    pub state: NotificationState,
    /// The output's metadata, spread after `state` when there is an output.
    pub output: Option<Metadata>,
}

/// `ShellResult.notification(input)`.
pub fn notification(
    shell_id: &str,
    job_id: Option<&str>,
    command: &str,
    state: NotificationState,
    text: &str,
    output: Option<&Output>,
) -> Notification {
    Notification {
        text: format!(
            "<shell id=\"{}\" state=\"{}\" command=\"{}\">\n{}\n</shell>",
            job_id.unwrap_or(shell_id),
            state.as_str(),
            command,
            text
        ),
        shell_id: shell_id.into(),
        job_id: job_id.map(String::from),
        state,
        output: output.map(metadata),
    }
}

/// `ShellResult.userNotification(result)`: the message a user's `!command` leaves in the session.
pub fn user_notification(shell_id: &str, command: &str, info: &Info, capture: Option<&Capture>) -> Notification {
    let captured = output(info, capture);
    let status = if info.status == Status::Killed {
        Some("Cancelled".to_string())
    } else if captured.exit == Some(0) {
        None
    } else {
        Some(
            notice(captured.exit, captured.signal.as_deref(), captured.timeout)
                .unwrap_or_else(|| "Exited with code unknown".into()),
        )
    };
    let text = [Some(captured.output.clone()), status]
        .into_iter()
        .flatten()
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let state = if info.status == Status::Killed { NotificationState::Cancelled } else { NotificationState::Completed };
    let mut message = notification(shell_id, None, command, state, &text, Some(&captured));
    message.text = format!("The following shell command was executed by the user:\n{}", message.text);
    message
}

/// The shell tool's `DEFAULT_TIMEOUT_MS`.
pub const DEFAULT_TIMEOUT_MS: u64 = 2 * 60 * 1_000;

/// The timeout a shell tool call runs with: background commands have none unless asked.
pub fn tool_timeout(timeout: Option<u64>, background: bool) -> u64 {
    timeout.unwrap_or(if background { 0 } else { DEFAULT_TIMEOUT_MS })
}

/// The instruction a backgrounded command's result carries.
pub const BACKGROUND_INSTRUCTION: &str = "You will be notified automatically when the command finishes. The notification will include the command's output. Unless the user explicitly asks otherwise, DO NOT poll for completion, even if you need the final result to continue. Repeatedly sleeping and reading or searching the output file is polling, not useful work. You may read the current output if it lets you do useful work now, but do not repeatedly check it while waiting for the command to finish. Keep working on anything that does not depend on the result. If you have nothing else to do, end your response; you will be resumed automatically when the command finishes.";

/// `process.platform` as the tool description names it (Linux and macOS: the port does not target Windows).
pub fn platform_name(platform: &str) -> &str {
    match platform {
        "darwin" => "macOS",
        "linux" => "Linux",
        other => other,
    }
}

/// The shell tool's description; `shell` is `ShellSelect.name` of the compatible shell, given once the session
/// hooks know it (the registered description has none). `platform` is `process.platform`.
pub fn tool_description(shell: Option<&str>, platform: &str) -> String {
    let mut parts = vec!["Execute a shell command and return its output.".to_string()];
    if let Some(shell) = shell {
        parts.push(format!("Commands run on {} using {}.", platform_name(platform), shell));
    }
    parts.extend(
        [
            "Quote file paths containing spaces or special characters.",
            "Prefer dedicated tools over shell commands when possible.",
            "When output is large, the full result is saved to a file and a truncated preview is returned.",
            "Rely on automatic truncation unless filtering the output is more useful.",
            "Commands accept an optional timeout, background commands have no timeout by default.",
            "Background commands return immediately, and you will be notified when they complete.",
        ]
        .map(String::from),
    );
    parts.join(" ")
}

/// `ToolStatus` of the shell tool's output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Completed,
    Running,
}

/// The shell tool's `Output`: `{ exit?, signal?, shellID?, truncated, timeout?, output, status? }`.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    pub exit: Option<i64>,
    pub signal: Option<String>,
    pub shell_id: Option<String>,
    pub truncated: bool,
    pub timeout: Option<bool>,
    pub output: String,
    pub status: Option<ToolStatus>,
}

/// The completed output of a foreground command (the `run` effect of the shell tool): a timed-out command's text
/// says which timeout it exceeded.
pub fn completed(output: &Output, timeout_ms: u64) -> ToolOutput {
    ToolOutput {
        exit: output.exit,
        signal: output.signal.clone(),
        shell_id: None,
        truncated: output.truncated,
        timeout: output.timeout,
        output: if output.timeout == Some(true) {
            format!(
                "{}\n\nCommand exceeded timeout of {} ms. Retry with a larger timeout if the command is expected to take longer.",
                output.output, timeout_ms
            )
        } else {
            output.output.clone()
        },
        status: Some(ToolStatus::Completed),
    }
}

/// `backgroundResult(shellID, file)`.
pub fn background(shell_id: &str, file: &str) -> ToolOutput {
    ToolOutput {
        exit: None,
        signal: None,
        shell_id: Some(shell_id.into()),
        truncated: false,
        timeout: None,
        output: format!("Command moved to the background (shell ID: {shell_id}).\nOutput is streaming to: {file}"),
        status: Some(ToolStatus::Running),
    }
}

/// `resultMessages(output)`: the text parts of the tool result.
pub fn result_messages(output: &ToolOutput) -> Vec<String> {
    let notice = if output.status == Some(ToolStatus::Running) {
        Some(BACKGROUND_INSTRUCTION.to_string())
    } else {
        notice(output.exit, output.signal.as_deref(), output.timeout)
    };
    let mut messages = Vec::new();
    if !output.output.is_empty() {
        messages.push(output.output.clone());
    }
    messages.extend(notice);
    messages
}

/// `toolResult(output).metadata`: `{ status, truncated, exit?, signal?, timeout?, shellID? }`. `status` is always a
/// key in TS, written only when defined.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolMetadata {
    pub status: Option<ToolStatus>,
    pub truncated: bool,
    pub exit: Option<i64>,
    pub signal: Option<String>,
    pub timeout: Option<bool>,
    pub shell_id: Option<String>,
}

pub fn tool_metadata(output: &ToolOutput) -> ToolMetadata {
    ToolMetadata {
        status: output.status,
        truncated: output.truncated,
        exit: output.exit,
        signal: output.signal.clone(),
        timeout: output.timeout,
        shell_id: output.shell_id.clone(),
    }
}

/// How a background shell job ended, for `notifyWhenDone`.
#[derive(Clone, Debug, PartialEq)]
pub enum JobEnd {
    Completed(ToolOutput),
    Error(Option<String>),
    Cancelled,
}

/// The text `notifyWhenDone` sends when a background command ends.
pub fn job_text(end: &JobEnd) -> String {
    match end {
        JobEnd::Completed(output) => result_messages(output).join("\n\n"),
        JobEnd::Error(error) => error.clone().unwrap_or_else(|| "Command failed".into()),
        JobEnd::Cancelled => "Cancelled".into(),
    }
}

/// The tool failure message of a shell call that could not run.
pub fn failure_message(command: &str) -> String {
    format!("Unable to execute command: {command}")
}
