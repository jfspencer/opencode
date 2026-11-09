//! `packages/core/src/shell/parse.ts`: turns a command line into the permission request the shell tool makes:
//! each command's resource and the pattern an "always" reply saves, and the directories `cd` moves to.
//!
//! What TS reads from the process is a [`Host`]: the home directory, other users' homes, environment variables.

use crate::js::{self, JsString, at, len, set_has, slice, starts_with_ascii};
use crate::scan::{self, Command, Dialect, Reason, Scan};
use crate::word::{has_powershell_dot_wildcard, is_static_bash_word};
use crate::{path, select, wildcard};

/// What the TS code reads from its process while analyzing a command.
pub trait Host {
    /// `os.homedir()`: `$HOME` when set at startup, else the password database.
    fn home(&self) -> String;
    /// `resolveUserHome(user)` for `~user`: the current user's home when the name is theirs, else the
    /// `/etc/passwd` entry (the last one of that name), else on macOS `/Users/<user>` when it is a directory.
    fn user_home(&self, user: &str) -> Option<String>;
    /// `process.env[name]`, an own property: PowerShell's `$env:NAME`.
    fn env(&self, name: &str) -> Option<String>;
}

/// One permission resource of a command and its saved pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub resource: JsString,
    pub save: JsString,
}

/// TS `ShellParse.Result`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    pub commands: Vec<Resource>,
    pub directories: Vec<JsString>,
}

const CWD: &[&str] = &["cd", "chdir", "popd", "pushd", "push-location", "set-location"];

const ARITY: &[(&str, usize)] = &[
    ("cat", 1), ("cd", 1), ("chmod", 1), ("chown", 1), ("cp", 1), ("echo", 1), ("env", 1), ("export", 1),
    ("grep", 1), ("kill", 1), ("killall", 1), ("ln", 1), ("ls", 1), ("mkdir", 1), ("mv", 1), ("ps", 1), ("pwd", 1),
    ("rm", 1), ("rmdir", 1), ("sleep", 1), ("source", 1), ("tail", 1), ("touch", 1), ("unset", 1), ("which", 1),
    ("aws", 3), ("az", 3), ("bazel", 2), ("brew", 2), ("bun", 2), ("bun run", 3), ("bun x", 3), ("cargo", 2),
    ("cargo add", 3), ("cargo run", 3), ("cdk", 2), ("cf", 2), ("cmake", 2), ("composer", 2), ("consul", 2),
    ("consul kv", 3), ("crictl", 2), ("deno", 2), ("deno task", 3), ("doctl", 3), ("docker", 2),
    ("docker builder", 3), ("docker compose", 3), ("docker container", 3), ("docker image", 3),
    ("docker network", 3), ("docker volume", 3), ("eksctl", 2), ("eksctl create", 3), ("firebase", 2),
    ("flyctl", 2), ("gcloud", 3), ("gh", 3), ("git", 2), ("git config", 3), ("git remote", 3), ("git stash", 3),
    ("go", 2), ("gradle", 2), ("helm", 2), ("heroku", 2), ("hugo", 2), ("ip", 2), ("ip addr", 3), ("ip link", 3),
    ("ip netns", 3), ("ip route", 3), ("kind", 2), ("kind create", 3), ("kubectl", 2), ("kubectl kustomize", 3),
    ("kubectl rollout", 3), ("kustomize", 2), ("make", 2), ("mc", 2), ("mc admin", 3), ("minikube", 2),
    ("mongosh", 2), ("mysql", 2), ("mvn", 2), ("ng", 2), ("npm", 2), ("npm exec", 3), ("npm init", 3),
    ("npm run", 3), ("npm view", 3), ("npx", 2), ("nvm", 2), ("nx", 2), ("openssl", 2), ("openssl req", 3),
    ("openssl x509", 3), ("pip", 2), ("pipenv", 2), ("pnpm", 2), ("pnpm dlx", 3), ("pnpm exec", 3),
    ("pnpm run", 3), ("poetry", 2), ("podman", 2), ("podman container", 3), ("podman image", 3), ("psql", 2),
    ("pulumi", 2), ("pulumi stack", 3), ("python", 2), ("pyenv", 2), ("rake", 2), ("rbenv", 2), ("redis-cli", 2),
    ("rustup", 2), ("serverless", 2), ("sfdx", 3), ("skaffold", 2), ("sls", 2), ("sst", 2), ("swift", 2),
    ("systemctl", 2), ("terraform", 2), ("terraform workspace", 3), ("tmux", 2), ("turbo", 2), ("ufw", 2),
    ("vault", 2), ("vault auth", 3), ("vault kv", 3), ("vercel", 2), ("volta", 2), ("wp", 2), ("yarn", 2),
    ("yarn dlx", 3), ("yarn run", 3),
];
const PREFIX_LENGTH: usize = 3;
// Wrappers run the command after their own operands (the value counts them), so a saved prefix has to name that
// command: `sudo *` would approve every command sudo can run.
const WRAPPERS: &[(&str, usize)] = &[
    ("bash", 0), ("builtin", 0), ("command", 0), ("coproc", 0), ("dash", 0), ("doas", 0), ("env", 0), ("exec", 0),
    ("ksh", 0), ("nice", 0), ("nohup", 0), ("sh", 0), ("sudo", 0), ("time", 0), ("timeout", 1), ("xargs", 0),
    ("zsh", 0),
];

