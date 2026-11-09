//! Formatters run after a file tool writes a file (packages/core/src/formatter.ts, formatter/builtins.ts,
//! config/plugin/formatter.ts). The config's `formatter` documents decide which: none at all unless one sets
//! `formatter` (`true` or an object), `false` resets everything before it, and an object merges each named formatter
//! over the same name from earlier documents. The built-ins are then registered in their order, a named one replaced
//! (its command, extensions and environment) or removed (`disabled`), and new names appended. A file is formatted by
//! the first formatter for its extension whose command is enabled and exits with 0, run in the location's
//! directory with its environment over the server's.
//!
//! The npm formatters (prettier, oxfmt, biome) run from opencode's package cache ([`crate::npm::which`], which installs
//! the package when the cache lacks it; without network access that fails and they stay disabled). TS caches an
//! enabled formatter's command until the config changes; here it is resolved on each write.

use std::collections::HashMap;

use opencode_protocol::types as t;

use crate::inventory::Entry;
use crate::location::Location;

/// A formatter (`Formatter.Info`): its command is resolved when a matching file is written.
struct Info {
    name: String,
    environment: Vec<(String, String)>,
    extensions: Vec<String>,
    enabled: Enabled,
}

#[derive(Clone)]
enum Enabled {
    /// A command from the config (`false` when it gave none).
    Command(Option<Vec<String>>),
    BuiltIn(BuiltIn),
}

