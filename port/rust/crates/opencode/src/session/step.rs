//! One step attempt (packages/core/src/session/runner/step.ts, runner/publish-llm-event.ts): streams the model's
//! answer, publishes it as durable session events (step, text, reasoning, tool lifecycle), runs the local tool calls
//! it makes concurrently, and settles the step (ended, failed, or handed back for a retry or continuation).
//!
//! Deltas are batched as TS batches them: a fragment's pending text is published 100 ms after it first became
//! pending, or earlier when another block starts or the fragment ends, so the published order matches TS's.
//!
//! TODO(port): start/end snapshots and changed files (`Snapshot.capture` in git projects), provider-hosted tool
//! results, tool progress throttling, `ToolOutput.truncate`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_protocol::Json;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::app::App;
use crate::bus::LocationRef;
use crate::json::{self, Obj};
use crate::llm::{self, AIError, Event};
use crate::session::execution::Cancel;
use crate::session::retry::{Decision, Retry};
use crate::tool;

const DELTA_BATCH: Duration = Duration::from_millis(100);
const INPUT_EXCERPT_LIMIT: usize = 2048;

pub enum Outcome {
    Completed { needs_continuation: bool },
    Retry { error: Json, decision: Decision },
    Continue { error: Json, decision: Decision },
    /// A WebSocket transport's failure before output asks for a fresh request (TODO(port): WebSocket transports).
    #[allow(dead_code)]
    RecoverFull,
    OutputLimit { needs_continuation: bool },
}

/// How an attempt ended without an outcome.
pub enum Stop {
    /// Interrupted (by the user, a declined permission, or shutdown).
    Interrupted,
    /// The step failed: the session error the execution records.
    Failed(Json),
}

fn error(ty: &str, message: &str) -> Json {
    let mut o = Obj::new();
    o.str("type", ty).str("message", message);
    o.done()
}

#[derive(Debug, Clone)]
struct Fragment {
    ordinal: i64,
    values: String,
    pending: String,
    due: Option<Instant>,
    native: Option<Json>,
}

#[derive(Default)]
struct Fragments {
    chunks: BTreeMap<String, Fragment>,
    order: Vec<String>,
    next: i64,
}

impl Fragments {
    fn start(&mut self, id: &str, native: Option<Json>) -> i64 {
        let ordinal = self.next;
        self.next += 1;
        self.chunks.insert(id.to_owned(), Fragment { ordinal, values: String::new(), pending: String::new(), due: None, native });
        self.order.push(id.to_owned());
        ordinal
    }

    fn has(&self, id: &str) -> bool {
        self.chunks.contains_key(id)
    }

    fn remove(&mut self, id: &str) -> Option<Fragment> {
        self.order.retain(|k| k != id);
        self.chunks.remove(id)
    }
}

fn merge_native(current: &Option<Json>, next: &Option<Json>) -> Option<Json> {
    match (current, next) {
        (_, None) => current.clone(),
        (None, Some(next)) => Some(next.clone()),
        (Some(current), Some(next)) => {
            let mut merged = current.clone();
            if let Json::Object(fields) = next {
                for (key, value) in fields {
                    json::set(&mut merged, key, value.clone());
                }
            }
            Some(merged)
        }
    }
}

#[derive(Debug, Clone)]
struct ToolState {
    name: String,
    called: bool,
    settled: bool,
    provider_executed: bool,
    progress: Option<Json>,
    input: Option<(String, usize)>,
    malformed: bool,
}

/// The durable facts recorded so far (`StepRecord`).
#[derive(Debug, Clone, Default)]
pub struct Record {
    pub output_started: bool,
    pub provider_failed: bool,
    pub failure: Option<Json>,
    pub finish: Option<Settlement>,
    pub needs_continuation: bool,
}

#[derive(Debug, Clone)]
pub struct Settlement {
    pub finish: String,
    pub raw: Option<String>,
    pub native: Option<Json>,
    pub usage: Option<llm::Usage>,
}

