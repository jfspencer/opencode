//! The server's events folded into ACP outputs (packages/cli/src/acp/translate.ts, child.ts, compaction.ts): the
//! root session's updates, its children's (as `opencode/session/child_update` notifications for clients that ask
//! for them, or projected into the root session's updates), permission and form asks, retries, compactions,
//! usage, and the turn's terminal state.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use opencode_protocol::Json;

use super::tool;
use crate::json::{self, Obj, get, get_num, get_str};

pub const CHILD_UPDATE_METHOD: &str = "opencode/session/child_update";
const RETRY_META: &str = "opencode/retry";
const COMPACTION_META: &str = "opencode/compaction";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Terminal {
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Clone)]
pub struct Context {
    pub session_id: String,
    pub cwd: String,
    pub start: String,
    pub child_updates: bool,
    pub compaction: bool,
}

#[derive(Clone, Debug)]
pub struct Tool {
    pub session_id: String,
    pub id: String,
    pub name: String,
    pub input: Json,
    pub metadata: Json,
}

#[derive(Clone, Debug)]
pub struct Child {
    pub id: String,
    pub parent_id: String,
    pub depth: i64,
    pub title: Option<String>,
}

#[derive(Clone, Debug)]
struct Started {
    message_id: String,
    reason: String,
}

#[derive(Clone, Default)]
pub struct State {
    pub started: bool,
    tools: IndexMap<String, Tool>,
    retries: HashMap<String, Json>,
    compactions: HashMap<String, Started>,
    children: HashMap<String, Child>,
    pub open_children: HashSet<String>,
    asks: HashSet<String>,
    finish: Option<String>,
    /// The turn's tokens and the last step's.
    pub usage: Option<(Json, Json)>,
    step_error: Option<Json>,
    execution_error: Option<Json>,
}

/// Where a completed tool's diffs come from (`DiffSource`).
#[derive(Clone)]
pub struct DiffSource {
    pub tool_name: String,
    pub input: Json,
    pub metadata: Option<Json>,
}

pub enum Output {
    SessionUpdate { update: Json, diff: Option<DiffSource> },
    ChildUpdate { update: Json, diff: Option<DiffSource> },
    PermissionAsk { event: Json, tool: Option<Tool>, child: Option<Child> },
    FormAsk { form: Json, child: Option<Child> },
    AskSettled { id: String },
}

pub struct Folded {
    pub outputs: Vec<Output>,
    pub terminal: Option<Terminal>,
}

fn folded(outputs: Vec<Output>) -> Folded {
    Folded { outputs, terminal: None }
}

fn tool_key(session: &str, id: &str) -> String {
    format!("{session}:{id}")
}

/// `sessionIDFromEvent`.
fn event_session(kind: &str, data: &Json) -> Option<String> {
    if let Some(id) = get_str(data, "sessionID") {
        return Some(id.to_owned());
    }
    if kind == "form.created" {
        return get(data, "form").and_then(|f| get_str(f, "sessionID")).map(String::from);
    }
    None
}

/// `fromTrackedChild`.
pub fn from_tracked_child(state: &State, event: &Json) -> bool {
    let kind = get_str(event, "type").unwrap_or("");
    let data = get(event, "data").cloned().unwrap_or(Json::Null);
    let session = if kind == "session.created" { get_str(&data, "parentID").map(String::from) } else { event_session(kind, &data) };
    session.is_some_and(|s| state.children.contains_key(&s))
}

