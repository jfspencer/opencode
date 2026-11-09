//! The session runner (packages/core/src/session/runner/llm.ts): one drain is a turn. At each boundary it promotes
//! pending input (all steers; at the idle boundary also one queued input), prepares the step's context (agent,
//! tools, instructions, model, history), starts title generation for an untitled session's first input, and runs
//! steps until the model stops calling tools. `runStep` owns the step policy: retries (`session.retry.scheduled`),
//! continuation after a stream that broke after output (a synthetic message), and the output-limit streak. A `move`
//! item at a boundary moves the session and ends the drain as `Moved`; the turn continues in the new location.
//!
//! Compaction (automatic before a step, overflow recovery, a manual `compaction` item at a boundary) is
//! [`crate::session::compaction`]'s.
//!
//! Each step reads its location as it is then (a reloaded location is booted again) and waits out an inventory
//! reload in progress (`plugins.awaitActivation`). At the idle boundary the goal plugin's `experimental.idle` hook,
//! given the session's running background work, may admit input that continues the execution
//! ([`crate::session::goal`]); external plugins' hooks are out of scope.

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
pub(crate) struct Loaded {
    pub(crate) session: Json,
    pub(crate) agent_id: String,
    pub(crate) agent: Json,
    pub(crate) location: Arc<Location>,
    pub(crate) reference: LocationRef,
    pub(crate) tools: crate::tool::Snapshot,
    pub(crate) sources: Vec<instructions::Source>,
    pub(crate) model: Arc<llm::Model>,
}

/// A boundary's selection (`SessionContext.Selection`), before the model is resolved.
pub(crate) struct Selected {
    session: Json,
    agent_id: String,
    agent: Json,
    location: Arc<Location>,
    reference: LocationRef,
    tools: crate::tool::Snapshot,
    sources: Vec<instructions::Source>,
    inventory: Arc<crate::inventory::Inventory>,
}

/// `prepareContext` (`SessionContext.select` + `InstructionState.prepare`); `record` false only selects (a preview).
pub(crate) async fn select(app: &Arc<App>, session_id: &str, record: bool) -> Result<Selected, Stop> {
    let stored = crate::session::require(&app.db.lock().unwrap(), session_id).map_err(failed)?;
    let location = app.locations.get(app, &stored.location).await.map_err(failed)?;
    let info = stored.info.clone();
    // One inventory snapshot per step: plugins may reload mid-turn, and each step reads them settled.
    location.inventory.settled().await;
    let inventory = location.inventory();
    let (agent_id, agent) = context::select_agent(&inventory, &info).map_err(Stop::Failed)?;
    // Session permissions narrow the tool snapshot the way they narrow discovery.
    let mut rules: Vec<Json> = match json::get(&agent, "permissions") {
        Some(Json::Array(rules)) => rules.clone(),
        _ => vec![],
    };
    if let Some(Json::Array(session_rules)) = json::get(&info, "permissions") {
        rules.extend(session_rules.iter().cloned());
    }
    let extra: Vec<_> = crate::tool::schedule::tools(&inventory).into_iter().chain(crate::tool::todo::tools(&inventory)).collect();
    let mut tools = crate::tool::registry().snapshot_with(&rules, &extra);
    crate::tool::shell::bind(&mut tools, &inventory);
    crate::tool::subagent::describe(&mut tools, &inventory, &agent_id);
    let git = location.vcs.as_ref().is_some_and(|vcs| vcs.kind == "git");
    let sources = {
        let db = app.db.lock().unwrap();
        let mut sources = instructions::sources(
            &db,
            &location.directory,
            &location.project.directory,
            git,
            tools.code_mode_catalog.clone(),
            instructions::skill_guidance(&inventory.skills, &rules),
            &inventory,
        );
        // API-managed entries come last: they vary the most between sessions.
        sources.extend(crate::session::entries::sources(&db, session_id).map_err(|e| failed(fatal(e)))?);
        sources
    };
    let reference = stored.location.clone();
    if record {
        instructions::prepare(app, session_id, &sources, &reference).map_err(failed)?;
    }
    Ok(Selected { session: info, agent_id, agent, location, reference, tools, sources, inventory })
}

