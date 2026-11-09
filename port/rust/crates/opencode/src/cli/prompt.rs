//! @clack/prompts 1.0.0-alpha.1 on a terminal, as the CLI's `auth` commands use it (packages/cli/src/ui/prompt.ts,
//! ui/integration-picker.ts): picocolors' colors, the log lines, `intro`/`outro`/`cancel`, the spinner (without its
//! animation: its frames are overwritten by the line it stops with), and the interactive prompts (`text`,
//! `password`, `select`, `confirm`, `multiselect`, `autocomplete` and the integration picker's
//! `AutocompletePrompt`).
//!
//! A prompt is @clack/core's `Prompt`: keys arrive as node's readline decodes them, a tracked prompt keeps readline's
//! line (its user input), and each change renders the prompt's frame (wrapped as wrap-ansi wraps it to the terminal's
//! width) over the previous one, redrawing only the lines that changed.

use std::io::Read;

use super::out::{Fail, Result, stdout};

pub const S_BAR_START: &str = "┌";
pub const S_BAR: &str = "│";
pub const S_BAR_END: &str = "└";
pub const S_STEP_ACTIVE: &str = "◆";
pub const S_STEP_CANCEL: &str = "■";
pub const S_STEP_ERROR: &str = "▲";
pub const S_STEP_SUBMIT: &str = "◇";
pub const S_RADIO_ACTIVE: &str = "●";
pub const S_RADIO_INACTIVE: &str = "○";
pub const S_CHECKBOX_ACTIVE: &str = "◻";
pub const S_CHECKBOX_SELECTED: &str = "◼";
pub const S_CHECKBOX_INACTIVE: &str = "◻";
pub const S_PASSWORD_MASK: &str = "▪";
pub const S_INFO: &str = "●";
pub const S_SUCCESS: &str = "◆";
pub const S_ERROR: &str = "■";

/// picocolors (colors when stdout is a terminal and `NO_COLOR` is unset) and clack's framing output.
pub struct Clack {
    pub color: bool,
}

impl Clack {
    pub fn new() -> Self {
        let color = std::env::var_os("NO_COLOR").is_none()
            && (std::env::var_os("FORCE_COLOR").is_some()
                || (super::stdout_tty() && std::env::var("TERM").as_deref() != Ok("dumb"))
                || std::env::var_os("CI").is_some());
        Clack { color }
    }

    /// picocolors' `formatter`: nested closes inside the text reopen the style.
    fn paint(&self, open: &str, close: &str, replace: &str, text: &str) -> String {
        if !self.color {
            return text.to_owned();
        }
        let open = format!("\x1b[{open}m");
        let close = format!("\x1b[{close}m");
        let replace = if replace.is_empty() { open.clone() } else { replace.to_owned() };
        let mut out = open.clone();
        match text.get(open.len()..).and_then(|rest| rest.find(&close)).map(|i| i + open.len()) {
            None => out.push_str(text),
            Some(mut index) => {
                let mut cursor = 0;
                loop {
                    out.push_str(&text[cursor..index]);
                    out.push_str(&replace);
                    cursor = index + close.len();
                    match text[cursor..].find(&close) {
                        Some(next) => index = cursor + next,
                        None => break,
                    }
                }
                out.push_str(&text[cursor..]);
            }
        }
        out.push_str(&close);
        out
    }
    pub fn gray(&self, text: &str) -> String {
        self.paint("90", "39", "", text)
    }
    pub fn red(&self, text: &str) -> String {
        self.paint("31", "39", "", text)
    }
    pub fn green(&self, text: &str) -> String {
        self.paint("32", "39", "", text)
    }
    pub fn yellow(&self, text: &str) -> String {
        self.paint("33", "39", "", text)
    }
    pub fn blue(&self, text: &str) -> String {
        self.paint("34", "39", "", text)
    }
    pub fn cyan(&self, text: &str) -> String {
        self.paint("36", "39", "", text)
    }
    pub fn dim(&self, text: &str) -> String {
        self.paint("2", "22", "\x1b[22m\x1b[2m", text)
    }
    pub fn bold(&self, text: &str) -> String {
        self.paint("1", "22", "\x1b[22m\x1b[1m", text)
    }
    pub fn inverse(&self, text: &str) -> String {
        self.paint("7", "27", "", text)
    }
    pub fn hidden(&self, text: &str) -> String {
        self.paint("8", "28", "", text)
    }
    pub fn strikethrough(&self, text: &str) -> String {
        self.paint("9", "29", "", text)
    }
    fn reset(&self, text: &str) -> String {
        self.paint("0", "0", "", text)
    }
    fn bg_white(&self, text: &str) -> String {
        self.paint("47", "49", "", text)
    }

    /// `symbol(state)`.
    fn symbol(&self, state: State) -> String {
        match state {
            State::Initial | State::Active => self.cyan(S_STEP_ACTIVE),
            State::Cancel => self.red(S_STEP_CANCEL),
            State::Error => self.yellow(S_STEP_ERROR),
            State::Submit => self.green(S_STEP_SUBMIT),
        }
    }

    pub fn intro(&self, title: &str) {
        stdout(&format!("{}  {title}\n", self.gray(S_BAR_START)));
    }
    pub fn outro(&self, message: &str) {
        stdout(&format!("{}\n{}  {message}\n\n", self.gray(S_BAR), self.gray(S_BAR_END)));
    }
    /// `cancel(message)`.
    pub fn cancel(&self, message: &str) {
        stdout(&format!("{}  {}\n\n", self.gray(S_BAR_END), self.red(message)));
    }

    /// `log.message` with its symbol: a bar line, then the message's lines.
    pub fn message(&self, message: &str, symbol: &str) {
        let mut parts = vec![self.gray(S_BAR)];
        let mut lines = message.split('\n');
        let first = lines.next().unwrap_or("");
        parts.push(if first.is_empty() { symbol.to_owned() } else { format!("{symbol}  {first}") });
        for line in lines {
            parts.push(if line.is_empty() { self.gray(S_BAR) } else { format!("{}  {line}", self.gray(S_BAR)) });
        }
        stdout(&format!("{}\n", parts.join("\n")));
    }
    pub fn info(&self, message: &str) {
        self.message(message, &self.blue(S_INFO));
    }
    pub fn success(&self, message: &str) {
        self.message(message, &self.green(S_SUCCESS));
    }
    pub fn step(&self, message: &str) {
        self.message(message, &self.green(S_STEP_SUBMIT));
    }
    pub fn error(&self, message: &str) {
        self.message(message, &self.red(S_ERROR));
    }
    pub fn plain(&self, message: &str) {
        self.message(message, &self.gray(S_BAR));
    }

