//! effect/cli 4.1's wizard mode (`--wizard`: `Command.runWith`'s `GlobalFlag.Wizard` branch, `internal/wizard.ts`)
//! with the prompts it uses from `Prompt.ts` (Select, Confirm, Toggle, Text and Int) on the Node terminal
//! (`@effect/platform-node-shared/NodeTerminal`: readline keypresses, raw mode, quit on Escape, Ctrl+C or Ctrl+D).
//!
//! The command at the parsed path is built prompt by prompt: a group picks a subcommand, a leaf asks for each flag,
//! then each argument (an optional one is confirmed first, a variadic one asks for a count), and every answer is
//! parsed as the command line would parse it. The command is shown, and run on request with the same arguments.
//! Each prompt draws a frame, erases it by its line count and draws the next, byte for byte as the TS prompts do.

use super::out::{stderr, stdout};
use super::parse::{self, CliError};
use super::spec::{Command, Kind, Mode, Param};
use opencode_protocol::wire::number::js_number_string;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const ITALIC: &str = "\x1b[3m";
const UNDERLINE: &str = "\x1b[4m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const MAGENTA: &str = "\x1b[35m";
const WHITE: &str = "\x1b[37m";
const BLACK_BRIGHT: &str = "\x1b[90m";
const CYAN_BRIGHT: &str = "\x1b[96m";
const CURSOR_HIDE: &str = "\x1b[?25l";
const CURSOR_SHOW: &str = "\x1b[?25h";
const CURSOR_LEFT: &str = "\x1b[G";
const CURSOR_SAVE: &str = "\x1b[s";
const CURSOR_RESTORE: &str = "\x1b[u";
const ERASE_LINE: &str = "\x1b[2K";
const BEEP: &str = "\x07";

// The default (non-Windows) theme's figures.
const PREFIX: &str = "?";
const POINTER: &str = "❯";
const POINTER_SMALL: &str = "›";
const TICK: &str = "✔";
const ELLIPSIS: &str = "…";
const ARROW_UP: &str = "↑";
const ARROW_DOWN: &str = "↓";
const DESCRIPTION_SEPARATOR: &str = "- ";
const TOGGLE_SEPARATOR: &str = "/";
const PRIMARY: &str = CYAN_BRIGHT;
const MUTED: &str = BLACK_BRIGHT;
const SUCCESS: &str = GREEN;
const ERROR: &str = RED;
const SUBMITTED: &str = WHITE;

/// How the wizard ended.
pub enum End {
    /// Run the command with these arguments (after the program name).
    Run(Vec<String>),
    /// Exit with this code.
    Exit(i32),
}

/// `Prompt.Theme`'s colors: `Ansi.detectColors` (stdout a terminal, `NO_COLOR` unset or empty).
#[derive(Clone, Copy)]
struct Theme {
    colors: bool,
}

impl Theme {
    fn detect() -> Self {
        Theme { colors: super::stdout_tty() && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty()) }
    }

    /// `Ansi.annotate`: the styles, the text, a reset.
    fn annotate(&self, text: &str, styles: &[&str]) -> String {
        if self.colors { format!("{}{text}{RESET}", styles.concat()) } else { text.to_owned() }
    }

    /// `annotateSymbol`: nothing for an empty symbol.
    fn symbol(&self, symbol: &str, styles: &[&str]) -> String {
        if symbol.is_empty() { String::new() } else { self.annotate(symbol, styles) }
    }
}

/// The wizard on the command at `path`; `argv` is the command line it started from (its `--wizard` dropped).
pub fn run(root: &Command, path: &[&str], argv: &[String]) -> End {
    let theme = Theme::detect();
    stdout(&format!("{}\n", introduction(root, theme)));
    let mut line: Vec<Arg> = std::iter::once(root.name.to_owned())
        .chain(argv.iter().filter(|arg| *arg != "--wizard" && !arg.starts_with("--wizard=")).cloned())
        .map(Arg::new)
        .collect();
    let mut selected = root;
    for name in path.iter().skip(1) {
        match selected.subcommands.iter().find(|c| c.name == *name || c.alias == Some(*name)) {
            Some(child) => selected = child,
            None => break,
        }
    }
    let wizard = Wizard { theme, columns: columns() };
    wizard.log_current(&line);
    let section = if std::ptr::eq(selected, root) { "ROOT" } else { selected.name };
    match wizard.command(selected, &mut line, section) {
        Ok(()) => {}
        Err(Stop::Quit) => {
            stdout(&format!("\n{}\n", theme.annotate("Wizard cancelled.", &[RED])));
            return End::Exit(0);
        }
        Err(Stop::Invalid(error)) => {
            // `Cause.pretty` of the CliError (its stack frames aside).
            stderr(&format!("~effect/cli/CliError/{}: {}\n", error.tag(), error.message()));
            return End::Exit(1);
        }
    }
    let shown: Vec<String> = line.iter().map(|arg| arg.display.clone()).collect();
    stdout(&format!("{}\n", block(theme, "Command ready", &shown, CYAN_BRIGHT, GREEN)));
    let toggle = Toggle { message: "Run this command?".into(), value: true, active: "yes", inactive: "no" };
    match wizard.prompt(toggle) {
        Ok(true) => {
            stdout("\n");
            End::Run(line.into_iter().skip(1).map(|arg| arg.value).collect())
        }
        Ok(false) => End::Exit(0),
        Err(_) => {
            stdout(&format!("\n{}\n", theme.annotate("Wizard cancelled.", &[RED])));
            End::Exit(0)
        }
    }
}

