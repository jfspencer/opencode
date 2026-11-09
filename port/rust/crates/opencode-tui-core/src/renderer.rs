//! `CliRenderer` (renderer.ts) as the TUI configures it: on-demand rendering (a frame when something asks for
//! one, at most `maxFps`), live mode while a node is live, the terminal session and its replies
//! ([`crate::terminal`]), diffed output ([`crate::output`]), keys (global listeners, then the focused node),
//! pastes, mouse dispatch (hit testing, over/out, capture, drag and drop, autoFocus off), resize on SIGWINCH,
//! destroy on SIGHUP, and the event loop the rest of the TUI runs on (timers, ticks, async tasks).
//!
//! The loop is a task on the current-thread runtime (inside a `LocalSet`): it reads stdin, applies resizes,
//! fires timers and frames, and writes output. Anything else (HTTP, event streams) runs as other local tasks
//! and reaches the screen through the reactive layer, which asks for frames.
//!
//! Divergences: the console overlay, debug overlay, split-footer/scrollback modes, post-process functions,
//! images and the output backpressure of the native session are not ported (the TUI uses none of them).
//! Selection is TODO(port-tui).
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::os::fd::RawFd;
use std::rc::Rc;
use std::time::Duration;

use tokio::io::unix::AsyncFd;
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::color::RGBA;
use crate::input::{Event, KeyEvent, Parser, PasteEvent, RawMouse};
use crate::output;
use crate::terminal::{self, Pump, ReplyEffects, Session, ThemeMode};
use crate::tree::{self, Element, MouseEvent, MouseEventType, ScrollDirection, ScrollInfo};

/// The options of `createCliRenderer` the TUI passes (app.tsx).
#[derive(Clone, Debug)]
pub struct RendererConfig {
    pub use_mouse: bool,
    pub target_fps: u32,
    pub max_fps: u32,
    pub exit_on_ctrl_c: bool,
    pub kitty_keyboard_flags: u8,
}

impl Default for RendererConfig {
    fn default() -> Self {
        RendererConfig { use_mouse: true, target_fps: 60, max_fps: 60, exit_on_ctrl_c: false, kitty_keyboard_flags: 0b101 }
    }
}

type KeyListener = Rc<dyn Fn(&mut KeyEvent)>;
type PasteListener = Rc<dyn Fn(&mut PasteEvent)>;

