//! Subagents as jobs (packages/core/src/session/subagent-job.ts, subagent-completion.ts, background.ts, and
//! session.ts `background` / `synthetic`): a child session's run is a job keyed by the child's ID, whose result is
//! the child's last complete answer. A foreground caller blocks on it; a background one (or a blocked caller moved
//! to the background) gets the result as a synthetic notice admitted into the parent session, which wakes it.
//!
//! Also the session API around them: `POST /api/session/:id/background` ([`background_session`]) and the job list
//! (`GET /api/experimental/job`, [`list_jobs`]).

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use opencode_protocol::Json;

use crate::app::{App, fatal};
use crate::http::Result;
use crate::job::{self, Info, Recovery, RunError, Status};
use crate::json::{self, Obj};
use crate::session::execution::Exit;
use crate::session::{self, inbox, message};

/// `SubagentCompletion.NO_TEXT`.
pub const NO_TEXT: &str = "Subagent completed without a text response.";

/// The first line of a new child session's task.
pub const PREAMBLE: &str = "You are a subagent spawned by another session.";

/// A subagent job's recovery (`Extract<Job.Recovery, { kind: "subagent" }>`).
#[derive(Debug, Clone)]
pub struct Spawned {
    pub parent: String,
    pub child: String,
    /// What the notice and the job list name the agent by: the tool records its name, a command its ID.
    pub agent: String,
    pub description: String,
}

impl Spawned {
    fn recovery(&self) -> Recovery {
        Recovery::Subagent {
            parent_session_id: self.parent.clone(),
            child_session_id: self.child.clone(),
            agent: self.agent.clone(),
            description: self.description.clone(),
        }
    }
}

/// `SubagentJob.start`: the child's run (`sessions.resume(child)`, joining an active execution) as a job, whose
/// output is the text of the child's last complete answer. A running job is joined.
pub fn start(app: &App, spawned: &Spawned) -> Info {
    let input = job::StartInput {
        id: spawned.child.clone(),
        ty: "subagent".into(),
        title: Some(spawned.description.clone()),
        metadata: Some(json::object()),
        recovery: Some(spawned.recovery()),
        notification_id: None,
    };
    let owner = app.arc();
    let child = spawned.child.clone();
    app.sessions.jobs.start(app, input, move || {
        let exists = session::get(&owner.db.lock().unwrap(), &child).ok().flatten().is_some();
        let execution = exists.then(|| owner.sessions.executions.run(&owner, &child));
        Box::pin(async move {
            let Some(execution) = execution else { return Err(RunError::Failed(format!("Session not found: {child}"))) };
            match execution.await {
                Exit::Succeeded => Ok(final_text(&owner, &child)),
                Exit::Failed(error) => Err(RunError::Failed(
                    json::get_str(&error, "message").filter(|m| !m.is_empty()).unwrap_or("Unknown error").to_owned(),
                )),
                Exit::Interrupted => Err(RunError::Cancelled),
            }
        })
    })
}

/// `SubagentJob.background`: moves the child's job to the background and watches for its completion.
pub fn background(app: &App, spawned: &Spawned) {
    if let Some(info) = app.sessions.jobs.background(app, &spawned.child) {
        notify(app, spawned, info.started_at);
    }
}

/// `SubagentJob.notify`: one completion observer per job generation, which delivers the notice once the job ends.
pub fn notify(app: &App, spawned: &Spawned, started_at: i64) {
    static OBSERVED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let observed = OBSERVED.get_or_init(Default::default);
    let key = format!("{}:{started_at}", spawned.child);
    if !observed.lock().unwrap().insert(key.clone()) {
        return;
    }
    // The observer starts at once: a job that already finished is delivered right away.
    if let Some(info) = app.sessions.jobs.get(&spawned.child).filter(|info| info.status != Status::Running) {
        deliver(app, &info, spawned);
        observed.lock().unwrap().remove(&key);
        return;
    }
    let owner = app.arc();
    let spawned = spawned.clone();
    tokio::spawn(async move {
        if let Some(info) = owner.sessions.jobs.wait(&spawned.child).await {
            deliver(&owner, &info, &spawned);
        }
        observed.lock().unwrap().remove(&key);
    });
}

