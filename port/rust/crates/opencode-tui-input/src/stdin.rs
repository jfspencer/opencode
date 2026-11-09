//! `packages/core/src/lib/stdin-parser.ts`: the byte-level stdin parser OpenTUI's renderer uses (the legacy one;
//! the native `input-parser.zig` is opt-in through `OTUI_NATIVE_INPUT_PARSER`). Raw terminal input becomes key,
//! mouse, paste and response events, whatever the chunk boundaries; an incomplete unit (a lone ESC, a split UTF-8
//! code point, a reply still arriving) waits for more bytes or for the timeout.
//!
//! Focus reports (`CSI I`, `CSI O`), device attributes, DECRQM mode reports, the Kitty flags reply, color-scheme
//! reports, cursor position reports, window and pixel size reports, XTVERSION, OSC colors and Kitty graphics replies
//! all arrive as [`StdinEvent::Response`], with the protocol the framing found, as in TS; the renderer interprets
//! them.
//!
//! # Timers
//!
//! TS arms a `clock.setTimeout` whose callback runs `tryForceFlush()` then `onTimeoutFlush()` (the renderer drains).
//! Here the parser keeps the deadline: after any call, [`StdinParser::timeout_deadline`] is when that timer would
//! fire (on the parser's [`Clock`]); when it passes, the host calls [`StdinParser::on_timeout`] and then drains, as
//! `onTimeoutFlush` does. Each push re-arms the timer from the clock's current time, as TS does.
//!
//! # Divergences
//!
//! A call on a destroyed parser returns [`Destroyed`] (TS throws the same message). `drain` takes a callback that
//! cannot reach the parser; a host whose handlers can destroy it loops on [`StdinParser::read`] and
//! [`StdinParser::is_destroyed`].
use crate::js::{decode_latin1, decode_utf8};
use crate::keypress::{ParsedKey, parse_keypress, parse_keypress_high_byte};
use crate::mouse::{MouseParser, RawMouseEvent};
use crate::paste::PasteMetadata;
use std::collections::VecDeque;
use std::fmt;
use std::rc::Rc;

/// `StdinResponseProtocol`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResponseProtocol {
    Csi,
    Cpr,
    Osc,
    Dcs,
    Apc,
    Unknown,
}

impl ResponseProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            ResponseProtocol::Csi => "csi",
            ResponseProtocol::Cpr => "cpr",
            ResponseProtocol::Osc => "osc",
            ResponseProtocol::Dcs => "dcs",
            ResponseProtocol::Apc => "apc",
            ResponseProtocol::Unknown => "unknown",
        }
    }
}

/// A mouse report's encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseEncoding {
    Sgr,
    X10,
}

impl MouseEncoding {
    pub fn as_str(self) -> &'static str {
        match self {
            MouseEncoding::Sgr => "sgr",
            MouseEncoding::X10 => "x10",
        }
    }
}

/// `StdinEvent`.
#[derive(Clone, Debug, PartialEq)]
pub enum StdinEvent {
    Key {
        raw: String,
        key: ParsedKey,
    },
    /// `raw` is the report's bytes read as latin1.
    Mouse {
        raw: String,
        encoding: MouseEncoding,
        event: RawMouseEvent,
    },
    Paste {
        bytes: Vec<u8>,
        metadata: Option<PasteMetadata>,
    },
    /// `sequence` is the bytes read as latin1, or as UTF-8 when the unit was offered to `parseKeypress` first.
    Response {
        protocol: ResponseProtocol,
        sequence: String,
    },
}

/// `StdinParserProtocolContext`: which replies the renderer is waiting for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ProtocolContext {
    pub kitty_keyboard_enabled: bool,
    pub private_capability_replies_active: bool,
    pub pixel_resolution_query_active: bool,
    pub explicit_width_cpr_active: bool,
    pub startup_cursor_cpr_active: bool,
    /// Native parser only; the legacy parser ignores it.
    pub reply_sentinel_active: bool,
}

/// `Partial<StdinParserProtocolContext>`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ProtocolContextPatch {
    pub kitty_keyboard_enabled: Option<bool>,
    pub private_capability_replies_active: Option<bool>,
    pub pixel_resolution_query_active: Option<bool>,
    pub explicit_width_cpr_active: Option<bool>,
    pub startup_cursor_cpr_active: Option<bool>,
    pub reply_sentinel_active: Option<bool>,
}

impl ProtocolContext {
    fn apply(&mut self, patch: &ProtocolContextPatch) {
        let set = |field: &mut bool, value: Option<bool>| {
            if let Some(value) = value {
                *field = value;
            }
        };
        set(&mut self.kitty_keyboard_enabled, patch.kitty_keyboard_enabled);
        set(&mut self.private_capability_replies_active, patch.private_capability_replies_active);
        set(&mut self.pixel_resolution_query_active, patch.pixel_resolution_query_active);
        set(&mut self.explicit_width_cpr_active, patch.explicit_width_cpr_active);
        set(&mut self.startup_cursor_cpr_active, patch.startup_cursor_cpr_active);
        set(&mut self.reply_sentinel_active, patch.reply_sentinel_active);
    }
}

/// `Clock.now()`: milliseconds on any monotonic origin.
pub trait Clock {
    fn now(&self) -> f64;
}

/// `SystemClock`: `performance.now()`, milliseconds since the clock was made.
pub struct SystemClock(std::time::Instant);

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock(std::time::Instant::now())
    }
}

impl Clock for SystemClock {
    fn now(&self) -> f64 {
        self.0.elapsed().as_secs_f64() * 1000.0
    }
}

/// `StdinParserOptions`. Numbers are TS numbers: one that is not finite and above 0 means the default, others
/// are floored.
#[derive(Clone, Default)]
pub struct StdinParserOptions {
    pub timeout_ms: Option<f64>,
    pub max_pending_bytes: Option<f64>,
    pub arm_timeouts: Option<bool>,
    pub use_kitty_keyboard: Option<bool>,
    pub protocol_context: ProtocolContextPatch,
    pub clock: Option<Rc<dyn Clock>>,
}