/// `ACPTranslate.fold`: updates `state` and answers what the event becomes.
pub fn fold(state: &mut State, event: &Json, ctx: &Context) -> Folded {
    let kind = get_str(event, "type").unwrap_or("");
    let event_id = get_str(event, "id").unwrap_or("");
    let data = get(event, "data").cloned().unwrap_or(Json::Null);
    if kind == "session.created" {
        return child_created(state, &data, ctx);
    }
    let session = event_session(kind, &data);
    let child = session.as_ref().and_then(|s| state.children.get(s)).cloned();
    if kind == "permission.asked" && (get_str(&data, "sessionID") == Some(ctx.session_id.as_str()) || child.is_some()) {
        let tool = get(&data, "source")
            .and_then(|s| get_str(s, "id"))
            .filter(|id| !id.is_empty())
            .and_then(|id| state.tools.get(&tool_key(get_str(&data, "sessionID").unwrap_or(""), id)))
            .cloned();
        state.asks.insert(get_str(&data, "id").unwrap_or("").to_owned());
        return folded(vec![Output::PermissionAsk { event: event.clone(), tool, child }]);
    }
    if kind == "form.created" {
        let form = get(&data, "form").cloned().unwrap_or(Json::Null);
        if get_str(&form, "sessionID") == Some(ctx.session_id.as_str()) || child.is_some() {
            state.asks.insert(get_str(&form, "id").unwrap_or("").to_owned());
            return folded(vec![Output::FormAsk { form, child }]);
        }
    }
    let settled = match kind {
        "permission.replied" => get_str(&data, "requestID"),
        "form.replied" | "form.cancelled" => get_str(&data, "id"),
        _ => None,
    };
    if let Some(id) = settled.filter(|id| !id.is_empty() && state.asks.contains(*id)) {
        state.asks.remove(id);
        return folded(vec![Output::AskSettled { id: id.to_owned() }]);
    }
    let Some(session) = session else { return folded(vec![]) };
    if session != ctx.session_id && child.is_none() {
        return folded(vec![]);
    }
    if kind == "session.inbox.delivered" && get_str(&data, "inboxID") == Some(ctx.start.as_str()) {
        state.started = true;
        return folded(vec![]);
    }
    if !state.started {
        return folded(vec![]);
    }
    match child {
        Some(child) => child_event(state, kind, event_id, &data, ctx, &child),
        None => root_event(state, kind, event_id, &data, ctx),
    }
}

fn child_created(state: &mut State, data: &Json, ctx: &Context) -> Folded {
    let Some(parent_id) = get_str(data, "parentID") else { return folded(vec![]) };
    let parent = if parent_id == ctx.session_id { None } else { state.children.get(parent_id).cloned() };
    if parent.is_none() && parent_id != ctx.session_id {
        return folded(vec![]);
    }
    let child = Child {
        id: get_str(data, "sessionID").unwrap_or("").to_owned(),
        parent_id: parent_id.to_owned(),
        depth: parent.map_or(1, |p| p.depth + 1),
        title: get_str(data, "title").map(String::from),
    };
    state.children.insert(child.id.clone(), child.clone());
    state.open_children.insert(child.id.clone());
    folded(child_status(ctx, &child, status("created", None)))
}

fn root_event(state: &mut State, kind: &str, event_id: &str, data: &Json, ctx: &Context) -> Folded {
    match kind {
        "session.step.started" => {
            state.step_error = None;
            session_event(state, kind, event_id, data, ctx, None)
        }
        "session.step.ended" => {
            if let Some(tokens) = get(data, "tokens") {
                record_step(state, tokens);
            }
            state.finish = get_str(data, "finish").map(String::from);
            folded(vec![])
        }
        "session.step.failed" => {
            if let Some(tokens) = get(data, "tokens") {
                record_step(state, tokens);
            }
            state.step_error = get(data, "error").cloned();
            folded(vec![])
        }
        "session.execution.succeeded" => Folded { outputs: vec![], terminal: Some(Terminal::Succeeded) },
        "session.execution.interrupted" => Folded { outputs: vec![], terminal: Some(Terminal::Interrupted) },
        "session.execution.failed" => {
            state.execution_error = get(data, "error").cloned();
            Folded { outputs: vec![], terminal: Some(Terminal::Failed) }
        }
        _ => session_event(state, kind, event_id, data, ctx, None),
    }
}

