//! Restart continuity (packages/core/src/session/execution/restart.ts `resumeSuspendedSessions`): at boot, work a
//! dead process left behind continues.
//!
//! Backgrounded jobs come first, from their durable `job` rows, tool jobs before subagents so their outcomes are
//! admitted before a recovered child's first request: a shell or image job's notice tells its session how the job
//! ended (or that the restart cancelled it), without waking the session; a subagent whose child was still running
//! resumes as a background job again (its child's claim kept), and one that had finished delivers its notice. Then
//! top-level sessions whose execution claim was never released (the process died, or shut down, mid-turn) continue
//! where they left off, each at most ten times per turn: past that the turn fails instead.

use std::collections::HashSet;

use opencode_protocol::Json;
use opencode_shell::result::NotificationState;

use crate::app::App;
use crate::job::{self, Background, Info, Recovery, RunError, Status};
use crate::json::{self, Obj};
use crate::session::execution::Exit;
use crate::session::subagent::{self, Spawned, Synthetic};

const CONTINUE_AFTER_SERVER_RESTART: &str =
    "The server restarted while you were working. Continue from where you left off without repeating completed work.";
const MAX_RESUME_ATTEMPTS: i64 = 10;
const RESUME_EXHAUSTED: &str = "Execution was interrupted repeatedly and will not be resumed automatically.";

/// `listSuspended`: top-level sessions whose claim survived.
fn list_suspended(app: &App) -> Vec<String> {
    let db = app.db.lock().unwrap();
    let statement = db.prepare("SELECT id FROM session_v2 WHERE time_suspended IS NOT NULL AND parent_id IS NULL");
    match statement {
        Ok(mut statement) => {
            statement.query_map([], |row| row.get(0)).map(|rows| rows.filter_map(Result::ok).collect()).unwrap_or_default()
        }
        Err(_) => vec![],
    }
}

/// `releaseChildClaims`: orphaned child claims are cleared, except those of recoverable background subagents.
fn release_child_claims(app: &App, recoverable: &[String]) {
    let db = app.db.lock().unwrap();
    let mut sql = String::from(
        "UPDATE session_v2 SET time_suspended = NULL, resume_attempts = 0 WHERE time_suspended IS NOT NULL AND parent_id IS NOT NULL",
    );
    if !recoverable.is_empty() {
        let placeholders = (1..=recoverable.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
        sql.push_str(&format!(" AND id NOT IN ({placeholders})"));
    }
    if let Err(error) = db.execute(&sql, rusqlite::params_from_iter(recoverable.iter())) {
        eprintln!("failed to release child claims: {error}");
    }
}

/// `prepareResume`: counts the resume durably before it runs, so a turn that keeps dying cannot dodge the budget;
/// past it, the turn fails (which releases the claim). `false` when the session should not resume.
fn prepare_resume(app: &App, session: &str) -> bool {
    let attempts: Option<i64> = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "UPDATE session_v2 SET resume_attempts = resume_attempts + 1 WHERE id = ?1 RETURNING resume_attempts",
            [session],
            |row| row.get(0),
        )
        .ok();
    let Some(attempts) = attempts else { return false };
    let mut data = Obj::new();
    data.str("sessionID", session);
    if attempts > MAX_RESUME_ATTEMPTS {
        let mut error = Obj::new();
        error.str("type", "aborted").str("message", RESUME_EXHAUSTED);
        data.set("error", error.done());
        let _ = crate::session::publish_event(app, "session.execution.failed", data.done(), None, None);
        return false;
    }
    let mut metadata = Obj::new();
    metadata.str("notice", "restart");
    data.str("text", CONTINUE_AFTER_SERVER_RESTART).str("description", "Continuing after restart").set("metadata", metadata.done());
    let _ = crate::session::publish_event(app, "session.synthetic", data.done(), None, None);
    true
}

/// `restartMessage` of a shell or image job: its description, notice text and metadata.
fn restart_message(background: &Background) -> (String, String, Json) {
    let state = match background.status {
        Status::Running | Status::Cancelled => "cancelled",
        Status::Completed => "completed",
        Status::Error => "error",
    };
    let running = background.status == Status::Running;
    match &background.recovery {
        Recovery::Shell { shell_id, command, .. } => {
            let text = if running {
                "Command cancelled because the server restarted".to_owned()
            } else {
                match state {
                    "completed" => background.output.clone().unwrap_or_else(|| "Command completed".into()),
                    "error" => background.error.clone().unwrap_or_else(|| "Command failed".into()),
                    _ => "Command cancelled".into(),
                }
            };
            let notification_state = match state {
                "completed" => NotificationState::Completed,
                "error" => NotificationState::Error,
                _ => NotificationState::Cancelled,
            };
            let notice = opencode_shell::result::notification(shell_id, Some(&background.id), command, notification_state, &text, None);
            let metadata = crate::session::shell::notification_metadata(&notice);
            (command.clone(), notice.text, metadata)
        }
        Recovery::Image { path, model, .. } => {
            let body = if running {
                format!("Image request for {path} was cancelled because the server restarted.")
            } else {
                match state {
                    "completed" => background.output.clone().unwrap_or_else(|| format!("Image request finished. Saved image: {path} ({model}).")),
                    "error" => background.error.clone().unwrap_or_else(|| format!("Image request failed for {path}.")),
                    _ => format!("Image request for {path} was cancelled."),
                }
            };
            // `ImageNotice.text`.
            let text = format!("<image id=\"{}\" state=\"{state}\">\n{body}\n</image>", background.id);
            let mut metadata = Obj::new();
            metadata.str("source", "experimental_image_generate").str("jobID", &background.id).str("state", state).str("path", path);
            ("Image generation".into(), text, metadata.done())
        }
        Recovery::Subagent { .. } => unreachable!("subagents recover on their own"),
    }
}