/// `createLLMEventPublisher`: the step's writer.
pub struct Publisher {
    app: Arc<App>,
    session: String,
    assistant: String,
    agent: String,
    model: Json,
    location: LocationRef,
    started_at: i64,
    step_started: bool,
    provider_failed: bool,
    output_started: bool,
    failure: Option<Json>,
    settlement: Option<Settlement>,
    tools: Vec<(String, ToolState)>,
    text: Fragments,
    reasoning: Fragments,
    tool_input: Fragments,
}

impl Publisher {
    fn publish(&self, ty: &'static str, data: Json) {
        if let Err(error) = crate::session::publish_event(&self.app, ty, data, Some(self.location.clone()), None) {
            eprintln!("failed to publish {ty}: {error}");
        }
    }

    fn base(&self) -> Obj {
        let mut o = Obj::new();
        o.str("sessionID", &self.session).str("assistantMessageID", &self.assistant);
        o
    }

    fn tool(&mut self, id: &str) -> Option<&mut ToolState> {
        self.tools.iter_mut().find(|(k, _)| k == id).map(|(_, t)| t)
    }

    pub fn start_assistant(&mut self) -> String {
        if self.step_started {
            return self.assistant.clone();
        }
        self.step_started = true;
        let mut o = Obj::new();
        o.str("sessionID", &self.session).str("agent", &self.agent).set("model", self.model.clone());
        o.str("assistantMessageID", &self.assistant);
        // TODO(port): `snapshot` (the start snapshot in git projects).
        o.int("started", self.started_at);
        self.publish("session.step.started", o.done());
        self.assistant.clone()
    }

    pub fn streamed(&mut self) {
        self.start_assistant();
        let o = self.base();
        let mut o = o;
        self.publish("session.step.streamed", o.done());
    }

    fn publish_delta(&mut self, reasoning: bool, id: &str) {
        let fragments = if reasoning { &mut self.reasoning } else { &mut self.text };
        let Some(fragment) = fragments.chunks.get_mut(id) else { return };
        fragment.due = None;
        if fragment.pending.is_empty() {
            return;
        }
        let value = std::mem::take(&mut fragment.pending);
        let ordinal = fragment.ordinal;
        let mut o = self.base();
        o.int("ordinal", ordinal).str("delta", &value);
        self.publish(if reasoning { "session.reasoning.delta" } else { "session.text.delta" }, o.done());
    }

    /// Publishes batched deltas now, keeping every fragment open (`publishPendingDeltas`).
    fn publish_pending(&mut self) {
        for id in self.text.order.clone() {
            self.publish_delta(false, &id);
        }
        for id in self.reasoning.order.clone() {
            self.publish_delta(true, &id);
        }
    }

    /// Publishes the deltas whose batch interval has passed.
    pub fn publish_due(&mut self, now: Instant) {
        for reasoning in [false, true] {
            let fragments = if reasoning { &self.reasoning } else { &self.text };
            let due: Vec<String> =
                fragments.order.iter().filter(|id| fragments.chunks[*id].due.is_some_and(|d| d <= now)).cloned().collect();
            for id in due {
                self.publish_delta(reasoning, &id);
            }
        }
    }

    /// When the next batched delta is due.
    pub fn next_due(&self) -> Option<Instant> {
        self.text.chunks.values().chain(self.reasoning.chunks.values()).filter_map(|f| f.due).min()
    }

    fn append(&mut self, reasoning: bool, id: &str, value: &str, native: &Option<Json>) {
        let fragments = if reasoning { &mut self.reasoning } else { &mut self.text };
        let Some(fragment) = fragments.chunks.get_mut(id) else { return };
        fragment.values.push_str(value);
        fragment.pending.push_str(value);
        fragment.native = merge_native(&fragment.native, native);
        if !fragment.pending.is_empty() && fragment.due.is_none() {
            fragment.due = Some(Instant::now() + DELTA_BATCH);
        }
    }

