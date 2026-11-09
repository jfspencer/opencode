//! The terminal: `terminal.zig` (capabilities, modes, the startup queries and what their replies change,
//! notifications, OSC 52), `session.zig` (the setup/suspend/resume/close lifecycle as control packets) and the
//! parts of `renderer.ts` that answer the terminal's replies (`processCapabilitySequence`, the pixel
//! resolution, focus and theme-mode handlers, `renderer-theme-mode.ts`, palette detection with
//! `lib/terminal-palette.ts`, the startup reply batch) and `lib/terminal-capability-detection.ts`.
//!
//! Everything here is without I/O: methods append the bytes to write to a caller-owned `Vec`, and replies come
//! in already split by the stdin parser. The host loop:
//! - creates a [`crate::output::Renderer`] with a [`Terminal::new`] from the environment, a [`Session`] and a
//!   [`Replies`];
//! - calls [`Session::setup_terminal`], then [`Session::pump`] after each write has drained until it returns
//!   [`Pump::Idle`], then [`Replies::after_setup`] (which queries the pixel size and the palette as the
//!   renderer does once `setupTerminal` resolves);
//! - hands every terminal reply to [`Replies`]: [`Replies::handle_cpr`] for cursor position reports (parser
//!   protocol `cpr`), [`Replies::handle_osc`] for every OSC reply (the palette detector listens to all of them),
//!   then [`Replies::handle_sequence`] for every reply and every key's raw sequence, which returns whether one of
//!   the renderer's own handlers took it (capability replies, the pixel size `CSI 4;h;w t`, focus `CSI I`/`CSI O`,
//!   the theme colors OSC 10/11 and `CSI ?997;n`); unhandled sequences go on to the application;
//! - calls [`Replies::tick`] at the deadlines it reports (`next_deadline`), and on teardown
//!   [`Session::begin_close`] then [`Session::pump`] until [`Pump::Closed`], waiting for the `WaitUntil`
//!   deadlines in between (the cursor-settle delays of `session.zig`).
//!
//! Replies the parser produces (`StdinEvent` `response` in `lib/stdin-parser.ts`): DECRPM `CSI ? n ; m $ y`,
//! the kitty keyboard flags `CSI ? n u`, DA1 `CSI ? ... c`, CPR `CSI r ; c R`, XTVERSION `DCS > | ... ST`,
//! XTGETTCAP `DCS 1 + r ... ST`, kitty graphics `APC G ... ST`, OSC 10/11/4 colors, OSC 99, OSC 1337, OSC 7501,
//! the pixel size `CSI 4 ; h ; w t`, color-scheme `CSI ? 997 ; n n`, focus `CSI I`/`CSI O`.
//!
//! Divergences: kitty image transport probes, Windows console defaults and the main-screen/split-footer row
//! reservation are not ported (the TUI uses the alternate screen and no images); clipboard and notification
//! writes are always "accepted" (there is no output queue to be full); the reply sentinel (native input parser
//! only) is kept as an option but the TUI runs the legacy parser, so its replies end the startup batch only by
//! the 5 s backstop, as in TS.
use std::collections::HashMap;

use crate::color::{self, RGBA};
use crate::output::{Renderer, push_fmt};
use crate::unicode::WidthMethod;

// ---- terminal.zig ----

/// `Capabilities`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub kitty_keyboard: bool,
    pub kitty_graphics: bool,
    pub rgb: bool,
    pub ansi256: bool,
    pub unicode: WidthMethod,
    pub sgr_pixels: bool,
    pub color_scheme_updates: bool,
    pub explicit_width: bool,
    pub scaled_text: bool,
    pub sixel: bool,
    pub focus_tracking: bool,
    pub sync: bool,
    pub bracketed_paste: bool,
    pub hyperlinks: bool,
    pub osc52: bool,
    pub notifications: bool,
    pub explicit_cursor_positioning: bool,
    pub remote: bool,
    pub program_status: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            kitty_keyboard: false,
            kitty_graphics: false,
            rgb: false,
            ansi256: false,
            unicode: WidthMethod::Unicode,
            sgr_pixels: false,
            color_scheme_updates: false,
            explicit_width: false,
            scaled_text: false,
            sixel: false,
            focus_tracking: false,
            sync: false,
            bracketed_paste: false,
            hyperlinks: false,
            osc52: false,
            notifications: false,
            explicit_cursor_positioning: false,
            remote: false,
            program_status: false,
        }
    }
}

/// `RemoteMode`: whether the terminal is across a link (`auto` decides from SSH/mosh variables).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RemoteMode {
    #[default]
    Auto,
    Local,
    Remote,
}

/// `Multiplexer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Multiplexer {
    #[default]
    None,
    Tmux,
    Zellij,
    Screen,
}

/// `SynchronizedOutput`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SynchronizedOutput {
    #[default]
    Auto,
    Always,
    Never,
}

/// `Osc52Support`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Osc52Support {
    #[default]
    Unknown,
    Supported,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
enum NotificationProtocol {
    #[default]
    None,
    Osc9,
    Osc777,
    Osc99,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
enum NotificationSource {
    #[default]
    None,
    Heuristic,
    Xtversion,
    Query,
    Override,
}

/// `CursorStyle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorStyle {
    Block,
    Line,
    Underline,
    #[default]
    Default,
}

/// `MousePointerStyle` (OSC 22 names).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MousePointerStyle {
    Auto,
    #[default]
    Default,
    None,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    AllScroll,
    ColResize,
    RowResize,
    NResize,
    EResize,
    SResize,
    WResize,
    NeResize,
    NwResize,
    SeResize,
    SwResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ZoomIn,
    ZoomOut,
}

impl MousePointerStyle {
    const NAMES: [&'static str; 36] = [
        "auto", "default", "none", "context-menu", "help", "pointer", "progress", "wait", "cell", "crosshair", "text",
        "vertical-text", "alias", "copy", "move", "no-drop", "not-allowed", "grab", "grabbing", "all-scroll",
        "col-resize", "row-resize", "n-resize", "e-resize", "s-resize", "w-resize", "ne-resize", "nw-resize",
        "se-resize", "sw-resize", "ew-resize", "ns-resize", "nesw-resize", "nwse-resize", "zoom-in", "zoom-out",
    ];
    const ALL: [MousePointerStyle; 36] = {
        use MousePointerStyle::*;
        [
            Auto, Default, None, ContextMenu, Help, Pointer, Progress, Wait, Cell, Crosshair, Text, VerticalText, Alias,
            Copy, Move, NoDrop, NotAllowed, Grab, Grabbing, AllScroll, ColResize, RowResize, NResize, EResize, SResize,
            WResize, NeResize, NwResize, SeResize, SwResize, EwResize, NsResize, NeswResize, NwseResize, ZoomIn, ZoomOut,
        ]
    };

    /// `toName`.
    pub fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }

    /// The style a CSS-like name names (the TS `MousePointerStyle` strings).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::NAMES.iter().position(|n| *n == name).map(|i| Self::ALL[i])
    }
}

/// `ClipboardTarget`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ClipboardTarget {
    #[default]
    Clipboard,
    Primary,
    Select,
    Secondary,
}

impl ClipboardTarget {
    fn char(self) -> u8 {
        match self {
            ClipboardTarget::Clipboard => b'c',
            ClipboardTarget::Primary => b'p',
            ClipboardTarget::Select => b's',
            ClipboardTarget::Secondary => b'q',
        }
    }
}

/// `TerminalInfo`: the terminal's name and version (from XTVERSION, or `TERM_PROGRAM`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalInfo {
    pub name: String,
    pub version: String,
    pub from_xtversion: bool,
}

/// The cursor as the renderer shows it (1-based position).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorPosition {
    pub x: u32,
    pub y: u32,
    pub visible: bool,
}

/// The cursor's style and blinking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorStyleState {
    pub style: CursorStyle,
    pub blinking: bool,
}

/// The terminal modes that are on (`Terminal.state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalState {
    pub alt_screen: bool,
    pub kitty_keyboard: bool,
    pub kitty_keyboard_flags: u8,
    pub bracketed_paste: bool,
    pub mouse: bool,
    pub mouse_movement: bool,
    pub mouse_was_enabled: bool,
    pub pixel_mouse: bool,
    pub color_scheme_updates: bool,
    pub theme_queries_sent: bool,
    pub focus_tracking: bool,
    pub modify_other_keys: bool,
    pub mouse_pointer: MousePointerStyle,
    cursor_x: u32,
    cursor_y: u32,
    cursor_row: u16,
    cursor_visible: bool,
    cursor_style: CursorStyle,
    cursor_blinking: bool,
    cursor_color: RGBA,
}

impl Default for TerminalState {
    fn default() -> Self {
        TerminalState {
            alt_screen: false,
            kitty_keyboard: false,
            kitty_keyboard_flags: 0,
            bracketed_paste: false,
            mouse: false,
            mouse_movement: true,
            mouse_was_enabled: false,
            pixel_mouse: false,
            color_scheme_updates: false,
            theme_queries_sent: false,
            focus_tracking: false,
            modify_other_keys: false,
            mouse_pointer: MousePointerStyle::Default,
            cursor_x: 1,
            cursor_y: 1,
            cursor_row: 0,
            cursor_visible: true,
            cursor_style: CursorStyle::Default,
            cursor_blinking: false,
            cursor_color: RGBA::rgb(255, 255, 255, 255),
        }
    }
}

const NOTIFICATION_QUERY_ID: &str = "opentui-notifications";

// ANSI sequences (ansi.zig).
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const RESET: &[u8] = b"\x1b[0m";
const RESET_MOUSE_POINTER: &[u8] = b"\x1b]22;\x1b\\";
const SAVE_CURSOR: &[u8] = b"\x1b[s";
const RESTORE_CURSOR: &[u8] = b"\x1b[u";
const ALT_SCREEN_ON: &[u8] = b"\x1b[?1049h";
const ALT_SCREEN_OFF: &[u8] = b"\x1b[?1049l";
const PRIMARY_DEVICE_ATTRS: &[u8] = b"\x1b[c";
const XTVERSION: &[u8] = b"\x1b[>0q";
const CURSOR_POSITION_REQUEST: &[u8] = b"\x1b[6n";
const HOME: &[u8] = b"\x1b[H";
const EXPLICIT_WIDTH_QUERY: &[u8] = b"\x1b]66;w=1; \x1b\\";
const SCALED_TEXT_QUERY: &[u8] = b"\x1b]66;s=2; \x1b\\";
const CAPABILITY_QUERIES_BASE: &[u8] =
    b"\x1bP+q4d73\x1b\\\x1b[?1016$p\x1b[?2027$p\x1b[?2031$p\x1b[?1004$p\x1b[?2004$p\x1b[?2026$p";
const CSI_U_QUERY: &[u8] = b"\x1b[?u";
const NOTIFICATION_QUERIES: &[u8] = b"\x1b]99;i=opentui-notifications:p=?;\x1b\\\x1b]1337;Capabilities\x1b\\";
const PROGRAM_STATUS_QUERY: &[u8] = b"\x1b]7501;?\x1b\\";
const KITTY_GRAPHICS_QUERY: &[u8] = b"\x1b_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
const OSC_THEME_QUERIES: &[u8] = b"\x1b]10;?\x07\x1b]11;?\x07";
const QUERY_PIXEL_SIZE: &[u8] = b"\x1b[14t";
const RESET_TERMINAL_BG: &[u8] = b"\x1b]111\x07";

/// The terminal (`Terminal` in terminal.zig).
#[derive(Clone, Debug)]
pub struct Terminal {
    caps: Capabilities,
    kitty_keyboard_flags: u8,
    remote_mode: RemoteMode,
    env: Option<HashMap<String, String>>,
    remote: bool,
    multiplexer: Multiplexer,
    osc52_support: Osc52Support,
    is_foot: bool,
    skip_graphics_query: bool,
    graphics_enabled: bool,
    kitty_graphics_queried: bool,
    sixel_queried: bool,
    skip_explicit_width_query: bool,
    startup_cursor_query_pending: bool,
    startup_cursor_query_captured: bool,
    explicit_width_probe_reports_pending: u8,
    /// The host ends each query batch with DA1 and waits for its reply (`reply_sentinel`).
    pub reply_sentinel: bool,
    /// `synchronized_output`.
    pub synchronized: SynchronizedOutput,
    sync_reply_settled: bool,
    /// A multiplexer runs between the app and the link on this host (`multiplexer_on_host`).
    pub multiplexer_on_host: bool,
    unicode_wide_locked: Option<bool>,
    host_env_hyperlinks: bool,
    hyperlinks_override: Option<bool>,
    /// The modes that are on.
    pub state: TerminalState,
    term_info: TerminalInfo,
    notification_protocol: NotificationProtocol,
    notification_source: NotificationSource,
    notification_id_counter: u64,
}

fn find(haystack: &str, needle: &str) -> bool {
    haystack.contains(needle)
}

fn find_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())
}

fn is_osc52_term(value: &str) -> bool {
    ["iterm", "kitty", "alacritty", "wezterm", "contour", "foot", "rio", "ghostty", "tmux", "screen"]
        .iter()
        .any(|n| find_ignore_case(value, n))
}

fn is_hyperlink_term(value: &str) -> bool {
    ["ghostty", "kitty", "wezterm", "alacritty", "foot", "iterm", "xterm.js", "terminology"].iter().any(|n| find_ignore_case(value, n))
        || value.eq_ignore_ascii_case("rex")
        || value.eq_ignore_ascii_case("xterm-rex")
}

fn detect_notification_protocol(value: &str) -> Option<NotificationProtocol> {
    if find_ignore_case(value, "kitty") || find_ignore_case(value, "foot") {
        return Some(NotificationProtocol::Osc99);
    }
    if [
        "ghostty", "wezterm", "warp", "hterm", "blink", "contour", "vte", "gnome", "tilix", "terminator", "xfce", "urxvt",
        "rxvt", "windows terminal", "windows_terminal", "terminology",
    ]
    .iter()
    .any(|n| find_ignore_case(value, n))
    {
        return Some(NotificationProtocol::Osc777);
    }
    if ["iterm", "Apple_Terminal", "Terminal.app", "conemu"].iter().any(|n| find_ignore_case(value, n)) {
        return Some(NotificationProtocol::Osc9);
    }
    None
}

