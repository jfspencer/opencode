//! A schedule's condition (packages/core/src/schedule/check.ts): validated when the schedule is created, authorized
//! once while the user is present, and evaluated with no model call before each firing, under the narrow
//! `Permission.Unattended` ruleset of what the check was authorized for, within a 30 s wall-clock cap.
//!
//! A shell check runs its command in the Session's directory with the compatible shell; its permission resources
//! come from the shell tool's analysis (`opencode_shell::analyze`) and `FileAccess.resolve`.
//!
//! A code check runs on the Code Mode runtime ([`crate::codemode::check`]) over the Code Mode tools its program names
//! (`CodeMode.scanTools`), with `state`, `now`, `runs` and `input` bound, under the ruleset that allows `execute` and
//! those tools' actions.

use std::collections::HashSet;

use opencode_protocol::Json;
use opencode_shell::Host;

use crate::app::App;
use crate::json::{self, Obj};
use crate::location::Location;
use crate::permission::{self, Ask, Requester};
use crate::session::Stored;

pub const TIMEOUT_MS: u64 = 30_000;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;
/// `CODE_LIMITS`.
const CODE_LIMITS: opencode_codemode::Limits =
    opencode_codemode::Limits { timeout_ms: Some(TIMEOUT_MS), max_tool_calls: Some(20), max_output_bytes: Some(MAX_OUTPUT_BYTES as u64) };

const FORBIDDEN_CODE_TOOLS: &[&str] = &[
    "opencode.schedule_create",
    "opencode.schedule_pause",
    "opencode.schedule_resume",
    "opencode.schedule_delete",
    "opencode.session_move",
    "opencode.session_rename",
    "opencode.worktree_create",
    "opencode.worktree_remove",
];

/// What one check run gives (`ScheduleCheck.Outcome`).
#[derive(Debug, Clone)]
pub enum Outcome {
    Ok { wake: bool, state: Json, message: Option<String> },
    Failed { error: String, invalid: bool },
}

impl Outcome {
    fn failed(error: impl Into<String>) -> Outcome {
        Outcome::Failed { error: error.into(), invalid: false }
    }

    /// `wakeOutcome`: the message only goes with a wake.
    fn wake(wake: bool, state: Json, message: Option<String>) -> Outcome {
        Outcome::Ok { wake, state, message: if wake { message } else { None } }
    }
}

/// `RunContext`.
pub struct RunContext {
    pub message_id: String,
    pub now: i64,
    pub checks: i64,
}

/// Why authorizing a check failed.
#[derive(Debug)]
pub enum Refused {
    /// A string failure (`InvalidError` with this reason).
    Invalid(String),
    Permission(permission::Error),
}

fn kind(condition: &Json) -> &str {
    json::get_str(condition, "type").unwrap_or_default()
}

/// `ScheduleCheck.validate`: the condition's syntax, before asking for permissions or spawning anything.
pub fn validate(condition: &Json) -> Result<(), String> {
    match kind(condition) {
        "shell" => {
            if json::get_str(condition, "run").unwrap_or_default().trim().is_empty() {
                return Err("A shell check needs a command".into());
            }
            let predicates = ["is", "matches", "changes"].iter().filter(|key| json::get(condition, key).is_some()).count();
            if predicates > 1 {
                return Err("Choose at most one of is, matches or changes".into());
            }
            match json::get_str(condition, "matches") {
                Some(pattern) => regex(pattern).map(drop).map_err(|_| {
                    // The engine's own wording is not part of the standard.
                    format!("Invalid regular expression {}: {}", json::text(&json::string(pattern)), "Invalid regular expression")
                }),
                None => Ok(()),
            }
        }
        _ => scan_tools(json::get_str(condition, "code").unwrap_or_default()).and_then(|paths| {
            match paths.iter().find(|path| FORBIDDEN_CODE_TOOLS.contains(&path.as_str())) {
                Some(path) => Err(format!("Tool {path} cannot be used inside a schedule check")),
                None => Ok(()),
            }
        }),
    }
}