fn lookup(table: &[(&str, usize)], key: &[u16]) -> Option<usize> {
    table.iter().find(|(name, _)| js::eq_ascii(key, name)).map(|&(_, value)| value)
}

#[derive(Clone)]
struct Part {
    parameter: bool,
    text: JsString,
    raw: JsString,
    unquoted_tilde: bool,
}

/// `ShellParse.scan(command, shell, cwd)`. `Err` is the scanner's reason; TS fails with
/// `Portable shell scanner cannot analyze command: <reason>` (see [`error_message`]).
pub fn analyze_units(command: &[u16], shell: &[u16], cwd: &[u16], host: &dyn Host) -> Result<Analysis, Reason> {
    let powershell = select::ps_units(shell);
    let shell_name = select::name_units(shell);
    let scanned = if powershell {
        scan::scan_powershell(command)
    } else {
        let dialect = if js::eq_ascii(&shell_name, "bash") {
            Dialect::Bash
        } else if js::eq_ascii(&shell_name, "zsh") {
            Dialect::Zsh
        } else {
            Dialect::Posix
        };
        scan::scan(command, dialect)
    };
    let commands = match scanned {
        Scan::Opaque(reason) => return Err(reason),
        Scan::Scanned(commands) => commands,
    };

    let mut output = Analysis::default();
    for item in &commands {
        // Declarations are skipped as standalone permission resources, while nested substitutions remain.
        if item.declaration {
            continue;
        }
        if let Some(end) = item.redirect_prefix_end {
            let head = slice(&item.resource, 0, end);
            let separator = match at(&item.resource, end) {
                Some(c) if c == b'\t' as u16 || c == b'\n' as u16 => c,
                _ => b' ' as u16,
            };
            let mut save = wildcard::escape_units(head);
            save.extend_from_slice(&[separator, b'*' as u16]);
            output.commands.push(Resource { resource: item.resource.clone(), save });
            continue;
        }
        let words: Vec<JsString> = match item.redirect_word_count {
            None => item.raw_words.clone(),
            Some(count) => item.raw_words[..(count.clamp(0, item.raw_words.len() as isize) as usize)].to_vec(),
        };
        // Bare statement-head foreach blocks act as PowerShell control flow rather than cmdlet invocations.
        if powershell
            && item.statement_head
            && words.first().is_some_and(|w| foreach_head(w))
            && words.len() > 1
            && words[1..].iter().all(|word| starts_with_ascii(word, "{", 0))
        {
            continue;
        }
        let parts = if powershell { powershell_parts(item, &words) } else { bash_parts(item) };
        // By value: a quoted or escaped `cd` (`'cd' /tmp`, `c\d /tmp`) still changes the directory, which
        // external_directory rules check. Written that way, the command also stays under shell authorization.
        let directory_words =
            if powershell { words.iter().map(|word| word.as_slice()).collect() } else { values(item, &item.raw_words) };
        if let Some((offset, identity)) = unwrap_directory_command(&directory_words, powershell) {
            let resolved = directory_args(&parts[offset.min(parts.len())..], powershell, cwd, shell, host);
            output.directories.extend(resolved.directories);
            let plain = powershell || item.raw_words.first() == item.words.first();
            if identity && resolved.complete && plain {
                continue;
            }
        }
        let selected = if powershell {
            Some(prefix(&words, &words.iter().map(|word| word.as_slice()).collect::<Vec<_>>()))
        } else {
            bash_prefix(&words, &values(item, &words))
        };
        // Command text is escaped, so only the added trailing `*` is a wildcard: `r? x` must not approve `rm`.
        let prefixed = selected.as_ref().map(|selected| {
            let mut pattern = wildcard::escape_units(&js::join(selected, &[b' ' as u16]));
            pattern.extend_from_slice(&[b' ' as u16, b'*' as u16]);
            pattern
        });
        // A command that only the shell can identify is saved exactly as approved. So is one whose resource starts with
        // assignments or redirects (`FOO=1 git status`): a prefix of its words would never match it, and dropping them
        // would approve the command under any other environment.
        let conventional = match prefixed {
            Some(prefixed) if powershell || wildcard::matches_units(&item.resource, &prefixed) => prefixed,
            _ => wildcard::escape_units(&item.resource),
        };
        let end = selected.as_ref().and_then(|selected| {
            let index = selected.len() as isize - 1;
            item.word_ends.as_ref().and_then(|ends| if index < 0 { None } else { ends.get(index as usize).copied() })
        });
        // Keep existing grants stable unless normalized spacing loses the original source boundary.
        let save = match end {
            Some(end) if powershell && !wildcard::matches_units(&item.resource, &conventional) => {
                if js::is(at(&item.resource, end), b'(') {
                    let mut save = wildcard::escape_units(slice(&item.resource, 0, end));
                    save.extend_from_slice(&[b'(' as u16, b'*' as u16]);
                    save
                } else {
                    let resource_len = len(&item.resource);
                    let boundary = item
                        .word_ends
                        .as_ref()
                        .and_then(|ends| {
                            ends.iter().copied().find(|&value| {
                                value >= end && (value >= resource_len || js::is_space_opt(at(&item.resource, value)))
                            })
                        })
                        .unwrap_or(end);
                    let separator = leading_separator(js::slice_from(&item.resource, boundary));
                    let mut save = wildcard::escape_units(slice(&item.resource, 0, boundary));
                    save.extend_from_slice(separator.unwrap_or(&[b' ' as u16]));
                    save.push(b'*' as u16);
                    save
                }
            }
            _ => conventional,
        };
        output.commands.push(Resource { resource: item.resource.clone(), save });
    }
    Ok(output)
}