/// `SessionContext.load`: resolves the selection's model. It runs after promotion, so input whose model is unavailable
/// is still delivered before the turn fails.
pub(crate) fn load(selected: Selected) -> Result<Loaded, Stop> {
    let Selected { session, agent_id, agent, location, reference, mut tools, sources, inventory } = selected;
    let model = context::resolve_model(&inventory, &session, Some(&agent)).map_err(Stop::Failed)?;
    // The patch plugin's `context`, `compaction` and `generate` hook: which editing tools the model gets.
    crate::tool::patch::filter(&mut tools, &model.id);
    Ok(Loaded { session, agent_id, agent, location, reference, tools, sources, model })
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

/// A drain's input (`DrainInput`): `continuation` is the step a turn that moved resumes at, `stepped` whether a step
/// already ran before it moved.
pub struct Drain {
    pub force: bool,
    pub promotable: Promotable,
    pub continuation: Option<i64>,
    pub stepped: bool,
}

/// How a drain ended (`DrainResult`).
pub enum Drained {
    Complete,
    /// The session moved at a boundary; the turn continues in its new location.
    Moved { continuation: Option<i64>, stepped: bool },
}

/// One drain (`SessionRunner.drain`).
pub async fn drain(app: &Arc<App>, session: &str, input: Drain, cancel: &Cancel) -> Result<Drained, Stop> {
    let Drain { force, promotable, continuation, stepped } = input;
    let mut force = force;
    let mut continuing = continuation.is_some();
    let mut step: i64 = continuation.unwrap_or(1);
    let mut entering = true;
    let mut stepped = stepped;
    if !force && !continuing && !stepped {
        let pending = inbox::next_promotable(&app.db.lock().unwrap(), session, Promotable::Input).map_err(|e| failed(fatal(e)))?;
        let Some(pending) = pending else { return Ok(Drained::Complete) };
        if promotable == Promotable::Steer && pending.delivery == "queue" && !pending.control() {
            return Ok(Drained::Complete);
        }
    }
    let location = crate::session::require(&app.db.lock().unwrap(), session).map_err(failed)?.location;
    crate::session::compaction::settle_stale(app, session, &location)?;
    settle_stale_tool_calls(app, session, &location)?;
    loop {
        if cancel.is_cancelled() {
            return Err(Stop::Interrupted);
        }
        // Advance to the next step boundary.
        let loaded = loop {
            let scope = if entering || !continuing { Promotable::Input } else { Promotable::Steer };
            let pending = inbox::next_promotable(&app.db.lock().unwrap(), session, scope).map_err(|e| failed(fatal(e)))?;
            if let Some(next) = pending.as_ref().filter(|p| p.ty == "move") {
                crate::session::moves::deliver(app, session, next).map_err(failed)?;
            }
            if !continuing && pending.as_ref().is_none_or(|p| p.delivery != "steer") {
                entering = true;
                step = 1;
            }
            if pending.as_ref().is_some_and(|p| p.ty == "move") {
                return Ok(Drained::Moved { continuation: continuing.then_some(step), stepped: !continuing && stepped });
            }
            if let Some(next) = pending.as_ref().filter(|p| p.ty == "compaction") {
                crate::session::compaction::manual(app, session, &next.id, &location, cancel).await?;
                force = false;
                continue;
            }
            if !force
                && !continuing
                && pending.as_ref().is_none_or(|p| p.delivery == "queue" && promotable == Promotable::Steer)
            {
                if pending.is_some() || !stepped {
                    return Ok(Drained::Complete);
                }
                // The session would go idle; the goal plugin (the `experimental.idle` hook) may admit input to keep this
                // execution working.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(Stop::Interrupted),
                    _ = crate::session::goal::idle(app, session) => {}
                }
                if !inbox::has(&app.db.lock().unwrap(), session, None).map_err(|e| failed(fatal(e)))? {
                    return Ok(Drained::Complete);
                }
                continue;
            }
            let selected = select(app, session, true).await?;
            let scope = if entering && !continuing { promotable } else { Promotable::Steer };
            let Some(promoted) = inbox::promote(app, session, scope, &selected.reference).map_err(failed)? else { continue };
            if promoted > 0
                && json::get(&selected.session, "parentID").is_none()
                && crate::session::title::untitled(&selected.session)
            {
                crate::session::title::spawn(app, session);
            }
            if promoted > 0 {
                step = 1;
            }
            break load(selected)?;
        };
        continuing = run_step(app, session, loaded, step, cancel).await?;
        stepped = true;
        step += 1;
        force = false;
        entering = false;
    }
}

/// The request's system parts (`baseTranscript` with the `context` hooks): the agent's prompt, the model's identity, the
/// initial instructions, then the worktree hint.
pub(crate) fn system(loaded: &Loaded, initial: &[(String, String)]) -> Vec<String> {
    let names = loaded.tools.names();
    let mut system = vec![match json::get_str(&loaded.agent, "system") {
        Some(system) => system.to_owned(),
        None => context::model_prompt(&loaded.model, &names),
    }];
    let joined = initial.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("\n\n");
    if !joined.is_empty() {
        system.push(joined);
    }
    system.retain(|part| !part.is_empty());
    system.insert(1.min(system.len()), context::identity(&loaded.model.name, &loaded.model.reference));
    system.push(context::WORKTREE_HINT.to_owned());
    system
}