    /// `spinner().start(message)` (no frames are drawn: the line it stops with overwrites them).
    pub fn start(&self) {
        stdout(&format!("\x1b[?25l{}\n", self.gray(S_BAR)));
    }
    pub fn stop(&self, message: &str, code: u8) {
        let step = match code {
            0 => self.green(S_STEP_SUBMIT),
            1 => self.red(S_STEP_CANCEL),
            _ => self.red(S_STEP_ERROR),
        };
        stdout(&format!("{step}  {message}\n\x1b[?25h"));
    }
}

/// A prompt the user dismissed (Ctrl+C, Escape): the command ends as cancelled.
pub fn cancelled() -> Fail {
    Fail::named("Cancelled", "Cancelled")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Initial,
    Active,
    Error,
    Submit,
    Cancel,
}

// --- keys (node's readline `emitKeypressEvents`) ---

#[derive(Clone, Debug, Default)]
pub struct Key {
    /// The key's text as `keypress` passes it (`undefined` for function keys).
    pub char: Option<String>,
    pub name: Option<&'static str>,
    pub ctrl: bool,
}

/// Raw mode on stdin (`setRawMode(true)`: no echo, no line buffering, no signals; output processing stays).
struct Raw {
    saved: libc::termios,
}

impl Raw {
    fn enable() -> Option<Raw> {
        // SAFETY: termios calls on stdin with a zeroed struct they fill.
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut saved) != 0 {
                return None;
            }
            let mut raw = saved;
            libc::cfmakeraw(&mut raw);
            raw.c_oflag = saved.c_oflag;
            libc::tcsetattr(0, libc::TCSANOW, &raw);
            Some(Raw { saved })
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        // SAFETY: restores the settings read in `enable`.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.saved);
        }
    }
}

type Bytes = tokio::sync::mpsc::UnboundedReceiver<u8>;

/// What stdin delivered, read by one thread for the process: a prompt takes the bytes while it listens, and what
/// arrives between prompts waits for the next one (as it waits in the terminal for readline).
fn input() -> &'static std::sync::Mutex<Option<Bytes>> {
    static INPUT: std::sync::OnceLock<std::sync::Mutex<Option<Bytes>>> = std::sync::OnceLock::new();
    INPUT.get_or_init(|| {
        let (sender, bytes) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buffer = [0u8; 256];
            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if buffer[..read].iter().any(|byte| sender.send(*byte).is_err()) {
                            break;
                        }
                    }
                }
            }
        });
        std::sync::Mutex::new(Some(bytes))
    })
}

/// Keys from stdin while a prompt listens, in raw mode.
struct Keys {
    bytes: Option<Bytes>,
    pending: Vec<u8>,
    _raw: Option<Raw>,
}

impl Keys {
    fn listen() -> Keys {
        let raw = Raw::enable();
        let bytes = input().lock().unwrap().take();
        let pending = std::mem::take(&mut *READ_AHEAD.lock().unwrap());
        Keys { bytes, pending, _raw: raw }
    }

    async fn byte(&mut self, wait: Option<std::time::Duration>) -> Option<u8> {
        if !self.pending.is_empty() {
            return Some(self.pending.remove(0));
        }
        let bytes = self.bytes.as_mut()?;
        match wait {
            None => bytes.recv().await,
            Some(wait) => tokio::time::timeout(wait, bytes.recv()).await.ok().flatten(),
        }
    }

    /// The next key; `None` once stdin ends.
    async fn next(&mut self) -> Option<Key> {
        let first = self.byte(None).await?;
        let named = |name: &'static str, char: Option<&str>| Some(Key { char: char.map(str::to_owned), name: Some(name), ctrl: false });
        match first {
            b'\r' => named("return", Some("\r")),
            b'\n' => named("enter", Some("\n")),
            b'\t' => named("tab", Some("\t")),
            0x7f | 0x08 => named("backspace", Some(&(first as char).to_string())),
            b' ' => named("space", Some(" ")),
            0x1b => {
                // A lone escape is told apart from a sequence by a pause (readline's `escapeCodeTimeout`, 50 ms).
                let Some(second) = self.byte(Some(std::time::Duration::from_millis(50))).await else {
                    return named("escape", Some("\x1b"));
                };
                if second != b'[' && second != b'O' {
                    self.pending.insert(0, second);
                    return named("escape", Some("\x1b"));
                }
                let mut sequence = vec![];
                loop {
                    let Some(byte) = self.byte(Some(std::time::Duration::from_millis(50))).await else { break };
                    sequence.push(byte);
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
                let name = match sequence.as_slice() {
                    b"A" => "up",
                    b"B" => "down",
                    b"C" => "right",
                    b"D" => "left",
                    b"H" | b"1~" | b"7~" => "home",
                    b"F" | b"4~" | b"8~" => "end",
                    b"3~" => "delete",
                    _ => "undefined",
                };
                Some(Key { char: None, name: (name != "undefined").then_some(name), ctrl: false })
            }
            0x01..=0x1a => {
                let letter = (b'a' + first - 1) as char;
                let name: &'static str = LETTERS[(letter as u8 - b'a') as usize];
                Some(Key { char: Some((first as char).to_string()), name: Some(name), ctrl: true })
            }
            byte if byte < 0x20 => Some(Key { char: Some((byte as char).to_string()), name: None, ctrl: false }),
            byte => {
                // A UTF-8 character.
                let length = match byte {
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf7 => 4,
                    _ => 1,
                };
                let mut bytes = vec![byte];
                for _ in 1..length {
                    match self.byte(Some(std::time::Duration::from_millis(50))).await {
                        Some(next) => bytes.push(next),
                        None => break,
                    }
                }
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let name = (text.len() == 1 && text.as_bytes()[0].is_ascii_alphanumeric())
                    .then(|| LETTERS_AND_DIGITS[letter_index(text.as_bytes()[0])]);
                Some(Key { char: Some(text), name, ctrl: false })
            }
        }
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        // What was read ahead (an escape sequence's lookahead) waits for the next prompt too.
        if let Some(bytes) = self.bytes.take() {
            *input().lock().unwrap() = Some(bytes);
            READ_AHEAD.lock().unwrap().extend(self.pending.drain(..));
        }
    }
}

static READ_AHEAD: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

const LETTERS: [&str; 26] =
    ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z"];
const LETTERS_AND_DIGITS: [&str; 36] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x",
    "y", "z", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9",
];