/// Each of a Bash command's `words` (its raw words, or the first of them) as the scanner unquoted it:
/// `words.map((raw, index) => item.words[index] ?? raw)`.
fn values<'a>(item: &'a Command, words: &'a [JsString]) -> Vec<&'a [u16]> {
    words.iter().enumerate().map(|(index, raw)| item.words.get(index).unwrap_or(raw).as_slice()).collect()
}

/// The error TS raises for a command the scanner cannot analyze.
pub fn error_message(reason: Reason) -> String {
    format!("Portable shell scanner cannot analyze command: {reason}")
}

/// `/^\s+(?:`(?:\r\n|\r|\n)\s*)?/`: the whitespace (and an escaped line break) after a PowerShell token.
fn leading_separator(s: &[u16]) -> Option<&[u16]> {
    let mut k = s.iter().take_while(|&&c| js::is_space(c)).count();
    if k == 0 {
        return None;
    }
    if s.get(k) == Some(&(b'`' as u16)) {
        let newline = match (s.get(k + 1), s.get(k + 2)) {
            (Some(&0x0d), Some(&0x0a)) => 2,
            (Some(&0x0d), _) | (Some(&0x0a), _) => 1,
            _ => 0,
        };
        if newline > 0 {
            k += 1 + newline;
            k += s[k..].iter().take_while(|&&c| js::is_space(c)).count();
        }
    }
    Some(&s[..k])
}

