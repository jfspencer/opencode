//! `component/terminal-pane.tsx` (`TerminalPane`): a persistent terminal of the session shown beside the
//! transcript. The pane attaches over the terminal's WebSocket, writes what the server replays and the shell
//! prints, and sends the keys typed into it.
//!
//! The embedded terminal (`@opentui/core`'s `EmbeddedTerminal`) is [`super::terminal_screen::Screen`] here, drawn
//! by a `<text>`; the connection is [`opencode_tui_sync::pty`] and its WebSocket. What the pane shows survives a
//! hide and a show because the terminal lives in the server's daemon: mounting again replays its snapshot.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::input::{KeyEventType, PasteEvent};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Element, MouseEventType, Overflow};
use opencode_tui_sync::client::{Client, Error, RequestOptions};
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::pty::{
    PersistentPtyClient, PersistentPtyConnectInput, PersistentPtyMessage, decode_persistent_pty_message, persistent_pty_frame,
};
use opencode_tui_sync::websocket::{Incoming, SocketHandle};
use opencode_tui_sync::obj;

use crate::ctx::tui;
use crate::keymap;
use crate::util::rgba;

use super::terminal_screen::Screen;

/// The props of `<TerminalPane>`.
pub struct TerminalPaneProps {
    pub pty_id: String,
    pub resizing: Rc<dyn Fn() -> bool>,
    pub auto_focus: Rc<dyn Fn() -> bool>,
    pub on_auto_focus: Option<Rc<dyn Fn()>>,
    pub on_focus_request: Option<Rc<dyn Fn(Option<Rc<dyn Fn()>>)>>,
    pub on_disconnect: Option<Rc<dyn Fn()>>,
}

impl Default for TerminalPaneProps {
    fn default() -> Self {
        Self {
            pty_id: String::new(),
            resizing: Rc::new(|| false),
            auto_focus: Rc::new(|| false),
            on_auto_focus: None,
            on_focus_request: None,
            on_disconnect: None,
        }
    }
}

/// The pane's own state, shared by its handlers (the TS component's closure).
struct Pane {
    screen: Rc<RefCell<Screen>>,
    display: Signal<String>,
    failure: Signal<Option<String>>,
    title: Signal<Option<String>>,
    socket: Rc<RefCell<Option<SocketHandle>>>,
    pending_input: Rc<RefCell<Vec<Vec<u8>>>>,
    size: Rc<Cell<Option<(u16, u16)>>>,
    attached: Rc<Cell<bool>>,
    controller: Rc<Cell<bool>>,
    restored: Rc<Cell<bool>>,
    wants_control: Rc<Cell<bool>>,
    disposed: Rc<Cell<bool>>,
    exited: Rc<Cell<bool>>,
}

impl Pane {
    fn flush_display(&self) {
        self.display.set(self.screen.borrow().content());
    }

    fn resize(&self, cols: u16, rows: u16) {
        self.screen.borrow_mut().resize(cols.max(1), rows.max(1));
        self.flush_display();
    }

    fn write(&self, bytes: &[u8]) {
        self.screen.borrow_mut().write(bytes);
        self.flush_display();
    }

    /// `send(data)`: a frame only once the socket is attached and open.
    fn send(&self, data: Vec<u8>) {
        let Some(handle) = self.socket.borrow().clone() else { return };
        if !self.attached.get() || !handle.is_open() {
            return;
        }
        let Some((cols, rows)) = self.size.get() else { return };
        handle.send_binary(persistent_pty_frame(cols as f64, rows as f64, Some(&data)));
    }

    /// `interact()`: asks for control, or reports the pane's size.
    fn interact(&self) {
        if !self.restored.get() {
            self.wants_control.set(true);
            return;
        }
        let Some((cols, rows)) = self.size.get() else { return };
        let Some(handle) = self.socket.borrow().clone() else { return };
        if self.attached.get() && handle.is_open() {
            handle.send_binary(persistent_pty_frame(cols as f64, rows as f64, None));
        }
    }