/// `renderIntroduction` (the root command's name and description).
fn introduction(root: &Command, theme: Theme) -> String {
    let title = format!(
        "{} {} {}",
        theme.annotate(root.name, &[BOLD, CYAN_BRIGHT]),
        theme.annotate(&format!("v{}", crate::app::VERSION), &[WHITE]),
        theme.annotate("· Command wizard", &[BOLD, WHITE])
    );
    let mut lines = vec![title];
    if !root.description.is_empty() {
        lines.push(root.description.to_owned());
    }
    lines.push(theme.annotate("Build a command interactively. Press Ctrl+C to cancel.", &[BLACK_BRIGHT]));
    lines.push(String::new());
    lines.join("\n")
}

/// A command-line word and how it is shown.
#[derive(Clone)]
struct Arg {
    value: String,
    display: String,
}

impl Arg {
    fn new(value: String) -> Self {
        Arg { display: value.clone(), value }
    }
}

enum Stop {
    Quit,
    Invalid(CliError),
}

struct Wizard {
    theme: Theme,
    columns: usize,
}

impl Wizard {
    fn log(&self, text: &str) {
        stdout(&format!("{text}\n"));
    }

    fn log_current(&self, line: &[Arg]) {
        let shown: Vec<String> = line.iter().map(|arg| arg.display.clone()).collect();
        self.log(&block(self.theme, "Current command", &shown, MAGENTA, WHITE));
    }

    fn section(&self, command: &str, section: &str) -> String {
        format!(
            "{} {} {}",
            self.theme.annotate(&command.to_uppercase(), &[BOLD, CYAN_BRIGHT]),
            self.theme.annotate("·", &[BLACK_BRIGHT]),
            self.theme.annotate(section, &[BOLD, WHITE])
        )
    }

    /// `promptCommand`.
    fn command(&self, command: &Command, line: &mut Vec<Arg>, section: &str) -> Result<(), Stop> {
        let visible = &command.subcommands;
        let (flags, arguments): (Vec<&Param>, Vec<&Param>) =
            if visible.is_empty() { (command.flags().collect(), command.arguments().collect()) } else { (vec![], vec![]) };
        if !flags.is_empty() {
            self.log(&self.section(section, "FLAGS"));
            for param in &flags {
                let args = self.param(param)?;
                line.extend(args);
            }
            if !arguments.is_empty() || !visible.is_empty() {
                self.log_current(line);
            }
        }
        if !arguments.is_empty() {
            self.log(&self.section(section, "ARGUMENTS"));
            for param in &arguments {
                let args = self.param(param)?;
                line.extend(args);
            }
            if !visible.is_empty() {
                self.log_current(line);
            }
        }
        if visible.is_empty() {
            return Ok(());
        }
        let choices = visible.iter().map(|c| (c.name.to_owned(), Some(c.description.to_owned()))).collect();
        let index = self.prompt(Select::new("Command".into(), choices))?;
        self.log("");
        let child = &visible[index];
        line.push(Arg::new(child.name.to_owned()));
        if !child.subcommands.is_empty() || !child.params.is_empty() {
            self.log_current(line);
        }
        self.command(child, line, child.name)
    }