/// `/^foreach(?:-|$)/i`
fn foreach_head(word: &[u16]) -> bool {
    js::starts_with_ignore_case(word, 0, "foreach") && (word.len() == 7 || word[7] == b'-' as u16)
}

fn powershell_parts(item: &Command, words: &[JsString]) -> Vec<Part> {
    let mut parts = Vec::new();
    for (index, raw) in words.iter().enumerate() {
        let escaped_dollar = js::units("`$");
        let text = if !js::includes_seq(raw, &escaped_dollar) && raw.iter().any(|&c| set_has("'\"`", c)) {
            item.words.get(index).cloned().unwrap_or_else(|| raw.clone())
        } else {
            js::replace_seq(raw, &escaped_dollar, &[0])
        };
        if !powershell_parameter(raw) {
            parts.push(Part { parameter: false, text, raw: raw.clone(), unquoted_tilde: false });
            continue;
        }
        let colon = js::index_of(&text, b':' as u16, 0);
        if colon >= 0 {
            let raw_colon = js::index_of(raw, b':' as u16, 0);
            if colon + 1 < len(&text) {
                parts.push(Part {
                    parameter: true,
                    text: slice(&text, 0, colon).to_vec(),
                    raw: slice(raw, 0, raw_colon).to_vec(),
                    unquoted_tilde: false,
                });
                parts.push(Part {
                    parameter: false,
                    text: js::slice_from(&text, colon + 1).to_vec(),
                    raw: if raw_colon >= 0 { js::slice_from(raw, raw_colon + 1).to_vec() } else { raw.clone() },
                    unquoted_tilde: false,
                });
            } else {
                parts.push(Part { parameter: true, text: slice(&text, 0, colon).to_vec(), raw: raw.clone(), unquoted_tilde: false });
            }
            continue;
        }
        parts.push(Part { parameter: true, text, raw: raw.clone(), unquoted_tilde: false });
    }
    parts
}

/// `/^-[A-Za-z_?][\w?-]*(?::|$)/`
fn powershell_parameter(raw: &[u16]) -> bool {
    if raw.first() != Some(&(b'-' as u16)) || !raw.get(1).is_some_and(|&c| js::is_ident_start(c) || c == b'?' as u16) {
        return false;
    }
    let end = 2 + raw[2..].iter().take_while(|&&c| js::is_word(c) || c == b'?' as u16 || c == b'-' as u16).count();
    end == raw.len() || raw[end] == b':' as u16
}

fn bash_parts(item: &Command) -> Vec<Part> {
    item.raw_words
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            let word = item.words.get(index).unwrap_or(raw);
            Part {
                parameter: false,
                text: if is_static_bash_word(raw) { js::replace_unit(word, b'$' as u16, &[0]) } else { vec![b'$' as u16] },
                raw: raw.clone(),
                unquoted_tilde: raw.first() == Some(&(b'~' as u16)),
            }
        })
        .collect()
}

