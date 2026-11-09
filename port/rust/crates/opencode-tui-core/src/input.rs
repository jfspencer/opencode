//! Key and paste events (`lib/KeyHandler.ts`: KeyEvent, PasteEvent, the InternalKeyHandler's global-then-focused
//! dispatch) and a stand-in stdin parser.
//!
//! TODO(port-tui): opencode-tui-input. The parser here covers what the conformance terminal sends (UTF-8 text,
//! kitty CSI u with flags 5, legacy CSI/SS3 keys, SGR mouse, bracketed paste, focus, and the replies to the
//! startup queries); the input slice's port of `input-parser.zig`/`stdin-parser.ts`/`parse.keypress*.ts` replaces
//! it, keeping [`ParsedKey`] and [`Event`].

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

/// A raw SGR mouse report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawMouse {
    pub button: u32,
    pub x: i32,
    pub y: i32,
    pub release: bool,
}

/// What stdin carries.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Key(ParsedKey),
    Paste(Vec<u8>),
    Mouse(RawMouse),
    Focus(bool),
    /// A reply to a terminal query, as received (DA1, DECRPM, kitty `?u`, XTVERSION, OSC colors, CPR, ...).
    Response(String),
}

/// The stand-in stdin parser: feed bytes, take events. Incomplete sequences wait for more bytes; a lone ESC is
/// flushed by [`Parser::flush_timeout`].
#[derive(Default)]
pub struct Parser {
    pending: Vec<u8>,
    paste: Option<Vec<u8>>,
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            if let Some(paste) = self.paste.as_mut() {
                match find(&self.pending, PASTE_END) {
                    Some(end) => {
                        paste.extend_from_slice(&self.pending[..end]);
                        let done = self.paste.take().unwrap();
                        self.pending.drain(..end + PASTE_END.len());
                        out.push(Event::Paste(done));
                        continue;
                    }
                    None => {
                        // Keep a possible partial end marker.
                        let keep = PASTE_END.len().min(self.pending.len());
                        let cut = self.pending.len() - keep;
                        paste.extend_from_slice(&self.pending[..cut]);
                        self.pending.drain(..cut);
                        return out;
                    }
                }
            }
            if self.pending.is_empty() {
                return out;
            }
            match self.next_event() {
                Some((event, used)) => {
                    self.pending.drain(..used);
                    if let Some(event) = event {
                        out.push(event);
                    }
                }
                None => return out,
            }
        }
    }

    /// A lone ESC (or an unfinished sequence) after the timeout is the escape key.
    pub fn flush_timeout(&mut self) -> Vec<Event> {
        if self.pending == b"\x1b" {
            self.pending.clear();
            return vec![Event::Key(named("escape", "\x1b"))];
        }
        let raw = std::mem::take(&mut self.pending);
        if raw.is_empty() { Vec::new() } else { vec![Event::Key(named("", &String::from_utf8_lossy(&raw)))] }
    }

    /// One event from the front of `pending`: Some((event, bytes used)), None if incomplete.
    fn next_event(&mut self) -> Option<(Option<Event>, usize)> {
        let p = &self.pending;
        if p[0] != 0x1b {
            return Some(plain(p));
        }
        if p.len() == 1 {
            return None;
        }
        match p[1] {
            b'[' => self.csi(),
            b']' => string_terminated(p, 2).map(|end| (Some(Event::Response(text(&p[..end]))), end)),
            b'P' | b'_' => string_terminated(p, 2).map(|end| (Some(Event::Response(text(&p[..end]))), end)),
            b'O' => {
                if p.len() < 3 {
                    return None;
                }
                let name = match p[2] {
                    b'A' => "up",
                    b'B' => "down",
                    b'C' => "right",
                    b'D' => "left",
                    b'H' => "home",
                    b'F' => "end",
                    b'P' => "f1",
                    b'Q' => "f2",
                    b'R' => "f3",
                    b'S' => "f4",
                    _ => "",
                };
                Some((Some(Event::Key(named(name, &text(&p[..3])))), 3))
            }
            0x1b => Some((Some(Event::Key(named("escape", "\x1b"))), 1)),
            _ => {
                // ESC + key: meta.
                let (event, used) = plain(&p[1..]);
                let event = event.map(|e| match e {
                    Event::Key(mut key) => {
                        key.meta = true;
                        key.option = true;
                        key.sequence = format!("\x1b{}", key.sequence);
                        key.raw = key.sequence.clone();
                        Event::Key(key)
                    }
                    other => other,
                });
                Some((event, used + 1))
            }
        }
    }

    fn csi(&mut self) -> Option<(Option<Event>, usize)> {
        let p = &self.pending;
        if p.starts_with(PASTE_START) {
            self.paste = Some(Vec::new());
            return Some((None, PASTE_START.len()));
        }
        // Final byte 0x40..=0x7e after parameters.
        let mut end = 2;
        while end < p.len() && !(0x40..=0x7e).contains(&p[end]) {
            end += 1;
        }
        if end >= p.len() {
            return None;
        }
        let used = end + 1;
        let seq = text(&p[..used]);
        let params = &seq[2..seq.len() - 1];
        let fin = p[end] as char;
        // Mouse (SGR).
        if params.starts_with('<') && (fin == 'M' || fin == 'm') {
            let nums: Vec<i32> = params[1..].split(';').filter_map(|n| n.parse().ok()).collect();
            if nums.len() == 3 {
                return Some((
                    Some(Event::Mouse(RawMouse { button: nums[0] as u32, x: nums[1] - 1, y: nums[2] - 1, release: fin == 'm' })),
                    used,
                ));
            }
        }
        if params.is_empty() && fin == 'I' {
            return Some((Some(Event::Focus(true)), used));
        }
        if params.is_empty() && fin == 'O' {
            return Some((Some(Event::Focus(false)), used));
        }
        // Replies.
        let reply = (params.starts_with('?') && (fin == 'c' || fin == 'u' || fin == 'y' || fin == 'n'))
            || (fin == 'y' && params.contains('$'))
            || fin == 'R'
            || (fin == 't' && !params.is_empty());
        if reply {
            return Some((Some(Event::Response(seq)), used));
        }
        Some((Some(Event::Key(csi_key(&seq, params, fin))), used))
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// End (exclusive) of an OSC/DCS/APC string terminated by BEL or ST.
fn string_terminated(p: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i < p.len() {
        if p[i] == 0x07 {
            return Some(i + 1);
        }
        if p[i] == 0x1b && i + 1 < p.len() && p[i + 1] == b'\\' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

fn named(name: &str, sequence: &str) -> ParsedKey {
    ParsedKey {
        name: name.to_string(),
        sequence: sequence.to_string(),
        raw: sequence.to_string(),
        source: "raw",
        ..ParsedKey::default()
    }
}

/// One character of plain input.
fn plain(p: &[u8]) -> (Option<Event>, usize) {
    let len = match p[0] {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
    .min(p.len());
    let s = text(&p[..len]);
    let c = s.chars().next().unwrap_or('\u{fffd}');
    let mut key = named("", &s);
    match c {
        '\r' => key.name = "return".into(),
        '\n' => key.name = "linefeed".into(),
        '\t' => key.name = "tab".into(),
        '\x7f' | '\x08' => key.name = "backspace".into(),
        ' ' => key.name = "space".into(),
        '\x00' => {
            key.name = "space".into();
            key.ctrl = true;
        }
        '\x01'..='\x1a' => {
            key.name = ((c as u8 - 1 + b'a') as char).to_string();
            key.ctrl = true;
        }
        '0'..='9' => {
            key.name = c.to_string();
            key.number = true;
        }
        'A'..='Z' => {
            key.name = c.to_ascii_lowercase().to_string();
            key.shift = true;
        }
        _ => key.name = c.to_string(),
    }
    (Some(Event::Key(key)), len)
}

fn kitty_name(code: u32) -> Option<&'static str> {
    Some(match code {
        27 | 57344 => "escape",
        9 | 57346 => "tab",
        13 | 57345 => "return",
        127 | 57347 => "backspace",
        57348 => "insert",
        57349 => "delete",
        57350 => "left",
        57351 => "right",
        57352 => "up",
        57353 => "down",
        57354 => "pageup",
        57355 => "pagedown",
        57356 => "home",
        57357 => "end",
        57364 => "f1",
        57365 => "f2",
        57366 => "f3",
        57367 => "f4",
        57368 => "f5",
        57369 => "f6",
        57370 => "f7",
        57371 => "f8",
        57372 => "f9",
        57373 => "f10",
        57374 => "f11",
        57375 => "f12",
        32 => "space",
        _ => return None,
    })
}

fn apply_mods(key: &mut ParsedKey, mods: u32) {
    let m = mods.saturating_sub(1);
    key.shift = m & 1 != 0;
    let alt = m & 2 != 0;
    key.ctrl = m & 4 != 0;
    key.super_ = m & 8 != 0;
    key.hyper = m & 16 != 0;
    let meta = m & 32 != 0;
    key.meta = alt || meta;
    key.option = alt;
    key.caps_lock = m & 64 != 0;
    key.num_lock = m & 128 != 0;
}

fn csi_key(seq: &str, params: &str, fin: char) -> ParsedKey {
    let mut key = named("", seq);
    let fields: Vec<&str> = params.split(';').collect();
    let mods = fields.get(1).and_then(|f| f.split(':').next()).and_then(|m| m.parse::<u32>().ok()).unwrap_or(1);
    let event = fields.get(1).and_then(|f| f.split(':').nth(1)).and_then(|e| e.parse::<u32>().ok()).unwrap_or(1);
    key.event_type = match event {
        2 => KeyEventType::Repeat,
        3 => KeyEventType::Release,
        _ => KeyEventType::Press,
    };
    match fin {
        'u' => {
            key.source = "kitty";
            let codes: Vec<u32> = fields[0].split(':').map(|c| c.parse().unwrap_or(0)).collect();
            let code = codes.first().copied().unwrap_or(0);
            apply_mods(&mut key, mods);
            key.base_code = codes.get(2).copied().filter(|c| *c != 0);
            key.name = match kitty_name(code) {
                Some(name) => name.to_string(),
                None => char::from_u32(code).map(|c| c.to_lowercase().to_string()).unwrap_or_default(),
            };
            if let Some(c) = char::from_u32(code) {
                if c.is_ascii_digit() {
                    key.number = true;
                }
            }
        }
        'A' | 'B' | 'C' | 'D' | 'H' | 'F' | 'P' | 'Q' | 'R' | 'S' | 'Z' => {
            apply_mods(&mut key, mods);
            key.name = match fin {
                'A' => "up",
                'B' => "down",
                'C' => "right",
                'D' => "left",
                'H' => "home",
                'F' => "end",
                'P' => "f1",
                'Q' => "f2",
                'R' => "f3",
                'S' => "f4",
                _ => {
                    key.shift = true;
                    "tab"
                }
            }
            .to_string();
        }
        '~' => {
            apply_mods(&mut key, mods);
            key.name = match fields[0] {
                "1" | "7" => "home",
                "2" => "insert",
                "3" => "delete",
                "4" | "8" => "end",
                "5" => "pageup",
                "6" => "pagedown",
                "11" => "f1",
                "12" => "f2",
                "13" => "f3",
                "14" => "f4",
                "15" => "f5",
                "17" => "f6",
                "18" => "f7",
                "19" => "f8",
                "20" => "f9",
                "21" => "f10",
                "23" => "f11",
                "24" => "f12",
                _ => "",
            }
            .to_string();
        }
        _ => {}
    }
    key
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
                Event::Mouse(m) => format!("mouse {} {} {}", m.button, m.x, m.y),
                Event::Focus(f) => format!("focus {f}"),
                Event::Response(r) => format!("reply {}", r.escape_debug()),
            })
            .collect();
        assert_eq!(
            names,
            ["key a", "key super+a", "paste hi", "mouse 0 2 3", "reply \\u{1b}[?1;2c", "key return", "key escape"]
        );
    }
}