/// `recoverToolJob`: the job's notice, admitted without waking the session (an interrupted execution resumes
/// separately); a missing session drops it.
fn recover_tool_job(app: &App, background: &Background) {
    let (description, text, metadata) = restart_message(background);
    let notice = Synthetic {
        id: Some(background.notification_id.clone()),
        session: background.recovery.owner().to_owned(),
        text,
        description: Some(description),
        metadata: Some(metadata),
        resume: false,
        located: false,
    };
    let _ = subagent::synthetic(app, notice);
    app.sessions.jobs.complete_background(app, &background.notification_id);
}

/// `recoverSubagent`: a finished child's notice is delivered; a running one resumes as a background job again,
/// unless its turn has been resumed too often.
fn recover_subagent(app: &App, background: &Background, suspended: &HashSet<String>) {
    let Recovery::Subagent { parent_session_id, child_session_id, agent, description } = &background.recovery else { return };
    let spawned = Spawned {
        parent: parent_session_id.clone(),
        child: child_session_id.clone(),
        agent: agent.clone(),
        description: description.clone(),
    };
    let (child, parent) = {
        let db = app.db.lock().unwrap();
        (
            crate::session::get(&db, child_session_id).ok().flatten(),
            crate::session::get(&db, parent_session_id).ok().flatten(),
        )
    };
    let owned = child.as_ref().is_some_and(|child| json::get_str(&child.info, "parentID") == Some(parent_session_id.as_str()));
    if !owned || parent.is_none() {
        app.sessions.jobs.complete_background(app, &background.notification_id);
        return;
    }
    // Early notices wait for recovery's accounting: a suspended parent is not woken by them.
    let resume = !suspended.contains(parent_session_id);
    let notify = |status: Status, output: Option<String>, error: Option<String>, resume: bool| {
        let info = Info {
            id: background.id.clone(),
            ty: "subagent".into(),
            title: Some(description.clone()),
            status,
            started_at: 0,
            completed_at: None,
            output,
            error,
            metadata: None,
            notification_id: Some(background.notification_id.clone()),
        };
        subagent::deliver_with(app, &info, &spawned, resume, false);
    };
    if background.status != Status::Running {
        notify(background.status, background.output.clone(), background.error.clone(), resume);
        return;
    }
    if app.sessions.executions.is_active(child_session_id) {
        return;
    }
    if !prepare_resume(app, child_session_id) {
        notify(Status::Error, None, Some(RESUME_EXHAUSTED.into()), resume);
        return;
    }
    let input = job::StartInput {
        id: background.id.clone(),
        ty: "subagent".into(),
        title: Some(description.clone()),
        metadata: None,
        recovery: Some(background.recovery.clone()),
        notification_id: Some(background.notification_id.clone()),
    };
    let owner = app.arc();
    let child = child_session_id.clone();
    app.sessions.jobs.start(app, input, move || {
        let execution = owner.sessions.executions.run(&owner, &child);
        Box::pin(async move {
            match execution.await {
                Exit::Succeeded => Ok(subagent::history_text(&owner, &child)),
                Exit::Failed(error) => Err(RunError::Failed(
                    json::get_str(&error, "message").filter(|m| !m.is_empty()).unwrap_or("Unknown error").to_owned(),
                )),
                Exit::Interrupted => Err(RunError::Cancelled),
            }
        })
    });
    app.sessions.jobs.background(app, &background.id);
    let owner = app.arc();
    let id = background.id.clone();
    tokio::spawn(async move {
        if let Some(info) = owner.sessions.jobs.wait(&id).await {
            subagent::deliver_with(&owner, &info, &spawned, true, false);
        }
    });
}

/// `SessionRestart.resumeSuspendedSessions`.
pub fn resume_suspended(app: &App) {
    let active: HashSet<String> = app.sessions.executions.active().into_iter().collect();
    let mut pending = app.sessions.jobs.pending_background(app);
    let children: Vec<String> = pending
        .iter()
        .filter(|background| background.status == Status::Running)
        .filter_map(|background| match &background.recovery {
            Recovery::Subagent { child_session_id, .. } => Some(child_session_id.clone()),
            _ => None,
        })
        .collect();
    let suspended: HashSet<String> =
        list_suspended(app).into_iter().chain(children.iter().cloned()).filter(|session| !active.contains(session)).collect();
    release_child_claims(app, &children);
    // Shell outcomes are admitted before a recovered child can start its first model request.
    pending.sort_by_key(|background| matches!(background.recovery, Recovery::Subagent { .. }));
    for background in &pending {
        if app.sessions.jobs.get(&background.id).is_some_and(|info| info.status == Status::Running) {
            continue;
        }
        match background.recovery {
            Recovery::Subagent { .. } => recover_subagent(app, background, &suspended),
            _ => recover_tool_job(app, background),
        }
    }
    // Background completion can wake a parent, so local ownership is inspected only after recovery.
    let resumed: HashSet<String> = app.sessions.executions.active().into_iter().collect();
    for session in list_suspended(app).into_iter().filter(|session| !resumed.contains(session)) {
        if prepare_resume(app, &session) {
            app.sessions.executions.resume(app, &session);
        }
    }
}