/// The offset of the directory command within the parts, and whether it is the whole command (TS returns `parts`
/// itself then, and a slice of it after `builtin` / `command`). `words` are PowerShell's raw words, or the values
/// of Bash's.
fn unwrap_directory_command(words: &[&[u16]], powershell: bool) -> Option<(usize, bool)> {
    let head = words.first().map(|w| if powershell { js::to_lower(w) } else { w.to_vec() });
    if head.is_some_and(|head| CWD.iter().any(|name| js::eq_ascii(&head, name))) {
        return Some((0, true));
    }
    if powershell {
        return None;
    }
    let word = |index: usize| words.get(index).copied();
    let mut index = 0;
    while let Some(wrapper) = word(index).filter(|w| js::eq_ascii(w, "builtin") || js::eq_ascii(w, "command")) {
        let command = js::eq_ascii(wrapper, "command");
        index += 1;
        while let Some(flag) = word(index).filter(|w| starts_with_ascii(w, "-", 0)) {
            index += 1;
            if js::eq_ascii(flag, "--") {
                break;
            }
            // /^-[p]*[vV]/
            let ps = 1 + flag[1..].iter().take_while(|&&c| c == b'p' as u16).count();
            if command && flag.get(ps).is_some_and(|&c| c == b'v' as u16 || c == b'V' as u16) {
                return None;
            }
        }
    }
    (index > 0 && word(index).is_some_and(|w| CWD.iter().any(|name| js::eq_ascii(w, name)))).then_some((index, false))
}

struct Directories {
    directories: Vec<JsString>,
    complete: bool,
}

fn directory_args(command: &[Part], powershell: bool, cwd: &[u16], shell: &[u16], host: &dyn Host) -> Directories {
    let head = command.first().map(|part| js::to_lower(&part.text)).unwrap_or_default();
    if js::eq_ascii(&head, "popd") || js::eq_ascii(&head, "pop-location") {
        return Directories { directories: Vec::new(), complete: false };
    }

    if !powershell {
        let mut end_of_options = false;
        let operands: Vec<&Part> = command
            .iter()
            .skip(1)
            .filter(|part| {
                if end_of_options {
                    return true;
                }
                if js::eq_ascii(&part.text, "--") {
                    end_of_options = true;
                    return false;
                }
                if js::eq_ascii(&part.text, "-") {
                    return true;
                }
                !starts_with_ascii(&part.text, "-", 0)
            })
            .collect();
        if operands.is_empty() {
            return Directories { directories: Vec::new(), complete: false };
        }
        let directories: Vec<JsString> = operands
            .iter()
            .filter_map(|part| {
                if js::eq_ascii(&part.text, "-") {
                    None
                } else {
                    directory_argument(&part.text, false, cwd, shell, part.unquoted_tilde, host)
                }
            })
            .collect();
        let complete = directories.len() == operands.len();
        return Directories { directories, complete };
    }

    let mut directories = Vec::new();
    let mut path_flag = false;
    let mut literal_flag = false;
    let mut end_of_parameters = false;
    let mut saw_operand = false;
    let mut complete = true;
    for part in command.iter().skip(1) {
        if !end_of_parameters && js::eq_ascii(&part.text, "--") {
            end_of_parameters = true;
            continue;
        }
        if path_flag {
            saw_operand = true;
            let value = if !literal_flag
                && (has_powershell_dot_wildcard(&part.raw) || js::eq_ascii(&part.text, "-") || js::eq_ascii(&part.text, "+"))
            {
                None
            } else {
                directory_argument(&part.text, true, cwd, shell, false, host)
            };
            match value {
                Some(value) if !value.is_empty() => directories.push(value),
                _ => complete = false,
            }
            path_flag = false;
            literal_flag = false;
            continue;
        }
        if !end_of_parameters && part.parameter {
            if stack_flag(&part.text) {
                return Directories { directories, complete: false };
            }
            literal_flag = literal_path_flag(&part.text);
            path_flag = literal_flag || path_flag_name(&part.text);
            continue;
        }
        saw_operand = true;
        let value = if has_powershell_dot_wildcard(&part.raw)
            || (!end_of_parameters && (js::eq_ascii(&part.raw, "-") || js::eq_ascii(&part.raw, "+")))
        {
            None
        } else {
            directory_argument(&part.text, true, cwd, shell, false, host)
        };
        match value {
            Some(value) if !value.is_empty() => directories.push(value),
            _ => complete = false,
        }
    }
    Directories { directories, complete: saw_operand && complete }
}

