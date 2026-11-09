//! Key and paste events (`lib/KeyHandler.ts`: KeyEvent, PasteEvent, the InternalKeyHandler's global-then-focused
//! dispatch) and the renderer's stdin parser, `opencode-tui-input`'s port of `stdin-parser.ts`, whose events are
//! mapped onto this crate's [`ParsedKey`] and [`Event`].

/// `ParsedKey` (parse.keypress.ts).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedKey {
    pub name: String,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
    pub option: bool,
    pub sequence: String,
    pub number: bool,
    pub raw: String,
    pub event_type: KeyEventType,
    /// "raw" or "kitty".
    pub source: &'static str,
    pub code: Option<String>,
    pub super_: bool,
    pub hyper: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
    pub base_code: Option<u32>,
    pub repeated: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyEventType {
    #[default]
    Press,
    Repeat,
    Release,
}

/// `KeyEvent`: a parsed key with preventDefault/stopPropagation.
#[derive(Clone, Debug, Default)]
pub struct KeyEvent {
    pub key: ParsedKey,
    pub default_prevented: bool,
    pub propagation_stopped: bool,
}

impl KeyEvent {
    pub fn new(key: ParsedKey) -> KeyEvent {
        KeyEvent { key, default_prevented: false, propagation_stopped: false }
    }
    pub fn prevent_default(&mut self) {
        self.default_prevented = true;
    }
    pub fn stop_propagation(&mut self) {
        self.propagation_stopped = true;
    }
    pub fn name(&self) -> &str {
        &self.key.name
    }
}

impl std::ops::Deref for KeyEvent {
    type Target = ParsedKey;
    fn deref(&self) -> &ParsedKey {
        &self.key
    }
}

/// `PasteEvent`.
#[derive(Clone, Debug, Default)]
pub struct PasteEvent {
    pub bytes: Vec<u8>,
    pub default_prevented: bool,
    pub propagation_stopped: bool,
}

impl PasteEvent {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
    pub fn prevent_default(&mut self) {
        self.default_prevented = true;
    }
    pub fn stop_propagation(&mut self) {
        self.propagation_stopped = true;
    }
}

/// A mouse report (`RawMouseEvent`): 0-based cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawMouse {
    pub kind: crate::tree::MouseEventType,
    pub button: u32,
    pub x: i32,
    pub y: i32,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub scroll: Option<crate::tree::ScrollInfo>,
}

/// What stdin carries.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Key(ParsedKey),
    Paste(Vec<u8>),
    Mouse(RawMouse),
    /// A reply to a terminal query, as received (DA1, DECRPM, kitty `?u`, XTVERSION, OSC colors, CPR, focus, ...).
    Response(String),
}

/// The renderer's stdin parser: `opencode_tui_input::StdinParser` (`stdin-parser.ts`) with the options the
/// renderer gives it, its events in this crate's types.
pub struct Parser {
    inner: opencode_tui_input::StdinParser,
}

fn key_of(key: opencode_tui_input::ParsedKey) -> ParsedKey {
    ParsedKey {
        name: key.name,
        ctrl: key.ctrl,
        meta: key.meta,
        shift: key.shift,
        option: key.option,
        sequence: key.sequence,
        number: key.number,
        raw: key.raw,
        event_type: match key.event_type {
            opencode_tui_input::KeyEventType::Press => KeyEventType::Press,
            opencode_tui_input::KeyEventType::Repeat => KeyEventType::Repeat,
            opencode_tui_input::KeyEventType::Release => KeyEventType::Release,
        },
        source: match key.source {
            opencode_tui_input::KeySource::Raw => "raw",
            opencode_tui_input::KeySource::Kitty => "kitty",
        },
        code: key.code,
        super_: key.super_.unwrap_or(false),
        hyper: key.hyper.unwrap_or(false),
        caps_lock: key.caps_lock.unwrap_or(false),
        num_lock: key.num_lock.unwrap_or(false),
        base_code: key.base_code,
        repeated: key.repeated.unwrap_or(false),
    }
}