    fn end(&mut self, reasoning: bool, id: &str, native: &Option<Json>, value: Option<String>) {
        if !(if reasoning { &self.reasoning } else { &self.text }).has(id) {
            return;
        }
        self.publish_delta(reasoning, id);
        let fragments = if reasoning { &mut self.reasoning } else { &mut self.text };
        let Some(fragment) = fragments.remove(id) else { return };
        let text = value.unwrap_or(fragment.values);
        let native = merge_native(&fragment.native, native);
        self.start_assistant();
        let mut o = self.base();
        o.int("ordinal", fragment.ordinal).str("text", &text);
        o.opt("native", native);
        self.publish(if reasoning { "session.reasoning.ended" } else { "session.text.ended" }, o.done());
    }

    fn end_tool_input(&mut self, id: &str, value: Option<String>) {
        let Some(fragment) = self.tool_input.remove(id) else { return };
        let text = value.unwrap_or(fragment.values);
        if let Some(tool) = self.tool(id) {
            tool.input = Some((text.chars().take(INPUT_EXCERPT_LIMIT).collect(), text.chars().count()));
        }
        let mut o = self.base();
        o.str("id", id).str("text", &text);
        self.publish("session.tool.input.ended", o.done());
    }

    fn flush_fragments(&mut self) {
        for id in self.text.order.clone() {
            self.end(false, &id, &None, None);
        }
        for id in self.reasoning.order.clone() {
            self.end(true, &id, &None, None);
        }
        for id in self.tool_input.order.clone() {
            self.end_tool_input(&id, None);
        }
    }

    fn start_tool_input(&mut self, id: &str, name: &str) {
        if self.tool(id).is_some() {
            return;
        }
        self.start_assistant();
        self.tools.push((
            id.to_owned(),
            ToolState { name: name.to_owned(), called: false, settled: false, provider_executed: false, progress: None, input: None, malformed: false },
        ));
        self.tool_input.start(id, None);
        self.publish_pending();
        let mut o = self.base();
        o.str("id", id).str("name", name);
        self.publish("session.tool.input.started", o.done());
    }

    /// `flush`: ends every fragment, then fails malformed tool inputs.
    pub fn flush(&mut self) {
        self.flush_fragments();
        let malformed: Vec<String> =
            self.tools.iter().filter(|(_, t)| t.malformed && !t.settled).map(|(id, _)| id.clone()).collect();
        for id in malformed {
            self.fail_tool_input(&id, "malformed");
        }
    }

    fn fail_tool_input(&mut self, id: &str, reason: &str) {
        let reason = if self.settlement.as_ref().is_some_and(|s| s.finish == "length") { "length" } else { reason };
        let input = self.tool(id).and_then(|t| t.input.clone());
        let (ty, message) = match reason {
            "malformed" => (
                "tool.input-json",
                "The tool input could not be parsed as JSON, so the call was not executed. Reissue the call with valid JSON arguments.",
            ),
            "length" => (
                "tool.input-incomplete",
                "The tool call was not executed because the output token limit (stop reason: length) was reached before you could complete the arguments. Break large payloads into smaller tool calls and reissue the call with complete arguments.",
            ),
            _ => (
                "tool.input-incomplete",
                "Tool call arguments were not completed and were not executed. Reissue the call with complete arguments.",
            ),
        };
        let message = match input {
            Some((text, length)) if length > 0 => {
                let shown = text.chars().count();
                let truncated = if length > shown { "\n[truncated]" } else { "" };
                format!("{message}\n\nInput excerpt (first {shown} of {length} characters):\n{text}{truncated}")
            }
            _ => message.to_owned(),
        };
        self.fail_tool(id, error(ty, &message), None);
    }