/// `new RegExp(pattern)`. TODO(port): JavaScript's regular expression syntax (lookaround, backreferences) where it
/// differs from the `regex` crate's.
fn regex(pattern: &str) -> Result<regex::Regex, regex::Error> {
    regex::Regex::new(pattern)
}

/// `CodeMode.scanTools`.
fn scan_tools(code: &str) -> Result<Vec<String>, String> {
    crate::codemode::scan_tools(code)
}

/// `codeActions`: the registered Code Mode tools at `paths` (the schedule tools included while schedules are on) and
/// their permission actions, deduplicated; a forbidden, unknown or editing tool is an invalid check.
fn code_actions(location: &Location, paths: &[String]) -> Result<Vec<String>, String> {
    let extra = crate::tool::schedule::tools(&location.inventory());
    let registered = crate::tool::registry().code_mode_inventory(&[], &extra, None).tools;
    let mut actions: Vec<String> = Vec::new();
    for path in paths {
        let forbidden = || format!("Tool {path} cannot be used inside a schedule check");
        if FORBIDDEN_CODE_TOOLS.contains(&path.as_str()) {
            return Err(forbidden());
        }
        let Some(found) = registered.iter().find(|tool| tool.code_mode_path() == *path) else {
            return Err(format!("Unknown Code Mode tool: {path}"));
        };
        if found.permission() == "edit" {
            return Err(forbidden());
        }
        if !actions.iter().any(|action| action == found.permission()) {
            actions.push(found.permission().to_owned());
        }
    }
    Ok(actions)
}

fn any(action: &str, metadata: Option<Json>) -> Ask {
    Ask { action: action.into(), resources: vec!["*".into()], save: None, metadata }
}

/// `authorizeCode`: `execute` and each tool's action approved once (asked for `execute` when no tool is used),
/// failing fast on an explicit deny.
async fn authorize_code(
    app: &App,
    location: &Location,
    requester: &Requester,
    name: Option<&str>,
    when: &Json,
    mode: &str,
    code: &str,
) -> Result<(), Refused> {
    let paths = scan_tools(code).map_err(Refused::Invalid)?;
    let permission = &location.permission;
    permission.assert_not_denied(app, location, requester, &any("execute", None)).map_err(Refused::Permission)?;
    let actions = code_actions(location, &paths).map_err(Refused::Invalid)?;
    let metadata = |tools: &[String]| {
        let mut o = Obj::new();
        o.opt_str("schedule", name).str("mode", mode).set("when", when.clone()).set("unattended", Json::Bool(true));
        o.str("code", code).set("tools", Json::Array(tools.iter().map(|t| json::string(t)).collect()));
        o.done()
    };
    if actions.is_empty() {
        let ask = any("execute", Some(metadata(&[])));
        return permission.assert(app, location, requester, &ask).await.map_err(Refused::Permission);
    }
    for action in &actions {
        permission.assert_not_denied(app, location, requester, &any(action, None)).map_err(Refused::Permission)?;
    }
    for action in &actions {
        // No `save`: "always" would save a project-wide allow, while scheduled ticks run unattended.
        let ask = any(action, Some(metadata(&paths)));
        permission.assert(app, location, requester, &ask).await.map_err(Refused::Permission)?;
    }
    Ok(())
}

/// `ScheduleCheck.describe`: the condition in one line, for notes.
pub fn describe(condition: &Json) -> String {
    match kind(condition) {
        "shell" => json::get_str(condition, "run").unwrap_or_default().to_owned(),
        _ => {
            let code = json::get_str(condition, "code").unwrap_or_default();
            match code.split('\n').map(str::trim).find(|line| !line.is_empty()) {
                Some(line) => format!("code ({})", String::from_utf16_lossy(&line.encode_utf16().take(60).collect::<Vec<_>>())),
                None => "code".into(),
            }
        }
    }
}