fn child_event(state: &mut State, kind: &str, event_id: &str, data: &Json, ctx: &Context, child: &Child) -> Folded {
    match kind {
        "session.execution.started" => folded(child_status(ctx, child, status("running", None))),
        "session.execution.succeeded" => child_ended(state, ctx, child, status("completed", None)),
        "session.execution.interrupted" => child_ended(state, ctx, child, status("interrupted", None)),
        "session.execution.failed" => child_ended(state, ctx, child, status("failed", get(data, "error").cloned())),
        _ => session_event(state, kind, event_id, data, ctx, Some(child)),
    }
}

fn session_event(state: &mut State, kind: &str, event_id: &str, data: &Json, ctx: &Context, child: Option<&Child>) -> Folded {
    let session = child.map_or(ctx.session_id.clone(), |c| c.id.clone());
    let send = |update: Json| route(ctx, child, update);
    let standard = uses_standard_updates(ctx, child.is_some());
    let id = get_str(data, "id").unwrap_or("").to_owned();
    let data_session = get_str(data, "sessionID").unwrap_or("").to_owned();
    match kind {
        "session.step.started" => {
            if state.retries.remove(&session).is_none() {
                return folded(vec![]);
            }
            let mut meta = Obj::new();
            meta.set(RETRY_META, Json::Null);
            let mut update = Obj::new();
            update.str("sessionUpdate", "session_info_update").set("_meta", meta.done());
            folded(send(update.done()))
        }
        "session.retry.scheduled" => {
            let mut retry = Obj::new();
            retry
                .opt("attempt", get(data, "attempt").cloned())
                .str("nextRetryAt", &super::iso(get_num(data, "at").unwrap_or(0.0)))
                .opt("error", get(data, "error").cloned());
            let retry = retry.done();
            state.retries.insert(session, retry.clone());
            let mut meta = Obj::new();
            meta.set(RETRY_META, retry);
            let mut update = Obj::new();
            update.str("sessionUpdate", "session_info_update").set("_meta", meta.done());
            folded(send(update.done()))
        }
        "session.compaction.started" | "session.compaction.ended" | "session.compaction.failed" => {
            let updates = compaction_apply(kind, event_id, data, &mut state.compactions, standard);
            folded(updates.into_iter().flat_map(send).collect())
        }
        "session.compaction.delta" => {
            let Some(started) = state.compactions.get(&session).filter(|_| standard) else { return folded(vec![]) };
            let mut content = Obj::new();
            content.str("type", "text").str("text", get_str(data, "text").unwrap_or(""));
            let mut update = Obj::new();
            update.str("sessionUpdate", "compaction_summary_chunk").str("compactionId", &started.message_id).set("content", content.done());
            folded(send(update.done()))
        }
        "session.text.delta" | "session.reasoning.delta" => {
            let message = get_str(data, "assistantMessageID").unwrap_or("");
            let text = kind == "session.text.delta";
            let message_id = if text { message.to_owned() } else { reasoning_message_id(message, get(data, "ordinal").unwrap_or(&Json::Number(0.0))) };
            let mut content = Obj::new();
            content.str("type", "text").str("text", get_str(data, "delta").unwrap_or(""));
            let mut update = Obj::new();
            update
                .str("sessionUpdate", if text { "agent_message_chunk" } else { "agent_thought_chunk" })
                .str("messageId", &message_id)
                .set("content", content.done());
            folded(send(update.done()))
        }
        "session.tool.input.started" => {
            let name = get_str(data, "name").unwrap_or("tool").to_owned();
            state.tools.insert(tool_key(&data_session, &id), new_tool(&data_session, &id, &name));
            let mut update = tool::pending(&id, &name, &json::object(), None, &ctx.cwd);
            update.str("sessionUpdate", "tool_call");
            folded(send(update.done()))
        }
        "session.tool.called" => {
            let key = tool_key(&data_session, &id);
            let mut tool = state.tools.get(&key).cloned().unwrap_or_else(|| new_tool(&data_session, &id, "tool"));
            tool.input = get(data, "input").cloned().unwrap_or_else(json::object);
            let mut update = tool::running(&id, &tool.name, &tool.input, &ctx.cwd);
            update.str("sessionUpdate", "tool_call_update");
            state.tools.insert(key, tool);
            folded(send(update.done()))
        }
        "session.tool.progress" => {
            let key = tool_key(&data_session, &id);
            let Some(current) = state.tools.get_mut(&key) else { return folded(vec![]) };
            current.metadata = get(data, "metadata").cloned().unwrap_or_else(json::object);
            let mut update = tool::running(&id, &current.name, &current.input, &ctx.cwd);
            update.str("sessionUpdate", "tool_call_update");
            folded(send(update.done()))
        }
        "session.tool.success" => {
            let key = tool_key(&data_session, &id);
            let tool = state.tools.shift_remove(&key).unwrap_or_else(|| new_tool(&data_session, &id, "tool"));
            let content = get(data, "content").and_then(json::array).unwrap_or(&[]);
            let metadata = get(data, "metadata");
            let mut update = tool::completed(&id, &tool.name, &tool.input, content, metadata, &ctx.cwd);
            update.str("sessionUpdate", "tool_call_update");
            let diff = DiffSource { tool_name: tool.name.clone(), input: tool.input.clone(), metadata: metadata.cloned() };
            folded(
                send(update.done())
                    .into_iter()
                    .map(|output| match output {
                        Output::SessionUpdate { update, .. } => Output::SessionUpdate { update, diff: Some(diff.clone()) },
                        Output::ChildUpdate { update, .. } => Output::ChildUpdate { update, diff: Some(diff.clone()) },
                        other => other,
                    })
                    .collect(),
            )
        }
        "session.tool.failed" => {
            let key = tool_key(&data_session, &id);
            let tool = state.tools.shift_remove(&key).unwrap_or_else(|| new_tool(&data_session, &id, "tool"));
            let content = get(data, "content").and_then(json::array).unwrap_or(&[]);
            let metadata = get(data, "metadata").cloned().unwrap_or_else(|| tool.metadata.clone());
            let error = get(data, "error").and_then(|e| get_str(e, "message")).unwrap_or("");
            let mut update = tool::failed(&id, &tool.name, &tool.input, content, Some(&metadata), error, &ctx.cwd);
            update.str("sessionUpdate", "tool_call_update");
            folded(send(update.done()))
        }
        _ => folded(vec![]),
    }
}