    /// `promptParam`.
    fn param(&self, param: &Param) -> Result<Vec<Arg>, Stop> {
        let label = label(param);
        if param.is_optional() {
            let include = self.prompt(Confirm {
                message: format!("Set {label}?"),
                initial: false,
                labels: ("yes", "no"),
                placeholders: ("(Y/n)", "(y/N)"),
            })?;
            if !include {
                self.log("");
                return Ok(vec![]);
            }
        }
        let count = match param.mode {
            Mode::Many { min, max, .. } => {
                let min = min.unwrap_or(0) as f64;
                let max = max.map_or(f64::INFINITY, |max| max as f64);
                self.prompt(Int::new(format!("{label} count"), Some(min), min, max))? as usize
            }
            _ => 1,
        };
        let mut values = vec![];
        for _ in 0..count {
            values.push(self.single(param, &label)?);
        }
        let checked = if param.flag {
            parse::parse_flag(param, &values).map(|_| ())
        } else {
            parse::parse_argument(param, &mut values.clone()).map(|_| ())
        };
        checked.map_err(Stop::Invalid)?;
        self.log("");
        if !param.flag {
            return Ok(values.into_iter().map(Arg::new).collect());
        }
        // Option-looking values are inlined so the lexer cannot take them for flags or `--`; a flag with an optional
        // value only accepts an inline one.
        Ok(values
            .into_iter()
            .flat_map(|value| {
                if param.optional_value.is_some() || (value.starts_with('-') && value.encode_utf16().count() > 1) {
                    vec![Arg::new(format!("--{}={value}", param.name))]
                } else {
                    vec![Arg::new(format!("--{}", param.name)), Arg::new(value)]
                }
            })
            .collect())
    }

    /// `promptSingle`.
    fn single(&self, param: &Param, label: &str) -> Result<String, Stop> {
        let message = label.to_owned();
        Ok(match param.kind {
            Kind::Bool => self
                .prompt(Confirm { message, initial: false, labels: ("true", "false"), placeholders: ("(T/f)", "(t/F)") })?
                .to_string(),
            Kind::Choice(choices) => {
                let options = choices.iter().map(|c| (c.to_string(), None)).collect();
                choices[self.prompt(Select::new(message, options))?].to_owned()
            }
            Kind::Int => js_number_string(self.prompt(Int::new(message, None, f64::NEG_INFINITY, f64::INFINITY))?),
            Kind::Str | Kind::KeyValue => self.prompt(Text { message, value: vec![], cursor: 0 })?,
        })
    }

    /// `Prompt.run`: the frame loop until a submission, or the end of input.
    fn prompt<P: Prompt>(&self, mut state: P) -> Result<P::Output, Stop> {
        let _raw = super::clack::Raw::enable();
        let mut clear = String::new();
        let mut message = state.next(self.theme);
        let mut quitting = false;
        loop {
            stdout(&format!("{clear}{message}"));
            clear.clear();
            if quitting {
                stdout(CURSOR_SHOW);
                return Err(Stop::Quit);
            }
            let Some(key) = read_key() else {
                stdout(CURSOR_SHOW);
                return Err(Stop::Quit);
            };
            quitting = key.name == "escape" || (key.ctrl && (key.name == "c" || key.name == "d"));
            match state.process(&key) {
                Action::Beep => message = BEEP.to_owned(),
                Action::Next(next) => {
                    clear = state.clear(self.columns);
                    state = next;
                    message = state.next(self.theme);
                }
                Action::Submit(value) => {
                    let clear = state.clear(self.columns);
                    stdout(&format!("{clear}{}", state.submitted(&value, self.theme)));
                    stdout(CURSOR_SHOW);
                    return Ok(value);
                }
            }
        }
    }
}

/// `renderParamLabel`: a flag's short description (at most 32 characters) or its humanized name, then the flag.
fn label(param: &Param) -> String {
    let description = param.description.map(str::trim);
    let text = match description {
        Some(text) if param.flag && text.encode_utf16().count() <= 32 => text.to_owned(),
        _ => humanize(param.name),
    };
    if param.flag { format!("{text} (--{})", param.name) } else { text }
}

/// `humanize`: words split on `-` and `_`, the first capitalized.
fn humanize(name: &str) -> String {
    let words: Vec<&str> = name.split(['-', '_']).filter(|w| !w.is_empty()).collect();
    let Some(first) = words.first() else { return name.to_owned() };
    let mut chars = first.chars();
    let head: String = chars.next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    let mut out = vec![format!("{head}{}", chars.as_str())];
    out.extend(words[1..].iter().map(|w| w.to_string()));
    out.join(" ")
}

/// `renderCommandBlock`.
fn block(theme: Theme, label: &str, line: &[String], command: &str, label_color: &str) -> String {
    let mut out = vec![theme.annotate(label, &[BOLD, label_color])];
    out.extend(wrap_command(line).iter().map(|l| theme.annotate(l, &[command])));
    out.push(String::new());
    out.join("\n")
}

