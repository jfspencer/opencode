//! @clack/prompts 1.0.0-alpha.1 (with @clack/core 1.0.0-alpha.1 and picocolors) as the CLI's handlers use it:
//! `intro`, `outro`, `cancel`, `log.*`, `spinner`, and the `confirm` and `autocomplete` prompts on a terminal.
//!
//! Prompts redraw their whole frame (cursor up over the previous frame, erase down, the new frame) where clack
//! rewrites only the lines that changed: a terminal shows the same screen after either. Frames are not wrapped at
//! the terminal's width (clack hard-wraps them with wrap-ansi), and the spinner draws no animation frames (it
//! settles on the same line; the frames only appear when an operation outlasts a tick).

// Not every handler uses every prompt yet.
#![allow(dead_code)]

use super::out::{stderr, stdout};

/// A prompt the user cancelled (Ctrl+C or Escape): clack's cancel symbol.
#[derive(Debug)]
pub struct Cancelled;

#[derive(Clone, Copy)]
pub struct Clack {
    /// picocolors' `isColorSupported`.
    pub color: bool,
    /// is-unicode-supported: every terminal but the Linux console.
    unicode: bool,
    /// The prompt's `output` is `process.stderr`.
    to_stderr: bool,
}

/// One option of `autocomplete`.
pub struct Choice {
    pub label: String,
    pub value: String,
    pub hint: Option<String>,
}

impl Clack {
    pub fn new() -> Self {
        let color = std::env::var_os("NO_COLOR").is_none()
            && (std::env::var_os("FORCE_COLOR").is_some()
                || (super::stdout_tty() && std::env::var("TERM").as_deref() != Ok("dumb"))
                || std::env::var_os("CI").is_some());
        let unicode = std::env::var("TERM").as_deref() != Ok("linux");
        Clack { color, unicode, to_stderr: false }
    }

    /// The same, writing to stderr (`{ output: process.stderr }`).
    pub fn stderr(self) -> Self {
        Clack { to_stderr: true, ..self }
    }

    pub fn write(&self, text: &str) {
        if self.to_stderr { stderr(text) } else { stdout(text) }
    }

    fn symbol<'a>(&self, unicode: &'a str, fallback: &'a str) -> &'a str {
        if self.unicode { unicode } else { fallback }
    }

    /// picocolors' `formatter(open, close, replace)`: a nested `close` reopens the style.
    fn paint(&self, open: &str, close: &str, replace: &str, text: &str) -> String {
        if !self.color {
            return text.to_owned();
        }
        let (open, close, replace) = (format!("\x1b[{open}m"), format!("\x1b[{close}m"), format!("\x1b[{replace}m"));
        // `string.indexOf(close, open.length)`: the search starts `open.length` UTF-16 units into the text.
        let mut units = 0;
        let start = text
            .char_indices()
            .find(|(_, c)| {
                let reached = units >= open.len();
                units += c.len_utf16();
                reached
            })
            .map_or(text.len(), |(at, _)| at);
        let (head, tail) = text.split_at(start);
        let body = if tail.contains(&close) { format!("{head}{}", tail.replace(&close, &replace)) } else { text.to_owned() };
        format!("{open}{body}{close}")
    }
    pub fn gray(&self, text: &str) -> String {
        self.paint("90", "39", "90", text)
    }
    pub fn red(&self, text: &str) -> String {
        self.paint("31", "39", "31", text)
    }
    pub fn green(&self, text: &str) -> String {
        self.paint("32", "39", "32", text)
    }
    pub fn yellow(&self, text: &str) -> String {
        self.paint("33", "39", "33", text)
    }
    pub fn blue(&self, text: &str) -> String {
        self.paint("34", "39", "34", text)
    }
    pub fn magenta(&self, text: &str) -> String {
        self.paint("35", "39", "35", text)
    }
    pub fn cyan(&self, text: &str) -> String {
        self.paint("36", "39", "36", text)
    }
    pub fn dim(&self, text: &str) -> String {
        self.paint("2", "22", "22m\x1b[2", text)
    }
    pub fn inverse(&self, text: &str) -> String {
        self.paint("7", "27", "7", text)
    }
    pub fn hidden(&self, text: &str) -> String {
        self.paint("8", "28", "8", text)
    }
    pub fn strikethrough(&self, text: &str) -> String {
        self.paint("9", "29", "9", text)
    }