fn letter_index(byte: u8) -> usize {
    match byte {
        b'0'..=b'9' => 26 + (byte - b'0') as usize,
        _ => (byte.to_ascii_lowercase() - b'a') as usize,
    }
}

/// clack's actions: the keys that move the cursor (`settings.actions`), and the aliases of untracked prompts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    Space,
}

fn action(name: &str, tracked: bool) -> Option<Action> {
    match name {
        "up" => Some(Action::Up),
        "down" => Some(Action::Down),
        "left" => Some(Action::Left),
        "right" => Some(Action::Right),
        "space" => Some(Action::Space),
        // vim keys move the cursor of prompts without a text input.
        "k" if !tracked => Some(Action::Up),
        "j" if !tracked => Some(Action::Down),
        "h" if !tracked => Some(Action::Left),
        "l" if !tracked => Some(Action::Right),
        _ => None,
    }
}

// --- the terminal's size and wrap-ansi ---

/// `process.stdout.columns` and `rows` (80x24 when unknown).
pub fn size() -> (usize, usize) {
    // SAFETY: TIOCGWINSZ fills a zeroed winsize.
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_col > 0 {
            return (size.ws_col as usize, size.ws_row as usize);
        }
    }
    (80, 24)
}

/// string-width: the printing width of text, escape sequences left out.
pub fn width(text: &str) -> usize {
    strip(text).chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    let code = c as u32;
    if code <= 0x1f || (0x7f..=0x9f).contains(&code) || (0x300..=0x36f).contains(&code) {
        return 0;
    }
    let wide = matches!(code,
        0x1100..=0x115f | 0x2e80..=0x303e | 0x3041..=0x33ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3 | 0xf900..=0xfaff | 0xfe30..=0xfe4f | 0xff00..=0xff60 | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f | 0x1f900..=0x1f9ff | 0x1fa70..=0x1faff | 0x20000..=0x3fffd);
    if wide { 2 } else { 1 }
}

