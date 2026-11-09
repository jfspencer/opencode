//! The shell tool (packages/core/src/tool/plugin/shell.ts): runs a command line in the session's location with the
//! compatible shell, and returns its bounded output.
//!
//! Before the command starts (`prepare`, in TS's order): the working directory is resolved, the command is analyzed
//! (`opencode_shell::analyze`; a command the scanner cannot read is refused), external directories it names or moves
//! to are approved, the `shell` permission is asked for the commands found, and the working directory is checked
//! again. The command then runs as a job: a foreground call blocks on it until it finishes, is interrupted (the
//! command is stopped), or is moved to the background (`session.background`); a background call returns at once. A
//! backgrounded command's outcome comes back to the session as a synthetic notice, which wakes it.
//!
//! The description names the compatible shell; [`bind`] makes it live for a request (TS's session hooks).

use std::sync::Arc;

use opencode_protocol::Json;
use opencode_shell::result::{self, ToolOutput, ToolStatus};
use tokio::sync::watch;

use super::{Context, Definition, Execution, Failure, Output, Snapshot, Tool, ToolError};
use crate::file_access::{self, Kind};
use crate::job::{self, Blocked, Recovery, StartInput};
use crate::json::{self, Obj};
use crate::shell::{self, CreateError, CreateInput, select};

pub const NAME: &str = "shell";

pub struct Shell {
    definition: Definition,
}

/// The registered tool, with its recorded definition.
pub fn tool(definition: Definition) -> Arc<dyn Tool> {
    Arc::new(Shell { definition })
}

/// `process.platform`.
fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

/// The session hooks' description (`context`, `compaction`, `generate`): it names the compatible shell of the
/// location the request is for.
pub fn bind(snapshot: &mut Snapshot, inventory: &crate::inventory::Inventory) {
    let Some(index) = snapshot.tools.iter().position(|tool| tool.definition().name == NAME) else { return };
    let compatible = select::resolve(select::Priority::Compat, inventory);
    let name = opencode_shell::select::name(&compatible);
    let mut definition = snapshot.tools[index].definition().clone();
    definition.description = result::tool_description(Some(&name), platform());
    snapshot.tools[index] = Arc::new(Shell { definition });
}

impl Tool for Shell {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a> {
        Box::pin(execute(context, input))
    }
}

struct Input {
    command: String,
    workdir: Option<String>,
    timeout: Option<u64>,
    background: bool,
}

/// `JSON.stringify(value, null, 2)`.
fn pretty(value: &Json, indent: usize, out: &mut String) {
    use opencode_protocol::wire::out::{js_key_order, write_json_string};
    let pad = |out: &mut String, level: usize| out.extend(std::iter::repeat_n(' ', level * 2));
    match value {
        Json::Array(items) if !items.is_empty() => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                out.push_str(if index == 0 { "\n" } else { ",\n" });
                pad(out, indent + 1);
                pretty(item, indent + 1, out);
            }
            out.push('\n');
            pad(out, indent);
            out.push(']');
        }
        Json::Object(fields) if !fields.is_empty() => {
            out.push('{');
            for (index, key) in js_key_order(fields.keys().map(String::as_str)).into_iter().enumerate() {
                out.push_str(if index == 0 { "\n" } else { ",\n" });
                pad(out, indent + 1);
                write_json_string(out, key);
                out.push_str(": ");
                pretty(&fields[key], indent + 1, out);
            }
            out.push('\n');
            pad(out, indent);
            out.push('}');
        }
        other => out.push_str(&json::text(other)),
    }
}

