//! Running a slash command in a session (packages/core/src/session/command.ts, command.ts `execute`,
//! plugin/command.ts for `init` and `review`, config/plugin/command.ts for config and markdown commands): the
//! command's template becomes a prompt, after switching to the command's agent and model when it names them.
//!
//! TODO(port): `goal` (plugin/goal.ts), commands that run as a subagent (`SubagentJob`), MCP prompts, the
//! interleaved output of interpolated shell commands (stdout, then stderr).

use opencode_protocol::Json;

use crate::app::App;
use crate::http::{Failure, Result};
use crate::inventory::Inventory;
use crate::inventory::commands::{self, Kind};
use crate::json::{self, Obj};
use crate::session::{self, prompt};

fn not_found(name: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "CommandNotFoundError").str("command", name).str("message", &format!("Command not found: {name}"));
    Failure::new(404, o.done())
}

fn execution_error(name: &str, message: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "CommandExecutionError").str("command", name).str("message", message);
    Failure::new(500, o.done())
}

/// An executable by path or on `PATH`, as its full path.
fn which(file: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let runnable = |path: &str| std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if file.contains('/') {
        return runnable(file).then(|| file.to_owned());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':').filter(|d| !d.is_empty()).map(|dir| format!("{dir}/{file}")).find(|candidate| runnable(candidate))
}

/// `ShellSelect.resolve({ priority: "config" })` on Unix: the configured shell, else `$SHELL`, else bash, else `sh`.
fn shell(inventory: &Inventory) -> String {
    let configured = inventory.latest(|c| c.shell.as_deref()).filter(|s| !s.is_empty()).map(str::to_owned);
    let file = configured.or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()));
    file.and_then(|file| which(&file)).or_else(|| which("bash")).unwrap_or_else(|| "/bin/sh".into())
}

/// `ShellSelect.args`.
fn shell_args(shell: &str, source: &str) -> Vec<String> {
    let name = shell.rsplit('/').next().unwrap_or(shell).to_lowercase();
    match name.as_str() {
        "cmd" => vec!["/c".into(), source.into()],
        "pwsh" | "powershell" => ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", source].map(String::from).to_vec(),
        _ => vec!["-c".into(), source.into()],
    }
}

/// `` !`cmd` `` interpolation: each command runs in the location's directory and is replaced by its output.
async fn interpolate(text: &str, directory: &str, shell: &str) -> std::result::Result<String, String> {
    let pattern = regex::Regex::new(r"!`([^`]+)`").expect("pattern");
    let sources: Vec<String> = pattern.captures_iter(text).map(|c| c[1].to_owned()).collect();
    if sources.is_empty() {
        return Ok(text.to_owned());
    }
    let mut outputs = Vec::new();
    for source in &sources {
        let output = crate::process::Command::new(shell, directory)
            .args(shell_args(shell, source))
            .run()
            .await
            .map_err(|error| format!("Shell interpolation failed for {}: {error}", json::text(&json::string(source))))?;
        let mut bytes = output.stdout.clone();
        bytes.extend_from_slice(&output.stderr);
        outputs.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    let mut outputs = outputs.into_iter();
    Ok(pattern.replace_all(text, |_: &regex::Captures| outputs.next().unwrap_or_default()).into_owned())
}

/// `Session.command`: `input` has the command payload's shape (`name`, `text`, `delivery?`, ...).
pub async fn execute(app: &App, session_id: &str, input: &Json) -> Result<()> {
    let stored = session::require(&app.db.lock().unwrap(), session_id)?;
    let location = app.locations.get(app, &stored.location).await?;
    let inventory = location.inventory();
    let name = json::get_str(input, "name").unwrap_or_default();
    let text = json::get_str(input, "text").unwrap_or_default();
    let Some(command) = inventory.command(name) else { return Err(not_found(name)) };
    let delivery = json::get_str(input, "delivery").unwrap_or("steer");
    let mut request = Obj::new();
    for key in ["files", "agents", "skills"] {
        request.opt(key, json::get(input, key).filter(|v| !matches!(v, Json::Null)).cloned());
    }
    request.str("delivery", delivery);
    let prompt_text = match &command.kind {
        Kind::Init => commands::append(&commands::PROMPT_INITIALIZE.replace("${path}", &location.project.directory), text),
        Kind::Review => commands::append(&commands::PROMPT_REVIEW.replace("${path}", &location.project.directory), text),
        // TODO(port): the goal loop.
        Kind::Goal => return Err(execution_error(name, "The goal command is not yet supported by the Rust port")),
        Kind::Config(config) => {
            let agent = config.agent.as_deref();
            let command_agent = match agent {
                Some(agent) => match inventory.agent(agent) {
                    Some(info) => Some(json::of(info)),
                    None => return Err(execution_error(name, &format!("Agent not found: {agent}"))),
                },
                None => None,
            };
            let model = match &config.model {
                Some(selection) => {
                    let mut o = Obj::new();
                    o.str("id", &selection.model).str("providerID", &selection.provider_id);
                    o.opt_str("variant", selection.variant.as_deref());
                    Some(o.done())
                }
                None => command_agent.as_ref().and_then(|a| json::get(a, "model")).cloned(),
            };
            let templated = commands::template(&config.template, text);
            let text = interpolate(&templated, &location.directory, &shell(&inventory)).await.map_err(|message| execution_error(name, &message))?;
            let subagent = command
                .subagent()
                .unwrap_or_else(|| command_agent.as_ref().and_then(|a| json::get_str(a, "mode")) == Some("subagent"));
            if subagent {
                // TODO(port): a child session run as a background subagent job.
                return Err(execution_error(name, "Commands that run as a subagent are not yet supported by the Rust port"));
            }
            if let Some(agent) = agent {
                let current = session::require(&app.db.lock().unwrap(), session_id)?;
                if json::get_str(&current.info, "agent") != Some(agent) {
                    prompt::switch_agent(app, session_id, agent, Some(stored.location.clone()))?;
                }
            }
            if let Some(model) = model {
                prompt::switch_model(app, session_id, model, Some(stored.location.clone()))?;
            }
            text
        }
    };
    request.str("text", &prompt_text);
    // A prompt that cannot be prepared (an unreadable attachment, an unknown skill) fails the command.
    prompt::submit(app, session_id, &request.done()).await.map_err(|failure| match json::get_str(&failure.body, "_tag") {
        Some("InvalidRequestError") => execution_error(name, json::get_str(&failure.body, "message").unwrap_or_default()),
        _ => failure,
    })?;
    Ok(())
}