fn length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `wrapCommand`: shell-quoted words in lines of at most 88 columns.
fn wrap_command(line: &[String]) -> Vec<String> {
    const WIDTH: usize = 88;
    const FIRST: &str = "  $ ";
    const CONTINUATION: &str = "    ";
    let mut lines = vec![];
    let mut current = FIRST.to_owned();
    for arg in line.iter().map(|a| shell_arg(a)) {
        let separator = if current == FIRST || current == CONTINUATION { "" } else { " " };
        if length(&current) + separator.len() + length(&arg) > WIDTH && current != FIRST {
            lines.push(format!("{current} \\"));
            current = format!("{CONTINUATION}{arg}");
        } else {
            current.push_str(separator);
            current.push_str(&arg);
        }
    }
    if current != FIRST {
        lines.push(current);
    }
    lines
}

/// `formatShellArg`.
fn shell_arg(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg.chars().all(|c| c.is_ascii_alphanumeric() || "_./:@%+=,-".contains(c));
    if plain { arg.to_owned() } else { format!("'{}'", arg.replace('\'', "'\"'\"'")) }
}

/// The terminal's columns (`stdout.columns ?? 0`).
fn columns() -> usize {
    // SAFETY: TIOCGWINSZ fills the zeroed struct.
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut size) == 0 { size.ws_col as usize } else { 0 }
    }
}

// --- the prompt machinery ---

enum Action<S, O> {
    Beep,
    Next(S),
    Submit(O),
}

trait Prompt: Sized {
    type Output;
    /// The frame of the current state.
    fn next(&self, theme: Theme) -> String;
    /// The final frame.
    fn submitted(&self, value: &Self::Output, theme: Theme) -> String;
    /// What erases the current state's frame.
    fn clear(&self, columns: usize) -> String;
    fn process(&self, key: &Key) -> Action<Self, Self::Output>;
}

/// `renderPrompt`: the leading symbol, the (bold) message, the trailing symbol and the value.
fn render_prompt(confirm: &str, message: &str, leading: &str, trailing: &str, theme: Option<Theme>) -> String {
    let prefix = if leading.is_empty() { String::new() } else { format!("{leading} ") };
    let lines: Vec<String> = message
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .map(|line| match theme {
            Some(theme) => theme.annotate(line, &[BOLD]),
            None => line.to_owned(),
        })
        .collect();
    let mut out = format!("{prefix}{}", lines.join("\n"));
    if !trailing.is_empty() {
        out.push(' ');
        out.push_str(trailing);
    }
    if !confirm.is_empty() {
        out.push(' ');
        out.push_str(confirm);
    } else if !trailing.is_empty() {
        out.push(' ');
    }
    out
}

fn active_symbols(theme: Theme) -> (String, String) {
    (theme.symbol(PREFIX, &[PRIMARY]), theme.symbol(POINTER_SMALL, &[MUTED]))
}

fn submitted_symbols(theme: Theme) -> (String, String) {
    (theme.symbol(TICK, &[SUCCESS]), theme.symbol(ELLIPSIS, &[MUTED]))
}

/// `eraseText`: erases the rows `text` takes at `columns`, ending at the first column.
fn erase_text(text: &str, columns: usize) -> String {
    if columns == 0 {
        return format!("{ERASE_LINE}\x1b[1G");
    }
    let rows: usize = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .map(|line| 1 + length(line).saturating_sub(1) / columns)
        .sum();
    let mut out = String::new();
    for i in 0..rows {
        out.push_str(ERASE_LINE);
        if i + 1 < rows {
            out.push_str("\x1b[1A");
        }
    }
    if rows > 0 {
        out.push_str(CURSOR_LEFT);
    }
    out
}

/// `lines`: the rows `text` takes at `columns`.
fn rows(text: &str, columns: usize) -> usize {
    let lines = text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line));
    if columns == 0 { lines.count() } else { lines.map(|line| length(line).div_ceil(columns)).sum() }
}

/// `clearOutputWithError`.
fn clear_with_error(output: &str, columns: usize, error: &str) -> String {
    if error.is_empty() {
        return erase_text(output, columns);
    }
    format!("\x1b[{}B{}{}", rows(error, columns), erase_text(&format!("\n{error}"), columns), erase_text(output, columns))
}