struct State {
    output: Rc<RefCell<output::Renderer>>,
    session: Session,
    replies: terminal::Replies,
    parser: Parser,
    width: u32,
    height: u32,
    use_mouse: bool,
    destroyed: bool,
    destroy_requested: bool,
    terminal_setup: bool,
    rendering: bool,
    immediate_rerender: bool,
    frame_deadline: Option<Instant>,
    ready_frame: bool,
    last_time: Instant,
    start: Instant,
    target_frame_ms: f64,
    min_frame_ms: f64,
    live_requests: u32,
    running: bool,
    frame_id: u64,
    force_full_repaint: bool,
    next_listener: u64,
    key_listeners: Vec<(u64, KeyListener)>,
    paste_listeners: Vec<(u64, PasteListener)>,
    resize_listeners: Vec<(u64, Rc<dyn Fn(u32, u32)>)>,
    frame_listeners: Vec<(u64, Rc<dyn Fn()>, bool)>,
    destroy_listeners: Vec<Rc<dyn Fn()>>,
    theme_listeners: Vec<(u64, Rc<dyn Fn(ThemeMode)>)>,
    focus_listeners: Vec<(u64, Rc<dyn Fn(bool)>)>,
    // Mouse (processSingleMouseEvent).
    captured: Option<Element>,
    pressed: Option<Element>,
    last_over: Option<Element>,
    last_over_num: u32,
    pointer: Option<(i32, i32)>,
    // Timers and ticks.
    ticks: VecDeque<Box<dyn FnOnce()>>,
    timers: BTreeMap<(Instant, u64), Box<dyn FnOnce()>>,
    next_timer: u64,
    escape_deadline: Option<Instant>,
    session_deadline: Option<Instant>,
    out: Vec<u8>,
    resize_deadline: Option<Instant>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static WAKE: Rc<Notify> = Rc::new(Notify::new());
    static THEME: Rc<Notify> = Rc::new(Notify::new());
    static CLOSED: Rc<Notify> = Rc::new(Notify::new());
    static FOCUSED: Cell<Option<Element>> = const { Cell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
}

fn wake() {
    WAKE.with(|w| w.notify_one());
}

fn now_ms(state: &State) -> u64 {
    state.start.elapsed().as_millis() as u64
}

// ---------------------------------------------------------------------------------------------------------------
// Public API (CliRenderer)
// ---------------------------------------------------------------------------------------------------------------

/// `requestRender()`.
pub fn request_render() {
    let scheduled = with(|s| {
        if s.destroyed {
            return false;
        }
        if s.running {
            if !s.rendering {
                schedule_render_timer(s);
            }
            return true;
        }
        if s.rendering {
            s.immediate_rerender = true;
            return true;
        }
        if !s.ready_frame && s.frame_deadline.is_none() {
            let delay = next_frame_delay(s);
            if delay == 0.0 {
                s.ready_frame = true;
            } else {
                s.frame_deadline = Some(Instant::now() + Duration::from_micros((delay * 1000.0) as u64));
            }
        }
        true
    });
    if scheduled == Some(true) {
        wake();
    }
}

fn next_frame_delay(s: &State) -> f64 {
    let elapsed = s.last_time.elapsed().as_secs_f64() * 1000.0;
    (s.min_frame_ms - elapsed).max(0.0)
}

fn schedule_render_timer(s: &mut State) {
    if s.frame_deadline.is_some() || s.ready_frame || s.destroyed {
        return;
    }
    let delay = next_frame_delay(s);
    s.frame_deadline = Some(Instant::now() + Duration::from_micros((delay * 1000.0) as u64));
}

/// `requestLive()`: render continuously at the target fps while any node is live.
pub fn request_live() {
    with(|s| {
        s.live_requests += 1;
        if !s.running {
            s.running = true;
            s.ready_frame = true;
        }
    });
    wake();
}

/// `dropLive()`.
pub fn drop_live() {
    with(|s| {
        if s.live_requests == 0 {
            return;
        }
        s.live_requests -= 1;
        if s.live_requests == 0 {
            s.running = false;
        }
    });
}

/// Runs `f` at the end of the current turn (`process.nextTick` / `queueMicrotask`).
pub fn next_tick(f: impl FnOnce() + 'static) {
    with(|s| s.ticks.push_back(Box::new(f)));
    wake();
}

/// The reconciler's deferred destroys run at the end of the turn.
pub(crate) fn schedule_tick() {
    wake();
}

/// `setTimeout(f, ms)`; returns an id for [`clear_timeout`].
pub fn set_timeout(ms: u64, f: impl FnOnce() + 'static) -> u64 {
    let id = with(|s| {
        s.next_timer += 1;
        let id = s.next_timer;
        s.timers.insert((Instant::now() + Duration::from_millis(ms), id), Box::new(f));
        id
    })
    .unwrap_or(0);
    wake();
    id
}

/// `clearTimeout(id)`.
pub fn clear_timeout(id: u64) {
    with(|s| s.timers.retain(|(_, timer), _| *timer != id));
}

pub fn width() -> u32 {
    with(|s| s.width).unwrap_or(80)
}

pub fn height() -> u32 {
    with(|s| s.height).unwrap_or(24)
}

pub fn is_destroyed() -> bool {
    with(|s| s.destroyed || s.destroy_requested).unwrap_or(true)
}

pub fn frame_id() -> u64 {
    with(|s| s.frame_id).unwrap_or(0)
}

/// `keyInput.on("keypress", handler)`: global handlers run before the focused node, in registration order.
pub fn on_key(handler: impl Fn(&mut KeyEvent) + 'static) -> u64 {
    with(|s| {
        s.next_listener += 1;
        s.key_listeners.push((s.next_listener, Rc::new(handler)));
        s.next_listener
    })
    .unwrap_or(0)
}

/// `keyInput.prependListener("keypress", handler)`.
pub fn prepend_on_key(handler: impl Fn(&mut KeyEvent) + 'static) -> u64 {
    with(|s| {
        s.next_listener += 1;
        s.key_listeners.insert(0, (s.next_listener, Rc::new(handler)));
        s.next_listener
    })
    .unwrap_or(0)
}

pub fn off_key(token: u64) {
    with(|s| s.key_listeners.retain(|(t, _)| *t != token));
}

pub fn on_paste(handler: impl Fn(&mut PasteEvent) + 'static) -> u64 {
    with(|s| {
        s.next_listener += 1;
        s.paste_listeners.push((s.next_listener, Rc::new(handler)));
        s.next_listener
    })
    .unwrap_or(0)
}

pub fn off_paste(token: u64) {
    with(|s| s.paste_listeners.retain(|(t, _)| *t != token));
}

/// `on("resize", (width, height) => ...)`.
pub fn on_resize(handler: impl Fn(u32, u32) + 'static) -> u64 {
    with(|s| {
        s.next_listener += 1;
        s.resize_listeners.push((s.next_listener, Rc::new(handler)));
        s.next_listener
    })
    .unwrap_or(0)
}

pub fn off_resize(token: u64) {
    with(|s| s.resize_listeners.retain(|(t, _)| *t != token));
}

/// `on(FRAME, ...)` (`once` when `once`).
pub fn on_frame(handler: impl Fn() + 'static, once: bool) -> u64 {
    with(|s| {
        s.next_listener += 1;
        s.frame_listeners.push((s.next_listener, Rc::new(handler), once));
        s.next_listener
    })
    .unwrap_or(0)
}

pub fn off_frame(token: u64) {
    with(|s| s.frame_listeners.retain(|(t, _, _)| *t != token));
}

/// `once("destroy", ...)`.
pub fn on_destroy(handler: impl Fn() + 'static) {
    with(|s| s.destroy_listeners.push(Rc::new(handler)));
}

/// `on("theme_mode", ...)`.
pub fn on_theme_mode(handler: impl Fn(ThemeMode) + 'static) -> u64 {
    with(|s| {
        s.next_listener += 1;
        s.theme_listeners.push((s.next_listener, Rc::new(handler)));
        s.next_listener
    })
    .unwrap_or(0)
}

/// `on("focus" | "blur", ...)`: terminal focus.
pub fn on_terminal_focus(handler: impl Fn(bool) + 'static) -> u64 {
    with(|s| {
        s.next_listener += 1;
        s.focus_listeners.push((s.next_listener, Rc::new(handler)));
        s.next_listener
    })
    .unwrap_or(0)
}

pub fn theme_mode() -> Option<ThemeMode> {
    with(|s| s.replies.theme_mode()).flatten()
}

/// `waitForThemeMode(timeoutMs)`.
pub async fn wait_for_theme_mode(timeout_ms: u64) -> Option<ThemeMode> {
    if let Some(mode) = theme_mode() {
        return Some(mode);
    }
    let notify = THEME.with(|t| t.clone());
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        tokio::select! {
            _ = notify.notified() => {
                if let Some(mode) = theme_mode() { return Some(mode) }
            }
            _ = tokio::time::sleep_until(deadline) => return theme_mode(),
        }
        if is_destroyed() {
            return None;
        }
    }
}