/// An optional trailing `:` after a flag name.
fn flag_name(text: &[u16]) -> Option<&[u16]> {
    if text.first() != Some(&(b'-' as u16)) {
        return None;
    }
    let body = &text[1..];
    Some(if body.last() == Some(&(b':' as u16)) { &body[..body.len() - 1] } else { body })
}

fn flag_in(text: &[u16], names: &[&str]) -> bool {
    flag_name(text).is_some_and(|name| {
        names.iter().any(|candidate| {
            name.len() == candidate.len() && candidate.bytes().zip(name).all(|(b, &c)| js::eq_ignore_ascii_case(c, b))
        })
    })
}

/// `POWERSHELL_LITERAL_FLAG_RE`: `-lp`, `-l` through `-literalpath`, `-psp` through `-pspath`, ignoring case.
fn literal_path_flag(text: &[u16]) -> bool {
    let literal = "literalpath";
    let pspath = "pspath";
    let mut names: Vec<&str> = (1..=literal.len()).map(|n| &literal[..n]).collect();
    names.extend((3..=pspath.len()).map(|n| &pspath[..n]));
    names.push("lp");
    flag_in(text, &names)
}

/// `POWERSHELL_STACK_FLAG_RE = /^-(?:stack(?:name|nam|na|n)?|st):?$/i`
fn stack_flag(text: &[u16]) -> bool {
    flag_in(text, &["stack", "stackname", "stacknam", "stackna", "stackn", "st"])
}

/// `/^-p(?:ath|at|a)?:?$/i`
fn path_flag_name(text: &[u16]) -> bool {
    flag_in(text, &["p", "pa", "pat", "path"])
}

fn directory_argument(
    value: &[u16],
    powershell: bool,
    cwd: &[u16],
    shell: &[u16],
    unquoted_tilde: bool,
    host: &dyn Host,
) -> Option<JsString> {
    if !powershell {
        return expand_known_directory(value, false, cwd, unquoted_tilde, host);
    }
    let quote = value.first().copied();
    let text = if (quote == Some(b'"' as u16) || quote == Some(b'\'' as u16)) && value.last().copied() == quote {
        slice(value, 1, -1)
    } else {
        value
    };
    // PowerShell exposes environment variables through $env:NAME and provides these
    // automatic directory variables. Expand only values we can determine without executing code.
    let text = replace_braced_env(text, host);
    let text = replace_env(&text, host);
    let text = replace_automatic(&text, cwd, shell, host);
    expand_known_directory(&text, true, cwd, false, host)
}

/// `environment(key) ?? ""`: an own key of the environment only, so `$env:toString` names no variable rather than
/// an `Object.prototype` member.
fn environment(key: &[u16], host: &dyn Host) -> JsString {
    host.env(&js::text(key)).map(|value| js::units(&value)).unwrap_or_default()
}

