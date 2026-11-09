//! effect/cli's help document (`internal/help.ts`, `internal/command.ts` `buildHelpDoc`) and its default formatter
//! (`CliOutput.ts` `defaultFormatter`), with its ANSI colors when `TerminalCapabilities` says the terminal can color
//! (stdout a terminal, or `FORCE_COLOR`; never with `NO_COLOR` or `TERM=dumb`).

use super::parse::{commands_for, path_globals};
use super::spec::{Command, Kind, Param};

/// `TerminalCapabilities.canColor`, as `fromProcess` detects it.
pub fn can_color() -> bool {
    let present = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
    if std::env::var("TERM").as_deref() == Ok("dumb") {
        return false;
    }
    let forcing = present("FORCE_COLOR").is_some_and(|value| value != "0" && !value.eq_ignore_ascii_case("false"));
    present("NO_COLOR").is_none() && (forcing || super::stdout_tty())
}

/// The default formatter's palette: each style is closed with a full reset.
#[derive(Clone, Copy)]
pub struct Palette {
    pub colors: bool,
}

impl Palette {
    pub fn detect() -> Self {
        Palette { colors: can_color() }
    }
    fn wrap(&self, code: &str, text: &str) -> String {
        if self.colors { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_owned() }
    }
    fn bold(&self, text: &str) -> String {
        self.wrap("1", text)
    }
    fn dim(&self, text: &str) -> String {
        self.wrap("2", text)
    }
    fn cyan(&self, text: &str) -> String {
        self.wrap("36", text)
    }
    fn green(&self, text: &str) -> String {
        self.wrap("32", text)
    }
}

fn width(text: &str) -> usize {
    text.chars().count()
}

/// A table row: its left column as shown and its plain width, and the right column.
struct Row {
    left: String,
    plain: usize,
    right: String,
}

