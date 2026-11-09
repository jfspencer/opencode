//! effect/cli's help document (`internal/help.ts`, `internal/command.ts` `buildHelpDoc`) and its default formatter
//! without colors (`CliOutput.ts`): output is never a terminal in the cases that matter, and `NO_COLOR` or a pipe
//! turn colors off. TODO(port): ANSI colors when stdout is a TTY.

use super::parse::{commands_for, path_globals};
use super::spec::{Command, Kind, Param};

fn width(text: &str) -> usize {
    text.chars().count()
}

fn table(rows: &[(String, String)], cap: Option<usize>) -> String {
    let max = rows.iter().map(|(left, _)| width(left)).max().unwrap_or(0) + 4;
    let column = cap.map_or(max, |cap| max.min(cap));
    rows.iter()
        .map(|(left, right)| {
            let pad = column.max(width(left) + 1) - width(left);
            format!("  {left}{}{right}", " ".repeat(pad))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn flag_row(param: &Param) -> (String, String) {
    let mut names = vec![format!("--{}", param.name)];
    names.extend(param.alias.map(|a| if a.chars().count() == 1 { format!("-{a}") } else { format!("--{a}") }));
    let type_name = match param.optional_value {
        Some(_) => format!("[={}]", param.type_name()),
        None => param.type_name().to_owned(),
    };
    let left = if type_name == "boolean" { names.join(", ") } else { format!("{} {type_name}", names.join(", ")) };
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
    (left, right)
}

fn required_argument(param: &Param) -> bool {
    match param.mode {
        super::spec::Mode::Many { min, optional, .. } => !optional && min.is_some_and(|m| m > 0),
        _ => !param.is_optional(),
    }
}

/// The help text of the command at `path` (without the trailing newline `Console.log` adds).
pub fn render(root: &Command, path: &[&str]) -> String {
    let commands = commands_for(root, path);
    let current: &Command = commands.last().copied().unwrap_or(root);
    let mut sections: Vec<String> = vec![];
    if !current.description.is_empty() {
        sections.push("DESCRIPTION".into());
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
    sections.push("USAGE".into());
    sections.push(format!("  {usage}"));
    sections.push(String::new());

    if !arguments.is_empty() {
        sections.push("ARGUMENTS".into());
        let rows: Vec<(String, String)> = arguments
            .iter()
            .map(|argument| {
                let name = if argument.is_variadic() { format!("{}...", argument.name) } else { argument.name.to_owned() };
                let optional = if required_argument(argument) { "" } else { " (optional)" };
                (format!("{name} {}", argument.type_name()), format!("{}{optional}", argument.description.unwrap_or("")))
            })
            .collect();
        sections.push(table(&rows, Some(25)));
        sections.push(String::new());
    }

    let flags: Vec<(String, String)> = current.flags().filter(|p| !p.hidden).map(flag_row).collect();
    if !flags.is_empty() {
        sections.push("FLAGS".into());
        sections.push(table(&flags, None));
        sections.push(String::new());
    }

    let globals: Vec<(String, String)> =
        path_globals(root, path).iter().map(|g| g.param()).filter(|p| !p.hidden).map(|p| flag_row(&p)).collect();
    if !globals.is_empty() {
        sections.push("GLOBAL FLAGS".into());
        sections.push(table(&globals, None));
        sections.push(String::new());
    }

    if !current.subcommands.is_empty() {
        sections.push("SUBCOMMANDS".into());
        let rows: Vec<(String, String)> = current
            .subcommands
            .iter()
            .map(|sub| {
                let name = match sub.alias {
                    Some(alias) => format!("{}, {alias}", sub.name),
                    None => sub.name.to_owned(),
                };
                (name, sub.description.to_owned())
            })
            .collect();
        sections.push(table(&rows, Some(20)));
    }
    if sections.last().is_some_and(|s| s.is_empty()) {
        sections.pop();
    }
    sections.join("\n")
}

/// `formatErrors`: one error under `ERROR`, several under `ERRORS` grouped by kind.
pub fn errors(errors: &[super::parse::CliError]) -> String {
    if errors.is_empty() {
        return String::new();
    }
    if errors.len() == 1 {
        return format!("\nERROR\n  {}", escape_control(&errors[0].message()));
    }
    let mut tags: Vec<&str> = vec![];
    for error in errors {
        if !tags.contains(&error.tag()) {
            tags.push(error.tag());
        }
    }
    let mut sections = vec!["\nERRORS".to_owned()];
    for tag in tags {
        for error in errors.iter().filter(|e| e.tag() == tag) {
            sections.push(format!("  {}", escape_control(&error.message())));
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