fn requester(location: &Location, session: &Stored, unattended: Option<Vec<Json>>) -> Requester {
    let inventory = location.inventory();
    let (agent, _) = inventory.select(json::get_str(&session.info, "agent"));
    Requester { session_id: session.id().to_owned(), agent: Some(agent), unattended, ..Default::default() }
}

/// `ScheduleCheck.authorize`: runs in the Session's location while the user is present (or under the caller's
/// unattended rules); approves what the check needs once, without saving.
pub async fn authorize(
    app: &App,
    location: &Location,
    session: &Stored,
    name: Option<&str>,
    when: &Json,
    check: &Json,
    unattended: Option<Vec<Json>>,
) -> Result<(), Refused> {
    let condition = json::get(check, "condition").cloned().unwrap_or(Json::Null);
    let mode = json::get_str(check, "mode").unwrap_or_default();
    match kind(&condition) {
        "shell" => {
            let run = json::get_str(&condition, "run").unwrap_or_default();
            let scanned = scan_shell(location, run).map_err(Refused::Invalid)?;
            let mut metadata = Obj::new();
            metadata.opt_str("schedule", name).str("mode", mode).set("when", when.clone());
            metadata.set("unattended", Json::Bool(true)).str("run", run);
            let requester = requester(location, session, unattended);
            assert_shell(app, location, &requester, &scanned, Some(metadata.done())).await.map_err(Refused::Permission)
        }
        _ => {
            let code = json::get_str(&condition, "code").unwrap_or_default();
            let requester = requester(location, session, unattended);
            authorize_code(app, location, &requester, name, when, mode, code).await
        }
    }
}

/// `ScheduleCheck.evaluate`: one run of the check in the Session's location, capped at 30 s.
pub async fn evaluate(app: &App, location: &Location, session: &Stored, check: &Json, previous: &Json, context: RunContext) -> Outcome {
    let condition = json::get(check, "condition").cloned().unwrap_or(Json::Null);
    let run = async {
        match kind(&condition) {
            "shell" => run_shell(app, location, session, &condition, previous).await,
            _ => run_code(app, location, session, &condition, previous, &context).await,
        }
    };
    match tokio::time::timeout(std::time::Duration::from_millis(TIMEOUT_MS), run).await {
        Ok(outcome) => outcome,
        Err(_) => Outcome::failed(format!("Check timed out after {TIMEOUT_MS}ms")),
    }
}