/// `termFeaturesHasCode` (iTerm2 feature strings: capitalized codes with optional digits).
fn term_features_has_code(features: &str, code: &str) -> bool {
    let bytes = features.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if !c.is_ascii_alphanumeric() {
            break;
        }
        if !c.is_ascii_uppercase() {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_lowercase() {
            i += 1;
        }
        if &features[start..i] == code {
            return true;
        }
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    false
}

/// `findOscTerminator`: the offset of the first BEL or ST.
fn find_osc_terminator(payload: &str) -> usize {
    let bel = payload.find('\x07');
    let st = payload.find("\x1b\\");
    match (bel, st) {
        (Some(b), Some(s)) => b.min(s),
        (Some(b), None) => b,
        (None, Some(s)) => s,
        (None, None) => payload.len(),
    }
}

fn semantic_version_at_least(version: &str, major_req: u32, minor_req: u32) -> bool {
    let (core, suffix) = match version.find('-') {
        Some(i) => (&version[..i], Some(&version[i + 1..])),
        None => (version, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    let (Ok(major), Ok(minor), Ok(patch)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>(), parts[2].parse::<u32>()) else {
        return false;
    };
    if major > major_req || (major == major_req && minor > minor_req) {
        return true;
    }
    if major != major_req || minor != minor_req {
        return false;
    }
    if patch > 0 {
        return true;
    }
    let Some(suffix) = suffix else { return true };
    let parts: Vec<&str> = suffix.split('-').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].len() < 2 || !parts[1].starts_with('g') {
        return false;
    }
    parts[0].parse::<u32>().is_ok() && parts[1][1..].bytes().all(|c| c.is_ascii_hexdigit())
}

fn ghostty_wide_grapheme_widths(version: &str) -> bool {
    if semantic_version_at_least(version, 1, 3) {
        return true;
    }
    let prefix = "0.0.0-";
    if !version.starts_with(prefix) || version.len() <= prefix.len() + 8 || version.as_bytes()[prefix.len() + 8] != b'.' {
        return false;
    }
    let date = &version[prefix.len()..prefix.len() + 8];
    date.bytes().all(|c| c.is_ascii_digit()) && date.parse::<u32>().is_ok_and(|d| d >= 20260224)
}

fn wezterm_build_at_least(version: &str, required: u32) -> bool {
    let mut parts = version.split(['-', '.', '_']);
    let (Some(date), Some(time), Some(hash)) = (parts.next(), parts.next(), parts.next()) else { return false };
    if date.len() != 8 || time.len() != 6 || hash.len() != 8 {
        return false;
    }
    if parts.any(|p| p.is_empty()) {
        return false;
    }
    if !date.bytes().all(|c| c.is_ascii_digit()) || !time.bytes().all(|c| c.is_ascii_digit()) || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let year: u16 = date[0..4].parse().unwrap_or(0);
    let month: u8 = date[4..6].parse().unwrap_or(0);
    let day: u8 = date[6..8].parse().unwrap_or(0);
    if !(1..=12).contains(&month) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if day < 1 || day > days[month as usize - 1] {
        return false;
    }
    let hour: u8 = time[0..2].parse().unwrap_or(99);
    let minute: u8 = time[2..4].parse().unwrap_or(99);
    let second: u8 = time[4..6].parse().unwrap_or(99);
    if hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    date.parse::<u32>().is_ok_and(|d| d >= required)
}

impl Terminal {
    /// `Terminal.init` + `adoptHostEnvironment`: `env` is the forwarded environment (the renderer forwards a fixed
    /// list of variables, `DEFAULT_FORWARDED_ENV_KEYS`), or `None` for none at all.
    pub fn new(remote_mode: RemoteMode, env: Option<HashMap<String, String>>) -> Self {
        let mut terminal = Terminal {
            caps: Capabilities::default(),
            kitty_keyboard_flags: 0b00101,
            remote_mode,
            env: None,
            remote: false,
            multiplexer: Multiplexer::None,
            osc52_support: Osc52Support::Unknown,
            is_foot: false,
            skip_graphics_query: false,
            graphics_enabled: true,
            kitty_graphics_queried: false,
            sixel_queried: false,
            skip_explicit_width_query: false,
            startup_cursor_query_pending: false,
            startup_cursor_query_captured: false,
            explicit_width_probe_reports_pending: 0,
            reply_sentinel: false,
            synchronized: SynchronizedOutput::Auto,
            sync_reply_settled: false,
            multiplexer_on_host: false,
            unicode_wide_locked: None,
            host_env_hyperlinks: false,
            hyperlinks_override: None,
            state: TerminalState::default(),
            term_info: TerminalInfo::default(),
            notification_protocol: NotificationProtocol::None,
            notification_source: NotificationSource::None,
            notification_id_counter: 0,
        };
        terminal.check_environment_overrides();
        if env.is_some() {
            terminal.env = env;
            terminal.check_environment_overrides();
        }
        terminal
    }

    /// The variables `checkEnvironmentOverrides` reads, from this process's environment
    /// (`DEFAULT_FORWARDED_ENV_KEYS`).
    pub fn forwarded_environment() -> HashMap<String, String> {
        const KEYS: [&str; 33] = [
            "TMPDIR", "SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "MOSH_CONNECTION", "TMUX", "ZELLIJ", "ZELLIJ_SESSION_NAME",
            "ZELLIJ_PANE_ID", "TERM", "OPENTUI_GRAPHICS", "OPENTUI_IMAGE_PROTOCOL", "TERM_PROGRAM", "TERM_PROGRAM_VERSION",
            "TERM_FEATURES", "VTE_VERSION", "FORCE_HYPERLINK", "ALACRITTY_SOCKET", "ALACRITTY_LOG", "COLORTERM",
            "TERMUX_VERSION", "VHS_RECORD", "OPENTUI_FORCE_WCWIDTH", "OPENTUI_FORCE_UNICODE", "OPENTUI_FORCE_NOZWJ",
            "OPENTUI_FORCE_EXPLICIT_WIDTH", "OPENTUI_NOTIFICATION_PROTOCOL", "OPENTUI_NOTIFICATIONS", "WT_SESSION", "STY",
            "WSL_DISTRO_NAME", "WSL_INTEROP", "OPENTUI_FORCE_WCWIDTH",
        ];
        KEYS.iter().filter_map(|key| std::env::var(key).ok().map(|value| (key.to_string(), value))).collect()
    }

    fn env(&self, key: &str) -> Option<&str> {
        self.env.as_ref()?.get(key).map(String::as_str)
    }

    pub fn capabilities(&self) -> Capabilities {
        self.caps
    }

    pub fn terminal_info(&self) -> &TerminalInfo {
        &self.term_info
    }

    pub fn multiplexer(&self) -> Multiplexer {
        self.multiplexer
    }

    pub fn osc52_support(&self) -> Osc52Support {
        self.osc52_support
    }

    pub fn is_in_tmux(&self) -> bool {
        self.multiplexer == Multiplexer::Tmux
    }

    pub fn is_in_zellij(&self) -> bool {
        self.multiplexer == Multiplexer::Zellij
    }

    pub fn is_in_screen(&self) -> bool {
        self.multiplexer == Multiplexer::Screen
    }

    fn is_xtversion_named(&self, name: &str, ignore_case: bool) -> bool {
        self.term_info.from_xtversion && if ignore_case { self.term_info.name.eq_ignore_ascii_case(name) } else { self.term_info.name == name }
    }

    /// `synchronizedOutput`: whether frames are wrapped in mode 2026.
    pub fn synchronized_output(&self) -> bool {
        match self.synchronized {
            SynchronizedOutput::Always => true,
            SynchronizedOutput::Never => false,
            SynchronizedOutput::Auto => !self.remote || !self.sync_reply_settled || (self.caps.sync && !self.multiplexer_on_host),
        }
    }

    pub fn set_mouse_pointer_style(&mut self, style: MousePointerStyle) {
        self.state.mouse_pointer = style;
    }

    pub fn mouse_pointer(&self) -> MousePointerStyle {
        self.state.mouse_pointer
    }

    /// `setCursorPosition`: 1-based, at least 1.
    pub fn set_cursor_position(&mut self, x: u32, y: u32, visible: bool) {
        self.state.cursor_x = x.max(1);
        self.state.cursor_y = y.max(1);
        self.state.cursor_visible = visible;
        self.state.cursor_row = y.saturating_sub(1).min(u16::MAX as u32) as u16;
    }

    pub fn set_cursor_style(&mut self, style: CursorStyle, blinking: bool) {
        self.state.cursor_style = style;
        self.state.cursor_blinking = blinking;
    }

    pub fn set_cursor_color(&mut self, color: RGBA) {
        self.state.cursor_color = color;
    }

    pub fn cursor_position(&self) -> CursorPosition {
        CursorPosition { x: self.state.cursor_x, y: self.state.cursor_y, visible: self.state.cursor_visible }
    }

    pub fn cursor_style(&self) -> CursorStyleState {
        CursorStyleState { style: self.state.cursor_style, blinking: self.state.cursor_blinking }
    }

    pub fn cursor_color(&self) -> RGBA {
        self.state.cursor_color
    }

    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.kitty_keyboard_flags
    }

    /// `setKittyKeyboardFlags`: the flags the next push uses.
    pub fn set_kitty_keyboard_flags(&mut self, flags: u8) {
        self.kitty_keyboard_flags = flags;
    }

    fn clear_notification_protocol(&mut self) {
        self.notification_protocol = NotificationProtocol::None;
        self.notification_source = NotificationSource::None;
        self.caps.notifications = false;
    }

    fn set_notification_protocol(&mut self, protocol: NotificationProtocol, source: NotificationSource) {
        if protocol == NotificationProtocol::None {
            return;
        }
        if self.is_in_zellij() && source != NotificationSource::Override && (source != NotificationSource::Query || protocol != NotificationProtocol::Osc99) {
            return;
        }
        if source < self.notification_source {
            return;
        }
        if source == self.notification_source && protocol < self.notification_protocol {
            return;
        }
        self.notification_protocol = protocol;
        self.notification_source = source;
        self.caps.notifications = true;
    }

    fn enforce_notification_protocol_for_multiplexer(&mut self) {
        if !self.is_in_zellij() || self.notification_source == NotificationSource::Override {
            return;
        }
        if self.notification_source == NotificationSource::Query && self.notification_protocol == NotificationProtocol::Osc99 {
            return;
        }
        self.clear_notification_protocol();
    }

    fn apply_notification_heuristic(&mut self, value: &str) {
        if self.is_in_zellij() {
            return;
        }
        if let Some(protocol) = detect_notification_protocol(value) {
            self.set_notification_protocol(protocol, NotificationSource::Heuristic);
        }
    }

    fn apply_notification_protocol_override(&mut self, value: &str) {
        let lower = value.to_ascii_lowercase();
        if value == "0" || lower == "false" || lower == "off" || lower == "none" {
            self.clear_notification_protocol();
            self.notification_source = NotificationSource::Override;
            return;
        }
        if value == "1" || lower == "true" || lower == "on" {
            return;
        }
        match lower.as_str() {
            "osc99" => self.set_notification_protocol(NotificationProtocol::Osc99, NotificationSource::Override),
            "osc777" => self.set_notification_protocol(NotificationProtocol::Osc777, NotificationSource::Override),
            "osc9" => self.set_notification_protocol(NotificationProtocol::Osc9, NotificationSource::Override),
            _ => {}
        }
    }

    fn apply_known_unicode_width_identity(&mut self) {
        if let Some(locked) = self.unicode_wide_locked {
            if locked {
                self.caps.unicode = WidthMethod::UnicodeWide;
            }
            return;
        }
        if self.caps.unicode == WidthMethod::UnicodeWide {
            self.caps.unicode = WidthMethod::Unicode;
        }
        if self.remote || self.multiplexer != Multiplexer::None {
            return;
        }
        let Some(program) = self.env("TERM_PROGRAM") else { return };
        if !program.eq_ignore_ascii_case("ghostty") {
            return;
        }
        let Some(version) = self.env("TERM_PROGRAM_VERSION") else { return };
        if !ghostty_wide_grapheme_widths(version) {
            return;
        }
        if self.term_info.from_xtversion && !self.term_info.name.eq_ignore_ascii_case("ghostty") {
            return;
        }
        self.caps.unicode = WidthMethod::UnicodeWide;
    }

    fn apply_known_graphics_identity(&mut self) {
        if !self.graphics_enabled || self.multiplexer != Multiplexer::None || !self.term_info.from_xtversion {
            return;
        }
        let name = self.term_info.name.clone();
        let version = self.term_info.version.clone();
        if name.eq_ignore_ascii_case("kitty") || name.eq_ignore_ascii_case("ghostty") {
            self.caps.kitty_graphics = true;
        } else if name.eq_ignore_ascii_case("foot") && semantic_version_at_least(&version, 1, 2) {
            self.caps.sixel = true;
        } else if name.eq_ignore_ascii_case("wezterm") && wezterm_build_at_least(&version, 20200620) {
            self.caps.sixel = true;
        }
    }

    /// `checkEnvironmentOverrides`.
    pub fn check_environment_overrides(&mut self) {
        self.multiplexer = if self.is_xtversion_named("tmux", false) {
            Multiplexer::Tmux
        } else if self.is_xtversion_named("Zellij", true) {
            Multiplexer::Zellij
        } else {
            Multiplexer::None
        };
        self.is_foot = self.term_info.from_xtversion && find_ignore_case(&self.term_info.name, "foot");
        self.skip_graphics_query = false;
        self.graphics_enabled = true;
        self.skip_explicit_width_query = false;
        self.hyperlinks_override = None;
        // Bracketed paste is always tried, even where it was reported unsupported.
        self.caps.bracketed_paste = true;
        if self.caps.rgb {
            self.caps.ansi256 = true;
        }
        match self.remote_mode {
            RemoteMode::Remote => {
                self.remote = true;
                self.caps.remote = true;
            }
            RemoteMode::Local => {
                self.remote = false;
                self.caps.remote = false;
            }
            RemoteMode::Auto => {}
        }
        let Some(env) = self.env.clone() else { return };
        let get = |key: &str| env.get(key).map(String::as_str);
        if self.remote_mode == RemoteMode::Auto {
            self.remote = self.remote
                || get("SSH_CONNECTION").is_some()
                || get("SSH_CLIENT").is_some()
                || get("SSH_TTY").is_some()
                || get("MOSH_CONNECTION").is_some();
            self.caps.remote = self.remote;
        }
        // The environment is always the forwarded one here (`adoptHostEnvironment`).
        let forwarded = true;
        self.apply_known_unicode_width_identity();
        if let Some(term) = get("TERM") {
            if find_ignore_case(term, "256color") {
                self.caps.ansi256 = true;
            }
        }
        if let Some(colorterm) = get("COLORTERM") {
            if colorterm == "truecolor" || colorterm == "24bit" {
                self.caps.rgb = true;
                self.caps.ansi256 = true;
            }
        }
        if self.remote_mode == RemoteMode::Auto && self.remote && forwarded {
            return;
        }
        if !self.term_info.from_xtversion {
            if get("TMUX").is_some() {
                self.multiplexer = Multiplexer::Tmux;
                self.caps.unicode = WidthMethod::Wcwidth;
                self.caps.explicit_cursor_positioning = true;
            } else if get("ZELLIJ").is_some() || get("ZELLIJ_SESSION_NAME").is_some() || get("ZELLIJ_PANE_ID").is_some() {
                self.multiplexer = Multiplexer::Zellij;
                if self.term_info.name.is_empty() {
                    self.term_info.name = "Zellij".to_owned();
                }
            } else if get("STY").is_some() {
                self.multiplexer = Multiplexer::Screen;
                self.skip_graphics_query = true;
                self.caps.unicode = WidthMethod::Wcwidth;
                self.caps.explicit_cursor_positioning = true;
            } else if let Some(term) = get("TERM") {
                if term.starts_with("tmux") {
                    self.multiplexer = Multiplexer::Tmux;
                    self.caps.unicode = WidthMethod::Wcwidth;
                    self.caps.explicit_cursor_positioning = true;
                } else if term.starts_with("screen") {
                    self.multiplexer = Multiplexer::Screen;
                    self.skip_graphics_query = true;
                    self.caps.unicode = WidthMethod::Wcwidth;
                    self.caps.explicit_cursor_positioning = true;
                }
                if find(term, "alacritty") {
                    self.caps.explicit_cursor_positioning = true;
                }
            }
        }
        if let Some(term) = get("TERM") {
            self.apply_notification_heuristic(term);
            self.is_foot = self.is_foot || find_ignore_case(term, "foot");
        }
        if let Some(features) = get("TERM_FEATURES") {
            if term_features_has_code(features, "No") {
                self.set_notification_protocol(NotificationProtocol::Osc9, NotificationSource::Heuristic);
            }
        }
        if let Some(value) = get("OPENTUI_GRAPHICS") {
            if value == "false" || value == "0" {
                self.skip_graphics_query = true;
                self.graphics_enabled = false;
                self.kitty_graphics_queried = false;
                self.sixel_queried = false;
                self.caps.kitty_graphics = false;
                self.caps.sixel = false;
            } else if value == "true" || value == "1" {
                self.skip_graphics_query = false;
            }
        }
        if !self.term_info.from_xtversion {
            if let Some(program) = get("TERM_PROGRAM") {
                if !self.is_in_zellij() {
                    self.term_info.name = program.chars().take(64).collect();
                }
                if !self.is_in_zellij() && program == "tmux" {
                    self.multiplexer = Multiplexer::Tmux;
                    self.caps.unicode = WidthMethod::Wcwidth;
                    self.caps.explicit_cursor_positioning = true;
                }
                self.apply_notification_heuristic(program);
                if !self.is_in_zellij() {
                    if let Some(version) = get("TERM_PROGRAM_VERSION") {
                        self.term_info.version = version.chars().take(32).collect();
                    }
                }
                match program {
                    "vscode" => {
                        self.caps.kitty_keyboard = false;
                        self.caps.kitty_graphics = false;
                        self.caps.unicode = WidthMethod::Unicode;
                    }
                    "Apple_Terminal" => self.caps.unicode = WidthMethod::Wcwidth,
                    "Alacritty" => self.caps.explicit_cursor_positioning = true,
                    _ => {}
                }
            }
            if get("ALACRITTY_SOCKET").is_some() || get("ALACRITTY_LOG").is_some() {
                self.caps.explicit_cursor_positioning = true;
                if self.term_info.name.is_empty() {
                    self.term_info.name = "Alacritty".to_owned();
                }
            }
        }
        if get("WT_SESSION").is_some() {
            self.caps.rgb = true;
            self.caps.ansi256 = true;
            self.set_notification_protocol(NotificationProtocol::Osc777, NotificationSource::Heuristic);
        }
        if let Some(protocol) = get("OPENTUI_NOTIFICATION_PROTOCOL") {
            self.apply_notification_protocol_override(protocol);
        }
        if let Some(value) = get("OPENTUI_NOTIFICATIONS") {
            if value == "0" || value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("off") {
                self.apply_notification_protocol_override("none");
            }
        }
        self.enforce_notification_protocol_for_multiplexer();
        if !self.term_info.from_xtversion {
            if get("TERMUX_VERSION").is_some() {
                self.caps.unicode = WidthMethod::Wcwidth;
            }
            if get("VHS_RECORD").is_some() {
                self.caps.unicode = WidthMethod::Wcwidth;
                self.caps.kitty_keyboard = false;
                self.caps.kitty_graphics = false;
            }
        }
        self.apply_known_unicode_width_identity();
        if get("OPENTUI_FORCE_WCWIDTH").is_some() {
            self.caps.unicode = WidthMethod::Wcwidth;
        }
        if get("OPENTUI_FORCE_UNICODE").is_some() {
            self.caps.unicode = WidthMethod::Unicode;
        }
        if get("OPENTUI_FORCE_NOZWJ").is_some() {
            self.caps.unicode = WidthMethod::NoZwj;
        }
        if let Some(value) = get("OPENTUI_FORCE_EXPLICIT_WIDTH") {
            if value == "true" || value == "1" {
                self.caps.explicit_width = true;
            } else if value == "false" || value == "0" {
                self.caps.explicit_width = false;
                self.skip_explicit_width_query = true;
            }
        }
        if (self.is_foot || self.host_env_hyperlinks) && self.multiplexer != Multiplexer::None {
            self.caps.hyperlinks = false;
            self.host_env_hyperlinks = false;
        }
        if !self.caps.hyperlinks && self.term_info.from_xtversion && is_hyperlink_term(&self.term_info.name) {
            self.caps.hyperlinks = true;
        }
        if !self.caps.hyperlinks && !self.term_info.from_xtversion {
            if let Some(term) = get("TERM") {
                if is_hyperlink_term(term) && (!self.is_foot || self.multiplexer == Multiplexer::None) {
                    self.caps.hyperlinks = true;
                }
            }
        }
        if !self.caps.hyperlinks && !self.term_info.from_xtversion {
            let wsl = get("WSL_DISTRO_NAME").is_some() || get("WSL_INTEROP").is_some();
            if wsl && get("WT_SESSION").is_some() && get("TERM").is_some_and(|t| t.starts_with("xterm")) {
                self.caps.hyperlinks = true;
            }
        }
        if !self.caps.hyperlinks && !self.term_info.from_xtversion && self.multiplexer == Multiplexer::None {
            let host = matches!(get("TERM_PROGRAM"), Some("vscode" | "zed" | "mintty"))
                || get("VTE_VERSION").and_then(|v| v.parse::<u32>().ok()).is_some_and(|v| v >= 5202);
            if host {
                self.caps.hyperlinks = true;
                self.host_env_hyperlinks = true;
            }
        }
        if let Some(value) = get("FORCE_HYPERLINK") {
            self.hyperlinks_override = Some(!(value == "0" || value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("off")));
        }
        if let Some(enabled) = self.hyperlinks_override {
            self.caps.hyperlinks = enabled;
        }
        if !self.caps.osc52 && !self.term_info.from_xtversion {
            if get("WT_SESSION").is_some() {
                self.caps.osc52 = true;
            }
            if !self.caps.osc52 && (self.is_in_tmux() || self.is_in_screen() || get("STY").is_some()) {
                self.caps.osc52 = true;
            }
            if !self.caps.osc52 && get("TERM_PROGRAM").is_some_and(is_osc52_term) {
                self.caps.osc52 = true;
            }
            if !self.caps.osc52 && get("TERM").is_some_and(|t| is_osc52_term(t) || find(t, "256color") || find(t, "xterm")) {
                self.caps.osc52 = true;
            }
        }
    }

    // ---- mode writes ----

    /// `setMouseMode`.
    pub fn set_mouse_mode(&mut self, out: &mut Vec<u8>, enable: bool, movement: bool) {
        if enable {
            if self.state.mouse && self.state.mouse_movement == movement {
                return;
            }
        } else if !self.state.mouse {
            return;
        }
        if enable {
            self.state.mouse = true;
            self.state.mouse_movement = movement;
            self.state.mouse_was_enabled = true;
            if !movement {
                out.extend_from_slice(b"\x1b[?1003l");
            }
            out.extend_from_slice(b"\x1b[?1000h\x1b[?1002h");
            if movement {
                out.extend_from_slice(b"\x1b[?1003h");
            }
            out.extend_from_slice(b"\x1b[?1006h");
        } else {
            self.state.mouse = false;
            self.state.pixel_mouse = false;
            out.extend_from_slice(b"\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l");
        }
    }

    /// `forceDisableMouseMode`.
    pub fn force_disable_mouse_mode(&mut self, out: &mut Vec<u8>) {
        self.state.mouse = false;
        self.state.pixel_mouse = false;
        out.extend_from_slice(b"\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l");
    }

    pub fn set_bracketed_paste(&mut self, out: &mut Vec<u8>, enable: bool) {
        out.extend_from_slice(if enable { b"\x1b[?2004h" } else { b"\x1b[?2004l" });
        self.state.bracketed_paste = enable;
    }

    pub fn set_focus_tracking(&mut self, out: &mut Vec<u8>, enable: bool) {
        out.extend_from_slice(if enable { b"\x1b[?1004h" } else { b"\x1b[?1004l" });
        self.state.focus_tracking = enable;
    }

    /// `setKittyKeyboard`: push the flags (once) or pop them.
    pub fn set_kitty_keyboard(&mut self, out: &mut Vec<u8>, enable: bool, flags: u8) {
        if enable {
            if !self.state.kitty_keyboard {
                push_fmt(out, format_args!("\x1b[>{flags}u"));
                self.state.kitty_keyboard = true;
                self.state.kitty_keyboard_flags = flags;
            }
        } else if self.state.kitty_keyboard {
            out.extend_from_slice(b"\x1b[<u");
            self.state.kitty_keyboard = false;
            self.state.kitty_keyboard_flags = 0;
        }
    }

    pub fn set_modify_other_keys(&mut self, out: &mut Vec<u8>, enable: bool) {
        out.extend_from_slice(if enable { b"\x1b[>4;1m" } else { b"\x1b[>4;0m" });
        self.state.modify_other_keys = enable;
    }

    pub fn set_color_scheme_updates(&mut self, out: &mut Vec<u8>, enable: bool) {
        out.extend_from_slice(if enable { b"\x1b[?2031h" } else { b"\x1b[?2031l" });
        self.state.color_scheme_updates = enable;
    }

    /// `queryThemeColors`: OSC 10 and 11.
    pub fn query_theme_colors(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(OSC_THEME_QUERIES);
    }

    /// `restoreTerminalModes`: every mode that is on, sent again (after a focus-in that followed a blur).
    pub fn restore_terminal_modes(&mut self, out: &mut Vec<u8>) {
        if self.state.mouse {
            if !self.state.mouse_movement {
                out.extend_from_slice(b"\x1b[?1003l");
            }
            out.extend_from_slice(b"\x1b[?1000h\x1b[?1002h");
            if self.state.mouse_movement {
                out.extend_from_slice(b"\x1b[?1003h");
            }
            out.extend_from_slice(b"\x1b[?1006h");
        }
        if self.state.focus_tracking {
            out.extend_from_slice(b"\x1b[?1004h");
        }
        if self.state.bracketed_paste {
            out.extend_from_slice(b"\x1b[?2004h");
        }
        if self.state.kitty_keyboard {
            out.extend_from_slice(b"\x1b[<u");
            push_fmt(out, format_args!("\x1b[>{}u", self.state.kitty_keyboard_flags));
        }
        if self.state.modify_other_keys {
            out.extend_from_slice(b"\x1b[>4;1m");
        }
    }

    /// `queryTerminalSend`: the startup query batch.
    pub fn query_terminal_send(&mut self, out: &mut Vec<u8>) {
        self.check_environment_overrides();
        self.unicode_wide_locked = Some(self.caps.unicode == WidthMethod::UnicodeWide);
        self.startup_cursor_query_pending = true;
        self.startup_cursor_query_captured = false;
        self.set_color_scheme_updates(out, true);
        self.query_theme_colors(out);
        self.state.theme_queries_sent = true;
        out.extend_from_slice(XTVERSION);
        out.extend_from_slice(HIDE_CURSOR);
        out.extend_from_slice(SAVE_CURSOR);
        out.extend_from_slice(CURSOR_POSITION_REQUEST);
        out.extend_from_slice(CAPABILITY_QUERIES_BASE);
        out.extend_from_slice(CSI_U_QUERY);
        if !self.is_foot {
            out.extend_from_slice(NOTIFICATION_QUERIES);
            out.extend_from_slice(PROGRAM_STATUS_QUERY);
        }
        if !self.skip_graphics_query && !self.is_in_tmux() {
            out.extend_from_slice(KITTY_GRAPHICS_QUERY);
            if !self.reply_sentinel {
                out.extend_from_slice(PRIMARY_DEVICE_ATTRS);
            }
        }
        if !self.skip_explicit_width_query {
            self.explicit_width_probe_reports_pending = 2;
            out.extend_from_slice(HOME);
            out.extend_from_slice(EXPLICIT_WIDTH_QUERY);
            out.extend_from_slice(CURSOR_POSITION_REQUEST);
            out.extend_from_slice(HOME);
            out.extend_from_slice(SCALED_TEXT_QUERY);
            out.extend_from_slice(CURSOR_POSITION_REQUEST);
        } else {
            self.explicit_width_probe_reports_pending = 0;
        }
        out.extend_from_slice(RESTORE_CURSOR);
        if self.reply_sentinel {
            out.extend_from_slice(PRIMARY_DEVICE_ATTRS);
        }
    }

    /// `enableDetectedFeatures`: the modes the capabilities found allow.
    pub fn enable_detected_features(&mut self, out: &mut Vec<u8>, use_kitty_keyboard: bool) {
        self.check_environment_overrides();
        if !self.state.modify_other_keys && !self.state.kitty_keyboard {
            self.set_modify_other_keys(out, true);
        }
        if self.caps.kitty_keyboard && use_kitty_keyboard {
            if self.state.modify_other_keys {
                self.set_modify_other_keys(out, false);
            }
            let flags = self.kitty_keyboard_flags;
            self.set_kitty_keyboard(out, true, flags);
        }
        if matches!(self.caps.unicode, WidthMethod::Unicode | WidthMethod::UnicodeWide) && !self.caps.explicit_width {
            out.extend_from_slice(b"\x1b[?2027h");
        }
        if self.caps.bracketed_paste {
            self.set_bracketed_paste(out, true);
        }
        if self.caps.focus_tracking {
            self.set_focus_tracking(out, true);
        }
        if !self.state.color_scheme_updates {
            self.set_color_scheme_updates(out, true);
        }
        if !self.state.theme_queries_sent {
            self.query_theme_colors(out);
            self.state.theme_queries_sent = true;
        }
    }

    /// `resetInputModes`.
    pub fn reset_input_modes(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(SHOW_CURSOR);
        out.extend_from_slice(RESET);
        out.extend_from_slice(RESET_MOUSE_POINTER);
        self.state.mouse_pointer = MousePointerStyle::Default;
        if self.state.kitty_keyboard {
            self.set_kitty_keyboard(out, false, 0);
        }
        if self.state.modify_other_keys {
            self.set_modify_other_keys(out, false);
        }
        if self.state.mouse_was_enabled {
            self.force_disable_mouse_mode(out);
        }
        if self.state.bracketed_paste {
            self.set_bracketed_paste(out, false);
        }
        if self.state.focus_tracking {
            self.set_focus_tracking(out, false);
        }
    }

    /// `resetOutputModes`: color-scheme updates off and the title cleared.
    pub fn reset_output_modes(&mut self, out: &mut Vec<u8>) {
        if self.state.color_scheme_updates {
            self.set_color_scheme_updates(out, false);
        }
        out.extend_from_slice(b"\x1b]0;\x07");
    }

    pub fn enter_alt_screen(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(ALT_SCREEN_ON);
        self.state.alt_screen = true;
    }

    pub fn exit_alt_screen(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(ALT_SCREEN_OFF);
        self.state.alt_screen = false;
    }

    // ---- replies ----

    fn parse_kitty_graphics_response(&mut self, response: &str) {
        if !self.graphics_enabled {
            return;
        }
        let mut offset = 0;
        while let Some(start) = response[offset..].find("\x1b_G").map(|i| i + offset) {
            let Some(end) = response[start + 3..].find("\x1b\\").map(|i| i + start + 3) else { return };
            let frame = &response[start + 3..end];
            let control_end = frame.find(';').unwrap_or(frame.len());
            if frame[..control_end].split(',').any(|field| field == "i=31337") {
                self.kitty_graphics_queried = true;
                self.caps.kitty_graphics = true;
                return;
            }
            offset = end + 2;
        }
    }

    /// Whether the response holds a DA1 reply (`parseSixelDeviceAttributes`).
    fn parse_sixel_device_attributes(&mut self, response: &str) -> bool {
        let graphics = self.graphics_enabled && !self.skip_graphics_query;
        let bytes = response.as_bytes();
        let mut found = false;
        let mut offset = 0;
        while let Some(start) = response[offset..].find("\x1b[?").map(|i| i + offset) {
            let mut end = start + 3;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
                end += 1;
            }
            if end >= bytes.len() {
                return found;
            }
            offset = end + 1;
            if bytes[end] != b'c' {
                continue;
            }
            found = true;
            if !graphics {
                continue;
            }
            if response[start + 3..end].split(';').any(|param| param == "4") {
                self.sixel_queried = true;
                self.caps.sixel = true;
            }
        }
        found
    }

    fn parse_xtgettcap_ms(&mut self, response: &str) {
        let mut scan = 0;
        while let Some(start) = response[scan..].find("\x1bP").map(|i| i + scan) {
            let body_start = start + 2;
            let Some(end) = response[body_start..].find("\x1b\\").map(|i| i + body_start) else { return };
            let body = &response[body_start..end];
            scan = end + 2;
            if body.len() < 6 || !body.starts_with('1') || &body[1..3] != "+r" {
                continue;
            }
            let result = &body[3..];
            let Some(separator) = result.find('=') else { continue };
            if !result[..separator].eq_ignore_ascii_case("4d73") {
                continue;
            }
            let value = &result[separator + 1..];
            if value.is_empty() || value.len() % 2 != 0 {
                continue;
            }
            if value.bytes().all(|b| b.is_ascii_hexdigit()) {
                self.osc52_support = Osc52Support::Supported;
                self.caps.osc52 = true;
            }
        }
    }

    fn parse_iterm_capabilities(&mut self, response: &str) {
        let prefix = "\x1b]1337;Capabilities=";
        let mut search = 0;
        while let Some(rel) = response[search..].find(prefix) {
            let start = search + rel + prefix.len();
            let end = start + find_osc_terminator(&response[start..]);
            if term_features_has_code(&response[start..end], "No") {
                self.set_notification_protocol(NotificationProtocol::Osc9, NotificationSource::Query);
                return;
            }
            search = end;
        }
    }

    fn parse_osc99_notification_query(&mut self, response: &str) {
        let prefix = "\x1b]99;";
        let mut search = 0;
        while let Some(rel) = response[search..].find(prefix) {
            let payload_start = search + rel + prefix.len();
            let end = payload_start + find_osc_terminator(&response[payload_start..]);
            let payload = &response[payload_start..end];
            if payload.contains(&format!("i={NOTIFICATION_QUERY_ID}")) && payload.contains("p=?") && payload.contains("p=") && payload.contains("title") {
                self.set_notification_protocol(NotificationProtocol::Osc99, NotificationSource::Query);
                return;
            }
            search = end;
        }
    }

    fn parse_program_status_reply(&mut self, response: &str) {
        let prefix = "\x1b]7501;";
        let Some(start) = response.find(prefix).map(|i| i + prefix.len()) else { return };
        let payload = &response[start..start + find_osc_terminator(&response[start..])];
        let first = &payload[..payload.find(':').unwrap_or(payload.len())];
        if first.trim_matches(' ') == "?" {
            self.caps.program_status = true;
        }
    }

    fn parse_xtversion(&mut self, term: &str) {
        if term.is_empty() {
            return;
        }
        self.caps.kitty_graphics = self.kitty_graphics_queried;
        self.caps.sixel = self.sixel_queried;
        let truncate = |s: &str, n: usize| -> String {
            let mut end = s.len().min(n);
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s[..end].to_owned()
        };
        if let Some(paren) = term.find('(') {
            self.term_info.name = truncate(&term[..paren], 64);
            self.term_info.version = match term[paren..].find(')') {
                Some(close) => truncate(&term[paren + 1..paren + close], 32),
                None => String::new(),
            };
        } else if let Some(space) = term.find(' ') {
            self.term_info.name = truncate(&term[..space], 64);
            self.term_info.version = truncate(&term[space + 1..], 32);
        } else {
            self.term_info.name = truncate(term, 64);
            self.term_info.version = String::new();
        }
        self.term_info.from_xtversion = true;
        self.is_foot = find_ignore_case(&self.term_info.name, "foot");
        self.multiplexer = if self.term_info.name == "tmux" {
            Multiplexer::Tmux
        } else if self.term_info.name.eq_ignore_ascii_case("Zellij") {
            Multiplexer::Zellij
        } else {
            Multiplexer::None
        };
        if !self.is_in_zellij() {
            if let Some(protocol) = detect_notification_protocol(&self.term_info.name.clone()) {
                self.set_notification_protocol(protocol, NotificationSource::Xtversion);
            }
        }
        self.enforce_notification_protocol_for_multiplexer();
        self.apply_known_graphics_identity();
    }

    /// `processCapabilityResponse`: what one reply tells about the terminal.
    pub fn process_capability_response(&mut self, response: &str) {
        self.parse_osc99_notification_query(response);
        self.parse_iterm_capabilities(response);
        self.parse_program_status_reply(response);
        self.parse_xtgettcap_ms(response);
        self.parse_kitty_graphics_response(response);
        let device_attributes = self.parse_sixel_device_attributes(response);

        if find(response, "1016;2$y") {
            self.caps.sgr_pixels = true;
        }
        if find(response, "2027;2$y") {
            self.caps.unicode = WidthMethod::Unicode;
        }
        if find(response, "2031;1$y") || find(response, "2031;2$y") {
            self.caps.color_scheme_updates = true;
        }
        if find(response, "1004;1$y") || find(response, "1004;2$y") {
            self.caps.focus_tracking = true;
        }
        if find(response, "2026;1$y") || find(response, "2026;2$y") {
            self.caps.sync = true;
        }
        if find(response, "2004;1$y") || find(response, "2004;2$y") {
            self.caps.bracketed_paste = true;
        }

        // Cursor position reports: the first after the query batch is the pre-home position, then the probes.
        let bytes = response.as_bytes();
        let mut scan = 0;
        while scan < bytes.len() {
            let Some(esc) = response[scan..].find("\x1b[").map(|i| i + scan) else { break };
            let mut pos = esc + 2;
            let row_start = pos;
            while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                pos += 1;
            }
            if pos == row_start || pos >= bytes.len() || bytes[pos] != b';' {
                scan = esc + 2;
                continue;
            }
            let Ok(row) = response[row_start..pos].parse::<u16>() else {
                scan = pos + 1;
                continue;
            };
            pos += 1;
            let col_start = pos;
            while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                pos += 1;
            }
            if pos == col_start || pos >= bytes.len() || bytes[pos] != b'R' {
                scan = col_start;
                continue;
            }
            let Ok(col) = response[col_start..pos].parse::<u16>() else {
                scan = pos + 1;
                continue;
            };
            if self.startup_cursor_query_pending && !self.startup_cursor_query_captured && row >= 1 && col >= 1 {
                let visible = self.state.cursor_visible;
                self.set_cursor_position(col as u32, row as u32, visible);
                self.startup_cursor_query_captured = true;
                self.startup_cursor_query_pending = false;
            } else if self.explicit_width_probe_reports_pending > 0 {
                let probe_index = 2 - self.explicit_width_probe_reports_pending;
                self.explicit_width_probe_reports_pending -= 1;
                self.sync_reply_settled = true;
                if row == 1 && probe_index == 0 && col == 2 {
                    self.caps.explicit_width = true;
                } else if row == 1 && probe_index == 1 && col == 3 {
                    self.caps.explicit_width = true;
                    self.caps.scaled_text = true;
                }
            }
            scan = pos + 1;
        }
        if device_attributes {
            self.sync_reply_settled = true;
        }
        if device_attributes && self.reply_sentinel {
            self.startup_cursor_query_pending = false;
            self.explicit_width_probe_reports_pending = 0;
        }

        if let Some(pos) = response.find("\x1bP>|") {
            let start = pos + 4;
            if let Some(end) = response[start..].find("\x1b\\") {
                let term = response[start..start + end].to_owned();
                self.parse_xtversion(&term);
            }
        }
        if self.is_xtversion_named("kitty", true) {
            self.caps.kitty_keyboard = true;
            self.caps.unicode = WidthMethod::Unicode;
            self.caps.rgb = true;
            self.caps.ansi256 = true;
            self.caps.bracketed_paste = true;
            self.caps.hyperlinks = true;
        }
        // The kitty keyboard query reply: CSI ? n u.
        if find(response, "\x1b[?") && find(response, "u") {
            let mut i = 0;
            while i + 4 < bytes.len() {
                if bytes[i] == 0x1b && bytes[i + 1] == b'[' && bytes[i + 2] == b'?' {
                    let mut end = i + 3;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                    if end > i + 3 && end < bytes.len() && bytes[end] == b'u' {
                        self.caps.kitty_keyboard = true;
                        break;
                    }
                }
                i += 1;
            }
        }
        if find(response, "tmux") {
            self.caps.unicode = WidthMethod::Wcwidth;
            self.caps.explicit_cursor_positioning = true;
        }
        if find(response, "alacritty") {
            self.caps.explicit_cursor_positioning = true;
        }
        if !self.caps.osc52 && is_osc52_term(response) {
            self.caps.osc52 = true;
        }
        if !self.caps.hyperlinks && is_hyperlink_term(response) {
            self.caps.hyperlinks = true;
        }
        if let Some(enabled) = self.hyperlinks_override {
            self.caps.hyperlinks = enabled;
        }
        if self.is_in_tmux() {
            self.caps.focus_tracking = true;
            self.kitty_graphics_queried = false;
            self.caps.kitty_graphics = false;
            self.sixel_queried = false;
            self.caps.sixel = false;
        }
    }

    // ---- notifications and clipboard ----

    fn write_passthrough(&self, out: &mut Vec<u8>, sequence: &[u8]) {
        let wrap = |out: &mut Vec<u8>, start: &[u8]| {
            out.extend_from_slice(start);
            for &c in sequence {
                if c == 0x1b {
                    out.push(0x1b);
                }
                out.push(c);
            }
            out.extend_from_slice(b"\x1b\\");
        };
        if self.is_in_tmux() {
            wrap(out, b"\x1bPtmux;");
            return;
        }
        if !self.remote && self.env("STY").is_some() {
            wrap(out, b"\x1bP");
            return;
        }
        out.extend_from_slice(sequence);
    }

    /// `writeNotification`: a desktop notification in the terminal's protocol; false when it has none.
    pub fn write_notification(&mut self, out: &mut Vec<u8>, message: &str, title: Option<&str>) -> bool {
        if !self.caps.notifications || self.notification_protocol == NotificationProtocol::None {
            return false;
        }
        self.notification_id_counter = self.notification_id_counter.wrapping_add(1);
        let sanitize = |buffer: &mut Vec<u8>, text: &str, semicolon: bool| {
            for &c in text.as_bytes() {
                buffer.push(if c < 0x20 || c == 0x7f || c == 0x1b || (semicolon && c == b';') { b' ' } else { c });
            }
        };
        let mut buffer = Vec::new();
        match self.notification_protocol {
            NotificationProtocol::None => return false,
            NotificationProtocol::Osc99 => {
                let id = format!("opentui-{}", self.notification_id_counter);
                let payload = |buffer: &mut Vec<u8>, kind: &str, text: &str, done: bool| {
                    push_fmt(buffer, format_args!("\x1b]99;i={id}:p={kind}:e=1:d={};", done as u8));
                    buffer.extend_from_slice(base64(text.as_bytes()).as_bytes());
                    buffer.extend_from_slice(b"\x1b\\");
                };
                match title {
                    Some(title) if !title.is_empty() => {
                        payload(&mut buffer, "title", title, false);
                        payload(&mut buffer, "body", message, true);
                    }
                    _ => payload(&mut buffer, "body", message, true),
                }
            }
            NotificationProtocol::Osc777 => {
                buffer.extend_from_slice(b"\x1b]777;notify;");
                match title {
                    Some(title) => {
                        sanitize(&mut buffer, title, true);
                        buffer.push(b';');
                        sanitize(&mut buffer, message, true);
                    }
                    None => {
                        sanitize(&mut buffer, message, true);
                        buffer.push(b';');
                    }
                }
                buffer.extend_from_slice(b"\x1b\\");
            }
            NotificationProtocol::Osc9 => {
                buffer.extend_from_slice(b"\x1b]9;");
                if let Some(title) = title.filter(|t| !t.is_empty()) {
                    sanitize(&mut buffer, title, false);
                    buffer.extend_from_slice(b": ");
                }
                sanitize(&mut buffer, message, false);
                buffer.extend_from_slice(b"\x1b\\");
            }
        }
        self.write_passthrough(out, &buffer);
        true
    }

    /// `writeClipboard`: OSC 52 (wrapped for tmux and screen); false when OSC 52 is known unsupported.
    pub fn write_clipboard(&self, out: &mut Vec<u8>, target: ClipboardTarget, text: &[u8]) -> bool {
        if self.osc52_support == Osc52Support::Unsupported {
            return false;
        }
        let encoded = base64(text);
        if self.is_in_tmux() {
            out.extend_from_slice(b"\x1bPtmux;\x1b\x1b]52;");
            out.push(target.char());
            out.push(b';');
            out.extend_from_slice(encoded.as_bytes());
            out.extend_from_slice(b"\x1b\x1b\\\x1b\\");
            return true;
        }
        if self.is_in_screen() {
            let mut sequence = b"\x1b]52;".to_vec();
            sequence.push(target.char());
            sequence.push(b';');
            sequence.extend_from_slice(encoded.as_bytes());
            sequence.push(0x07);
            for chunk in sequence.chunks(252) {
                out.extend_from_slice(b"\x1bP");
                out.extend_from_slice(chunk);
                out.extend_from_slice(b"\x1b\\");
            }
            return true;
        }
        out.extend_from_slice(b"\x1b]52;");
        out.push(target.char());
        out.push(b';');
        out.extend_from_slice(encoded.as_bytes());
        out.extend_from_slice(b"\x1b\\");
        true
    }
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

