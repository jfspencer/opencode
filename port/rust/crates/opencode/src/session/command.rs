//! Running a slash command in a session (packages/core/src/session/command.ts, command.ts `execute`,
//! plugin/command.ts for `init` and `review`, config/plugin/command.ts for config and markdown commands): the
//! command's template becomes a prompt, after switching to the command's agent and model when it names them.
//!
//! A command that runs as a subagent starts a child session as a background subagent job ([`session::subagent`]).
//!
//! `goal` is the goal plugin's ([`session::goal`]).
//!
//! An interpolated shell command's output is its stdout and stderr combined in arrival order.
//!
//! Out of scope: MCP prompts (MCP is dropped).

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

/// `ShellSelect.resolve({ priority: "config" })`.
fn shell(inventory: &Inventory) -> String {
    crate::shell::select::resolve(crate::shell::select::Priority::Config, inventory)
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
        let output = combined(shell, opencode_shell::select::args(shell, source), directory)
            .await
            .map_err(|error| format!("Shell interpolation failed for {}: {error}", json::text(&json::string(source))))?;
        outputs.push(String::from_utf8_lossy(&output).into_owned());
    }
    let mut outputs = outputs.into_iter();
    Ok(pattern.replace_all(text, |_: &regex::Captures| outputs.next().unwrap_or_default()).into_owned())
}

/// `AppProcess.run(.., { combineOutput: true })`: the command's stdout and stderr as one output, in the order their
/// chunks arrive from the two pipes.
async fn combined(program: &str, args: Vec<String>, cwd: &str) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut output = Vec::new();
    let (mut out_buffer, mut err_buffer) = ([0u8; 8192], [0u8; 8192]);
    let (mut out_open, mut err_open) = (true, true);
    while out_open || err_open {
        tokio::select! {
            read = stdout.read(&mut out_buffer), if out_open => match read? {
                0 => out_open = false,
                n => output.extend_from_slice(&out_buffer[..n]),
            },
            read = stderr.read(&mut err_buffer), if err_open => match read? {
                0 => err_open = false,
                n => output.extend_from_slice(&err_buffer[..n]),
            },
        }
    }
    child.wait().await?;
    Ok(output)
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
        Kind::Goal => {
            return session::goal::command(app, session_id, input).await.map_err(|error| match error {
                session::goal::CommandError::Message(message) => execution_error(name, &message),
                session::goal::CommandError::Failure(failure) => failure,
            });
        }
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
                // A child session of the command's agent (else the session's), run as a background subagent job.
                let parent = session::require(&app.db.lock().unwrap(), session_id)?;
                let (agent_id, selected) = inventory.select(agent.or(json::get_str(&parent.info, "agent")));
                let description = command.description.clone().unwrap_or_else(|| name.to_owned());
                let model = model
                    .or_else(|| selected.and_then(|info| info.model.as_ref()).map(json::of))
                    .or_else(|| json::get(&parent.info, "model").cloned());
                let child = session::create(
                    app,
                    session::CreateInput {
                        id: None,
                        parent_id: Some(session_id.to_owned()),
                        title: Some(description.clone()),
                        agent: Some(agent_id.clone()),
                        model,
                        location: None,
                        metadata: None,
                        permissions: None,
                    },
                )?;
                let child = child.id().to_owned();
                // The command's prompt (its attachments; the child's delivery is the default).
                let attachments = request.done();
                let mut prompt = Obj::new();
                for key in ["files", "agents", "skills"] {
                    prompt.opt(key, json::get(&attachments, key).cloned());
                }
                prompt.str("text", &format!("{}\n{text}", session::subagent::PREAMBLE)).set("resume", Json::Bool(false));
                prompt::submit(app, &child, &prompt.done()).await?;
                let spawned = session::subagent::Spawned { parent: session_id.to_owned(), child, agent: agent_id, description };
                session::subagent::start(app, &spawned);
                session::subagent::background(app, &spawned);
                return Ok(());
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