/// What a call on a destroyed parser fails with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Destroyed;

impl fmt::Display for Destroyed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StdinParser has been destroyed")
    }
}

impl std::error::Error for Destroyed {}

const DEFAULT_TIMEOUT_MS: f64 = 20.0;
const DEFAULT_MAX_PENDING_BYTES: f64 = 64.0 * 1024.0;
const INITIAL_PENDING_CAPACITY: usize = 256;
const ESC: i32 = 0x1b;
const BEL: i32 = 0x07;
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

/// `ByteQueue`: pending input with a consumed prefix, compacted once it passes half the buffer.
struct ByteQueue {
    buf: Vec<u8>,
    start: usize,
    end: usize,
}

impl ByteQueue {
    fn new(capacity: usize) -> ByteQueue {
        ByteQueue { buf: vec![0; capacity], start: 0, end: 0 }
    }

    fn len(&self) -> usize {
        self.end - self.start
    }

    fn view(&self) -> &[u8] {
        &self.buf[self.start..self.end]
    }

    fn take(&mut self) -> Vec<u8> {
        let chunk = self.view().to_vec();
        self.start = 0;
        self.end = 0;
        chunk
    }

    fn append(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.ensure_capacity(self.len() + chunk.len());
        self.buf[self.end..self.end + chunk.len()].copy_from_slice(chunk);
        self.end += chunk.len();
    }