fn new_tool(session: &str, id: &str, name: &str) -> Tool {
    Tool { session_id: session.to_owned(), id: id.to_owned(), name: name.to_owned(), input: json::object(), metadata: json::object() }
}

fn record_step(state: &mut State, tokens: &Json) {
    let turn = match &state.usage {
        Some((turn, _)) => add_tokens(turn, tokens),
        None => tokens.clone(),
    };
    state.usage = Some((turn, tokens.clone()));
}

pub fn reasoning_message_id(message: &str, ordinal: &Json) -> String {
    format!("{message}:reasoning:{}", json::text(ordinal))
}

fn status(status: &str, error: Option<Json>) -> Json {
    let mut o = Obj::new();
    o.str("type", "status").str("status", status).opt("error", error);
    o.done()
}

/// `ACPChild.update`.
fn child_update(root: &str, child: &Child, event: Json) -> Json {
    let mut o = Obj::new();
    o.str("rootSessionId", root).str("childSessionId", &child.id).str("parentSessionId", &child.parent_id).int("depth", child.depth);
    if let Some(title) = child.title.as_deref().filter(|t| !t.is_empty()) {
        o.str("title", title);
    }
    if let Json::Object(fields) = event {
        for (key, value) in fields {
            o.set(&key, value);
        }
    }
    o.done()
}