fn table(rows: &[Row], cap: Option<usize>) -> String {
    let max = rows.iter().map(|row| row.plain).max().unwrap_or(0) + 4;
    let column = cap.map_or(max, |cap| max.min(cap));
    rows.iter()
        .map(|row| {
            let pad = column.max(row.plain + 1) - row.plain;
            format!("  {}{}{}", row.left, " ".repeat(pad), row.right)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn flag_row(palette: Palette, param: &Param) -> Row {
    let mut names = vec![format!("--{}", param.name)];
    names.extend(param.alias.map(|a| if a.chars().count() == 1 { format!("-{a}") } else { format!("--{a}") }));
    let type_name = match param.optional_value {
        Some(_) => format!("[={}]", param.type_name()),
        None => param.type_name().to_owned(),
    };
    let shown = names.iter().map(|name| palette.green(name)).collect::<Vec<_>>().join(", ");
    let (left, plain) = if type_name == "boolean" {
        (shown, width(&names.join(", ")))
    } else {
        (format!("{shown} {}", palette.dim(&type_name)), width(&names.join(", ")) + 1 + width(&type_name))
    };
    let choices = match param.kind {
        Kind::Choice(keys) if !keys.is_empty() => Some(format!("(choices: {})", keys.join(", "))),
        _ => None,
    };
    let right = match (param.description, choices) {
        (Some(text), Some(choices)) => format!("{text} {choices}"),
        (None, Some(choices)) => choices,
        (Some(text), None) => text.to_owned(),
        (None, None) => String::new(),
    };
    Row { left, plain, right }
}

fn required_argument(param: &Param) -> bool {
    match param.mode {
        super::spec::Mode::Many { min, optional, .. } => !optional && min.is_some_and(|m| m > 0),
        _ => !param.is_optional(),
    }
}

/// The help text of the command at `path` (without the trailing newline `Console.log` adds).
pub fn render(root: &Command, path: &[&str], palette: Palette) -> String {
    let commands = commands_for(root, path);
    let current: &Command = commands.last().copied().unwrap_or(root);
    let mut sections: Vec<String> = vec![];
    if !current.description.is_empty() {
        sections.push(palette.bold("DESCRIPTION"));
        sections.push(format!("  {}", current.description));
        sections.push(String::new());
    }
    let mut usage = path.join(" ");
    if !current.subcommands.is_empty() {
        usage.push_str(" <subcommand>");
    }
    usage.push_str(" [flags]");
    let arguments: Vec<&Param> = current.arguments().collect();
    for argument in &arguments {
        let name = if argument.is_variadic() { format!("<{}...>", argument.name) } else { format!("<{}>", argument.name) };
        if required_argument(argument) {
            usage.push_str(&format!(" {name}"));
        } else {
            usage.push_str(&format!(" [{name}]"));
        }
    }
    sections.push(palette.bold("USAGE"));
    sections.push(format!("  {}", palette.cyan(&usage)));
    sections.push(String::new());

    if !arguments.is_empty() {
        sections.push(palette.bold("ARGUMENTS"));
        let rows: Vec<Row> = arguments
            .iter()
            .map(|argument| {
                let name = if argument.is_variadic() { format!("{}...", argument.name) } else { argument.name.to_owned() };
                let optional = if required_argument(argument) { String::new() } else { palette.dim(" (optional)") };
                Row {
                    left: format!("{} {}", palette.green(&name), palette.dim(argument.type_name())),
                    plain: width(&name) + 1 + width(argument.type_name()),
                    right: format!("{}{optional}", argument.description.unwrap_or("")),
                }
            })
            .collect();
        sections.push(table(&rows, Some(25)));
        sections.push(String::new());
    }

    let flags: Vec<Row> = current.flags().filter(|p| !p.hidden).map(|p| flag_row(palette, p)).collect();
    if !flags.is_empty() {
        sections.push(palette.bold("FLAGS"));
        sections.push(table(&flags, None));
        sections.push(String::new());
    }

    let globals: Vec<Row> =
        path_globals(root, path).iter().map(|g| g.param()).filter(|p| !p.hidden).map(|p| flag_row(palette, &p)).collect();
    if !globals.is_empty() {
        sections.push(palette.bold("GLOBAL FLAGS"));
        sections.push(table(&globals, None));
        sections.push(String::new());
    }

    if !current.subcommands.is_empty() {
        sections.push(palette.bold("SUBCOMMANDS"));
        let rows: Vec<Row> = current
            .subcommands
            .iter()
            .map(|sub| {
                let name = match sub.alias {
                    Some(alias) => format!("{}, {alias}", sub.name),
                    None => sub.name.to_owned(),
                };
                Row { left: palette.cyan(&name), plain: width(&name), right: sub.description.to_owned() }
            })
            .collect();
        sections.push(table(&rows, Some(20)));
    }
    if sections.last().is_some_and(|s| s.is_empty()) {
        sections.pop();
    }
    sections.join("\n")
}

/// `formatVersion`.
pub fn version(name: &str, version: &str, palette: Palette) -> String {
    format!("{} {}{}", palette.bold(name), palette.dim("v"), palette.bold(version))
}

/// `formatErrors`: one error under `ERROR`, several under `ERRORS` grouped by kind.
pub fn errors(errors: &[super::parse::CliError], palette: Palette) -> String {
    if errors.is_empty() {
        return String::new();
    }
    let (reset, red, bold) = if palette.colors { ("\x1b[0m", "\x1b[31m", "\x1b[1m") } else { ("", "", "") };
    if errors.len() == 1 {
        return format!("\n{bold}{red}ERROR{reset}\n  {}{reset}", escape_control(&errors[0].message()));
    }
    let mut tags: Vec<&str> = vec![];
    for error in errors {
        if !tags.contains(&error.tag()) {
            tags.push(error.tag());
        }
    }
    let mut sections = vec![format!("\n{bold}{red}ERRORS{reset}")];
    for tag in tags {
        for error in errors.iter().filter(|e| e.tag() == tag) {
            sections.push(format!("  {}{reset}", escape_control(&error.message())));
        }
    }
    sections.join("\n")
}

/// `escapeControlCharacters`: C0 controls but tab, newline and carriage return, and DEL, as `\xNN`.
fn escape_control(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let code = ch as u32;
        if code <= 0x08 || code == 0x0b || code == 0x0c || (0x0e..=0x1f).contains(&code) || code == 0x7f {
            out.push_str(&format!("\\x{code:02x}"));
        } else {
            out.push(ch);
        }
    }
    out
}