    fn bar(&self) -> &'static str {
        if self.unicode { "│" } else { "|" }
    }
    fn bar_start(&self) -> &'static str {
        if self.unicode { "┌" } else { "T" }
    }
    fn bar_end(&self) -> &'static str {
        if self.unicode { "└" } else { "—" }
    }

    /// The prompt state symbol (`symbol(state)`).
    fn state(&self, state: State) -> String {
        match state {
            State::Active => self.cyan(self.symbol("◆", "*")),
            State::Cancel => self.red(self.symbol("■", "x")),
            State::Submit => self.green(self.symbol("◇", "o")),
        }
    }

    pub fn intro(&self, title: &str) {
        self.write(&format!("{}  {title}\n", self.gray(self.bar_start())));
    }

    pub fn outro(&self, message: &str) {
        self.write(&format!("{}\n{}  {message}\n\n", self.gray(self.bar()), self.gray(self.bar_end())));
    }

    pub fn cancel(&self, message: &str) {
        self.write(&format!("{}  {}\n\n", self.gray(self.bar_end()), self.red(message)));
    }

    /// `log.message(message, { symbol })`.
    pub fn message_with(&self, message: &str, symbol: &str) {
        let bar = self.gray(self.bar());
        let mut parts = vec![bar.clone()];
        let mut lines = message.split('\n');
        let first = lines.next().unwrap_or("");
        parts.push(if first.is_empty() { symbol.to_owned() } else { format!("{symbol}  {first}") });
        for line in lines {
            parts.push(if line.is_empty() { bar.clone() } else { format!("{bar}  {line}") });
        }
        self.write(&format!("{}\n", parts.join("\n")));
    }
    pub fn message(&self, message: &str) {
        self.message_with(message, &self.gray(self.bar()));
    }
    pub fn info(&self, message: &str) {
        self.message_with(message, &self.blue(self.symbol("●", "•")));
    }
    pub fn success(&self, message: &str) {
        self.message_with(message, &self.green(self.symbol("◆", "*")));
    }
    pub fn step(&self, message: &str) {
        self.message_with(message, &self.green(self.symbol("◇", "o")));
    }
    pub fn warn(&self, message: &str) {
        self.message_with(message, &self.yellow(self.symbol("▲", "!")));
    }
    pub fn error(&self, message: &str) {
        self.message_with(message, &self.red(self.symbol("■", "x")));
    }

    /// `spinner().start(message)`.
    pub fn spinner_start(&self) {
        self.write(&format!("\x1b[?25l{}\n", self.gray(self.bar())));
    }

    /// `spinner().stop(message, code)`.
    pub fn spinner_stop(&self, message: &str, code: u8) {
        let step = match code {
            0 => self.green(self.symbol("◇", "o")),
            1 => self.red(self.symbol("■", "x")),
            _ => self.red(self.symbol("▲", "x")),
        };
        self.write(&format!("{step}  {message}\n\x1b[?25h"));
    }

    /// `confirm({ message, initialValue })` with the default labels.
    pub fn confirm(&self, message: &str, initial: bool) -> Result<bool, Cancelled> {
        let (active, inactive) = ("Yes", "No");
        let mut value = initial;
        let frame = |state: State, value: bool| {
            let title = format!("{}\n{}  {message}\n", self.gray(self.bar()), self.state(state));
            let label = if value { active } else { inactive };
            match state {
                State::Submit => format!("{title}{}  {}", self.gray(self.bar()), self.dim(label)),
                State::Cancel => format!(
                    "{title}{}  {}\n{}",
                    self.gray(self.bar()),
                    self.strikethrough(&self.dim(label)),
                    self.gray(self.bar())
                ),
                State::Active => {
                    let radio = |on: bool, text: &str| {
                        if on {
                            format!("{} {text}", self.green(self.symbol("●", ">")))
                        } else {
                            format!("{} {}", self.dim(self.symbol("○", " ")), self.dim(text))
                        }
                    };
                    format!(
                        "{title}{}  {} {} {}\n{}\n",
                        self.cyan(self.bar()),
                        radio(value, active),
                        self.dim("/"),
                        radio(!value, inactive),
                        self.cyan(self.bar_end())
                    )
                }
            }
        };
        let mut screen = Screen::new(*self);
        screen.draw(&frame(State::Active, value));
        let terminal = Raw::enable();
        loop {
            match read_key() {
                Key::Return => {
                    screen.finish(&frame(State::Submit, value));
                    drop(terminal);
                    return Ok(value);
                }
                Key::Char(c) if c.eq_ignore_ascii_case(&'y') || c.eq_ignore_ascii_case(&'n') => {
                    value = c.eq_ignore_ascii_case(&'y');
                    screen.finish(&frame(State::Submit, value));
                    drop(terminal);
                    return Ok(value);
                }
                Key::Cancel | Key::Closed => {
                    screen.finish(&frame(State::Cancel, value));
                    drop(terminal);
                    return Err(Cancelled);
                }
                Key::Up | Key::Down | Key::Left | Key::Right | Key::Space => value = !value,
                Key::Char('h' | 'j' | 'k' | 'l') => value = !value,
                _ => {}
            }
            screen.draw(&frame(State::Active, value));
        }
    }

    /// `autocomplete({ message, options, maxItems })`: type to filter (label, hint and value), arrows to move.
    pub fn autocomplete(&self, message: &str, options: &[Choice], max_items: usize) -> Result<String, Cancelled> {
        let mut prompt = Autocomplete::new(options);
        let mut screen = Screen::new(*self);
        screen.draw(&prompt.frame(self, message, State::Active, max_items));
        let terminal = Raw::enable();
        loop {
            let key = read_key();
            if !matches!(key, Key::Return) {
                prompt.edit(&key);
            }
            match key {
                Key::Up | Key::Down => prompt.navigate(matches!(key, Key::Up)),
                Key::Return => {
                    screen.finish(&prompt.frame(self, message, State::Submit, max_items));
                    drop(terminal);
                    return Ok(prompt.selected.clone().unwrap_or_default());
                }
                Key::Cancel | Key::Closed => {
                    screen.finish(&prompt.frame(self, message, State::Cancel, max_items));
                    drop(terminal);
                    return Err(Cancelled);
                }
                _ => {
                    if let Some(focused) = prompt.focused() {
                        prompt.selected = Some(focused);
                    }
                }
            }
            screen.draw(&prompt.frame(self, message, State::Active, max_items));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Active,
    Submit,
    Cancel,
}

/// A prompt's frames on the terminal: each one replaces the previous one.
struct Screen {
    clack: Clack,
    /// Line breaks in the frame on screen.
    previous: Option<usize>,
}

impl Screen {
    fn new(clack: Clack) -> Self {
        Screen { clack, previous: None }
    }

    fn draw(&mut self, frame: &str) {
        let mut out = String::new();
        match self.previous {
            None => out.push_str("\x1b[?25l"),
            Some(lines) => {
                out.push_str("\x1b[999D");
                if lines > 0 {
                    out.push_str(&format!("\x1b[{lines}A"));
                }
                out.push_str("\x1b[J");
            }
        }
        out.push_str(frame);
        self.clack.write(&out);
        self.previous = Some(frame.matches('\n').count());
    }

    /// The final frame, the line break `close()` writes, and the cursor shown again.
    fn finish(&mut self, frame: &str) {
        self.draw(frame);
        self.clack.write("\n\x1b[?25h");
    }
}

/// The state of `AutocompletePrompt` (single selection) and the line its readline edits.
struct Autocomplete<'a> {
    options: &'a [Choice],
    filtered: Vec<usize>,
    cursor: usize,
    navigating: bool,
    selected: Option<String>,
    focused_value: Option<String>,
    input: Vec<char>,
    caret: usize,
}