/// Text without its ANSI escape sequences.
fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                for c in chars.by_ref() {
                    if c == '\x07' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// wrap-ansi 8 (`{ hard: true }`, trimming): each line broken into rows of at most `columns`, at spaces when it can,
/// leading spaces of a row and trailing ones dropped, an open color closed at a break and reopened after it.
pub fn wrap(text: &str, columns: usize) -> String {
    text.replace("\r\n", "\n").split('\n').map(|line| wrap_line(line, columns)).collect::<Vec<_>>().join("\n")
}

fn wrap_line(line: &str, columns: usize) -> String {
    if line.trim().is_empty() {
        return String::new();
    }
    let words: Vec<&str> = line.split(' ').collect();
    let lengths: Vec<usize> = words.iter().map(|w| width(w)).collect();
    let mut rows: Vec<String> = vec![String::new()];
    for (index, word) in words.iter().enumerate() {
        let last = rows.last_mut().unwrap();
        *last = last.trim_start().to_owned();
        let mut row_length = width(rows.last().unwrap());
        if index != 0 && row_length > 0 {
            rows.last_mut().unwrap().push(' ');
            row_length += 1;
        }
        if lengths[index] > columns {
            let remaining = columns.saturating_sub(row_length);
            let starting_this = 1 + (lengths[index].saturating_sub(remaining).saturating_sub(1)) / columns;
            let starting_next = (lengths[index] - 1) / columns;
            if starting_next < starting_this {
                rows.push(String::new());
            }
            wrap_word(&mut rows, word, columns);
            continue;
        }
        if row_length + lengths[index] > columns && row_length > 0 && lengths[index] > 0 {
            rows.push(String::new());
        }
        rows.last_mut().unwrap().push_str(word);
    }
    let rows: Vec<String> = rows.iter().map(|row| trim_spaces_right(row)).collect();
    reopen(&rows.join("\n"))
}

fn wrap_word(rows: &mut Vec<String>, word: &str, columns: usize) {
    let characters: Vec<char> = word.chars().collect();
    let mut inside = false;
    let mut visible = width(rows.last().unwrap());
    for (index, &c) in characters.iter().enumerate() {
        let length = char_width(c);
        if visible + length <= columns {
            rows.last_mut().unwrap().push(c);
        } else {
            rows.push(c.to_string());
            visible = 0;
        }
        if c == '\x1b' {
            inside = true;
        }
        if inside {
            if c == 'm' {
                inside = false;
            }
            continue;
        }
        visible += length;
        if visible == columns && index < characters.len() - 1 {
            rows.push(String::new());
            visible = 0;
        }
    }
    if visible == 0 && !rows.last().unwrap().is_empty() && rows.len() > 1 {
        let tail = rows.pop().unwrap();
        rows.last_mut().unwrap().push_str(&tail);
    }
}

fn trim_spaces_right(row: &str) -> String {
    let words: Vec<&str> = row.split(' ').collect();
    let mut last = words.len();
    while last > 0 && width(words[last - 1]) == 0 {
        last -= 1;
    }
    if last == words.len() {
        return row.to_owned();
    }
    format!("{}{}", words[..last].join(" "), words[last..].concat())
}

/// wrap-ansi's last pass: an SGR color open at a row break is closed before it and reopened after it.
fn reopen(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut code: Option<u32> = None;
    for (index, &c) in chars.iter().enumerate() {
        out.push(c);
        if c == '\x1b' && chars.get(index + 1) == Some(&'[') {
            let digits: String = chars[index + 2..].iter().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() && chars.get(index + 2 + digits.len()) == Some(&'m') {
                let value: u32 = digits.parse().unwrap_or(0);
                code = if value == 39 { None } else { Some(value) };
            }
        }
        let close = code.and_then(close_code);
        if chars.get(index + 1) == Some(&'\n') {
            if let Some(close) = close {
                out.push_str(&format!("\x1b[{close}m"));
            }
        } else if c == '\n' {
            if let (Some(open), Some(_)) = (code, close) {
                out.push_str(&format!("\x1b[{open}m"));
            }
        }
    }
    out
}

/// ansi-styles' close code of an open code.
fn close_code(open: u32) -> Option<u32> {
    match open {

        1 | 2 => Some(22),
        3 => Some(23),
        4 => Some(24),
        7 => Some(27),
        8 => Some(28),
        9 => Some(29),
        30..=37 | 90..=97 => Some(39),
        40..=47 | 100..=107 => Some(49),
        _ => None,
    }
}

// --- @clack/core's Prompt ---

/// A prompt's own part: its frame for the engine's state, and its answers to the engine's events.
trait Model {
    /// Whether the prompt keeps a text input (readline's line).
    fn tracked(&self) -> bool;
    fn frame(&self, c: &Clack, view: &View) -> String;
    /// `userInput`: the line changed (or a key was pressed).
    fn user_input(&mut self, _input: &str) {}
    fn cursor(&mut self, _action: Action) {}
    fn key(&mut self, _key: &Key) {}
    /// `y`/`n` (a confirmation answers at once).
    fn confirm(&mut self, _yes: bool) -> bool {
        false
    }
    fn validate(&self) -> Option<String> {
        None
    }
    fn finalize(&mut self) {}
    /// Keys readline must not insert (`_isActionKey`): a tab.
    fn action_key(&self, key: &Key) -> bool {
        key.char.as_deref() == Some("\t")
    }
}

/// What a frame is drawn from: the state, the error and the text input.
pub struct View {
    pub state: State,
    pub error: String,
    pub input: String,
    /// The text cursor, in characters.
    pub at: usize,
    pub rows: usize,
    pub columns: usize,
}

impl View {
    /// `userInputWithCursor` of the text prompts: the character under the cursor inverted, a block at the end.
    fn with_cursor(&self, c: &Clack) -> String {
        let chars: Vec<char> = self.input.chars().collect();
        if self.at >= chars.len() {
            return format!("{}█", self.input);
        }
        let before: String = chars[..self.at].iter().collect();
        let after: String = chars[self.at + 1..].iter().collect();
        format!("{before}{}{after}", c.inverse(&chars[self.at].to_string()))
    }
}

/// Runs a prompt until it is submitted (`Ok`) or cancelled (`Err(Cancelled)`).
async fn run<M: Model>(c: &Clack, model: &mut M, initial: Option<&str>) -> Result<()> {
    let mut keys = Keys::listen();
    let (columns, rows) = size();
    let mut view = View { state: State::Initial, error: String::new(), input: String::new(), at: 0, rows, columns };
    let mut previous = String::new();
    let tracked = model.tracked();
    if let Some(initial) = initial.filter(|_| tracked) {
        view.input = initial.to_owned();
        view.at = initial.chars().count();
        model.user_input(initial);
    }
    draw(c, model, &mut view, &mut previous);
    loop {
        let Some(key) = keys.next().await else {
            view.state = State::Cancel;
            draw(c, model, &mut view, &mut previous);
            stdout("\n\x1b[?25h");
            return Err(cancelled());
        };
        let name = key.name.unwrap_or("");
        if tracked && name != "return" {
            if !(key.name.is_some() && model.action_key(&key)) {
                edit(&mut view, &key);
            }
            let input = view.input.clone();
            model.user_input(&input);
        }
        if view.state == State::Error {
            view.state = State::Active;
        }
        if let Some(action) = action(name, tracked) {
            model.cursor(action);
        }
        if let Some(char) = key.char.as_deref().map(str::to_lowercase).filter(|c| c == "y" || c == "n") {
            if model.confirm(char == "y") {
                view.state = State::Submit;
                draw(c, model, &mut view, &mut previous);
                stdout("\n\x1b[?25h");
                return Ok(());
            }
        }
        model.key(&key);
        if name == "return" {
            if let Some(problem) = model.validate() {
                view.error = problem;
                view.state = State::Error;
                view.at = view.input.chars().count();
            }
            if view.state != State::Error {
                view.state = State::Submit;
            }
        }
        if key.char.as_deref() == Some("\x03") || name == "escape" {
            view.state = State::Cancel;
        }
        if matches!(view.state, State::Submit | State::Cancel) {
            model.finalize();
        }
        draw(c, model, &mut view, &mut previous);
        match view.state {
            State::Submit => {
                stdout("\n\x1b[?25h");
                return Ok(());
            }
            State::Cancel => {
                stdout("\n\x1b[?25h");
                return Err(cancelled());
            }
            _ => {}
        }
    }
}

/// readline's line editing.
fn edit(view: &mut View, key: &Key) {
    let mut chars: Vec<char> = view.input.chars().collect();
    let at = view.at.min(chars.len());
    match (key.name, key.ctrl) {
        (Some("backspace"), _) | (Some("h"), true) => {
            if at > 0 {
                chars.remove(at - 1);
                view.at = at - 1;
            }
        }
        (Some("delete"), _) | (Some("d"), true) => {
            if at < chars.len() {
                chars.remove(at);
            }
        }
        (Some("left"), _) | (Some("b"), true) => view.at = at.saturating_sub(1),
        (Some("right"), _) | (Some("f"), true) => view.at = (at + 1).min(chars.len()),
        (Some("home"), _) | (Some("a"), true) => view.at = 0,
        (Some("end"), _) | (Some("e"), true) => view.at = chars.len(),
        (Some("u"), true) => {
            chars.drain(..at);
            view.at = 0;
        }
        (Some("k"), true) => chars.truncate(at),
        (_, true) => {}
        (Some("tab" | "escape" | "up" | "down" | "return" | "enter"), _) => {}
        _ => {
            if let Some(text) = key.char.as_deref().filter(|t| !t.chars().any(char::is_control)) {
                for (offset, c) in text.chars().enumerate() {
                    chars.insert(at + offset, c);
                }
                view.at = at + text.chars().count();
            }
        }
    }
    view.input = chars.into_iter().collect();
}

/// `Prompt.render`: the frame (wrapped) over the previous one; only the changed lines are redrawn.
fn draw<M: Model>(c: &Clack, model: &M, view: &mut View, previous: &mut String) {
    let frame = wrap(&model.frame(c, view), view.columns);
    if frame == *previous {
        return;
    }
    if view.state == State::Initial {
        stdout("\x1b[?25l");
    } else {
        let old: Vec<&str> = previous.split('\n').collect();
        let new: Vec<&str> = frame.split('\n').collect();
        let diff: Vec<usize> = (0..old.len().max(new.len())).filter(|i| old.get(*i) != new.get(*i)).collect();
        // Back to the frame's first line.
        let up = wrap(previous, view.columns).split('\n').count() - 1;
        stdout(&format!("\x1b[999D{}", up_down(-(up as isize))));
        if diff.len() == 1 {
            let line = diff[0];
            stdout(&format!("{}\x1b[2K\x1b[G{}", up_down(line as isize), new[line]));
            stdout(&up_down((new.len() - line - 1) as isize));
            *previous = frame;
            return;
        }
        if diff.len() > 1 {
            let line = diff[0];
            stdout(&format!("{}\x1b[J{}", up_down(line as isize), new[line..].join("\n")));
            *previous = frame;
            return;
        }
        stdout("\x1b[J");
    }
    stdout(&frame);
    if view.state == State::Initial {
        view.state = State::Active;
    }
    *previous = frame;
}

/// sisteransi's vertical `cursor.move`.
fn up_down(rows: isize) -> String {
    match rows {
        0 => String::new(),
        r if r < 0 => format!("\x1b[{}A", -r),
        r => format!("\x1b[{r}B"),
    }
}

fn title(c: &Clack, state: State, message: &str) -> String {
    format!("{}\n{}  {message}\n", c.gray(S_BAR), c.symbol(state))
}

/// `limitOptions`: a window of at most `max` options (the terminal's height less four), ellipses where it cuts.
fn limit<T>(options: &[T], cursor: usize, max: Option<usize>, rows: usize, c: &Clack, style: impl Fn(&T, bool) -> String) -> Vec<String> {
    let max_items = rows.saturating_sub(4).min(max.unwrap_or(usize::MAX).max(5));
    let mut window = 0usize;
    if cursor + 3 >= window + max_items {
        window = (cursor as isize - max_items as isize + 3).min(options.len() as isize - max_items as isize).max(0) as usize;
    } else if cursor < window + 2 {
        window = cursor.saturating_sub(2);
    }
    let top = max_items < options.len() && window > 0;
    let bottom = max_items < options.len() && window + max_items < options.len();
    let shown = &options[window.min(options.len())..(window + max_items).min(options.len())];
    shown
        .iter()
        .enumerate()
        .map(|(i, option)| {
            if (i == 0 && top) || (i == shown.len() - 1 && bottom) {
                c.dim("...")
            } else {
                style(option, i + window == cursor)
            }
        })
        .collect()
}

// --- the prompts ---

/// An option of a choice prompt.
#[derive(Clone)]
pub struct Choice {
    pub label: String,
    pub hint: Option<String>,
}

struct Text<'a> {
    message: &'a str,
    placeholder: Option<&'a str>,
    validate: &'a dyn Fn(Option<&str>) -> Option<String>,
    value: Option<String>,
}