    /// `sendInput(data)`: input typed before the replay finished waits for it.
    fn send_input(&self, data: Vec<u8>) {
        if !self.restored.get() {
            self.pending_input.borrow_mut().push(data);
            return;
        }
        self.send(data);
    }

    /// The pane's own size (`size`), which frames report; the screen's size follows the server's `canonical` one.
    fn set_pane_size(&self, cols: u16, rows: u16) {
        self.size.set(Some((cols.max(1), rows.max(1))));
    }

    /// The terminal screen's size, from the server (`canonicalSize`).
    fn set_canonical(&self, cols: u16, rows: u16) {
        self.resize(cols.max(1), rows.max(1));
    }
}

/// `<TerminalPane ptyID resizing autoFocus onAutoFocus onFocusRequest onDisconnect>`.
pub fn terminal_pane(props: TerminalPaneProps) -> View {
    let t = tui();
    let theme = t.theme;
    let api = t.data.client_context.api();
    let (display, set_display) = create_signal(String::new());
    let (failure, _) = create_signal(None::<String>);
    let (title, _) = create_signal(None::<String>);
    let mut uuid = [0u8; 16];
    opencode_tui_sync::runtime::random(&mut uuid);
    let attachment_id = uuid.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let pane = Rc::new(Pane {
        screen: Rc::new(RefCell::new(Screen::new(80, 24))),
        display: set_display,
        failure,
        title,
        socket: Rc::new(RefCell::new(None)),
        pending_input: Rc::new(RefCell::new(Vec::new())),
        size: Rc::new(Cell::new(None)),
        attached: Rc::new(Cell::new(false)),
        controller: Rc::new(Cell::new(false)),
        restored: Rc::new(Cell::new(false)),
        wants_control: Rc::new(Cell::new(false)),
        disposed: Rc::new(Cell::new(false)),
        exited: Rc::new(Cell::new(false)),
    });

    let root = box_();
    root.set_flex_grow(Some(1.0));
    root.set_min_width(Some(Value::Point(0.0)));
    root.set_min_height(Some(Value::Point(0.0)));
    root.set_overflow(Overflow::Hidden);
    root.set_focusable(true);
    bind_always(move || rgba(theme.with(|th| th.background.raised.base)), move |c| root.set_background_color(Some(c)));

    let terminal = text();
    terminal.set_position_type(true);
    terminal.set_positions(Some(Some(Value::Point(1.0))), Some(Some(Value::Point(0.0))), None, None);
    terminal.set_wrap_mode(WrapMode::None);
    terminal.set_truncate(true);
    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| terminal.set_fg(Some(c)));
    bind(
        {
            let pane = pane.clone();
            move || {
                let (cols, rows) = pane.canonical();
                (cols as f32, rows as f32)
            }
        },
        move |(cols, rows)| {
            terminal.set_width(Some(Value::Point(cols)));
            terminal.set_height(Some(Value::Point(rows)));
        },
    );
    bind(move || display.get(), move |value| terminal.set_content(&value));

    create_effect({
        let pane = pane.clone();
        let resizing = props.resizing.clone();
        move || {
            if resizing() {
                pane.interact();
            }
        }
    });

    // The pane's size leaves two columns for the border: the terminal reports its own size to the shell.
    {
        let pane = pane.clone();
        root.set_on_size_change(Some(Rc::new(move || {
            let next = ((root.width().round() as u16).saturating_sub(2).max(1), (root.height().round() as u16).max(1));
            pane.set_pane_size(next.0, next.1);
            if pane.controller.get() && pane.restored.get() {
                pane.interact();
            }
        })));
    }

    root.set_on_mouse(
        Some(MouseEventType::Down),
        Some(Rc::new({
            let pane = pane.clone();
            move |_| {
                root.focus();
                pane.interact();
            }
        })),
    );
    root.set_on_paste(Some(Rc::new({
        let pane = pane.clone();
        move |event: &mut PasteEvent| {
            event.prevent_default();
            pane.send_input(event.text().into_bytes());
        }
    })));

    {
        let pane = pane.clone();
        let leader = keymap::use_leader_active();
        let off = keymap::intercept_key(100, move |event| {
            if !root.focused() || root.is_destroyed() {
                return false;
            }
            let Some(key) = keymap::parsed_key(event) else { return false };
            // The leader and its sequences stay with the keymap.
            if keymap::is_leader(&opencode_tui_core::input::KeyEvent::new(key.clone())) || leader() {
                return false;
            }
            if key.event_type == KeyEventType::Release {
                return false;
            }
            let bytes = key_bytes(key);
            if bytes.is_empty() {
                return false;
            }
            pane.send_input(bytes);
            true
        });
        on_cleanup(off);
    }

    // The terminal takes the keyboard when it opens for the user (a new terminal, or one they selected).
    {
        let auto_focus = props.auto_focus.clone();
        let on_auto = props.on_auto_focus.clone();
        create_effect(move || {
            if !auto_focus() {
                return;
            }
            root.focus();
            if let Some(on_auto) = &on_auto {
                on_auto();
            }
        });
    }

    if let Some(on_focus_request) = &props.on_focus_request {
        let interact = {
            let pane = pane.clone();
            Rc::new(move || pane.interact())
        };
        on_focus_request(Some(Rc::new(move || {
            root.focus();
            interact();
        })));
    }

    connect(&pane, root, api, props.pty_id.clone(), attachment_id, props.on_disconnect.clone());

    on_cleanup({
        let pane = pane.clone();
        let on_focus = props.on_focus_request.clone();
        move || {
            pane.disposed.set(true);
            if let Some(handle) = pane.socket.borrow_mut().take() {
                handle.close();
            }
            if let Some(on_focus) = &on_focus {
                on_focus(None);
            }
        }
    });

    let _ = title;
    let content = show(
        {
            let failure = pane.failure;
            move || failure.get().is_none()
        },
        move || View::Node(terminal),
        Some(Rc::new({
            let failure = pane.failure;
            move || {
                let label = text();
                label.set_wrap_mode(WrapMode::None);
                bind_always(move || rgba(theme.with(|th| th.text.feedback.error.base)), move |c| label.set_fg(Some(c)));
                bind(move || failure.get().unwrap_or_default(), move |value| label.set_content(&value));
                View::Node(label)
            }
        })),
    );
    insert(root, content, At::Only);
    View::Node(root)
}