/// `SubagentCompletion.text`: the answer's non-empty text parts, or [`NO_TEXT`].
fn completion_text(message: Option<&Json>) -> String {
    let texts: Vec<&str> = match message.and_then(|m| json::get(m, "content")) {
        Some(Json::Array(content)) => content
            .iter()
            .filter(|part| json::get_str(part, "type") == Some("text"))
            .filter_map(|part| json::get_str(part, "text"))
            .filter(|text| !text.is_empty())
            .collect(),
        _ => vec![],
    };
    if texts.is_empty() { NO_TEXT.to_owned() } else { texts.join("\n\n") }
}

/// The text of the child's last complete answer (among its latest 20 messages).
fn final_text(app: &App, child: &str) -> String {
    let input = message::ListInput { session: child.to_owned(), limit: Some(20), order: "desc".into(), ty: None, cursor: None };
    let messages = message::list(&app.db.lock().unwrap(), &input).unwrap_or_default();
    let answer = messages.iter().find(|m| {
        json::get_str(m, "type") == Some("assistant")
            && json::get(m, "time").and_then(|t| json::get(t, "completed")).is_some()
            && json::get(m, "error").is_none()
    });
    completion_text(answer)
}

/// `SubagentCompletion.deliver`: the finished job's notice, admitted into the parent session (which it wakes).
fn deliver(app: &App, info: &Info, spawned: &Spawned) {
    let text = match info.status {
        Status::Running => return,
        Status::Completed => info.output.clone().unwrap_or_else(|| NO_TEXT.into()),
        Status::Error => info.error.clone().unwrap_or_else(|| "Subagent failed".into()),
        Status::Cancelled => "Subagent cancelled".into(),
    };
    let state = info.status.as_str();
    let mut metadata = Obj::new();
    metadata.str("source", "subagent").str("childID", &spawned.child).str("agent", &spawned.agent).str("state", state);
    let notice = Synthetic {
        id: info.notification_id.clone(),
        session: spawned.parent.clone(),
        text: format!(
            "<subagent sessionID=\"{}\" state=\"{state}\" description=\"{}\">\n{text}\n</subagent>",
            spawned.child, spawned.description
        ),
        description: Some(spawned.description.clone()),
        metadata: Some(metadata.done()),
        resume: true,
        located: true,
    };
    if synthetic(app, notice).is_ok() {
        if let Some(notification) = &info.notification_id {
            app.sessions.jobs.complete_background(app, notification);
        }
    }
}

/// A synthetic input (`Session.synthetic`'s input).
pub struct Synthetic {
    pub id: Option<String>,
    pub session: String,
    pub text: String,
    pub description: Option<String>,
    pub metadata: Option<Json>,
    pub resume: bool,
    /// Published in the session's location (a job's notice), or in none (the session API's).
    pub located: bool,
}

/// `Session.synthetic`: admits the input (steering) and wakes the session unless it has a staged revert. `Err` when
/// the session is missing or being removed, or the ID conflicts.
pub fn synthetic(app: &App, input: Synthetic) -> std::result::Result<(), ()> {
    if app.sessions.is_removing(&input.session) {
        return Err(());
    }
    let Some(stored) = session::get(&app.db.lock().unwrap(), &input.session).ok().flatten() else { return Err(()) };
    let id = input.id.unwrap_or_else(message::create_id);
    let mut payload = Obj::new();
    payload.str("text", &input.text);
    payload.opt_str("description", input.description.as_deref());
    payload.opt("metadata", input.metadata);
    let mut item = Obj::new();
    item.str("type", "synthetic").set("payload", payload.done()).str("delivery", "steer");
    let location = input.located.then_some(stored.location);
    match inbox::admit(app, &id, &input.session, item.done(), location) {
        Ok(Ok(_)) => {}
        _ => return Err(()),
    }
    let reverted = session::get(&app.db.lock().unwrap(), &input.session)
        .ok()
        .flatten()
        .is_some_and(|s| json::get(&s.info, "revert").is_some());
    if input.resume && !reverted {
        app.sessions.executions.wake(app, &input.session, inbox::Promotable::Input);
    }
    Ok(())
}

