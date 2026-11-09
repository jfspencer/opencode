//! `opencode upgrade` and `opencode uninstall` (commands/handlers/upgrade.ts, uninstall.ts) over the installation
//! `Updater` (services/updater.ts, updater-action.ts), in clack's frames.
//!
//! The installation method is detected as TS does: the executable is the curl installer's (`~/.opencode/bin/opencode`),
//! inside a Homebrew Cellar or a WinGet package directory, or an npm package's bin that a package manager lists. An
//! executable installed none of these ways (a Rust build) has no method: `upgrade` asks for `--method`, `uninstall`
//! only stops the services and removes the cache. With a method, the package manager or installer runs exactly as TS
//! runs it, with the update service's answer (`opencode.ai/update/api`) naming the package and version.

use std::time::Duration;

use opencode_protocol::Json;

use super::clack::Clack;
use super::out::Result;
use super::parse::Input;
use crate::json;

/// `OPENCODE_ARTIFACT` (the build's distribution; `cli` unless the build defines it).
const ARTIFACT: &str = match option_env!("OPENCODE_ARTIFACT") {
    Some(artifact) => artifact,
    None => "cli",
};

const WINGET_PACKAGE: &str = "Anomaly.OpenCode";
const WINGET_ARGS: &[&str] =
    &["--id", WINGET_PACKAGE, "--exact", "--source", "winget", "--accept-source-agreements", "--disable-interactivity"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Method {
    Curl,
    Npm,
    Pnpm,
    Bun,
    Yarn,
    Vp,
    Brew,
    Winget,
}

impl Method {
    fn parse(name: &str) -> Option<Method> {
        Some(match name {
            "curl" => Method::Curl,
            "npm" => Method::Npm,
            "pnpm" => Method::Pnpm,
            "bun" => Method::Bun,
            "yarn" => Method::Yarn,
            "vp" => Method::Vp,
            "brew" => Method::Brew,
            "winget" => Method::Winget,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Method::Curl => "curl",
            Method::Npm => "npm",
            Method::Pnpm => "pnpm",
            Method::Bun => "bun",
            Method::Yarn => "yarn",
            Method::Vp => "vp",
            Method::Brew => "brew",
            Method::Winget => "winget",
        }
    }

    /// `installNames`.
    fn install_name(self) -> &'static str {
        match self {
            Method::Curl => "The OpenCode installer",
            Method::Npm => "npm",
            Method::Pnpm => "pnpm",
            Method::Bun => "Bun",
            Method::Yarn => "Yarn",
            Method::Vp => "Vite+",
            Method::Brew => "Homebrew",
            Method::Winget => "WinGet",
        }
    }
}

/// `Updater.UpgradeError`.
struct UpgradeError {
    title: String,
    detail: String,
    command: Option<String>,
    retry: String,
}

/// A failure of the updater: an `UpgradeError`, or a plain `Error` with its message.
enum Failure {
    Upgrade(UpgradeError),
    Plain(String),
}

impl Failure {
    fn upgrade(title: &str, detail: impl Into<String>, command: Option<String>, retry: &str) -> Self {
        Failure::Upgrade(UpgradeError { title: title.into(), detail: detail.into(), command, retry: retry.into() })
    }

    /// The handler's `mapError`: an `UpgradeError` becomes an `Error` whose message is `formatUpgradeError`'s.
    fn message(&self) -> String {
        match self {
            Failure::Plain(message) => message.clone(),
            Failure::Upgrade(error) => format_upgrade_error(error),
        }
    }
}

/// What `AppProcess.run` returned.
struct RunResult {
    code: i32,
    stdout: String,
    stderr: String,
}

/// `AppProcessError` without output (the command did not start or timed out): `errorDetail` of it.
struct ExecError {
    detail: String,
}

/// `exec`: `AppProcess.run` with a timeout and 100 kB of each stream kept.
async fn exec(command: &[String], timeout: Duration) -> std::result::Result<RunResult, ExecError> {
    use tokio::io::AsyncReadExt;
    let description = command.join(" ");
    let mut child = match tokio::process::Command::new(&command[0])
        .args(&command[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            // `PlatformError` (`NotFound: ChildProcess.spawn (...)`) with its cause's message.
            let detail = match error.kind() {
                std::io::ErrorKind::NotFound => format!(
                    "NotFound: ChildProcess.spawn ({description}): Executable not found in $PATH: \"{}\"",
                    command[0]
                ),
                std::io::ErrorKind::PermissionDenied => {
                    format!("PermissionDenied: ChildProcess.spawn ({description}): {error}")
                }
                _ => format!("Unknown: ChildProcess.spawn ({description}): {error}"),
            };
            return Err(ExecError { detail });
        }
    };
    let collect = |pipe: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>| async move {
        let mut kept = Vec::new();
        if let Some(mut pipe) = pipe {
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let room = 100_000usize.saturating_sub(kept.len());
                        kept.extend_from_slice(&chunk[..n.min(room)]);
                    }
                }
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    };
    let stdout = child.stdout.take().map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
    let stderr = child.stderr.take().map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
    let run = async {
        let (stdout, stderr, status) = tokio::join!(collect(stdout), collect(stderr), child.wait());
        let code = status.ok().and_then(|s| s.code()).unwrap_or(1);
        RunResult { code, stdout, stderr }
    };
    tokio::time::timeout(timeout, run).await.map_err(|_| ExecError { detail: "Timed out".into() })
}