fn mouse_of(event: opencode_tui_input::RawMouseEvent) -> RawMouse {
    use crate::tree::{MouseEventType as T, ScrollDirection as D, ScrollInfo};
    use opencode_tui_input::{MouseEventType as M, ScrollDirection as S};
    RawMouse {
        kind: match event.kind {
            M::Down => T::Down,
            M::Up => T::Up,
            M::Move => T::Move,
            M::Drag => T::Drag,
            M::DragEnd => T::DragEnd,
            M::Drop => T::Drop,
            M::Over => T::Over,
            M::Out => T::Out,
            M::Scroll => T::Scroll,
        },
        button: event.button.max(0) as u32,
        x: event.x as i32,
        y: event.y as i32,
        shift: event.modifiers.shift,
        alt: event.modifiers.alt,
        ctrl: event.modifiers.ctrl,
        scroll: event.scroll.map(|s| ScrollInfo {
            direction: match s.direction {
                S::Up => D::Up,
                S::Down => D::Down,
                S::Left => D::Left,
                S::Right => D::Right,
            },
            delta: s.delta,
        }),
    }
}

impl Parser {
    /// The renderer's parser: Kitty keyboard on (the TUI asks for flags 5), the default ESC timeout.
    pub fn new() -> Parser {
        let options = opencode_tui_input::StdinParserOptions {
            use_kitty_keyboard: Some(true),
            protocol_context: opencode_tui_input::ProtocolContextPatch {
                kitty_keyboard_enabled: Some(true),
                ..Default::default()
            },
            ..Default::default()
        };
        Parser { inner: opencode_tui_input::StdinParser::new(options) }
    }

    /// `updateStdinParserProtocolContext`: the replies the terminal side waits for.
    pub fn set_protocol_context(&mut self, context: crate::terminal::ProtocolContext) {
        let _ = self.inner.update_protocol_context(opencode_tui_input::ProtocolContextPatch {
            kitty_keyboard_enabled: None,
            private_capability_replies_active: Some(context.private_capability_replies_active),
            pixel_resolution_query_active: Some(context.pixel_resolution_query_active),
            explicit_width_cpr_active: Some(context.explicit_width_cpr_active),
            startup_cursor_cpr_active: Some(context.startup_cursor_cpr_active),
            reply_sentinel_active: Some(context.reply_sentinel_active),
        });
    }

    fn take(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        let _ = self.inner.drain(|event| {
            out.push(match event {
                opencode_tui_input::StdinEvent::Key { key, .. } => Event::Key(key_of(key)),
                opencode_tui_input::StdinEvent::Mouse { event, .. } => Event::Mouse(mouse_of(event)),
                opencode_tui_input::StdinEvent::Paste { bytes, .. } => Event::Paste(bytes),
                opencode_tui_input::StdinEvent::Response { sequence, .. } => Event::Response(sequence),
            })
        });
        out
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let _ = self.inner.push(bytes);
        self.take()
    }

    /// When the pending bytes time out (ms on the parser's clock), if any.
    pub fn timeout_deadline(&self) -> Option<f64> {
        self.inner.timeout_deadline()
    }

    pub fn has_pending(&self) -> bool {
        self.inner.timeout_deadline().is_some()
    }

    /// The ESC timeout fired.
    pub fn flush_timeout(&mut self) -> Vec<Event> {
        let _ = self.inner.flush_timeout(None);
        self.take()
    }

    /// After a resize (`resetMouseState`).
    pub fn reset_mouse_state(&mut self) {
        let _ = self.inner.reset_mouse_state();
    }
}

impl Default for Parser {
    fn default() -> Self {
        Parser::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_paste_mouse_and_replies() {
        let mut parser = Parser::new();
        let events = parser.feed(b"a\x1b[97;9u\x1b[200~hi\x1b[201~\x1b[<0;3;4M\x1b[?1;2c\r\x1b[27u");
        let names: Vec<String> = events
            .iter()
            .map(|e| match e {
                Event::Key(k) => format!("key {}{}", if k.super_ { "super+" } else { "" }, k.name),
                Event::Paste(p) => format!("paste {}", String::from_utf8_lossy(p)),
                Event::Mouse(m) => format!("mouse {:?} {} {}", m.kind, m.x, m.y),
                Event::Response(r) => format!("reply {}", r.escape_debug()),
            })
            .collect();
        assert_eq!(
            names,
            ["key a", "key super+a", "paste hi", "mouse Down 2 3", "reply \\u{1b}[?1;2c", "key return", "key escape"]
        );
    }
}