    /// `failTool`: settles a call as failed, once.
    pub fn fail_tool(&mut self, id: &str, failure: Json, metadata: Option<Json>) -> bool {
        let Some(tool) = self.tool(id) else { return false };
        if tool.settled {
            return false;
        }
        tool.settled = true;
        let tool = tool.clone();
        let failure = match (tool.name.as_str(), json::get_str(&failure, "type"), tool.progress.as_ref().and_then(|p| json::get_str(p, "sessionID"))) {
            ("subagent", Some("aborted"), Some(child)) => {
                let mut f = failure.clone();
                let message = format!("{} (sessionID: {child})", json::get_str(&failure, "message").unwrap_or_default());
                json::set(&mut f, "message", json::string(&message));
                f
            }
            _ => failure,
        };
        let snapshot = match (tool.progress, metadata) {
            (None, None) => None,
            (None, Some(m)) => Some(m),
            (Some(p), None) => Some(p),
            (Some(p), Some(m)) => merge_native(&Some(p), &Some(m)),
        };
        let mut o = self.base();
        o.str("id", id).set("error", failure);
        o.opt("metadata", snapshot);
        o.set("executed", Json::Bool(tool.provider_executed));
        self.publish("session.tool.failed", o.done());
        true
    }

    fn fail_tools(&mut self, failure: &Json, mode: &str) -> bool {
        let ids: Vec<String> = self
            .tools
            .iter()
            .filter(|(_, t)| !(t.settled || (mode == "hosted" && !t.provider_executed) || (mode == "uncalled" && t.called)))
            .map(|(id, _)| id.clone())
            .collect();
        let mut failed = false;
        for id in ids {
            if self.fail_tool(&id, failure.clone(), None) {
                failed = true;
            }
        }
        failed
    }

    pub fn fail_unsettled_tools(&mut self, failure: &Json, mode: &str) -> bool {
        self.fail_tools(failure, mode)
    }

    pub fn fail_incomplete_tools(&mut self) {
        let ids: Vec<String> = self.tools.iter().filter(|(_, t)| !t.called && !t.settled).map(|(id, _)| id.clone()).collect();
        for id in ids {
            self.fail_tool_input(&id, "incomplete");
        }
    }

    pub fn fail_assistant(&mut self, failure: Json) {
        self.flush();
        self.fail_tools(&failure, "uncalled");
        self.start_assistant();
        if self.failure.is_none() {
            self.failure = Some(failure);
        }
    }

    /// `publishStepFailure`.
    pub fn publish_step_failure(&mut self, usage: Option<(Json, Json)>) {
        let Some(failure) = self.failure.clone() else { return };
        self.start_assistant();
        let mut o = self.base();
        o.set("error", failure);
        if let Some(settlement) = &self.settlement {
            if settlement.finish == "content-filter" {
                o.str("finish", "content-filter");
            }
            o.opt_str("rawFinish", settlement.raw.as_deref());
            o.opt("native", settlement.native.clone());
        }
        if let Some((cost, tokens)) = usage {
            o.set("cost", cost).set("tokens", tokens);
        }
        self.publish("session.step.failed", o.done());
    }