    fn consume(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        if count >= self.len() {
            self.start = 0;
            self.end = 0;
            return;
        }
        self.start += count;
        if self.start * 2 >= self.buf.len() {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
    }

    fn clear(&mut self) {
        self.start = 0;
        self.end = 0;
    }

    fn reset(&mut self, capacity: usize) {
        self.buf = vec![0; capacity];
        self.start = 0;
        self.end = 0;
    }

    fn ensure_capacity(&mut self, required: usize) {
        let current = self.len();
        if required <= self.buf.len() {
            if self.buf.len() - self.end >= required - current {
                return;
            }
            self.buf.copy_within(self.start..self.end, 0);
            self.end = current;
            self.start = 0;
            if required <= self.buf.len() {
                return;
            }
        }
        let mut capacity = self.buf.len();
        while capacity < required {
            capacity *= 2;
        }
        let mut next = vec![0; capacity];
        next[..current].copy_from_slice(self.view());
        self.buf = next;
        self.start = 0;
        self.end = current;
    }
}

fn normalize_positive_option(value: Option<f64>, fallback: f64) -> f64 {
    match value {
        Some(value) if value.is_finite() && value > 0.0 => value.floor(),
        _ => fallback,
    }
}

/// The byte count of a UTF-8 sequence from its lead byte; 0 for a legacy high byte.
fn utf8_sequence_length(first: i32) -> u8 {
    match first {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 0,
    }
}

fn is_digit(byte: i32) -> bool {
    (0x30..=0x39).contains(&byte)
}

/// `isMouseSgrSequence`: `ESC [ < digits ; digits ; digits M/m`.
fn is_mouse_sgr_sequence(sequence: &[u8]) -> bool {
    if sequence.len() < 7 || !sequence.starts_with(b"\x1b[<") {
        return false;
    }
    let last = sequence[sequence.len() - 1];
    if last != b'M' && last != b'm' {
        return false;
    }
    let mut part = 0;
    let mut has_digit = false;
    for &byte in &sequence[3..sequence.len() - 1] {
        if byte.is_ascii_digit() {
            has_digit = true;
            continue;
        }
        if byte == b';' && has_digit && part < 2 {
            part += 1;
            has_digit = false;
            continue;
        }
        return false;
    }
    part == 2 && has_digit
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Parametric {
    semicolons: u32,
    segments: u32,
    has_digit: bool,
    first_param_value: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PrivateReply {
    semicolons: u32,
    has_digit: bool,
    saw_dollar: bool,
}

/// `ParserState`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Ground,
    Utf8 { expected: u8, seen: u8 },
    Esc,
    Ss3,
    Csi,
    CsiSgrMouse { part: u32, has_digit: bool },
    CsiSgrMouseDeferred { part: u32, has_digit: bool },
    CsiParametric(Parametric),
    CsiParametricDeferred(Parametric),
    CsiParametricIgnored(Parametric),
    CsiPrivateReply(PrivateReply),
    CsiPrivateReplyDeferred(PrivateReply),
    Osc { saw_esc: bool },
    Dcs { saw_esc: bool },
    Apc { saw_esc: bool },
    EscRecovery,
    EscLessMouse,
    EscLessX10Mouse,
}

/// `parsePositiveDecimalPrefix`: the digits of `[start, end)` as a number, `None` if any is not one or none.
fn parse_positive_decimal_prefix(sequence: &[u8], start: usize, end: usize) -> Option<f64> {
    if start >= end {
        return None;
    }
    let mut value = 0.0;
    for &byte in &sequence[start..end] {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10.0 + (byte - b'0') as f64;
    }
    Some(value)
}

/// `parseKittyFirstFieldCodepoint`: the code point before the first `:` of a Kitty field 1 (`97:65`).
fn parse_kitty_first_field_codepoint(sequence: &[u8], start: usize, end: usize) -> Option<f64> {
    if start >= end {
        return None;
    }
    let colon = start + sequence[start..end].iter().position(|&byte| byte == b':')?;
    let code = parse_positive_decimal_prefix(sequence, start, colon)?;
    if sequence[colon + 1..end].iter().any(|&byte| byte != b':' && !byte.is_ascii_digit()) {
        return None;
    }
    Some(code)
}

fn can_still_be_explicit_width_cpr(state: &Parametric) -> bool {
    state.first_param_value == Some(1.0) && state.semicolons == 1
}

fn can_still_be_startup_cursor_cpr_prefix(state: &Parametric) -> bool {
    state.segments == 1 && state.semicolons <= 1
}

/// `canStillBePixelResolutionPrefix`: a prefix of `ESC [ 4 ; height ; width t`.
fn can_still_be_pixel_resolution_prefix(bytes: &[u8]) -> bool {
    let fixed = b"\x1b[4;";
    let length = bytes.len().min(fixed.len());
    if bytes[..length] != fixed[..length] {
        return false;
    }
    if bytes.len() <= fixed.len() {
        return !bytes.is_empty();
    }
    let mut index = fixed.len();
    let height_start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == bytes.len() {
        return index > height_start;
    }
    if index == height_start || bytes[index] != b';' {
        return false;
    }
    index += 1;
    let width_start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == bytes.len() {
        return true;
    }
    index > width_start && bytes[index] == b't'
}

fn can_defer_parametric_csi(state: &Parametric, context: &ProtocolContext) -> bool {
    (context.kitty_keyboard_enabled && (state.semicolons >= 1 || (state.semicolons == 1 && state.segments > 1)))
        || (context.explicit_width_cpr_active && can_still_be_explicit_width_cpr(state))
        || (context.startup_cursor_cpr_active && state.semicolons == 1)
        || (context.pixel_resolution_query_active && state.first_param_value == Some(4.0) && state.semicolons == 2)
}

fn can_complete_deferred_parametric_csi(state: &Parametric, byte: i32, context: &ProtocolContext) -> bool {
    if context.kitty_keyboard_enabled {
        if state.has_digit && byte == 0x75 {
            return true;
        }
        if state.has_digit
            && state.semicolons == 1
            && state.segments > 1
            && (byte == 0x7e || (0x41..=0x5a).contains(&byte))
        {
            return true;
        }
    }
    if context.explicit_width_cpr_active
        && state.has_digit
        && state.first_param_value == Some(1.0)
        && state.semicolons == 1
        && byte == 0x52
    {
        return true;
    }
    if context.startup_cursor_cpr_active && state.has_digit && state.semicolons == 1 && byte == 0x52 {
        return true;
    }
    context.pixel_resolution_query_active
        && state.has_digit
        && state.first_param_value == Some(4.0)
        && state.semicolons == 2
        && byte == 0x74
}

fn classify_parametric_csi_protocol(state: &Parametric, final_byte: i32) -> ResponseProtocol {
    if final_byte == 0x52 && state.semicolons == 1 && state.segments == 1 && state.has_digit {
        ResponseProtocol::Cpr
    } else {
        ResponseProtocol::Csi
    }
}

fn can_complete_deferred_private_reply_csi(state: &PrivateReply, byte: i32, context: &ProtocolContext) -> bool {
    if !context.private_capability_replies_active {
        return false;
    }
    if state.saw_dollar {
        return state.has_digit && byte == 0x79;
    }
    if byte == 0x63 {
        return state.has_digit || state.semicolons > 0;
    }
    if byte == 0x6e {
        return state.has_digit;
    }
    state.has_digit && byte == 0x75
}

fn index_of_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// `PasteCollector`: the paste body so far and a tail that may hold a split end marker.
#[derive(Default)]
struct PasteCollector {
    tail: Vec<u8>,
    body: Vec<u8>,
}

/// `StdinParser`.
pub struct StdinParser {
    pending: ByteQueue,
    events: VecDeque<StdinEvent>,
    timeout_ms: f64,
    max_pending_bytes: f64,
    arm_timeouts: bool,
    use_kitty_keyboard: bool,
    mouse_parser: MouseParser,
    clock: Rc<dyn Clock>,
    protocol_context: ProtocolContext,
    /// The armed timer's deadline (`timeoutId`).
    timeout_at: Option<f64>,
    destroyed: bool,
    pending_since_ms: Option<f64>,
    pending_timeout_paused: bool,
    suspended_pixel_resolution_prefix_length: usize,
    force_flush: bool,
    just_flushed_esc: bool,
    state: State,
    cursor: usize,
    unit_start: usize,
    paste: Option<PasteCollector>,
}

impl StdinParser {
    pub fn new(options: StdinParserOptions) -> StdinParser {
        let mut protocol_context = ProtocolContext::default();
        let patch = options.protocol_context;
        protocol_context.apply(&ProtocolContextPatch { reply_sentinel_active: None, ..patch });
        StdinParser {
            pending: ByteQueue::new(INITIAL_PENDING_CAPACITY),
            events: VecDeque::new(),
            timeout_ms: normalize_positive_option(options.timeout_ms, DEFAULT_TIMEOUT_MS),
            max_pending_bytes: normalize_positive_option(options.max_pending_bytes, DEFAULT_MAX_PENDING_BYTES),
            arm_timeouts: options.arm_timeouts.unwrap_or(true),
            use_kitty_keyboard: options.use_kitty_keyboard.unwrap_or(true),
            mouse_parser: MouseParser::new(),
            clock: options.clock.unwrap_or_else(|| Rc::new(SystemClock::default())),
            protocol_context,
            timeout_at: None,
            destroyed: false,
            pending_since_ms: None,
            pending_timeout_paused: false,
            suspended_pixel_resolution_prefix_length: 0,
            force_flush: false,
            just_flushed_esc: false,
            state: State::Ground,
            cursor: 0,
            unit_start: 0,
            paste: None,
        }
    }

    /// `bufferCapacity`.
    pub fn buffer_capacity(&self) -> usize {
        self.pending.buf.len()
    }

    pub fn protocol_context(&self) -> ProtocolContext {
        self.protocol_context
    }

    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    /// When the armed timeout fires on the parser's clock, if one is armed.
    pub fn timeout_deadline(&self) -> Option<f64> {
        self.timeout_at
    }

    fn ensure_alive(&self) -> Result<(), Destroyed> {
        if self.destroyed { Err(Destroyed) } else { Ok(()) }
    }

    fn byte(&self, index: usize) -> i32 {
        self.pending.view().get(index).map_or(-1, |&byte| byte as i32)
    }

    fn slice(&self, start: usize, end: usize) -> Vec<u8> {
        self.pending.view()[start..end].to_vec()
    }

    fn utf8(&self, start: usize, end: usize) -> String {
        decode_utf8(&self.pending.view()[start..end])
    }

    /// `updateProtocolContext(patch)`.
    pub fn update_protocol_context(&mut self, patch: ProtocolContextPatch) -> Result<(), Destroyed> {
        self.ensure_alive()?;
        self.protocol_context.apply(&patch);
        if !self.protocol_context.pixel_resolution_query_active && self.suspended_pixel_resolution_prefix_length > 0 {
            let prefix = self.suspended_pixel_resolution_prefix_length;
            self.state = State::Ground;
            self.consume_prefix(prefix);
            self.scan_pending();
        }
        self.reconcile_deferred_state_with_protocol_context();
        self.reconcile_timeout_state();
        Ok(())
    }

    /// `getAbortableStartupCursorCprState`.
    fn abortable_startup_cursor_cpr_state(&self) -> Option<Parametric> {
        if self.pending.len() == 0 {
            return None;
        }
        match self.state {
            State::Csi => {
                let first = self.unit_start + 2;
                if self.cursor < first {
                    return None;
                }
                let mut value: Option<f64> = None;
                for index in first..self.cursor {
                    let byte = self.byte(index);
                    if !is_digit(byte) {
                        return None;
                    }
                    value = Some(value.unwrap_or(0.0) * 10.0 + (byte - 0x30) as f64);
                }
                Some(Parametric {
                    semicolons: 0,
                    segments: 1,
                    has_digit: self.cursor > first,
                    first_param_value: value,
                })
            }
            State::CsiParametric(state) | State::CsiParametricDeferred(state) => {
                if !can_still_be_startup_cursor_cpr_prefix(&state)
                    || (self.protocol_context.explicit_width_cpr_active && can_still_be_explicit_width_cpr(&state))
                {
                    return None;
                }
                Some(state)
            }
            _ => None,
        }
    }

    /// `abortPendingStartupCursorCpr()`.
    pub fn abort_pending_startup_cursor_cpr(&mut self) -> Result<(), Destroyed> {
        self.ensure_alive()?;
        let Some(next) = self.abortable_startup_cursor_cpr_state() else { return Ok(()) };
        self.state = State::CsiParametricIgnored(next);
        if self.pending_since_ms.is_none() {
            self.mark_pending();
        }
        self.force_flush = false;
        self.reconcile_timeout_state();
        Ok(())
    }

    /// `push(data)`.
    pub fn push(&mut self, data: &[u8]) -> Result<(), Destroyed> {
        self.ensure_alive()?;
        if data.is_empty() {
            self.emit_key_or_response(ResponseProtocol::Unknown, String::new());
            return Ok(());
        }
        let mut remainder: Vec<u8> = data.to_vec();
        while !remainder.is_empty() {
            if self.paste.is_some() {
                remainder = self.consume_paste_bytes(&remainder);
                continue;
            }
            let immediate = if self.state == State::Ground && self.pending.len() == 0 {
                index_of_bytes(&remainder, BRACKETED_PASTE_START)
            } else {
                None
            };
            let append_end = immediate.map_or(remainder.len(), |index| index + BRACKETED_PASTE_START.len());
            self.pending.append(&remainder[..append_end]);
            remainder.drain(..append_end);
            if self.suspended_pixel_resolution_prefix_length > 0
                && self.protocol_context.pixel_resolution_query_active
                && !can_still_be_pixel_resolution_prefix(self.pending.view())
            {
                let prefix = self.suspended_pixel_resolution_prefix_length;
                self.state = State::Ground;
                self.consume_prefix(prefix);
            }
            self.scan_pending();

            if self.paste.is_some() && self.pending.len() > 0 {
                let mut buffered = self.take_pending_bytes();
                buffered.extend_from_slice(&remainder);
                remainder = self.consume_paste_bytes(&buffered);
                continue;
            }

            if self.paste.is_none() && self.pending.len() as f64 > self.max_pending_bytes {
                self.flush_pending_overflow();
                self.scan_pending();
                if self.paste.is_some() && self.pending.len() > 0 {
                    let mut buffered = self.take_pending_bytes();
                    buffered.extend_from_slice(&remainder);
                    remainder = self.consume_paste_bytes(&buffered);
                }
            }
        }
        self.reconcile_timeout_state();
        Ok(())
    }

    /// `read()`: the next event; after a timeout, the incomplete unit flushed first.
    pub fn read(&mut self) -> Result<Option<StdinEvent>, Destroyed> {
        self.ensure_alive()?;
        if self.events.is_empty() && self.force_flush {
            self.scan_pending();
            self.reconcile_timeout_state();
        }
        Ok(self.events.pop_front())
    }

    /// `drain(onEvent)`.
    pub fn drain(&mut self, mut on_event: impl FnMut(StdinEvent)) -> Result<(), Destroyed> {
        self.ensure_alive()?;
        loop {
            if self.destroyed {
                return Ok(());
            }
            let Some(event) = self.read()? else { return Ok(()) };
            on_event(event);
        }
    }

    /// `flushTimeout(now)`: marks the incomplete unit for flushing once the timeout has passed since it began.
    pub fn flush_timeout(&mut self, now: Option<f64>) -> Result<(), Destroyed> {
        self.ensure_alive()?;
        let now = now.unwrap_or_else(|| self.clock.now());
        if let Some(since) = self.pending_since_ms
            && (now < since || now - since < self.timeout_ms)
        {
            return Ok(());
        }
        self.try_force_flush();
        Ok(())
    }

    /// The armed timer's callback (`tryForceFlush`); the host then does what `onTimeoutFlush` does (drains).
    pub fn on_timeout(&mut self) {
        self.timeout_at = None;
        if self.destroyed {
            return;
        }
        self.try_force_flush();
    }

    fn try_force_flush(&mut self) {
        if self.paste.is_some() || self.pending_since_ms.is_none() || self.pending.len() == 0 {
            return;
        }
        self.force_flush = true;
    }

    /// `reset()`.
    pub fn reset(&mut self) {
        if self.destroyed {
            return;
        }
        self.clear_timeout();
        self.reset_state();
    }

    /// `hasPendingPixelResolutionResponse()`.
    pub fn has_pending_pixel_resolution_response(&self) -> bool {
        if !self.protocol_context.pixel_resolution_query_active || self.pending.len() == 0 {
            return false;
        }
        can_still_be_pixel_resolution_prefix(self.pending.view())
    }

    /// `pausePendingTimeout()`.
    pub fn pause_pending_timeout(&mut self) -> Result<(), Destroyed> {
        self.ensure_alive()?;
        self.pending_timeout_paused = true;
        self.suspended_pixel_resolution_prefix_length = self.pending.len();
        self.clear_timeout();
        Ok(())
    }

    /// `resumePendingTimeout()`.
    pub fn resume_pending_timeout(&mut self) -> Result<(), Destroyed> {
        self.ensure_alive()?;
        if self.pending.len() == 0 {
            self.pending_timeout_paused = false;
        }
        self.reconcile_timeout_state();
        Ok(())
    }

    /// `resetMouseState()`.
    pub fn reset_mouse_state(&mut self) -> Result<(), Destroyed> {
        self.ensure_alive()?;
        self.mouse_parser.reset();
        Ok(())
    }

    /// `destroy()`.
    pub fn destroy(&mut self) {
        if self.destroyed {
            return;
        }
        self.clear_timeout();
        self.destroyed = true;
        self.reset_state();
    }

    /// Flushes the unit `[unit_start, end)` as an opaque `unknown` response and returns to ground.
    fn opaque_to(&mut self, end: usize) {
        let bytes = self.slice(self.unit_start, end);
        self.emit_opaque_response(ResponseProtocol::Unknown, &bytes);
        self.state = State::Ground;
        self.consume_prefix(end);
    }

    /// `scanPending()`: the state machine over the pending bytes.
    fn scan_pending(&mut self) {
        while self.paste.is_none() {
            let length = self.pending.len();
            if self.state == State::Ground && self.cursor >= length {
                self.pending.clear();
                self.cursor = 0;
                self.unit_start = 0;
                self.pending_since_ms = None;
                self.force_flush = false;
                return;
            }
            let at_end = self.cursor >= length;
            let byte = self.byte(self.cursor);
            match self.state {
                State::Ground => {
                    self.unit_start = self.cursor;
                    if self.just_flushed_esc {
                        self.just_flushed_esc = false;
                        if byte == 0x5b {
                            self.cursor += 1;
                            self.state = State::EscRecovery;
                            continue;
                        }
                    }
                    if byte == ESC {
                        self.cursor += 1;
                        self.state = State::Esc;
                        continue;
                    }
                    if byte < 0x80 {
                        let text = self.utf8(self.cursor, self.cursor + 1);
                        self.emit_key_or_response(ResponseProtocol::Unknown, text);
                        self.consume_prefix(self.cursor + 1);
                        continue;
                    }
                    let expected = utf8_sequence_length(byte);
                    if expected == 0 {
                        if !self.force_flush && self.cursor + 1 == length {
                            self.mark_pending();
                            return;
                        }
                        self.emit_legacy_high_byte(byte as u8);
                        self.consume_prefix(self.cursor + 1);
                        continue;
                    }
                    self.cursor += 1;
                    self.state = State::Utf8 { expected, seen: 1 };
                }

                State::Utf8 { expected, seen } => {
                    if at_end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        let lead = self.byte(self.unit_start) as u8;
                        self.emit_legacy_high_byte(lead);
                        self.state = State::Ground;
                        self.consume_prefix(self.unit_start + 1);
                        continue;
                    }
                    if byte & 0xc0 != 0x80 {
                        let lead = self.byte(self.unit_start) as u8;
                        self.emit_legacy_high_byte(lead);
                        self.state = State::Ground;
                        self.consume_prefix(self.unit_start + 1);
                        continue;
                    }
                    self.cursor += 1;
                    if seen + 1 < expected {
                        self.state = State::Utf8 { expected, seen: seen + 1 };
                        continue;
                    }
                    let text = self.utf8(self.unit_start, self.cursor);
                    self.emit_key_or_response(ResponseProtocol::Unknown, text);
                    self.state = State::Ground;
                    self.consume_prefix(self.cursor);
                }

                State::Esc => {
                    if at_end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        let lone = self.cursor == self.unit_start + 1 && self.byte(self.unit_start) == ESC;
                        let text = self.utf8(self.unit_start, self.cursor);
                        self.emit_key_or_response(ResponseProtocol::Unknown, text);
                        self.just_flushed_esc = lone;
                        self.state = State::Ground;
                        self.consume_prefix(self.cursor);
                        continue;
                    }
                    self.cursor += 1;
                    match byte {
                        0x5b => self.state = State::Csi,
                        0x4f => self.state = State::Ss3,
                        0x5d => self.state = State::Osc { saw_esc: false },
                        0x50 => self.state = State::Dcs { saw_esc: false },
                        0x5f => self.state = State::Apc { saw_esc: false },
                        ESC => {}
                        _ => {
                            let text = self.utf8(self.unit_start, self.cursor);
                            self.emit_key_or_response(ResponseProtocol::Unknown, text);
                            self.state = State::Ground;
                            self.consume_prefix(self.cursor);
                        }
                    }
                }

                State::Ss3 => {
                    if at_end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if byte == ESC {
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    self.cursor += 1;
                    let text = self.utf8(self.unit_start, self.cursor);
                    self.emit_key_or_response(ResponseProtocol::Unknown, text);
                    self.state = State::Ground;
                    self.consume_prefix(self.cursor);
                }

                State::EscRecovery => {
                    if at_end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        let text = self.utf8(self.unit_start, self.cursor);
                        self.emit_key_or_response(ResponseProtocol::Unknown, text);
                        self.state = State::Ground;
                        self.consume_prefix(self.cursor);
                        continue;
                    }
                    if byte == 0x3c {
                        self.cursor += 1;
                        self.state = State::EscLessMouse;
                        continue;
                    }
                    if byte == 0x4d {
                        self.cursor += 1;
                        self.state = State::EscLessX10Mouse;
                        continue;
                    }
                    let text = self.utf8(self.unit_start, self.unit_start + 1);
                    self.emit_key_or_response(ResponseProtocol::Unknown, text);
                    self.state = State::Ground;
                    self.consume_prefix(self.unit_start + 1);
                }

                State::Csi => {
                    if at_end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if byte == ESC {
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if byte == 0x4d && self.cursor == self.unit_start + 2 {
                        let end = self.cursor + 4;
                        if length < end {
                            if !self.force_flush {
                                self.mark_pending();
                                return;
                            }
                            self.opaque_to(length);
                            continue;
                        }
                        let raw = self.slice(self.unit_start, end);
                        self.emit_mouse(&raw, crate::MouseEncoding::X10);
                        self.state = State::Ground;
                        self.consume_prefix(end);
                        continue;
                    }
                    if byte == 0x24 {
                        let candidate_end = self.cursor + 1;
                        // /^\x1b\[\d+\$$/
                        let candidate = &self.pending.view()[self.unit_start..candidate_end];
                        let digits = &candidate[2..candidate.len() - 1];
                        if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                            let text = decode_utf8(candidate);
                            self.emit_key_or_response(ResponseProtocol::Csi, text);
                            self.state = State::Ground;
                            self.consume_prefix(candidate_end);
                            continue;
                        }
                        if !self.force_flush && candidate_end >= length {
                            self.mark_pending();
                            return;
                        }
                    }
                    if byte == 0x3c && self.cursor == self.unit_start + 2 {
                        self.cursor += 1;
                        self.state = State::CsiSgrMouse { part: 0, has_digit: false };
                        continue;
                    }
                    if byte == 0x5b && self.cursor == self.unit_start + 2 {
                        self.cursor += 1;
                        continue;
                    }
                    if byte == 0x3f && self.cursor == self.unit_start + 2 {
                        self.cursor += 1;
                        self.state =
                            State::CsiPrivateReply(PrivateReply { semicolons: 0, has_digit: false, saw_dollar: false });
                        continue;
                    }
                    if byte == 0x3b {
                        let first = self.unit_start + 2;
                        let view = self.pending.view();
                        let mut value = parse_positive_decimal_prefix(view, first, self.cursor);
                        if value.is_none() && self.protocol_context.kitty_keyboard_enabled {
                            value = parse_kitty_first_field_codepoint(view, first, self.cursor);
                        }
                        if value.is_some() {
                            self.cursor += 1;
                            self.state = State::CsiParametric(Parametric {
                                semicolons: 1,
                                segments: 1,
                                has_digit: false,
                                first_param_value: value,
                            });
                            continue;
                        }
                    }
                    if (0x40..=0x7e).contains(&byte) {
                        let end = self.cursor + 1;
                        let raw = self.slice(self.unit_start, end);
                        if raw == BRACKETED_PASTE_START {
                            self.state = State::Ground;
                            self.consume_prefix(end);
                            self.paste = Some(PasteCollector::default());
                            continue;
                        }
                        if is_mouse_sgr_sequence(&raw) {
                            self.emit_mouse(&raw, crate::MouseEncoding::Sgr);
                            self.state = State::Ground;
                            self.consume_prefix(end);
                            continue;
                        }
                        self.emit_key_or_response(ResponseProtocol::Csi, decode_utf8(&raw));
                        self.state = State::Ground;
                        self.consume_prefix(end);
                        continue;
                    }
                    self.cursor += 1;
                }

                State::CsiSgrMouse { part, has_digit } => {
                    if at_end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        self.state = State::CsiSgrMouseDeferred { part, has_digit };
                        self.pending_since_ms = None;
                        self.force_flush = false;
                        return;
                    }
                    if byte == ESC {
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if is_digit(byte) {
                        self.cursor += 1;
                        self.state = State::CsiSgrMouse { part, has_digit: true };
                        continue;
                    }
                    if byte == 0x3b && has_digit && part < 2 {
                        self.cursor += 1;
                        self.state = State::CsiSgrMouse { part: part + 1, has_digit: false };
                        continue;
                    }
                    if (0x40..=0x7e).contains(&byte) {
                        let end = self.cursor + 1;
                        let raw = self.slice(self.unit_start, end);
                        if is_mouse_sgr_sequence(&raw) {
                            self.emit_mouse(&raw, crate::MouseEncoding::Sgr);
                        } else {
                            self.emit_key_or_response(ResponseProtocol::Csi, decode_utf8(&raw));
                        }
                        self.state = State::Ground;
                        self.consume_prefix(end);
                        continue;
                    }
                    self.state = State::Csi;
                }

                State::CsiSgrMouseDeferred { part, has_digit } => {
                    if at_end {
                        self.pending_since_ms = None;
                        self.force_flush = false;
                        return;
                    }
                    if byte == ESC {
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if is_digit(byte) || byte == 0x3b || byte == 0x4d || byte == 0x6d {
                        self.state = State::CsiSgrMouse { part, has_digit };
                        continue;
                    }
                    self.opaque_to(self.cursor);
                }

                State::CsiParametric(state) => {
                    if at_end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        if can_defer_parametric_csi(&state, &self.protocol_context) {
                            self.state = State::CsiParametricDeferred(state);
                            self.pending_since_ms = None;
                            self.force_flush = false;
                            return;
                        }
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if byte == ESC {
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if is_digit(byte) {
                        self.cursor += 1;
                        self.state = State::CsiParametric(Parametric { has_digit: true, ..state });
                        continue;
                    }
                    if byte == 0x3a && state.has_digit && state.segments < 3 {
                        self.cursor += 1;
                        self.state = State::CsiParametric(Parametric {
                            segments: state.segments + 1,
                            has_digit: false,
                            ..state
                        });
                        continue;
                    }
                    if byte == 0x3b && state.semicolons < 2 {
                        self.cursor += 1;
                        self.state = State::CsiParametric(Parametric {
                            semicolons: state.semicolons + 1,
                            segments: 1,
                            has_digit: false,
                            ..state
                        });
                        continue;
                    }
                    if (0x40..=0x7e).contains(&byte) {
                        let end = self.cursor + 1;
                        let protocol = classify_parametric_csi_protocol(&state, byte);
                        let text = self.utf8(self.unit_start, end);
                        self.emit_key_or_response(protocol, text);
                        self.state = State::Ground;
                        self.consume_prefix(end);
                        continue;
                    }
                    self.state = State::Csi;
                }

                State::CsiParametricDeferred(state) => {
                    if at_end {
                        self.pending_since_ms = None;
                        self.force_flush = false;
                        return;
                    }
                    if byte == ESC {
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if is_digit(byte)
                        || byte == 0x3a
                        || byte == 0x3b
                        || can_complete_deferred_parametric_csi(&state, byte, &self.protocol_context)
                    {
                        self.state = State::CsiParametric(state);
                        continue;
                    }
                    self.opaque_to(self.cursor);
                }

                State::CsiParametricIgnored(state) => {
                    if at_end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        self.state = State::Ground;
                        self.consume_prefix(self.cursor);
                        continue;
                    }
                    if byte == ESC {
                        self.state = State::Ground;
                        self.consume_prefix(self.cursor);
                        continue;
                    }
                    if is_digit(byte) {
                        self.cursor += 1;
                        let first_param_value = if state.semicolons == 0 {
                            Some(state.first_param_value.unwrap_or(0.0) * 10.0 + (byte - 0x30) as f64)
                        } else {
                            state.first_param_value
                        };
                        self.state =
                            State::CsiParametricIgnored(Parametric { has_digit: true, first_param_value, ..state });
                        continue;
                    }
                    if byte == 0x3b && state.semicolons == 0 && state.has_digit {
                        if self.protocol_context.explicit_width_cpr_active && state.first_param_value == Some(1.0) {
                            self.state = State::Csi;
                            continue;
                        }
                        self.cursor += 1;
                        self.state = State::CsiParametricIgnored(Parametric {
                            semicolons: 1,
                            segments: 1,
                            has_digit: false,
                            first_param_value: state.first_param_value,
                        });
                        continue;
                    }
                    if byte == 0x52 && state.semicolons == 1 && state.has_digit {
                        let end = self.cursor + 1;
                        self.state = State::Ground;
                        self.consume_prefix(end);
                        continue;
                    }
                    if state.semicolons == 0 {
                        self.state = State::Csi;
                        continue;
                    }
                    self.state = State::Ground;
                    self.consume_prefix(self.cursor);
                }

                State::CsiPrivateReply(state) => {
                    if at_end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        if self.protocol_context.private_capability_replies_active {
                            self.state = State::CsiPrivateReplyDeferred(state);
                            self.pending_since_ms = None;
                            self.force_flush = false;
                            return;
                        }
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if byte == ESC {
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if is_digit(byte) {
                        self.cursor += 1;
                        self.state = State::CsiPrivateReply(PrivateReply { has_digit: true, ..state });
                        continue;
                    }
                    if byte == 0x3b {
                        self.cursor += 1;
                        self.state = State::CsiPrivateReply(PrivateReply {
                            semicolons: state.semicolons + 1,
                            has_digit: false,
                            saw_dollar: false,
                        });
                        continue;
                    }
                    if byte == 0x24 && state.has_digit && !state.saw_dollar {
                        self.cursor += 1;
                        self.state =
                            State::CsiPrivateReply(PrivateReply { has_digit: true, saw_dollar: true, ..state });
                        continue;
                    }
                    if (0x40..=0x7e).contains(&byte) {
                        let end = self.cursor + 1;
                        let raw = self.slice(self.unit_start, end);
                        self.emit_opaque_response(ResponseProtocol::Csi, &raw);
                        self.state = State::Ground;
                        self.consume_prefix(end);
                        continue;
                    }
                    self.state = State::Csi;
                }

                State::CsiPrivateReplyDeferred(state) => {
                    if at_end {
                        self.pending_since_ms = None;
                        self.force_flush = false;
                        return;
                    }
                    if byte == ESC {
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if is_digit(byte)
                        || byte == 0x3b
                        || byte == 0x24
                        || can_complete_deferred_private_reply_csi(&state, byte, &self.protocol_context)
                    {
                        self.state = State::CsiPrivateReply(state);
                        continue;
                    }
                    self.opaque_to(self.cursor);
                }

                State::Osc { saw_esc } | State::Dcs { saw_esc } | State::Apc { saw_esc } => {
                    let (protocol, rebuild): (ResponseProtocol, fn(bool) -> State) = match self.state {
                        State::Osc { .. } => (ResponseProtocol::Osc, |saw_esc| State::Osc { saw_esc }),
                        State::Dcs { .. } => (ResponseProtocol::Dcs, |saw_esc| State::Dcs { saw_esc }),
                        _ => (ResponseProtocol::Apc, |saw_esc| State::Apc { saw_esc }),
                    };
                    if at_end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if saw_esc {
                        if byte == 0x5c {
                            let end = self.cursor + 1;
                            let raw = self.slice(self.unit_start, end);
                            self.emit_opaque_response(protocol, &raw);
                            self.state = State::Ground;
                            self.consume_prefix(end);
                            continue;
                        }
                        self.state = rebuild(false);
                        continue;
                    }
                    // OSC also ends at BEL; DCS and APC only at ESC \.
                    if byte == BEL && protocol == ResponseProtocol::Osc {
                        let end = self.cursor + 1;
                        let raw = self.slice(self.unit_start, end);
                        self.emit_opaque_response(protocol, &raw);
                        self.state = State::Ground;
                        self.consume_prefix(end);
                        continue;
                    }
                    self.cursor += 1;
                    if byte == ESC {
                        self.state = rebuild(true);
                    }
                }

                State::EscLessMouse => {
                    if at_end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        self.opaque_to(self.cursor);
                        continue;
                    }
                    if is_digit(byte) || byte == 0x3b {
                        self.cursor += 1;
                        continue;
                    }
                    if byte == 0x4d || byte == 0x6d {
                        let end = self.cursor + 1;
                        let raw = self.slice(self.unit_start, end);
                        let mut prefixed = vec![ESC as u8];
                        prefixed.extend_from_slice(&raw);
                        if is_mouse_sgr_sequence(&prefixed) {
                            self.emit_mouse(&prefixed, crate::MouseEncoding::Sgr);
                        } else {
                            self.emit_opaque_response(ResponseProtocol::Unknown, &raw);
                        }
                        self.state = State::Ground;
                        self.consume_prefix(end);
                        continue;
                    }
                    self.opaque_to(self.cursor);
                }

                State::EscLessX10Mouse => {
                    let end = self.unit_start + 5;
                    if length < end {
                        if !self.force_flush {
                            self.mark_pending();
                            return;
                        }
                        self.opaque_to(length);
                        continue;
                    }
                    let raw = self.slice(self.unit_start, end);
                    let mut prefixed = vec![ESC as u8];
                    prefixed.extend_from_slice(&raw);
                    self.emit_mouse(&prefixed, crate::MouseEncoding::X10);
                    self.state = State::Ground;
                    self.consume_prefix(end);
                }
            }
        }
    }

    /// `emitKeyOrResponse`: a key if `parseKeypress` takes it, else a response.
    fn emit_key_or_response(&mut self, protocol: ResponseProtocol, raw: String) {
        match parse_keypress(&raw, self.use_kitty_keyboard) {
            Some(key) => self.events.push_back(StdinEvent::Key { raw: key.raw.clone(), key }),
            None => self.events.push_back(StdinEvent::Response { protocol, sequence: raw }),
        }
    }

    fn emit_mouse(&mut self, raw: &[u8], encoding: crate::MouseEncoding) {
        match self.mouse_parser.parse_mouse_event(raw) {
            Some(event) => self.events.push_back(StdinEvent::Mouse { raw: decode_latin1(raw), encoding, event }),
            None => self.emit_opaque_response(ResponseProtocol::Unknown, raw),
        }
    }

    /// `emitLegacyHighByte`: a byte above 0x7F that is not a UTF-8 lead, through the meta-key path.
    fn emit_legacy_high_byte(&mut self, byte: u8) {
        match parse_keypress_high_byte(byte, self.use_kitty_keyboard) {
            Some(key) => self.events.push_back(StdinEvent::Key { raw: key.raw.clone(), key }),
            None => self.events.push_back(StdinEvent::Response {
                protocol: ResponseProtocol::Unknown,
                sequence: (byte as char).to_string(),
            }),
        }
    }

    fn emit_opaque_response(&mut self, protocol: ResponseProtocol, raw: &[u8]) {
        self.events.push_back(StdinEvent::Response { protocol, sequence: decode_latin1(raw) });
    }

    fn consume_prefix(&mut self, end: usize) {
        self.pending.consume(end);
        self.pending_timeout_paused = false;
        self.suspended_pixel_resolution_prefix_length = 0;
        self.cursor = 0;
        self.unit_start = 0;
        self.pending_since_ms = None;
        self.force_flush = false;
    }

    fn take_pending_bytes(&mut self) -> Vec<u8> {
        let buffered = self.pending.take();
        self.cursor = 0;
        self.unit_start = 0;
        self.pending_since_ms = None;
        self.force_flush = false;
        buffered
    }

    /// `flushPendingOverflow`: everything pending as one opaque response.
    fn flush_pending_overflow(&mut self) {
        if self.pending.len() == 0 {
            return;
        }
        let bytes = self.pending.view().to_vec();
        self.emit_opaque_response(ResponseProtocol::Unknown, &bytes);
        self.pending.clear();
        self.cursor = 0;
        self.unit_start = 0;
        self.pending_since_ms = None;
        self.pending_timeout_paused = false;
        self.suspended_pixel_resolution_prefix_length = 0;
        self.force_flush = false;
        self.state = State::Ground;
    }

    fn mark_pending(&mut self) {
        self.pending_since_ms = Some(self.clock.now());
    }

    /// `consumePasteBytes`: the paste body up to the end marker (which may split across chunks); returns what
    /// follows the marker.
    fn consume_paste_bytes(&mut self, chunk: &[u8]) -> Vec<u8> {
        let paste = self.paste.as_mut().unwrap();
        let mut combined = std::mem::take(&mut paste.tail);
        combined.extend_from_slice(chunk);
        if let Some(end) = index_of_bytes(&combined, BRACKETED_PASTE_END) {
            paste.body.extend_from_slice(&combined[..end]);
            let bytes = std::mem::take(&mut paste.body);
            self.events.push_back(StdinEvent::Paste { bytes, metadata: None });
            self.paste = None;
            return combined.split_off(end + BRACKETED_PASTE_END.len());
        }
        let keep = (BRACKETED_PASTE_END.len() - 1).min(combined.len());
        let stable = combined.len() - keep;
        paste.body.extend_from_slice(&combined[..stable]);
        paste.tail = combined.split_off(stable);
        Vec::new()
    }

    fn reconcile_deferred_state_with_protocol_context(&mut self) {
        let release = match self.state {
            State::CsiParametricDeferred(state) => !can_defer_parametric_csi(&state, &self.protocol_context),
            State::CsiPrivateReplyDeferred(_) => !self.protocol_context.private_capability_replies_active,
            _ => false,
        };
        if release {
            self.opaque_to(self.cursor);
        }
    }

    /// `reconcileTimeoutState`: (re)arms the timeout while an incomplete unit waits outside a paste.
    fn reconcile_timeout_state(&mut self) {
        if !self.arm_timeouts {
            return;
        }
        if self.pending_timeout_paused
            || self.paste.is_some()
            || self.pending_since_ms.is_none()
            || self.pending.len() == 0
        {
            self.clear_timeout();
            return;
        }
        self.timeout_at = Some(self.clock.now() + self.timeout_ms);
    }

    fn clear_timeout(&mut self) {
        self.timeout_at = None;
    }

    fn reset_state(&mut self) {
        self.pending.reset(INITIAL_PENDING_CAPACITY);
        self.events.clear();
        self.pending_since_ms = None;
        self.pending_timeout_paused = false;
        self.suspended_pixel_resolution_prefix_length = 0;
        self.force_flush = false;
        self.just_flushed_esc = false;
        self.state = State::Ground;
        self.cursor = 0;
        self.unit_start = 0;
        self.paste = None;
        self.mouse_parser.reset();
    }
}