impl Model for Text<'_> {
    fn tracked(&self) -> bool {
        true
    }
    fn user_input(&mut self, input: &str) {
        self.value = Some(input.to_owned());
    }
    fn validate(&self) -> Option<String> {
        (self.validate)(self.value.as_deref())
    }
    fn finalize(&mut self) {
        if self.value.is_none() {
            self.value = Some(String::new());
        }
    }
    fn frame(&self, c: &Clack, view: &View) -> String {
        let title = title(c, view.state, self.message);
        let placeholder = match self.placeholder.filter(|p| !p.is_empty()) {
            Some(placeholder) => {
                let first: String = placeholder.chars().take(1).collect();
                let rest: String = placeholder.chars().skip(1).collect();
                format!("{}{}", c.inverse(&first), c.dim(&rest))
            }
            None => c.inverse(&c.hidden("_")),
        };
        let input = if view.input.is_empty() {
            placeholder
        } else if view.state == State::Submit {
            view.input.clone()
        } else {
            view.with_cursor(c)
        };
        let value = self.value.clone().unwrap_or_default();
        match view.state {
            State::Error => format!(
                "{}\n{}  {input}\n{}  {}\n",
                title.trim(),
                c.yellow(S_BAR),
                c.yellow(S_BAR_END),
                c.yellow(&view.error)
            ),
            State::Submit => format!("{title}{}  {}", c.gray(S_BAR), c.dim(&value)),
            State::Cancel => format!(
                "{title}{}  {}{}",
                c.gray(S_BAR),
                c.strikethrough(&c.dim(&value)),
                if value.trim().is_empty() { String::new() } else { format!("\n{}", c.gray(S_BAR)) }
            ),
            _ => format!("{title}{}  {input}\n{}\n", c.cyan(S_BAR), c.cyan(S_BAR_END)),
        }
    }
}

/// `text`: a line of text, `None` for an empty answer the prompt accepts.
pub async fn text(
    c: &Clack,
    message: &str,
    placeholder: Option<&str>,
    initial: Option<&str>,
    validate: &dyn Fn(Option<&str>) -> Option<String>,
) -> Result<String> {
    let mut model = Text { message, placeholder, validate, value: None };
    run(c, &mut model, initial).await?;
    Ok(model.value.unwrap_or_default())
}

struct Password<'a> {
    message: &'a str,
    value: Option<String>,
}

impl Model for Password<'_> {
    fn tracked(&self) -> bool {
        true
    }
    fn user_input(&mut self, input: &str) {
        self.value = Some(input.to_owned());
    }
    fn validate(&self) -> Option<String> {
        self.value.as_deref().filter(|v| !v.is_empty()).is_none().then(|| "Required".to_owned())
    }
    fn frame(&self, c: &Clack, view: &View) -> String {
        let title = title(c, view.state, self.message);
        let masked = S_PASSWORD_MASK.repeat(view.input.chars().count());
        let input = match view.state {
            State::Submit | State::Cancel => masked.clone(),
            _ if view.at >= view.input.chars().count() => format!("{masked}{}", c.inverse(&c.hidden("_"))),
            _ => {
                let before = S_PASSWORD_MASK.repeat(view.at);
                let after = S_PASSWORD_MASK.repeat(view.input.chars().count() - view.at - 1);
                format!("{before}{}{after}", c.inverse(S_PASSWORD_MASK))
            }
        };
        match view.state {
            State::Error => format!(
                "{}\n{}  {masked}\n{}  {}\n",
                title.trim(),
                c.yellow(S_BAR),
                c.yellow(S_BAR_END),
                c.yellow(&view.error)
            ),
            State::Submit => format!("{title}{}  {}", c.gray(S_BAR), c.dim(&masked)),
            State::Cancel => format!(
                "{title}{}  {}{}",
                c.gray(S_BAR),
                c.strikethrough(&c.dim(&masked)),
                if masked.is_empty() { String::new() } else { format!("\n{}", c.gray(S_BAR)) }
            ),
            _ => format!("{title}{}  {input}\n{}\n", c.cyan(S_BAR), c.cyan(S_BAR_END)),
        }
    }
}

