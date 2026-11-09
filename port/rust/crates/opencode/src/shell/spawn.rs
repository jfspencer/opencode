//! Starting a shell command as the TS server's spawner does (packages/util/src/cross-spawn-spawner.ts with
//! `detached: true` and `stdin: "ignore"`): the process leads a session of its own (so its whole process group can be
//! signalled) and reads nothing.
//!
//! stdout and stderr are separate pipes, read as TS's `Stream.merge(stdout, stderr)` reads them: whatever stdout has
//! ready goes first.
//!
//! A failure to start is described as TS describes it: `findNulCharacter` first, then the system's error as a
//! `PlatformError` (`<Tag>: ChildProcess.spawn (<shell> <args>)`).

use std::os::fd::OwnedFd;
use std::process::Stdio;

/// Why a command could not start (`PlatformError`'s reason).
#[derive(Debug, Clone)]
pub enum Failure {
    /// `BadArgument`: a NUL character in the command, an argument or an environment variable.
    BadArgument(String),
    /// `SystemError` with its tag (`toTag` of the errno) and the errno itself.
    System { tag: &'static str, errno: Option<i32> },
}

#[derive(Debug, Clone)]
pub struct Error {
    pub failure: Failure,
    /// `PlatformError.message`.
    pub message: String,
}

impl Error {
    pub fn too_long(&self) -> bool {
        matches!(self.failure, Failure::System { errno: Some(libc::E2BIG), .. })
    }

    pub fn permission_denied(&self) -> bool {
        matches!(self.failure, Failure::System { tag: "PermissionDenied", .. })
    }

    pub fn bad_argument(&self) -> bool {
        matches!(self.failure, Failure::BadArgument(_))
    }
}

pub struct Spawned {
    pub child: tokio::process::Child,
    pub pid: u32,
    /// The read ends of the stdout and stderr pipes.
    pub stdout: tokio::net::unix::pipe::Receiver,
    pub stderr: tokio::net::unix::pipe::Receiver,
}

/// `findNulCharacter`.
fn nul(shell: &str, args: &[String], env: &[(String, String)]) -> Option<String> {
    if shell.contains('\0') {
        return Some("The command contains a NUL character".into());
    }
    if let Some(index) = args.iter().position(|arg| arg.contains('\0')) {
        return Some(format!("Argument {} contains a NUL character", index + 1));
    }
    env.iter()
        .find(|(_, value)| value.contains('\0'))
        .map(|(name, _)| format!("The environment variable {name} contains a NUL character"))
}

/// `toTag`: the `SystemError` tag of an errno.
fn tag(errno: Option<i32>) -> &'static str {
    match errno {
        Some(libc::ENOENT) => "NotFound",
        Some(libc::EACCES) => "PermissionDenied",
        Some(libc::EEXIST) => "AlreadyExists",
        Some(libc::EISDIR | libc::ENOTDIR | libc::ELOOP) => "BadResource",
        Some(libc::EBUSY) => "Busy",
        _ => "Unknown",
    }
}

fn system(error: &std::io::Error, method: &str, shell: &str, args: &[String]) -> Error {
    let errno = error.raw_os_error();
    let tag = tag(errno);
    Error { failure: Failure::System { tag, errno }, message: format!("{tag}: ChildProcess.{method} ({shell} {})", args.join(" ")) }
}

/// Starts `shell args` in `cwd` with exactly `env` (the parent's environment is not inherited).
pub fn spawn(shell: &str, args: &[String], cwd: &str, env: &[(String, String)]) -> Result<Spawned, Error> {
    if let Some(description) = nul(shell, args, env) {
        return Err(Error {
            message: format!("ChildProcess.spawn: {description}"),
            failure: Failure::BadArgument(description),
        });
    }
    // `fs.access(cwd)` before spawning.
    if let Err(error) = std::fs::metadata(cwd) {
        let errno = error.raw_os_error();
        let tag = tag(errno);
        return Err(Error { failure: Failure::System { tag, errno }, message: format!("{tag}: FileSystem.access ({cwd})") });
    }
    let (stdout_reader, stdout_writer) = std::io::pipe().map_err(|e| system(&e, "spawn", shell, args))?;
    let (stderr_reader, stderr_writer) = std::io::pipe().map_err(|e| system(&e, "spawn", shell, args))?;
    let mut command = tokio::process::Command::new(shell);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_writer))
        .stderr(Stdio::from(stderr_writer))
        .kill_on_drop(false);
    // SAFETY: setsid is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().map_err(|e| system(&e, "spawn", shell, args))?;
    // The parent's copies of the write end go with the command, so the pipe ends when the command's processes close it.
    drop(command);
    let pid = child.id().unwrap_or(0);
    let receiver = |reader: std::io::PipeReader| {
        tokio::net::unix::pipe::Receiver::from_owned_fd(OwnedFd::from(reader)).map_err(|e| system(&e, "spawn", shell, args))
    };
    let stdout = receiver(stdout_reader)?;
    let stderr = receiver(stderr_reader)?;
    Ok(Spawned { child, pid, stdout, stderr })
}

/// Signals the process group `pid` leads, else the process alone while it has not been reaped (`killGroup`, then
/// `killOne`, which Node refuses for an exited child). False when neither is signalled.
pub fn kill(pid: u32, signal: i32, reaped: bool) -> bool {
    if pid == 0 {
        return false;
    }
    let pid = pid as libc::pid_t;
    // SAFETY: plain system calls on a process ID.
    unsafe { libc::kill(-pid, signal) == 0 || (!reaped && libc::kill(pid, signal) == 0) }
}

/// Node's name for a signal number (`os.constants.signals`).
pub fn signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        #[cfg(target_os = "linux")]
        libc::SIGSTKFLT => "SIGSTKFLT",
        libc::SIGCHLD => "SIGCHLD",
        libc::SIGCONT => "SIGCONT",
        libc::SIGSTOP => "SIGSTOP",
        libc::SIGTSTP => "SIGTSTP",
        libc::SIGTTIN => "SIGTTIN",
        libc::SIGTTOU => "SIGTTOU",
        libc::SIGURG => "SIGURG",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        libc::SIGVTALRM => "SIGVTALRM",
        libc::SIGPROF => "SIGPROF",
        libc::SIGWINCH => "SIGWINCH",
        libc::SIGIO => "SIGIO",
        #[cfg(target_os = "linux")]
        libc::SIGPWR => "SIGPWR",
        libc::SIGSYS => "SIGSYS",
        other => return format!("SIG{other}"),
    };
    name.into()
}