// ---- session.zig ----

/// `TerminalPhase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TerminalPhase {
    #[default]
    Uninitialized,
    SettingUp,
    Active,
    Suspending,
    Suspended,
    Resuming,
    Closing,
    Restored,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
enum Step {
    #[default]
    Idle,
    Query,
    SetupScreen,
    Enable,
    Activate,
    ResetInput,
    ResetOutput,
    SettleFirst,
    ShowCursor,
    SettleSecond,
}

/// The options of [`Session::setup_terminal`] (`TerminalOptions`, from the renderer's config).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalOptions {
    pub use_alternate_screen: bool,
    pub mouse: bool,
    pub mouse_movement: bool,
    pub kitty_keyboard_flags: u8,
    pub clear_on_close: bool,
    pub reply_sentinel: bool,
    pub synchronized_output: SynchronizedOutput,
    pub host_multiplexer: bool,
}

impl Default for TerminalOptions {
    /// What the TUI's renderer passes (`useKittyKeyboard: {}` is flags 5, mouse on with movement).
    fn default() -> Self {
        TerminalOptions {
            use_alternate_screen: true,
            mouse: true,
            mouse_movement: true,
            kitty_keyboard_flags: 0b00101,
            clear_on_close: true,
            reply_sentinel: false,
            synchronized_output: SynchronizedOutput::Auto,
            host_multiplexer: false,
        }
    }
}