/// `password` with a required answer, masked.
pub async fn password(c: &Clack, message: &str) -> Result<String> {
    let mut model = Password { message, value: None };
    run(c, &mut model, None).await?;
    Ok(model.value.unwrap_or_default())
}

struct Select<'a> {
    message: &'a str,
    options: &'a [Choice],
    cursor: usize,
}

impl Model for Select<'_> {
    fn tracked(&self) -> bool {
        false
    }
    fn cursor(&mut self, action: Action) {
        let last = self.options.len().saturating_sub(1);
        match action {
            Action::Up | Action::Left => self.cursor = if self.cursor == 0 { last } else { self.cursor - 1 },
            Action::Down | Action::Right => self.cursor = if self.cursor == last { 0 } else { self.cursor + 1 },
            Action::Space => {}
        }
    }
    fn frame(&self, c: &Clack, view: &View) -> String {
        let title = title(c, view.state, self.message);
        let label = |option: &Choice| option.label.clone();
        match view.state {
            State::Submit => format!("{title}{}  {}", c.gray(S_BAR), c.dim(&label(&self.options[self.cursor]))),
            State::Cancel => format!(
                "{title}{}  {}\n{}",
                c.gray(S_BAR),
                c.strikethrough(&c.dim(&label(&self.options[self.cursor]))),
                c.gray(S_BAR)
            ),
            _ => {
                let lines = limit(self.options, self.cursor, None, view.rows, c, |option, active| {
                    if active {
                        let hint = option.hint.as_deref().map(|h| c.dim(&format!("({h})"))).unwrap_or_default();
                        format!("{} {} {hint}", c.green(S_RADIO_ACTIVE), option.label)
                    } else {
                        format!("{} {}", c.dim(S_RADIO_INACTIVE), c.dim(&option.label))
                    }
                });
                format!("{title}{}  {}\n{}\n", c.cyan(S_BAR), lines.join(&format!("\n{}  ", c.cyan(S_BAR))), c.cyan(S_BAR_END))
            }
        }
    }
}

/// `select`: one of the options (its index), starting on `initial`.
pub async fn select(c: &Clack, message: &str, options: &[Choice], initial: usize) -> Result<usize> {
    let mut model = Select { message, options, cursor: initial.min(options.len().saturating_sub(1)) };
    run(c, &mut model, None).await?;
    Ok(model.cursor)
}

struct Confirm<'a> {
    message: &'a str,
    value: bool,
}

impl Model for Confirm<'_> {
    fn tracked(&self) -> bool {
        false
    }
    fn cursor(&mut self, _action: Action) {
        self.value = !self.value;
    }
    fn confirm(&mut self, yes: bool) -> bool {
        self.value = yes;
        true
    }
    fn frame(&self, c: &Clack, view: &View) -> String {
        let title = title(c, view.state, self.message);
        let value = if self.value { "Yes" } else { "No" };
        match view.state {
            State::Submit => format!("{title}{}  {}", c.gray(S_BAR), c.dim(value)),
            State::Cancel => format!("{title}{}  {}\n{}", c.gray(S_BAR), c.strikethrough(&c.dim(value)), c.gray(S_BAR)),
            _ => {
                let choice = |selected: bool, label: &str| {
                    if selected {
                        format!("{} {label}", c.green(S_RADIO_ACTIVE))
                    } else {
                        format!("{} {}", c.dim(S_RADIO_INACTIVE), c.dim(label))
                    }
                };
                format!(
                    "{title}{}  {} {} {}\n{}\n",
                    c.cyan(S_BAR),
                    choice(self.value, "Yes"),
                    c.dim("/"),
                    choice(!self.value, "No"),
                    c.cyan(S_BAR_END)
                )
            }
        }
    }
}

/// `confirm`.
pub async fn confirm(c: &Clack, message: &str, initial: bool) -> Result<bool> {
    let mut model = Confirm { message, value: initial };
    run(c, &mut model, None).await?;
    Ok(model.value)
}

struct MultiSelect<'a> {
    message: &'a str,
    options: &'a [Choice],
    cursor: usize,
    selected: Vec<usize>,
    required: bool,
}