/// `decodeInput` with the tool's `Input` schema (`errors: "all"`), and `formatInputIssues`.
fn decode(value: &Json) -> Result<Input, String> {
    let mut issues: Vec<(String, &str)> = Vec::new();
    let mut input = Input { command: String::new(), workdir: None, timeout: None, background: false };
    match value {
        Json::Object(fields) => {
            match fields.get("command") {
                None => issues.push(("command".into(), "Missing key")),
                Some(Json::String(command)) => input.command = command.clone(),
                Some(_) => issues.push(("command".into(), "Expected string")),
            }
            match fields.get("workdir") {
                None => {}
                Some(Json::String(workdir)) => input.workdir = Some(workdir.clone()),
                Some(_) => issues.push(("workdir".into(), "Expected string")),
            }
            match fields.get("timeout") {
                None => {}
                Some(Json::Number(n)) => {
                    let integer = n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0;
                    if !integer {
                        issues.push(("timeout".into(), "Expected an integer"));
                    }
                    if !(*n >= 0.0) {
                        issues.push(("timeout".into(), "Expected a value greater than or equal to 0"));
                    }
                    if integer && *n >= 0.0 {
                        input.timeout = Some(*n as u64);
                    }
                }
                Some(_) => issues.push(("timeout".into(), "Expected number")),
            }
            match fields.get("background") {
                None => {}
                Some(Json::Bool(background)) => input.background = *background,
                Some(_) => issues.push(("background".into(), "Expected boolean")),
            }
        }
        _ => issues.push(("root".into(), "Expected object")),
    }
    if issues.is_empty() {
        return Ok(input);
    }
    let mut details: Vec<String> = issues.iter().take(5).map(|(path, message)| format!("- {path}: {message}")).collect();
    if issues.len() > 5 {
        let more = issues.len() - 5;
        details.push(format!("- ...and {more} more {}", if issues.len() == 6 { "issue" } else { "issues" }));
    }
    let mut provided = String::new();
    pretty(value, 0, &mut provided);
    Err(format!(
        "Invalid arguments for tool \"{NAME}\":\n{}\n\nArguments provided:\n{provided}\n\nUpdate the arguments and call the tool again.",
        details.join("\n")
    ))
}

/// The call's failure (`ToolFailure({ message: "Unable to execute command: …", error })` through `toSessionError`):
/// the cause's own message, typed `unknown`, unless it is empty.
fn failure(command: &str, message: String) -> Failure {
    if message.is_empty() {
        return Failure::Error(ToolError::new(result::failure_message(command)));
    }
    Failure::Error(ToolError { kind: "unknown", message, metadata: None })
}

/// A permission check's failure, as the shell tool reports it: rejections keep their type, other errors are causes.
fn permission_failure(command: &str, error: Failure) -> Failure {
    match error {
        Failure::Error(error) if error.kind == "tool.execution" => failure(command, error.message),
        other => other,
    }
}

/// `prepare`: everything that must hold before the command starts.
async fn prepare(context: &Context, mut invocation: shell::Invocation) -> Result<shell::Invocation, Failure> {
    let command = invocation.command.clone();
    let location = &context.location;
    let target = file_access::resolve(location, &invocation.cwd, Some(Kind::Directory)).await;
    invocation.cwd = target.absolute.clone();
    let request = opencode_shell::analyze(&invocation.command, &invocation.shell, &target.absolute, file_access::host())
        .map_err(|message| failure(&command, message))?;
    let mut targets = vec![target.clone()];
    for directory in &request.directories {
        let path = file_access::resolve_path(&target.absolute, directory);
        targets.push(file_access::resolve(location, &path, Some(Kind::Directory)).await);
    }
    file_access::authorize_external(context, &targets, None).await.map_err(|e| permission_failure(&command, e))?;
    if !request.resources.is_empty() {
        context
            .assert(crate::permission::Ask {
                action: NAME.into(),
                resources: request.resources,
                save: Some(request.save),
                metadata: None,
            })
            .await
            .map_err(|e| permission_failure(&command, e))?;
    }
    // Approval can outlive the directory, so it is checked again right before the command starts.
    let workdir = target.absolute.clone();
    let kind = tokio::task::spawn_blocking(move || std::fs::metadata(&workdir).map(|m| m.is_dir())).await;
    match kind {
        Ok(Ok(true)) => Ok(invocation),
        Ok(Ok(false)) => Err(failure(&command, format!("Working directory is not a directory: {}", target.absolute))),
        _ => Err(failure(&command, format!("Working directory does not exist: {}", target.absolute))),
    }
}

/// `toolResult(output)`.
fn tool_result(output: &ToolOutput) -> Output {
    let content = result::result_messages(output)
        .into_iter()
        .map(|text| {
            let mut o = Obj::new();
            o.str("type", "text").str("text", &text);
            o.done()
        })
        .collect();
    let meta = result::tool_metadata(output);
    let mut metadata = Obj::new();
    match meta.status {
        Some(ToolStatus::Completed) => {
            metadata.str("status", "completed");
        }
        Some(ToolStatus::Running) => {
            metadata.str("status", "running");
        }
        None => {}
    }
    metadata.set("truncated", Json::Bool(meta.truncated));
    if let Some(exit) = meta.exit {
        metadata.int("exit", exit);
    }
    metadata.opt_str("signal", meta.signal.as_deref());
    if let Some(timeout) = meta.timeout {
        metadata.set("timeout", Json::Bool(timeout));
    }
    metadata.opt_str("shellID", meta.shell_id.as_deref());
    Output { content, metadata: Some(metadata.done()) }
}