/// `stripVTControlCharacters` (Node's ANSI pattern).
fn strip_vt(text: &str) -> String {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"[\x1b\x{9b}][\[\]()#;?]*(?:(?:(?:(?:;[-a-zA-Z\d/#&.:=?%@~_]+)*|[a-zA-Z\d]+(?:;[-a-zA-Z\d/#&.:=?%@~_]*)*)?(?:\x07|\x1b\\|\x{9c}))|(?:(?:\d{1,4}(?:;\d{0,4})*)?[\dA-PR-TZcf-ntqry=><~]))",
        )
        .expect("ANSI pattern")
    });
    pattern.replace_all(text, "").into_owned()
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `conciseDetail`: the last 12 non-blank lines, at most 2,000 UTF-16 units.
fn concise_detail(input: &str) -> Option<String> {
    let stripped = strip_vt(input);
    let normalized = stripped.trim().replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').map(|line| line.trim_end()).filter(|line| !line.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(12)..].join("\n");
    let units: Vec<u16> = tail.encode_utf16().collect();
    let clipped = units.len() > 2_000;
    let detail = if clipped { format!("…{}", String::from_utf16_lossy(&units[units.len() - 1_999..])) } else { tail };
    if detail.is_empty() {
        return None;
    }
    if lines.len() <= 12 && !clipped {
        return Some(detail);
    }
    Some(format!("{detail}\n\nOutput shortened to the last 12 lines."))
}

/// `resultDetail`.
fn result_detail(result: &RunResult) -> String {
    concise_detail(&result.stderr)
        .or_else(|| concise_detail(&result.stdout))
        .unwrap_or_else(|| format!("The command exited with code {} without any error output.", result.code))
}

/// `process.stdout.columns ?? 80`.
fn columns() -> usize {
    if super::stdout_tty() {
        // SAFETY: TIOCGWINSZ fills the zeroed struct.
        unsafe {
            let mut size: libc::winsize = std::mem::zeroed();
            if libc::ioctl(1, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_col > 0 {
                return size.ws_col as usize;
            }
        }
    }
    80
}

/// upgrade.ts `wrap`: lines longer than `width` are refilled word by word.
fn wrap(value: &str, width: usize) -> Vec<String> {
    let mut out = vec![];
    for source in value.split('\n') {
        if utf16_len(source) <= width {
            out.push(source.to_owned());
            continue;
        }
        let mut result: Vec<String> = vec![];
        for word in split_whitespace_js(source) {
            match result.last_mut() {
                Some(last) if utf16_len(last) + utf16_len(word) + 1 <= width => {
                    last.push(' ');
                    last.push_str(word);
                }
                _ => result.push(word.to_owned()),
            }
        }
        out.extend(result);
    }
    out
}

/// `source.split(/\s+/)`: empty words at the ends are kept, as JS keeps them.
fn split_whitespace_js(text: &str) -> Vec<&str> {
    let mut words = vec![];
    let mut start = 0;
    let mut in_space = false;
    let mut space_start = 0;
    for (index, ch) in text.char_indices() {
        let space = crate::text::is_js_whitespace(ch);
        if space && !in_space {
            space_start = index;
            in_space = true;
        } else if !space && in_space {
            words.push(&text[start..space_start]);
            start = index;
            in_space = false;
        }
    }
    words.push(if in_space { &text[start..space_start] } else { &text[start..] });
    if in_space {
        words.push("");
    }
    words
}

/// `formatUpgradeError`.
fn format_upgrade_error(error: &UpgradeError) -> String {
    let clean = |value: &str| strip_vt(value).replace('\r', "").trim().to_owned();
    let line = |value: &str| {
        let cleaned = clean(value);
        let mut out = String::new();
        let mut space = false;
        for ch in cleaned.chars() {
            if crate::text::is_js_whitespace(ch) {
                if !space {
                    out.push(' ');
                }
                space = true;
            } else {
                out.push(ch);
                space = false;
            }
        }
        out
    };
    let columns = columns();
    let field = |label: &str, value: &str| {
        let prefix = format!("{label:<9}");
        let width = (columns as i64 - utf16_len(&prefix) as i64 - 10).clamp(i64::MIN, 61).max(24) as usize;
        wrap(value, width)
            .iter()
            .enumerate()
            .map(|(index, text)| if index == 0 { format!("{prefix}{text}") } else { format!("{}{text}", " ".repeat(utf16_len(&prefix))) })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let detail = wrap(&clean(&error.detail), (columns as i64 - 7).max(24) as usize).join("\n");
    let mut parts = vec![line(&error.title), String::new(), detail];
    if let Some(command) = &error.command {
        parts.push(String::new());
        parts.push(field("Command", &line(command)));
    }
    parts.push(field("Retry", &line(&error.retry)));
    parts.join("\n")
}

/// `parseReleaseVersion`: a semantic version (with or without a leading `v`), each core part at most 2^53 - 1.
fn parse_release_version(input: &str) -> bool {
    if utf16_len(input) > 256 {
        return false;
    }
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"^v?([0-9]+)\.([0-9]+)\.([0-9]+)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$",
        )
        .expect("version pattern")
    });
    let trimmed = crate::text::trim(input);
    let Some(found) = pattern.captures(trimmed) else { return false };
    const MAXIMUM: &str = "9007199254740991";
    let invalid = |value: &str| {
        if value.len() > 1 && value.starts_with('0') {
            return true;
        }
        if value.len() != MAXIMUM.len() {
            return value.len() > MAXIMUM.len();
        }
        value > MAXIMUM
    };
    if [1, 2, 3].iter().any(|&i| invalid(&found[i])) {
        return false;
    }
    if let Some(pre) = found.get(4) {
        if pre.as_str().split('.').any(|id| id.len() > 1 && id.starts_with('0') && id.bytes().all(|b| b.is_ascii_digit())) {
            return false;
        }
    }
    true
}