impl<'a> Autocomplete<'a> {
    fn new(options: &'a [Choice]) -> Self {
        let first = options.first().map(|o| o.value.clone());
        Autocomplete {
            options,
            filtered: (0..options.len()).collect(),
            cursor: 0,
            navigating: false,
            selected: first.clone(),
            focused_value: first,
            input: vec![],
            caret: 0,
        }
    }

    fn focused(&self) -> Option<String> {
        self.focused_value.clone()
    }

    fn user_input(&self) -> String {
        self.input.iter().collect()
    }

    /// readline's line editing, then `#onUserInputChanged` when the line changed.
    fn edit(&mut self, key: &Key) {
        let before = self.user_input();
        match key {
            Key::Char(c) => {
                self.input.insert(self.caret, *c);
                self.caret += 1;
            }
            Key::Space => {
                self.input.insert(self.caret, ' ');
                self.caret += 1;
            }
            Key::Backspace if self.caret > 0 => {
                self.caret -= 1;
                self.input.remove(self.caret);
            }
            Key::Delete if self.caret < self.input.len() => {
                self.input.remove(self.caret);
            }
            Key::Left => self.caret = self.caret.saturating_sub(1),
            Key::Right => self.caret = (self.caret + 1).min(self.input.len()),
            Key::Home => self.caret = 0,
            Key::End => self.caret = self.input.len(),
            _ => {}
        }
        let value = self.user_input();
        if value == before {
            return;
        }
        // Typing leaves navigation mode only through the filter's own reset of the cursor.
        let term = value.to_lowercase();
        self.filtered = (0..self.options.len())
            .filter(|&index| {
                let option = &self.options[index];
                term.is_empty()
                    || option.label.to_lowercase().contains(&term)
                    || option.hint.as_deref().unwrap_or("").to_lowercase().contains(&term)
                    || option.value.to_lowercase().contains(&term)
            })
            .collect();
        self.cursor = match &self.focused_value {
            Some(focused) => self.filtered.iter().position(|&i| &self.options[i].value == focused).unwrap_or(0),
            None => 0,
        };
        self.focused_value = self.filtered.get(self.cursor).map(|&i| self.options[i].value.clone());
        self.selected = self.focused_value.clone();
    }

