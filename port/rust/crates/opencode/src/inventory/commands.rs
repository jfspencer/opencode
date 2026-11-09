//! Slash commands (packages/core/src/command.ts and the plugins that fill it, in activation order): `init` and `review`
//! (plugin/command.ts), `goal` (plugin/goal.ts), then config (config/plugin/command.ts): config `commands` and the
//! markdown files `{command,commands}/**/*.md` of config directories, document by document. A command added under a
//! name that exists replaces it in place (a JS `Map`).
//!
//! Running a command is the execution loop's: [`Command::kind`] says what it does. [`template`] is
//! `evaluateTemplate`'s argument substitution (shell interpolation, `` !`cmd` ``, runs after it, in the location's
//! directory with the configured shell); [`append`] is the built-ins' `$ARGUMENTS` handling.
//!
//! MCP prompts as commands are out of scope (MCP is not ported).

use opencode_protocol::types as t;
use opencode_protocol::Wire;

use super::config::{self, Entry};
use super::jsvalue::{self, Shape, Value};
use super::{markdown, scan};

/// `plugin/command/initialize.txt` (`${path}` is the project directory).
pub const PROMPT_INITIALIZE: &str = include_str!("builtin/initialize.txt");
/// `plugin/command/review.txt` (`${path}` is the project directory).
pub const PROMPT_REVIEW: &str = include_str!("builtin/review.txt");

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// `init`: prompts with [`PROMPT_INITIALIZE`] (see [`append`]).
    Init,
    /// `review`: prompts with [`PROMPT_REVIEW`] (see [`append`]).
    Review,
    /// `goal`: plugin/goal.ts.
    Goal,
    /// A config or markdown command.
    Config(t::ConfigCommand),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub name: String,
    pub description: Option<String>,
    pub kind: Kind,
}

impl Command {
    /// `Command.Info`.
    pub fn info(&self) -> t::CommandInfo {
        t::CommandInfo { name: self.name.clone(), description: self.description.clone() }
    }

    /// Whether the command runs as a subagent (`subagent`, or the deprecated `subtask`).
    pub fn subagent(&self) -> Option<bool> {
        match &self.kind {
            Kind::Config(c) => c.subagent.or(c.subtask),
            _ => None,
        }
    }
}

/// What `command.updated` compares: the listing.
pub fn listing(commands: &[Command]) -> String {
    let infos: Vec<t::CommandInfo> = commands.iter().map(Command::info).collect();
    opencode_protocol::to_string(&infos)
}

fn add(commands: &mut Vec<Command>, command: Command) {
    match commands.iter_mut().find(|c| c.name == command.name) {
        Some(existing) => *existing = command,
        None => commands.push(command),
    }
}

pub fn build(entries: &[Entry]) -> Vec<Command> {
    let mut commands = vec![];
    add(&mut commands, Command { name: "init".into(), description: Some("guided AGENTS.md setup".into()), kind: Kind::Init });
    add(
        &mut commands,
        Command {
            name: "review".into(),
            description: Some("review changes [commit|branch|pr], defaults to uncommitted".into()),
            kind: Kind::Review,
        },
    );
    add(
        &mut commands,
        Command {
            name: "goal".into(),
            description: Some("keep working until an objective is met [pause|resume|clear]".into()),
            kind: Kind::Goal,
        },
    );
    for entry in entries {
        let configured: Vec<(String, t::ConfigCommand)> = match entry {
            Entry::Document { info, .. } => info.commands.iter().flat_map(|c| c.0.clone()).collect(),
            Entry::Directory(directory) => markdown_commands(directory),
        };
        for (name, command) in configured {
            add(&mut commands, Command { name, description: command.description.clone(), kind: Kind::Config(command) });
        }
    }
    commands
}

/// `ConfigCommand.Info` as Effect decodes it from a frontmatter's values (see [`jsvalue::lower`]).
const COMMAND: Shape = Shape::Struct(&[
    ("template", Shape::Leaf),
    ("description", Shape::Leaf),
    ("agent", Shape::Leaf),
    ("model", Shape::Selection),
    ("subagent", Shape::Leaf),
    ("subtask", Shape::Leaf),
]);

/// The commands of a config directory's markdown files: the name is the path below the commands directory, the body
/// the template; a file whose frontmatter does not decode is skipped.
fn markdown_commands(directory: &str) -> Vec<(String, t::ConfigCommand)> {
    let mut out = vec![];
    for file in scan::markdown_all(directory, &["command", "commands"]) {
        let Some(content) = config::read(&file) else { continue };
        let Some(command) = markdown_command(&content) else { continue };
        let relative = file.strip_prefix(directory).unwrap_or(&file).trim_start_matches('/');
        let name = ["command/", "commands/"].iter().find_map(|p| relative.strip_prefix(p)).unwrap_or(relative);
        out.push((name.strip_suffix(".md").unwrap_or(name).to_owned(), command));
    }
    out
}