/// `useMouse = value`.
pub fn set_use_mouse(enabled: bool) {
    with(|s| {
        if s.use_mouse == enabled {
            return;
        }
        s.use_mouse = enabled;
        if s.terminal_setup {
            let mode = if enabled { terminal::MouseMode::Motion } else { terminal::MouseMode::Disabled };
            let output = s.output.clone();
            let _ = s.session.control(&mut output.borrow_mut(), terminal::Control::Mouse(mode), &mut s.out);
        }
    });
    wake();
}

/// `setTerminalTitle(title)`.
pub fn set_terminal_title(title: &str) {
    with(|s| {
        let output = s.output.clone();
        let _ = s.session.control(&mut output.borrow_mut(), terminal::Control::Title(title.to_string()), &mut s.out);
    });
    wake();
}

/// `setBackgroundColor(color)`.
pub fn set_background_color(color: RGBA) {
    with(|s| s.output.borrow_mut().set_background_color(color));
    request_render();
}

/// `setCursorPosition(x, y, visible)` (0-based cells).
pub fn set_cursor_position(x: u32, y: u32, visible: bool) {
    with(|s| s.output.borrow_mut().terminal.set_cursor_position(x + 1, y + 1, visible));
}

pub fn set_cursor_style(style: terminal::CursorStyle, blinking: bool) {
    with(|s| s.output.borrow_mut().terminal.set_cursor_style(style, blinking));
}

pub fn set_cursor_color(color: RGBA) {
    with(|s| s.output.borrow_mut().terminal.set_cursor_color(color));
}

/// Writes raw bytes to the terminal with the next output (OSC sequences the app emits itself).
pub fn write_raw(bytes: &[u8]) {
    with(|s| s.out.extend_from_slice(bytes));
    wake();
}

/// Copies to the system clipboard through OSC 52.
pub fn copy_to_clipboard_osc52(text: &str) -> bool {
    with(|s| {
        let output = s.output.clone();
        let out = &mut s.out;
        s.session.write_clipboard(&output.borrow(), terminal::ClipboardTarget::Clipboard, text.as_bytes(), out)
    })
    .unwrap_or(false)
}

/// `destroy()`: the terminal is restored by the loop, which then ends.
pub fn destroy() {
    let first = with(|s| {
        let first = !s.destroy_requested;
        s.destroy_requested = true;
        first
    })
    .unwrap_or(false);
    if first {
        let listeners = with(|s| s.destroy_listeners.clone()).unwrap_or_default();
        for listener in listeners {
            listener();
        }
    }
    wake();
}