/// `target.trim().replace(/^v/, "")`.
fn bare_version(target: &str) -> String {
    let trimmed = crate::text::trim(target);
    trimmed.strip_prefix('v').unwrap_or(trimmed).to_owned()
}

/// The installation as `Updater` sees it at startup.
struct Updater {
    executable: String,
    installed_package: Option<String>,
    installed_formula: Option<&'static str>,
    installed_winget: bool,
    curl_binary: String,
}

fn realpath(path: &str) -> Option<String> {
    std::fs::canonicalize(path).ok().map(|p| p.to_string_lossy().into_owned())
}

fn dirname(path: &str) -> String {
    std::path::Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())
}

/// `process.execPath`.
fn exec_path() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

impl Updater {
    fn new() -> Self {
        let exec_path = exec_path();
        let executable = realpath(&exec_path).unwrap_or(exec_path);
        let installed_package = Self::package(&executable);
        let installed_formula =
            ["opencode", "opencode-beta", "opencode-v2"].into_iter().find(|name| executable.contains(&format!("/Cellar/{name}/")));
        static WINGET: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let winget = WINGET.get_or_init(|| regex::Regex::new(r"(?i)[\\/]winget[\\/]packages[\\/]anomaly\.opencode_").expect("winget"));
        let installed_winget = winget.is_match(&executable);
        let curl_path = format!("{}/.opencode/bin/opencode", super::paths::home());
        let curl_binary = realpath(&curl_path).unwrap_or(curl_path);
        Updater { executable, installed_package, installed_formula, installed_winget, curl_binary }
    }

    /// The npm package whose bin is this executable (`@opencode/cli`, `@opencode-ai/cli`, their `-node` builds).
    fn package(executable: &str) -> Option<String> {
        let directory = dirname(&dirname(executable));
        let text = std::fs::read_to_string(format!("{directory}/package.json")).ok()?;
        let manifest = json::parse(&text)?;
        let name = json::get_str(&manifest, "name")?.to_owned();
        static NAME: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let pattern = NAME.get_or_init(|| regex::Regex::new(r"^@opencode(?:-ai)?/cli(?:-node)?$").expect("package name"));
        if !pattern.is_match(&name) {
            return None;
        }
        let bins = match json::get(&manifest, "bin") {
            Some(Json::Object(bins)) => bins.values().filter_map(json::as_str).map(str::to_owned).collect::<Vec<_>>(),
            _ => vec![],
        };
        let base = std::path::Path::new(&directory);
        bins.iter()
            .any(|bin| normalize(&base.join(bin).to_string_lossy()) == executable)
            .then_some(name)
    }