/// The `ShellResult.Output` part of a tool output.
fn shell_output(output: &ToolOutput) -> result::Output {
    result::Output {
        output: output.output.clone(),
        truncated: output.truncated,
        exit: output.exit,
        signal: output.signal.clone(),
        timeout: output.timeout,
    }
}

/// Removes the command if its job's work stops before it settles (`Effect.onInterrupt(() => shell.remove(id))`).
struct RemoveOnDrop {
    shell: Option<(Arc<shell::Service>, String)>,
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some((service, id)) = self.shell.take() {
            tokio::spawn(async move {
                let _ = service.remove(&id).await;
            });
        }
    }
}

/// Cancels the job if the blocked call stops waiting (`Effect.onInterrupt(() => jobs.cancel(id))`).
struct CancelOnDrop {
    job: Option<(Arc<crate::app::App>, String)>,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some((app, id)) = self.job.take() {
            app.sessions.jobs.cancel(&app, &id);
        }
    }
}

/// `notifyWhenDone`: once the backgrounded job ends, its outcome is admitted to the session as a synthetic notice.
fn notify_when_done(
    app: Arc<crate::app::App>,
    session_id: String,
    job_id: String,
    shell_id: String,
    command: String,
    settled: watch::Receiver<Option<ToolOutput>>,
) {
    tokio::spawn(async move {
        let Some(info) = app.sessions.jobs.wait(&job_id).await else { return };
        let state = match info.status {
            job::Status::Running => return,
            job::Status::Completed => result::NotificationState::Completed,
            job::Status::Error => result::NotificationState::Error,
            job::Status::Cancelled => result::NotificationState::Cancelled,
        };
        let output = if info.status == job::Status::Completed {
            let mut settled = settled;
            settled.wait_for(|v| v.is_some()).await.ok().and_then(|v| v.clone())
        } else {
            None
        };
        let text = match (&output, info.status) {
            (Some(output), _) => result::result_messages(output).join("\n\n"),
            (None, job::Status::Error) => info.error.clone().unwrap_or_else(|| "Command failed".into()),
            (None, _) => "Cancelled".into(),
        };
        let shell_output = output.as_ref().map(shell_output);
        let notice = result::notification(&shell_id, Some(&job_id), &command, state, &text, shell_output.as_ref());
        let metadata = crate::session::shell::notification_metadata(&notice);
        let admitted = crate::session::subagent::synthetic(
            &app,
            crate::session::subagent::Synthetic {
                id: info.notification_id.clone(),
                session: session_id,
                text: notice.text,
                description: Some(command),
                metadata: Some(metadata),
                resume: true,
                located: true,
            },
        );
        if admitted.is_err() {
            return;
        }
        if let Some(notification) = &info.notification_id {
            app.sessions.jobs.complete_background(&app, notification);
        }
    });
}