/// One markdown command file: `decodeCommand({ ...markdown.data, template })`.
pub(super) fn markdown_command(content: &str) -> Option<t::ConfigCommand> {
    let parsed = markdown::parse_option(content)?;
    let input = jsvalue::spread(&parsed.data.root);
    input.define("template", Value::string(config::js_trim(&parsed.content)));
    let lowered = jsvalue::lower(&Value::Object(input), &COMMAND).ok()??;
    t::ConfigCommand::decode(&lowered).ok()
}

/// `parseArguments`: words, `"double"` or `'single'` quoted runs (quotes trimmed), and `[Image N]` placeholders.
pub fn arguments(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // `[Image\s+\d+]`, case-insensitive.
        if c == '[' {
            let rest: String = chars[i..].iter().collect();
            if let Some(len) = image_placeholder(&rest) {
                out.push(rest.chars().take(len).collect());
                i += len;
                continue;
            }
        }
        if c == '"' || c == '\'' {
            if let Some(end) = chars[i + 1..].iter().position(|&x| x == c) {
                let token: String = chars[i..i + end + 2].iter().collect();
                out.push(trim_quotes(&token));
                i += end + 2;
                continue;
            }
            // An unmatched quote is not part of any token.
            i += 1;
            continue;
        }
        if config::js_space(c) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !(config::js_space(chars[i]) || chars[i] == '"' || chars[i] == '\'') {
            i += 1;
        }
        let token: String = chars[start..i].iter().collect();
        out.push(trim_quotes(&token));
    }
    out
}

fn image_placeholder(text: &str) -> Option<usize> {
    let lower = text.to_lowercase();
    let rest = lower.strip_prefix("[image")?;
    let spaces = rest.chars().take_while(|c| config::js_space(*c)).count();
    if spaces == 0 {
        return None;
    }
    let rest = &rest[rest.char_indices().nth(spaces).map(|(i, _)| i).unwrap_or(rest.len())..];
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || !rest[digits..].starts_with(']') {
        return None;
    }
    Some("[image".chars().count() + spaces + digits + 1)
}

/// `.replace(/^["']|["']$/g, "")`.
fn trim_quotes(token: &str) -> String {
    let token = token.strip_prefix(['"', '\'']).unwrap_or(token);
    token.strip_suffix(['"', '\'']).unwrap_or(token).to_owned()
}

/// `evaluateTemplate` without the shell step: `$ARGUMENTS` is the whole input, `$N` the Nth argument (the highest
/// placeholder takes the rest), and input without any placeholder is appended after a blank line.
pub fn template(template: &str, input: &str) -> String {
    let args = arguments(input);
    let placeholders = positions(template);
    let last = placeholders.iter().map(|(_, _, n)| *n).max().unwrap_or(0);
    let mut out = String::new();
    let mut cursor = 0;
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            if template[i + 1..].starts_with("ARGUMENTS") {
                out.push_str(&template[cursor..i]);
                out.push_str(input);
                i += 1 + "ARGUMENTS".len();
                cursor = i;
                continue;
            }
            if let Some(&(start, end, n)) = placeholders.iter().find(|(s, _, _)| *s == i) {
                out.push_str(&template[cursor..start]);
                let index = n - 1;
                if index < args.len() {
                    if n == last {
                        out.push_str(&args[index..].join(" "));
                    } else {
                        out.push_str(&args[index]);
                    }
                }
                i = end;
                cursor = end;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&template[cursor..]);
    if placeholders.is_empty() && !template.contains("$ARGUMENTS") && !config::js_trim(input).is_empty() {
        config::js_trim(&format!("{out}\n\n{input}")).to_owned()
    } else {
        config::js_trim(&out).to_owned()
    }
}

/// `$N` placeholders (`/\$([1-9]\d*)/g`): byte start, end and N.
fn positions(template: &str) -> Vec<(usize, usize, usize)> {
    let bytes = template.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && (b'1'..=b'9').contains(&bytes[i + 1]) {
            let mut end = i + 2;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let n = template[i + 1..end].parse().unwrap_or(usize::MAX);
            out.push((i, end, n));
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

/// The built-ins' `append`: the template with `$ARGUMENTS` replaced by the trimmed input, or the input appended after
/// a blank line.
pub fn append(template: &str, input: &str) -> String {
    let value = config::js_trim(input);
    if template.contains("$ARGUMENTS") {
        return template.replace("$ARGUMENTS", value);
    }
    if value.is_empty() { template.to_owned() } else { format!("{template}\n\n{value}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates() {
        assert_eq!(template("Say hello to $ARGUMENTS.", "the whole world"), "Say hello to the whole world.");
        assert_eq!(template("First: $1. Second: $2.", "alpha \"beta gamma\" delta"), "First: alpha. Second: beta gamma delta.");
        assert_eq!(template("Head $1, tail $2", "one two three four"), "Head one, tail two three four");
        assert_eq!(template("Summarize the project.", "focus on tests"), "Summarize the project.\n\nfocus on tests");
        assert_eq!(template("Only $3 matters", "a b"), "Only  matters");
        assert_eq!(template("Summarize the project.", ""), "Summarize the project.");
        assert_eq!(arguments("[Image 1] 'a b' c\"d"), vec!["[Image 1]", "a b", "c", "d"]);
    }
}