/// A [`Session`] call the lifecycle does not allow now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    /// `InvalidTerminalState`.
    InvalidState,
    /// `TerminalInactive`: the terminal is not active (or a reply arrived before the query batch was sent).
    Inactive,
    /// `InvalidOptions`: a malformed control (a capability reply that is not one, a bad title, flags over 31).
    InvalidOptions,
}

/// What [`Session::pump`] wants next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pump {
    /// Nothing to do until another call changes the lifecycle.
    Idle,
    /// Bytes were appended: write them, then pump again.
    Again,
    /// Pump again at this time (ms on the caller's clock), after the bytes so far are written.
    WaitUntil(u64),
    /// The terminal is restored; nothing more will be written.
    Closed,
}

/// The pointer and cursor parts of a cursor control (`Control.cursor`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CursorControl {
    /// (x, y, visible), 1-based.
    pub position: Option<(i32, i32, bool)>,
    pub style: Option<CursorStyle>,
    pub blinking: Option<bool>,
    pub color: Option<RGBA>,
    pub pointer: Option<MousePointerStyle>,
}

/// A mouse mode (`"disabled" | "drag" | "motion"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseMode {
    Disabled,
    Drag,
    Motion,
}

/// `ProgramStatus` state (OSC 7501).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramState {
    Idle,
    Busy,
    Blocked,
    Done,
    Error,
}