/// `ACPChild.meta`.
pub fn child_meta(child: &Child) -> Json {
    let mut inner = Obj::new();
    inner.str("id", &child.id).str("parentID", &child.parent_id).int("depth", child.depth);
    if let Some(title) = child.title.as_deref().filter(|t| !t.is_empty()) {
        inner.str("title", title);
    }
    let mut o = Obj::new();
    o.set("opencode/child-session", inner.done());
    o.done()
}

/// `ACPChild.toolCallID`.
pub fn child_tool_call_id(child: Option<&Child>, id: &str) -> String {
    match child {
        Some(child) => format!("{}:{id}", child.id),
        None => id.to_owned(),
    }
}

/// `ACPChild.prefixTitle`.
pub fn prefix_title(child: Option<&Child>, value: &str) -> String {
    match child.and_then(|c| c.title.as_deref()).filter(|t| !t.is_empty()) {
        Some(title) => format!("{title}: {value}"),
        None => value.to_owned(),
    }
}

/// `ACPChild.project`: a child's update marked with the child, its tool calls prefixed.
fn project(mut update: Json, child: &Child) -> Json {
    let mut meta = get(&update, "_meta").cloned().unwrap_or_else(json::object);
    if let Json::Object(fields) = child_meta(child) {
        for (key, value) in fields {
            json::set(&mut meta, &key, value);
        }
    }
    json::set(&mut update, "_meta", meta);
    if matches!(get_str(&update, "sessionUpdate"), Some("tool_call" | "tool_call_update")) {
        let id = get_str(&update, "toolCallId").unwrap_or("").to_owned();
        json::set(&mut update, "toolCallId", Json::String(child_tool_call_id(Some(child), &id)));
        if let Some(title) = get_str(&update, "title").filter(|t| !t.is_empty()).map(String::from) {
            json::set(&mut update, "title", Json::String(prefix_title(Some(child), &title)));
        }
    }
    update
}

fn route(ctx: &Context, child: Option<&Child>, update: Json) -> Vec<Output> {
    let Some(child) = child else { return vec![Output::SessionUpdate { update, diff: None }] };
    let projected = project(update, child);
    if ctx.child_updates {
        let mut event = Obj::new();
        event.str("type", "update").set("update", projected);
        return child_status(ctx, child, event.done());
    }
    vec![Output::SessionUpdate { update: projected, diff: None }]
}

fn child_status(ctx: &Context, child: &Child, event: Json) -> Vec<Output> {
    if !ctx.child_updates {
        return vec![];
    }
    vec![Output::ChildUpdate { update: child_update(&ctx.session_id, child, event), diff: None }]
}

fn child_ended(state: &mut State, ctx: &Context, child: &Child, event: Json) -> Folded {
    state.open_children.remove(&child.id);
    folded(child_status(ctx, child, event))
}

/// `ACPTranslate.abandon`: the tools still running when a cancelled turn stopped waiting fail as cancelled.
pub fn abandon(state: &mut State, ctx: &Context) -> Vec<Output> {
    let tools: Vec<Tool> = std::mem::take(&mut state.tools).into_values().collect();
    let mut outputs = vec![];
    for tool in tools {
        let child = state.children.get(&tool.session_id).cloned();
        let mut update = tool::failed(&tool.id, &tool.name, &tool.input, &[], Some(&tool.metadata), "Cancelled", &ctx.cwd);
        update.str("sessionUpdate", "tool_call_update");
        outputs.extend(route(ctx, child.as_ref(), update.done()));
    }
    if let Some(started) = state.compactions.remove(&ctx.session_id) {
        let mut error = Obj::new();
        error.str("type", "aborted").str("message", "Compaction cancelled");
        let update = compaction_update(&started, "failed", None, Some(&error.done()), uses_standard_updates(ctx, false));
        outputs.extend(route(ctx, None, update));
    }
    outputs
}