    /// One provider event (`publish`).
    pub fn event(&mut self, event: Event) -> Option<(String, String, Json)> {
        match event {
            Event::StepStart => {
                self.start_assistant();
            }
            Event::TextStart { id, native } => {
                self.output_started = true;
                if self.text.has(&id) || !self.text.chunks.is_empty() {
                    return None;
                }
                let ordinal = self.text.start(&id, native);
                self.publish_pending();
                self.start_assistant();
                let mut o = self.base();
                o.int("ordinal", ordinal);
                self.publish("session.text.started", o.done());
            }
            Event::TextDelta { id, text } => self.append(false, &id, &text, &None),
            Event::TextEnd { id, text, native } => self.end(false, &id, &native, text),
            Event::ReasoningStart { id, native } => {
                self.output_started = true;
                if self.reasoning.has(&id) || !self.reasoning.chunks.is_empty() {
                    return None;
                }
                let ordinal = self.reasoning.start(&id, native.clone());
                self.publish_pending();
                self.start_assistant();
                let mut o = self.base();
                o.int("ordinal", ordinal);
                o.opt("native", native);
                self.publish("session.reasoning.started", o.done());
            }
            Event::ReasoningDelta { id, text, native } => self.append(true, &id, &text, &native),
            Event::ReasoningEnd { id, text, native } => self.end(true, &id, &native, text),
            Event::ToolInputStart { id, name } => {
                self.output_started = true;
                self.start_tool_input(&id, &name);
            }
            Event::ToolInputDelta { id, text, .. } => {
                if let Some(fragment) = self.tool_input.chunks.get_mut(&id) {
                    fragment.values.push_str(&text);
                }
            }
            Event::ToolInputEnd { id, .. } => self.end_tool_input(&id, None),
            Event::ToolInputError { id, name, raw } => {
                self.output_started = true;
                if self.tool(&id).is_none() {
                    self.start_tool_input(&id, &name);
                }
                if self.tool_input.has(&id) {
                    self.end_tool_input(&id, Some(raw.clone()));
                }
                if let Some(tool) = self.tool(&id) {
                    tool.input = Some((raw.chars().take(INPUT_EXCERPT_LIMIT).collect(), raw.chars().count()));
                    tool.malformed = true;
                }
            }
            Event::ToolCall { id, name, input, native } => {
                self.output_started = true;
                if self.tool(&id).is_none() {
                    self.start_tool_input(&id, &name);
                }
                if self.tool_input.has(&id) {
                    self.end_tool_input(&id, None);
                }
                let tool = self.tool(&id)?;
                if tool.called {
                    return None;
                }
                tool.called = true;
                tool.provider_executed = false;
                let input = if matches!(input, Json::Object(_)) {
                    input
                } else {
                    let mut o = Obj::new();
                    o.set("value", input);
                    o.done()
                };
                let mut o = self.base();
                o.str("id", &id).set("input", input.clone()).set("executed", Json::Bool(false));
                o.opt("native", native);
                self.publish("session.tool.called", o.done());
                return Some((id, name, input));
            }
            Event::StepFinish { finish, usage, native } => {
                self.flush_fragments();
                if self.settlement.is_some() {
                    return None;
                }
                let filtered = finish.normalized == "content-filter";
                self.settlement = Some(Settlement { finish: finish.normalized, raw: finish.raw, native, usage });
                if filtered {
                    self.provider_failed = true;
                    self.fail_assistant(error("provider.content-filter", "Provider blocked the response"));
                }
            }
            Event::Finish => {}
        }
        None
    }

    /// `toolExecution`: the terminal event of a local call.
    pub fn tool_execution(&mut self, id: &str, output: tool::Output) {
        let Some(tool) = self.tool(id) else { return };
        if !tool.called || tool.settled {
            return;
        }
        tool.settled = true;
        let executed = tool.provider_executed;
        let mut o = self.base();
        o.str("id", id).set("content", Json::Array(output.content));
        o.opt("metadata", output.metadata);
        o.set("executed", Json::Bool(executed));
        self.publish("session.tool.success", o.done());
    }

    pub fn progress(&mut self, id: &str, metadata: Json) {
        let Some(tool) = self.tool(id) else { return };
        if !tool.called || tool.settled {
            return;
        }
        tool.progress = Some(metadata.clone());
        let mut o = self.base();
        o.str("id", id).set("metadata", metadata);
        self.publish("session.tool.progress", o.done());
    }

    pub fn record(&self) -> Record {
        Record {
            output_started: self.output_started,
            provider_failed: self.provider_failed,
            failure: self.failure.clone(),
            finish: self.settlement.clone(),
            needs_continuation: self.tools.iter().any(|(_, t)| !t.provider_executed && (t.called || t.settled)),
        }
    }

    pub fn started(&self) -> bool {
        self.step_started
    }
}