    /// `method()`.
    async fn method(&self) -> Option<Method> {
        if self.executable == self.curl_binary {
            return Some(Method::Curl);
        }
        if self.installed_formula.is_some() {
            return Some(Method::Brew);
        }
        if self.installed_winget {
            return Some(Method::Winget);
        }
        let package = self.installed_package.clone()?;
        let parts: Vec<String> =
            self.executable.to_lowercase().split('/').map(|part| part.strip_prefix('.').unwrap_or(part).to_owned()).collect();
        let command = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut checks: Vec<(Method, Vec<String>)> = vec![
            (Method::Npm, command(&["npm", "list", "-g", "--depth=0", &package])),
            (Method::Pnpm, command(&["pnpm", "list", "-g", "--depth=0", &package])),
            (Method::Bun, command(&["bun", "pm", "ls", "-g"])),
            (Method::Yarn, command(&["yarn", "global", "list"])),
            (Method::Vp, command(&["vp", "list", "-g", "--json", &package])),
        ];
        // A stable sort: methods named in the executable's path first.
        checks.sort_by_key(|(method, _)| !parts.iter().any(|part| part == method.name()));
        let timeout = Duration::from_secs(30);
        let run = |command: Vec<String>| async move { exec(&command, timeout).await.map(|r| r.stdout).unwrap_or_default() };
        let (a, b, c, d, e) = tokio::join!(
            run(checks[0].1.clone()),
            run(checks[1].1.clone()),
            run(checks[2].1.clone()),
            run(checks[3].1.clone()),
            run(checks[4].1.clone())
        );
        let outputs = [a, b, c, d, e];
        checks.iter().zip(outputs.iter()).find_map(|((method, _), stdout)| {
            let found = if *method == Method::Vp {
                // Vite+ repeats the filter in its no-match message: only its JSON list counts.
                json::parse(stdout).is_some_and(|value| match value {
                    Json::Array(items) => items.iter().any(|item| json::get_str(item, "name") == Some(package.as_str())),
                    _ => false,
                })
            } else {
                stdout.contains(package.as_str())
            };
            found.then_some(*method)
        })
    }

    fn removal_command(&self, method: Method) -> Option<Vec<String>> {
        if method == Method::Winget {
            return Some(["winget", "uninstall"].iter().chain(WINGET_ARGS).map(|s| s.to_string()).collect());
        }
        let package = self.installed_package.as_deref()?;
        let command: &[&str] = match method {
            Method::Curl | Method::Brew | Method::Winget => return None,
            Method::Npm => &["npm", "uninstall", "--global"],
            Method::Pnpm => &["pnpm", "remove", "--global"],
            Method::Bun => &["bun", "remove", "--global"],
            Method::Yarn => &["yarn", "global", "remove"],
            Method::Vp => &["vp", "uninstall", "-g"],
        };
        Some(command.iter().map(|s| s.to_string()).chain([package.to_owned()]).collect())
    }

