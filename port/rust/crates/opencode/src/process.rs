//! Child processes (packages/util/src/process.ts, `AppProcess.run`): a command runs to completion off the runtime's
//! thread (tokio::process), with its output buffered. With `max_output`, each stream keeps only its first bytes and
//! is marked truncated (`collectStream`).

use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncReadExt};

#[derive(Debug, Default)]
pub struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl Output {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn error_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    pub fn truncated(&self) -> bool {
        self.stdout_truncated || self.stderr_truncated
    }
}

pub struct Command<'a> {
    pub program: &'a str,
    pub args: Vec<String>,
    pub cwd: &'a str,
    pub env: Vec<(String, String)>,
    pub max_output: Option<usize>,
}

impl<'a> Command<'a> {
    pub fn new(program: &'a str, cwd: &'a str) -> Self {
        Self { program, args: vec![], cwd, env: vec![], max_output: None }
    }

    pub fn args<I: IntoIterator<Item = S>, S: Into<String>>(mut self, args: I) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }

    pub fn max_output(mut self, bytes: Option<usize>) -> Self {
        self.max_output = bytes;
        self
    }

    /// `describeCommand`: the program and its arguments joined by spaces.
    pub fn describe(&self) -> String {
        if self.args.is_empty() { self.program.to_owned() } else { format!("{} {}", self.program, self.args.join(" ")) }
    }

    /// Runs the command to completion; a spawn failure is an error (`AppProcessError` in TS).
    pub async fn run(&self) -> std::io::Result<Output> {
        let mut command = tokio::process::Command::new(self.program);
        command
            .args(&self.args)
            .current_dir(self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (out, err, status) = tokio::join!(collect(stdout, self.max_output), collect(stderr, self.max_output), child.wait());
        let (stdout, stdout_truncated) = out?;
        let (stderr, stderr_truncated) = err?;
        let status = status?;
        Ok(Output { code: exit_code(status), stdout, stderr, stdout_truncated, stderr_truncated })
    }
}

/// The exit code as Node reports it; a process killed by a signal has none (`128 + signal`, as shells report it).
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

async fn collect(mut stream: impl AsyncRead + Unpin, max: Option<usize>) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut total = 0usize;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        total += read;
        match max {
            None => kept.extend_from_slice(&buffer[..read]),
            Some(max) => {
                let room = max.saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..read.min(room)]);
            }
        }
    }
    Ok((kept, max.is_some_and(|max| total > max)))
}

/// `which`: the first executable named `name` on `PATH`, else in `extra`.
pub fn which(name: &str, extra: Option<&str>) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .chain(extra)
        .map(|dir| std::path::Path::new(dir).join(name))
        .find(|candidate| {
            std::fs::metadata(candidate).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .map(|p| p.to_string_lossy().into_owned())
}