/// Resolves once the terminal is restored (`renderer.closed`).
pub async fn closed() {
    let notify = CLOSED.with(|c| c.clone());
    if with(|s| s.destroyed).unwrap_or(true) {
        return;
    }
    notify.notified().await;
}

pub(crate) fn focus_changed(el: Option<Element>) {
    FOCUSED.with(|f| f.set(el));
}

pub fn current_focused_renderable() -> Option<Element> {
    tree::focused()
}

// ---------------------------------------------------------------------------------------------------------------
// Creation and the loop
// ---------------------------------------------------------------------------------------------------------------

struct Fd(RawFd);
impl std::os::fd::AsRawFd for Fd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

struct TerminalGuard {
    termios: Option<libc::termios>,
    stdin_flags: i32,
    stdout_flags: i32,
}

impl TerminalGuard {
    fn enter() -> TerminalGuard {
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            let saved = if libc::tcgetattr(0, &mut termios) == 0 {
                let original = termios;
                // libuv's UV_TTY_MODE_RAW.
                termios.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
                termios.c_oflag |= libc::ONLCR;
                termios.c_cflag |= libc::CS8;
                termios.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG);
                termios.c_cc[libc::VMIN] = 1;
                termios.c_cc[libc::VTIME] = 0;
                libc::tcsetattr(0, libc::TCSADRAIN, &termios);
                Some(original)
            } else {
                None
            };
            let stdin_flags = libc::fcntl(0, libc::F_GETFL);
            libc::fcntl(0, libc::F_SETFL, stdin_flags | libc::O_NONBLOCK);
            let stdout_flags = libc::fcntl(1, libc::F_GETFL);
            TerminalGuard { termios: saved, stdin_flags, stdout_flags }
        }
    }

    fn leave(&self) {
        unsafe {
            libc::fcntl(0, libc::F_SETFL, self.stdin_flags);
            libc::fcntl(1, libc::F_SETFL, self.stdout_flags);
            if let Some(termios) = &self.termios {
                libc::tcsetattr(0, libc::TCSADRAIN, termios);
            }
        }
    }
}

fn terminal_size() -> (u32, u32) {
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_col > 0 && size.ws_row > 0 {
            (size.ws_col as u32, size.ws_row as u32)
        } else {
            (80, 24)
        }
    }
}

/// Writes all of `bytes` to stdout (blocking: a frame is small and the terminal reads it promptly).
fn write_all(mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let n = unsafe { libc::write(1, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == std::io::ErrorKind::WouldBlock {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            return;
        }
        bytes = &bytes[n as usize..];
    }
}

fn flush() {
    let out = with(|s| std::mem::take(&mut s.out)).unwrap_or_default();
    if !out.is_empty() {
        write_all(&out);
    }
}

/// `createCliRenderer(config)`: raw mode, the terminal setup (queries, alternate screen, modes), the root node,
/// and the loop task. Must run inside a `LocalSet`.
pub async fn create_cli_renderer(config: RendererConfig) -> std::io::Result<()> {
    let guard = Rc::new(TerminalGuard::enter());
    let (width, height) = terminal_size();
    let term = terminal::Terminal::new(terminal::RemoteMode::Auto, Some(terminal::Terminal::forwarded_environment()));
    let output = Rc::new(RefCell::new(output::Renderer::new(width, height, term)));
    let now = Instant::now();
    let state = State {
        output: output.clone(),
        session: Session::new(),
        replies: terminal::Replies::new(),
        parser: Parser::new(),
        width,
        height,
        use_mouse: config.use_mouse,
        destroyed: false,
        destroy_requested: false,
        terminal_setup: false,
        rendering: false,
        immediate_rerender: false,
        frame_deadline: None,
        ready_frame: false,
        last_time: now - Duration::from_secs(1),
        start: now,
        target_frame_ms: 1000.0 / config.target_fps as f64,
        min_frame_ms: 1000.0 / config.max_fps as f64,
        live_requests: 0,
        running: false,
        frame_id: 0,
        force_full_repaint: false,
        next_listener: 0,
        key_listeners: Vec::new(),
        paste_listeners: Vec::new(),
        resize_listeners: Vec::new(),
        frame_listeners: Vec::new(),
        destroy_listeners: Vec::new(),
        theme_listeners: Vec::new(),
        focus_listeners: Vec::new(),
        captured: None,
        pressed: None,
        last_over: None,
        last_over_num: 0,
        pointer: None,
        ticks: VecDeque::new(),
        timers: BTreeMap::new(),
        next_timer: 0,
        escape_deadline: None,
        session_deadline: None,
        out: Vec::new(),
        resize_deadline: None,
    };
    STATE.with(|s| *s.borrow_mut() = Some(state));
    tree::create_root(width, height);
    with(|s| {
        s.replies.before_setup(&output.borrow());
        let options = terminal::TerminalOptions {
            mouse: config.use_mouse,
            kitty_keyboard_flags: config.kitty_keyboard_flags,
            ..terminal::TerminalOptions::default()
        };
        let _ = s.session.setup_terminal(&mut output.borrow_mut(), options);
    });
    // SAFETY: fd 0 stays open for the process; the AsyncFd is dropped when the loop ends.
    let stdin = unsafe { AsyncFd::register(Fd(0)) }.map_err(|e| e.into_parts().1)?;
    // Setup: pump until the session is idle, answering replies as they arrive.
    loop {
        let pump = with(|s| {
            let now = now_ms(s);
            let output = s.output.clone();
            s.session.pump(&mut output.borrow_mut(), now, &mut s.out)
        })
        .unwrap();
        flush();
        match pump {
            Pump::Again => continue,
            Pump::Idle | Pump::Closed => break,
            Pump::WaitUntil(at) => {
                let start = with(|s| s.start).unwrap();
                let deadline = start + Duration::from_millis(at);
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep_until(deadline) => break,
                        ready = stdin.readable() => {
                            if let Ok(mut ready) = ready {
                                read_stdin(&mut ready);
                            }
                        }
                    }
                }
            }
        }
    }
    with(|s| {
        s.terminal_setup = true;
        let now = now_ms(s);
        let output = s.output.clone();
        s.replies.after_setup(&mut s.session, &mut output.borrow_mut(), now, &mut s.out);
    });
    flush();
    tokio::task::spawn_local(run_loop(stdin, guard));
    Ok(())
}