/// `runCode`: the program over the tools it names, under the ruleset that allows `execute` and their actions.
async fn run_code(app: &App, location: &Location, session: &Stored, condition: &Json, previous: &Json, context: &RunContext) -> Outcome {
    let code = json::get_str(condition, "code").unwrap_or_default();
    let inventory = location.inventory();
    let (agent_id, agent) = inventory.select(json::get_str(&session.info, "agent"));
    let Some(agent) = agent else { return Outcome::failed(format!("Agent not found: {agent_id}")) };
    let paths = match scan_tools(code) {
        Ok(paths) => paths,
        Err(error) => return Outcome::failed(error),
    };
    let actions = match code_actions(location, &paths) {
        Ok(actions) => actions,
        Err(error) => return Outcome::failed(error),
    };
    let allowed: Vec<String> = std::iter::once("execute".to_owned()).chain(actions).collect();
    let unattended: Vec<Json> = allowed.iter().map(|action| rule(action, "*")).collect();
    let requester = requester(location, session, Some(unattended.clone()));
    for action in &allowed {
        if let Err(error) = location.permission.assert_not_denied(app, location, &requester, &any(action, None)) {
            return Outcome::failed(error.message());
        }
    }
    let Some(located) = app.locations.cached(&location.reference()) else {
        return Outcome::failed(format!("Location unavailable: {}", location.directory));
    };
    let mut input = Obj::new();
    input.set("state", previous.clone()).int("now", context.now).int("runs", context.checks);
    let bindings = vec![
        ("state".to_owned(), previous.clone()),
        ("now".to_owned(), Json::Number(context.now as f64)),
        ("runs".to_owned(), Json::Number(context.checks as f64)),
        ("input".to_owned(), input.done()),
    ];
    // `Permission.merge(agent.permissions, session.permissions ?? [])`.
    let mut permissions: Vec<Json> = json::get(&json::of(agent), "permissions").and_then(json::array).unwrap_or_default().to_vec();
    permissions.extend(json::get(&session.info, "permissions").and_then(json::array).unwrap_or_default().iter().cloned());
    let tool_context = crate::tool::Context {
        app: app.arc(),
        location: located,
        session_id: session.id().to_owned(),
        agent: agent_id,
        message_id: context.message_id.clone(),
        call_id: String::new(),
        directory: location.directory.clone(),
        progress: std::sync::Arc::new(|_| {}),
        unattended: Some(unattended),
    };
    let checked = crate::codemode::check(crate::codemode::CheckInput {
        code: code.to_owned(),
        tools: paths,
        bindings,
        permissions,
        extra: crate::tool::schedule::tools(&inventory),
        limits: CODE_LIMITS,
        context: tool_context,
    })
    .await;
    let result = match checked {
        Ok(checked) => checked.result,
        Err(error) => return Outcome::failed(error),
    };
    code_outcome(&result, previous)
}

/// The outcome of a code check's `CodeMode.Result`: a boolean wakes or not, `{ wake, state?, message? }` also
/// carries state and a message; any other value is an invalid check.
fn code_outcome(result: &Json, previous: &Json) -> Outcome {
    if json::get_bool(result, "ok") != Some(true) {
        let error = json::get(result, "error");
        let kind = error.and_then(|e| json::get_str(e, "kind")).unwrap_or_default();
        let message = error.and_then(|e| json::get_str(e, "message")).unwrap_or_default().to_owned();
        let invalid = ["ParseError", "UnsupportedSyntax", "UnknownTool", "InvalidToolInput"].contains(&kind)
            || message.contains("needs approval");
        return Outcome::Failed { error: message, invalid };
    }
    let value = json::get(result, "value").cloned().unwrap_or(Json::Null);
    if let Json::Bool(wake) = value {
        return Outcome::Ok { wake, state: previous.clone(), message: None };
    }
    // `Schema.decodeUnknownResult(Struct({ wake: Boolean, state?: Json, message?: String }))`, first issue only.
    let issue = match &value {
        Json::Object(fields) => match (fields.get("wake"), fields.get("message")) {
            (None, _) => Some("Missing key\n  at [\"wake\"]"),
            (Some(Json::Bool(_)), None | Some(Json::String(_))) => None,
            (Some(Json::Bool(_)), Some(_)) => Some("Expected string\n  at [\"message\"]"),
            (Some(_), _) => Some("Expected boolean\n  at [\"wake\"]"),
        },
        _ => Some("Expected object"),
    };
    if let Some(issue) = issue {
        return Outcome::Failed {
            error: format!("A code check must return a boolean or {{ wake, state?, message? }}: SchemaError({issue})"),
            invalid: true,
        };
    }
    let wake = json::get_bool(&value, "wake").unwrap_or(false);
    let state = json::get(&value, "state").cloned().unwrap_or_else(|| previous.clone());
    let message = json::get_str(&value, "message").map(|m| m.trim_matches(crate::text::is_js_whitespace)).filter(|m| !m.is_empty());
    Outcome::wake(wake, state, message.map(str::to_owned))
}

/// What a shell check runs and needs (`scanShell`).
struct Scanned {
    shell: String,
    cwd: String,
    workspace: bool,
    external: Vec<String>,
    commands: Vec<String>,
}