    /// `release(method)`: the update service's latest release for this channel and distribution.
    async fn release(&self, method: Option<Method>) -> std::result::Result<(String, String), Failure> {
        let distribution = if method == Some(Method::Brew) { "homebrew" } else { "npm" };
        let core = method == Some(Method::Brew) && self.installed_formula == Some("opencode");
        let channel: String =
            crate::app::CHANNEL.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '-' }).collect();
        let url = if core {
            "https://formulae.brew.sh/api/formula/opencode.json".to_owned()
        } else {
            format!(
                "https://opencode.ai/update/api/{}/{}/{distribution}?current={}",
                super::client::encode_component(&channel),
                super::client::encode_component(ARTIFACT),
                super::client::encode_component(crate::app::VERSION)
            )
        };
        let unreachable = |detail: String| {
            Failure::upgrade("Could not check for OpenCode updates", detail, None, "Check your network, then run opencode upgrade again.")
        };
        let answer = match tokio::time::timeout(Duration::from_secs(10), super::client::fetch("GET", &url, &[], None)).await {
            Err(_) => return Err(unreachable("The operation timed out.".into())),
            Ok(Err(message)) => return Err(unreachable(message)),
            Ok(Ok(answer)) => answer,
        };
        if !(200..300).contains(&answer.status) {
            return Err(Failure::upgrade(
                "Could not check for OpenCode updates",
                format!("The update service returned HTTP {}.", answer.status),
                None,
                "Try again in a few minutes.",
            ));
        }
        let Some(data) = json::parse(&answer.body) else {
            return Err(Failure::upgrade(
                "Could not read the OpenCode update information",
                "JSON Parse error: Unable to parse JSON string",
                None,
                "Try again in a few minutes.",
            ));
        };
        let text = |value: Option<&Json>| match value {
            Some(Json::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };
        let version = if core {
            text(json::get(&data, "versions").and_then(|v| json::get(v, "stable")))
        } else {
            text(json::get(&data, "version"))
        };
        let package = if core { Some("opencode".to_owned()) } else { text(json::get(&data, "metadata").and_then(|m| json::get(m, "package"))) };
        match (version, package) {
            (Some(version), Some(package)) => Ok((package, version)),
            _ => Err(Failure::upgrade(
                "Could not read the OpenCode update information",
                "The update service returned incomplete release information.",
                None,
                "Try again in a few minutes.",
            )),
        }
    }

    /// `wingetVersion`: WinGet's own index names the release it can install.
    async fn winget_version(&self) -> std::result::Result<String, Failure> {
        let command: Vec<String> = ["winget", "search"].iter().chain(WINGET_ARGS).map(|s| s.to_string()).collect();
        let failure = |detail: String| {
            Failure::upgrade(
                "Could not check for OpenCode updates",
                detail,
                Some(command.join(" ")),
                "Fix the issue above, then run opencode upgrade again.",
            )
        };
        let result = exec(&command, Duration::from_secs(60)).await.map_err(|e| failure(e.detail))?;
        let version = result
            .stdout
            .split(['\r', '\n'])
            .flat_map(|line| {
                let tokens: Vec<&str> = line.split_whitespace().collect();
                tokens
                    .iter()
                    .position(|t| t.eq_ignore_ascii_case(WINGET_PACKAGE))
                    .and_then(|index| tokens.get(index + 1).map(|t| t.to_string()))
            })
            .find(|token| parse_release_version(token));
        match version {
            Some(version) if result.code == 0 => Ok(version),
            _ => Err(failure(result_detail(&result))),
        }
    }

    async fn latest(&self, method: Option<Method>) -> std::result::Result<String, Failure> {
        let method = match method {
            Some(method) => Some(method),
            None => self.method().await,
        };
        if method == Some(Method::Winget) {
            return self.winget_version().await;
        }
        self.release(method).await.map(|(_, version)| version)
    }

    /// `runUpgrade`.
    async fn run_upgrade(
        &self,
        method: Method,
        command: Vec<String>,
        display: Option<Vec<String>>,
        title: Option<&str>,
        retry: Option<&str>,
    ) -> std::result::Result<(), Failure> {
        let shown = display.unwrap_or_else(|| command.clone()).join(" ");
        let title = title.map(str::to_owned).unwrap_or_else(|| format!("{} could not install OpenCode", method.install_name()));
        let retry = retry.unwrap_or("Fix the issue above, then run opencode upgrade again.");
        match exec(&command, Duration::from_secs(300)).await {
            Ok(result) if result.code == 0 => Ok(()),
            Ok(result) => Err(Failure::upgrade(&title, result_detail(&result), Some(shown), retry)),
            Err(error) => Err(Failure::upgrade(&title, error.detail, Some(shown), retry)),
        }
    }

    /// `upgrade(method, version)`.
    async fn upgrade(&self, method: Method, input: &str) -> std::result::Result<(), Failure> {
        if !parse_release_version(input) {
            return Err(Failure::Plain(format!("Invalid version: {input}")));
        }
        let version = bare_version(input);
        let package = if method == Method::Winget {
            WINGET_PACKAGE.to_owned()
        } else if method == Method::Brew && self.installed_formula == Some("opencode") {
            "opencode".to_owned()
        } else {
            self.release(Some(method)).await?.0
        };
        let target = format!("{package}@{version}");
        if let Some(installed) = &self.installed_package {
            if &package != installed && matches!(method, Method::Pnpm | Method::Yarn) {
                return Err(Failure::Plain(format!("Reinstall {target} with {} to migrate from {installed}.", method.name())));
            }
        }
        let prepare = |detail: String| {
            Failure::upgrade(
                "Could not prepare the OpenCode upgrade",
                detail,
                None,
                "Fix the issue above, then run opencode upgrade again.",
            )
        };
        // Other OpenCode processes may be installing at the same time: wait longer than the slowest install.
        let _lock = Lock::acquire("cli-upgrade", Duration::from_secs(15 * 60)).await.map_err(prepare)?;
        let strings = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let migrating = self.installed_package.as_ref().is_some_and(|installed| &package != installed);
        match method {
            Method::Bun => {
                // Bun does not prune old versions from its shared package cache.
                let cache = TempDir::new("update-").map_err(prepare)?;
                let mut command = strings(&["bun", "install", "--global", "--trust", "--cache-dir"]);
                command.push(cache.path.clone());
                command.push(target.clone());
                let display = strings(&["bun", "install", "--global", "--trust", &target]);
                self.run_upgrade(method, command, Some(display), None, None).await
            }
            Method::Curl => {
                let directory = TempDir::new("update-").map_err(prepare)?;
                let installer = format!("{}/install", directory.path);
                self.run_upgrade(
                    method,
                    strings(&["curl", "-fsSL", "-o", &installer, "https://opencode.ai/v2/install"]),
                    Some(strings(&["curl", "-fsSL", "https://opencode.ai/v2/install"])),
                    Some("Could not download the OpenCode installer"),
                    Some("Check your network, then run opencode upgrade again."),
                )
                .await?;
                self.run_upgrade(
                    method,
                    strings(&["bash", &installer, "--version", &version, "--no-modify-path"]),
                    Some(strings(&["opencode", "upgrade", &version, "--method", "curl"])),
                    Some("The OpenCode installer failed"),
                    None,
                )
                .await
            }
            Method::Brew => self.run_upgrade(method, strings(&["brew", "upgrade", &package]), None, None, None).await,
            Method::Winget => {
                let mut command: Vec<String> = ["winget", "upgrade"].iter().chain(WINGET_ARGS).map(|s| s.to_string()).collect();
                command.extend(strings(&["--version", &version, "--silent", "--accept-package-agreements"]));
                self.run_upgrade(method, command, None, None, None).await
            }
            Method::Npm => {
                let mut command = strings(&["npm", "install", "--global", &format!("--allow-scripts={package}")]);
                // Keep the old package: uninstalling it can unlink the replacement command.
                let node = self.installed_package.as_deref().is_some_and(|p| p.ends_with("/cli-node"));
                if (ARTIFACT == "cli" && !node) || migrating {
                    command.push("--force".into());
                }
                command.push(target.clone());
                self.run_upgrade(method, command, None, None, None).await
            }
            Method::Pnpm => {
                let command = strings(&["pnpm", "add", "--global", &format!("--allow-build={package}"), &target]);
                self.run_upgrade(method, command, None, None, None).await
            }
            Method::Yarn => self.run_upgrade(method, strings(&["yarn", "global", "add", &target]), None, None, None).await,
            Method::Vp => {
                let command = if migrating {
                    strings(&["vp", "install", "-g", "--force", &target])
                } else {
                    strings(&["vp", "update", "-g", &target])
                };
                self.run_upgrade(method, command, None, None, None).await
            }
        }
    }
}