async fn execute(context: Context, raw: Json) -> Result<Output, Failure> {
    let input = decode(&raw).map_err(|message| Failure::Error(ToolError::new(message)))?;
    let command = input.command.clone();
    let app = context.app.clone();
    let location = context.location.clone();
    let session_id = context.session_id.clone();
    let timeout = if input.background { input.timeout.unwrap_or(0) } else { input.timeout.unwrap_or(result::DEFAULT_TIMEOUT_MS) };
    let inventory = location.inventory();
    let compatible = select::resolve(select::Priority::Compat, &inventory);
    let mut metadata = Obj::new();
    metadata.str("sessionID", &session_id);
    let metadata = metadata.done();
    let base = crate::session::shell::base_environment(location.workspace_id.is_some(), Some(&metadata));
    let create = CreateInput {
        command: command.clone(),
        cwd: input.workdir.clone(),
        timeout: Some(timeout),
        shell: Some(compatible),
        metadata: Some(metadata),
    };
    let prepare_context = context.clone();
    let info = location
        .shell
        .create(create, base, &location.directory, String::new, |mut invocation| async move {
            invocation.set_env("AGENT", "1");
            invocation.set_env("OPENCODE", "1");
            if invocation.env.get("AI_AGENT").is_none_or(|v| v.is_empty()) {
                invocation.set_env("AI_AGENT", "opencode");
            }
            invocation.set_env("OPENCODE_SESSION_ID", &prepare_context.session_id);
            prepare(&prepare_context, invocation).await
        })
        .await
        .map_err(|error| match error {
            CreateError::Before(failure) => failure,
            CreateError::Start(error) => self::failure(&command, error.message()),
        })?;
    let mut progress = Obj::new();
    progress.str("shellID", &info.id);
    (context.progress)(progress.done());

    let (settled_tx, settled) = watch::channel::<Option<ToolOutput>>(None);
    let work = {
        let service = location.shell.clone();
        let info = info.clone();
        let (max_lines, max_bytes) = crate::session::shell::output_limits(&inventory);
        async move {
            let mut guard = RemoveOnDrop { shell: Some((service.clone(), info.id.clone())) };
            let outcome = service.result(&info, max_lines, max_bytes).await;
            guard.shell = None;
            let Some(capture) = outcome.capture else {
                return Err(job::RunError::Failed(shell::NotFound(info.id.clone()).message()));
            };
            let output = result::output(&outcome.info.result_info(), Some(&capture));
            let completed = result::completed(&output, timeout);
            let text = result::result_messages(&completed).join("\n\n");
            settled_tx.send_replace(Some(completed));
            Ok(text)
        }
    };
    let mut job_metadata = Obj::new();
    job_metadata.str("sessionID", &session_id).str("shellID", &info.id);
    let started = app.sessions.jobs.start(
        &app,
        StartInput {
            // Code Mode children share a tool-call ID, but each shell owns its job.
            id: info.id.clone(),
            ty: NAME.into(),
            title: Some(info.command.clone()),
            metadata: Some(job_metadata.done()),
            recovery: Some(Recovery::Shell {
                session_id: session_id.clone(),
                shell_id: info.id.clone(),
                command: info.command.clone(),
            }),
            notification_id: None,
        },
        move || Box::pin(work),
    );
    let notify = |settled: watch::Receiver<Option<ToolOutput>>| {
        notify_when_done(app.clone(), session_id.clone(), started.id.clone(), info.id.clone(), info.command.clone(), settled)
    };
    if input.background {
        app.sessions.jobs.background(&app, &started.id);
        notify(settled);
        return Ok(tool_result(&result::background(&info.id, &info.file)));
    }
    let mut cancel = CancelOnDrop { job: Some((app.clone(), started.id.clone())) };
    let blocked = app.sessions.jobs.block(&started.id, &session_id).await;
    cancel.job = None;
    match blocked {
        Some(Blocked::Backgrounded(_)) => {
            let _ = location.shell.timeout(&info.id, 0);
            notify(settled);
            return Ok(tool_result(&result::background(&info.id, &info.file)));
        }
        Some(Blocked::Finished(job)) if job.status == job::Status::Error => {
            return Err(failure(&command, job.error.unwrap_or_else(|| "Command failed".into())));
        }
        Some(Blocked::Finished(job)) if job.status == job::Status::Cancelled => {
            return Err(failure(&command, "Command cancelled".into()));
        }
        _ => {}
    }
    let mut settled = settled;
    let output = settled.wait_for(|v| v.is_some()).await.ok().and_then(|v| v.clone());
    match output {
        Some(output) => Ok(tool_result(&output)),
        None => Err(failure(&command, "Command cancelled".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_input_messages() {
        let value = json::parse(r#"{"command":"echo hi","timeout":-1}"#).unwrap();
        let message = decode(&value).err().unwrap();
        assert_eq!(
            message,
            "Invalid arguments for tool \"shell\":\n- timeout: Expected a value greater than or equal to 0\n\nArguments provided:\n{\n  \"command\": \"echo hi\",\n  \"timeout\": -1\n}\n\nUpdate the arguments and call the tool again."
        );
        let value = json::parse(r#"{"workdir":"src"}"#).unwrap();
        assert!(decode(&value).err().unwrap().contains("- command: Missing key\n"));
    }
}