impl ProgramState {
    fn name(self) -> &'static str {
        match self {
            ProgramState::Idle => "idle",
            ProgramState::Busy => "busy",
            ProgramState::Blocked => "blocked",
            ProgramState::Done => "done",
            ProgramState::Error => "error",
        }
    }
}

/// A program status report (`ProgramStatus`).
#[derive(Clone, Debug, PartialEq)]
pub struct ProgramStatus {
    pub state: ProgramState,
    pub id: String,
    pub kind: Option<String>,
    pub progress: Option<u8>,
    pub app: String,
    pub title: String,
    pub message: String,
}

/// `Control`: what the renderer asks of an active terminal.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    /// One or more replies, as they arrived.
    CapabilityResponse(Vec<u8>),
    Title(String),
    Mouse(MouseMode),
    KittyKeyboardFlags(u8),
    RestoreModes,
    QueryPixelResolution { sentinel: bool },
    QueryThemeColors,
    PaletteQuery(Vec<u8>),
    ResetBackground,
    Cursor(CursorControl),
    ProgramStatus(ProgramStatus),
}

/// The terminal lifecycle (`Session` in session.zig): setup, suspend, resume and close as packets.
#[derive(Clone, Debug, Default)]
pub struct Session {
    phase: TerminalPhase,
    step: Step,
    mouse: bool,
    mouse_movement: bool,
    screen_claimed: bool,
    closing: bool,
    deadline: Option<u64>,
    use_alternate_screen: bool,
    clear_on_close: bool,
}

/// `cursor_settle_ns`, in ms.
const CURSOR_SETTLE_MS: u64 = 10;

impl Session {
    pub fn new() -> Self {
        Session { mouse_movement: true, use_alternate_screen: true, clear_on_close: true, ..Default::default() }
    }

    pub fn phase(&self) -> TerminalPhase {
        self.phase
    }

    /// `setupTerminal`: accepts setup; [`Self::pump`] writes the query batch, the screen switch and the modes.
    pub fn setup_terminal(&mut self, renderer: &mut Renderer, options: TerminalOptions) -> Result<(), SessionError> {
        if self.phase != TerminalPhase::Uninitialized || self.closing {
            return Err(SessionError::InvalidState);
        }
        if options.kitty_keyboard_flags & !0b11111 != 0 {
            return Err(SessionError::InvalidOptions);
        }
        self.use_alternate_screen = options.use_alternate_screen;
        self.clear_on_close = options.clear_on_close;
        let terminal = &mut renderer.terminal;
        terminal.reply_sentinel = options.reply_sentinel;
        terminal.synchronized = options.synchronized_output;
        terminal.multiplexer_on_host = options.host_multiplexer;
        terminal.kitty_keyboard_flags = options.kitty_keyboard_flags;
        self.phase = TerminalPhase::SettingUp;
        self.step = Step::Query;
        self.mouse = options.mouse;
        self.mouse_movement = options.mouse_movement;
        Ok(())
    }

    /// `suspendTerminal`: the input and output modes reset and the main screen back (the same packets as close).
    pub fn suspend_terminal(&mut self) -> Result<(), SessionError> {
        match self.phase {
            TerminalPhase::Suspending | TerminalPhase::Suspended => Ok(()),
            TerminalPhase::Active => {
                self.phase = TerminalPhase::Suspending;
                self.step = Step::ResetInput;
                Ok(())
            }
            _ => Err(SessionError::InvalidState),
        }
    }

    /// `resumeTerminal`: the screen and modes again (no queries).
    pub fn resume_terminal(&mut self, renderer: &mut Renderer) -> Result<(), SessionError> {
        if self.phase != TerminalPhase::Suspended {
            return Err(SessionError::InvalidState);
        }
        renderer.terminal.set_cursor_position(1, 1, false);
        self.phase = TerminalPhase::Resuming;
        self.step = Step::SetupScreen;
        Ok(())
    }

    /// `beginClose`: restores the terminal through [`Self::pump`]; a terminal never set up closes at once.
    pub fn begin_close(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        match self.phase {
            TerminalPhase::Uninitialized | TerminalPhase::Suspended => {
                self.phase = TerminalPhase::Restored;
                self.step = Step::Idle;
            }
            TerminalPhase::Active | TerminalPhase::SettingUp | TerminalPhase::Resuming => {
                self.step = Step::ResetInput;
                self.phase = TerminalPhase::Closing;
            }
            TerminalPhase::Suspending => self.phase = TerminalPhase::Closing,
            TerminalPhase::Closing | TerminalPhase::Restored => {}
        }
    }

    /// `pump`: the next packet (call again after it is written), a wait, or nothing. `now` is a monotonic ms clock.
    pub fn pump(&mut self, renderer: &mut Renderer, now: u64, out: &mut Vec<u8>) -> Pump {
        loop {
            match self.step {
                Step::Idle => {
                    return if self.closing && self.phase == TerminalPhase::Restored { Pump::Closed } else { Pump::Idle };
                }
                Step::Activate => {
                    renderer.invalidate_terminal_state();
                    self.phase = TerminalPhase::Active;
                    self.step = Step::Idle;
                    return Pump::Idle;
                }
                Step::SettleFirst | Step::SettleSecond => {
                    let deadline = *self.deadline.get_or_insert(now + CURSOR_SETTLE_MS);
                    if now < deadline {
                        return Pump::WaitUntil(deadline);
                    }
                    self.deadline = None;
                    if self.step == Step::SettleFirst {
                        self.step = Step::ShowCursor;
                        continue;
                    }
                    renderer.invalidate_terminal_state();
                    self.step = Step::Idle;
                    self.screen_claimed = false;
                    self.phase = if self.closing { TerminalPhase::Restored } else { TerminalPhase::Suspended };
                    return if self.closing { Pump::Closed } else { Pump::Idle };
                }
                _ => {
                    self.write_terminal_packet(renderer, out);
                    renderer.sync_width_method();
                    return Pump::Again;
                }
            }
        }
    }

    /// `writeTerminalPacket`.
    fn write_terminal_packet(&mut self, renderer: &mut Renderer, out: &mut Vec<u8>) {
        let terminal = &mut renderer.terminal;
        match self.step {
            Step::Query => {
                // Initialized before replies can supply the startup cursor position.
                terminal.set_cursor_position(1, 1, false);
                terminal.query_terminal_send(out);
                self.step = Step::SetupScreen;
            }
            Step::SetupScreen => {
                out.extend_from_slice(SAVE_CURSOR);
                if self.use_alternate_screen {
                    terminal.enter_alt_screen(out);
                }
                self.step = Step::Enable;
            }
            Step::Enable => {
                out.extend_from_slice(HIDE_CURSOR);
                let kitty = terminal.kitty_keyboard_flags != 0;
                terminal.enable_detected_features(out, kitty);
                terminal.set_mouse_mode(out, self.mouse, self.mouse_movement);
                self.screen_claimed = true;
                self.step = Step::Activate;
            }
            Step::ResetInput => {
                terminal.reset_input_modes(out);
                self.step = Step::ResetOutput;
                if terminal.state.alt_screen {
                    terminal.exit_alt_screen(out);
                }
            }
            Step::ResetOutput => {
                terminal.reset_output_modes(out);
                if self.screen_claimed && !self.use_alternate_screen && self.clear_on_close {
                    out.extend_from_slice(b"\x1b[H\x1b[J");
                }
                out.extend_from_slice(b"\x1b]12;default\x07\x1b]112\x07\x1b[0 q\x1b[?25h");
                self.step = Step::SettleFirst;
            }
            Step::ShowCursor => {
                out.extend_from_slice(SHOW_CURSOR);
                self.step = Step::SettleSecond;
            }
            Step::Idle | Step::Activate | Step::SettleFirst | Step::SettleSecond => unreachable!(),
        }
    }

    /// `control`: a control packet for an active terminal (a capability reply is also accepted while setting
    /// up, once the queries are out). Cursor controls apply in any phase and write nothing.
    pub fn control(&mut self, renderer: &mut Renderer, command: Control, out: &mut Vec<u8>) -> Result<(), SessionError> {
        if let Control::Cursor(cursor) = &command {
            let terminal = &mut renderer.terminal;
            if let Some((x, y, visible)) = cursor.position {
                let max = u16::MAX as i32 + 1;
                if x > max || y > max {
                    return Err(SessionError::InvalidOptions);
                }
                terminal.set_cursor_position(x.max(1) as u32, y.max(1) as u32, visible);
            }
            let current = terminal.cursor_style();
            terminal.set_cursor_style(cursor.style.unwrap_or(current.style), cursor.blinking.unwrap_or(current.blinking));
            if let Some(color) = cursor.color {
                terminal.set_cursor_color(color);
            }
            if let Some(pointer) = cursor.pointer {
                terminal.set_mouse_pointer_style(pointer);
            }
            return Ok(());
        }
        match &command {
            Control::Title(title) if !valid_text(title) => return Err(SessionError::InvalidOptions),
            Control::KittyKeyboardFlags(flags) if flags & !0b11111 != 0 => return Err(SessionError::InvalidOptions),
            _ => {}
        }
        match self.phase {
            TerminalPhase::Active => {}
            TerminalPhase::SettingUp | TerminalPhase::Resuming => {
                if !matches!(command, Control::CapabilityResponse(_)) || self.step == Step::Query {
                    return Err(SessionError::Inactive);
                }
            }
            _ => return Err(SessionError::Inactive),
        }
        let start = out.len();
        let mut candidate = renderer.terminal.clone();
        let result = self.write_control_packet(&command, &mut candidate, out);
        if result.is_err() {
            out.truncate(start);
            return result;
        }
        renderer.terminal = candidate;
        renderer.sync_width_method();
        if matches!(command, Control::CapabilityResponse(_)) {
            renderer.invalidate_terminal_state();
        }
        Ok(())
    }