/// `Session.background`: moves the work the session's tool calls are blocked on to the background, and tells the
/// session what moved.
pub async fn background_session(app: &App, session_id: &str) -> Result<()> {
    session::require(&app.db.lock().unwrap(), session_id)?;
    let mut events = app.bus.subscribe();
    let moved = app.sessions.jobs.background_all(app, session_id, None);
    if moved.is_empty() {
        return Ok(());
    }
    // The blocked calls return at once and their step ends. TS admits the notice only once the step's next boundary
    // has passed (its admission yields to the runner at each database call), so it is delivered at the boundary
    // after: the notice waits for the step to end, which leaves the runner at its next request. A step that still
    // runs other tools does not end soon, and then the notice goes in at once, as in TS.
    let ended = async {
        loop {
            match events.recv().await {
                Ok(event)
                    if matches!(event.ty, "session.step.ended" | "session.step.failed")
                        && json::get_str(&event.data, "sessionID") == Some(session_id) =>
                {
                    return;
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), ended).await;
    let mut lines = vec![
        "User requested that active blocking work be moved to the background.".to_owned(),
        String::new(),
        "Backgrounded work:".to_owned(),
    ];
    lines.extend(moved.iter().map(|job| {
        let title = job.title.as_deref().filter(|t| !t.is_empty()).unwrap_or(&job.id);
        format!("- {}: {title}", job.ty)
    }));
    lines.push(String::new());
    lines.push("The backgrounded work is still unfinished. Move on to other work if you can. If there is nothing else useful to do, finish your response. Do not wait, sleep, poll, or report the backgrounded work as complete until a later completion notification is added to the conversation.".to_owned());
    let notice = Synthetic {
        id: None,
        session: session_id.to_owned(),
        text: lines.join("\n"),
        description: None,
        metadata: None,
        resume: true,
        located: false,
    };
    if synthetic(app, notice).is_err() {
        session::require(&app.db.lock().unwrap(), session_id)?;
    }
    Ok(())
}

/// `SessionBackground.list` for `GET /api/experimental/job`: backgrounded jobs filtered by kind and status, and by
/// session (its own, or its descendants' too).
pub fn list_jobs(app: &App, query: &Json) -> Result<Vec<Json>> {
    let session_id = json::get_str(query, "sessionID");
    if let Some(session_id) = session_id {
        session::require(&app.db.lock().unwrap(), session_id)?;
    }
    let kind = json::get_str(query, "kind");
    let status = json::get_str(query, "status");
    let descendants = matches!(json::get(query, "descendants"), Some(Json::Bool(true)))
        || json::get_str(query, "descendants") == Some("true");
    let items = app
        .sessions
        .jobs
        .background_jobs(app)
        .into_iter()
        .filter(|(_, item_status, item)| {
            kind.is_none_or(|kind| json::get_str(item, "kind") == Some(kind))
                && status.is_none_or(|status| item_status.as_str() == status)
        });
    let Some(ancestor) = session_id else { return Ok(items.map(|(_, _, item)| item).collect()) };
    if !descendants {
        return Ok(items.filter(|(owner, _, _)| owner == ancestor).map(|(_, _, item)| item).collect());
    }
    let mut parents: HashMap<String, Option<String>> = HashMap::new();
    let mut listed = Vec::new();
    for (owner, _, item) in items {
        if descends(app, &mut parents, &owner, ancestor)? {
            listed.push(item);
        }
    }
    Ok(listed)
}

/// Whether `from` is `ancestor` or one of its descendants (following parent links, each looked up once).
fn descends(app: &App, parents: &mut HashMap<String, Option<String>>, from: &str, ancestor: &str) -> Result<bool> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut current = Some(from.to_owned());
    while let Some(id) = current.take() {
        if id == ancestor {
            return Ok(true);
        }
        if !seen.insert(id.clone()) {
            break;
        }
        let parent = match parents.get(&id) {
            Some(parent) => parent.clone(),
            None => {
                let stored = session::get(&app.db.lock().unwrap(), &id).map_err(fatal)?;
                let parent = stored.and_then(|s| json::get_str(&s.info, "parentID").map(str::to_owned));
                parents.insert(id, parent.clone());
                parent
            }
        };
        current = parent;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_texts() {
        let answer = json::parse(
            r#"{"type":"assistant","content":[{"type":"reasoning","text":"r"},{"type":"text","text":"a"},{"type":"text","text":""},{"type":"text","text":"b"}]}"#,
        )
        .unwrap();
        assert_eq!(completion_text(Some(&answer)), "a\n\nb");
        assert_eq!(completion_text(None), NO_TEXT);
        let silent = json::parse(r#"{"type":"assistant","content":[{"type":"tool","name":"read"}]}"#).unwrap();
        assert_eq!(completion_text(Some(&silent)), NO_TEXT);
    }
}