impl Pane {
    fn canonical(&self) -> (u16, u16) {
        let screen = self.screen.borrow();
        (screen.cols() as u16, screen.rows() as u16)
    }
}

/// `connect()`: the snapshot, then the socket, then its messages.
fn connect(pane: &Rc<Pane>, root: Element, api: Client, pty_id: String, attachment_id: String, on_disconnect: Option<Rc<dyn Fn()>>) {
    let pane = pane.clone();
    tokio::task::spawn_local(async move {
        let snapshot = match api
            .call_named("experimental.persistentPty.snapshot", &obj! { "ptyID" => pty_id.as_str() }, RequestOptions::default())
            .await
        {
            Ok(value) => value.json(),
            Err(error) => return fail(&pane, &error),
        };
        if pane.disposed.get() {
            return;
        }
        let cols = snapshot.at("info.size.cols").and_then(number).unwrap_or(80.0) as u16;
        let rows = snapshot.at("info.size.rows").and_then(number).unwrap_or(24.0) as u16;
        pane.set_canonical(cols, rows);
        if let Some(checkpoint) = snapshot.str("checkpoint")
            && let Ok(bytes) = base64_decode(checkpoint)
        {
            pane.write(&bytes);
        }
        let cursor = snapshot.at("info.output.tail").and_then(number).unwrap_or(0.0);
        let input =
            PersistentPtyConnectInput { pty_id: pty_id.clone(), cursor, attachment_id: attachment_id.clone(), takeover: true };
        let mut next = match PersistentPtyClient::new(api.clone(), api.base_url().to_string()).connect_socket(&input).await {
            Ok(socket) => socket,
            Err(error) => return fail(&pane, &error),
        };
        if pane.disposed.get() {
            return;
        }
        pane.socket.borrow_mut().replace(next.handle());
        while let Some(message) = next.recv().await {
            if pane.disposed.get() {
                break;
            }
            match message {
                Incoming::Binary(bytes) => pane.write(&bytes),
                Incoming::Closed(_) => {
                    let focused = root.focused();
                    pane.socket.borrow_mut().take();
                    if !pane.exited.get() {
                        pane.failure.set(Some("Terminal disconnected".into()));
                    }
                    if focused && let Some(on_disconnect) = &on_disconnect {
                        on_disconnect();
                    }
                    break;
                }
                Incoming::Text(text) => {
                    let Some(message) = decode_persistent_pty_message(&text) else { continue };
                    match message {
                        PersistentPtyMessage::Attached { input_protocol, cols, rows, role } => {
                            if input_protocol != Some(1.0) {
                                pane.failure.set(Some("Persistent terminal server is out of date; restart OpenCode".into()));
                                break;
                            }
                            pane.set_canonical(cols as u16, rows as u16);
                            pane.controller.set(role.as_deref() == Some("controller"));
                            pane.attached.set(true);
                        }
                        PersistentPtyMessage::ReplayComplete => {
                            pane.restored.set(true);
                            let input = std::mem::take(&mut *pane.pending_input.borrow_mut());
                            for data in input {
                                pane.send(data);
                            }
                            if pane.size.get().is_none() {
                                pane.set_pane_size((root.width().round() as u16).saturating_sub(2).max(1), (root.height().round() as u16).max(1));
                            }
                            if pane.pending_input.borrow().is_empty() && (pane.controller.get() || pane.wants_control.get()) {
                                pane.interact();
                            }
                            pane.wants_control.set(false);
                        }
                        PersistentPtyMessage::Resized { cols, rows, checkpoint } => {
                            pane.set_canonical(cols as u16, rows as u16);
                            if let Ok(bytes) = base64_decode(&checkpoint) {
                                pane.write(b"\x1bc");
                                pane.write(&bytes);
                            }
                        }
                        PersistentPtyMessage::ControllerChanged { attachment_id: changed } => {
                            let previous = pane.controller.get();
                            pane.controller.set(changed.as_ref().and_then(|id| id.as_ref()) == Some(&attachment_id));
                            if pane.controller.get() && !previous && pane.restored.get() {
                                pane.interact();
                            }
                        }
                        PersistentPtyMessage::Exited { .. } => pane.exited.set(true),
                        PersistentPtyMessage::TitleChanged { title } => pane.title.set(Some(title)),
                        PersistentPtyMessage::ForegroundProcessChanged { .. } => {}
                    }
                }
            }
        }
    });
}

fn fail(pane: &Rc<Pane>, error: &Error) {
    if !pane.disposed.get() {
        pane.failure.set(Some(error.message()));
    }
}

fn number(value: &Json) -> Option<f64> {
    match value {
        Json::Number(value) => Some(*value),
        _ => None,
    }
}

fn base64_decode(value: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(value)
}

/// The bytes a key sends to the terminal, as a terminal's own encoder writes them (`event.sequence`); keys the
/// parser named without one fall back to their control sequence.
fn key_bytes(key: &opencode_tui_core::input::ParsedKey) -> Vec<u8> {
    if !key.sequence.is_empty() {
        return key.sequence.as_bytes().to_vec();
    }
    match key.name.as_str() {
        "return" | "enter" => b"\r".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "space" => b" ".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        name if key.ctrl && name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() => {
            vec![name.as_bytes()[0].to_ascii_lowercase() & 0x1f]
        }
        name => name.as_bytes().to_vec(),
    }
}