    fn write_control_packet(&mut self, command: &Control, terminal: &mut Terminal, out: &mut Vec<u8>) -> Result<(), SessionError> {
        match command {
            Control::CapabilityResponse(response) => {
                apply_capability_responses(terminal, response)?;
                // Setup enables modes after choosing the screen: kitty's keyboard stack belongs to that screen.
                if self.phase == TerminalPhase::Active || self.step == Step::Activate {
                    let kitty = terminal.kitty_keyboard_flags != 0;
                    terminal.enable_detected_features(out, kitty);
                } else {
                    terminal.check_environment_overrides();
                }
            }
            Control::Title(title) => push_fmt(out, format_args!("\x1b]0;{title}\x07")),
            Control::Mouse(mode) => {
                let enabled = *mode != MouseMode::Disabled;
                let movement = if enabled { *mode == MouseMode::Motion } else { self.mouse_movement };
                terminal.set_mouse_mode(out, enabled, movement);
                self.mouse = enabled;
                self.mouse_movement = movement;
            }
            Control::KittyKeyboardFlags(flags) => {
                if terminal.state.kitty_keyboard && terminal.state.kitty_keyboard_flags != *flags {
                    terminal.set_kitty_keyboard(out, false, 0);
                }
                terminal.set_kitty_keyboard_flags(*flags);
                terminal.enable_detected_features(out, *flags != 0);
            }
            Control::RestoreModes => terminal.restore_terminal_modes(out),
            Control::QueryPixelResolution { sentinel } => {
                out.extend_from_slice(QUERY_PIXEL_SIZE);
                if *sentinel {
                    out.extend_from_slice(PRIMARY_DEVICE_ATTRS);
                }
            }
            Control::QueryThemeColors => terminal.query_theme_colors(out),
            Control::PaletteQuery(bytes) => out.extend_from_slice(bytes),
            Control::ResetBackground => out.extend_from_slice(RESET_TERMINAL_BG),
            Control::Cursor(_) => unreachable!(),
            Control::ProgramStatus(status) => {
                push_fmt(out, format_args!("\x1b]7501;state={}", status.state.name()));
                if !status.id.is_empty() {
                    push_fmt(out, format_args!(":id={}", status.id));
                }
                if let Some(kind) = &status.kind {
                    push_fmt(out, format_args!(":kind={kind}"));
                }
                if let Some(progress) = status.progress {
                    push_fmt(out, format_args!(":progress={progress}"));
                }
                if !status.app.is_empty() {
                    push_fmt(out, format_args!(":app={}", status.app));
                }
                if !status.title.is_empty() {
                    push_fmt(out, format_args!(":title={}", base64(status.title.as_bytes())));
                }
                if !status.message.is_empty() {
                    push_fmt(out, format_args!(":msg={}", base64(status.message.as_bytes())));
                }
                out.extend_from_slice(b"\x1b\\");
            }
        }
        Ok(())
    }

    /// `triggerNotification`.
    pub fn trigger_notification(&mut self, renderer: &mut Renderer, message: &str, title: Option<&str>, out: &mut Vec<u8>) -> bool {
        if self.phase != TerminalPhase::Active {
            return false;
        }
        renderer.terminal.write_notification(out, message, title)
    }

    /// `writeClipboard`.
    pub fn write_clipboard(&mut self, renderer: &Renderer, target: ClipboardTarget, bytes: &[u8], out: &mut Vec<u8>) -> bool {
        if self.phase != TerminalPhase::Active {
            return false;
        }
        renderer.terminal.write_clipboard(out, target, bytes)
    }

    /// `setPaletteState` (active terminals only).
    pub fn set_palette_state(&mut self, renderer: &mut Renderer, palette: &[RGBA], default_fg: RGBA, default_bg: RGBA, epoch: u32) -> Result<(), SessionError> {
        if self.phase != TerminalPhase::Active {
            return Err(SessionError::Inactive);
        }
        renderer.set_palette_state(palette, default_fg, default_bg, epoch);
        Ok(())
    }
}

/// UTF-8 without C0, DEL or C1 controls (`validateText`).
fn valid_text(text: &str) -> bool {
    text.chars().all(|c| !(c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c)))
}

/// `applyCapabilityResponses`: each reply in the bytes, validated, in wire order.
fn apply_capability_responses(terminal: &mut Terminal, response: &[u8]) -> Result<(), SessionError> {
    if response.is_empty() || response.len() > 4096 {
        return Err(SessionError::InvalidOptions);
    }
    let mut offset = 0;
    let mut pieces = Vec::new();
    while offset < response.len() {
        let bytes = &response[offset..];
        if bytes.len() < 3 || bytes[0] != 0x1b {
            return Err(SessionError::InvalidOptions);
        }
        let mut end = 2;
        match bytes[1] {
            b'[' => {
                while end < bytes.len() && (0x30..=0x3f).contains(&bytes[end]) {
                    end += 1;
                }
                while end < bytes.len() && (0x20..=0x2f).contains(&bytes[end]) {
                    end += 1;
                }
                if end == bytes.len() {
                    return Err(SessionError::InvalidOptions);
                }
                end += 1;
                validate_capability_csi(&bytes[2..end])?;
            }
            b'P' | b']' | b'_' => {
                while end < bytes.len() && (0x20..=0x7e).contains(&bytes[end]) {
                    end += 1;
                }
                let payload = &bytes[2..end];
                if bytes[1] == b']' && end < bytes.len() && bytes[end] == 0x07 {
                    end += 1;
                } else if bytes[end..].starts_with(b"\x1b\\") {
                    end += 2;
                } else {
                    return Err(SessionError::InvalidOptions);
                }
                validate_capability_string(bytes[1], payload)?;
            }
            _ => return Err(SessionError::InvalidOptions),
        }
        pieces.push(&response[offset..offset + end]);
        offset += end;
    }
    for piece in pieces {
        // Every piece was checked to be printable ASCII and escapes.
        terminal.process_capability_response(std::str::from_utf8(piece).unwrap_or_default());
    }
    Ok(())
}

fn validate_capability_string(kind: u8, payload: &[u8]) -> Result<(), SessionError> {
    let bad = Err(SessionError::InvalidOptions);
    match kind {
        b'P' => {
            if payload.starts_with(b">|") {
                return if payload.len() == 2 { bad } else { Ok(()) };
            }
            if payload == b"0+r" {
                return Ok(());
            }
            if !payload.starts_with(b"0+r") && !payload.starts_with(b"1+r") {
                return bad;
            }
            for (count, field) in payload[3..].split(|&b| b == b'=').enumerate() {
                if count == 2 || field.len() % 2 != 0 || (count == 0 && field.is_empty()) || !field.iter().all(u8::is_ascii_hexdigit) {
                    return bad;
                }
            }
            Ok(())
        }
        b']' => {
            if payload.starts_with(b"99;") || payload.starts_with(b"1337;Capabilities=") || payload.starts_with(b"7501;") {
                Ok(())
            } else {
                bad
            }
        }
        _ => {
            if !payload.starts_with(b"G") {
                return bad;
            }
            let Some(separator) = payload.iter().position(|&b| b == b';') else { return bad };
            if separator == 1 || separator + 1 == payload.len() {
                return bad;
            }
            for field in payload[1..separator].split(|&b| b == b',') {
                if field.len() < 3 || !field[0].is_ascii_alphabetic() || field[1] != b'=' || !field[2..].iter().all(u8::is_ascii_digit) {
                    return bad;
                }
                if std::str::from_utf8(&field[2..]).ok().and_then(|s| s.parse::<u32>().ok()).is_none() {
                    return bad;
                }
            }
            Ok(())
        }
    }
}

fn validate_capability_csi(payload: &[u8]) -> Result<(), SessionError> {
    let bad = Err(SessionError::InvalidOptions);
    let Some((&last, params)) = payload.split_last() else { return bad };
    let mut params = params;
    match last {
        b'y' | b'u' | b'c' => {
            if !params.starts_with(b"?") {
                return bad;
            }
            params = &params[1..];
        }
        b'R' => {}
        _ => return bad,
    }
    if last == b'y' {
        if !params.ends_with(b"$") {
            return bad;
        }
        params = &params[..params.len() - 1];
    }
    let mut values = [0u16; 2];
    let mut count = 0;
    for part in params.split(|&b| b == b';') {
        if !part.iter().all(u8::is_ascii_digit) {
            return bad;
        }
        let value = if part.is_empty() && last == b'c' {
            0
        } else {
            match std::str::from_utf8(part).ok().and_then(|s| s.parse::<u16>().ok()) {
                Some(v) => v,
                None => return bad,
            }
        };
        if count < 2 {
            values[count] = value;
        }
        count += 1;
    }
    let ok = match last {
        b'y' => count == 2 && values[1] <= 4 && matches!(values[0], 1004 | 1016 | 2004 | 2026 | 2027 | 2031),
        b'u' => count == 1 && values[0] <= 31,
        b'R' => count == 2 && values[0] != 0 && values[1] != 0,
        _ => true,
    };
    if ok { Ok(()) } else { bad }
}

// ---- terminal-capability-detection.ts ----

/// `isCapabilityResponse`: whether a sequence is one of the capability replies the renderer consumes.
pub fn is_capability_response(sequence: &str) -> bool {
    let b = sequence.as_bytes();
    let n = b.len();
    // DECRPM: ESC [ ? digits (; digits)* $ y
    let csi_private = |i: usize| i + 2 < n && b[i] == 0x1b && b[i + 1] == b'[' && b[i + 2] == b'?';
    for i in 0..n {
        if csi_private(i) {
            let mut j = i + 3;
            let digits = |j: &mut usize| {
                let s = *j;
                while *j < n && b[*j].is_ascii_digit() {
                    *j += 1;
                }
                *j > s
            };
            // DECRPM
            {
                let mut k = j;
                if digits(&mut k) {
                    loop {
                        if k < n && b[k] == b';' {
                            let mut m = k + 1;
                            if digits(&mut m) {
                                k = m;
                                continue;
                            }
                        }
                        break;
                    }
                    if k + 1 < n && b[k] == b'$' && b[k + 1] == b'y' {
                        return true;
                    }
                }
            }
            // Kitty keyboard: ESC [ ? digits (; digits)? u
            {
                let mut k = j;
                if digits(&mut k) {
                    if k < n && b[k] == b'u' {
                        return true;
                    }
                    if k < n && b[k] == b';' {
                        let mut m = k + 1;
                        if digits(&mut m) && m < n && b[m] == b'u' {
                            return true;
                        }
                    }
                }
            }
            // DA1: ESC [ ? [0-9;]* c
            while j < n && (b[j].is_ascii_digit() || b[j] == b';') {
                j += 1;
            }
            if j < n && b[j] == b'c' {
                return true;
            }
        }
    }
    // XTVersion: ESC P > | ... ESC \
    if let Some(start) = sequence.find("\x1bP>|") {
        if sequence[start + 4..].contains("\x1b\\") {
            return true;
        }
    }
    // XTGETTCAP Ms: ESC P (1+r4d73(=[^ESC]*)? | 0+r(4d73)?) ESC \  (case-insensitive)
    let lower = sequence.to_ascii_lowercase();
    let mut search = 0;
    while let Some(rel) = lower[search..].find("\x1bp") {
        let start = search + rel + 2;
        let rest = &lower[start..];
        let ok = if let Some(r) = rest.strip_prefix("1+r4d73") {
            let r = match r.strip_prefix('=') {
                Some(value) => &value[value.find('\x1b').unwrap_or(value.len())..],
                None => r,
            };
            r.starts_with("\x1b\\")
        } else if let Some(r) = rest.strip_prefix("0+r") {
            r.strip_prefix("4d73").unwrap_or(r).starts_with("\x1b\\")
        } else {
            false
        };
        if ok {
            return true;
        }
        search = start;
    }
    // Kitty graphics: ESC _ G ... ESC \
    if let Some(start) = sequence.find("\x1b_G") {
        if sequence[start + 3..].contains("\x1b\\") {
            return true;
        }
    }
    // OSC 99 notification query reply.
    let mut search = 0;
    while let Some(rel) = sequence[search..].find("\x1b]99;") {
        let start = search + rel + 5;
        let body_end = sequence[start..].find(['\x07', '\x1b']).map(|i| start + i);
        if let Some(end) = body_end {
            let body = &sequence[start..end];
            if let Some(id) = body.find("i=opentui-notifications") {
                if body[id..].contains("p=?") && (sequence[end..].contains('\x07') || sequence[end..].contains("\x1b\\")) {
                    return true;
                }
            }
        }
        search = start;
    }
    // iTerm2 feature reporting: ESC ] 1337 ; Capabilities = ... (BEL | ST)
    if let Some(start) = sequence.find("\x1b]1337;Capabilities=") {
        let rest = &sequence[start..];
        if rest.contains('\x07') || rest.contains("\x1b\\") {
            return true;
        }
    }
    // OSC 7501: ESC ] 7501 ; spaces ? [^BEL ESC]* (BEL | ST)
    let mut search = 0;
    while let Some(rel) = sequence[search..].find("\x1b]7501;") {
        let start = search + rel + 7;
        let rest = sequence[start..].trim_start_matches(' ');
        if let Some(after) = rest.strip_prefix('?') {
            let end = after.find(['\x07', '\x1b']).unwrap_or(after.len());
            let tail = &after[end..];
            if tail.starts_with('\x07') || tail.starts_with("\x1b\\") {
                return true;
            }
        }
        search = start;
    }
    false
}