/// An input error below the prompt line (the cursor stays on the prompt line), or its plain text.
fn render_error(error: &Option<String>, theme: Option<Theme>) -> String {
    let Some(error) = error else { return String::new() };
    let lines: Vec<&str> = error.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).collect();
    match theme {
        None => format!("{POINTER_SMALL} {}", lines.join("\n")),
        Some(theme) => {
            let prefix = theme.symbol(POINTER_SMALL, &[ERROR]);
            let lines: Vec<String> = lines.iter().map(|line| theme.annotate(line, &[ITALIC, ERROR])).collect();
            let body = if prefix.is_empty() { lines.join("\n") } else { format!("{prefix} {}", lines.join("\n")) };
            format!("{CURSOR_SAVE}\n{body}{CURSOR_RESTORE}")
        }
    }
}

struct Confirm {
    message: String,
    initial: bool,
    labels: (&'static str, &'static str),
    placeholders: (&'static str, &'static str),
}

impl Confirm {
    fn placeholder(&self) -> &'static str {
        if self.initial { self.placeholders.0 } else { self.placeholders.1 }
    }
}

impl Prompt for Confirm {
    type Output = bool;

    fn next(&self, theme: Theme) -> String {
        let (leading, trailing) = active_symbols(theme);
        let confirm = theme.annotate(self.placeholder(), &[MUTED]);
        format!("{CURSOR_HIDE}{}", render_prompt(&confirm, &self.message, &leading, &trailing, Some(theme)))
    }

    fn submitted(&self, value: &bool, theme: Theme) -> String {
        let (leading, trailing) = submitted_symbols(theme);
        let label = if *value { self.labels.0 } else { self.labels.1 };
        format!("{}\n", render_prompt(label, &self.message, &leading, &trailing, Some(theme)))
    }

    fn clear(&self, columns: usize) -> String {
        let text = render_prompt(self.placeholder(), &self.message, PREFIX, POINTER_SMALL, None);
        format!("{}{ERASE_LINE}{CURSOR_LEFT}", erase_text(&text, columns))
    }

    fn process(&self, key: &Key) -> Action<Self, bool> {
        if key.name == "enter" || key.name == "return" {
            return Action::Submit(self.initial);
        }
        let value = key.input.clone().unwrap_or_default().to_lowercase();
        if value.starts_with('y') || value.ends_with('t') {
            return Action::Submit(true);
        }
        if value.starts_with('n') || value.ends_with('f') {
            return Action::Submit(false);
        }
        Action::Beep
    }
}

struct Toggle {
    message: String,
    value: bool,
    active: &'static str,
    inactive: &'static str,
}

impl Toggle {
    fn render(&self, theme: Theme, submitted: bool) -> String {
        let separator = theme.symbol(TOGGLE_SEPARATOR, &[MUTED]);
        let selected = [UNDERLINE, if submitted { SUBMITTED } else { PRIMARY }];
        let inactive = if self.value { self.inactive.to_owned() } else { theme.annotate(self.inactive, &selected) };
        let active = if self.value { theme.annotate(self.active, &selected) } else { self.active.to_owned() };
        let tail = if separator.is_empty() { inactive } else { format!("{separator} {inactive}") };
        format!("{active} {tail}")
    }
}

impl Prompt for Toggle {
    type Output = bool;

    fn next(&self, theme: Theme) -> String {
        let (leading, trailing) = active_symbols(theme);
        format!("{CURSOR_HIDE}{}", render_prompt(&self.render(theme, false), &self.message, &leading, &trailing, Some(theme)))
    }

    fn submitted(&self, _: &bool, theme: Theme) -> String {
        let (leading, trailing) = submitted_symbols(theme);
        format!("{}\n", render_prompt(&self.render(theme, true), &self.message, &leading, &trailing, Some(theme)))
    }

    fn clear(&self, columns: usize) -> String {
        let toggle = format!("{} {TOGGLE_SEPARATOR} {}", self.active, self.inactive);
        let text = render_prompt(&toggle, &self.message, PREFIX, POINTER_SMALL, None);
        format!("{}{ERASE_LINE}{CURSOR_LEFT}", erase_text(&text, columns))
    }

    fn process(&self, key: &Key) -> Action<Self, bool> {
        let with = |value: bool| Action::Next(Toggle { message: self.message.clone(), value, ..*self });
        match key.name.as_str() {
            "0" | "j" | "delete" | "right" | "down" => with(false),
            "1" | "k" | "left" | "up" => with(true),
            " " | "tab" => with(!self.value),
            "enter" | "return" => Action::Submit(self.value),
            _ => Action::Beep,
        }
    }
}

struct Select {
    message: String,
    /// Each choice's title and description.
    choices: Vec<(String, Option<String>)>,
    index: usize,
}

const MAX_PER_PAGE: usize = 10;

