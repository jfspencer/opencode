//! A program in a pseudo-terminal, as bun-pty 0.4.11 runs one (portable-pty underneath, packages/core/src/pty/pty.bun.ts):
//! the program leads a new session whose controlling terminal is the PTY's slave side (on stdin, stdout and stderr),
//! with the default dispositions of the signals a server may have changed, in `cwd` (the home directory when that is
//! not a directory) and exactly the given environment. Output is read from the master side in chunks of at most
//! 4 KiB and decoded as streaming UTF-8 (`TextDecoder`, invalid bytes as U+FFFD); a kill sends SIGHUP, then SIGKILL
//! when the program is still there 200 ms later; a program killed by a signal exits with code 1.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::Stdio;
use std::time::Duration;

/// `DEFAULT_COLS` and `DEFAULT_ROWS`: the size a terminal starts with.
pub const COLS: u16 = 80;
pub const ROWS: u16 = 24;
/// What one read from the master side takes at most (bun-pty's read buffer).
pub const CHUNK: usize = 4096;

pub struct Spawned {
    pub pid: u32,
    /// The master side, non-blocking.
    pub master: OwnedFd,
    pub child: tokio::process::Child,
}

fn cvt(result: libc::c_int) -> io::Result<libc::c_int> {
    if result == -1 { Err(io::Error::last_os_error()) } else { Ok(result) }
}

fn set_flags(fd: RawFd, nonblocking: bool) -> io::Result<()> {
    // SAFETY: plain fcntl calls on a descriptor this module owns.
    unsafe {
        let fd_flags = cvt(libc::fcntl(fd, libc::F_GETFD))?;
        cvt(libc::fcntl(fd, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC))?;
        if nonblocking {
            let flags = cvt(libc::fcntl(fd, libc::F_GETFL))?;
            cvt(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK))?;
        }
    }
    Ok(())
}

/// Starts `file` with `args` in a new pseudo-terminal of `cols` × `rows`.
pub fn spawn(file: &str, args: &[String], cwd: &str, env: &[(String, String)], cols: u16, rows: u16) -> io::Result<Spawned> {
    let mut size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: openpty writes the two descriptors; the name and termios arguments may be null. (macOS declares the
    // termios and size arguments mutable, Linux constant: mutable pointers suit both.)
    cvt(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) })?;
    // SAFETY: openpty succeeded, so both descriptors are open and ours.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    set_flags(master.as_raw_fd(), true)?;
    set_flags(slave.as_raw_fd(), false)?;
    // portable-pty's `CommandBuilder`: a cwd that is not a directory falls back to the home directory.
    let directory = if std::path::Path::new(cwd).is_dir() {
        cwd.to_owned()
    } else {
        std::env::var("HOME").ok().filter(|h| !h.is_empty()).unwrap_or_else(|| "/".into())
    };
    let child = {
        let mut command = tokio::process::Command::new(file);
        command
            .args(args)
            .current_dir(directory)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                for signal in [libc::SIGCHLD, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGALRM] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                let mut empty: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut empty);
                libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn()?
        // The command (and its copies of the slave side) drops here: only the program keeps the slave open.
    };
    let pid = child.id().unwrap_or(0);
    Ok(Spawned { pid, master, child })
}

/// Sets the terminal's size; the kernel tells the foreground process group (SIGWINCH).
pub fn resize(master: &OwnedFd, cols: u16, rows: u16) {
    let size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCSWINSZ reads a winsize from the pointer.
    unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) };
}

/// One non-blocking read: `Ok(0)` at the end (the slave side closed: EIO on Linux).
pub fn read(fd: RawFd, buffer: &mut [u8]) -> io::Result<usize> {
    // SAFETY: the buffer is valid for its length.
    let n = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if n >= 0 {
        return Ok(n as usize);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EIO) { Ok(0) } else { Err(error) }
}

pub fn write(fd: RawFd, data: &[u8]) -> io::Result<usize> {
    // SAFETY: the data is valid for its length.
    let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
    if n >= 0 { Ok(n as usize) } else { Err(io::Error::last_os_error()) }
}

/// `ChildKiller::kill` of portable-pty: SIGHUP, a grace period, then SIGKILL.
pub async fn kill(child: &mut tokio::process::Child) {
    let Some(pid) = child.id() else { return };
    // SAFETY: the child is not reaped yet (`id` is set), so the pid is still its own.
    if unsafe { libc::kill(pid as i32, libc::SIGHUP) } != 0 {
        return;
    }
    for attempt in 0..5 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// portable-pty's `ExitStatus`: the exit code, or 1 for a program a signal ended.
pub fn exit_code(status: std::process::ExitStatus) -> i64 {
    status.code().map(i64::from).unwrap_or(1)
}

/// `TextDecoder("utf-8")` in streaming mode: a sequence split between chunks waits for its end, invalid bytes become
/// U+FFFD (maximal subparts, as the WHATWG decoder replaces them).
#[derive(Default)]
pub struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    pub fn decode(&mut self, bytes: &[u8]) -> String {
        let joined;
        let mut rest: &[u8] = if self.pending.is_empty() {
            bytes
        } else {
            let mut data = std::mem::take(&mut self.pending);
            data.extend_from_slice(bytes);
            joined = data;
            &joined
        };
        let mut out = String::with_capacity(rest.len());
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.push_str(text);
                    return out;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    out.push_str(std::str::from_utf8(&rest[..valid]).unwrap_or_default());
                    match error.error_len() {
                        Some(length) => {
                            out.push('\u{FFFD}');
                            rest = &rest[valid + length..];
                        }
                        None => {
                            self.pending = rest[valid..].to_vec();
                            return out;
                        }
                    }
                }
            }
        }
    }

    /// The end of the stream: an unfinished sequence is one U+FFFD.
    pub fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        self.pending.clear();
        "\u{FFFD}".into()
    }
}

#[cfg(test)]
mod tests {
    use super::Utf8Stream;

    #[test]
    fn streaming_utf8() {
        let mut stream = Utf8Stream::default();
        let text = "a─b";
        let bytes = text.as_bytes();
        assert_eq!(stream.decode(&bytes[..2]), "a");
        assert_eq!(stream.decode(&bytes[2..]), "─b");
        assert_eq!(stream.decode(b"x\xffy"), "x\u{FFFD}y");
        assert_eq!(stream.decode(b"\xe2\x94"), "");
        assert_eq!(stream.finish(), "\u{FFFD}");
    }
}