#[derive(Clone, Copy)]
enum BuiltIn {
    /// An executable found on `PATH` (or opencode's bin directory), with its arguments.
    Executable(&'static str, &'static [&'static str]),
    /// An npm-installed formatter: when a `package.json` up to the worktree depends on the package, its executable
    /// with the arguments.
    NpmDependency(&'static str, &'static [&'static str]),
    /// Biome: when a `biome.json` or `biome.jsonc` is found up to the worktree.
    Biome,
    /// An executable enabled by a marker file found walking up to the worktree.
    Marker(&'static str, &'static str, &'static [&'static str]),
    Ruff,
    Air,
    Uv,
    Pint,
}

const JS: &[&str] = &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"];
const WEB: &[&str] = &[
    ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".html", ".htm", ".css", ".scss", ".sass", ".less", ".vue",
    ".svelte", ".json", ".jsonc", ".yaml", ".yml", ".toml", ".xml", ".md", ".mdx", ".graphql", ".gql",
];
const RUBY: &[&str] = &[".rb", ".rake", ".gemspec", ".ru"];

/// `make`: the built-in formatters in their registration order.
fn builtins() -> Vec<Info> {
    use BuiltIn::*;
    let bun = || vec![("BUN_BE_BUN".to_owned(), "1".to_owned())];
    let item = |name: &str, extensions: &[&str], environment: Vec<(String, String)>, enabled: BuiltIn| Info {
        name: name.to_owned(),
        environment,
        extensions: extensions.iter().map(|e| (*e).to_owned()).collect(),
        enabled: Enabled::BuiltIn(enabled),
    };
    let exe = |name: &'static str, extensions: &[&str], args: &'static [&'static str]| item(name, extensions, vec![], Executable(name, args));
    vec![
        exe("gofmt", &[".go"], &["-w", "$FILE"]),
        exe("mix", &[".ex", ".exs", ".eex", ".heex", ".leex", ".neex", ".sface"], &["format", "$FILE"]),
        item("oxfmt", JS, bun(), NpmDependency("oxfmt", &["$FILE"])),
        item("prettier", WEB, bun(), NpmDependency("prettier", &["--write", "$FILE"])),
        item("biome", WEB, bun(), Biome),
        exe("zig", &[".zig", ".zon"], &["fmt", "$FILE"]),
        item(
            "clang-format",
            &[".c", ".cc", ".cpp", ".cxx", ".c++", ".h", ".hh", ".hpp", ".hxx", ".h++", ".ino", ".C", ".H"],
            vec![],
            Marker(".clang-format", "clang-format", &["-i", "$FILE"]),
        ),
        exe("ktlint", &[".kt", ".kts"], &["-F", "$FILE"]),
        item("ruff", &[".py", ".pyi"], vec![], Ruff),
        item("air", &[".R"], vec![], Air),
        item("uv", &[".py", ".pyi"], vec![], Uv),
        exe("rubocop", RUBY, &["--autocorrect", "$FILE"]),
        exe("standardrb", RUBY, &["--fix", "$FILE"]),
        exe("htmlbeautifier", &[".erb", ".html.erb"], &["$FILE"]),
        exe("dart", &[".dart"], &["format", "$FILE"]),
        item("ocamlformat", &[".ml", ".mli"], vec![], Marker(".ocamlformat", "ocamlformat", &["-i", "$FILE"])),
        exe("terraform", &[".tf", ".tfvars"], &["fmt", "$FILE"]),
        exe("latexindent", &[".tex"], &["-w", "-s", "$FILE"]),
        exe("gleam", &[".gleam"], &["format", "$FILE"]),
        exe("shfmt", &[".sh", ".bash"], &["-w", "$FILE"]),
        exe("nixfmt", &[".nix"], &["$FILE"]),
        exe("rustfmt", &[".rs"], &["$FILE"]),
        item("pint", &[".php"], vec![], Pint),
        exe("ormolu", &[".hs"], &["-i", "$FILE"]),
        exe("cljfmt", &[".clj", ".cljs", ".cljc", ".edn"], &["fix", "--quiet", "$FILE"]),
        exe("dfmt", &[".d"], &["-i", "$FILE"]),
    ]
}

/// The merged `formatter` setting of the config documents (`resolve`).
enum Configured {
    Enabled,
    Entries(Vec<(String, t::ConfigFormatterEntry)>),
}

fn configured(entries: &[Entry]) -> Option<Configured> {
    let mut result: Option<Result<bool, Vec<(String, t::ConfigFormatterEntry)>>> = None;
    for info in crate::inventory::config::documents(entries) {
        match &info.formatter {
            None => {}
            Some(t::ConfigInfoFormatter::Bool(value)) => result = Some(Ok(*value)),
            Some(t::ConfigInfoFormatter::Record(record)) => {
                let mut merged = match result.take() {
                    Some(Err(merged)) => merged,
                    _ => Vec::new(),
                };
                for (name, item) in &record.0 {
                    let environment = |entry: Option<&t::ConfigFormatterEntry>| {
                        entry.and_then(|e| e.environment.clone()).map(|r| r.0).unwrap_or_default()
                    };
                    match merged.iter_mut().find(|(existing, _)| existing == name) {
                        Some((_, previous)) => {
                            let mut env = environment(Some(previous));
                            env.extend(environment(Some(item)));
                            *previous = t::ConfigFormatterEntry {
                                disabled: item.disabled.or(previous.disabled),
                                command: item.command.clone().or(previous.command.clone()),
                                environment: Some(opencode_protocol::wire::Record(env)),
                                extensions: item.extensions.clone().or(previous.extensions.clone()),
                            };
                        }
                        None => {
                            let mut entry = item.clone();
                            entry.environment = Some(opencode_protocol::wire::Record(environment(Some(item))));
                            merged.push((name.clone(), entry));
                        }
                    }
                }
                result = Some(Err(merged));
            }
        }
    }
    match result? {
        Ok(false) => None,
        Ok(true) => Some(Configured::Enabled),
        Err(entries) => Some(Configured::Entries(entries)),
    }
}

/// The formatter state of the config (`ConfigFormatterPlugin`'s transform).
fn formatters(entries: &[Entry]) -> Vec<Info> {
    let Some(configured) = configured(entries) else { return vec![] };
    let mut list = builtins();
    let Configured::Entries(items) = configured else { return list };
    for (name, entry) in items {
        if entry.disabled == Some(true) {
            list.retain(|formatter| formatter.name != name);
            continue;
        }
        let builtin = builtins().into_iter().find(|formatter| formatter.name == name);
        let mut environment = builtin.as_ref().map(|b| b.environment.clone()).unwrap_or_default();
        for (key, value) in entry.environment.as_ref().map(|r| r.0.clone()).unwrap_or_default() {
            match environment.iter_mut().find(|(existing, _)| *existing == key) {
                Some(slot) => slot.1 = value,
                None => environment.push((key, value)),
            }
        }
        let enabled = match (&builtin, &entry.command) {
            (Some(builtin), None) => builtin.enabled.clone(),
            _ => Enabled::Command(entry.command.clone()),
        };
        let current = Info {
            name: name.clone(),
            extensions: entry.extensions.clone().or_else(|| builtin.as_ref().map(|b| b.extensions.clone())).unwrap_or_default(),
            environment,
            enabled,
        };
        match list.iter_mut().find(|formatter| formatter.name == name) {
            Some(slot) => *slot = current,
            None => list.push(current),
        }
    }
    list
}

/// `findUp(target, directory, worktree)`: every match walking up.
fn find_up(target: &str, start: &str, stop: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = start.to_owned();
    loop {
        let candidate = format!("{}/{target}", current.trim_end_matches('/'));
        if std::path::Path::new(&candidate).exists() {
            found.push(candidate);
        }
        if crate::project::normalize(stop) == crate::project::normalize(&current) {
            break;
        }
        match std::path::Path::new(&current).parent() {
            Some(parent) => current = parent.to_string_lossy().into_owned(),
            None => break,
        }
    }
    found
}

fn bin() -> String {
    let cache = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.cache", std::env::var("HOME").unwrap_or_default()));
    format!("{cache}/opencode/bin")
}

fn which(name: &str) -> Option<String> {
    crate::process::which(name, Some(&bin()))
}

fn with_args(binary: String, args: &[&str]) -> Vec<String> {
    std::iter::once(binary).chain(args.iter().map(|a| (*a).to_owned())).collect()
}

/// `commandOutput`: a command's exit code and stdout, or `None` when it cannot run.
async fn output(command: &[String], directory: &str) -> Option<(i32, Vec<u8>)> {
    let result = tokio::process::Command::new(&command[0])
        .args(&command[1..])
        .current_dir(directory)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    Some((result.status.code().unwrap_or(-1), result.stdout))
}

/// `formatter.enabled`: the command to run, or `None` when the formatter is disabled.
async fn command(enabled: &Enabled, location: &Location) -> Option<Vec<String>> {
    let (directory, worktree) = (location.directory.as_str(), location.project.directory.as_str());
    let read = |file: &str| std::fs::read_to_string(file).unwrap_or_default();
    match enabled {
        Enabled::Command(command) => command.clone(),
        Enabled::BuiltIn(builtin) => match *builtin {
            BuiltIn::Executable(name, args) => which(name).map(|bin| with_args(bin, args)),
            BuiltIn::NpmDependency(package, args) => {
                for file in find_up("package.json", directory, worktree) {
                    // A `package.json` that cannot be read as JSON disables the formatter.
                    let manifest = crate::json::parse(&std::fs::read_to_string(&file).ok()?)?;
                    let depends = ["dependencies", "devDependencies"].iter().any(|field| {
                        matches!(crate::json::get(&manifest, field), Some(opencode_protocol::Json::Object(o)) if o.contains_key(package))
                    });
                    if !depends {
                        continue;
                    }
                    if let Some(bin) = crate::npm::which(package, None).await {
                        return Some(with_args(bin, args));
                    }
                }
                None
            }
            BuiltIn::Biome => {
                if ["biome.json", "biome.jsonc"].iter().all(|name| find_up(name, directory, worktree).is_empty()) {
                    return None;
                }
                crate::npm::which("@biomejs/biome", None).await.map(|bin| with_args(bin, &["format", "--write", "$FILE"]))
            }
            BuiltIn::Marker(marker, name, args) => {
                if find_up(marker, directory, worktree).is_empty() {
                    return None;
                }
                which(name).map(|bin| with_args(bin, args))
            }
            BuiltIn::Ruff => {
                let bin = which("ruff")?;
                let format = || Some(with_args(bin.clone(), &["format", "$FILE"]));
                for config in ["pyproject.toml", "ruff.toml", ".ruff.toml"] {
                    let found = find_up(config, directory, worktree);
                    let Some(first) = found.first() else { continue };
                    if config != "pyproject.toml" || read(first).contains("[tool.ruff]") {
                        return format();
                    }
                }
                for dependency in ["requirements.txt", "pyproject.toml", "Pipfile"] {
                    if let Some(first) = find_up(dependency, directory, worktree).first() {
                        if read(first).contains("ruff") {
                            return format();
                        }
                    }
                }
                None
            }
            BuiltIn::Air => {
                let bin = which("air")?;
                let (code, stdout) = output(&[bin.clone(), "--help".into()], directory).await?;
                if code != 0 {
                    return None;
                }
                let text = String::from_utf8_lossy(&stdout);
                let first = text.split('\n').next().unwrap_or("");
                (first.contains("R language") && first.contains("formatter")).then(|| with_args(bin, &["format", "$FILE"]))
            }
            BuiltIn::Uv => {
                let bin = which("uv")?;
                let (code, _) = output(&[bin.clone(), "format".into(), "--help".into()], directory).await?;
                (code == 0).then(|| with_args(bin, &["format", "--", "$FILE"]))
            }
            BuiltIn::Pint => {
                for file in find_up("composer.json", directory, worktree) {
                    let json = crate::json::parse(&read(&file))?;
                    let has = |field: &str| {
                        matches!(crate::json::get(&json, field), Some(opencode_protocol::Json::Object(o)) if o.contains_key("laravel/pint"))
                    };
                    if has("require") || has("require-dev") {
                        return Some(vec!["./vendor/bin/pint".into(), "$FILE".into()]);
                    }
                }
                None
            }
        },
    }
}

/// `Formatter.file`: formats a file the way the config says; whether a formatter ran successfully.
pub async fn file(location: &Location, filepath: &str) -> bool {
    let extension = crate::text::extname(filepath);
    let inventory = location.inventory();
    let matching: Vec<Info> =
        formatters(&inventory.entries).into_iter().filter(|f| f.extensions.iter().any(|e| e == extension)).collect();
    drop(inventory);
    for formatter in matching {
        let Some(command) = command(&formatter.enabled, location).await else { continue };
        if command.is_empty() {
            continue;
        }
        // `String.prototype.replace` with a string pattern: the first occurrence only.
        let command: Vec<String> = command.iter().map(|argument| argument.replacen("$FILE", filepath, 1)).collect();
        let environment: HashMap<String, String> = formatter.environment.iter().cloned().collect();
        let status = tokio::process::Command::new(&command[0])
            .args(&command[1..])
            .current_dir(&location.directory)
            .envs(&environment)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status()
            .await;
        match status {
            Ok(status) if status.success() => return true,
            Ok(status) => eprintln!("formatter exited unsuccessfully: {} ({:?})", formatter.name, status.code()),
            Err(error) => eprintln!("failed to format file {filepath}: {error}"),
        }
    }
    false
}