/// `SessionUsage.record`: the step's tokens and its cost (as reported, else priced from the catalog).
pub fn usage(usage: Option<&llm::Usage>, costs: &[Json]) -> (Json, Json) {
    let safe = |v: Option<f64>| v.filter(|n| n.is_finite()).unwrap_or(0.0).max(0.0);
    let input = safe(usage.and_then(|u| u.non_cached_input));
    let output = safe(usage.map(|u| u.visible_output()));
    let reasoning = safe(usage.and_then(|u| u.reasoning));
    let read = safe(usage.and_then(|u| u.cache_read));
    let write = safe(usage.and_then(|u| u.cache_write));
    let mut cache = Obj::new();
    cache.set("read", Json::Number(read)).set("write", Json::Number(write));
    let mut tokens = Obj::new();
    tokens
        .set("input", Json::Number(input))
        .set("output", Json::Number(output))
        .set("reasoning", Json::Number(reasoning))
        .set("cache", cache.done());
    let cost = match usage.and_then(|u| u.cost) {
        Some(cost) => cost,
        None => {
            let context = input + read + write;
            let tier = costs
                .iter()
                .filter(|c| {
                    json::get(c, "tier").is_some_and(|t| {
                        json::get_str(t, "type") == Some("context") && json::get_num(t, "size").is_some_and(|s| context > s)
                    })
                })
                .max_by(|a, b| {
                    let size = |c: &Json| json::get(c, "tier").and_then(|t| json::get_num(t, "size")).unwrap_or(0.0);
                    size(a).total_cmp(&size(b))
                })
                .or_else(|| costs.iter().find(|c| json::get(c, "tier").is_none()));
            match tier {
                None => 0.0,
                Some(c) => {
                    let finite = |v: Option<f64>| v.filter(|n| n.is_finite()).unwrap_or(0.0);
                    let cache = json::get(c, "cache");
                    (input * finite(json::get_num(c, "input"))
                        + (output + reasoning) * finite(json::get_num(c, "output"))
                        + read * finite(cache.and_then(|c| json::get_num(c, "read")))
                        + write * finite(cache.and_then(|c| json::get_num(c, "write"))))
                        / 1_000_000.0
                }
            }
        }
    };
    (Json::Number(cost), tokens.done())
}

pub struct Input {
    pub session: String,
    pub assistant: String,
    pub agent: String,
    pub model: Arc<llm::Model>,
    pub request: llm::Request,
    pub tools: tool::Snapshot,
    pub location: LocationRef,
    /// The booted location the step runs in (what its tools act on).
    pub located: Arc<crate::location::Location>,
    pub directory: String,
    pub recover_continuation: bool,
    pub cancel: Cancel,
}

/// `isInterruptedStream`.
fn interrupted_stream(error: &AIError) -> bool {
    match error.kind {
        llm::error::Kind::InvalidProviderOutput => error.classification == Some("incomplete-stream"),
        llm::error::Kind::Transport => error.operation == Some("read"),
        _ => false,
    }
}

enum ToolExit {
    Done,
    /// The user declined the call (or dismissed its question): the turn stops.
    Declined(String, tool::Failure),
}