impl Select {
    fn new(message: String, choices: Vec<(String, Option<String>)>) -> Self {
        Select { message, choices, index: 0 }
    }

    fn with(&self, index: usize) -> Self {
        Select { message: self.message.clone(), choices: self.choices.clone(), index }
    }

    /// `renderSelectChoices`, styled or plain.
    fn render_choices(&self, theme: Option<Theme>) -> String {
        let total = self.choices.len();
        let max = MAX_PER_PAGE.min(total);
        let start = (total as i64 - max as i64).min(self.index as i64 - (max / 2) as i64).max(0) as usize;
        let end = (start + max).min(total);
        (start..end)
            .map(|index| {
                let (title, description) = &self.choices[index];
                let active = index == self.index;
                let paging = if index == start && start > 0 {
                    ARROW_UP
                } else if index == end - 1 && end < total {
                    ARROW_DOWN
                } else {
                    " "
                };
                let prefix = match (active, theme) {
                    (true, Some(theme)) => format!("{}{paging}", theme.symbol(POINTER, &[PRIMARY])),
                    (true, None) => format!("{POINTER}{paging}"),
                    (false, _) => format!("{paging} "),
                };
                let title = match (active, theme) {
                    (true, Some(theme)) => theme.annotate(title, &[UNDERLINE, PRIMARY]),
                    _ => title.clone(),
                };
                let description = match (active, description) {
                    (true, Some(text)) if !text.is_empty() => {
                        let text = format!("{DESCRIPTION_SEPARATOR}{text}");
                        match theme {
                            Some(theme) => theme.annotate(&text, &[MUTED]),
                            None => text,
                        }
                    }
                    _ => String::new(),
                };
                format!("{prefix}{title} {description}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Prompt for Select {
    type Output = usize;

    fn next(&self, theme: Theme) -> String {
        let (leading, trailing) = active_symbols(theme);
        let prompt = render_prompt("", &self.message, &leading, &trailing, Some(theme));
        format!("{CURSOR_HIDE}{prompt}\n{}", self.render_choices(Some(theme)))
    }

    fn submitted(&self, value: &usize, theme: Theme) -> String {
        let (leading, trailing) = submitted_symbols(theme);
        let prompt = render_prompt("", &self.message, &leading, &trailing, Some(theme));
        format!("{prompt} {}\n", theme.annotate(&self.choices[*value].0, &[SUBMITTED]))
    }

    fn clear(&self, columns: usize) -> String {
        let prompt = render_prompt("", &self.message, PREFIX, POINTER_SMALL, None);
        let text = format!("{prompt}\n{}", self.render_choices(None));
        format!("{}{ERASE_LINE}{CURSOR_LEFT}", erase_text(&text, columns))
    }

    fn process(&self, key: &Key) -> Action<Self, usize> {
        let last = self.choices.len().saturating_sub(1);
        match key.name.as_str() {
            "k" | "up" => Action::Next(self.with(if self.index == 0 { last } else { self.index - 1 })),
            "j" | "down" => Action::Next(self.with(if self.index == last { 0 } else { self.index + 1 })),
            "tab" => Action::Next(self.with((self.index + 1) % self.choices.len().max(1))),
            "enter" | "return" => Action::Submit(self.index),
            _ => Action::Beep,
        }
    }
}

/// The text prompt (`Prompt.String`): UTF-16 units, as the JS string it edits.
struct Text {
    message: String,
    value: Vec<u16>,
    cursor: usize,
}

impl Text {
    fn text(&self) -> String {
        String::from_utf16_lossy(&self.value)
    }

    fn with(&self, value: Vec<u16>, cursor: usize) -> Self {
        Text { message: self.message.clone(), value, cursor }
    }
}

impl Prompt for Text {
    type Output = String;

    fn next(&self, theme: Theme) -> String {
        let (leading, trailing) = active_symbols(theme);
        let text = self.text();
        let value = if text.is_empty() { String::new() } else { theme.annotate(&text, &[UNDERLINE, PRIMARY]) };
        let prompt = render_prompt(&value, &self.message, &leading, &trailing, Some(theme));
        let offset = self.cursor as i64 - self.value.len() as i64;
        let movement = match offset {
            0 => String::new(),
            n if n > 0 => format!("\x1b[{n}C"),
            n => format!("\x1b[{}D", -n),
        };
        format!("{prompt}{movement}")
    }

    fn submitted(&self, _: &String, theme: Theme) -> String {
        let (leading, trailing) = submitted_symbols(theme);
        let text = self.text();
        let value = if text.is_empty() { String::new() } else { theme.annotate(&text, &[SUBMITTED]) };
        format!("{}\n", render_prompt(&value, &self.message, &leading, &trailing, Some(theme)))
    }

    fn clear(&self, columns: usize) -> String {
        let text = render_prompt(&self.text(), &self.message, PREFIX, POINTER_SMALL, None);
        format!("{}{ERASE_LINE}{CURSOR_LEFT}", clear_with_error(&text, columns, ""))
    }

    fn process(&self, key: &Key) -> Action<Self, String> {
        let len = self.value.len();
        if key.ctrl {
            return match key.name.as_str() {
                "u" => Action::Next(self.with(vec![], 0)),
                "a" => Action::Next(self.with(self.value.clone(), 0)),
                "e" => Action::Next(self.with(self.value.clone(), len)),
                _ => Action::Beep,
            };
        }
        match key.name.as_str() {
            "backspace" => {
                if self.cursor == 0 {
                    return Action::Beep;
                }
                let mut value = self.value.clone();
                value.remove(self.cursor - 1);
                Action::Next(self.with(value, self.cursor - 1))
            }
            "left" if self.cursor == 0 => Action::Beep,
            "left" => Action::Next(self.with(self.value.clone(), self.cursor - 1)),
            "right" if self.cursor >= len => Action::Beep,
            "right" => Action::Next(self.with(self.value.clone(), (self.cursor + 1).min(len))),
            "home" => Action::Next(self.with(self.value.clone(), 0)),
            "end" => Action::Next(self.with(self.value.clone(), len)),
            "enter" | "return" => Action::Submit(self.text()),
            // `processTab` with the empty default: an empty value stays as it is.
            "tab" if self.value.is_empty() => Action::Beep,
            "tab" => Action::Next(self.with(self.value.clone(), len)),
            _ => {
                let input: Vec<u16> = key.input.as_deref().unwrap_or("").encode_utf16().collect();
                let mut value = self.value[..self.cursor].to_vec();
                value.extend(&input);
                value.extend(&self.value[self.cursor..]);
                Action::Next(self.with(value, self.cursor + input.len()))
            }
        }
    }
}

/// The integer prompt (`Prompt.Int`).
struct Int {
    message: String,
    value: String,
    error: Option<String>,
    min: f64,
    max: f64,
}

impl Int {
    fn new(message: String, default: Option<f64>, min: f64, max: f64) -> Self {
        Int { message, value: default.map(js_number_string).unwrap_or_default(), error: None, min, max }
    }

    fn with(&self, value: String, error: Option<String>) -> Self {
        Int { message: self.message.clone(), value, error, min: self.min, max: self.max }
    }

    fn render(&self, leading: &str, trailing: &str, theme: Option<Theme>) -> String {
        let value = match theme {
            Some(theme) => {
                let annotation: &[&str] = if self.error.is_some() { &[ERROR] } else { &[UNDERLINE, PRIMARY] };
                theme.annotate(&self.value, annotation)
            }
            None => self.value.clone(),
        };
        render_prompt(&value, &self.message, leading, trailing, theme)
    }
}

impl Prompt for Int {
    type Output = f64;

    fn next(&self, theme: Theme) -> String {
        let (leading, trailing) = active_symbols(theme);
        format!("{}{}", self.render(&leading, &trailing, Some(theme)), render_error(&self.error, Some(theme)))
    }

    fn submitted(&self, _: &f64, theme: Theme) -> String {
        let (leading, trailing) = submitted_symbols(theme);
        format!("{}\n", render_prompt(&self.value, &self.message, &leading, &trailing, Some(theme)))
    }

    fn clear(&self, columns: usize) -> String {
        let error = render_error(&self.error, None);
        let text = self.render(PREFIX, POINTER_SMALL, None);
        format!("{}{ERASE_LINE}{CURSOR_LEFT}", clear_with_error(&text, columns, &error))
    }

    fn process(&self, key: &Key) -> Action<Self, f64> {
        if key.ctrl && key.name == "u" {
            return Action::Next(self.with(String::new(), None));
        }
        let step = |delta: f64, empty: &str| {
            if self.value.is_empty() || self.value == "-" {
                empty.to_owned()
            } else {
                js_number_string(parse_int(&self.value) + delta)
            }
        };
        match key.name.as_str() {
            "backspace" => {
                if self.value.is_empty() {
                    return Action::Beep;
                }
                let mut units: Vec<u16> = self.value.encode_utf16().collect();
                units.pop();
                Action::Next(self.with(String::from_utf16_lossy(&units), None))
            }
            "k" | "up" => Action::Next(self.with(step(1.0, "1"), None)),
            "j" | "down" => Action::Next(self.with(step(-1.0, "-1"), None)),
            "enter" | "return" => {
                let parsed = parse_int(&self.value);
                if parsed.is_nan() {
                    return Action::Next(self.with(self.value.clone(), Some("Must provide an integer value".into())));
                }
                if parsed < self.min {
                    let message = format!("{} must be greater than or equal to {}", js_number_string(parsed), js_number_string(self.min));
                    return Action::Next(self.with(self.value.clone(), Some(message)));
                }
                if parsed > self.max {
                    let message = format!("{} must be less than or equal to {}", js_number_string(parsed), js_number_string(self.max));
                    return Action::Next(self.with(self.value.clone(), Some(message)));
                }
                Action::Submit(parsed)
            }
            _ => {
                let input = key.input.as_deref().unwrap_or("");
                if self.value.is_empty() && input == "-" {
                    return Action::Next(self.with("-".into(), None));
                }
                let parsed = parse_int(&format!("{}{input}", self.value));
                if parsed.is_nan() { Action::Beep } else { Action::Next(self.with(js_number_string(parsed), None)) }
            }
        }
    }
}

/// `Number.parseInt(text)` (radix 10, or 16 after `0x`).
fn parse_int(text: &str) -> f64 {
    let whitespace = |c: char| {
        matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{feff}' | '\u{2028}' | '\u{2029}')
            || (c != '\u{85}' && c.is_whitespace() && !c.is_ascii())
    };
    let mut rest = text.trim_start_matches(whitespace);
    let negative = rest.starts_with('-');
    if let Some(stripped) = rest.strip_prefix(['-', '+']) {
        rest = stripped;
    }
    let (radix, digits) = match rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        Some(hex) => (16, hex),
        None => (10, rest),
    };
    let end = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
    let digits = &digits[..end];
    if digits.is_empty() {
        return f64::NAN;
    }
    let value = if radix == 10 {
        digits.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        digits.chars().fold(0.0, |acc, c| acc * 16.0 + c.to_digit(16).unwrap_or(0) as f64)
    };
    if negative { -value } else { value }
}

// --- keys (readline's `emitKeys`) ---

/// A keypress: readline's key name, whether Ctrl was held, and the text it typed (none for escape sequences).
struct Key {
    name: String,
    ctrl: bool,
    input: Option<String>,
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

/// The next keypress, or `None` at the end of input.
fn read_key() -> Option<Key> {
    let byte = read_byte(-1)?;
    let key = |name: &str, ctrl: bool, input: Option<String>| Key { name: name.to_owned(), ctrl, input };
    let text = |byte: u8| Some((byte as char).to_string());
    Some(match byte {
        b'\r' => key("return", false, text(byte)),
        b'\n' => key("enter", false, text(byte)),
        b'\t' => key("tab", false, text(byte)),
        0x08 | 0x7f => key("backspace", false, text(byte)),
        b' ' => key("space", false, text(byte)),
        0x1b => match read_byte(50) {
            None => key("escape", false, None),
            Some(b'[') | Some(b'O') => {
                let mut sequence = vec![];
                while let Some(next) = read_byte(50) {
                    sequence.push(next);
                    if next.is_ascii_alphabetic() || next == b'~' {
                        break;
                    }
                }
                let name = match sequence.as_slice() {
                    [b'A'] => "up",
                    [b'B'] => "down",
                    [b'C'] => "right",
                    [b'D'] => "left",
                    [b'H'] | [b'1', b'~'] | [b'7', b'~'] => "home",
                    [b'F'] | [b'4', b'~'] | [b'8', b'~'] => "end",
                    [b'2', b'~'] => "insert",
                    [b'3', b'~'] => "delete",
                    [b'5', b'~'] => "pageup",
                    [b'6', b'~'] => "pagedown",
                    [b'Z'] => "tab",
                    _ => "",
                };
                key(name, false, None)
            }
            Some(other) if other.is_ascii_alphanumeric() => key(&(other as char).to_ascii_lowercase().to_string(), false, None),
            Some(_) => key("", false, None),
        },
        0x01..=0x1a => key(&((byte - 1 + b'a') as char).to_string(), true, text(byte)),
        byte if byte.is_ascii_alphanumeric() => key(&(byte as char).to_ascii_lowercase().to_string(), false, text(byte)),
        byte if byte < 0x80 => key("", false, text(byte)),
        byte => {
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
            key("", false, Some(String::from_utf8_lossy(&bytes).into_owned()))
        }
    })
}
