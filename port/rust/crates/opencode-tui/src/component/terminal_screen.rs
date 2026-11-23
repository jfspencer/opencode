//! The terminal pane's screen: enough of a VT to show a shell correctly (`@opentui/core`'s `EmbeddedTerminal`
//! interprets the daemon's output with a full emulator; here the pane only has to display it).
//!
//! Printable text, CR/LF/BS/TAB, scrolling, the cursor movements and erases a shell and `clear` use (CSI
//! `A`-`H`, `J`, `K`, `L`, `M`, `P`, `@`, `S`, `T`, `X`, `d`, `f`, `r`), SGR and modes ignored, OSC titles
//! captured, and resizes that keep what was on screen. Wide characters take one cell (the standard records text,
//! and no scenario puts CJK in a terminal pane). No colors or attributes: the frame records the characters.

/// One cell: a character, or a continuing half of a wide character (never produced here).
type Cells = Vec<char>;

pub struct Screen {
    cols: usize,
    rows: usize,
    lines: Vec<Cells>,
    row: usize,
    col: usize,
    /// The cursor sits past the last column: the next printable character wraps first.
    wrap_pending: bool,
    saved: (usize, usize),
    scroll_top: usize,
    scroll_bottom: usize,
    state: State,
    title: Option<String>,
    pending: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
enum State {
    Text,
    Escape,
    Csi(String),
    Osc(String),
    /// An OSC ended by `ESC`, waiting for the `\` of ST.
    OscEscape(String),
    /// A charset designation's designator, ignored.
    Charset,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = (cols.max(1) as usize, rows.max(1) as usize);
        Self {
            cols,
            rows,
            lines: vec![vec![' '; cols]; rows],
            row: 0,
            col: 0,
            wrap_pending: false,
            saved: (0, 0),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            state: State::Text,
            title: None,
            pending: Vec::new(),
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The screen as rows of text, trailing blanks trimmed.
    pub fn content(&self) -> String {
        self.lines
            .iter()
            .map(|line| {
                let end = line.iter().rposition(|c| *c != ' ').map(|index| index + 1).unwrap_or(0);
                line[..end].iter().collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A row's text, trailing blanks trimmed.
    pub fn line(&self, row: usize) -> Option<String> {
        let line = self.lines.get(row)?;
        let end = line.iter().rposition(|c| *c != ' ').map(|index| index + 1).unwrap_or(0);
        Some(line[..end].iter().collect())
    }

    /// Resizes the screen, keeping the top-left of what it showed.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1) as usize, rows.max(1) as usize);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let mut lines = vec![vec![' '; cols]; rows];
        for (row, line) in self.lines.iter().enumerate().take(rows) {
            lines[row][..line.len().min(cols)].copy_from_slice(&line[..line.len().min(cols)]);
        }
        self.lines = lines;
        self.cols = cols;
        self.rows = rows;
        self.row = self.row.min(rows - 1);
        self.col = self.col.min(cols - 1);
        self.wrap_pending = false;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
    }

    /// Feeds output bytes (they may split a UTF-8 sequence or an escape).
    pub fn write(&mut self, bytes: &[u8]) {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);
        let mut index = 0;
        while index < input.len() {
            let byte = input[index];
            if byte < 0x80 {
                self.step(byte as char);
                index += 1;
                continue;
            }
            let length = utf8_length(byte);
            if length == 0 {
                index += 1;
                continue;
            }
            if index + length > input.len() {
                break;
            }
            match std::str::from_utf8(&input[index..index + length]) {
                Ok(text) => {
                    for c in text.chars() {
                        self.step(c);
                    }
                    index += length;
                }
                Err(_) => index += 1,
            }
        }
        self.pending = input[index..].to_vec();
    }

    fn step(&mut self, c: char) {
        match std::mem::replace(&mut self.state, State::Text) {
            State::Text => match c {
                '\x1b' => self.state = State::Escape,
                '\r' => {
                    self.col = 0;
                    self.wrap_pending = false;
                }
                '\n' => self.linefeed(),
                '\x08' => {
                    self.col = self.col.saturating_sub(1);
                    self.wrap_pending = false;
                }
                '\t' => {
                    let next = (self.col / 8 + 1) * 8;
                    self.col = next.min(self.cols - 1);
                    self.wrap_pending = false;
                }
                '\x07' | '\x0e' | '\x0f' => {}
                c if (c as u32) < 0x20 => {}
                c => self.print(c),
            },
            State::Escape => match c {
                '[' => self.state = State::Csi(String::new()),
                ']' => self.state = State::Osc(String::new()),
                '7' => self.saved = (self.row, self.col),
                '8' => {
                    self.row = self.saved.0.min(self.rows - 1);
                    self.col = self.saved.1.min(self.cols - 1);
                    self.wrap_pending = false;
                }
                '(' | ')' | '*' | '+' | '-' | '.' | '/' => self.state = State::Charset,
                'c' => self.reset(),
                _ => {}
            },
            State::Charset => {}
            State::Csi(mut params) => {
                if ('\x40'..='\x7e').contains(&c) {
                    self.control(&params, c);
                } else if (c as u32) < 0x20 {
                    // A control character cancels the sequence.
                } else if params.len() < 64 {
                    params.push(c);
                    self.state = State::Csi(params);
                }
            }
            State::Osc(mut text) => match c {
                '\x07' => self.osc(&text),
                '\x1b' => self.state = State::OscEscape(text),
                c if text.len() < 4096 => {
                    text.push(c);
                    self.state = State::Osc(text);
                }
                _ => {}
            },
            State::OscEscape(mut text) => {
                if c == '\\' {
                    self.osc(&text);
                    return;
                }
                text.push('\x1b');
                text.push(c);
                self.state = State::Osc(text);
            }
        }
    }

    fn reset(&mut self) {
        self.lines = vec![vec![' '; self.cols]; self.rows];
        self.row = 0;
        self.col = 0;
        self.wrap_pending = false;
        self.saved = (0, 0);
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
    }

    fn print(&mut self, c: char) {
        if self.wrap_pending {
            self.linefeed();
            self.col = 0;
            self.wrap_pending = false;
        }
        self.lines[self.row][self.col] = c;
        if self.col + 1 >= self.cols {
            self.wrap_pending = true;
        } else {
            self.col += 1;
        }
    }

    fn linefeed(&mut self) {
        if self.row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
        self.wrap_pending = false;
    }

    fn scroll_up(&mut self, count: usize) {
        let count = count.min(self.scroll_bottom - self.scroll_top + 1);
        for _ in 0..count {
            self.lines.remove(self.scroll_top);
            self.lines.insert(self.scroll_bottom, vec![' '; self.cols]);
        }
    }

    fn scroll_down(&mut self, count: usize) {
        let count = count.min(self.scroll_bottom - self.scroll_top + 1);
        for _ in 0..count {
            self.lines.remove(self.scroll_bottom);
            self.lines.insert(self.scroll_top, vec![' '; self.cols]);
        }
    }

    fn osc(&mut self, text: &str) {
        // `0;title` / `2;title`: the window title.
        let (command, value) = text.split_once(';').unwrap_or((text, ""));
        if command == "0" || command == "2" {
            self.title = Some(value.to_string());
        }
    }

    fn control(&mut self, params: &str, final_char: char) {
        let body = params.trim_start_matches(['?', '>', '!', '=']);
        let numbers: Vec<usize> = body
            .split(';')
            .map(|part| {
                let digits = part.split(':').next().unwrap_or(part);
                digits.parse::<usize>().unwrap_or(0)
            })
            .collect();
        let number = |index: usize, default: usize| numbers.get(index).copied().filter(|n| *n != 0).unwrap_or(default);
        let count = || number(0, 1);
        match final_char {
            'A' => self.row = self.row.saturating_sub(count()),
            'B' => self.row = (self.row + count()).min(self.rows - 1),
            'C' => self.col = (self.col + count()).min(self.cols - 1),
            'D' => self.col = self.col.saturating_sub(count()),
            'E' => {
                self.row = (self.row + count()).min(self.rows - 1);
                self.col = 0;
            }
            'F' => {
                self.row = self.row.saturating_sub(count());
                self.col = 0;
            }
            'G' => self.col = (count() - 1).min(self.cols - 1),
            'H' | 'f' => {
                self.row = (number(0, 1) - 1).min(self.rows - 1);
                self.col = (number(1, 1) - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            'd' => self.row = (count() - 1).min(self.rows - 1),
            'J' => match numbers.first().copied().unwrap_or(0) {
                1 => {
                    for row in 0..self.row {
                        self.lines[row].fill(' ');
                    }
                    self.lines[self.row][..=self.col].fill(' ');
                }
                2 | 3 => {
                    for line in &mut self.lines {
                        line.fill(' ');
                    }
                }
                _ => {
                    self.lines[self.row][self.col..].fill(' ');
                    for row in self.row + 1..self.rows {
                        self.lines[row].fill(' ');
                    }
                }
            },
            'K' => match numbers.first().copied().unwrap_or(0) {
                1 => self.lines[self.row][..=self.col].fill(' '),
                2 => self.lines[self.row].fill(' '),
                _ => self.lines[self.row][self.col..].fill(' '),
            },
            'L' => {
                let limit = self.scroll_bottom;
                if self.row <= limit {
                    for _ in 0..count().min(limit - self.row + 1) {
                        self.lines.remove(limit);
                        self.lines.insert(self.row, vec![' '; self.cols]);
                    }
                }
            }
            'M' => {
                let limit = self.scroll_bottom;
                if self.row <= limit {
                    for _ in 0..count().min(limit - self.row + 1) {
                        self.lines.remove(self.row);
                        self.lines.insert(limit, vec![' '; self.cols]);
                    }
                }
            }
            'P' => {
                let count = count().min(self.cols - self.col);
                let line = &mut self.lines[self.row];
                line.drain(self.col..self.col + count);
                line.extend(std::iter::repeat_n(' ', count));
            }
            '@' => {
                let count = count().min(self.cols - self.col);
                let line = &mut self.lines[self.row];
                line.truncate(self.cols - count);
                for _ in 0..count {
                    line.insert(self.col, ' ');
                }
            }
            'S' => self.scroll_up(count()),
            'T' => self.scroll_down(count()),
            'X' => {
                let end = (self.col + count()).min(self.cols);
                self.lines[self.row][self.col..end].fill(' ');
            }
            'r' => {
                let top = number(0, 1) - 1;
                let bottom = number(1, self.rows) - 1;
                if top < bottom {
                    self.scroll_top = top.min(self.rows - 1);
                    self.scroll_bottom = bottom.min(self.rows - 1);
                    self.row = 0;
                    self.col = 0;
                }
            }
            's' => self.saved = (self.row, self.col),
            'u' => {
                self.row = self.saved.0.min(self.rows - 1);
                self.col = self.saved.1.min(self.cols - 1);
            }
            _ => {}
        }
        self.wrap_pending = false;
    }
}

fn utf8_length(byte: u8) -> usize {
    match byte {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::Screen;

    fn text(screen: &Screen) -> Vec<String> {
        (0..screen.rows()).map(|row| screen.line(row).unwrap()).collect()
    }

    #[test]
    fn prints_echoes_and_clears() {
        let mut screen = Screen::new(20, 4);
        screen.write(b"$ echo hi\r\nhi\r\n$ ");
        assert_eq!(text(&screen), vec!["$ echo hi", "hi", "$", ""]);
        screen.write(b"clear");
        assert_eq!(screen.line(2).unwrap(), "$ clear");
        screen.write(b"\r\n\x1b[3J\x1b[H\x1b[2J$ ");
        assert_eq!(text(&screen), vec!["$", "", "", ""]);
    }

    #[test]
    fn scrolling_keeps_the_bottom() {
        let mut screen = Screen::new(10, 3);
        screen.write(b"a\r\nb\r\nc\r\nd");
        assert_eq!(text(&screen), vec!["b", "c", "d"]);
    }

    #[test]
    fn erase_line_and_cursor_moves() {
        let mut screen = Screen::new(10, 2);
        screen.write(b"hello\x1b[2K\r$ ");
        assert_eq!(screen.line(0).unwrap(), "$");
        screen.write(b"\x1b[1;1Hx\x1b[K");
        assert_eq!(screen.line(0).unwrap(), "x");
    }

    #[test]
    fn backspace_and_tab() {
        let mut screen = Screen::new(20, 2);
        screen.write(b"ab\x08c\tz");
        assert_eq!(screen.line(0).unwrap(), "ac      z");
    }

    #[test]
    fn osc_titles_are_captured_and_ignored() {
        let mut screen = Screen::new(20, 2);
        screen.write(b"\x1b]0;bash\x07$ ");
        assert_eq!(screen.line(0).unwrap(), "$");
        assert_eq!(screen.title(), Some("bash"));
        screen.write(b"\x1b]2;other\x1b\\ok");
        assert_eq!(screen.line(0).unwrap(), "$ ok");
        assert_eq!(screen.title(), Some("other"));
    }

    #[test]
    fn resize_keeps_the_screen_and_wraps_incomplete_utf8() {
        let mut screen = Screen::new(10, 2);
        let bytes = "\u{e9}".as_bytes();
        screen.write(&bytes[..1]);
        screen.write(&bytes[1..]);
        assert_eq!(screen.line(0).unwrap(), "\u{e9}");
        screen.resize(4, 3);
        assert_eq!(screen.cols(), 4);
        assert_eq!(screen.rows(), 3);
        assert_eq!(screen.line(0).unwrap(), "\u{e9}");
    }

    #[test]
    fn wrap_at_the_last_column() {
        let mut screen = Screen::new(4, 2);
        screen.write(b"abcd");
        assert_eq!(screen.line(0).unwrap(), "abcd");
        screen.write(b"e");
        assert_eq!(text(&screen), vec!["abcd", "e"]);
    }
}
