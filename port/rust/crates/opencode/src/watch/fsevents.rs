//! macOS watches over FSEvents (CoreServices), as the TS server gets them through Parcel: each subscription is an event
//! stream with file-level events on a dispatch queue of its own. A stream is recursive by nature, so a file watch is a
//! stream on the file's directory filtered by the file's name (only that directory's entries), and a tree watch keeps
//! every path below its root that its `ignore` does not reject (new subdirectories need no watch of their own).
//!
//! FSEvents reports real paths (`/private/var/...` for `/var/...`): an event below the root's real path is sent below
//! the root as the subscriber named it, so callers see the same paths as on Linux. Dropping a [`Stream`] stops it.

use std::ffi::{CStr, CString, c_char, c_void};

use super::Sender;

type CFRef = *const c_void;

#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<extern "C" fn(*const c_void)>,
    copy_description: Option<extern "C" fn(*const c_void) -> CFRef>,
}

type Callback = extern "C" fn(*const c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    fn CFStringCreateWithCString(allocator: CFRef, string: *const c_char, encoding: u32) -> CFRef;
    fn CFArrayCreate(allocator: CFRef, values: *const CFRef, count: isize, callbacks: *const c_void) -> CFRef;
    fn CFRelease(object: CFRef);
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        allocator: CFRef,
        callback: Callback,
        context: *const Context,
        paths: CFRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> *mut c_void;
    fn FSEventStreamSetDispatchQueue(stream: *mut c_void, queue: *mut c_void);
    fn FSEventStreamStart(stream: *mut c_void) -> u8;
    fn FSEventStreamStop(stream: *mut c_void);
    fn FSEventStreamInvalidate(stream: *mut c_void);
    fn FSEventStreamRelease(stream: *mut c_void);
}

// libdispatch is part of libSystem.
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attributes: *const c_void) -> *mut c_void;
    fn dispatch_sync_f(queue: *mut c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dispatch_release(object: *mut c_void);
}

const UTF8: u32 = 0x0800_0100;
const SINCE_NOW: u64 = u64::MAX;
const NO_DEFER: u32 = 0x02;
const FILE_EVENTS: u32 = 0x10;
/// Parcel's latency: events are delivered almost at once.
const LATENCY: f64 = 0.001;

/// What a stream sends, and how it maps the paths FSEvents reports.
enum Filter {
    /// One name in one directory.
    File { directory: String, name: String },
    /// A tree minus what `ignore` rejects (relative paths).
    Tree { root: String, ignore: fn(&str) -> bool },
}

struct Subscriber {
    filter: Filter,
    /// The real path of the watched directory, as FSEvents reports it, and the path the subscriber named.
    real: String,
    named: String,
    send: Sender,
}

impl Subscriber {
    fn dispatch(&self, reported: &str) {
        let path = match reported.strip_prefix(&self.real) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", self.named),
            _ => reported.to_owned(),
        };
        match &self.filter {
            Filter::File { directory, name } => {
                let (parent, base) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
                if parent == directory && base == name {
                    let _ = self.send.send(path);
                }
            }
            Filter::Tree { root, ignore } => {
                let relative = path.strip_prefix(root.as_str()).unwrap_or(&path).trim_start_matches('/');
                if relative.is_empty() || !ignore(relative) {
                    let _ = self.send.send(path);
                }
            }
        }
    }
}

extern "C" fn callback(_: *const c_void, info: *mut c_void, count: usize, paths: *mut c_void, _: *const u32, _: *const u64) {
    // SAFETY: `info` is the subscriber the stream was created with, alive until the stream is invalidated and its
    // queue drained; without `kFSEventStreamCreateFlagUseCFTypes`, `paths` is an array of `count` C strings.
    let subscriber = unsafe { &*(info as *const Subscriber) };
    let paths = paths as *const *const c_char;
    for index in 0..count {
        // SAFETY: see above.
        let path = unsafe { CStr::from_ptr(*paths.add(index)) }.to_string_lossy();
        subscriber.dispatch(path.trim_end_matches('/'));
    }
}

extern "C" fn drained(_: *mut c_void) {}

/// A running event stream.
pub struct Stream {
    stream: *mut c_void,
    queue: *mut c_void,
    subscriber: *mut Subscriber,
}

// SAFETY: the stream and queue are thread-safe CoreServices/libdispatch objects; the subscriber is only read by the
// queue's callbacks and freed after they are drained.
unsafe impl Send for Stream {}
unsafe impl Sync for Stream {}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: the stream was started on this queue; after it is invalidated no callback is scheduled, and the
        // empty synchronous work item waits for one already running, so the subscriber can be freed.
        unsafe {
            FSEventStreamStop(self.stream);
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
            dispatch_sync_f(self.queue, std::ptr::null_mut(), drained);
            dispatch_release(self.queue);
            drop(Box::from_raw(self.subscriber));
        }
    }
}

fn start(directory: &str, filter: Filter, send: &Sender) -> Option<Stream> {
    if !std::path::Path::new(directory).is_dir() {
        return None;
    }
    let real = std::fs::canonicalize(directory).ok()?.to_string_lossy().into_owned();
    let path = CString::new(real.clone()).ok()?;
    let subscriber = Box::into_raw(Box::new(Subscriber { filter, real, named: directory.to_owned(), send: send.clone() }));
    let context = Context { version: 0, info: subscriber.cast(), retain: None, release: None, copy_description: None };
    // SAFETY: plain CoreFoundation and CoreServices calls with valid arguments; every object created here is released
    // here or by the returned stream's drop.
    unsafe {
        let string = CFStringCreateWithCString(std::ptr::null(), path.as_ptr(), UTF8);
        let values = [string];
        let paths = CFArrayCreate(std::ptr::null(), values.as_ptr(), 1, &kCFTypeArrayCallBacks as *const c_void);
        CFRelease(string);
        let stream = FSEventStreamCreate(std::ptr::null(), callback, &context, paths, SINCE_NOW, LATENCY, NO_DEFER | FILE_EVENTS);
        CFRelease(paths);
        if stream.is_null() {
            drop(Box::from_raw(subscriber));
            return None;
        }
        let queue = dispatch_queue_create(c"ai.opencode.watch".as_ptr(), std::ptr::null());
        FSEventStreamSetDispatchQueue(stream, queue);
        if FSEventStreamStart(stream) == 0 {
            FSEventStreamInvalidate(stream);
            FSEventStreamRelease(stream);
            dispatch_release(queue);
            drop(Box::from_raw(subscriber));
            return None;
        }
        Some(Stream { stream, queue, subscriber })
    }
}

/// A file watch: the file's directory, filtered by its name.
pub fn file(directory: String, name: String, send: &Sender) -> Option<Stream> {
    let watched = directory.clone();
    start(&watched, Filter::File { directory, name }, send)
}

/// A tree watch.
pub fn directory(root: String, ignore: fn(&str) -> bool, send: &Sender) -> Option<Stream> {
    let watched = root.clone();
    start(&watched, Filter::Tree { root, ignore }, send)
}