fn dedupe(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items.into_iter().filter(|item| seen.insert(item.clone())).collect()
}

fn scan_shell(location: &Location, command: &str) -> Result<Scanned, String> {
    let target = resolve_directory(location, &location.directory);
    let shell = compatible_shell(location);
    let host = shell_host();
    let parsed = opencode_shell::analyze(command, &shell, &target.0, host)?;
    let directories: Vec<(String, Option<String>)> = parsed
        .directories
        .iter()
        .map(|directory| resolve_directory(location, &opencode_shell::resolve_path(&target.0, directory, &host.home())))
        .collect();
    let external = dedupe(std::iter::once(&target).chain(directories.iter()).filter_map(|(_, external)| external.clone()));
    Ok(Scanned {
        shell,
        cwd: target.0.clone(),
        workspace: location.workspace_id.is_some(),
        external,
        commands: dedupe(parsed.resources),
    })
}

fn shell_host() -> &'static opencode_shell::SystemHost {
    static HOST: std::sync::OnceLock<opencode_shell::SystemHost> = std::sync::OnceLock::new();
    HOST.get_or_init(opencode_shell::SystemHost::new)
}

/// `ShellSelect.resolve({priority: "compat"})`: the configured shell when the shell tool may use it, else `$SHELL`
/// when it may, else bash from `PATH`, else /bin/sh. TODO(port): share the shell tool's selection once it lands.
fn compatible_shell(location: &Location) -> String {
    let inventory = location.inventory();
    let executable = |file: &str| -> Option<String> {
        if file.starts_with('/') {
            return std::path::Path::new(file).is_file().then(|| file.to_owned());
        }
        crate::process::which(file, None)
    };
    let select = |file: Option<String>| -> String {
        if let Some(file) = file.filter(|file| opencode_shell::select::compatible(file)) {
            if let Some(shell) = executable(&file) {
                return shell;
            }
        }
        if cfg!(target_os = "macos") {
            return "/bin/zsh".into();
        }
        crate::process::which("bash", None).unwrap_or_else(|| "/bin/sh".into())
    };
    match inventory.latest(|info| info.shell.as_deref()) {
        Some(configured) if !configured.is_empty() => select(Some(configured.to_owned())),
        _ => select(std::env::var("SHELL").ok().filter(|shell| !shell.is_empty())),
    }
}

/// `realPath`: symlinks resolved through the part of the path that exists.
fn real_path(target: &str) -> String {
    match std::fs::canonicalize(target) {
        Ok(real) => real.to_string_lossy().into_owned(),
        Err(_) => {
            let path = std::path::Path::new(target);
            match (path.parent(), path.file_name()) {
                (Some(parent), Some(name)) if parent != path => {
                    let real = real_path(&parent.to_string_lossy());
                    crate::project::normalize(&format!("{real}/{}", name.to_string_lossy()))
                }
                _ => target.to_owned(),
            }
        }
    }
}

/// `FileAccess.resolve({path, kind: "directory"})`: the absolute path, and the `external_directory` resource when it
/// is outside the location and its project's worktree. TODO(port): share the file tools' FileAccess once it lands.
fn resolve_directory(location: &Location, input: &str) -> (String, Option<String>) {
    let absolute = opencode_shell::resolve_path(&location.directory, input, &shell_host().home());
    let workspace = location.workspace_id.is_some();
    let real = |path: &str| if workspace { path.to_owned() } else { real_path(path) };
    let canonical = real(&absolute);
    let worktree = crate::project::normalize(&location.project.directory);
    let inside = |target: &str, directory: &str, worktree: &str| {
        crate::project::contains(directory, target) || (worktree != "/" && crate::project::contains(worktree, target))
    };
    if inside(&absolute, &location.directory, &worktree) && inside(&canonical, &real(&location.directory), &real(&worktree)) {
        return (absolute, None);
    }
    let resource = if canonical == "/" { "/*".to_owned() } else { format!("{canonical}/*") };
    (absolute, Some(resource))
}