fn read_stdin(ready: &mut tokio::io::unix::AsyncFdReadyGuard<'_, Fd>) {
    let mut buf = [0u8; 8192];
    loop {
        let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            handle_input(&buf[..n as usize]);
            continue;
        }
        if n == 0 {
            // EOF: the terminal is gone.
            destroy();
            ready.clear_ready();
            return;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            ready.clear_ready();
        }
        return;
    }
}

const ESCAPE_TIMEOUT_MS: u64 = 10;

fn handle_input(bytes: &[u8]) {
    let events = with(|s| s.parser.feed(bytes)).unwrap_or_default();
    with(|s| s.escape_deadline = s.parser.has_pending().then(|| Instant::now() + Duration::from_millis(ESCAPE_TIMEOUT_MS)));
    for event in events {
        dispatch_event(event);
    }
}

fn apply_effects(effects: ReplyEffects) {
    if effects.capabilities_changed {
        with(|s| s.force_full_repaint = true);
        request_render();
    }
    if effects.resolution.is_some() || effects.palette_applied {
        request_render();
    }
    if let Some(mode) = effects.theme_mode {
        THEME.with(|t| t.notify_waiters());
        let listeners = with(|s| s.theme_listeners.clone()).unwrap_or_default();
        for (_, listener) in listeners {
            listener(mode);
        }
    }
    if let Some(focus) = effects.focus {
        let listeners = with(|s| s.focus_listeners.clone()).unwrap_or_default();
        for (_, listener) in listeners {
            listener(focus);
        }
    }
}

fn dispatch_event(event: Event) {
    match event {
        Event::Response(sequence) => {
            let effects = with(|s| {
                let now = now_ms(s);
                let output = s.output.clone();
                let mut out = std::mem::take(&mut s.out);
                let mut effects = ReplyEffects::default();
                if sequence.ends_with('R') && sequence.starts_with("\x1b[") {
                    effects = s.replies.handle_cpr(&sequence, &mut s.session, &mut output.borrow_mut(), &mut out);
                } else if sequence.starts_with("\x1b]") {
                    effects = s.replies.handle_osc(&sequence, &mut s.session, &mut output.borrow_mut(), now, &mut out);
                }
                if !effects.handled {
                    let more = s.replies.handle_sequence(&sequence, &mut s.session, &mut output.borrow_mut(), now, &mut out);
                    effects = merge(effects, more);
                }
                s.out = out;
                effects
            });
            if let Some(effects) = effects {
                apply_effects(effects);
            }
            flush();
        }
        Event::Focus(focused) => {
            let sequence = if focused { "\x1b[I" } else { "\x1b[O" };
            let effects = with(|s| {
                let now = now_ms(s);
                let output = s.output.clone();
                let mut out = std::mem::take(&mut s.out);
                let e = s.replies.handle_sequence(sequence, &mut s.session, &mut output.borrow_mut(), now, &mut out);
                s.out = out;
                e
            });
            if let Some(effects) = effects {
                apply_effects(effects);
            }
            flush();
        }
        Event::Key(key) => {
            let mut event = KeyEvent::new(key);
            let listeners = with(|s| s.key_listeners.clone()).unwrap_or_default();
            for (_, listener) in listeners {
                listener(&mut event);
                if event.propagation_stopped {
                    return;
                }
            }
            if event.default_prevented || event.propagation_stopped {
                return;
            }
            if let Some(focused) = tree::focused() {
                focused.dispatch_key(&mut event);
            }
        }
        Event::Paste(bytes) => {
            let mut event = PasteEvent { bytes, ..PasteEvent::default() };
            let listeners = with(|s| s.paste_listeners.clone()).unwrap_or_default();
            for (_, listener) in listeners {
                listener(&mut event);
                if event.propagation_stopped {
                    return;
                }
            }
            if event.default_prevented {
                return;
            }
            if let Some(focused) = tree::focused() {
                focused.dispatch_paste(&mut event);
            }
        }
        Event::Mouse(raw) => {
            if with(|s| s.use_mouse).unwrap_or(false) {
                process_mouse(raw);
            }
        }
    }
}