/// `SessionStep.attempt`.
pub async fn attempt(app: &Arc<App>, input: Input, retry: &mut Retry) -> Result<Outcome, Stop> {
    let started_at = crate::id::now();
    let publisher = Arc::new(Mutex::new(Publisher {
        app: app.clone(),
        session: input.session.clone(),
        assistant: input.assistant.clone(),
        agent: input.agent.clone(),
        model: input.model.reference.clone(),
        location: input.location.clone(),
        started_at,
        step_started: false,
        provider_failed: false,
        output_started: false,
        failure: None,
        settlement: None,
        tools: Vec::new(),
        text: Fragments::default(),
        reasoning: Fragments::default(),
        tool_input: Fragments::default(),
    }));
    let request = Arc::new(input.request);
    let (sender, mut receiver) = mpsc::channel::<Result<Event, AIError>>(64);
    let streaming = {
        let request = request.clone();
        tokio::spawn(async move { llm::stream(&request, sender).await })
    };
    let mut tools: tokio::task::JoinSet<ToolExit> = tokio::task::JoinSet::new();
    let tools_disabled = request.tool_choice == Some("none");
    let mut stream_failure: Option<AIError> = None;
    let mut stream_interrupted = false;
    loop {
        let due = publisher.lock().unwrap().next_due();
        let next = tokio::select! {
            biased;
            _ = input.cancel.cancelled() => {
                stream_interrupted = true;
                break;
            }
            _ = async { match due { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => {
                publisher.lock().unwrap().publish_due(Instant::now());
                continue;
            }
            next = receiver.recv() => next,
        };
        let event = match next {
            None => break,
            Some(Err(error)) => {
                stream_failure = Some(error);
                break;
            }
            Some(Ok(event)) => event,
        };
        let call = {
            let mut p = publisher.lock().unwrap();
            if p.record().provider_failed {
                continue;
            }
            p.event(event)
        };
        let Some((id, name, call_input)) = call else { continue };
        let publisher = publisher.clone();
        let snapshot = input.tools.clone();
        let context_publisher = publisher.clone();
        let call_id = id.clone();
        let context = tool::Context {
            app: app.clone(),
            location: input.located.clone(),
            session_id: input.session.clone(),
            agent: input.agent.clone(),
            message_id: input.assistant.clone(),
            call_id: id.clone(),
            directory: input.directory.clone(),
            progress: Arc::new(move |metadata| context_publisher.lock().unwrap().progress(&call_id, metadata)),
        };
        tools.spawn(async move {
            let result = if tools_disabled {
                Err(tool::Failure::Error(tool::ToolError::new("Tools are disabled after the maximum agent steps")))
            } else {
                snapshot.execute(&name, context, call_input).await
            };
            let mut p = publisher.lock().unwrap();
            match result {
                Ok(output) => {
                    p.tool_execution(&id, tool::output::truncate(output));
                    ToolExit::Done
                }
                Err(tool::Failure::Error(failure)) => {
                    p.fail_tool(&id, error(failure.kind, &failure.message), failure.metadata);
                    ToolExit::Done
                }
                Err(declined @ tool::Failure::Declined { .. }) => ToolExit::Declined(id, declined),
            }
        });
    }
    streaming.abort();
    drop(receiver);
    publisher.lock().unwrap().flush();
    if publisher.lock().unwrap().started() {
        publisher.lock().unwrap().streamed();
    }
    // Interrupted: the tools stop with the stream.
    let mut tools_interrupted = false;
    let mut declines: Vec<(String, tool::Failure)> = Vec::new();
    if stream_interrupted {
        tools.abort_all();
    }
    loop {
        let joined = tokio::select! {
            biased;
            _ = input.cancel.cancelled(), if !stream_interrupted && !tools_interrupted => {
                tools.abort_all();
                tools_interrupted = true;
                continue;
            }
            joined = tools.join_next() => joined,
        };
        match joined {
            None => break,
            Some(Ok(ToolExit::Done)) => {}
            Some(Ok(ToolExit::Declined(id, failure))) => declines.push((id, failure)),
            Some(Err(_)) => tools_interrupted = true,
        }
    }
    let mut p = publisher.lock().unwrap();
    // TODO(port): context overflow recovery by compaction before output (`recoverOverflow`).
    let recorded = p.record();
    let unknown_finish = stream_failure.is_none()
        && !stream_interrupted
        && recorded.finish.as_ref().is_some_and(|f| f.finish == "unknown");
    let llm_failure = stream_failure.clone().or_else(|| {
        unknown_finish.then(|| AIError::incomplete("The provider response ended with an unknown finish reason."))
    });
    let llm_error = llm_failure.as_ref().filter(|_| !recorded.provider_failed).map(AIError::session_error);
    // TODO(port): `RecoverFull` for WebSocket transports (`retry-full` recovery).
    let _ = input.recover_continuation;
    let decision = match (&llm_failure, &llm_error) {
        (Some(failure), Some(_)) if !failure.context_overflow() => {
            let proposed = failure.retryable() || (recorded.output_started && interrupted_stream(failure));
            retry.decide(failure, proposed)
        }
        _ => None,
    };
    if let (Some(_), Some(error), Some(decision)) = (&llm_failure, &llm_error, &decision) {
        if decision.retry && !recorded.output_started {
            p.start_assistant();
            return Ok(Outcome::Retry { error: error.clone(), decision: decision.clone() });
        }
    }
    if let Some(error) = &llm_error {
        p.fail_assistant(error.clone());
    }
    for (id, decline) in &declines {
        let message = match decline {
            _ if input.located.permission.is_closed() => "Interaction cancelled because the location shut down".to_owned(),
            tool::Failure::Declined { question: true, message } => message.clone(),
            _ => "The user declined this tool call".to_owned(),
        };
        p.fail_tool(id, error("aborted", &message), None);
    }
    let interrupted = !declines.is_empty() || stream_interrupted || tools_interrupted;
    let tools_failure = if interrupted || recorded.provider_failed { Some(error("aborted", "Tool execution interrupted")) } else { None };
    if let Some(failure) = &tools_failure {
        p.fail_unsettled_tools(failure, "all");
    }
    if interrupted {
        p.fail_assistant(error("aborted", "Step interrupted"));
    }
    if llm_error.is_some() || (stream_failure.is_none() && !stream_interrupted && !recorded.provider_failed) {
        let missing = p.fail_unsettled_tools(&error("tool.result-missing", "Provider did not return a tool result"), "hosted");
        if missing && llm_error.is_none() && recorded.finish.is_none() {
            p.fail_assistant(error("tool.result-missing", "Provider did not return a tool result"));
        }
        p.fail_incomplete_tools();
    }
    let record = p.record();
    if record.finish.is_some() || record.failure.is_some() {
        // TODO(port): the end snapshot and changed files.
        let usage = record.finish.as_ref().map(|f| usage(f.usage.as_ref(), &input.model.cost));
        if record.failure.is_some() {
            p.publish_step_failure(usage.clone());
        }
        if let (Some(settlement), Some((cost, tokens)), None) = (&record.finish, &usage, &record.failure) {
            let assistant = p.start_assistant();
            let mut o = Obj::new();
            o.str("sessionID", &input.session).str("assistantMessageID", &assistant);
            o.str("finish", &settlement.finish);
            o.opt_str("rawFinish", settlement.raw.as_deref());
            o.opt("native", settlement.native.clone());
            o.set("cost", cost.clone()).set("tokens", tokens.clone());
            p.publish("session.step.ended", o.done());
        }
    }
    drop(p);
    if let (Some(_), Some(error), Some(decision)) = (&llm_failure, &llm_error, &decision) {
        if decision.retry && record.output_started && declines.is_empty() && !tools_interrupted {
            return Ok(Outcome::Continue { error: error.clone(), decision: decision.clone() });
        }
    }
    if stream_interrupted {
        return Err(Stop::Interrupted);
    }
    if let Some(failure) = stream_failure {
        return Err(Stop::Failed(failure.session_error()));
    }
    if !declines.is_empty() || tools_interrupted {
        return Err(Stop::Interrupted);
    }
    if let Some(failure) = record.failure {
        return Err(Stop::Failed(failure));
    }
    let needs_continuation = !tools_disabled && record.needs_continuation;
    if record.finish.as_ref().is_some_and(|f| f.finish == "length") {
        return Ok(Outcome::OutputLimit { needs_continuation });
    }
    Ok(Outcome::Completed { needs_continuation })
}
