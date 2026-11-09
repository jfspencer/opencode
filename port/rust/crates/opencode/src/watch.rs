//! File system watches (packages/core/src/filesystem/watcher.ts) over inotify: a file watch is the parent directory
//! filtered by the file's name (Node's `fs.watch`), a directory watch covers the whole tree below it (Parcel's
//! recursive watcher: new subdirectories are watched as they appear) minus what its `ignore` rejects. Each change is
//! sent as the path it concerns. A watch whose directory does not exist is no watch at all (TS ends its stream), and
//! `OPENCODE_DISABLE_FILEWATCHER` / `OPENCODE_FILEWATCHER_DISABLE` turn every watch off. Dropping a [`Watch`] stops it.
//!
//! On Linux the process has one inotify instance (the per-user instance limit is small and shared with every other
//! program), read by one task; watches of the same directory share its watch descriptor.
//!
//! TODO(port): macOS (TS: FSEvents through Parcel, kqueue-free `fs.watch`); until then nothing is watched there.

use tokio::sync::mpsc::UnboundedSender;

/// Where a watch sends the paths that changed.
pub type Sender = UnboundedSender<String>;

/// A live watch (or a task that stops with its handle).
pub struct Watch(Stop);

enum Stop {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Subscription(u64),
    Task(tokio::task::JoinHandle<()>),
}

impl Watch {
    /// A task that stops with the returned handle.
    pub fn task(handle: tokio::task::JoinHandle<()>) -> Watch {
        Watch(Stop::Task(handle))
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        match &self.0 {
            Stop::Task(handle) => handle.abort(),
            Stop::Subscription(_id) => {
                #[cfg(target_os = "linux")]
                inotify::unsubscribe(*_id);
            }
        }
    }
}