/// `SessionGenerate.generate`'s request: the session's context as the next step would send it (its settled history and
/// any pending instruction update, recorded nowhere), then the prompt, without an output limit. `Err` is the message of
/// what kept the context from being selected.
pub async fn generate_request(app: &Arc<App>, session_id: &str, prompt: &str) -> Result<llm::Request, String> {
    let reason = |stop: Stop| match stop {
        Stop::Failed(error) => json::get_str(&error, "message").unwrap_or_default().to_owned(),
        Stop::Interrupted => "Interrupted".to_owned(),
    };
    let loaded = select(app, session_id, false).await.and_then(load).map_err(reason)?;
    let (history, initial, update) = {
        let db = app.db.lock().unwrap();
        let mut history = message::history(&db, session_id).map_err(|e| e.to_string())?;
        // An active assistant may hold an unresolved tool call: only the settled prefix is previewed.
        if let Some(unsettled) = history.iter().position(|m| {
            json::get_str(m, "type") == Some("assistant") && json::get(m, "time").and_then(|t| json::get(t, "completed")).is_none()
        }) {
            history.truncate(unsettled);
        }
        let (initial, update) = instructions::preview(&db, session_id, &loaded.sources).map_err(|e| e.to_string())?;
        (history, initial, update)
    };
    let mut messages = context::messages(&history, &loaded.model.reference);
    if !update.is_empty() {
        messages.push(llm::Message::System(update));
    }
    messages.push(llm::Message::User(vec![llm::UserPart::Text(prompt.to_owned())]));
    context::unsupported_parts(&mut messages, &loaded.model.input, &loaded.model.provider);
    Ok(llm::Request {
        model: loaded.model.clone(),
        system: system(&loaded, &initial),
        messages,
        tools: loaded.tools.definitions().cloned().collect(),
        tool_choice: None,
        max_tokens: None,
        prompt_cache_key: Some(context::cache_key(&loaded.session)),
        headers: context::headers(&loaded.session),
    })
}

/// Builds the step's request (`baseTranscript` + `SessionModelRequest.primary` with the context hooks).
/// The history and initial instructions are the loaded context's (`compacting`), whose size `estimate` is.
fn request(
    loaded: &Loaded,
    step_limit: bool,
    compacting: &crate::session::compaction::Context,
    estimate: crate::session::compaction::Estimate,
) -> Result<llm::Request, Stop> {
    let session = &loaded.session;
    let system = system(loaded, compacting.initial());
    let mut messages = context::messages(compacting.messages(), &loaded.model.reference);
    if step_limit {
        messages.push(llm::Message::Assistant(vec![llm::AssistantPart::Text { text: MAX_STEPS_PROMPT.into(), native: None }]));
    }
    crate::session::plan::reconcile(&mut messages, &loaded.agent_id);
    context::unsupported_parts(&mut messages, &loaded.model.input, &loaded.model.provider);
    Ok(llm::Request {
        model: loaded.model.clone(),
        system,
        messages,
        tools: loaded.tools.definitions().cloned().collect(),
        tool_choice: step_limit.then_some("none"),
        max_tokens: Some(context::output_limit(&loaded.model, estimate.measured, estimate.estimated)),
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
    let mut recover_overflow = true;
    loop {
        let loaded = match initial.take() {
            Some(loaded) => loaded,
            None => load(select(app, session, true).await?)?,
        };
        let compacting = Arc::new(crate::session::compaction::Context::load(app, &loaded)?);
        let estimate = crate::session::compaction::estimate_prompt(&compacting);
        match crate::session::compaction::auto(app, &compacting, estimate, cancel).await? {
            crate::session::compaction::Outcome::Failed(error) => return Err(Stop::Failed(error)),
            crate::session::compaction::Outcome::Completed => {
                assistant = message::create_id();
                continue;
            }
            crate::session::compaction::Outcome::Skipped => {}
        }
        crate::session::compaction::precompute(app, &compacting, Some(estimate));
        let limit = json::get_num(&loaded.agent, "steps").is_some_and(|steps| step as f64 >= steps);
        let mut request = request(&loaded, limit, &compacting, estimate)?;
        // The goal plugin's `context` hook (after plan mode's).
        crate::session::goal::remind(app, session, &mut request.messages);
        let location = loaded.reference.clone();
        let input = step::Input {
            session: session.to_owned(),
            assistant: assistant.clone(),
            agent: loaded.agent_id.clone(),
            model: loaded.model.clone(),
            request,
            tools: loaded.tools.clone(),
            location: location.clone(),
            located: loaded.location.clone(),
            directory: loaded.location.directory.clone(),
            recover_continuation,
            recover_overflow: recover_overflow.then(|| {
                crate::session::compaction::recover_overflow(app.clone(), compacting.clone(), estimate, cancel.clone())
            }),
            cancel: cancel.clone(),
        };
        match step::attempt(app, input, &mut retry).await? {
            Outcome::Completed { needs_continuation } => {
                // The session is about to go idle, which leaves a background summary time to finish.
                if !needs_continuation {
                    crate::session::compaction::precompute_idle(app, &compacting);
                }
                return Ok(needs_continuation);
            }
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
            Outcome::Compacted => {
                recover_overflow = false;
                assistant = message::create_id();
            }
        }
    }
}