/// `text.replace(/\$\{env:([^}]+)\}/gi, (_, key) => environment(key) ?? "")`
fn replace_braced_env(text: &[u16], host: &dyn Host) -> JsString {
    let mut out = JsString::new();
    let mut i = 0;
    while i < text.len() {
        if starts_with_ascii(text, "${", i as isize) && js::starts_with_ignore_case(text, i as isize + 2, "env:") {
            let key_start = i + 6;
            let close = js::index_of(text, b'}' as u16, key_start as isize);
            if close > key_start as isize {
                out.extend_from_slice(&environment(&text[key_start..close as usize], host));
                i = close as usize + 1;
                continue;
            }
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

/// `text.replace(/\$env:([A-Za-z_][A-Za-z0-9_]*)/gi, (_, key) => environment(key) ?? "")`
fn replace_env(text: &[u16], host: &dyn Host) -> JsString {
    let mut out = JsString::new();
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'$' as u16
            && js::starts_with_ignore_case(text, i as isize + 1, "env:")
            && text.get(i + 5).is_some_and(|&c| js::is_ident_start(c))
        {
            let end = i + 6 + text[i + 6..].iter().take_while(|&&c| js::is_word(c)).count();
            out.extend_from_slice(&environment(&text[i + 5..end], host));
            i = end;
            continue;
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

/// `text.replace(/\$(?:\{(HOME|PWD|PSHOME)\}|(HOME|PWD|PSHOME)(?=$|[\\/]))/gi, ...)`
fn replace_automatic(text: &[u16], cwd: &[u16], shell: &[u16], host: &dyn Host) -> JsString {
    let names = ["HOME", "PWD", "PSHOME"];
    let value = |name: &str| -> JsString {
        match name {
            "HOME" => js::units(&host.home()),
            "PWD" => cwd.to_vec(),
            _ => path::dirname(shell),
        }
    };
    let mut out = JsString::new();
    let mut i = 0;
    'scan: while i < text.len() {
        if text[i] == b'$' as u16 {
            let at_index = i as isize;
            if js::is(at(text, at_index + 1), b'{') {
                for name in names {
                    let end = at_index + 2 + name.len() as isize;
                    if js::starts_with_ignore_case(text, at_index + 2, name) && js::is(at(text, end), b'}') {
                        out.extend_from_slice(&value(name));
                        i = end as usize + 1;
                        continue 'scan;
                    }
                }
            }
            for name in names {
                let end = at_index + 1 + name.len() as isize;
                if js::starts_with_ignore_case(text, at_index + 1, name)
                    && (end == len(text) || js::set_has_opt("\\/", at(text, end)))
                {
                    out.extend_from_slice(&value(name));
                    i = end as usize;
                    continue 'scan;
                }
            }
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

fn expand_known_directory(
    value: &[u16],
    powershell: bool,
    cwd: &[u16],
    unquoted_tilde: bool,
    host: &dyn Host,
) -> Option<JsString> {
    // Unknown shell expressions cannot be resolved safely during permission analysis.
    if js::includes(value, b'$' as u16)
        || js::includes(value, b'`' as u16)
        || (powershell && starts_with_ascii(value, "(", 0))
    {
        return None;
    }
    let resolved = js::replace_unit(value, 0, &[b'$' as u16]);
    let home = || js::units(&host.home());
    if js::eq_ascii(&resolved, "~") {
        return Some(home());
    }
    if starts_with_ascii(&resolved, "~/", 0) {
        return Some(path::join(&[&home(), &resolved[2..]]));
    }
    if unquoted_tilde {
        if js::eq_ascii(&resolved, "~+") {
            return Some(cwd.to_vec());
        }
        if starts_with_ascii(&resolved, "~+/", 0) {
            return Some(path::join(&[cwd, &resolved[3..]]));
        }
        // /^~(?:[-+](?:\d+)?|\d+)(?=$|\/)/
        let boundary = |k: usize| k == resolved.len() || resolved[k] == b'/' as u16;
        let digits = |from: usize| from + resolved[from..].iter().take_while(|&&c| js::is_digit(c)).count();
        let tilde = resolved.first() == Some(&(b'~' as u16));
        let directory_stack = tilde
            && match resolved.get(1) {
            Some(&c) if c == b'-' as u16 || c == b'+' as u16 => boundary(digits(2)),
            Some(&c) if js::is_digit(c) => boundary(digits(1)),
            _ => false,
        };
        if directory_stack {
            return None;
        }
        // /^~([A-Za-z_][A-Za-z0-9._-]*)(?=$|\/)/
        if tilde && resolved.get(1).is_some_and(|&c| js::is_ident_start(c)) {
            let end = 2 + resolved[2..].iter().take_while(|&&c| js::is_word(c) || set_has(".-", c)).count();
            if boundary(end) {
                let user = js::text(&resolved[1..end]);
                let home = host.user_home(&user)?;
                return Some(path::join(&[&js::units(&home), js::slice_from(&resolved, end as isize)]));
            }
        }
    }
    Some(resolved)
}

/// The command's words up to its arity, looked up by `values` (one per word): `prefix(words, values)`.
fn prefix(words: &[JsString], values: &[&[u16]]) -> Vec<JsString> {
    let tokens = &words[..words.len().min(PREFIX_LENGTH)];
    for length in (1..=tokens.len()).rev() {
        let key = values[..length].join(&(b' ' as u16));
        // Own keys only: a command named like an Object.prototype member (`toString`) has no arity entry.
        if let Some(arity) = lookup(ARITY, &key) {
            return tokens[..arity.min(tokens.len())].to_vec();
        }
    }
    tokens[..tokens.len().min(1)].to_vec()
}

/// The prefix of a POSIX command that names what it runs, or None when that depends on the shell: a dynamic
/// command name, `eval`, or options that hide where the wrapped command or the subcommand starts. Words are looked
/// up by their values (one per word), so a quoted or escaped name (`'sudo'`, `\git`) is the command it names, while
/// the prefix keeps the words as written.
fn bash_prefix(words: &[JsString], values: &[&[u16]]) -> Option<Vec<JsString>> {
    // TS recurses through each wrapper; the loop keeps the wrapper words gathered so far.
    let mut gathered: Vec<JsString> = Vec::new();
    let (mut words, mut values) = (words, values);
    loop {
        let head = words.first()?;
        if js::eq_ascii(values[0], "eval") || !is_static_bash_word(head) {
            return None;
        }
        let name = path::basename(values[0]);
        let Some(operands) = lookup(WRAPPERS, &name) else {
            // The arity table names commands, not paths: `/usr/bin/git status` is git's `status`.
            let keys: Vec<&[u16]> = std::iter::once(name.as_slice()).chain(values[1..].iter().copied()).collect();
            let selected = prefix(words, &keys);
            // An option where a subcommand belongs hides which subcommand runs: `git -C *` would approve every git command.
            if values[1..selected.len()].iter().any(|value| starts_with_ascii(value, "-", 0)) {
                return None;
            }
            gathered.extend(selected);
            return Some(gathered);
        };
        // Wrappers like env and sudo take assignments before the command; they stay part of the prefix.
        let command = words.iter().enumerate().position(|(index, word)| index > 0 && !is_assignment(word))?;
        if starts_with_ascii(values[command], "-", 0) {
            return None;
        }
        let split = (command + operands).min(words.len());
        gathered.extend_from_slice(&words[..split]);
        words = &words[split..];
        values = &values[split..];
    }
}

/// `/^[A-Za-z_][A-Za-z0-9_]*=/`
fn is_assignment(word: &[u16]) -> bool {
    word.first().is_some_and(|&c| js::is_ident_start(c)) && {
        let end = 1 + word[1..].iter().take_while(|&&c| js::is_word(c)).count();
        word.get(end) == Some(&(b'=' as u16))
    }
}

/// `FileAccess.resolvePath(directory, input, home)`: where the shell tool resolves a directory it found.
pub fn resolve_path_units(directory: &[u16], input: &[u16], home: &[u16]) -> JsString {
    let target = if js::eq_ascii(input, "~") {
        home.to_vec()
    } else if starts_with_ascii(input, "~/", 0) {
        path::join(&[home, &input[2..]])
    } else {
        input.to_vec()
    };
    // `directory` is absolute, so `process.cwd()` is never read.
    path::resolve(&[directory, &target], &[b'/' as u16])
}