/// `path.resolve` of an absolute path: `.` and `..` segments folded, no trailing slash.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = vec![];
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

/// A temporary directory in the cache (`fs.makeTempDirectory({ directory: global.cache, prefix })`), removed when
/// dropped.
struct TempDir {
    path: String,
}

impl TempDir {
    fn new(prefix: &str) -> std::result::Result<TempDir, String> {
        let cache = super::paths::cache();
        std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
        let template = std::ffi::CString::new(format!("{cache}/{prefix}XXXXXX")).map_err(|e| e.to_string())?;
        let mut bytes = template.into_bytes_with_nul();
        // SAFETY: mkdtemp rewrites the template's X's in place in the NUL-terminated buffer.
        let made = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
        if made.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        bytes.pop();
        Ok(TempDir { path: String::from_utf8_lossy(&bytes).into_owned() })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `EffectFlock.acquire(key)`: a lock directory `<state>/locks/<sha1(key)>.lock` holding the owner's `meta.json` and a
/// heartbeat; another owner's lock is waited for (it is broken once its heartbeat is a minute old). Released on drop.
struct Lock {
    directory: String,
}

impl Lock {
    async fn acquire(key: &str, timeout: Duration) -> std::result::Result<Lock, String> {
        use sha1::Digest;
        let root = format!("{}/locks", super::paths::state());
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let digest = sha1::Sha1::digest(key.as_bytes());
        let name: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let directory = format!("{root}/{name}.lock");
        let deadline = tokio::time::Instant::now() + timeout;
        let mut delay = Duration::from_millis(100);
        loop {
            match std::fs::DirBuilder::new().mode_private().create(&directory) {
                Ok(()) => {
                    let mut meta = json::Obj::new();
                    meta.str("token", &crate::id::uuid())
                        .int("pid", i64::from(std::process::id()))
                        .str("hostname", &hostname())
                        .str("createdAt", &crate::zone::iso(crate::id::now()));
                    let _ = std::fs::write(format!("{directory}/meta.json"), json::text(&meta.done()));
                    let _ = std::fs::write(format!("{directory}/heartbeat"), "");
                    return Ok(Lock { directory });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(format!("{directory}/heartbeat"))
                        .or_else(|_| std::fs::metadata(&directory))
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|m| m.elapsed().ok())
                        .is_none_or(|age| age > Duration::from_secs(60));
                    if stale {
                        let _ = std::fs::remove_dir_all(&directory);
                        continue;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(format!("Timed out waiting for lock: {key}"));
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 17 / 10).min(Duration::from_secs(2));
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

trait PrivateDir {
    fn mode_private(&mut self) -> &mut Self;
}

impl PrivateDir for std::fs::DirBuilder {
    fn mode_private(&mut self) -> &mut Self {
        use std::os::unix::fs::DirBuilderExt;
        self.mode(0o700)
    }
}

fn hostname() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: gethostname writes at most the buffer's length.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return String::new();
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// `handlePromptErrors`: cancellation is `cancel("Cancelled")` and 130, a failure its message in an error frame, 1.
fn prompt_errors(clack: &Clack, result: std::result::Result<i32, Outcome>) -> Result {
    match result {
        Ok(code) => Ok(code),
        Err(Outcome::Cancelled) => {
            clack.cancel("Cancelled");
            Ok(130)
        }
        Err(Outcome::Failed(message)) => {
            clack.error(&message);
            clack.outro("Failed");
            Ok(1)
        }
    }
}

enum Outcome {
    Cancelled,
    Failed(String),
}

/// `opencode upgrade [target] [--method]`.
pub async fn upgrade(input: &Input) -> Result {
    let clack = Clack::new();
    clack.intro("Upgrade");
    let result = async {
        let updater = Updater::new();
        let method = match input.str("method").and_then(Method::parse) {
            Some(method) => Some(method),
            None => updater.method().await,
        };
        let Some(method) = method else {
            return Err(Outcome::Failed(
                "Could not detect the installation method. Pass --method to choose how to upgrade OpenCode.".into(),
            ));
        };
        clack.info(&format!("Using method: {}", method.name()));
        let target = match input.str("target") {
            Some(target) => target.to_owned(),
            None => updater.latest(Some(method)).await.map_err(|f| Outcome::Failed(f.message()))?,
        };
        let version = bare_version(&target);
        if version == crate::app::VERSION {
            clack.warn(&format!("OpenCode upgrade skipped: {version} is already installed"));
            clack.outro("Done");
            return Ok(0);
        }
        clack.info(&format!("From {} → {version}", crate::app::VERSION));
        let mut progress = clack.spinner();
        progress.start("Upgrading...");
        match updater.upgrade(method, &target).await {
            Ok(()) => progress.stop("Upgrade complete", 0),
            Err(failure) => {
                progress.stop("Upgrade failed", 1);
                return Err(Outcome::Failed(failure.message()));
            }
        }
        clack.outro("Done");
        Ok(0)
    }
    .await;
    prompt_errors(&clack, result)
}

/// The shell startup files a curl install added its PATH entry to (`shellConfigs`), when no other binary shares it.
fn shell_configs(home: &str) -> Vec<String> {
    let executable = exec_path();
    let bin = dirname(&executable);
    let own = std::path::Path::new(&executable).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // V1 and V2 curl installs share a PATH entry; retain it while another binary uses it.
    let Ok(entries) = std::fs::read_dir(&bin) else { return vec![] };
    if entries.flatten().any(|entry| entry.file_name().to_string_lossy() != own) {
        return vec![];
    }
    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
    let xdg = env("XDG_CONFIG_HOME").unwrap_or_else(|| format!("{home}/.config"));
    let zsh = env("ZDOTDIR").unwrap_or_else(|| home.to_owned());
    let shell = env("SHELL").unwrap_or_else(|| "bash".into());
    let name = std::path::Path::new(&shell).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let candidates: Vec<String> = match name.as_str() {
        "fish" => vec![format!("{home}/.config/fish/config.fish"), format!("{xdg}/fish/config.fish")],
        "zsh" => vec![format!("{zsh}/.zshrc"), format!("{zsh}/.zshenv"), format!("{xdg}/zsh/.zshrc"), format!("{xdg}/zsh/.zshenv")],
        "ash" | "sh" => vec![format!("{home}/.ashrc"), format!("{home}/.profile")],
        _ => vec![
            format!("{home}/.bashrc"),
            format!("{home}/.bash_profile"),
            format!("{home}/.profile"),
            format!("{xdg}/bash/.bashrc"),
            format!("{xdg}/bash/.bash_profile"),
        ],
    };
    let mut unique: Vec<String> = vec![];
    for file in candidates {
        if !unique.contains(&file) {
            unique.push(file);
        }
    }
    unique
        .into_iter()
        .filter(|file| std::fs::read(file).is_ok_and(|bytes| {
            let content = String::from_utf8_lossy(&bytes);
            clean_shell_config(&content) != content
        }))
        .collect()
}

/// `cleanShellConfig`: drops the installer's PATH lines (and the `# opencode` comment above one).
fn clean_shell_config(content: &str) -> String {
    let lines: Vec<&str> = content.split('\n').collect();
    let entry = |line: &str| {
        let trimmed = crate::text::trim(line);
        (trimmed.starts_with("export PATH=") || trimmed.strip_prefix("fish_add_path").is_some_and(|rest| rest.starts_with(|c: char| crate::text::is_js_whitespace(c))))
            && line.contains(".opencode/bin")
    };
    lines
        .iter()
        .enumerate()
        .filter(|(index, line)| !entry(line) && !(crate::text::trim(line) == "# opencode" && entry(lines.get(index + 1).copied().unwrap_or(""))))
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `fs.remove(path, { recursive: true, force: true })`'s failure as `errorMessage` shows it.
fn remove_error(path: &str, error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => format!("EACCES: permission denied, rm '{path}'"),
        _ => format!("{error}, rm '{path}'"),
    }
}

/// `opencode uninstall [--dry-run] [--force]`: stops this user's background services and removes the cache (data,
/// config and state stay), then the package if a package manager installed it.
pub async fn uninstall(input: &Input) -> Result {
    let clack = Clack::new();
    clack.intro("Uninstall OpenCode");
    let result = async {
        let updater = Updater::new();
        let method = updater.method().await;
        let removal = method.and_then(|m| updater.removal_command(m));
        let state = super::paths::state();
        let cache = super::paths::cache();
        let home = super::paths::home();
        // Registrations are read directly: resolving the service options could migrate files even in a dry run.
        static SERVICE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let pattern = SERVICE.get_or_init(|| regex::Regex::new(r"^service(?:-.*)?\.json$").expect("service file"));
        let mut services: Vec<String> = std::fs::read_dir(&state)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| pattern.is_match(name))
                    .collect()
            })
            .unwrap_or_default();
        services.sort();
        let shell = if method == Some(Method::Curl) { shell_configs(&home) } else { vec![] };

        clack.info(&format!("Installation method: {}", method.map_or("unknown", Method::name)));
        clack.message("The following global files will be removed (shared by OpenCode versions and channels):");
        if std::path::Path::new(&cache).exists() {
            clack.info(&format!("  Cache: {cache}"));
        }
        for name in &services {
            clack.info(&format!("  Stop background service and persistent terminals: {state}/{name}"));
        }
        for file in &shell {
            clack.info(&format!("  Shell PATH: {file}"));
        }
        if let Some(command) = &removal {
            clack.info(&format!("  Package: {}", command.join(" ")));
        }
        let executable = exec_path();
        if method == Some(Method::Curl) {
            clack.info(&format!("  Binary (manual removal): {executable}"));
        }
        if method.is_none() {
            clack.warn("Could not detect the installation method. Remove the installation manually after cleanup.");
        }

        if input.flag("dry-run") {
            clack.warn("Dry run - no changes made");
            clack.outro("Done");
            return Ok(0);
        }
        if !input.flag("force") {
            if !super::clack::interactive() {
                return Err(Outcome::Failed(
                    "Use --force to uninstall without an interactive terminal, or --dry-run to preview.".into(),
                ));
            }
            match clack.confirm("Are you sure you want to uninstall?", false) {
                Err(_) => return Err(Outcome::Cancelled),
                Ok(false) => {
                    clack.outro("Cancelled");
                    return Ok(0);
                }
                Ok(true) => {}
            }
        }

        let mut progress = clack.spinner();
        if !services.is_empty() {
            progress.start("Stopping background services...");
            for name in &services {
                let file = format!("{state}/{name}");
                if let Some(endpoint) = super::service::discover(&file, None).await {
                    let client = super::client::Client::new(&endpoint);
                    let _ = client.call("experimental.persistentPty.shutdown", &[], None, None).await;
                }
                if let Err(fail) = super::service::stop_file(&file).await {
                    progress.stop("Failed to stop background services", 1);
                    return Err(Outcome::Failed(fail.message));
                }
            }
            progress.stop("Background services stopped", 0);
        }

        let mut errors: Vec<String> = vec![];
        progress.start("Removing Cache...");
        match std::fs::remove_dir_all(&cache) {
            Ok(()) => progress.stop("Removed Cache", 0),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => progress.stop("Removed Cache", 0),
            Err(error) => {
                progress.stop("Failed to remove Cache", 1);
                errors.push(format!("Cache: {}", remove_error(&cache, &error)));
            }
        }
        for file in &shell {
            let outcome = std::fs::read(file)
                .and_then(|bytes| std::fs::write(file, clean_shell_config(&String::from_utf8_lossy(&bytes))));
            if let Err(error) = outcome {
                errors.push(format!("Shell config {file}: {error}"));
            }
        }
        if let Some(command) = &removal {
            progress.start(&format!("Running {}...", command.join(" ")));
            let outcome = match exec(command, Duration::from_secs(300)).await {
                Ok(result) if result.code == 0 => Ok(()),
                Ok(result) => Err(result_detail(&result)),
                Err(error) => Err(format!("Command failed: {}: {}", command.join(" "), error.detail)),
            };
            match outcome {
                Ok(()) => progress.stop("Package removed", 0),
                Err(message) => {
                    progress.stop("Package manager uninstall failed", 1);
                    errors.push(message);
                    clack.warn(&format!("Run manually: {}", command.join(" ")));
                }
            }
        }
        if method == Some(Method::Curl) {
            clack.message("To finish removing the binary, run:");
            clack.info(&format!("  rm '{}'", executable.replace('\'', "'\\''")));
        }
        if !errors.is_empty() {
            return Err(Outcome::Failed(errors.join("\n")));
        }
        clack.outro("Done");
        Ok(0)
    }
    .await;
    prompt_errors(&clack, result)
}