impl Model for MultiSelect<'_> {
    fn tracked(&self) -> bool {
        false
    }
    fn cursor(&mut self, action: Action) {
        let last = self.options.len().saturating_sub(1);
        match action {
            Action::Up | Action::Left => self.cursor = if self.cursor == 0 { last } else { self.cursor - 1 },
            Action::Down | Action::Right => self.cursor = if self.cursor == last { 0 } else { self.cursor + 1 },
            Action::Space => match self.selected.iter().position(|i| *i == self.cursor) {
                Some(index) => {
                    self.selected.remove(index);
                }
                None => self.selected.push(self.cursor),
            },
        }
    }
    fn key(&mut self, key: &Key) {
        if key.char.as_deref() == Some("a") {
            self.selected =
                if self.selected.len() == self.options.len() { vec![] } else { (0..self.options.len()).collect() };
        }
    }
    fn validate(&self) -> Option<String> {
        let c = Clack::new();
        (self.required && self.selected.is_empty()).then(|| {
            format!(
                "Please select at least one option.\n{}",
                c.reset(&c.dim(&format!(
                    "Press {} to select, {} to submit",
                    c.gray(&c.bg_white(&c.inverse(" space "))),
                    c.gray(&c.bg_white(&c.inverse(" enter ")))
                )))
            )
        })
    }
    fn frame(&self, c: &Clack, view: &View) -> String {
        let title = title(c, view.state, self.message);
        let style = |index: usize, active: bool| {
            let option = &self.options[index];
            let hint = option.hint.as_deref().map(|h| c.dim(&format!("({h})"))).unwrap_or_default();
            let selected = self.selected.contains(&index);
            match (active, selected) {
                (true, true) => format!("{} {} {hint}", c.green(S_CHECKBOX_SELECTED), option.label),
                (false, true) => format!("{} {} {hint}", c.green(S_CHECKBOX_SELECTED), c.dim(&option.label)),
                (true, false) => format!("{} {} {hint}", c.cyan(S_CHECKBOX_ACTIVE), option.label),
                (false, false) => format!("{} {}", c.dim(S_CHECKBOX_INACTIVE), c.dim(&option.label)),
            }
        };
        let in_order: Vec<usize> = (0..self.options.len()).filter(|i| self.selected.contains(i)).collect();
        let indexes: Vec<usize> = (0..self.options.len()).collect();
        let window = |bar: String| limit(&indexes, self.cursor, None, view.rows, c, |i, active| style(*i, active)).join(&format!("\n{bar}  "));
        match view.state {
            State::Submit => {
                let labels: Vec<String> = in_order.iter().map(|i| c.dim(&self.options[*i].label)).collect();
                let shown = if labels.is_empty() { c.dim("none") } else { labels.join(&c.dim(", ")) };
                format!("{title}{}  {shown}", c.gray(S_BAR))
            }
            State::Cancel => {
                let labels: Vec<String> =
                    in_order.iter().map(|i| c.strikethrough(&c.dim(&self.options[*i].label))).collect();
                let label = labels.join(&c.dim(", "));
                let shown = if label.trim().is_empty() { String::new() } else { format!("{label}\n{}", c.gray(S_BAR)) };
                format!("{title}{}  {shown}", c.gray(S_BAR))
            }
            State::Error => {
                let footer: Vec<String> = view
                    .error
                    .split('\n')
                    .enumerate()
                    .map(|(i, line)| if i == 0 { format!("{}  {}", c.yellow(S_BAR_END), c.yellow(line)) } else { format!("   {line}") })
                    .collect();
                format!("{title}{}  {}\n{}\n", c.yellow(S_BAR), window(c.yellow(S_BAR)), footer.join("\n"))
            }
            _ => format!("{title}{}  {}\n{}\n", c.cyan(S_BAR), window(c.cyan(S_BAR)), c.cyan(S_BAR_END)),
        }
    }
}

/// `multiselect`: the indexes of the options chosen, in the options' order.
pub async fn multiselect(c: &Clack, message: &str, options: &[Choice], initial: Vec<usize>, required: bool) -> Result<Vec<usize>> {
    let mut model = MultiSelect { message, options, cursor: 0, selected: initial, required };
    run(c, &mut model, None).await?;
    Ok((0..options.len()).filter(|i| model.selected.contains(i)).collect())
}

/// @clack/core's `AutocompletePrompt` state, shared by `autocomplete` and the integration picker.
struct Filtered<'a> {
    options: &'a [Choice],
    /// Whether an option matches a search.
    matches: &'a dyn Fn(&str, usize) -> bool,
    filtered: Vec<usize>,
    cursor: usize,
    focused: Option<usize>,
    selected: Option<usize>,
    navigating: bool,
    last_input: String,
}

impl<'a> Filtered<'a> {
    fn new(options: &'a [Choice], matches: &'a dyn Fn(&str, usize) -> bool) -> Self {
        Filtered {
            options,
            matches,
            filtered: (0..options.len()).collect(),
            cursor: 0,
            focused: (!options.is_empty()).then_some(0),
            selected: (!options.is_empty()).then_some(0),
            navigating: false,
            last_input: String::new(),
        }
    }

    fn key(&mut self, key: &Key) {
        match key.name {
            Some(name @ ("up" | "down")) => {
                let moved = self.cursor as isize + if name == "up" { -1 } else { 1 };
                self.cursor = moved.min(self.filtered.len() as isize - 1).max(0) as usize;
                self.focused = self.filtered.get(self.cursor).copied();
                self.selected = self.focused;
                self.navigating = true;
            }
            Some("return") => {}
            _ => {
                if self.focused.is_some() {
                    self.selected = self.focused;
                }
            }
        }
    }

    fn user_input(&mut self, input: &str) {
        if input == self.last_input {
            return;
        }
        self.last_input = input.to_owned();
        self.filtered = (0..self.options.len()).filter(|i| input.is_empty() || (self.matches)(input, *i)).collect();
        self.cursor = self.focused.and_then(|f| self.filtered.iter().position(|i| *i == f)).unwrap_or(0);
        self.focused = self.filtered.get(self.cursor).copied();
        self.selected = if self.filtered.is_empty() { None } else { self.focused };
    }

    fn search(&self, c: &Clack, view: &View) -> String {
        if self.navigating {
            return c.dim(&view.input);
        }
        if view.input.is_empty() {
            return c.inverse(&c.hidden("_"));
        }
        view.with_cursor(c)
    }
}

struct Autocomplete<'a> {
    message: &'a str,
    max_items: Option<usize>,
    state: Filtered<'a>,
}

impl Model for Autocomplete<'_> {
    fn tracked(&self) -> bool {
        true
    }
    fn user_input(&mut self, input: &str) {
        self.state.user_input(input);
    }
    fn key(&mut self, key: &Key) {
        self.state.key(key);
    }
    fn frame(&self, c: &Clack, view: &View) -> String {
        let title = title(c, view.state, self.message);
        let state = &self.state;
        match view.state {
            State::Submit => {
                let label = state.selected.map(|i| state.options[i].label.clone()).unwrap_or_default();
                format!("{title}{}  {}", c.gray(S_BAR), c.dim(&label))
            }
            State::Cancel => format!("{title}{}  {}", c.gray(S_BAR), c.strikethrough(&c.dim(&view.input))),
            _ => {
                let matches = if state.filtered.len() != state.options.len() {
                    let count = state.filtered.len();
                    c.dim(&format!(" ({count} match{})", if count == 1 { "" } else { "es" }))
                } else {
                    String::new()
                };
                let options = limit(&state.filtered, state.cursor, self.max_items, view.rows, c, |index, active| {
                    let option = &state.options[*index];
                    let hint = match &option.hint {
                        Some(hint) if Some(*index) == state.focused => c.dim(&format!(" ({hint})")),
                        _ => String::new(),
                    };
                    if active {
                        format!("{} {}{hint}", c.green(S_RADIO_ACTIVE), option.label)
                    } else {
                        format!("{} {}{hint}", c.dim(S_RADIO_INACTIVE), c.dim(&option.label))
                    }
                });
                let mut lines = vec![title, format!("{}  {} {}{matches}", c.cyan(S_BAR), c.dim("Search:"), state.search(c, view))];
                if state.filtered.is_empty() && !view.input.is_empty() {
                    lines.push(format!("{}  {}", c.cyan(S_BAR), c.yellow("No matches found")));
                }
                if view.state == State::Error {
                    lines.push(format!("{}  {}", c.yellow(S_BAR), c.yellow(&view.error)));
                }
                lines.extend(options.into_iter().map(|option| format!("{}  {option}", c.cyan(S_BAR))));
                let instructions =
                    [format!("{} to select", c.dim("↑/↓")), format!("{} confirm", c.dim("Enter:")), format!("{} to search", c.dim("Type:"))];
                lines.push(format!("{}  {}", c.cyan(S_BAR), c.dim(&instructions.join(" • "))));
                lines.push(c.cyan(S_BAR_END));
                lines.join("\n")
            }
        }
    }
}