/// `isExplicitWidthReport`: a CPR `ESC [ 1 ; n R` with n other than 1.
pub fn is_explicit_width_report(sequence: &str) -> bool {
    let mut search = 0;
    while let Some(rel) = sequence[search..].find("\x1b[1;") {
        let start = search + rel + 4;
        let rest = &sequence[start..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && rest.as_bytes().get(digits) == Some(&b'R') && &rest[..digits] != "1" {
            // `(?!1R)\d+R`: "11R" passes, only exactly "1R" is excluded.
            return true;
        }
        search = start;
    }
    false
}

/// `parsePixelResolution`: `ESC [ 4 ; height ; width t`.
pub fn parse_pixel_resolution(sequence: &str) -> Option<(u32, u32)> {
    let start = sequence.find("\x1b[4;")? + 4;
    let rest = &sequence[start..];
    let h_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    if h_len == 0 || rest.as_bytes().get(h_len) != Some(&b';') {
        return None;
    }
    let after = &rest[h_len + 1..];
    let w_len = after.bytes().take_while(u8::is_ascii_digit).count();
    if w_len == 0 || after.as_bytes().get(w_len) != Some(&b't') {
        return None;
    }
    let height: u64 = rest[..h_len].parse().ok()?;
    let width: u64 = after[..w_len].parse().ok()?;
    if width > 0x7fff_ffff || height > 0x7fff_ffff {
        return None;
    }
    Some((width as u32, height as u32))
}

/// `isPixelResolutionResponse`.
pub fn is_pixel_resolution_response(sequence: &str) -> bool {
    let Some(start) = sequence.find("\x1b[4;") else { return false };
    let rest = &sequence[start + 4..];
    let h = rest.bytes().take_while(u8::is_ascii_digit).count();
    if h == 0 || rest.as_bytes().get(h) != Some(&b';') {
        return false;
    }
    let after = &rest[h + 1..];
    let w = after.bytes().take_while(u8::is_ascii_digit).count();
    w > 0 && after.as_bytes().get(w) == Some(&b't')
}

// ---- renderer.ts reply handling, renderer-theme-mode.ts, terminal-palette.ts ----

/// `ThemeMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

/// What a reply changed, for the host to act on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplyEffects {
    /// One of the renderer's handlers consumed the sequence (it goes no further).
    pub handled: bool,
    /// The capabilities changed: the renderer repaints every cell (`forceFullRepaintRequested`) and emits
    /// `capabilities`.
    pub capabilities_changed: bool,
    /// The theme mode changed (`theme_mode` event).
    pub theme_mode: Option<ThemeMode>,
    /// The terminal's pixel size arrived (width, height): the renderer requests a frame.
    pub resolution: Option<(u32, u32)>,
    /// The terminal gained (`true`) or lost focus (`focus`/`blur` events).
    pub focus: Option<bool>,
    /// A palette detection finished and was applied: the renderer requests a frame.
    pub palette_applied: bool,
}

/// The renderer's protocol context for the stdin parser (`StdinParserProtocolContext`, without
/// `kittyKeyboardEnabled`, which follows the kitty flags).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProtocolContext {
    pub private_capability_replies_active: bool,
    pub pixel_resolution_query_active: bool,
    pub explicit_width_cpr_active: bool,
    pub startup_cursor_cpr_active: bool,
    pub reply_sentinel_active: bool,
}

/// `REPLY_BACKSTOP_MS`.
const REPLY_BACKSTOP_MS: u64 = 5000;
/// `PIXEL_QUERIES_MAX`.
const PIXEL_QUERIES_MAX: u32 = 4;
/// `RendererThemeMode.QUERY_TIMEOUT_MS`.
const THEME_REFRESH_MS: u64 = 250;
/// `NATIVE_PALETTE_QUERY_SIZE`.
const NATIVE_PALETTE_QUERY_SIZE: usize = 16;
/// `OTUI_PALETTE_IDLE_TIMEOUT_MS`'s default.
const PALETTE_IDLE_MS: u64 = 300;
/// `detectOSCSupport`'s timeout.
const PALETTE_PROBE_MS: u64 = 300;
/// `detect`'s timeout.
const PALETTE_TIMEOUT_MS: u64 = 5000;

/// A detected palette (`TerminalColors`): `#rrggbb` per entry where the terminal answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalColors {
    pub palette: Vec<Option<String>>,
    pub default_foreground: Option<String>,
    pub default_background: Option<String>,
    pub cursor_color: Option<String>,
    pub mouse_foreground: Option<String>,
    pub mouse_background: Option<String>,
    pub tek_foreground: Option<String>,
    pub tek_background: Option<String>,
    pub highlight_background: Option<String>,
    pub highlight_foreground: Option<String>,
}

/// Palette detection in progress (`TerminalPalette.detect`).
#[derive(Clone, Debug)]
enum Detection {
    /// `detectOSCSupport`: one OSC 4 query, 300 ms.
    Probe { size: usize, deadline: u64, buffer: String },
    /// `queryPalette` and `querySpecialColors` together.
    Query {
        size: usize,
        palette: Vec<Option<String>>,
        palette_done: bool,
        palette_buffer: String,
        palette_deadline: u64,
        palette_idle: u64,
        specials: [Option<String>; 9],
        specials_done: bool,
        specials_buffer: String,
        specials_deadline: u64,
        specials_idle: u64,
    },
}

const SPECIAL_INDEXES: [u32; 9] = [10, 11, 12, 13, 14, 15, 16, 17, 19];

/// The renderer's handling of terminal replies (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct Replies {
    /// The startup reply batch's backstop deadline, once setup is done.
    startup_until: Option<u64>,
    startup_pending: bool,
    terminal_setup: bool,
    theme_mode: Option<ThemeMode>,
    theme_refresh_until: Option<u64>,
    unanswered_pixel_queries: u32,
    waiting_for_pixel: bool,
    pixel_requery_pending: bool,
    resolution: Option<(u32, u32)>,
    focus: Option<bool>,
    restore_modes_on_next_focus: bool,
    capabilities: Option<Capabilities>,
    detection: Option<Detection>,
    detection_generation: u64,
    detection_queued: bool,
    publish_generation: u64,
    palette_cache: HashMap<usize, TerminalColors>,
    palette_epoch: u32,
    native_palette_signature: Option<String>,
}

fn scale_component(component: &str) -> String {
    let value = u64::from_str_radix(component, 16).unwrap_or(0) as f64;
    let max = ((1u64 << (4 * component.len().min(15))) - 1) as f64;
    format!("{:02x}", ((value / max) * 255.0 + 0.5).floor() as u64)
}

/// Every `OSC n ; (rgb:r/g/b | #rrggbb) (BEL|ST)` in a text: (n, `#rrggbb`).
fn osc_colors(text: &str) -> Vec<(u32, Option<u32>, String)> {
    let mut found = Vec::new();
    let mut search = 0;
    while let Some(rel) = text[search..].find("\x1b]") {
        let start = search + rel + 2;
        search = start;
        let rest = &text[start..];
        let n_len = rest.bytes().take_while(u8::is_ascii_digit).count();
        if n_len == 0 || rest.as_bytes().get(n_len) != Some(&b';') {
            continue;
        }
        let number: u32 = rest[..n_len].parse().unwrap_or(u32::MAX);
        let mut body = &rest[n_len + 1..];
        // OSC 4 carries the index before the color.
        let mut index = None;
        if number == 4 {
            let i_len = body.bytes().take_while(u8::is_ascii_digit).count();
            if i_len == 0 || body.as_bytes().get(i_len) != Some(&b';') {
                continue;
            }
            index = body[..i_len].parse().ok();
            body = &body[i_len + 1..];
        }
        let (hex, after) = if let Some(rgb) = body.strip_prefix("rgb:") {
            let parts: Vec<&str> = rgb.splitn(3, '/').collect();
            if parts.len() != 3 {
                continue;
            }
            let hexlen = |s: &str| s.bytes().take_while(u8::is_ascii_hexdigit).count();
            let (r, g) = (parts[0], parts[1]);
            let b_len = hexlen(parts[2]);
            if r.is_empty() || g.is_empty() || b_len == 0 || hexlen(r) != r.len() || hexlen(g) != g.len() {
                continue;
            }
            let b = &parts[2][..b_len];
            (format!("#{}{}{}", scale_component(r), scale_component(g), scale_component(b)), &parts[2][b_len..])
        } else if let Some(hex) = body.strip_prefix('#') {
            if hex.len() < 6 || !hex.as_bytes()[..6].iter().all(u8::is_ascii_hexdigit) {
                continue;
            }
            (format!("#{}", hex[..6].to_ascii_lowercase()), &hex[6..])
        } else {
            continue;
        };
        if after.starts_with('\x07') || after.starts_with("\x1b\\") {
            found.push((number, index, hex));
        }
    }
    found
}

/// `normalizeTerminalPalette` + `buildTerminalPaletteSignature`.
fn normalize_palette(colors: &TerminalColors) -> (Vec<RGBA>, RGBA, RGBA, String) {
    let palette: Vec<RGBA> = (0..256)
        .map(|i| match colors.palette.get(i).cloned().flatten() {
            Some(hex) => RGBA::from_hex(&hex),
            None => color::fallback_ansi256_color(i as u8),
        })
        .collect();
    let fg = colors.default_foreground.as_deref().map_or(RGBA::rgb(255, 255, 255, 255), RGBA::from_hex);
    let bg = colors.default_background.as_deref().map_or(RGBA::rgb(0, 0, 0, 255), RGBA::from_hex);
    let ints = |c: &RGBA| c.to_ints().iter().map(u8::to_string).collect::<Vec<_>>().join(",");
    let signature = format!("{}|{}|{}", palette.iter().map(ints).collect::<Vec<_>>().join(";"), ints(&fg), ints(&bg));
    (palette, fg, bg, signature)
}

impl Replies {
    pub fn new() -> Self {
        Replies::default()
    }

    /// The theme mode the terminal's background color implies, once known (`themeMode`).
    pub fn theme_mode(&self) -> Option<ThemeMode> {
        self.theme_mode
    }

    /// The terminal's size in pixels, once it answered (`resolution`).
    pub fn resolution(&self) -> Option<(u32, u32)> {
        self.resolution
    }

    /// Whether the terminal has focus, once it reported it.
    pub fn focused(&self) -> Option<bool> {
        self.focus
    }

    /// The latest detected palette of at least `size` entries, if any (`getCachedPaletteBySize`).
    pub fn cached_palette(&self, size: usize) -> Option<TerminalColors> {
        if let Some(exact) = self.palette_cache.get(&size) {
            return Some(exact.clone());
        }
        let larger = self.palette_cache.keys().filter(|k| **k >= size).min()?;
        let mut projected = self.palette_cache[larger].clone();
        projected.palette.truncate(size);
        Some(projected)
    }

    /// The parser's protocol context now.
    pub fn protocol_context(&self) -> ProtocolContext {
        ProtocolContext {
            private_capability_replies_active: self.startup_pending,
            pixel_resolution_query_active: self.waiting_for_pixel,
            explicit_width_cpr_active: self.startup_pending,
            startup_cursor_cpr_active: false,
            reply_sentinel_active: false,
        }
    }

    /// The setup's reply batch starts (`admitReplyBatch("startup")`): call when [`Session::setup_terminal`]
    /// is accepted.
    pub fn before_setup(&mut self, renderer: &Renderer) {
        self.startup_pending = true;
        self.capabilities = Some(renderer.terminal.capabilities());
    }

    /// After setup's packets are written (`setupTerminal` resolved): the startup batch's backstop starts, the
    /// pixel size is queried and, on a 256-color terminal, the palette.
    pub fn after_setup(&mut self, session: &mut Session, renderer: &mut Renderer, now: u64, out: &mut Vec<u8>) {
        self.terminal_setup = true;
        self.capabilities = Some(renderer.terminal.capabilities());
        self.startup_until = Some(now + REPLY_BACKSTOP_MS);
        self.query_pixel_resolution(session, renderer, out);
        if self.should_sync_native_palette(renderer) {
            self.refresh_palette(session, renderer, now, out);
        }
    }

    fn should_sync_native_palette(&self, renderer: &Renderer) -> bool {
        let caps = renderer.terminal.capabilities();
        self.terminal_setup && caps.ansi256 && !caps.rgb
    }

    /// `queryPixelResolution`: `CSI 14 t`, at most 4 unanswered.
    pub fn query_pixel_resolution(&mut self, session: &mut Session, renderer: &mut Renderer, out: &mut Vec<u8>) {
        self.pixel_requery_pending = true;
        if !self.terminal_setup || self.waiting_for_pixel || self.unanswered_pixel_queries >= PIXEL_QUERIES_MAX {
            return;
        }
        if session.control(renderer, Control::QueryPixelResolution { sentinel: false }, out).is_err() {
            return;
        }
        self.pixel_requery_pending = false;
        self.waiting_for_pixel = true;
        self.unanswered_pixel_queries += 1;
    }

    /// The earliest time [`Self::tick`] has something to do.
    pub fn next_deadline(&self) -> Option<u64> {
        let detection = match &self.detection {
            Some(Detection::Probe { deadline, .. }) => Some(*deadline),
            Some(Detection::Query { palette_done, palette_deadline, palette_idle, specials_done, specials_deadline, specials_idle, .. }) => {
                let palette = (!palette_done).then(|| (*palette_deadline).min(*palette_idle));
                let specials = (!specials_done).then(|| (*specials_deadline).min(*specials_idle));
                palette.into_iter().chain(specials).min()
            }
            None => None,
        };
        [self.startup_until, self.theme_refresh_until, detection].into_iter().flatten().min()
    }

    /// Runs the timers due at `now`.
    pub fn tick(&mut self, session: &mut Session, renderer: &mut Renderer, now: u64, out: &mut Vec<u8>) -> ReplyEffects {
        let mut effects = ReplyEffects::default();
        if self.startup_until.is_some_and(|t| now >= t) {
            self.startup_until = None;
            self.startup_pending = false;
        }
        if self.theme_refresh_until.is_some_and(|t| now >= t) {
            self.theme_refresh_until = None;
        }
        let mut finished = None;
        match &mut self.detection {
            Some(Detection::Probe { size, deadline, .. }) if now >= *deadline => {
                finished = Some(TerminalColors { palette: vec![None; *size], ..Default::default() });
            }
            Some(Detection::Query { palette_done, palette_deadline, palette_idle, specials_done, specials_deadline, specials_idle, .. }) => {
                if !*palette_done && now >= (*palette_deadline).min(*palette_idle) {
                    *palette_done = true;
                }
                if !*specials_done && now >= (*specials_deadline).min(*specials_idle) {
                    *specials_done = true;
                }
                finished = self.query_result();
            }
            _ => {}
        }
        if let Some(colors) = finished {
            effects.palette_applied = self.finish_detection(colors, session, renderer, now, out);
        }
        effects
    }