/// `ACPTranslate.failure`: the error a finished turn answers with, if any.
pub fn failure(state: &State) -> Option<super::RpcError> {
    let error = state.step_error.as_ref().or(state.execution_error.as_ref())?;
    let kind = get_str(error, "type").unwrap_or("");
    if kind == "provider.auth" {
        return Some(super::RpcError { code: -32000, message: "Authentication required: provider authentication required".into(), data: Some(json::object()) });
    }
    if kind == "aborted" || kind == "provider.content-filter" {
        return None;
    }
    let message = get_str(error, "message").filter(|m| !m.is_empty()).unwrap_or("OpenCode prompt failed");
    Some(super::RpcError::service(message, Some("session"), Some(kind)))
}

pub fn total(tokens: &Json) -> f64 {
    let cache = get(tokens, "cache");
    ["input", "output", "reasoning"].iter().map(|k| get_num(tokens, k).unwrap_or(0.0)).sum::<f64>()
        + cache.map_or(0.0, |c| get_num(c, "read").unwrap_or(0.0) + get_num(c, "write").unwrap_or(0.0))
}

fn add_tokens(a: &Json, b: &Json) -> Json {
    let n = |v: &Json, k: &str| get_num(v, k).unwrap_or(0.0);
    let c = |v: &Json, k: &str| get(v, "cache").and_then(|c| get_num(c, k)).unwrap_or(0.0);
    let mut cache = Obj::new();
    cache.set("read", Json::Number(c(a, "read") + c(b, "read"))).set("write", Json::Number(c(a, "write") + c(b, "write")));
    let mut o = Obj::new();
    o.set("input", Json::Number(n(a, "input") + n(b, "input")))
        .set("output", Json::Number(n(a, "output") + n(b, "output")))
        .set("reasoning", Json::Number(n(a, "reasoning") + n(b, "reasoning")))
        .set("cache", cache.done());
    o.done()
}

/// `ACPTranslate.response`.
pub fn response(state: &State, session: &str, terminal: Terminal) -> Json {
    let error = state.step_error.as_ref().or(state.execution_error.as_ref()).and_then(|e| get_str(e, "type"));
    let stop = if terminal == Terminal::Interrupted || error == Some("aborted") {
        "cancelled"
    } else if state.finish.as_deref() == Some("length") {
        "max_tokens"
    } else if state.finish.as_deref() == Some("content-filter") || error == Some("provider.content-filter") {
        "refusal"
    } else {
        "end_turn"
    };
    let mut o = Obj::new();
    o.str("stopReason", stop);
    if let Some((tokens, _)) = &state.usage {
        let n = |k: &str| get_num(tokens, k).unwrap_or(0.0);
        let c = |k: &str| get(tokens, "cache").and_then(|c| get_num(c, k)).unwrap_or(0.0);
        let mut usage = Obj::new();
        usage.set("inputTokens", Json::Number(n("input"))).set("outputTokens", Json::Number(n("output"))).set("totalTokens", Json::Number(total(tokens)));
        if n("reasoning") > 0.0 {
            usage.set("thoughtTokens", Json::Number(n("reasoning")));
        }
        if c("read") > 0.0 {
            usage.set("cachedReadTokens", Json::Number(c("read")));
        }
        if c("write") > 0.0 {
            usage.set("cachedWriteTokens", Json::Number(c("write")));
        }
        o.set("usage", usage.done());
    }
    let mut meta = Obj::new();
    if let Some(retry) = state.retries.get(session) {
        meta.set(RETRY_META, retry.clone());
    }
    o.set("_meta", meta.done());
    o.done()
}

/// `ACPCompaction.usesStandardUpdates`: without child updates, a child's standard update would read as the parent's.
fn uses_standard_updates(ctx: &Context, child: bool) -> bool {
    ctx.compaction && (!child || ctx.child_updates)
}

