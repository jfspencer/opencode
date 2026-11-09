//! Compaction (packages/core/src/session/compaction.ts, the compaction parts of session/runner/llm.ts,
//! runner/step.ts, history.ts, recent-text.ts and config/plugin/compaction.ts): a summary of the conversation replaces
//! the history before it, so later requests open with a checkpoint (the summary, plus the newest messages kept
//! verbatim as text) instead of the whole transcript.
//!
//! A compaction is automatic (before a step, once the prompt reaches the threshold below the model's window), an
//! overflow recovery (the provider rejected the step's request as too long, before any output), or manual (a
//! `compaction` inbox item, delivered at a step boundary). The summary request is the step's own request (system,
//! history and tools unchanged, so it reuses the prompt cache) followed by the summary prompt; a request that cannot
//! fit, or that the provider rejects as too long, is resent with the history as one text transcript, shortened oldest
//! first. A reply that skips the summary template gets one nudge.
//!
//! Its record is the `compaction` message: `session.compaction.started` opens it (running), `ended` completes it with
//! the summary (`metadata.keepID` names the first message kept verbatim; history rebuilds that text) and advances the
//! instruction epoch, `failed` records the error. Every summary call is billed as it finishes
//! (`session.usage.recorded`, source `compaction`); the message shows the total.
//!
//! With `experimental.background_compaction`, a summary of everything before the latest response is written in the
//! background once the prompt nears the threshold, and automatic compaction swaps it in when it still fits.
//!
//! TODO(port): provider-native compaction (no route in scope supports it: such a policy fails model resolution, as
//! `UnsupportedCompactionError` does), the `compaction` plugin hook (a hook-supplied summary), image bounding of the
//! summary request (`boundImages`), the model's `cache.ttl` (the default 5-minute prompt cache is assumed).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use opencode_protocol::Json;
use opencode_protocol::types as t;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use tokio::sync::{mpsc, watch};

use crate::app::{App, fatal};
use crate::bus::{Event, LocationRef};
use crate::inventory::Inventory;
use crate::json::{self, Obj};
use crate::llm::{self, AIError, AssistantPart, Message, ToolResult, ToolResultValue, UserPart};
use crate::session::execution::Cancel;
use crate::session::runner::Loaded;
use crate::session::step::Stop;
use crate::session::{context, instructions, message};

/// After each "too long" rejection, the next attempt aims at this share of the first rejected request's size.
const SHRINK_STEPS: [f64; 4] = [0.85, 0.7, 0.5, 0.35];
/// The least of the window kept free for the last reply before compaction and for the summary itself.
const RESERVE_MIN: f64 = 16_000.0;
/// Share of the window kept free before automatic compaction.
const RESERVE_SHARE: f64 = 0.15;
/// A common window size, assumed for the compaction request when the model's window is unknown.
const UNKNOWN_WINDOW: f64 = 200_000.0;
/// A summary gets at least this much output, or the room left if less.
const SUMMARY_OUTPUT_FLOOR: f64 = 32_000.0;
/// How far one exchange of a summary transcript is shortened: whole; tool outputs cut; tool outputs left out and tool
/// inputs cut; only the user's text, cut; omitted.
const OUTPUTS_CLIPPED: usize = 1;
const OUTPUTS_REMOVED: usize = 2;
const USER_ONLY: usize = 3;
const OMITTED: usize = 4;
const LEVELS: usize = 5;
/// Characters kept, from the start and end, of a cut tool input, tool output, or user message.
const TRANSCRIPT_CLIP: usize = 2_000;
const IMAGE_TOKEN_ESTIMATE: f64 = 1_500.0;
const PDF_TOKEN_ESTIMATE: f64 = 2_000.0;
/// Tokens before the ceiling at which a background summary starts.
const PRECOMPUTE_LEAD: f64 = 60_000.0;
/// Prompt caches expire this long after their last use.
const CACHE_LIFETIME: f64 = 5.0 * 60_000.0;
/// A background summary starts only while this much of the latest response's cache lifetime is left.
const CACHE_MARGIN: f64 = 30_000.0;
/// The most text a swapped-in summary keeps verbatim beside it, as a share of the budget.
const PRECOMPUTED_TAIL_SHARE: f64 = 0.4;
/// Failed background summaries after which no new one starts until the next compaction.
const PRECOMPUTE_MAX_FAILURES: u32 = 2;
/// Characters of a tool's output the recent text keeps.
const TOOL_OUTPUT_MAX_CHARS: usize = 1_250;

const SUMMARY_TEMPLATE: &str = "You MUST use this format for your response (you may omit sections that aren't applicable). Do not include the <template> tags in your response.
<template>
## Objective
- [one or two brief sentences describing what the user is trying to accomplish]