/// Whether the server watches files (`fs.filewatcher`).
pub fn enabled() -> bool {
    let value = std::env::var("OPENCODE_FILEWATCHER_DISABLE").or_else(|_| std::env::var("OPENCODE_DISABLE_FILEWATCHER")).ok();
    !value.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

#[cfg(target_os = "linux")]
mod inotify {
    use std::collections::{HashMap, HashSet};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::{Arc, Mutex, OnceLock};

    use tokio::io::unix::AsyncFd;

    use super::Sender;

    const MASK: u32 = libc::IN_ATTRIB
        | libc::IN_CREATE
        | libc::IN_MODIFY
        | libc::IN_CLOSE_WRITE
        | libc::IN_DELETE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF
        | libc::IN_ONLYDIR;

    enum Subscriber {
        /// One name in one directory.
        File { directory: String, name: String, send: Sender },
        /// A tree: its root, the filter of paths relative to it, and its directories' descriptors.
        Tree { root: String, ignore: fn(&str) -> bool, send: Sender, descriptors: HashSet<i32> },
    }

    struct Registry {
        fd: Arc<AsyncFd<OwnedFd>>,
        /// Descriptor → its directory and the subscribers that use it.
        descriptors: HashMap<i32, (String, HashSet<u64>)>,
        subscribers: HashMap<u64, Subscriber>,
        next: u64,
    }

    static REGISTRY: OnceLock<Option<Mutex<Registry>>> = OnceLock::new();

    fn registry() -> Option<&'static Mutex<Registry>> {
        REGISTRY
            .get_or_init(|| {
                // SAFETY: inotify_init1 has no preconditions; a non-negative result is a new descriptor we own.
                let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
                if raw < 0 {
                    return None;
                }
                // SAFETY: `raw` was just returned by inotify_init1 and is owned by nothing else; the OwnedFd keeps it open
                // for as long as the AsyncFd owns it.
                let fd = Arc::new(unsafe { AsyncFd::register(OwnedFd::from_raw_fd(raw)) }.ok()?);
                tokio::spawn(read_events(fd.clone()));
                Some(Mutex::new(Registry { fd, descriptors: HashMap::new(), subscribers: HashMap::new(), next: 0 }))
            })
            .as_ref()
    }

    fn join(directory: &str, name: &str) -> String {
        if name.is_empty() {
            directory.to_owned()
        } else if directory.ends_with('/') {
            format!("{directory}{name}")
        } else {
            format!("{directory}/{name}")
        }
    }

    fn relative<'a>(root: &str, path: &'a str) -> &'a str {
        path.strip_prefix(root).unwrap_or(path).trim_start_matches('/')
    }

    impl Registry {
        /// Watches a directory for a subscriber; its descriptor.
        fn add(&mut self, directory: &str, id: u64) -> Option<i32> {
            let path = std::ffi::CString::new(directory).ok()?;
            // SAFETY: the descriptor is a live inotify instance and `path` is NUL-terminated.
            let wd = unsafe { libc::inotify_add_watch(self.fd.get_ref().as_raw_fd(), path.as_ptr(), MASK) };
            if wd < 0 {
                return None;
            }
            self.descriptors.entry(wd).or_insert_with(|| (directory.to_owned(), HashSet::new())).1.insert(id);
            Some(wd)
        }

        /// Watches a directory and every directory below it that the subscriber does not ignore.
        fn add_tree(&mut self, root: &str, directory: &str, ignore: fn(&str) -> bool, id: u64) -> Vec<i32> {
            let Some(wd) = self.add(directory, id) else { return vec![] };
            let mut out = vec![wd];
            let Ok(entries) = std::fs::read_dir(directory) else { return out };
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let child = join(directory, &entry.file_name().to_string_lossy());
                if !ignore(relative(root, &child)) {
                    out.extend(self.add_tree(root, &child, ignore, id));
                }
            }
            out
        }

        fn release(&mut self, wd: i32, id: u64) {
            let Some((_, users)) = self.descriptors.get_mut(&wd) else { return };
            users.remove(&id);
            if users.is_empty() {
                self.descriptors.remove(&wd);
                // SAFETY: `wd` is a descriptor of this live inotify instance.
                unsafe { libc::inotify_rm_watch(self.fd.get_ref().as_raw_fd(), wd) };
            }
        }

        fn unsubscribe(&mut self, id: u64) {
            let used: Vec<i32> = match self.subscribers.remove(&id) {
                Some(Subscriber::Tree { descriptors, .. }) => descriptors.into_iter().collect(),
                Some(Subscriber::File { .. }) => {
                    self.descriptors.iter().filter(|(_, (_, users))| users.contains(&id)).map(|(wd, _)| *wd).collect()
                }
                None => vec![],
            };
            for wd in used {
                self.release(wd, id);
            }
        }

        fn dispatch(&mut self, wd: i32, mask: u32, name: &str) {
            if mask & libc::IN_IGNORED != 0 {
                if let Some((_, users)) = self.descriptors.remove(&wd) {
                    for id in users {
                        match self.subscribers.get_mut(&id) {
                            Some(Subscriber::Tree { descriptors, .. }) => {
                                descriptors.remove(&wd);
                            }
                            // Its directory is gone: the watch ends (its sender is dropped).
                            Some(Subscriber::File { .. }) => {
                                self.subscribers.remove(&id);
                            }
                            None => {}
                        }
                    }
                }
                return;
            }
            let Some((directory, users)) = self.descriptors.get(&wd).cloned() else { return };
            let path = join(&directory, name);
            let created_directory = mask & libc::IN_ISDIR != 0 && mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0;
            for id in users {
                let mut grow = None;
                match self.subscribers.get(&id) {
                    Some(Subscriber::File { directory: watched, name: wanted, send }) => {
                        if *watched == directory && name == wanted {
                            let _ = send.send(path.clone());
                        }
                    }
                    Some(Subscriber::Tree { root, ignore, send, .. }) => {
                        let rel = relative(root, &path);
                        if !rel.is_empty() && ignore(rel) {
                            continue;
                        }
                        if created_directory {
                            grow = Some((root.clone(), *ignore));
                        }
                        let _ = send.send(path.clone());
                    }
                    None => {}
                }
                if let Some((root, ignore)) = grow {
                    let added = self.add_tree(&root, &path, ignore, id);
                    if let Some(Subscriber::Tree { descriptors, .. }) = self.subscribers.get_mut(&id) {
                        descriptors.extend(added);
                    }
                }
            }
        }
    }

    /// Reads the instance's events and hands them to the subscribers.
    async fn read_events(fd: Arc<AsyncFd<OwnedFd>>) {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let Ok(mut guard) = fd.readable().await else { return };
            let read = guard.try_io(|inner| {
                // SAFETY: the buffer is valid for its length; inotify writes whole events into it.
                let n = unsafe { libc::read(inner.get_ref().as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
                if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(n as usize) }
            });
            let n = match read {
                Ok(Ok(n)) => n,
                Ok(Err(_)) => return,
                Err(_) => continue,
            };
            let Some(registry) = REGISTRY.get().and_then(Option::as_ref) else { return };
            let mut registry = registry.lock().unwrap();
            let mut offset = 0usize;
            while offset + 16 <= n {
                let field = |at: usize| u32::from_ne_bytes(buffer[offset + at..offset + at + 4].try_into().unwrap());
                let (wd, mask, len) = (field(0) as i32, field(4), field(12) as usize);
                let raw = &buffer[offset + 16..(offset + 16 + len).min(n)];
                let name = String::from_utf8_lossy(raw).trim_end_matches('\0').to_owned();
                registry.dispatch(wd, mask, &name);
                offset += 16 + len;
            }
        }
    }

    pub fn unsubscribe(id: u64) {
        if let Some(registry) = REGISTRY.get().and_then(Option::as_ref) {
            registry.lock().unwrap().unsubscribe(id);
        }
    }

    /// A file subscription: its ID.
    pub fn file(directory: String, name: String, send: &Sender) -> Option<u64> {
        let mut registry = registry()?.lock().unwrap();
        let id = registry.next;
        registry.next += 1;
        registry.add(&directory, id)?;
        registry.subscribers.insert(id, Subscriber::File { directory, name, send: send.clone() });
        Some(id)
    }

    /// A tree subscription: its ID.
    pub fn directory(root: String, ignore: fn(&str) -> bool, send: &Sender) -> Option<u64> {
        let mut registry = registry()?.lock().unwrap();
        let id = registry.next;
        registry.next += 1;
        let descriptors: HashSet<i32> = registry.add_tree(&root, &root, ignore, id).into_iter().collect();
        if descriptors.is_empty() {
            return None;
        }
        registry.subscribers.insert(id, Subscriber::Tree { root, ignore, send: send.clone(), descriptors });
        Some(id)
    }
}