/// `autocomplete`: an option found by typing (its label, hint or value), or by moving to it.
pub async fn autocomplete(c: &Clack, message: &str, options: &[Choice], values: &[String], max_items: usize) -> Result<usize> {
    let matches = |search: &str, index: usize| {
        let term = search.to_lowercase();
        let option = &options[index];
        option.label.to_lowercase().contains(&term)
            || option.hint.as_deref().unwrap_or("").to_lowercase().contains(&term)
            || values[index].to_lowercase().contains(&term)
    };
    let mut model = Autocomplete { message, max_items: Some(max_items), state: Filtered::new(options, &matches) };
    run(c, &mut model, None).await?;
    // A submitted autocomplete without a match resolves with nothing.
    model.state.selected.ok_or_else(|| Fail::error("No option selected"))
}

/// An integration the picker offers (`IntegrationChoice`).
pub struct IntegrationChoice {
    pub value: String,
    pub label: String,
    pub category: &'static str,
    pub connected: bool,
}

struct Picker<'a> {
    choices: &'a [IntegrationChoice],
    state: Filtered<'a>,
}

impl Model for Picker<'_> {
    fn tracked(&self) -> bool {
        true
    }
    fn user_input(&mut self, input: &str) {
        self.state.user_input(input);
    }
    fn key(&mut self, key: &Key) {
        self.state.key(key);
    }
    fn validate(&self) -> Option<String> {
        self.state.selected.is_none().then(|| "Select an integration".to_owned())
    }
    fn frame(&self, c: &Clack, view: &View) -> String {
        let title = format!("{}\n{}  Select integration", c.gray(S_BAR), c.symbol(view.state));
        let state = &self.state;
        if view.state == State::Submit {
            let label = state.selected.map(|i| self.choices[i].label.as_str()).unwrap_or("");
            return format!("{title}\n{}  {}", c.gray(S_BAR), c.dim(label));
        }
        if view.state == State::Cancel {
            return format!("{title}\n{}  {}", c.gray(S_BAR), c.strikethrough(&c.dim(&view.input)));
        }
        // Leave room for the category headings as well as Clack's title and footer.
        let error = usize::from(view.state == State::Error);
        let max_items = (view.rows as isize - 14 - error as isize).max(2).min(8) as usize;
        let compact = view.rows < 18;
        let filtered = &state.filtered;
        let start = (state.cursor as isize - (max_items as isize - 1).min(2))
            .max(0)
            .min((filtered.len() as isize - max_items as isize).max(0)) as usize;
        let visible = &filtered[start.min(filtered.len())..(start + max_items).min(filtered.len())];
        let mut rows = vec![];
        for (index, choice) in visible.iter().map(|i| &self.choices[*i]).enumerate() {
            if index == 0 || self.choices[visible[index - 1]].category != choice.category {
                if !compact {
                    rows.push(format!("{}  ", c.cyan(S_BAR)));
                }
                rows.push(format!("{}  {}", c.cyan(S_BAR), c.bold(choice.category)));
            }
            let active = start + index == state.cursor;
            rows.push(format!(
                "{}  {} {}{}",
                c.cyan(S_BAR),
                if active { c.green(S_RADIO_ACTIVE) } else { c.dim(S_RADIO_INACTIVE) },
                if active { choice.label.clone() } else { c.dim(&choice.label) },
                if choice.connected { format!(" {}", c.green("✓")) } else { String::new() },
            ));
        }
        let search = if state.navigating { c.dim(&view.input) } else { state.search(c, view) };
        let mut lines = vec![title, format!("{}  {} {search}", c.cyan(S_BAR), c.dim("Search:"))];
        if visible.is_empty() && !view.input.is_empty() {
            lines.push(format!("{}  {}", c.cyan(S_BAR), c.yellow("No integrations found")));
        }
        if view.state == State::Error && !visible.is_empty() {
            lines.push(format!("{}  {}", c.yellow(S_BAR), c.yellow(&view.error)));
        }
        if start > 0 {
            lines.push(format!("{}  {}", c.cyan(S_BAR), c.dim("…")));
        }
        lines.extend(rows);
        if start + max_items < filtered.len() {
            lines.push(format!("{}  {}", c.cyan(S_BAR), c.dim("…")));
        }
        let help = if view.columns < 50 { "↑/↓ navigate • Enter select" } else { "↑/↓ to select • Enter: confirm • Type: to search" };
        lines.push(format!("{}  {}", c.cyan(S_BAR), c.dim(help)));
        lines.push(c.cyan(S_BAR_END));
        lines.join("\n")
    }
}

/// `selectIntegration`: an integration found by typing (its name, ID or category) or by moving to it.
pub async fn select_integration(c: &Clack, choices: &[IntegrationChoice]) -> Result<String> {
    let options: Vec<Choice> = choices.iter().map(|choice| Choice { label: choice.label.clone(), hint: None }).collect();
    let matches = |search: &str, index: usize| {
        let term = search.to_lowercase();
        let choice = &choices[index];
        [choice.label.as_str(), choice.value.as_str(), choice.category].iter().any(|v| v.to_lowercase().contains(&term))
    };
    let mut model = Picker { choices, state: Filtered::new(&options, &matches) };
    run(c, &mut model, None).await?;
    model.state.selected.map(|i| choices[i].value.clone()).ok_or_else(|| Fail::error("No integration selected"))
}

/// `openUrl` (packages/util/src/open.ts, the `open` package): an http(s) link opens in the browser, detached; a
/// failure is ignored (the link is on the screen).
pub fn open_url(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if !crate::url::is_http(url) {
        return;
    }
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