/// `ACPCompaction.apply`.
fn compaction_apply(kind: &str, event_id: &str, data: &Json, tracked: &mut HashMap<String, Started>, standard: bool) -> Vec<Json> {
    let session = get_str(data, "sessionID").unwrap_or("").to_owned();
    let reason = get_str(data, "reason").unwrap_or("").to_owned();
    if kind == "session.compaction.started" {
        let started = open(event_id, data, &reason);
        tracked.insert(session, started.clone());
        return vec![compaction_update(&started, "started", None, None, standard)];
    }
    let current = tracked.remove(&session);
    if kind == "session.compaction.ended" {
        let Some(current) = current else { return vec![] };
        let completed = Started { message_id: current.message_id, reason };
        return vec![compaction_update(&completed, "completed", get_str(data, "text"), None, standard)];
    }
    let opened = current.is_none();
    let started = current.unwrap_or_else(|| open(event_id, data, &reason));
    let failed = Started { message_id: started.message_id.clone(), reason };
    let mut updates = vec![];
    if opened && standard {
        updates.push(compaction_update(&started, "started", None, None, standard));
    }
    updates.push(compaction_update(&failed, "failed", None, get(data, "error"), standard));
    updates
}

/// `open`: core's compaction message ID, so live and replayed compactions line up.
fn open(event_id: &str, data: &Json, reason: &str) -> Started {
    let message_id = match get_str(data, "inputID") {
        Some(id) => id.to_owned(),
        None => event_id.strip_prefix("evt_").map(|rest| format!("msg_{rest}")).unwrap_or_else(|| event_id.to_owned()),
    };
    Started { message_id, reason: reason.to_owned() }
}

/// `ACPCompaction.replay`: a finished compaction message (none for a running one).
pub fn compaction_replay(message: &Json, standard: bool) -> Option<Json> {
    let status = get_str(message, "status").unwrap_or("");
    if status == "running" {
        return None;
    }
    let base = Started { message_id: get_str(message, "id").unwrap_or("").to_owned(), reason: get_str(message, "reason").unwrap_or("").to_owned() };
    Some(if status == "failed" {
        compaction_update(&base, "failed", None, get(message, "error"), standard)
    } else {
        compaction_update(&base, "completed", get_str(message, "summary"), None, standard)
    })
}

fn compaction_update(compaction: &Started, status: &str, summary: Option<&str>, error: Option<&Json>, standard: bool) -> Json {
    let mut o = Obj::new();
    if !standard {
        let mut marker = Obj::new();
        marker.str("status", status).str("messageId", &compaction.message_id).str("reason", &compaction.reason);
        if status == "failed" {
            marker.opt("error", error.cloned());
        }
        let mut meta = Obj::new();
        meta.set(COMPACTION_META, marker.done());
        o.str("sessionUpdate", "session_info_update").set("_meta", meta.done());
        return o.done();
    }
    o.str("sessionUpdate", "compaction_update").str("compactionId", &compaction.message_id);
    match status {
        "started" => {
            o.str("status", "in_progress");
        }
        "completed" => {
            let summary = match summary.filter(|s| !s.is_empty()) {
                Some(text) => {
                    let mut part = Obj::new();
                    part.str("type", "text").str("text", text);
                    Json::Array(vec![part.done()])
                }
                None => Json::Null,
            };
            o.str("status", "completed").set("summary", summary);
        }
        _ => {
            let error = error.cloned().unwrap_or(Json::Null);
            let kind = get_str(&error, "type").unwrap_or("");
            if kind == "aborted" || kind == "compaction.interrupted" {
                o.str("status", "cancelled").set("summary", Json::Null);
            } else {
                let message = get_str(&error, "message").unwrap_or("");
                let shown = if message.is_empty() || (kind == "compaction.failed" && message.contains('\n')) { "Compaction failed" } else { message };
                o.str("status", "failed").set("summary", Json::Null).str("error", shown);
            }
        }
    }
    o.done()
}
