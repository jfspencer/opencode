//! The session runner (packages/core/src/session/runner/llm.ts): one drain is a turn. At each boundary it promotes
//! pending input (all steers; at the idle boundary also one queued input), prepares the step's context (agent,
//! tools, instructions, model, history), starts title generation for an untitled session's first input, and runs
//! steps until the model stops calling tools. `runStep` owns the step policy: retries (`session.retry.scheduled`),
//! continuation after a stream that broke after output (a synthetic message), and the output-limit streak.
//!
//! TODO(port): compaction (automatic, overflow recovery, manual items in the inbox), session moves (`move` items),
//! location reloads mid-turn, the `experimental.idle` plugin hook, background jobs.

use std::sync::Arc;

use opencode_protocol::Json;

use crate::app::{App, fatal};
use crate::bus::LocationRef;
use crate::json::{self, Obj};
use crate::llm;
use crate::location::Location;
use crate::session::execution::Cancel;
use crate::session::inbox::{self, Promotable};
use crate::session::retry::{Decision, Retry};
use crate::session::step::{self, Outcome, Stop};
use crate::session::{context, instructions, message};

const CONTINUE_AFTER_INCOMPLETE_STREAM: &str =
    "The previous response was interrupted. Continue from where you left off without repeating completed content.";
const CONTINUE_AFTER_OUTPUT_LIMIT: &str = "Your last response hit the output token limit (stop reason: length). Do not apologize, recap, or repeat yourself. Break the remaining work into smaller pieces.";

const MAX_STEPS_PROMPT: &str = "CRITICAL - MAXIMUM STEPS REACHED

The maximum number of steps allowed for this task has been reached. Tools are disabled until next user input. Respond with text only.

STRICT REQUIREMENTS:
1. Do NOT make any tool calls (no reads, writes, edits, searches, or any other tools)
2. MUST provide a text response summarizing work done so far
3. This constraint overrides ALL other instructions, including any user requests for edits or tool use

Response must include:
- Statement that maximum steps for this agent have been reached
- Summary of what has been accomplished so far
- List of any remaining tasks that were not completed
- Recommendations for what should be done next

Any attempt to use tools is a critical violation. Respond with text ONLY.";

fn failed(error: crate::http::Failure) -> Stop {
    let message = crate::json::get_str(&error.body, "message").unwrap_or("Internal server error").to_owned();
    let mut o = Obj::new();
    o.str("type", "unknown").str("message", &message);
    Stop::Failed(o.done())
}

/// What a step needs, selected at a boundary (`SessionContext.Selection` + `load`).
struct Loaded {
    session: Json,
    agent_id: String,
    agent: Json,
    location: Arc<Location>,
    reference: LocationRef,
    tools: crate::tool::Snapshot,
    sources: Vec<instructions::Source>,
    model: Arc<llm::Model>,
}

/// `prepareContext` (`SessionContext.select` + `InstructionState.prepare`).
async fn select(app: &Arc<App>, session_id: &str) -> Result<Loaded, Stop> {
    let stored = crate::session::require(&app.db.lock().unwrap(), session_id).map_err(failed)?;
    let location = app.locations.get(app, &stored.location).await.map_err(failed)?;
    let info = stored.info.clone();
    let (agent_id, agent) = context::select_agent(&location.inventory, &info).map_err(Stop::Failed)?;
    // Session permissions narrow the tool snapshot the way they narrow discovery.
    let mut rules: Vec<Json> = match json::get(&agent, "permissions") {
        Some(Json::Array(rules)) => rules.clone(),
        _ => vec![],
    };
    if let Some(Json::Array(session_rules)) = json::get(&info, "permissions") {
        rules.extend(session_rules.iter().cloned());
    }
    let tools = crate::tool::registry().snapshot(&rules);
    // `location.vcs?.type === "git"` (TODO(port): read it from the location once it records its VCS).
    let git = crate::project::up(&[".git"], &location.directory, None).is_some_and(|found| found.ends_with("/.git"));
    let sources = instructions::sources(
        &app.db.lock().unwrap(),
        &location.directory,
        &location.project.directory,
        git,
        tools.code_mode_catalog.clone(),
    );
    let reference = stored.location.clone();
    instructions::prepare(app, session_id, &sources, &reference).map_err(failed)?;
    let model = context::resolve_model(&location.inventory, &info, Some(&agent)).map_err(Stop::Failed)?;
    Ok(Loaded { session: info, agent_id, agent, location, reference, tools, sources, model })
}