    fn navigate(&mut self, up: bool) {
        let last = self.filtered.len().saturating_sub(1) as i64;
        let next = (self.cursor as i64 + if up { -1 } else { 1 }).min(last).max(0);
        self.cursor = next as usize;
        self.focused_value = self.filtered.get(self.cursor).map(|&i| self.options[i].value.clone());
        self.selected = self.focused_value.clone();
        self.navigating = true;
    }

    fn frame(&self, clack: &Clack, message: &str, state: State, max_items: usize) -> String {
        let title = format!("{}\n{}  {message}\n", clack.gray(clack.bar()), clack.state(state));
        let user_input = self.user_input();
        match state {
            State::Submit => {
                let label = self
                    .options
                    .iter()
                    .filter(|o| self.selected.as_deref() == Some(o.value.as_str()))
                    .map(|o| o.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{title}{}  {}", clack.gray(clack.bar()), clack.dim(&label))
            }
            State::Cancel => format!("{title}{}  {}", clack.gray(clack.bar()), clack.strikethrough(&clack.dim(&user_input))),
            State::Active => {
                let search = if self.navigating {
                    clack.dim(&user_input)
                } else if user_input.is_empty() {
                    clack.inverse(&clack.hidden("_"))
                } else if self.caret >= self.input.len() {
                    format!("{user_input}█")
                } else {
                    let before: String = self.input[..self.caret].iter().collect();
                    let at = self.input[self.caret].to_string();
                    let after: String = self.input[self.caret + 1..].iter().collect();
                    format!("{before}{}{after}", clack.inverse(&at))
                };
                let matches = if self.filtered.len() != self.options.len() {
                    let n = self.filtered.len();
                    clack.dim(&format!(" ({n} match{})", if n == 1 { "" } else { "es" }))
                } else {
                    String::new()
                };
                let mut lines = vec![title, format!("{}  {} {search}{matches}", clack.cyan(clack.bar()), clack.dim("Search:"))];
                if self.filtered.is_empty() && !user_input.is_empty() {
                    lines.push(format!("{}  {}", clack.cyan(clack.bar()), clack.yellow("No matches found")));
                }
                for option in self.limited(clack, max_items) {
                    lines.push(format!("{}  {option}", clack.cyan(clack.bar())));
                }
                let instructions = [
                    format!("{} to select", clack.dim("↑/↓")),
                    format!("{} confirm", clack.dim("Enter:")),
                    format!("{} to search", clack.dim("Type:")),
                ]
                .join(" • ");
                lines.push(format!("{}  {}", clack.cyan(clack.bar()), clack.dim(&instructions)));
                lines.push(clack.cyan(clack.bar_end()));
                lines.join("\n")
            }
        }
    }

    /// `limitOptions` on a terminal of `rows` rows (the output's), at most `maxItems` (at least 5).
    fn limited(&self, clack: &Clack, max_items: usize) -> Vec<String> {
        if self.filtered.is_empty() {
            return vec![];
        }
        let rows = terminal_rows().unwrap_or(10);
        let max = rows.saturating_sub(4).min(max_items.max(5));
        let count = self.filtered.len();
        let mut window = 0usize;
        if self.cursor + 3 >= window + max {
            window = (self.cursor as i64 - max as i64 + 3).min(count as i64 - max as i64).max(0) as usize;
        } else if self.cursor < window + 2 {
            window = self.cursor.saturating_sub(2);
        }
        let top = max < count && window > 0;
        let bottom = max < count && window + max < count;
        let shown: Vec<usize> = self.filtered.iter().copied().skip(window).take(max).collect();
        shown
            .iter()
            .enumerate()
            .map(|(i, &index)| {
                if (i == 0 && top) || (i == shown.len() - 1 && bottom) {
                    return clack.dim("...");
                }
                let option = &self.options[index];
                let active = i + window == self.cursor;
                let hint = match &option.hint {
                    Some(hint) if self.focused_value.as_deref() == Some(option.value.as_str()) => clack.dim(&format!(" ({hint})")),
                    _ => String::new(),
                };
                if active {
                    format!("{} {}{hint}", clack.green(clack.symbol("●", ">")), option.label)
                } else {
                    format!("{} {}{hint}", clack.dim(clack.symbol("○", " ")), clack.dim(&option.label))
                }
            })
            .collect()
    }
}

/// The terminal's rows (`output.rows`).
fn terminal_rows() -> Option<usize> {
    for fd in [2, 1, 0] {
        // SAFETY: TIOCGWINSZ fills the zeroed struct.
        unsafe {
            let mut size: libc::winsize = std::mem::zeroed();
            if libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_row > 0 {
                return Some(size.ws_row as usize);
            }
        }
    }
    None
}

/// A key as readline's keypress decoder names it.
#[derive(Debug)]
enum Key {
    Char(char),
    Space,
    Return,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Tab,
    /// Ctrl+C or Escape.
    Cancel,
    /// End of input (the terminal closed).
    Closed,
    Other,
}

fn read_byte(timeout_ms: i32) -> Option<u8> {
    let mut poll = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
    // SAFETY: one pollfd for stdin.
    if timeout_ms >= 0 && unsafe { libc::poll(&mut poll, 1, timeout_ms) } <= 0 {
        return None;
    }
    let mut byte = [0u8; 1];
    // SAFETY: a one-byte read into the buffer.
    let n = unsafe { libc::read(0, byte.as_mut_ptr().cast(), 1) };
    (n == 1).then_some(byte[0])
}

/// One keypress from stdin (raw mode): escape sequences within 50 ms (readline's `escapeCodeTimeout`).
fn read_key() -> Key {
    let Some(byte) = read_byte(-1) else { return Key::Closed };
    match byte {
        b'\r' | b'\n' => Key::Return,
        0x03 => Key::Cancel,
        0x7f | 0x08 => Key::Backspace,
        b'\t' => Key::Tab,
        b' ' => Key::Space,
        0x1b => match read_byte(50) {
            None => Key::Cancel,
            Some(b'[') | Some(b'O') => {
                let mut sequence = vec![];
                while let Some(next) = read_byte(50) {
                    sequence.push(next);
                    if next.is_ascii_alphabetic() || next == b'~' {
                        break;
                    }
                }
                match sequence.as_slice() {
                    [b'A'] => Key::Up,
                    [b'B'] => Key::Down,
                    [b'C'] => Key::Right,
                    [b'D'] => Key::Left,
                    [b'H'] | [b'1', b'~'] => Key::Home,
                    [b'F'] | [b'4', b'~'] => Key::End,
                    [b'3', b'~'] => Key::Delete,
                    _ => Key::Other,
                }
            }
            Some(_) => Key::Other,
        },
        byte if byte >= 0x20 => {
            // A UTF-8 character: its continuation bytes follow.
            let width = match byte {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => 1,
            };
            let mut bytes = vec![byte];
            while bytes.len() < width {
                match read_byte(50) {
                    Some(next) => bytes.push(next),
                    None => break,
                }
            }
            String::from_utf8(bytes).ok().and_then(|s| s.chars().next()).map_or(Key::Other, Key::Char)
        }
        _ => Key::Other,
    }
}

/// Raw mode on stdin while a prompt reads keys (`setRawMode`; output processing stays on, as libuv keeps ONLCR).
pub struct Raw {
    saved: libc::termios,
}

impl Raw {
    pub fn enable() -> Option<Raw> {
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

/// `requireInteractive`: stdin and stdout are terminals.
pub fn interactive() -> bool {
    super::stdin_tty() && super::stdout_tty()
}