## Requirements
- [constraints, preferences, requirements, and scope boundaries stated by the user, or \"(none)\"]

## Decisions
- [decisions already made and why, or \"(none)\"]

## Work State
Break the objective into smaller goals and report which are completed, which are being worked on, and which are blocked.
### Completed
- [goals that have been completed; otherwise \"(none)\"]

### Active
- [goals currently being worked on; otherwise \"(none)\"]

### Blocked
- [anything blocking progress, and why; otherwise \"(none)\"]

## Next Move
1. [ordered list of next actions, or \"(none)\"]

## Relevant Files
List the files and directories, other than the current working directory, that another agent would need to open to continue this work. Include at most 15, most important first. Do not list every file that was read or changed. Include paths outside the current working directory when relevant. If none, write \"(none)\".
- `[file or directory path]`: [brief reason it matters]

## Important Context
- [facts the next agent cannot continue without and cannot easily find on its own; or \"(none)\"]
</template>";

const SUMMARY_RULES: &str = "Rules:
- Keep each section concise. Use terse, single-line bullets, not prose paragraphs or nested lists.
- Prefer short references over detailed restatement. It is fine to leave out information the next agent can recover from the code or the files listed above.
- Preserve exact file paths, symbols, commands, error strings, URLs, and identifiers.
- Carry forward only user questions or requests that remain unanswered or require further action. Do not repeat ones that newer history has answered or resolved. Preserve exact wording when carrying one forward.
- Preserve consequential workflow state, including whether changes are uncommitted, committed, pushed, under review, or merged.
- Do not mention the summary process or that context was compacted.";

const NUDGE: &str = "The previous response did not fill in the required summary template. Do not call tools. Return the summary as text using the exact section headings from the template.";

/// Summaries written with the previous template carry this catch-all heading.
const LEGACY_HEADING: &str = "## Additional Context";

/// `buildPrompt`: the summary prompt, or the prompt that updates an existing checkpoint.
pub fn build_prompt(update: bool, legacy: bool) -> String {
    let shared = [
        "Summarize only what the user and the assistant said and did. Leave out instructions and setup the assistant was given rather than told by the user: repository conventions, instruction files such as AGENTS.md, and environment details like the session ID. The next agent receives current versions of all of these separately.",
        SUMMARY_TEMPLATE,
        SUMMARY_RULES,
        "Do not continue the task or call tools.",
        "Return only the structured summary in the requested format. Do not include a preamble, explanation, or other commentary.",
    ];
    let mut parts: Vec<&str> = Vec::new();
    if !update {
        parts.push("You MUST summarize the conversation above into a structured summary that will be given to another agent to resume the work.");
        parts.extend(shared);
        return parts.join("\n\n");
    }
    parts.push("Update the existing checkpoint in the conversation above into one consolidated summary.");
    if legacy {
        parts.push("The existing checkpoint was written with an earlier format that recorded far more detail than this one asks for. Rewrite it at the level of detail described below rather than carrying its detail forward. Keep its requirements, decisions, and open questions; they came from earlier conversation with the user.");
    }
    parts.push("Newer history always takes precedence over the existing checkpoint. Preserve previous information unless newer history clearly contradicts, supersedes, resolves, or makes it stale. If something is no longer relevant to continuing the work, you may remove it.");
    parts.push("Incorporate newer requirements, decisions, progress, and context. Reconcile Work State and Next Move: move completed work out of Active, remove resolved blockers and answered questions, and preserve unresolved or pending work.");
    parts.push("Return only the updated Markdown sections. Do not reproduce the `<conversation-checkpoint>`, `<summary>`, or `<recent-context>` wrapper tags from the previous checkpoint.");
    parts.extend(shared);
    parts.join("\n\n")
}

/// Whether a reply fills in the template: one of its lines is a template heading.
fn filled(text: &str) -> bool {
    let headings: Vec<&str> = SUMMARY_TEMPLATE.split('\n').filter(|line| line.starts_with("##")).collect();
    text.split('\n').any(|line| headings.contains(&line.trim()))
}

fn error(ty: &str, message: &str) -> Json {
    let mut o = Obj::new();
    o.str("type", ty).str("message", message);
    o.done()
}

/// A database failure, as the runner reports one.
fn defect(e: rusqlite::Error) -> Stop {
    let failure = fatal(e);
    Stop::Failed(error("unknown", json::get_str(&failure.body, "message").unwrap_or("Internal server error")))
}

/// `Token.estimate`: about 4 characters (UTF-16 units) per token.
pub fn tokens(text: &str) -> f64 {
    (text.encode_utf16().count() as f64 / 4.0 + 0.5).floor()
}

// --- Settings -------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Buffer {
    /// A share of the window, in percent.
    Share(f64),
    Tokens(f64),
}

/// `SessionCompaction.Settings`, from the config documents (`ConfigCompactionPlugin`).
#[derive(Debug, Clone, Copy)]
struct Settings {
    auto: bool,
    buffer: Option<Buffer>,
    keep: f64,
    precompute: bool,
}

fn settings(inventory: &Inventory) -> Settings {
    let mut settings = Settings { auto: true, buffer: None, keep: 15_000.0, precompute: background_flag(inventory) };
    for info in crate::inventory::config::documents(&inventory.entries) {
        let Some(compaction) = &info.compaction else { continue };
        if let Some(auto) = compaction.auto {
            settings.auto = auto;
        }
        if let Some(buffer) = &compaction.buffer {
            settings.buffer = Some(match buffer {
                t::ConfigCompactionBuffer::ConfigCompactionPercent(share) => {
                    Buffer::Share(share.0.trim_end_matches('%').parse().unwrap_or(f64::NAN))
                }
                t::ConfigCompactionBuffer::Int(tokens) => Buffer::Tokens(*tokens as f64),
            });
        }
        if let Some(tokens) = compaction.keep.as_ref().and_then(|keep| keep.tokens) {
            settings.keep = tokens as f64;
        }
    }
    settings
}

/// `Flag.resolve` of `background_compaction`: `OPENCODE_FLAGS`, then the latest `experimental` block, then
/// `experimental.all_flags`, then on for local and dev builds.
fn background_flag(inventory: &Inventory) -> bool {
    const KEY: &str = "background_compaction";
    let overrides = std::env::var("OPENCODE_FLAGS").unwrap_or_default();
    let mut env: HashMap<&str, bool> = HashMap::new();
    for item in overrides.split(',').map(str::trim).filter(|item| !item.is_empty()) {
        match item.strip_prefix('-') {
            Some(key) => env.insert(key, false),
            None => env.insert(item, true),
        };
    }
    if let Some(enabled) = env.get(KEY).copied().or_else(|| (env.get("all") == Some(&true)).then_some(true)) {
        return enabled;
    }
    if let Some(enabled) = inventory.latest(|info| info.experimental.as_ref()).and_then(|e| e.background_compaction) {
        return enabled;
    }
    let all_flags = crate::inventory::config::documents(&inventory.entries)
        .rev()
        .find_map(|info| info.experimental.as_ref().and_then(|e| e.all_flags));
    all_flags == Some(true) || matches!(crate::app::CHANNEL, "local" | "dev")
}

// --- Sizes ----------------------------------------------------------------------------------------------------

/// A prompt's size: `measured` is what the provider reported at the latest response, `estimated` the text since.
#[derive(Debug, Clone, Copy, Default)]
pub struct Estimate {
    pub measured: f64,
    pub estimated: f64,
}

impl Estimate {
    fn total(&self) -> f64 {
        self.measured + self.estimated
    }
}

/// A model's limits (`Model.Info["limit"]`).
#[derive(Debug, Clone, Copy)]
struct Limit {
    context: f64,
    input: f64,
}

impl Limit {
    fn of(model: &llm::Model) -> Self {
        Limit { context: model.limit_context, input: model.limit_input }
    }

    /// `inputWindow`: the largest prompt the model takes, or 0 when unknown.
    fn window(&self) -> f64 {
        if self.input > 0.0 && self.context > 0.0 { self.input.min(self.context) } else if self.input > 0.0 { self.input } else { self.context }
    }
}

/// `calculateCeiling`: the largest request the model takes while leaving room for its reply.
fn ceiling(limit: Limit, buffer: Option<Buffer>) -> f64 {
    let window = limit.window();
    if window <= 0.0 {
        return f64::INFINITY;
    }
    let tokens = buffer.map(|buffer| match buffer {
        Buffer::Share(share) => (window * share / 100.0 + 0.5).floor(),
        Buffer::Tokens(tokens) => tokens,
    });
    // A buffer that leaves no room, such as one set for a larger model, is ignored.
    if let Some(tokens) = tokens.filter(|tokens| *tokens < window) {
        return window - tokens;
    }
    window - (window * RESERVE_SHARE).floor().max(if window >= 2.0 * RESERVE_MIN { RESERVE_MIN } else { 0.0 })
}

/// `summaryRoom`: the largest summary request that leaves `RESERVE_MIN` of the window for the summary.
fn summary_room(limit: Limit, output: Option<f64>) -> f64 {
    (limit.window() - RESERVE_MIN.min(output.unwrap_or(RESERVE_MIN))).max(0.0)
}

fn estimate_media(mime: &str) -> f64 {
    let mime = mime.to_lowercase();
    if mime.starts_with("image/") {
        IMAGE_TOKEN_ESTIMATE
    } else if mime == "application/pdf" {
        PDF_TOKEN_ESTIMATE
    } else {
        0.0
    }
}

fn estimate_result(result: &ToolResult) -> f64 {
    match &result.value {
        ToolResultValue::Text(text) => tokens(text),
        ToolResultValue::Content(items) => items
            .iter()
            .map(|item| match json::get_str(item, "type") {
                Some("text") => tokens(json::get_str(item, "text").unwrap_or_default()),
                _ => estimate_media(json::get_str(item, "mime").unwrap_or_default()),
            })
            .sum(),
        ToolResultValue::Error { .. } => tokens(&error_value(result)),
    }
}

/// `JSON.stringify` of an error result's value (`{error, content}`).
fn error_value(result: &ToolResult) -> String {
    let ToolResultValue::Error { error, content } = &result.value else { return String::new() };
    let mut o = Obj::new();
    o.set("error", error.clone()).set("content", Json::Array(content.clone()));
    json::text(&o.done())
}

/// `estimateMessage`: only what providers receive.
fn estimate_message(message: &Message) -> f64 {
    match message {
        Message::User(parts) => parts
            .iter()
            .map(|part| match part {
                UserPart::Text(text) => tokens(text),
                UserPart::Media { mime, .. } => estimate_media(mime),
            })
            .sum(),
        Message::Assistant(parts) => parts
            .iter()
            .map(|part| match part {
                AssistantPart::Text { text, .. } | AssistantPart::Reasoning { text, .. } => tokens(text),
                AssistantPart::ToolCall { name, input, .. } => tokens(&format!("{name}{}", json::text(input))),
                AssistantPart::ToolResult(result) => estimate_result(result),
            })
            .sum(),
        Message::Tool(results) => results.iter().map(estimate_result).sum(),
        Message::System(text) => tokens(text),
    }
}

/// `estimateRequest`.
fn estimate_request<'a>(
    system: &[String],
    tools: impl Iterator<Item = &'a crate::tool::Definition>,
    messages: impl Iterator<Item = &'a Message>,
) -> f64 {
    system.iter().map(|part| tokens(part)).sum::<f64>()
        + tools.map(|tool| tokens(&format!("{}{}{}", tool.name, tool.description, json::text(&tool.parameters)))).sum::<f64>()
        + messages.map(estimate_message).sum::<f64>()
}

fn request_size(request: &llm::Request) -> f64 {
    estimate_request(&request.system, request.tools.iter(), request.messages.iter())
}

// --- Usage ----------------------------------------------------------------------------------------------------

/// `SessionUsage.Recorded`.
#[derive(Debug, Clone, Copy, Default)]
struct Usage {
    cost: f64,
    input: f64,
    output: f64,
    reasoning: f64,
    read: f64,
    write: f64,
}

impl Usage {
    fn record(usage: Option<&llm::Usage>, costs: &[Json]) -> Self {
        let (cost, tokens) = crate::session::step::usage(usage, costs);
        let n = |key: &str| json::get_num(&tokens, key).unwrap_or(0.0);
        let cache = |key: &str| json::get(&tokens, "cache").and_then(|c| json::get_num(c, key)).unwrap_or(0.0);
        Usage {
            cost: match cost {
                Json::Number(cost) => cost,
                _ => 0.0,
            },
            input: n("input"),
            output: n("output"),
            reasoning: n("reasoning"),
            read: cache("read"),
            write: cache("write"),
        }
    }

    fn add(self, other: Usage) -> Usage {
        Usage {
            cost: self.cost + other.cost,
            input: self.input + other.input,
            output: self.output + other.output,
            reasoning: self.reasoning + other.reasoning,
            read: self.read + other.read,
            write: self.write + other.write,
        }
    }

    fn tokens(&self) -> Json {
        let mut cache = Obj::new();
        cache.set("read", Json::Number(self.read)).set("write", Json::Number(self.write));
        let mut o = Obj::new();
        o.set("input", Json::Number(self.input))
            .set("output", Json::Number(self.output))
            .set("reasoning", Json::Number(self.reasoning))
            .set("cache", cache.done());
        o.done()
    }

    fn write(&self, o: &mut Obj) {
        o.set("cost", Json::Number(self.cost)).set("tokens", self.tokens());
    }
}

/// What each session's running compaction has spent so far; a session never runs two compactions at once.
fn spent() -> &'static Mutex<HashMap<String, Usage>> {
    static SPENT: OnceLock<Mutex<HashMap<String, Usage>>> = OnceLock::new();
    SPENT.get_or_init(Default::default)
}

fn add_spent(session: &str, usage: Usage) {
    let mut spent = spent().lock().unwrap();
    let total = spent.get(session).map_or(usage, |total| total.add(usage));
    spent.insert(session.to_owned(), total);
}

/// Forgets a compaction's spending when it ends, however it ends.
struct SpentGuard(String);

impl Drop for SpentGuard {
    fn drop(&mut self) {
        spent().lock().unwrap().remove(&self.0);
    }
}

// --- History --------------------------------------------------------------------------------------------------

fn is_completed_compaction(message: &Json) -> bool {
    json::get_str(message, "type") == Some("compaction") && json::get_str(message, "status") == Some("completed")
}

/// `SessionHistory.isNativeCompaction`: a completed provider compaction, which replays `native.messages`.
fn is_native(message: &Json) -> bool {
    is_completed_compaction(message) && json::get(message, "native").and_then(|n| json::get(n, "messages")).is_some()
}

/// `replayable`: a native window replays only on the model that made it (`None` is the model-neutral boundary).
fn replayable(message: &Json, model: Option<&Json>) -> bool {
    let Some(model) = model else { return true };
    if !is_native(message) {
        return true;
    }
    let made = json::get(message, "model");
    let same = |key: &str| made.and_then(|m| json::get_str(m, key)) == json::get_str(model, key);
    // TODO(port): decoding the window as canonical messages (`NativeWindow`); an array is taken as one.
    same("providerID") && same("id") && matches!(json::get(message, "native").and_then(|n| json::get(n, "messages")), Some(Json::Array(_)))
}

/// `latestCompaction`: the newest completed compaction that bounds a read.
fn latest_compaction(db: &Connection, session: &str, model: Option<&Json>, before: Option<i64>) -> rusqlite::Result<Option<i64>> {
    let mut sql = String::from(
        "SELECT seq FROM session_message WHERE session_id = ?1 AND type = 'compaction' \
         AND json_extract(data, '$.status') = 'completed' AND seq < ?2",
    );
    if model.is_some() {
        // Rows written before v2.1 hold the window in `providerContext`.
        sql.push_str(
            " AND (coalesce(json_extract(data, '$.native.messages'), json_extract(data, '$.providerContext')) IS NULL \
             OR (json_extract(data, '$.model.providerID') = ?3 AND json_extract(data, '$.model.id') = ?4))",
        );
    }
    sql.push_str(" ORDER BY seq DESC LIMIT 1");
    let before = before.unwrap_or(i64::MAX);
    match model {
        Some(model) => db
            .query_row(
                &sql,
                params![session, before, json::get_str(model, "providerID"), json::get_str(model, "id")],
                |row| row.get(0),
            )
            .optional(),
        None => db.query_row(&sql, params![session, before], |row| row.get(0)).optional(),
    }
}

fn rows(db: &Connection, session: &str, from: i64, until: i64) -> rusqlite::Result<Vec<(i64, Json)>> {
    let mut statement = db.prepare(
        "SELECT seq, id, type, data FROM session_message WHERE session_id = ?1 AND seq >= ?2 AND seq < ?3 ORDER BY seq ASC",
    )?;
    statement
        .query_map(params![session, from, until], |row| {
            Ok((row.get(0)?, message::from_row(row.get(1)?, row.get(2)?, &row.get::<_, String>(3)?)))
        })?
        .collect()
}

/// `messageEntries`: the messages from the latest completed compaction on (every message without one), with the
/// summary's kept text rebuilt.
fn entries(db: &Connection, session: &str, model: Option<&Json>, before: Option<i64>) -> rusqlite::Result<Vec<(i64, Json)>> {
    let compaction = latest_compaction(db, session, model, before)?;
    let decoded = rows(db, session, compaction.unwrap_or(i64::MIN), i64::MAX)?;
    // A window that cannot replay leaves nothing to start from, so read from the compaction before it.
    if let (Some(seq), Some((_, start))) = (compaction, decoded.first()) {
        if !replayable(start, model) {
            return entries(db, session, model, Some(seq));
        }
    }
    let entries = if compaction.is_some() { with_recent_text(db, session, decoded)? } else { decoded };
    // A native checkpoint's baseline supersedes the instruction updates before it.
    let native = entries.iter().rev().find(|(_, m)| is_native(m)).map(|(seq, _)| *seq);
    Ok(entries
        .into_iter()
        .filter(|(seq, m)| {
            !(json::get_str(m, "type") == Some("system") && native.is_some_and(|native| *seq < native)) && replayable(m, model)
        })
        .collect())
}

/// `withRecentText`: a summary references its kept messages by `metadata.keepID`; their text is rebuilt in memory.
fn with_recent_text(db: &Connection, session: &str, mut entries: Vec<(i64, Json)>) -> rusqlite::Result<Vec<(i64, Json)>> {
    let Some((first_seq, first)) = entries.first() else { return Ok(entries) };
    if !is_completed_compaction(first) {
        return Ok(entries);
    }
    let Some(keep) = json::get(first, "metadata").and_then(|m| json::get_str(m, "keepID")).filter(|id| id.starts_with("msg_"))
    else {
        return Ok(entries);
    };
    let start: Option<i64> = db
        .query_row("SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2", params![session, keep], |row| row.get(0))
        .optional()?;
    let Some(start) = start else { return Ok(entries) };
    let kept = rows(db, session, start, *first_seq)?;
    let recent = kept.iter().map(|(_, m)| message_text(m)).filter(|text| !text.is_empty()).collect::<Vec<_>>().join("\n\n");
    let first = &mut entries[0].1;
    let mut metadata = json::get(first, "metadata").cloned().filter(|m| matches!(m, Json::Object(_))).unwrap_or_else(json::object);
    json::set(&mut metadata, "recent", json::string(&recent));
    json::set(first, "metadata", metadata);
    Ok(entries)
}

/// `SessionHistory.load`: the history later requests see, from the latest completed compaction on. `model` is the
/// model the history is read for (its native windows bound the read); `None` reads model-neutrally (`latest`).
pub fn history(db: &Connection, session: &str, model: Option<&Json>) -> rusqlite::Result<Vec<Json>> {
    Ok(entries(db, session, model, None)?.into_iter().map(|(_, m)| m).collect())
}

/// The sequence the model-neutral history starts at (the latest completed compaction), 0 without one.
pub fn boundary(db: &Connection, session: &str) -> rusqlite::Result<i64> {
    Ok(latest_compaction(db, session, None, None)?.unwrap_or(0))
}

/// `SessionRecentText.kept`.
fn kept(message: &Json) -> &str {
    json::get(message, "metadata").and_then(|m| json::get_str(m, "recent")).unwrap_or_default()
}

/// `truncateToolOutput`: at most 1,250 code points.
fn truncate_output(value: &str) -> String {
    if value.encode_utf16().count() <= TOOL_OUTPUT_MAX_CHARS {
        return value.to_owned();
    }
    match value.char_indices().nth(TOOL_OUTPUT_MAX_CHARS) {
        Some((end, _)) => format!("{}\n[truncated]", &value[..end]),
        None => value.to_owned(),
    }
}

/// `serializeToolContent`.
fn serialize_content(content: &[Json]) -> String {
    content
        .iter()
        .map(|item| match json::get_str(item, "type") {
            Some("text") => json::get_str(item, "text").unwrap_or_default().to_owned(),
            _ => format!(
                "[Attached {}{}]",
                json::get_str(item, "mime").unwrap_or_default(),
                json::get_str(item, "name").map(|name| format!(": {name}")).unwrap_or_default()
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `SessionRecentText.messageToText`: one message as the recent, verbatim part of a summary shows it.
fn message_text(message: &Json) -> String {
    let text = || json::get_str(message, "text").unwrap_or_default();
    let list = |key: &str| match json::get(message, key) {
        Some(Json::Array(items)) => items.clone(),
        _ => vec![],
    };
    match json::get_str(message, "type") {
        Some("user") => {
            let mut lines: Vec<String> = list("skills")
                .iter()
                .filter_map(|skill| {
                    json::get_str(skill, "text")
                        .map(|text| format!("[Skill activated: {}]\n{text}", json::get_str(skill, "name").unwrap_or_default()))
                })
                .collect();
            lines.push(format!("[User]: {}", text()));
            for file in list("files") {
                let source = json::get(&file, "source");
                let uri = source.filter(|s| json::get_str(s, "type") == Some("uri")).and_then(|s| json::get_str(s, "uri"));
                let name = json::get_str(&file, "name").or(uri).unwrap_or("inline attachment");
                lines.push(format!("[Attached {}: {name}]", json::get_str(&file, "mime").unwrap_or_default()));
            }
            lines.join("\n")
        }
        Some("location-switched") => format!(
            "[User]: The working directory has been changed to {}.",
            json::get(message, "location").and_then(|l| json::get_str(l, "directory")).unwrap_or_default()
        ),
        Some("assistant") => list("content")
            .iter()
            .flat_map(|part| match json::get_str(part, "type") {
                Some("text") => vec![format!("[Assistant]: {}", json::get_str(part, "text").unwrap_or_default())],
                Some("reasoning") => vec![],
                _ => {
                    let state = json::get(part, "state").cloned().unwrap_or(Json::Null);
                    let input = match json::get(&state, "input") {
                        Some(Json::String(input)) => input.clone(),
                        Some(input) => json::text(input),
                        None => "undefined".to_owned(),
                    };
                    let call = format!("[Assistant tool call]: {}({input})", json::get_str(part, "name").unwrap_or_default());
                    match json::get_str(&state, "status") {
                        Some("completed") => {
                            let content = json::array(json::get(&state, "content").unwrap_or(&Json::Null)).unwrap_or_default();
                            vec![call, format!("[Tool result]: {}", truncate_output(&serialize_content(content)))]
                        }
                        Some("error") => vec![
                            call,
                            format!(
                                "[Tool error]: {}",
                                json::get(&state, "error").and_then(|e| json::get_str(e, "message")).unwrap_or_default()
                            ),
                        ],
                        _ => vec![call],
                    }
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some("synthetic") => format!("[Synthetic context]: {}", text()),
        Some("skill") => format!("[Skill activated: {}]\n{}", json::get_str(message, "name").unwrap_or_default(), text()),
        Some("shell") => {
            if json::get(message, "metadata").and_then(|m| json::get_bool(m, "background")) == Some(true) {
                return String::new();
            }
            let output = json::get(message, "output").and_then(|o| json::get_str(o, "output")).unwrap_or_default();
            format!("[Shell]: {}\n{}", json::get_str(message, "command").unwrap_or_default(), truncate_output(output))
        }
        _ => String::new(),
    }
}

// --- The model-visible transcript -----------------------------------------------------------------------------

/// The canonical messages of a history (`toLLMMessages`), each with the ID of the message it came from where the
/// canonical message carries one.
struct Transcript {
    messages: Vec<Message>,
    ids: Vec<Option<String>>,
}

fn transcript(history: &[Json], model: &llm::Model) -> Transcript {
    let mut out = Transcript { messages: Vec::new(), ids: Vec::new() };
    for item in history {
        let produced = context::messages(std::slice::from_ref(item), &model.reference);
        let carries = match json::get_str(item, "type") {
            Some("user" | "synthetic" | "skill" | "shell" | "location-switched" | "compaction") => true,
            Some("assistant") => matches!(produced.first(), Some(Message::Assistant(_))),
            _ => false,
        };
        for (index, produced) in produced.into_iter().enumerate() {
            out.ids.push((index == 0 && carries).then(|| json::get_str(item, "id").unwrap_or_default().to_owned()));
            out.messages.push(produced);
        }
    }
    context::unsupported_parts(&mut out.messages, &model.input, &model.provider);
    out
}

fn tokens_of(message: &Json) -> Option<&Json> {
    json::get(message, "tokens").filter(|t| matches!(t, Json::Object(_)))
}

/// `promptTokens`: input, cache reads and cache writes.
fn prompt_tokens(tokens: &Json) -> f64 {
    let cache = |key: &str| json::get(tokens, "cache").and_then(|c| json::get_num(c, key)).unwrap_or(0.0);
    json::get_num(tokens, "input").unwrap_or(0.0) + cache("read") + cache("write")
}

/// `hasMeasuredPrompt`: a response of the current provider that reported its prompt.
fn measured(message: &Json, model: &Json) -> bool {
    json::get_str(message, "type") == Some("assistant")
        && json::get(message, "model").and_then(|m| json::get_str(m, "providerID")) == json::get_str(model, "providerID")
        && json::get(message, "error").is_none_or(|e| matches!(e, Json::Null))
        && tokens_of(message).is_some_and(|tokens| prompt_tokens(tokens) > 0.0)
}

fn join_initial(initial: &[(String, String)]) -> String {
    initial.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("\n\n")
}

/// `SessionContext.Loaded` as compaction sees it: the step's selection, its model, initial instructions and history.
#[derive(Clone)]
pub struct Context {
    session: Json,
    session_id: String,
    agent_id: String,
    agent: Json,
    tools: crate::tool::Snapshot,
    model: Arc<llm::Model>,
    initial: Vec<(String, String)>,
    /// Shared, so the context's copies (another model, a background summary) do not copy the history.
    messages: Arc<Vec<Json>>,
    /// The request's system parts after the request hooks (`baseTranscript` plus the `compaction` hooks).
    system: Vec<String>,
    location: LocationRef,
    inventory: Arc<Inventory>,
}

impl Context {
    fn new(loaded: &Loaded, initial: Vec<(String, String)>, messages: Vec<Json>) -> Self {
        Context {
            session_id: json::get_str(&loaded.session, "id").unwrap_or_default().to_owned(),
            session: loaded.session.clone(),
            agent_id: loaded.agent_id.clone(),
            agent: loaded.agent.clone(),
            tools: loaded.tools.clone(),
            model: loaded.model.clone(),
            system: crate::session::runner::system(loaded, &initial),
            initial,
            messages: Arc::new(messages),
            location: loaded.reference.clone(),
            inventory: loaded.location.inventory(),
        }
    }

    /// `SessionContext.load`: the history a step sends (`SessionHistory.entriesForRunner`).
    pub fn load(app: &App, loaded: &Loaded) -> Result<Self, Stop> {
        let session = json::get_str(&loaded.session, "id").unwrap_or_default();
        let (messages, initial) = {
            let db = app.db.lock().unwrap();
            let messages = history(&db, session, Some(&loaded.model.reference)).map_err(defect)?;
            (messages, instructions::initial(&db, session, &loaded.sources).map_err(defect)?)
        };
        Ok(Self::new(loaded, initial, messages))
    }

    /// `SessionHistory.preview`: the settled history and the initial instructions, recording nothing.
    fn preview(app: &App, loaded: &Loaded) -> Result<Self, Stop> {
        let session = json::get_str(&loaded.session, "id").unwrap_or_default();
        let (messages, initial) = {
            let db = app.db.lock().unwrap();
            let mut messages = history(&db, session, Some(&loaded.model.reference)).map_err(defect)?;
            // An active assistant may contain an unresolved tool call, so only the settled prefix is previewed.
            if let Some(unsettled) = messages.iter().position(|m| {
                json::get_str(m, "type") == Some("assistant") && json::get(m, "time").and_then(|t| json::get(t, "completed")).is_none()
            }) {
                messages.truncate(unsettled);
            }
            (messages, instructions::preview(&db, session, &loaded.sources).map_err(defect)?.0)
        };
        Ok(Self::new(loaded, initial, messages))
    }

    /// The history a step sends, from the latest completed compaction on.
    pub fn messages(&self) -> &[Json] {
        &self.messages
    }

    /// The initial instructions (the epoch baseline), rendered.
    pub fn initial(&self) -> &[(String, String)] {
        &self.initial
    }

    /// The same context on another model: the request hooks name the model in the system parts.
    fn with_model(&self, model: Arc<llm::Model>) -> Self {
        let before = context::identity(&self.model.name, &self.model.reference);
        let after = context::identity(&model.name, &model.reference);
        let mut next = self.clone();
        for part in next.system.iter_mut().filter(|part| **part == before) {
            *part = after.clone();
        }
        next.model = model;
        next
    }

    /// `baseTranscript`'s system parts: the agent's prompt and the initial instructions, before the request hooks.
    fn base_system(&self) -> Vec<String> {
        let prompt = match json::get_str(&self.agent, "system").filter(|s| !s.is_empty()) {
            Some(system) => system.to_owned(),
            None => context::system_prompt(&self.tools.names()),
        };
        [prompt, join_initial(&self.initial)].into_iter().filter(|part| !part.is_empty()).collect()
    }
}

/// `estimatePrompt`: the size of the prompt the context sends, measured at the latest response plus the estimated
/// size of what followed it.
fn estimate_of(context: &Context, messages: &[Json]) -> Estimate {
    let reference = &context.model.reference;
    let anchor = messages.iter().rposition(|m| measured(m, reference));
    let base = transcript(&messages[anchor.unwrap_or(0)..], &context.model);
    let anchor = anchor.map(|index| &messages[index]);
    let anchor_id = anchor.and_then(|a| json::get_str(a, "id"));
    // The anchor's usage covers its own output, but not its local tool results, which the provider never saw.
    let unmeasured = base
        .messages
        .iter()
        .zip(&base.ids)
        .filter(|(message, id)| !(matches!(message, Message::Assistant(_)) && anchor_id.is_some() && id.as_deref() == anchor_id))
        .map(|(message, _)| message);
    match anchor.and_then(tokens_of) {
        None => Estimate { measured: 0.0, estimated: estimate_request(&context.base_system(), context.tools.definitions(), unmeasured) },
        Some(tokens) => {
            let n = |key: &str| json::get_num(tokens, key).unwrap_or(0.0);
            Estimate {
                measured: prompt_tokens(tokens) + n("output") + n("reasoning"),
                estimated: unmeasured.map(estimate_message).sum(),
            }
        }
    }
}

/// `SessionCompaction.estimatePrompt` of a step's context.
pub fn estimate_prompt(context: &Context) -> Estimate {
    estimate_of(context, &context.messages)
}

/// `measuredPrompt`: the prompt size, only while the model, variant and agent that were measured still match.
fn measured_prompt(context: &Context, messages: &[Json]) -> Option<Estimate> {
    let reference = &context.model.reference;
    let anchor = messages.iter().rev().find(|m| measured(m, reference))?;
    let model = json::get(anchor, "model");
    let same = |key: &str| model.and_then(|m| json::get_str(m, key)) == json::get_str(reference, key);
    if !same("id") || !same("variant") || json::get_str(anchor, "agent") != Some(context.agent_id.as_str()) {
        return None;
    }
    Some(estimate_of(context, messages))
}

/// `due`: whether automatic compaction is due before the next step.
fn due(context: &Context, ceiling: f64, estimate: Option<Estimate>) -> bool {
    let messages = &context.messages;
    // A compaction just completed; let the runner rebuild the request from it first.
    if messages.last().is_some_and(is_completed_compaction) {
        return false;
    }
    // An encrypted native window estimates as nothing, so wait for a response to measure it.
    let measured_at = messages.iter().rposition(|m| measured(m, &context.model.reference));
    let native_at = messages.iter().rposition(is_native);
    if native_at.is_some() && measured_at < native_at {
        return false;
    }
    estimate.unwrap_or_else(|| estimate_prompt(context)).total() >= ceiling
}

/// `previousCompaction`: history loads from the latest completed compaction, so a previous one is always first.
fn previous_compaction(messages: &[Json]) -> Option<&Json> {
    messages.first().filter(|m| is_completed_compaction(m))
}

/// `oldestToDrop`: how many of the oldest items to drop so the rest fit `budget`.
fn oldest_to_drop(sizes: &[f64], budget: f64) -> usize {
    let mut total = 0.0;
    let mut start = sizes.len();
    while start > 0 {
        let next = total + sizes[start - 1];
        if next > budget {
            break;
        }
        total = next;
        start -= 1;
    }
    start
}

/// `splitConversation`: the first message kept verbatim beside the summary (the newest messages within `keep`
/// tokens, from a user message on); `None` when there is nothing to compact, `Some(None)` when nothing is kept.
fn split_conversation(messages: &[Json], keep: f64) -> Option<Option<String>> {
    let entries: Vec<(&Json, String)> =
        messages.iter().filter_map(|m| Some(message_text(m)).filter(|t| !t.is_empty()).map(|t| (m, t))).collect();
    if entries.is_empty() {
        return None;
    }
    let sizes: Vec<f64> = entries.iter().map(|(_, text)| tokens(text)).collect();
    // Drop the oldest entries until the rest fit the allowance, but always keep the newest one.
    let dropped = oldest_to_drop(&sizes, keep).min(entries.len() - 1);
    let user = |(m, _): &(&Json, String)| json::get_str(m, "type") == Some("user");
    // Start at a user message so an assistant's tool calls and results stay together.
    let boundary = entries.iter().enumerate().rposition(|(index, entry)| index <= dropped && user(entry));
    let start = match boundary.filter(|index| *index > 0) {
        Some(index) => index,
        // Everything fits. Keep only the latest exchange rather than duplicating the whole conversation.
        None => match entries.iter().rposition(user).filter(|index| *index > 0) {
            Some(index) => index,
            // One exchange, nothing older: summarize it all and keep nothing, unless a previous summary already kept
            // recent text, in which case keep everything and summarize only the summary before it.
            None if previous_compaction(messages).is_some_and(|p| !kept(p).is_empty()) => 0,
            None => entries.len(),
        },
    };
    Some(entries.get(start).and_then(|(m, _)| json::get_str(m, "id")).map(str::to_owned))
}

/// `clip`: `TRANSCRIPT_CLIP` characters from the start and end, never splitting a surrogate pair.
fn clip(value: &str) -> String {
    if value.encode_utf16().count() <= TRANSCRIPT_CLIP {
        return value.to_owned();
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= TRANSCRIPT_CLIP {
        return value.to_owned();
    }
    let half = TRANSCRIPT_CLIP / 2;
    let omitted = chars.len() - TRANSCRIPT_CLIP;
    let head: String = chars[..half].iter().collect();
    let tail: String = chars[chars.len() - half..].iter().collect();
    format!("{head}\n[… {omitted} characters omitted …]\n{tail}")
}

fn result_line(result: &ToolResult, level: usize) -> String {
    let label = if matches!(result.value, ToolResultValue::Error { .. }) { "Tool error" } else { "Tool result" };
    if level >= OUTPUTS_REMOVED {
        return format!("[{label} omitted]");
    }
    let output = match &result.value {
        ToolResultValue::Content(items) => serialize_content(items),
        ToolResultValue::Text(text) => text.clone(),
        ToolResultValue::Error { .. } => error_value(result),
    };
    format!("[{label}]: {}", if level >= OUTPUTS_CLIPPED { clip(&output) } else { output })
}

/// `flattenMessage`: one request message as transcript lines at a shortening level.
fn flatten_message(message: &Message, level: usize) -> String {
    if level == OMITTED || matches!(message, Message::System(_)) {
        return String::new();
    }
    let user = matches!(message, Message::User(_));
    if level >= USER_ONLY && !user {
        return String::new();
    }
    let text = |text: &str| {
        let text = if level >= USER_ONLY { clip(text) } else { text.to_owned() };
        format!("[{}]: {text}", if user { "User" } else { "Assistant" })
    };
    let lines: Vec<String> = match message {
        Message::User(parts) => parts
            .iter()
            .filter_map(|part| match part {
                UserPart::Text(t) if t.is_empty() => None,
                UserPart::Text(t) => Some(text(t)),
                _ if level >= USER_ONLY => None,
                UserPart::Media { mime, .. } => Some(format!("[{mime} omitted]")),
            })
            .collect(),
        Message::Assistant(parts) => parts
            .iter()
            .filter_map(|part| match part {
                AssistantPart::Text { text: t, .. } if t.is_empty() => None,
                AssistantPart::Text { text: t, .. } => Some(text(t)),
                AssistantPart::Reasoning { .. } => None,
                AssistantPart::ToolCall { name, input, .. } => {
                    let input = json::text(input);
                    Some(format!("[Assistant tool call]: {name}({})", if level >= OUTPUTS_REMOVED { clip(&input) } else { input }))
                }
                AssistantPart::ToolResult(result) => Some(result_line(result, level)),
            })
            .collect(),
        Message::Tool(results) => results.iter().map(|result| result_line(result, level)).collect(),
        Message::System(_) => vec![],
    };
    lines.join("\n")
}

// --- The summary request --------------------------------------------------------------------------------------

/// The conversation as the runner would send it, with the summary request's output limit (`prepare`).
struct Prepared {
    request: llm::Request,
    ids: Vec<Option<String>>,
}

/// `prepare`: the output limit leaves room for `budget`, the most `deliver` sends.
fn prepare(context: &Context, messages: &[Json], budget: f64) -> Prepared {
    let Transcript { messages, ids } = transcript(messages, &context.model);
    let request = llm::Request {
        model: context.model.clone(),
        system: context.system.clone(),
        messages,
        tools: context.tools.definitions().cloned().collect(),
        tool_choice: None,
        max_tokens: Some(context::fitted_output_limit(&context.model, Some((budget, 0.0)), 0.0)),
        prompt_cache_key: Some(context::cache_key(&context.session)),
        headers: context::headers(&context.session),
    };
    Prepared { request, ids }
}

/// `fitOutput`: lowers the output limit to the room the request being sent leaves in the window.
fn fit_output(request: &mut llm::Request, size: Option<Estimate>) {
    let Some(current) = request.max_tokens else { return };
    let input = size.unwrap_or(Estimate { measured: 0.0, estimated: request_size(request) });
    let model = request.model.clone();
    let limit = context::fitted_output_limit(&model, Some((input.measured, input.estimated)), 0.0)
        .max(SUMMARY_OUTPUT_FLOOR.min(context::fitted_output_limit(&model, Some((input.total(), 0.0)), 0.0)));
    request.max_tokens = Some(current.min(limit));
}

/// A request's settings without its messages.
fn without_messages(request: &llm::Request) -> llm::Request {
    llm::Request {
        model: request.model.clone(),
        system: request.system.clone(),
        messages: vec![],
        tools: request.tools.clone(),
        tool_choice: request.tool_choice,
        max_tokens: request.max_tokens,
        prompt_cache_key: request.prompt_cache_key.clone(),
        headers: request.headers.clone(),
    }
}

/// What a summary produced: its text, and the event metadata (`keepID`).
#[derive(Debug, Clone)]
struct Summary {
    text: String,
    metadata: Option<Json>,
}

enum Fail {
    /// A provider failure, which `deliver` may shrink the request for or retry.
    Ai(AIError),
    /// A reply that cannot be used; never retried.
    Unusable(Json),
}

/// A summary run off the critical path: it covers only `messages`, streams no deltas, and bills into `usage`.
struct Background {
    /// The context's messages, of which the first `covered` are summarized.
    messages: Arc<Vec<Json>>,
    covered: usize,
    usage: Arc<Mutex<Vec<Usage>>>,
}

fn publish(app: &App, location: &LocationRef, ty: &'static str, data: Json, metadata: Option<Json>) -> Result<(), Stop> {
    crate::session::publish_event(app, ty, data, Some(location.clone()), metadata).map(drop).map_err(defect)
}

/// `spend`: each model call is billed as soon as it finishes, so failed and interrupted compactions are billed too.
fn spend(app: &App, context: &Context, usage: Usage, background: Option<&Background>) -> Result<(), Stop> {
    match background {
        Some(background) => background.usage.lock().unwrap().push(usage),
        None => add_spent(&context.session_id, usage),
    }
    let mut data = Obj::new();
    data.str("sessionID", &context.session_id).str("source", "compaction");
    usage.write(&mut data);
    publish(app, &context.location, "session.usage.recorded", data.done(), None)
}

/// `stream`: one summary call; its text, or why it cannot be used.
async fn stream(app: &App, context: &Context, request: &llm::Request, background: Option<&Background>) -> Result<String, Fail> {
    let (sender, mut receiver) = mpsc::channel::<Result<llm::Event, AIError>>(64);
    let producer = llm::stream(request, sender);
    tokio::pin!(producer);
    let mut produced = false;
    let mut text = String::new();
    loop {
        let next = tokio::select! {
            _ = &mut producer, if !produced => {
                produced = true;
                continue;
            }
            next = receiver.recv() => next,
        };
        match next {
            None => return Ok(text),
            Some(Err(error)) => return Err(Fail::Ai(error)),
            Some(Ok(llm::Event::TextDelta { text: delta, .. })) => {
                text.push_str(&delta);
                if background.is_none() {
                    let mut data = Obj::new();
                    data.str("sessionID", &context.session_id).str("text", &delta);
                    publish(app, &context.location, "session.compaction.delta", data.done(), None).map_err(|_| Fail::Unusable(error("unknown", "Internal server error")))?;
                }
            }
            Some(Ok(llm::Event::StepFinish { finish, usage, .. })) => {
                spend(app, context, Usage::record(usage.as_ref(), &context.model.cost), background)
                    .map_err(|_| Fail::Unusable(error("unknown", "Internal server error")))?;
                match finish.normalized.as_str() {
                    "unknown" => return Err(Fail::Ai(AIError::incomplete("The provider response ended with an unknown finish reason."))),
                    "error" => return Err(Fail::Ai(AIError::new(llm::error::Kind::UnknownProvider, "Compaction generation failed"))),
                    "length" => return Err(Fail::Unusable(error("compaction.failed", "Compaction summary reached the output token limit"))),
                    "content-filter" => {
                        return Err(Fail::Unusable(error("provider.content-filter", "Compaction summary was blocked by the provider")));
                    }
                    _ => {}
                }
            }
            Some(Ok(_)) => {}
        }
    }
}

/// `send`: the request with the summary prompt; a reply that ignores the template gets one reminder. The prompt is
/// appended in place, and the request is left as it was given.
async fn send(
    app: &App,
    context: &Context,
    request: &mut llm::Request,
    size: Option<Estimate>,
    prompt: &str,
    background: Option<&Background>,
) -> Result<Summary, Fail> {
    let (length, max_tokens) = (request.messages.len(), request.max_tokens);
    let outcome = prompted(app, context, request, size, prompt, background).await;
    request.messages.truncate(length);
    request.max_tokens = max_tokens;
    outcome
}

async fn prompted(
    app: &App,
    context: &Context,
    request: &mut llm::Request,
    size: Option<Estimate>,
    prompt: &str,
    background: Option<&Background>,
) -> Result<Summary, Fail> {
    request.messages.push(Message::User(vec![UserPart::Text(prompt.to_owned())]));
    let grown = |extra: f64| size.map(|size| Estimate { measured: size.measured, estimated: size.estimated + extra });
    fit_output(request, grown(tokens(prompt)));
    let reply = stream(app, context, request, background).await?;
    if filled(&reply) {
        return Ok(Summary { text: reply, metadata: None });
    }
    request.messages.push(Message::User(vec![UserPart::Text(NUDGE.to_owned())]));
    fit_output(request, grown(tokens(prompt) + tokens(NUDGE)));
    let retry = stream(app, context, request, background).await?;
    if filled(&retry) {
        return Ok(Summary { text: retry, metadata: None });
    }
    let message = if retry.trim().is_empty() {
        "Compaction produced no summary"
    } else {
        "Compaction summary did not match the required template"
    };
    Err(Fail::Unusable(error("compaction.failed", message)))
}

/// `flattenAndDropOldest`: `[previous compaction][transcript]`, the conversation since that compaction as one text
/// message, its exchanges shortened oldest first, one level at a time, until it fits `target`. `None` when it cannot
/// fit even so (the newest exchange is never omitted), or when there is nothing to send.
fn flatten(prepared: &Prepared, history: usize, context: &Context, target: f64) -> Option<llm::Request> {
    let request = &prepared.request;
    let messages = &request.messages[..history];
    let ids = &prepared.ids[..history];
    let previous = previous_compaction(&context.messages);
    let previous_id = previous.and_then(|p| json::get_str(p, "id"));
    let end = ids.iter().rposition(|id| previous_id.is_some() && id.as_deref() == previous_id).map_or(0, |index| index + 1);
    let summary = previous.filter(|p| !is_native(p));
    // As in TS, the previous summary is rebuilt from storage, which would drop a compaction hook's edits to it.
    let lead: Vec<Message> = match summary {
        Some(summary) => {
            let mut bare = summary.clone();
            let mut metadata = json::get(&bare, "metadata").cloned().filter(|m| matches!(m, Json::Object(_))).unwrap_or_else(json::object);
            json::set(&mut metadata, "recent", json::string(""));
            json::set(&mut bare, "metadata", metadata);
            context::messages(&[bare], &context.model.reference)
        }
        None => messages[..end].to_vec(),
    };
    let recent = summary.map(kept).unwrap_or_default();

    // A user message and everything after it, up to the next one, is one exchange; each is rendered at every level.
    let mut exchanges: Vec<[String; LEVELS]> = Vec::new();
    if !recent.is_empty() {
        exchanges.push(std::array::from_fn(|level| if level == OMITTED { String::new() } else { recent.to_owned() }));
    }
    let mut groups: Vec<Vec<&Message>> = Vec::new();
    for message in &messages[end..] {
        match groups.last_mut() {
            Some(group) if !matches!(message, Message::User(_)) => group.push(message),
            _ => groups.push(vec![message]),
        }
    }
    for group in groups {
        let renders: [String; LEVELS] = std::array::from_fn(|level| {
            group.iter().map(|m| flatten_message(m, level)).filter(|text| !text.is_empty()).collect::<Vec<_>>().join("\n")
        });
        if !renders[0].is_empty() {
            exchanges.push(renders);
        }
    }
    if exchanges.is_empty() {
        return None;
    }
    let note = |omitted: usize| {
        if omitted == 0 {
            String::new()
        } else {
            format!("[{omitted} older {} omitted]\n\n", if omitted == 1 { "exchange" } else { "exchanges" })
        }
    };
    let size = |text: &str| if text.is_empty() { 0.0 } else { tokens(text) + 1.0 };
    // Room for the longest possible note is set aside before choosing what to keep.
    let room = target - estimate_request(&request.system, std::iter::empty(), lead.iter()) - tokens(&note(exchanges.len()));
    let mut levels = vec![0usize; exchanges.len()];
    let mut total: f64 = exchanges.iter().map(|renders| size(&renders[0])).sum();
    let newest = exchanges.len() - 1;
    for level in 1..LEVELS {
        let mut index = 0;
        while index < exchanges.len() && total > room {
            // The newest exchange is what the summary most needs; it is shortened but never left out.
            if !(index == newest && exchanges[index][level].is_empty()) {
                total += size(&exchanges[index][level]) - size(&exchanges[index][levels[index]]);
                levels[index] = level;
            }
            index += 1;
        }
    }
    if total > room {
        return None;
    }
    let omitted = levels.iter().enumerate().filter(|(index, level)| exchanges[*index][**level].is_empty()).count();
    let text = exchanges
        .iter()
        .zip(&levels)
        .map(|(renders, level)| renders[*level].as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut messages = lead;
    messages.push(Message::User(vec![UserPart::Text(format!("{}{text}", note(omitted)))]));
    Some(llm::Request { messages, tools: vec![], ..without_messages(request) })
}

/// How `deliver` may shrink the request.
struct Shrink {
    /// The size the request is sent unchanged within.
    fit: f64,
    /// The history before the kept tail (a prefix of the request's messages).
    history: usize,
    /// The measured size of the unchanged request, until the provider rejects it.
    measured: Option<Estimate>,
    /// Send only as text (the provider just rejected this conversation).
    text: bool,
}

enum Pick {
    Prepared,
    History,
    Text(llm::Request),
}

/// `deliver`: sends the request unchanged if it fits, else the history before the tail unchanged, else that history as
/// text aimed at `target`. A "too long" rejection aims the next attempt at 85%, 70%, 50%, then 35% of the first rejected
/// request's size; a "payload too large" one on an unchanged request resends it as text. Other provider failures are
/// retried under the session's retry policy.
async fn deliver(
    app: &App,
    context: &Context,
    prepared: &mut Prepared,
    mut target: f64,
    shrink: Shrink,
    prompt: &str,
    background: Option<&Background>,
) -> Result<Summary, Json> {
    let mut retry = crate::session::retry::Retry::default();
    let mut rejections = 0;
    let mut rejected: Option<f64> = None;
    let mut as_text = shrink.text;
    let mut fit = shrink.fit;
    let all = prepared.request.messages.len();
    // `withoutTail` returns the request itself when the tail is not found: then history and request are one.
    let same = shrink.history == all;
    loop {
        let measured = if rejections == 0 { shrink.measured } else { None };
        let request = &prepared.request;
        let unchanged = if as_text {
            None
        } else if measured.map(|m| m.total()).unwrap_or_else(|| request_size(request)) <= fit {
            Some(Pick::Prepared)
        } else if estimate_request(&request.system, request.tools.iter(), request.messages[..shrink.history].iter()) <= target {
            Some(Pick::History)
        } else {
            None
        };
        let cannot = || error("compaction.failed", "The compaction request cannot be reduced further without losing the latest exchange or checkpoint");
        let pick = match unchanged {
            Some(pick) => pick,
            None => match flatten(prepared, shrink.history, context, target) {
                Some(request) => Pick::Text(request),
                None if rejections > 0 => return Err(cannot()),
                None if as_text => match flatten(prepared, shrink.history, context, f64::INFINITY) {
                    Some(request) => Pick::Text(request),
                    None => return Err(cannot()),
                },
                None => Pick::History,
            },
        };
        let is_prepared = matches!(pick, Pick::Prepared) || (same && matches!(pick, Pick::History));
        let is_history = matches!(pick, Pick::History) || (same && matches!(pick, Pick::Prepared));
        let size = if is_prepared { measured } else { None };
        let (sent_size, outcome) = match pick {
            Pick::Prepared => {
                let sent = size.map(|s| s.total()).unwrap_or_else(|| request_size(&prepared.request));
                (sent, send(app, context, &mut prepared.request, size, prompt, background).await)
            }
            Pick::History => {
                // The tail is set aside, not copied, while the history goes out.
                let tail = prepared.request.messages.split_off(shrink.history);
                let sent = size.map(|s| s.total()).unwrap_or_else(|| request_size(&prepared.request));
                let outcome = send(app, context, &mut prepared.request, size, prompt, background).await;
                prepared.request.messages.extend(tail);
                (sent, outcome)
            }
            Pick::Text(mut request) => (request_size(&request), send(app, context, &mut request, size, prompt, background).await),
        };
        let cause = match outcome {
            Ok(summary) => return Ok(summary),
            Err(Fail::Unusable(error)) => return Err(error),
            Err(Fail::Ai(cause)) => cause,
        };
        let failure = cause.session_error();
        let too_large = cause.kind == llm::error::Kind::InvalidRequest && cause.classification == Some("payload-too-large");
        if too_large && (is_prepared || is_history) {
            as_text = true;
            continue;
        }
        if too_large || cause.context_overflow() {
            let Some(step) = SHRINK_STEPS.get(rejections) else { return Err(failure) };
            let rejected = *rejected.get_or_insert(sent_size);
            // The characters can still say the rejected request fits, so it only goes out again as text.
            if size.is_some() {
                as_text = true;
            }
            target = (rejected * step).floor();
            fit = target;
            rejections += 1;
            continue;
        }
        match retry.decide(&cause, cause.retryable()) {
            Some(decision) if decision.retry => tokio::time::sleep(std::time::Duration::from_millis(decision.delay as u64)).await,
            _ => return Err(failure),
        }
    }
}

/// `summarize`: the whole conversation, including the tail kept beside the summary, then the summary prompt.
async fn summarize(
    app: &App,
    overflow: bool,
    context: &Context,
    budget: f64,
    keep: f64,
    background: Option<&Background>,
) -> Result<Summary, Json> {
    let messages = background.map_or(context.messages.as_slice(), |b| &b.messages[..b.covered]);
    let Some(split) = split_conversation(messages, keep) else {
        return Err(error("compaction.unavailable", "Nothing to compact yet"));
    };
    // A background summary covers all of its messages; what follows them is kept when it is swapped in.
    let keep_id = if background.is_some() { None } else { split };
    let previous = previous_compaction(messages);
    let prompt = build_prompt(
        previous.is_some(),
        previous.and_then(|p| json::get_str(p, "summary")).is_some_and(|s| s.contains(LEGACY_HEADING)),
    );
    let mut prepared = prepare(context, messages, budget);
    // An overflow already proved the measured count low, so it keeps the character estimate.
    let measured = if overflow { None } else { measured_prompt(context, messages) };
    let overhead = tokens(&prompt) + tokens(NUDGE);
    let fit = if overflow { budget } else { budget.max(summary_room(Limit::of(&context.model), prepared.request.max_tokens)) };
    let history = keep_id
        .as_deref()
        .and_then(|keep| prepared.ids.iter().position(|id| id.as_deref() == Some(keep)))
        .unwrap_or(prepared.request.messages.len());
    let shrink = Shrink { fit: fit - overhead, history, measured, text: overflow };
    let result = deliver(app, context, &mut prepared, budget - overhead, shrink, &prompt, background).await?;
    let Some(keep_id) = keep_id else { return Ok(result) };
    let mut metadata = Obj::new();
    metadata.str("keepID", &keep_id);
    let mut metadata = metadata.done();
    if let Some(Json::Object(extra)) = &result.metadata {
        for (key, value) in extra {
            json::set(&mut metadata, key, value.clone());
        }
    }
    Ok(Summary { text: result.text, metadata: Some(metadata) })
}

/// `selectModel`: the model that writes the summary (the `compaction` agent's, else the context's) and the request
/// size it aims for.
fn select_model(context: &Context, buffer: Option<Buffer>) -> (Context, f64) {
    let preferred = context::agent(&context.inventory, "compaction").and_then(|agent| json::get(&agent, "model").cloned());
    let model = preferred
        .and_then(|model| {
            let mut session = context.session.clone();
            json::set(&mut session, "model", model);
            context::resolve_model(&context.inventory, &session, None).ok()
        })
        .unwrap_or_else(|| context.model.clone());
    let limit = Limit::of(&model);
    let ceiling = ceiling(limit, buffer);
    // An unknown window never triggers automatic compaction, but the compaction request still needs a size to aim for.
    let cap = if ceiling.is_finite() { ceiling } else { self::ceiling(Limit { context: UNKNOWN_WINDOW, ..limit }, buffer) };
    (context.with_model(model), cap)
}

// --- Compaction -----------------------------------------------------------------------------------------------

/// Why a compaction runs (`Trigger`).
pub enum Trigger {
    /// Before a step; `Estimate` is `estimatePrompt` of the step's context.
    Auto(Estimate),
    /// The provider just rejected the step's request as too long.
    Overflow(Estimate),
    /// The `/compact` inbox item, whose message shows the outcome.
    Manual(String),
}

/// How a compaction ended (`Outcome`).
pub enum Outcome {
    /// Only automatic compaction skips: the context fits, or automatic compaction is off.
    Skipped,
    Completed,
    Failed(Json),
}

impl Trigger {
    fn reason(&self) -> &'static str {
        if matches!(self, Trigger::Manual(_)) { "manual" } else { "auto" }
    }
}

/// `publish`: the compaction's terminal event.
fn finish(app: &App, trigger: &Trigger, context: &Context, result: Result<Summary, Json>) -> Result<Outcome, Stop> {
    let usage = spent().lock().unwrap().get(&context.session_id).copied();
    let mut data = Obj::new();
    data.str("sessionID", &context.session_id).str("reason", trigger.reason());
    match result {
        Err(failure) => {
            if let Trigger::Manual(input) = trigger {
                data.str("inputID", input);
            }
            data.set("error", failure.clone());
            if let Some(usage) = usage {
                usage.write(&mut data);
            }
            publish(app, &context.location, "session.compaction.failed", data.done(), None)?;
            Ok(Outcome::Failed(failure))
        }
        Ok(summary) => {
            data.set("model", context.model.reference.clone()).str("text", &summary.text);
            if let Some(usage) = usage {
                usage.write(&mut data);
            }
            publish(app, &context.location, "session.compaction.ended", data.done(), summary.metadata)?;
            Ok(Outcome::Completed)
        }
    }
}

async fn run(app: &Arc<App>, trigger: &Trigger, context: &Context) -> Result<Outcome, Stop> {
    let settings = settings(&context.inventory);
    // Only the user compacts when automatic compaction is off, overflow included.
    if !matches!(trigger, Trigger::Manual(_)) && !settings.auto {
        return Ok(Outcome::Skipped);
    }
    if let Trigger::Auto(estimate) = trigger {
        if !due(context, ceiling(Limit::of(&context.model), settings.buffer), Some(*estimate)) {
            return Ok(Outcome::Skipped);
        }
    }
    let held = held().lock().unwrap().remove(&context.session_id);
    let (selected, cap) = select_model(context, settings.buffer);
    // The provider just rejected this context, so the estimate ran low; the first attempt already aims below it.
    let budget = match trigger {
        Trigger::Overflow(estimate) => cap.min((estimate.total() * SHRINK_STEPS[0]).floor()),
        _ => cap,
    };
    // The runner opened the manual compaction's message when it delivered the `/compact` item. An automatic one opens
    // here, before any wait, so a background summary still being written shows as a running compaction.
    if !matches!(trigger, Trigger::Manual(_)) {
        let mut data = Obj::new();
        data.str("sessionID", &context.session_id).str("reason", "auto");
        publish(app, &context.location, "session.compaction.started", data.done(), None)?;
    }
    let swapped = match held {
        Some(held) => use_precomputed(trigger, &selected, held, budget, &settings).await,
        None => None,
    };
    let result = match swapped {
        Some(summary) => Ok(summary),
        None => summarize(app, matches!(trigger, Trigger::Overflow(_)), &selected, budget, settings.keep, None).await,
    };
    finish(app, trigger, &selected, result)
}

/// `SessionCompaction.compact`: an interrupted automatic compaction records its failure; a manual one settles through
/// its inbox item, which the runner owns.
pub async fn compact(app: &Arc<App>, trigger: &Trigger, context: &Context, cancel: &Cancel) -> Result<Outcome, Stop> {
    let _spent = SpentGuard(context.session_id.clone());
    tokio::select! {
        outcome = run(app, trigger, context) => outcome,
        _ = cancel.cancelled() => {
            if !matches!(trigger, Trigger::Manual(_)) {
                finish(app, trigger, context, Err(error("compaction.interrupted", "Compaction was interrupted")))?;
            }
            Err(Stop::Interrupted)
        }
    }
}

/// The automatic compaction before a step (`runStep`): `Completed` when the step must reload its context.
pub async fn auto(app: &Arc<App>, context: &Context, estimate: Estimate, cancel: &Cancel) -> Result<Outcome, Stop> {
    compact(app, &Trigger::Auto(estimate), context, cancel).await
}

/// A step's overflow recovery (`recoverOverflow`): whether a compaction completed, so the step can be retried on it.
pub type Recovery = Pin<Box<dyn Future<Output = Result<bool, Stop>> + Send>>;

pub fn recover_overflow(app: Arc<App>, context: Arc<Context>, estimate: Estimate, cancel: Cancel) -> Recovery {
    Box::pin(async move {
        Ok(matches!(compact(&app, &Trigger::Overflow(estimate), &context, &cancel).await?, Outcome::Completed))
    })
}

/// A manual compaction at a step boundary (`advanceToStep`): delivers the `/compact` item, opens its message, and
/// compacts the settled history. Its failure is recorded on its message and does not fail the turn; cancelling it
/// interrupts the turn.
pub async fn manual(app: &Arc<App>, session: &str, input: &str, location: &LocationRef, cancel: &Cancel) -> Result<(), Stop> {
    let mut delivered = Obj::new();
    delivered.str("sessionID", session).str("inboxID", input);
    publish(app, location, "session.inbox.delivered", delivered.done(), None)?;
    let mut started = Obj::new();
    started.str("sessionID", session).str("reason", "manual").str("inputID", input);
    publish(app, location, "session.compaction.started", started.done(), None)?;
    let work = async {
        let loaded = crate::session::runner::load(crate::session::runner::select(app, session, false).await?)?;
        let context = Context::preview(app, &loaded)?;
        compact(app, &Trigger::Manual(input.to_owned()), &context, cancel).await
    };
    let outcome = tokio::select! {
        outcome = work => outcome,
        _ = cancel.cancelled() => Err(Stop::Interrupted),
    };
    let failure = match outcome {
        Ok(_) => return Ok(()),
        Err(Stop::Interrupted) => error("aborted", "Compaction cancelled"),
        Err(Stop::Failed(failure)) => failure,
    };
    let mut data = Obj::new();
    data.str("sessionID", session).str("reason", "manual").str("inputID", input).set("error", failure);
    publish(app, location, "session.compaction.failed", data.done(), None)?;
    if cancel.is_cancelled() { Err(Stop::Interrupted) } else { Ok(()) }
}

/// `settleStaleCompactions`: a process death skips compaction finalizers; running ones fail, newest first.
pub fn settle_stale(app: &App, session: &str, location: &LocationRef) -> Result<(), Stop> {
    let running: Vec<(String, Option<String>)> = {
        let db = app.db.lock().unwrap();
        let mut statement = db
            .prepare(
                "SELECT id, json_extract(data, '$.reason') FROM session_message WHERE session_id = ?1 AND type = 'compaction' \
                 AND json_extract(data, '$.status') = 'running' ORDER BY seq DESC",
            )
            .map_err(defect)?;
        let rows = statement.query_map(params![session], |row| Ok((row.get(0)?, row.get(1)?))).map_err(defect)?;
        rows.collect::<rusqlite::Result<_>>().map_err(defect)?
    };
    for (id, reason) in running {
        let mut data = Obj::new();
        data.str("sessionID", session).str("reason", reason.as_deref().unwrap_or("auto")).str("inputID", &id);
        data.set("error", error("compaction.interrupted", "Compaction was interrupted"));
        publish(app, location, "session.compaction.failed", data.done(), None)?;
    }
    Ok(())
}

// --- Background summaries -------------------------------------------------------------------------------------

/// A background summary of everything before `keep_id`, held until automatic compaction is due (`Precomputed`).
struct Held {
    first_id: Option<String>,
    keep_id: String,
    model: Json,
    initial: String,
    /// The longest tail since `keep_id` that compaction still accepts beside this summary.
    tail_limit: f64,
    /// Background summaries that failed since the last compaction, this one included once it has.
    failures: u32,
    usage: Arc<Mutex<Vec<Usage>>>,
    result: watch::Receiver<Option<Result<Summary, Json>>>,
    task: tokio::task::AbortHandle,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Held {
    fn failed(&self) -> bool {
        matches!(&*self.result.borrow(), Some(Err(_)))
    }
}

fn held() -> &'static Mutex<HashMap<String, Held>> {
    static HELD: OnceLock<Mutex<HashMap<String, Held>>> = OnceLock::new();
    HELD.get_or_init(Default::default)
}

/// `heldIndex`: where the messages kept beside a held summary start, or `None` when it no longer describes this
/// conversation (a compaction or revert replaced its start, or the model or instructions changed).
fn held_index(held: &Held, context: &Context) -> Option<usize> {
    let first = context.messages.first().and_then(|m| json::get_str(m, "id"));
    if held.first_id.as_deref() != first || held.initial != join_initial(&context.initial) {
        return None;
    }
    let same = |key: &str| json::get_str(&held.model, key) == json::get_str(&context.model.reference, key);
    if !same("providerID") || !same("id") || !same("variant") {
        return None;
    }
    context.messages.iter().position(|m| json::get_str(m, "id") == Some(held.keep_id.as_str()))
}

/// `tailTokens`: the estimated text a swapped-in summary keeps verbatim (every message from `index` on).
fn tail_tokens(context: &Context, index: usize) -> f64 {
    tokens(&context.messages[index..].iter().map(message_text).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n"))
}

/// `usePrecomputed`: the held summary, when it still describes this conversation and the tail since it is short.
/// Waits for a summary still being written.
async fn use_precomputed(trigger: &Trigger, context: &Context, held: Held, budget: f64, settings: &Settings) -> Option<Summary> {
    let index = held_index(&held, context);
    let usable = settings.precompute
        && !matches!(trigger, Trigger::Overflow(_))
        && index.is_some_and(|index| tail_tokens(context, index) <= (budget * PRECOMPUTED_TAIL_SHARE).floor());
    if !usable {
        return None;
    }
    let mut result = held.result.clone();
    let summary = match result.wait_for(|value| value.is_some()).await.map(|value| value.clone()) {
        Ok(Some(Ok(summary))) => summary,
        _ => return None,
    };
    // Background calls were billed as they finished; the compaction's message still shows what it cost.
    for usage in held.usage.lock().unwrap().iter() {
        add_spent(&context.session_id, *usage);
    }
    let mut metadata = Obj::new();
    metadata.str("keepID", &held.keep_id);
    let mut metadata = metadata.done();
    if let Some(Json::Object(extra)) = &summary.metadata {
        for (key, value) in extra {
            json::set(&mut metadata, key, value.clone());
        }
    }
    Some(Summary { text: summary.text, metadata: Some(metadata) })
}

/// `precompute`: once the context is within `PRECOMPUTE_LEAD` of the ceiling, summarizes everything before the latest
/// response in the background (the prefix the provider just processed, so it reads from the prompt cache). The summary
/// is held, not applied, until automatic compaction is due. `estimate` is `estimatePrompt` of the context, when known.
pub fn precompute(app: &Arc<App>, context: &Context, estimate: Option<Estimate>) {
    let settings = settings(&context.inventory);
    if settings.precompute && settings.auto {
        start(app, context, estimate, &settings);
    }
}

/// `precompute` once the session is about to go idle, which leaves the background summary time to finish: the
/// context with the history as it now is.
pub fn precompute_idle(app: &Arc<App>, context: &Context) {
    let settings = settings(&context.inventory);
    if !settings.precompute || !settings.auto {
        return;
    }
    let messages = match history(&app.db.lock().unwrap(), &context.session_id, None) {
        Ok(messages) => messages,
        Err(error) => return eprintln!("failed to read the session history: {error}"),
    };
    start(app, &Context { messages: Arc::new(messages), ..context.clone() }, None, &settings);
}

fn start(app: &Arc<App>, context: &Context, estimate: Option<Estimate>, settings: &Settings) {
    let ceiling = ceiling(Limit::of(&context.model), settings.buffer);
    if !ceiling.is_finite() {
        return;
    }
    let estimate = estimate.unwrap_or_else(|| estimate_prompt(context));
    if due(context, ceiling, Some(estimate)) {
        return;
    }
    let reference = &context.model.reference;
    let messages = &context.messages;
    let first = messages.iter().find(|m| measured(m, reference));
    let cycle_start = first.and_then(tokens_of).map_or(0.0, prompt_tokens);
    if estimate.total() < ceiling - PRECOMPUTE_LEAD.min((ceiling - cycle_start) / 2.0) {
        return;
    }
    let Some(anchor_index) = messages.iter().rposition(|m| measured(m, reference)).filter(|index| *index > 0) else { return };
    let anchor = &messages[anchor_index];
    let anchor_id = json::get_str(anchor, "id").unwrap_or_default().to_owned();
    let mut map = held().lock().unwrap();
    let existing = map.get(&context.session_id);
    let failed = existing.is_some_and(Held::failed);
    let failures = existing.map_or(0, |held| held.failures) + u32::from(failed);
    if let Some(existing) = existing {
        let index = held_index(existing, context);
        if index.is_some_and(|index| !failed && tail_tokens(context, index) <= existing.tail_limit) {
            return;
        }
        // Summarizing up to the same response again would repeat the same request.
        if index.is_some() && (existing.keep_id == anchor_id || failures >= PRECOMPUTE_MAX_FAILURES) {
            return;
        }
        map.remove(&context.session_id);
    }
    // After a model switch nothing is cached for the new model yet; wait until its first response has written it.
    let model = json::get(anchor, "model");
    let same = |key: &str| model.and_then(|m| json::get_str(m, key)) == json::get_str(reference, key);
    if !same("id") || !same("variant") {
        return;
    }
    // The same once the latest response is older than the cache lifetime.
    let created = json::get(anchor, "time").and_then(|t| json::get_num(t, "created")).unwrap_or(0.0);
    if crate::id::now() as f64 - created > CACHE_LIFETIME - CACHE_MARGIN {
        return;
    }
    let (selected, cap) = select_model(context, settings.buffer);
    let usage: Arc<Mutex<Vec<Usage>>> = Arc::default();
    let background = Background { messages: messages.clone(), covered: anchor_index, usage: usage.clone() };
    let (sender, result) = watch::channel(None);
    let keep = settings.keep;
    let task = {
        let app = app.clone();
        tokio::spawn(async move {
            let summary = summarize(&app, false, &selected, cap, keep, Some(&background)).await;
            let _ = sender.send(Some(summary));
        })
    };
    map.insert(
        context.session_id.clone(),
        Held {
            first_id: messages.first().and_then(|m| json::get_str(m, "id")).map(str::to_owned),
            keep_id: anchor_id,
            model: reference.clone(),
            initial: join_initial(&context.initial),
            tail_limit: (cap * PRECOMPUTED_TAIL_SHARE).floor(),
            failures,
            usage,
            result,
            task: task.abort_handle(),
        },
    );
}

// --- Projection -----------------------------------------------------------------------------------------------

/// The newest running compaction (`getCompaction`).
fn running(tx: &Transaction, session: &str) -> rusqlite::Result<Option<Json>> {
    tx.query_row(
        "SELECT id, type, data FROM session_message WHERE session_id = ?1 AND type = 'compaction' \
         AND json_extract(data, '$.status') = 'running' ORDER BY seq DESC LIMIT 1",
        params![session],
        |row| Ok(message::from_row(row.get(0)?, row.get(1)?, &row.get::<_, String>(2)?)),
    )
    .optional()
}

/// The compaction events' projection (message-updater.ts, projector.ts): the `compaction` message, and the
/// instruction epoch a completed compaction starts (`InstructionState.advanceEpoch`).
pub fn project(tx: &Transaction, event: &Event) -> rusqlite::Result<()> {
    let data = &event.data;
    let session = json::get_str(data, "sessionID").unwrap_or_default();
    let get = |key: &str| json::get(data, key).filter(|v| !matches!(v, Json::Null)).cloned();
    let mut time = Obj::new();
    time.int("created", event.created);
    let time = time.done();
    match event.ty {
        "session.compaction.started" => {
            let mut o = Obj::new();
            o.str("id", &json::get_str(data, "inputID").map(str::to_owned).unwrap_or_else(|| message::id_from_event(&event.id)));
            o.str("type", "compaction").str("status", "running");
            o.opt("metadata", event.metadata.clone());
            o.opt("reason", get("reason")).str("summary", "").set("time", time);
            message::insert(tx, event, &o.done())
        }
        "session.compaction.ended" => {
            let current = running(tx, session)?;
            let metadata = match (current.as_ref().and_then(|c| json::get(c, "metadata")).cloned(), event.metadata.clone()) {
                (Some(Json::Object(mut current)), Some(Json::Object(extra))) => {
                    for (key, value) in extra {
                        current.insert(key, value);
                    }
                    Some(Json::Object(current))
                }
                (current, None) if current.is_some() => current,
                (_, extra) => extra,
            };
            let mut o = Obj::new();
            let id = current.as_ref().and_then(|c| json::get_str(c, "id")).map(str::to_owned).unwrap_or_else(|| message::id_from_event(&event.id));
            o.str("id", &id).str("type", "compaction").str("status", "completed");
            o.opt("metadata", metadata).opt("reason", get("reason")).opt("model", get("model")).opt("native", get("native"));
            o.str("summary", json::get_str(data, "text").unwrap_or_default());
            o.opt("cost", get("cost")).opt("tokens", get("tokens"));
            o.set("time", current.as_ref().and_then(|c| json::get(c, "time")).cloned().unwrap_or(time));
            let completed = o.done();
            match current {
                Some(_) => message::update(tx, session, &completed)?,
                None => message::insert(tx, event, &completed)?,
            }
            let seq = event.durable.as_ref().map_or(0, |d| d.seq);
            tx.execute(
                "UPDATE instruction_state SET epoch_start = ?1, through_seq = ?1, initial_values = current_values WHERE session_id = ?2",
                params![seq, session],
            )?;
            Ok(())
        }
        "session.compaction.failed" => {
            let current = running(tx, session)?;
            let id = current
                .as_ref()
                .and_then(|c| json::get_str(c, "id"))
                .or_else(|| json::get_str(data, "inputID"))
                .map(str::to_owned)
                .unwrap_or_else(|| message::id_from_event(&event.id));
            let mut o = Obj::new();
            o.str("id", &id).str("type", "compaction").str("status", "failed");
            o.opt("metadata", current.as_ref().and_then(|c| json::get(c, "metadata")).cloned().or_else(|| event.metadata.clone()));
            o.opt("reason", get("reason")).opt("error", get("error")).opt("cost", get("cost")).opt("tokens", get("tokens"));
            o.set("time", current.as_ref().and_then(|c| json::get(c, "time")).cloned().unwrap_or(time));
            let failed = o.done();
            match current {
                Some(_) => message::update(tx, session, &failed),
                None => message::insert(tx, event, &failed),
            }
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_round_like_js() {
        assert_eq!(tokens(""), 0.0);
        assert_eq!(tokens("ab"), 1.0);
        assert_eq!(tokens("abcdef"), 2.0);
        assert_eq!(tokens("And what does add do?"), 5.0);
    }

    #[test]
    fn ceilings() {
        let limit = Limit { context: 1_048_576.0, input: 0.0 };
        assert_eq!(ceiling(limit, None), 891_290.0);
        assert_eq!(ceiling(limit, Some(Buffer::Share(50.0))), 524_288.0);
        assert_eq!(ceiling(limit, Some(Buffer::Tokens(500_000.0))), 548_576.0);
        assert_eq!(summary_room(limit, Some(157_286.0)), 1_032_576.0);
    }

    #[test]
    fn templates_are_recognized() {
        assert!(filled("## Objective\n- x"));
        assert!(filled("  ### Completed  "));
        assert!(!filled("Here is a summary without the template."));
    }

    #[test]
    fn clips_long_text() {
        assert_eq!(clip("short"), "short");
        let long = "a".repeat(2_500);
        let clipped = clip(&long);
        assert!(clipped.contains("[… 500 characters omitted …]"));
    }
}