fn merge(a: ReplyEffects, b: ReplyEffects) -> ReplyEffects {
    ReplyEffects {
        handled: a.handled || b.handled,
        capabilities_changed: a.capabilities_changed || b.capabilities_changed,
        theme_mode: b.theme_mode.or(a.theme_mode),
        resolution: b.resolution.or(a.resolution),
        focus: b.focus.or(a.focus),
        palette_applied: a.palette_applied || b.palette_applied,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Mouse (processSingleMouseEvent)
// ---------------------------------------------------------------------------------------------------------------

const LEFT: u32 = 0;

fn mouse_event(kind: MouseEventType, raw: &RawMouse, target: Option<Element>) -> MouseEvent {
    let button = raw.button & 3;
    let scroll = if raw.button & 64 != 0 {
        Some(ScrollInfo {
            direction: match raw.button & 3 {
                0 => ScrollDirection::Up,
                1 => ScrollDirection::Down,
                2 => ScrollDirection::Left,
                _ => ScrollDirection::Right,
            },
            delta: 1.0,
        })
    } else {
        None
    };
    MouseEvent {
        kind,
        button: if raw.button & 64 != 0 { 0 } else { button },
        x: raw.x,
        y: raw.y,
        shift: raw.button & 4 != 0,
        alt: raw.button & 8 != 0,
        ctrl: raw.button & 16 != 0,
        scroll,
        target,
        current_target: None,
        source: None,
        is_dragging: false,
        propagation_stopped: false,
        default_prevented: false,
    }
}

fn hit_test(x: i32, y: i32) -> Option<Element> {
    if x < 0 || y < 0 {
        return None;
    }
    let num = with(|s| s.output.borrow().check_hit(x as u32, y as u32)).unwrap_or(0);
    if num == 0 { None } else { tree::element_by_num(num) }
}

fn send(target: Element, mut event: MouseEvent) -> MouseEvent {
    if target.is_destroyed() || is_destroyed() {
        return event;
    }
    target.process_mouse_event(&mut event);
    event
}

fn process_mouse(raw: RawMouse) {
    let kind = if raw.button & 64 != 0 {
        MouseEventType::Scroll
    } else if raw.button & 32 != 0 {
        if raw.button & 3 == 3 { MouseEventType::Move } else { MouseEventType::Drag }
    } else if raw.release {
        MouseEventType::Up
    } else {
        MouseEventType::Down
    };
    let button = raw.button & 3;
    let left = button == LEFT && raw.button & 64 == 0;
    let pressed = with(|s| s.pressed).flatten();
    if left && matches!(kind, MouseEventType::Down | MouseEventType::Up) {
        with(|s| s.pressed = None);
    }
    with(|s| s.pointer = Some((raw.x, raw.y)));
    if kind == MouseEventType::Scroll {
        let target = hit_test(raw.x, raw.y).or_else(|| tree::focused().filter(|f| f.focused()));
        if let Some(target) = target {
            send(target, mouse_event(kind, &raw, Some(target)));
        }
        return;
    }
    let found = hit_test(raw.x, raw.y);
    let num = found.map(|e| e.num()).unwrap_or(0);
    let same = with(|s| {
        let same = s.last_over_num == num;
        s.last_over_num = num;
        same
    })
    .unwrap_or(false);
    let captured = with(|s| s.captured).flatten();
    if !same && matches!(kind, MouseEventType::Drag | MouseEventType::Move) {
        let last = with(|s| s.last_over).flatten();
        if let Some(last) = last.filter(|l| Some(*l) != captured && !l.is_destroyed()) {
            send(last, mouse_event(MouseEventType::Out, &raw, Some(last)));
        }
        with(|s| s.last_over = found);
        if let Some(target) = found {
            let mut event = mouse_event(MouseEventType::Over, &raw, Some(target));
            event.source = captured;
            send(target, event);
        }
    }
    if let Some(source) = captured {
        if kind != MouseEventType::Up {
            send(source, mouse_event(kind, &raw, Some(source)));
            return;
        }
        send(source, mouse_event(MouseEventType::DragEnd, &raw, Some(source)));
        send(source, mouse_event(kind, &raw, Some(source)));
        with(|s| {
            s.last_over = Some(source);
            s.last_over_num = source.num();
            s.captured = None;
        });
        request_render();
        if let Some(target) = found {
            let mut within = Some(target);
            while let Some(el) = within {
                if el == source {
                    return;
                }
                within = el.parent();
            }
            let mut event = mouse_event(MouseEventType::Drop, &raw, Some(target));
            event.source = Some(source);
            send(target, event);
        }
    }
    let is_left_drag = left && kind == MouseEventType::Drag;
    let target = if is_left_drag { pressed.filter(|p| !p.is_destroyed()).or(found) } else { found };
    match target {
        Some(target) => {
            if is_left_drag {
                with(|s| s.captured = Some(target));
            } else if left && kind == MouseEventType::Down {
                with(|s| s.pressed = Some(target));
            }
            send(target, mouse_event(kind, &raw, Some(target)));
        }
        None => {
            with(|s| s.last_over = None);
        }
    }
}

/// `recheckHoverState` after a presented frame.
fn recheck_hover() {
    let Some((pointer, captured, last_over)) = with(|s| (s.pointer, s.captured, s.last_over)) else { return };
    let Some((x, y)) = pointer else { return };
    if captured.is_some() {
        return;
    }
    let hit = hit_test(x, y);
    let hit_num = hit.map(|e| e.num()).unwrap_or(0);
    if last_over.map(|e| e.num()) == Some(hit_num) && last_over.is_some() {
        with(|s| s.last_over_num = hit_num);
        return;
    }
    if last_over.is_none() && hit.is_none() {
        return;
    }
    let raw = RawMouse { button: 3 | 32, x, y, release: false };
    if let Some(last) = last_over.filter(|l| !l.is_destroyed()) {
        send(last, mouse_event(MouseEventType::Out, &raw, Some(last)));
    }
    if with(|s| s.last_over != last_over || s.captured.is_some()).unwrap_or(true) {
        return;
    }
    with(|s| {
        s.last_over = hit;
        s.last_over_num = hit_num;
    });
    if let Some(hit) = hit {
        send(hit, mouse_event(MouseEventType::Over, &raw, Some(hit)));
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------------------------------------------

fn run_ticks() {
    loop {
        let tick = with(|s| s.ticks.pop_front()).flatten();
        match tick {
            Some(tick) => tick(),
            None => break,
        }
    }
    tree::flush_pending_destroy();
}

fn fire_due_timers() {
    let now = Instant::now();
    loop {
        let due = with(|s| {
            let key = *s.timers.keys().next()?;
            if key.0 <= now { s.timers.remove(&key) } else { None }
        })
        .flatten();
        match due {
            Some(timer) => {
                timer();
                run_ticks();
            }
            None => break,
        }
    }
    let frame_due = with(|s| {
        if s.frame_deadline.is_some_and(|d| d <= now) {
            s.frame_deadline = None;
            s.ready_frame = true;
        }
        if s.escape_deadline.is_some_and(|d| d <= now) {
            s.escape_deadline = None;
            return Some(s.parser.flush_timeout());
        }
        None
    })
    .flatten();
    if let Some(events) = frame_due {
        for event in events {
            dispatch_event(event);
        }
    }
    // The terminal replies' own deadlines (startup backstop, palette detection).
    let effects = with(|s| {
        let now = now_ms(s);
        if s.replies.next_deadline().is_some_and(|d| d <= now) {
            let output = s.output.clone();
            let mut out = std::mem::take(&mut s.out);
            let e = s.replies.tick(&mut s.session, &mut output.borrow_mut(), now, &mut out);
            s.out = out;
            Some(e)
        } else {
            None
        }
    })
    .flatten();
    if let Some(effects) = effects {
        apply_effects(effects);
    }
    let resize = with(|s| {
        if s.resize_deadline.is_some_and(|d| d <= now) {
            s.resize_deadline = None;
            true
        } else {
            false
        }
    })
    .unwrap_or(false);
    if resize {
        let (w, h) = terminal_size();
        process_resize(w, h);
    }
}

fn next_deadline() -> Option<Instant> {
    with(|s| {
        let replies = s.replies.next_deadline().map(|ms| s.start + Duration::from_millis(ms));
        [s.frame_deadline, s.timers.keys().next().map(|k| k.0), s.escape_deadline, replies, s.resize_deadline, s.session_deadline]
            .into_iter()
            .flatten()
            .min()
    })
    .flatten()
}

/// `processResize`.
fn process_resize(width: u32, height: u32) {
    let changed = with(|s| {
        if s.width == width && s.height == height {
            return false;
        }
        s.width = width;
        s.height = height;
        s.output.borrow_mut().resize(width, height);
        s.captured = None;
        true
    })
    .unwrap_or(false);
    if !changed {
        return;
    }
    tree::resize_root(width, height);
    let listeners = with(|s| s.resize_listeners.clone()).unwrap_or_default();
    for (_, listener) in listeners {
        listener(width, height);
    }
    request_render();
}

/// One frame (`loop()`): layout, paint, diff, FRAME listeners, hover recheck, next frame scheduling.
fn render_frame() {
    let Some((delta, force, output, width, height)) = with(|s| {
        s.ready_frame = false;
        s.frame_deadline = None;
        s.rendering = true;
        s.frame_id += 1;
        let now = Instant::now();
        let delta = now.duration_since(s.last_time).as_secs_f64() * 1000.0;
        s.last_time = now;
        let force = std::mem::take(&mut s.force_full_repaint);
        (delta, force, s.output.clone(), s.width, s.height)
    }) else {
        return;
    };
    tree::prepare_frame(width, height, delta);
    let hits = {
        let mut out = output.borrow_mut();
        tree::paint(out.next_buffer(), delta)
    };
    let mut bytes = Vec::new();
    {
        let mut out = output.borrow_mut();
        for hit in hits {
            out.add_to_hit_grid(hit.x, hit.y, hit.width, hit.height, hit.num);
        }
        out.render(force, &mut bytes);
    }
    with(|s| s.out.extend_from_slice(&bytes));
    flush();
    let listeners = with(|s| {
        let listeners: Vec<_> = s.frame_listeners.iter().map(|(_, l, _)| l.clone()).collect();
        s.frame_listeners.retain(|(_, _, once)| !once);
        listeners
    })
    .unwrap_or_default();
    for listener in listeners {
        listener();
    }
    if with(|s| s.use_mouse).unwrap_or(false) {
        recheck_hover();
    }
    with(|s| {
        s.rendering = false;
        if s.running || s.immediate_rerender {
            let immediate = std::mem::take(&mut s.immediate_rerender);
            let target = if immediate { s.min_frame_ms } else { s.target_frame_ms };
            let remaining = target - s.last_time.elapsed().as_secs_f64() * 1000.0;
            if immediate && remaining <= 0.0 {
                s.ready_frame = true;
            } else {
                s.frame_deadline = Some(Instant::now() + Duration::from_micros((remaining.max(1.0) * 1000.0) as u64));
            }
        }
    });
}

async fn run_loop(stdin: AsyncFd<Fd>, guard: Rc<TerminalGuard>) {
    let wake = WAKE.with(|w| w.clone());
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok();
    let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok();
    loop {
        run_ticks();
        fire_due_timers();
        run_ticks();
        if with(|s| s.destroy_requested).unwrap_or(true) {
            break;
        }
        if with(|s| s.ready_frame).unwrap_or(false) {
            render_frame();
            run_ticks();
            continue;
        }
        flush();
        let deadline = next_deadline();
        let sleep = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            ready = stdin.readable() => {
                if let Ok(mut ready) = ready {
                    read_stdin(&mut ready);
                }
            }
            _ = async { match winch.as_mut() { Some(w) => { w.recv().await; } None => std::future::pending().await } } => {
                // SIGWINCH: resize at once (the renderer debounces only for its own requestResize).
                let (w, h) = terminal_size();
                process_resize(w, h);
            }
            _ = async { match hup.as_mut() { Some(h) => { h.recv().await; } None => std::future::pending().await } } => {
                destroy();
            }
            _ = sleep => {}
            _ = wake.notified() => {}
        }
    }
    // Teardown: the session restores the terminal.
    with(|s| s.session.begin_close());
    loop {
        let pump = with(|s| {
            let now = now_ms(s);
            let output = s.output.clone();
            s.session.pump(&mut output.borrow_mut(), now, &mut s.out)
        });
        flush();
        match pump {
            Some(Pump::Again) => continue,
            Some(Pump::WaitUntil(at)) => {
                let start = with(|s| s.start).unwrap();
                tokio::time::sleep_until(start + Duration::from_millis(at)).await;
            }
            _ => break,
        }
    }
    guard.leave();
    with(|s| s.destroyed = true);
    CLOSED.with(|c| c.notify_waiters());
}