/// `assertShellPermissions`: the external directories, then the commands.
async fn assert_shell(
    app: &App,
    location: &Location,
    requester: &Requester,
    scanned: &Scanned,
    metadata: Option<Json>,
) -> Result<(), permission::Error> {
    if !scanned.external.is_empty() {
        let ask = Ask {
            action: "external_directory".into(),
            resources: scanned.external.clone(),
            save: None,
            metadata: metadata.clone(),
        };
        location.permission.assert(app, location, requester, &ask).await?;
    }
    if !scanned.commands.is_empty() {
        let ask = Ask { action: "shell".into(), resources: scanned.commands.clone(), save: None, metadata };
        location.permission.assert(app, location, requester, &ask).await?;
    }
    Ok(())
}

fn rule(action: &str, resource: &str) -> Json {
    let mut o = Obj::new();
    o.str("action", action).str("resource", resource).str("effect", "allow");
    o.done()
}

/// `utf8Decode`: a cut-off character at the end is dropped.
fn utf8_decode(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    match text.strip_suffix('\u{FFFD}') {
        Some(rest) => rest.to_owned(),
        None => text.into_owned(),
    }
}

async fn run_shell(app: &App, location: &Location, session: &Stored, condition: &Json, previous: &Json) -> Outcome {
    let run = json::get_str(condition, "run").unwrap_or_default();
    let scanned = match scan_shell(location, run) {
        Ok(scanned) => scanned,
        Err(error) => return Outcome::failed(error),
    };
    let unattended: Vec<Json> = scanned
        .external
        .iter()
        .map(|resource| rule("external_directory", resource))
        .chain(scanned.commands.iter().map(|resource| rule("shell", resource)))
        .collect();
    let requester = requester(location, session, Some(unattended));
    if let Err(error) = assert_shell(app, location, &requester, &scanned, None).await {
        return Outcome::failed(error.message());
    }
    // The Session's environment applies to implicit-local placement only (`env: process.env`).
    let _ = scanned.workspace;
    let command = crate::process::Command::new(&scanned.shell, &scanned.cwd)
        .args(opencode_shell::select::args(&scanned.shell, run))
        .max_output(Some(MAX_OUTPUT_BYTES));
    let output = match command.run().await {
        Ok(output) => output,
        Err(error) => return Outcome::failed(error.to_string()),
    };
    let stdout = utf8_decode(&output.stdout);
    let stderr = utf8_decode(&output.stderr);
    let trimmed = stdout.trim_matches(crate::text::is_js_whitespace).to_owned();
    let message = (!trimmed.is_empty()).then(|| trimmed.clone());
    let is = json::get_str(condition, "is");
    let matches = json::get_str(condition, "matches");
    let changes = json::get_bool(condition, "changes") == Some(true);
    if is.is_none() && matches.is_none() && !changes {
        return Outcome::wake(output.code == 0, Json::Null, message);
    }
    if output.code != 0 {
        let error_text = stderr.trim_matches(crate::text::is_js_whitespace);
        let detail = if error_text.is_empty() { trimmed.as_str() } else { error_text };
        let suffix = if detail.is_empty() { String::new() } else { format!(": {detail}") };
        return Outcome::failed(format!("Command exited with code {}{suffix}", output.code));
    }
    if let Some(is) = is {
        return Outcome::wake(trimmed == is.trim_matches(crate::text::is_js_whitespace), Json::Null, message);
    }
    if let Some(pattern) = matches {
        let matched = regex(pattern).map(|re| re.is_match(&stdout)).unwrap_or(false);
        return Outcome::wake(matched, Json::Null, message);
    }
    let before = json::get_str(previous, "stdout");
    let mut state = Obj::new();
    state.str("stdout", &stdout);
    Outcome::wake(before.is_some_and(|before| before != stdout), state.done(), message)
}