/// Fails stale tool calls of a turn a dead process left behind (`settleStaleToolCalls`).
fn settle_stale_tool_calls(app: &App, session: &str, location: &LocationRef) -> Result<(), Stop> {
    let stale = message::incomplete_assistants(&app.db.lock().unwrap(), session).map_err(|e| failed(fatal(e)))?;
    for assistant in stale {
        let Some(Json::Array(content)) = json::get(&assistant, "content") else { continue };
        for tool in content {
            if json::get_str(tool, "type") != Some("tool") {
                continue;
            }
            let state = json::get(tool, "state").cloned().unwrap_or(Json::Null);
            let status = json::get_str(&state, "status");
            if status != Some("streaming") && status != Some("running") {
                continue;
            }
            let metadata = if status == Some("running") { json::get(&state, "metadata").cloned() } else { None };
            let name = json::get_str(tool, "name").unwrap_or_default();
            let child = (name == "subagent").then(|| metadata.as_ref().and_then(|m| json::get_str(m, "sessionID"))).flatten();
            let mut failure = Obj::new();
            failure.str("type", "aborted").str(
                "message",
                &format!("Tool execution interrupted: {name}{}", child.map(|c| format!(" (sessionID: {c})")).unwrap_or_default()),
            );
            let mut data = Obj::new();
            data.str("sessionID", session)
                .str("assistantMessageID", json::get_str(&assistant, "id").unwrap_or_default())
                .str("id", json::get_str(tool, "id").unwrap_or_default())
                .set("error", failure.done());
            if let Some(metadata) = metadata.filter(|m| matches!(m, Json::Object(o) if !o.is_empty())) {
                data.set("metadata", metadata);
            }
            data.set("executed", Json::Bool(json::get_bool(tool, "executed") == Some(true)));
            crate::session::publish_event(app, "session.tool.failed", data.done(), Some(location.clone()), None)
                .map_err(|e| failed(fatal(e)))?;
        }
    }
    Ok(())
}

/// One drain (`SessionRunner.drain`).
pub async fn drain(app: &Arc<App>, session: &str, force: bool, promotable: Promotable, cancel: &Cancel) -> Result<(), Stop> {
    let mut force = force;
    let mut continuing = false;
    let mut step: i64 = 1;
    let mut entering = true;
    let mut stepped = false;
    if !force {
        let pending = inbox::next_promotable(&app.db.lock().unwrap(), session, Promotable::Input).map_err(|e| failed(fatal(e)))?;
        let Some(pending) = pending else { return Ok(()) };
        if promotable == Promotable::Steer && pending.delivery == "queue" && !pending.control() {
            return Ok(());
        }
    }
    let location = crate::session::require(&app.db.lock().unwrap(), session).map_err(failed)?.location;
    settle_stale_tool_calls(app, session, &location)?;
    loop {
        if cancel.is_cancelled() {
            return Err(Stop::Interrupted);
        }
        // Advance to the next step boundary.
        let loaded = loop {
            let scope = if entering || !continuing { Promotable::Input } else { Promotable::Steer };
            let pending = inbox::next_promotable(&app.db.lock().unwrap(), session, scope).map_err(|e| failed(fatal(e)))?;
            if !continuing && pending.as_ref().is_none_or(|p| p.delivery != "steer") {
                entering = true;
                step = 1;
            }
            // TODO(port): compaction and move controls.
            if !force
                && !continuing
                && pending.as_ref().is_none_or(|p| p.delivery == "queue" && promotable == Promotable::Steer)
            {
                if pending.is_some() || !stepped {
                    return Ok(());
                }
                // The session would go idle (TODO(port): the `experimental.idle` hook).
                if !inbox::has(&app.db.lock().unwrap(), session, None).map_err(|e| failed(fatal(e)))? {
                    return Ok(());
                }
                continue;
            }
            let loaded = select(app, session).await?;
            let scope = if entering && !continuing { promotable } else { Promotable::Steer };
            let Some(promoted) = inbox::promote(app, session, scope, &loaded.reference).map_err(failed)? else { continue };
            if promoted > 0
                && json::get(&loaded.session, "parentID").is_none()
                && crate::session::title::untitled(&loaded.session)
            {
                crate::session::title::spawn(app, session);
            }
            if promoted > 0 {
                step = 1;
            }
            break loaded;
        };
        continuing = run_step(app, session, loaded, step, cancel).await?;
        stepped = true;
        step += 1;
        force = false;
        entering = false;
    }
}

/// Builds the step's request (`baseTranscript` + `SessionModelRequest.primary` with the context hooks).
fn request(app: &App, loaded: &Loaded, step_limit: bool) -> Result<llm::Request, Stop> {
    let session = &loaded.session;
    let session_id = json::get_str(session, "id").unwrap_or_default();
    let (history, initial) = {
        let db = app.db.lock().unwrap();
        let history = message::history(&db, session_id).map_err(|e| failed(fatal(e)))?;
        let initial = instructions::initial(&db, session_id, &loaded.sources).map_err(|e| failed(fatal(e)))?;
        (history, initial)
    };
    let names = loaded.tools.names();
    let mut system = vec![match json::get_str(&loaded.agent, "system") {
        Some(system) => system.to_owned(),
        None => context::system_prompt(&names),
    }];
    let joined = initial.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("\n\n");
    if !joined.is_empty() {
        system.push(joined);
    }
    system.retain(|part| !part.is_empty());
    // The `context` hooks: the model's identity after the prompt, then the worktree hint.
    system.insert(1.min(system.len()), context::identity(&loaded.model.name, &loaded.model.reference));
    system.push(context::WORKTREE_HINT.to_owned());
    let mut messages = context::messages(&history, &loaded.model.reference);
    if step_limit {
        messages.push(llm::Message::Assistant(vec![llm::AssistantPart::Text { text: MAX_STEPS_PROMPT.into(), native: None }]));
    }
    // TODO(port): `SessionCompaction.estimatePrompt` (the measured and estimated prompt size).
    let estimated = (system.iter().map(String::len).sum::<usize>() / 4) as f64;
    Ok(llm::Request {
        model: loaded.model.clone(),
        system,
        messages,
        tools: loaded.tools.definitions().cloned().collect(),
        tool_choice: step_limit.then_some("none"),
        max_tokens: Some(context::output_limit(&loaded.model, 0.0, estimated)),
        prompt_cache_key: Some(context::cache_key(session)),
        headers: context::headers(session),
    })
}