    fn query_result(&self) -> Option<TerminalColors> {
        let Some(Detection::Query { palette, palette_done, specials, specials_done, .. }) = &self.detection else { return None };
        if !palette_done || !specials_done {
            return None;
        }
        Some(TerminalColors {
            palette: palette.clone(),
            default_foreground: specials[0].clone(),
            default_background: specials[1].clone(),
            cursor_color: specials[2].clone(),
            mouse_foreground: specials[3].clone(),
            mouse_background: specials[4].clone(),
            tek_foreground: specials[5].clone(),
            tek_background: specials[6].clone(),
            highlight_background: specials[7].clone(),
            highlight_foreground: specials[8].clone(),
        })
    }

    /// `refreshPalette` (`getPalette({ size: 16 })`).
    pub fn refresh_palette(&mut self, session: &mut Session, renderer: &mut Renderer, now: u64, out: &mut Vec<u8>) {
        self.request_palette(NATIVE_PALETTE_QUERY_SIZE, session, renderer, now, out);
    }

    /// `getPalette({ size })`: starts a detection unless one of the same generation covers it or the cache does.
    pub fn request_palette(&mut self, size: usize, session: &mut Session, renderer: &mut Renderer, now: u64, out: &mut Vec<u8>) {
        if !self.terminal_setup || self.cached_palette(size).is_some() {
            return;
        }
        if let Some(detection) = &self.detection {
            let running = match detection {
                Detection::Probe { size, .. } | Detection::Query { size, .. } => *size,
            };
            if running < size || self.detection_generation != self.publish_generation {
                self.detection_queued = true;
            }
            return;
        }
        self.detection_generation = self.publish_generation;
        // detectOSCSupport: one query of entry 0.
        if session.control(renderer, Control::PaletteQuery(b"\x1b]4;0;?\x07".to_vec()), out).is_err() {
            return;
        }
        self.detection = Some(Detection::Probe { size, deadline: now + PALETTE_PROBE_MS, buffer: String::new() });
    }

    /// `clearPaletteCache`.
    pub fn clear_palette_cache(&mut self) {
        self.publish_generation += 1;
        self.palette_cache.clear();
    }

    /// Ends a detection: publishes it (native palette state on a 256-color terminal, the cache) when no newer
    /// generation replaced it, then runs a queued detection. Returns whether the native palette changed.
    fn finish_detection(&mut self, colors: TerminalColors, session: &mut Session, renderer: &mut Renderer, now: u64, out: &mut Vec<u8>) -> bool {
        self.detection = None;
        let publish = self.detection_generation == self.publish_generation;
        let mut applied = false;
        if publish && self.should_sync_native_palette(renderer) && colors.palette.len() >= NATIVE_PALETTE_QUERY_SIZE {
            let (palette, fg, bg, signature) = normalize_palette(&colors);
            let epoch = if self.native_palette_signature.as_deref() != Some(&signature) { self.palette_epoch.wrapping_add(1) } else { self.palette_epoch };
            if session.set_palette_state(renderer, &palette, fg, bg, epoch).is_ok() {
                applied = true;
            }
            self.native_palette_signature = Some(signature);
            self.palette_epoch = epoch;
        }
        if publish {
            self.palette_cache.insert(colors.palette.len(), colors);
        }
        let queued = std::mem::take(&mut self.detection_queued);
        if (publish && self.should_sync_native_palette(renderer) && !self.palette_cache.contains_key(&NATIVE_PALETTE_QUERY_SIZE)) || queued {
            self.refresh_palette(session, renderer, now, out);
        }
        applied
    }

    /// Every OSC reply, before the handlers (the palette detector's `subscribeOsc`).
    pub fn handle_osc(&mut self, sequence: &str, session: &mut Session, renderer: &mut Renderer, now: u64, out: &mut Vec<u8>) -> ReplyEffects {
        let mut effects = ReplyEffects::default();
        let mut start_query = None;
        match &mut self.detection {
            Some(Detection::Probe { size, buffer, .. }) => {
                buffer.push_str(sequence);
                if osc_colors(buffer).iter().any(|(n, index, _)| *n == 4 && index.is_some()) {
                    start_query = Some(*size);
                }
            }
            Some(Detection::Query {
                palette,
                palette_done,
                palette_buffer,
                palette_idle,
                specials,
                specials_done,
                specials_buffer,
                specials_idle,
                ..
            }) => {
                if !*palette_done {
                    palette_buffer.push_str(sequence);
                    for (n, index, hex) in osc_colors(palette_buffer) {
                        if let (4, Some(index)) = (n, index) {
                            if let Some(slot) = palette.get_mut(index as usize) {
                                *slot = Some(hex);
                            }
                        }
                    }
                    if palette_buffer.len() > 8192 {
                        let keep = palette_buffer.len() - 4096;
                        let keep = (keep..palette_buffer.len()).find(|i| palette_buffer.is_char_boundary(*i)).unwrap_or(palette_buffer.len());
                        palette_buffer.drain(..keep);
                    }
                    if palette.iter().all(Option::is_some) {
                        *palette_done = true;
                    } else {
                        *palette_idle = now + PALETTE_IDLE_MS;
                    }
                }
                if !*specials_done {
                    specials_buffer.push_str(sequence);
                    let mut updated = false;
                    for (n, _, hex) in osc_colors(specials_buffer) {
                        if let Some(slot) = SPECIAL_INDEXES.iter().position(|i| *i == n) {
                            specials[slot] = Some(hex);
                            updated = true;
                        }
                    }
                    if specials_buffer.len() > 8192 {
                        let keep = specials_buffer.len() - 4096;
                        let keep = (keep..specials_buffer.len()).find(|i| specials_buffer.is_char_boundary(*i)).unwrap_or(specials_buffer.len());
                        specials_buffer.drain(..keep);
                    }
                    let queried = if renderer.terminal.is_in_tmux() { &SPECIAL_INDEXES[..3] } else { &SPECIAL_INDEXES[..] };
                    if queried.iter().all(|i| specials[SPECIAL_INDEXES.iter().position(|j| j == i).unwrap_or(0)].is_some()) {
                        *specials_done = true;
                    } else if updated {
                        *specials_idle = now + PALETTE_IDLE_MS;
                    }
                }
                if let Some(colors) = self.query_result() {
                    effects.palette_applied = self.finish_detection(colors, session, renderer, now, out);
                }
            }
            None => {}
        }
        if let Some(size) = start_query {
            let tmux = renderer.terminal.is_in_tmux();
            let mut palette_query = String::new();
            for i in 0..size {
                palette_query.push_str(&format!("\x1b]4;{i};?\x07"));
            }
            let specials: &[u32] = if tmux { &SPECIAL_INDEXES[..3] } else { &SPECIAL_INDEXES };
            let special_query: String = specials.iter().map(|i| format!("\x1b]{i};?\x07")).collect();
            let _ = session.control(renderer, Control::PaletteQuery(palette_query.into_bytes()), out);
            let _ = session.control(renderer, Control::PaletteQuery(special_query.into_bytes()), out);
            self.detection = Some(Detection::Query {
                size,
                palette: vec![None; size],
                palette_done: false,
                palette_buffer: String::new(),
                palette_deadline: now + PALETTE_TIMEOUT_MS,
                palette_idle: now + PALETTE_IDLE_MS,
                specials: Default::default(),
                specials_done: false,
                specials_buffer: String::new(),
                specials_deadline: now + PALETTE_TIMEOUT_MS,
                specials_idle: now + PALETTE_IDLE_MS,
            });
        }
        effects
    }

    /// A cursor position report (parser protocol `cpr`) before the handlers: consumed when it answers a
    /// startup probe (`processCapabilitySequence(sequence, true)`).
    pub fn handle_cpr(&mut self, sequence: &str, session: &mut Session, renderer: &mut Renderer, out: &mut Vec<u8>) -> ReplyEffects {
        self.process_capability_sequence(sequence, true, session, renderer, out)
    }

    fn process_capability_sequence(&mut self, sequence: &str, cursor_report: bool, session: &mut Session, renderer: &mut Renderer, out: &mut Vec<u8>) -> ReplyEffects {
        let mut effects = ReplyEffects::default();
        let standard = is_capability_response(sequence) || (cursor_report && is_explicit_width_report(sequence));
        let process = standard || (cursor_report && (self.startup_pending || session.phase() == TerminalPhase::SettingUp));
        if !process {
            return effects;
        }
        match session.control(renderer, Control::CapabilityResponse(sequence.as_bytes().to_vec()), out) {
            Ok(()) => {}
            Err(SessionError::Inactive) => return effects,
            // A rejected reply is consumed without being applied.
            Err(_) => {
                effects.handled = true;
                return effects;
            }
        }
        let caps = renderer.terminal.capabilities();
        if self.capabilities != Some(caps) {
            self.capabilities = Some(caps);
            effects.capabilities_changed = true;
        }
        effects.handled = standard;
        effects
    }

    /// The renderer's sequence handlers, in order: the pixel size, capability replies, focus, the theme colors.
    /// Returns what they changed; `handled` false passes the sequence on to the application.
    pub fn handle_sequence(&mut self, sequence: &str, session: &mut Session, renderer: &mut Renderer, now: u64, out: &mut Vec<u8>) -> ReplyEffects {
        // Pixel resolution.
        if is_pixel_resolution_response(sequence) && self.unanswered_pixel_queries > 0 {
            self.unanswered_pixel_queries -= 1;
            self.waiting_for_pixel = false;
            if self.pixel_requery_pending {
                self.query_pixel_resolution(session, renderer, out);
                return ReplyEffects { handled: true, ..Default::default() };
            }
            let resolution = parse_pixel_resolution(sequence);
            if resolution.is_some() {
                self.resolution = resolution;
            }
            return ReplyEffects { handled: true, resolution, ..Default::default() };
        }
        // Capability replies.
        let effects = self.process_capability_sequence(sequence, false, session, renderer, out);
        if effects.handled {
            return effects;
        }
        // Focus.
        if sequence == "\x1b[I" {
            if self.restore_modes_on_next_focus {
                let _ = session.control(renderer, Control::RestoreModes, out);
                self.restore_modes_on_next_focus = false;
            }
            let focus = (self.focus != Some(true)).then_some(true);
            self.focus = Some(true);
            return ReplyEffects { handled: true, focus, ..effects };
        }
        if sequence == "\x1b[O" {
            self.restore_modes_on_next_focus = true;
            let focus = (self.focus != Some(false)).then_some(false);
            self.focus = Some(false);
            return ReplyEffects { handled: true, focus, ..effects };
        }
        // Theme mode (renderer-theme-mode.ts).
        if sequence == "\x1b[?997;1n" || sequence == "\x1b[?997;2n" {
            if self.theme_refresh_until.is_none() {
                let _ = session.control(renderer, Control::QueryThemeColors, out);
                self.theme_refresh_until = Some(now + THEME_REFRESH_MS);
            }
            return ReplyEffects { handled: true, ..effects };
        }
        let colors = osc_colors(sequence);
        let theme: Vec<_> = colors.iter().filter(|(n, index, _)| (*n == 10 || *n == 11) && index.is_none()).collect();
        if theme.is_empty() {
            return effects;
        }
        let mut result = ReplyEffects { handled: true, ..effects };
        let Some((_, _, background)) = theme.iter().rev().find(|(n, ..)| *n == 11) else { return result };
        self.theme_refresh_until = None;
        let [r, g, b, _] = RGBA::from_hex(background).to_ints();
        let brightness = (r as f64 * 299.0 + g as f64 * 587.0 + b as f64 * 114.0) / 1000.0;
        let mode = if brightness > 128.0 { ThemeMode::Light } else { ThemeMode::Dark };
        if self.theme_mode != Some(mode) {
            self.theme_mode = Some(mode);
            result.theme_mode = Some(mode);
            self.clear_palette_cache();
            if self.should_sync_native_palette(renderer) {
                self.refresh_palette(session, renderer, now, out);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_detection() {
        assert!(is_capability_response("\x1b[?2026;2$y"));
        assert!(is_capability_response("\x1b[?0u"));
        assert!(is_capability_response("\x1b[?62;22c"));
        assert!(is_capability_response("\x1bP>|ghostty 1.1.3\x1b\\"));
        assert!(is_capability_response("\x1bP0+r\x1b\\"));
        assert!(is_capability_response("\x1b_Gi=31337;OK\x1b\\"));
        assert!(!is_capability_response("\x1b[1;2R"));
        assert!(!is_capability_response("\x1b]11;rgb:0000/0000/0000\x07"));
        assert!(is_explicit_width_report("\x1b[1;2R"));
        assert!(!is_explicit_width_report("\x1b[1;1R"));
        assert_eq!(parse_pixel_resolution("\x1b[4;600;800t"), Some((800, 600)));
        assert_eq!(osc_colors("\x1b]11;rgb:ffff/0000/8080\x07"), vec![(11, None, "#ff0080".to_owned())]);
    }

    #[test]
    fn setup_sequence() {
        let mut env = HashMap::new();
        env.insert("TERM".to_owned(), "xterm-256color".to_owned());
        let mut renderer = Renderer::new(80, 24, Terminal::new(RemoteMode::Auto, Some(env)));
        let mut session = Session::new();
        session.setup_terminal(&mut renderer, TerminalOptions::default()).unwrap();
        let mut out = Vec::new();
        while session.pump(&mut renderer, 0, &mut out) == Pump::Again {}
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1b[?2031h\x1b]10;?\x07\x1b]11;?\x07\x1b[>0q\x1b[?25l\x1b[s\x1b[6n"));
        assert!(text.ends_with("\x1b[u\x1b[s\x1b[?1049h\x1b[?25l\x1b[>4;1m\x1b[?2027h\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h"));
        assert_eq!(session.phase(), TerminalPhase::Active);
        let mut out = Vec::new();
        session.control(&mut renderer, Control::CapabilityResponse(b"\x1b[?0u".to_vec()), &mut out).unwrap();
        assert_eq!(out, b"\x1b[>4;0m\x1b[>5u\x1b[?2027h\x1b[?2004h");
    }
}