/// Watches one file: changes to its name in its directory.
pub fn file(path: &str, send: &Sender) -> Option<Watch> {
    if !enabled() {
        return None;
    }
    let path = crate::project::normalize(path);
    let directory = crate::inventory::config::dirname(&path);
    let name = crate::inventory::config::basename(&path).to_owned();
    #[cfg(target_os = "linux")]
    return inotify::file(directory, name, send).map(|id| Watch(Stop::Subscription(id)));
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (directory, name, send);
        None
    }
}

/// Watches a directory tree; `ignore` gets each path relative to the root.
pub fn directory(path: &str, ignore: fn(&str) -> bool, send: &Sender) -> Option<Watch> {
    if !enabled() {
        return None;
    }
    let root = crate::project::normalize(path);
    #[cfg(target_os = "linux")]
    return inotify::directory(root, ignore, send).map(|id| Watch(Stop::Subscription(id)));
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (root, ignore, send);
        None
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn reports_changes() {
        let root = std::env::temp_dir().join(format!("oc-watch-{}", std::process::id()));
        let root = root.to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(format!("{root}/tree/sub")).unwrap();
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let tree = directory(&format!("{root}/tree"), |rel| rel.split('/').any(|c| c.starts_with('.')), &send).unwrap();
        let _file = file(&format!("{root}/watched.txt"), &send).unwrap();
        std::fs::write(format!("{root}/tree/.hidden"), "x").unwrap();
        std::fs::write(format!("{root}/tree/sub/a.md"), "x").unwrap();
        std::fs::write(format!("{root}/other.txt"), "x").unwrap();
        std::fs::write(format!("{root}/watched.txt"), "x").unwrap();
        std::fs::create_dir_all(format!("{root}/tree/new")).unwrap();
        let mut seen = Vec::new();
        while let Ok(Some(path)) = tokio::time::timeout(std::time::Duration::from_millis(200), receive.recv()).await {
            seen.push(path);
        }
        std::fs::write(format!("{root}/tree/new/b.md"), "x").unwrap();
        while let Ok(Some(path)) = tokio::time::timeout(std::time::Duration::from_millis(200), receive.recv()).await {
            seen.push(path);
        }
        assert!(seen.contains(&format!("{root}/tree/sub/a.md")));
        assert!(seen.contains(&format!("{root}/tree/new/b.md")));
        assert!(seen.contains(&format!("{root}/watched.txt")));
        assert!(!seen.iter().any(|p| p.ends_with(".hidden") || p.ends_with("other.txt")));
        drop(tree);
        std::fs::write(format!("{root}/tree/sub/c.md"), "x").unwrap();
        assert!(tokio::time::timeout(std::time::Duration::from_millis(200), receive.recv()).await.is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