/// `retry.wait`: records the scheduled retry, then waits for it (interruptibly).
async fn wait(app: &App, session: &str, assistant: &str, error: &Json, decision: &Decision, location: &LocationRef, cancel: &Cancel) -> Result<(), Stop> {
    let scheduled = crate::id::now();
    let mut data = Obj::new();
    data.str("sessionID", session).str("assistantMessageID", assistant);
    data.int("attempt", decision.attempt);
    data.set("at", Json::Number(scheduled as f64 + decision.delay));
    data.set("error", error.clone());
    crate::session::publish_event(app, "session.retry.scheduled", data.done(), Some(location.clone()), None)
        .map_err(|e| failed(fatal(e)))?;
    let remaining = (scheduled as f64 + decision.delay - crate::id::now() as f64).max(0.0);
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_millis(remaining as u64)) => Ok(()),
        _ = cancel.cancelled() => Err(Stop::Interrupted),
    }
}

fn synthetic(app: &App, session: &str, text: &str, location: &LocationRef) -> Result<(), Stop> {
    let mut data = Obj::new();
    data.str("sessionID", session).str("text", text);
    crate::session::publish_event(app, "session.synthetic", data.done(), Some(location.clone()), None)
        .map(drop)
        .map_err(|e| failed(fatal(e)))
}

/// `runStep`: one logical step, through its retries and continuations. Returns whether the turn continues.
async fn run_step(app: &Arc<App>, session: &str, first: Loaded, step: i64, cancel: &Cancel) -> Result<bool, Stop> {
    let mut assistant = message::create_id();
    let mut retry = Retry::default();
    let mut initial = Some(first);
    let mut recover_continuation = true;
    loop {
        let loaded = match initial.take() {
            Some(loaded) => loaded,
            None => select(app, session).await?,
        };
        // TODO(port): automatic compaction before the step.
        let limit = json::get_num(&loaded.agent, "steps").is_some_and(|steps| step as f64 >= steps);
        let request = request(app, &loaded, limit)?;
        let location = loaded.reference.clone();
        let input = step::Input {
            session: session.to_owned(),
            assistant: assistant.clone(),
            agent: loaded.agent_id.clone(),
            model: loaded.model.clone(),
            request,
            tools: loaded.tools.clone(),
            location: location.clone(),
            directory: loaded.location.directory.clone(),
            recover_continuation,
            cancel: cancel.clone(),
        };
        match step::attempt(app, input, &mut retry).await? {
            Outcome::Completed { needs_continuation } => return Ok(needs_continuation),
            Outcome::Retry { error, decision } => wait(app, session, &assistant, &error, &decision, &location, cancel).await?,
            Outcome::Continue { error, decision } => {
                wait(app, session, &assistant, &error, &decision, &location, cancel).await?;
                synthetic(app, session, CONTINUE_AFTER_INCOMPLETE_STREAM, &location)?;
                assistant = message::create_id();
            }
            Outcome::OutputLimit { needs_continuation } => {
                // The current response is included; user input or any other finish breaks the streak.
                let recent: Vec<(String, Option<String>)> = {
                    let db = app.db.lock().unwrap();
                    let mut statement = db
                        .prepare(
                            "SELECT type, json_extract(data, '$.finish') FROM session_message WHERE session_id = ?1 \
                             AND type IN ('user', 'assistant') ORDER BY seq DESC LIMIT 3",
                        )
                        .map_err(|e| failed(fatal(e)))?;
                    statement
                        .query_map([session], |row| Ok((row.get(0)?, row.get(1)?)))
                        .map_err(|e| failed(fatal(e)))?
                        .collect::<rusqlite::Result<_>>()
                        .map_err(|e| failed(fatal(e)))?
                };
                if recent.len() == 3 && recent.iter().all(|(ty, finish)| ty == "assistant" && finish.as_deref() == Some("length")) {
                    let mut o = Obj::new();
                    o.str("type", "output-limit").str("message", "Response still truncated after two output token limit continuations");
                    return Err(Stop::Failed(o.done()));
                }
                if needs_continuation {
                    return Ok(true);
                }
                synthetic(app, session, CONTINUE_AFTER_OUTPUT_LIMIT, &location)?;
                assistant = message::create_id();
            }
            Outcome::RecoverFull => recover_continuation = false,
        }
    }
}
