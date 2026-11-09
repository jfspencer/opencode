//! The goal loop (packages/core/src/plugin/goal.ts, a built-in plugin; schema/src/goal.ts): `/goal <objective>` arms a
//! session goal and kicks the session off with a synthetic notice (a prompt when it carries attachments). Whenever the
//! session would go idle, [`idle`] asks a reviewer (a one-shot generation over the session's context) for a verdict:
//! `done` completes the goal, `continue` admits a continuation that keeps the same execution working (unless the loop
//! stalls: no tool calls since the last kickoff or continuation, or the reviewer's reason repeated, three rounds in a
//! row), `wait` leaves it waiting on a background shell, anything else pauses it. `/goal pause`, `resume` and `clear`
//! steer it; a failed or user-interrupted execution pauses it; a goal left waiting for 30 minutes with nothing running
//! in the session or below it pauses; a goal armed by an earlier process pauses when it is next seen.
//!
//! The state lives in the plugin's storage (the `kv` table, under the plugin's namespace), so children and forks start
//! without one. Clients read it through the plugin's RPC (`POST /api/rpc/opencode.goal/get`, [`rpc`]) and follow the
//! `rpc.opencode.goal.changed` events every write publishes in the session's location. Until a continuation restates
//! the objective, a step's request that does not show it gets a reminder near its tail ([`remind`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use opencode_protocol::Json;

use crate::app::App;
use crate::bus::{LocationRef, PublishOptions};
use crate::http::{Failure, Result};
use crate::json::{self, Obj};
use crate::llm::{Message, UserPart};

const PLUGIN: &str = "opencode.goal";
const PREFIX: &str = "session:";
const CHANGED: &str = "rpc.opencode.goal.changed";
const MAX_OBJECTIVE: usize = 4_000;
const MAX_ROUNDS: f64 = 50.0;
const MAX_STALLS: f64 = 3.0;
/// How long an active goal may sit idle, waiting on background work that never reports, before it pauses.
const WAIT_LIMIT: Duration = Duration::from_secs(30 * 60);
const USAGE: &str = "Usage: /goal <objective> | pause | resume | clear";

const CONTINUATION: &str = include_str!("goal/continuation.txt");
const JUDGE: &str = include_str!("goal/judge.txt");
const KICKOFF: &str = include_str!("goal/kickoff.txt");
const REMINDER: &str = include_str!("goal/reminder.txt");

/// Identifies this server process: a goal armed by an earlier one comes back paused instead of resuming on its own.
fn boot() -> &'static str {
    static BOOT: OnceLock<String> = OnceLock::new();
    BOOT.get_or_init(crate::id::uuid)
}

/// The plugin's storage namespace (`storage` in plugin/host.ts): `plugin:` and the ID's UTF-16 units in hex.
fn namespace() -> String {
    let hex: String = PLUGIN.encode_utf16().map(|unit| format!("{unit:04x}")).collect();
    format!("plugin:{hex}:")
}

fn key(session: &str) -> String {
    format!("{}{PREFIX}{session}", namespace())
}

/// `Goal.State`'s schema: a stored value that does not decode is no goal.
fn decode(value: Json) -> Option<Json> {
    let string = |k: &str| matches!(json::get(&value, k), Some(Json::String(_)));
    let number = |k: &str| matches!(json::get(&value, k), Some(Json::Number(_)));
    let optional = |k: &str, ok: fn(&Json) -> bool| json::get(&value, k).is_none_or(ok);
    let valid = matches!(value, Json::Object(_))
        && string("id")
        && string("objective")
        && matches!(json::get_str(&value, "status"), Some("active" | "paused" | "complete"))
        && optional("reason", |v| matches!(v, Json::String(_)))
        && number("rounds")
        && number("maxRounds")
        && optional("stalls", |v| matches!(v, Json::Number(_)))
        && string("boot")
        && number("updatedAt");
    valid.then_some(value)
}

fn read(app: &App, session: &str) -> Option<Json> {
    let db = app.db.lock().unwrap();
    let text: Option<String> =
        db.query_row("SELECT value FROM kv WHERE key = ?1", [key(session)], |row| row.get(0)).ok();
    text.and_then(|text| json::parse(&text)).and_then(decode)
}

fn store(app: &App, session: &str, goal: Option<&Json>) {
    let db = app.db.lock().unwrap();
    let result = match goal {
        Some(goal) => {
            let now = crate::id::now();
            db.execute(
                "INSERT INTO kv (key, value, time_created, time_updated) VALUES (?1, ?2, ?3, ?3) \
                 ON CONFLICT (key) DO UPDATE SET value = ?2, time_updated = ?3",
                rusqlite::params![key(session), json::text(goal), now],
            )
        }
        None => db.execute("DELETE FROM kv WHERE key = ?1", [key(session)]),
    };
    if let Err(error) = result {
        eprintln!("failed to store a goal: {error}");
    }
}

/// The location the session's plugin instance publishes in.
fn location_of(app: &App, session: &str) -> Option<LocationRef> {
    crate::session::get(&app.db.lock().unwrap(), session).ok().flatten().map(|stored| stored.location)
}

/// `write`: every write publishes the new value, so clients that read it once stay current.
fn write(app: &App, session: &str, goal: Option<Json>) {
    store(app, session, goal.as_ref());
    let mut data = Obj::new();
    data.str("sessionID", session).set("goal", goal.unwrap_or(Json::Null));
    app.bus.emit(CHANGED, data.done(), PublishOptions { id: None, location: location_of(app, session) });
}

/// `replace`: the loop's own writes only apply to the goal they were decided on; a `/goal pause`, `clear` or new goal
/// that landed since wins.
fn replace(app: &App, session: &str, expected: &Json, next: Json) -> bool {
    let current = read(app, session);
    let same = current.as_ref().is_some_and(|current| {
        ["id", "updatedAt", "status"].iter().all(|k| json::get(current, k) == json::get(expected, k))
    });
    if !same {
        return false;
    }
    write(app, session, Some(next));
    true
}

fn with(goal: &Json, entries: &[(&str, Json)]) -> Json {
    let mut out = goal.clone();
    for (k, v) in entries {
        json::set(&mut out, k, v.clone());
    }
    out
}

fn now() -> Json {
    Json::Number(crate::id::now() as f64)
}

fn settle(app: &App, session: &str, goal: &Json, status: &str, reason: &str) {
    let next = with(goal, &[("status", json::string(status)), ("reason", json::string(reason)), ("updatedAt", now())]);
    replace(app, session, goal, next);
}

fn active(goal: &Option<Json>) -> bool {
    goal.as_ref().and_then(|g| json::get_str(g, "status")) == Some("active")
}

fn pause_active(app: &App, session: &str, reason: &str) {
    let goal = read(app, session);
    if active(&goal) {
        settle(app, session, goal.as_ref().unwrap(), "paused", reason);
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------------------------------------------------

/// One pass, so an objective that contains a placeholder is not expanded again.
fn render(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'));
        match end.filter(|&end| end > 0 && after[end..].starts_with('}')) {
            Some(end) => {
                let name = &after[..end];
                match values.iter().find(|(k, _)| *k == name) {
                    Some((_, value)) => out.push_str(value),
                    None => out.push_str(&rest[start..start + 2 + end + 1]),
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str("${");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn objective_of(goal: &Json) -> &str {
    json::get_str(goal, "objective").unwrap_or_default()
}

fn continuation(goal: &Json, feedback: &[Option<String>]) -> String {
    let feedback: Vec<&str> = feedback.iter().flatten().map(String::as_str).filter(|f| !f.is_empty()).collect();
    render(CONTINUATION, &[("objective", &escape(objective_of(goal))), ("feedback", &feedback.join("\n"))])
}

/// Every goal prompt wraps the objective the same way, so this marks the objective as visible to the model.
fn objective_block(objective: &str) -> String {
    format!("<untrusted_objective>\n{}\n</untrusted_objective>", escape(objective))
}

/// `text.replace(/\s+/g, " ")`.
fn collapse(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if crate::text::is_js_whitespace(c) {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

fn same_reason(previous: Option<&str>, next: &str) -> bool {
    let normalize = |text: &str| collapse(&crate::text::trim(text).to_lowercase());
    previous.is_some_and(|previous| normalize(previous) == normalize(next))
}

fn describe(goal: &Json) -> String {
    let status = match json::get_str(goal, "status") {
        Some("active") => "Goal active",
        Some("paused") => "Goal paused",
        _ => "Goal achieved",
    };
    let mut lines = vec![format!("{status}: {}", objective_of(goal))];
    lines.extend(json::get_str(goal, "reason").filter(|r| !r.is_empty()).map(str::to_owned));
    lines.join("\n")
}

/// `truncate`: whitespace collapsed, at most `length` UTF-16 units with an ellipsis.
fn truncate(text: &str, length: usize) -> String {
    let line = collapse(text);
    let units: Vec<u16> = line.encode_utf16().collect();
    if units.len() <= length {
        return line;
    }
    format!("{}…", String::from_utf16_lossy(&units[..length - 1]))
}

/// `/goal "Ship the release"` and `/goal Ship the release` set the same objective.
fn unquote(text: &str) -> String {
    let mut chars = text.chars();
    let (Some(first), Some(last)) = (chars.next(), chars.next_back()) else { return text.to_owned() };
    let opening = ['"', '\'', '“', '‘'];
    let closing = ['"', '\'', '”', '’'];
    if !opening.contains(&first) || !closing.contains(&last) {
        return text.to_owned();
    }
    let pair = match first {
        '"' => '"',
        '\'' => '\'',
        '“' => '”',
        _ => '’',
    };
    if pair != last {
        return text.to_owned();
    }
    crate::text::trim(chars.as_str()).to_owned()
}

/// A reviewer's answer (`Verdict`).
struct Verdict {
    verdict: String,
    reason: String,
    next_step: Option<String>,
}

/// `parseVerdict`: reviewers sometimes wrap the JSON in a code fence or add a sentence around it.
fn parse_verdict(text: &str) -> Option<Verdict> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    let value = json::parse(&text[start..=end])?;
    let verdict = json::get_str(&value, "verdict").filter(|v| ["done", "continue", "blocked", "paused", "wait"].contains(v))?;
    let reason = json::get_str(&value, "reason")?;
    let next_step = match json::get(&value, "next_step") {
        None => None,
        Some(Json::String(step)) => Some(step.clone()),
        Some(_) => return None,
    };
    Some(Verdict { verdict: verdict.to_owned(), reason: reason.to_owned(), next_step })
}

// ---------------------------------------------------------------------------------------------------------------------
// Session operations
// ---------------------------------------------------------------------------------------------------------------------

/// `ctx.session.synthetic`: input admitted in the session's location, which wakes the session.
fn synthetic(app: &App, session: &str, text: &str, description: Option<&str>, goal_id: &str, delivery: &str) -> Result<()> {
    let mut metadata = Obj::new();
    metadata.str("source", "goal").str("goalID", goal_id);
    let mut payload = Obj::new();
    payload.str("text", text).opt_str("description", description).set("metadata", metadata.done());
    let mut item = Obj::new();
    item.str("type", "synthetic").set("payload", payload.done()).str("delivery", delivery);
    let id = crate::session::message::create_id();
    let _ = crate::session::inbox::admit(app, &id, session, item.done(), location_of(app, session))?;
    let reverted = crate::session::get(&app.db.lock().unwrap(), session)
        .ok()
        .flatten()
        .is_some_and(|s| json::get(&s.info, "revert").is_some());
    if !reverted {
        app.sessions.executions.wake(app, session, crate::session::inbox::Promotable::Input);
    }
    Ok(())
}

/// `usedTools`: whether the model called a tool since the goal's last kickoff or continuation. Without that message in
/// view (after a compaction), the round counts as progress.
fn used_tools(app: &App, session: &str, goal_id: &str) -> bool {
    let history = crate::session::message::history(&app.db.lock().unwrap(), session).unwrap_or_default();
    let start = history.iter().rposition(|message| {
        matches!(json::get_str(message, "type"), Some("synthetic" | "user"))
            && json::get(message, "metadata").and_then(|m| json::get_str(m, "goalID")) == Some(goal_id)
    });
    let Some(start) = start else { return true };
    history[start + 1..].iter().any(|message| {
        json::get_str(message, "type") == Some("assistant")
            && matches!(json::get(message, "content"), Some(Json::Array(parts)) if parts.iter().any(|p| json::get_str(p, "type") == Some("tool")))
    })
}

enum Judged {
    Verdict(Option<Verdict>),
    Overflow,
}

/// `judge`: the reviewer's verdict, asked once more when the first answer is unusable. "wait" is only offered while a
/// shell runs; without one, nothing would wake the session to judge again.
async fn judge(app: &Arc<App>, session: &str, goal: &Json, shells: &[String]) -> std::result::Result<Judged, String> {
    let background = if shells.is_empty() {
        "No background shells are running.".to_owned()
    } else {
        let mut lines = vec!["Background shells still running:".to_owned()];
        lines.extend(shells.iter().map(|command| format!("- {command}")));
        lines.push(String::new());
        lines.push("Answer \"wait\" only when the goal needs the result of one of these shells. Never wait for a long-running server such as a dev server.".to_owned());
        lines.join("\n")
    };
    let wait = if shells.is_empty() { "" } else { "{\"verdict\":\"wait\",\"reason\":\"<the running shell whose result the goal needs>\"}\n" };
    let prompt = render(JUDGE, &[("objective", &escape(objective_of(goal))), ("background", &background), ("wait", wait)]);
    for _ in 0..2 {
        match crate::generate::session_text(app, session, &prompt).await {
            Ok(text) => {
                let verdict = parse_verdict(&text).filter(|v| !(v.verdict == "wait" && shells.is_empty()));
                if verdict.is_some() {
                    return Ok(Judged::Verdict(verdict));
                }
            }
            // The reviewer runs right after the last step, before the next step's automatic compaction, so a long
            // final step can leave more history than the model takes.
            Err(Err(error)) if error.context_overflow() => return Ok(Judged::Overflow),
            Err(Err(error)) => return Err(error.message),
            Err(Ok(message)) => return Err(message),
        }
    }
    Ok(Judged::Verdict(None))
}

/// One compaction per round when the reviewer's request is too large; a second overflow pauses instead.
fn compacted() -> &'static Mutex<HashMap<String, String>> {
    static COMPACTED: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    COMPACTED.get_or_init(Default::default)
}

/// The `experimental.idle` hook: the runner calls it once the session would go idle with nothing pending; input it
/// admits continues the same execution. A failure pauses the goal.
pub async fn idle(app: &Arc<App>, session: &str) {
    if let Err(error) = round(app, session).await {
        eprintln!("goal continuation failed: {error}");
        pause_active(app, session, "The goal stopped after an error");
    }
}

async fn round(app: &Arc<App>, session: &str) -> std::result::Result<(), String> {
    let Some(goal) = read(app, session).filter(|g| json::get_str(g, "status") == Some("active")) else { return Ok(()) };
    if json::get_str(&goal, "boot") != Some(boot()) {
        settle(app, session, &goal, "paused", "The server restarted while the goal was active");
        return Ok(());
    }
    // The session's own running subagent or image job wakes it with its notice, and the hook runs again then. A
    // grandchild reports to its own parent, so it would never wake this session.
    let background = app.sessions.jobs.running_background();
    if background.iter().any(|work| matches!(work.kind(), "subagent" | "image") && work.owner() == session) {
        return Ok(());
    }
    let rounds = json::get_num(&goal, "rounds").unwrap_or(0.0);
    let max = json::get_num(&goal, "maxRounds").unwrap_or(MAX_ROUNDS);
    if rounds >= max {
        settle(app, session, &goal, "paused", &format!("Reached the limit of {} rounds", crate::json::text(&Json::Number(max))));
        return Ok(());
    }
    // Only the session's own shells wake it when they finish.
    let shells: Vec<String> = background
        .iter()
        .filter_map(|work| match work {
            crate::job::Recovery::Shell { session_id, command, .. } if session_id == session => Some(command.clone()),
            _ => None,
        })
        .collect();
    let judged = judge(app, session, &goal, &shells).await?;
    if !matches!(judged, Judged::Overflow) {
        compacted().lock().unwrap().remove(session);
    }
    // `/goal pause`, `clear` or a new goal may have landed while the reviewer ran.
    let Some(latest) = read(app, session) else { return Ok(()) };
    if json::get(&latest, "id") != json::get(&goal, "id") || json::get_str(&latest, "status") != Some("active") {
        return Ok(());
    }
    let verdict = match judged {
        Judged::Overflow => {
            let round = format!("{}:{}", json::get_str(&latest, "id").unwrap_or_default(), crate::json::text(json::get(&latest, "rounds").unwrap_or(&Json::Null)));
            let mut compactions = compacted().lock().unwrap();
            if compactions.get(session) == Some(&round) {
                compactions.remove(session);
                drop(compactions);
                settle(app, session, &latest, "paused", "The goal reviewer's request is too large for the model");
                return Ok(());
            }
            compactions.insert(session.to_owned(), round);
            drop(compactions);
            // The runner compacts before going idle, then runs this hook again on the smaller history.
            let id = crate::session::message::create_id();
            crate::session::inbox::admit_compaction(app, &id, session, "steer").map_err(|f| json::text(&f.body))?.map_err(|_| "compaction conflict".to_owned())?;
            return Ok(());
        }
        Judged::Verdict(None) => {
            settle(app, session, &latest, "paused", "The goal reviewer gave no usable verdict");
            return Ok(());
        }
        Judged::Verdict(Some(verdict)) => verdict,
    };
    match verdict.verdict.as_str() {
        "wait" => return Ok(()),
        "done" => {
            settle(app, session, &latest, "complete", &verdict.reason);
            return Ok(());
        }
        "continue" => {}
        _ => {
            settle(app, session, &latest, "paused", &verdict.reason);
            return Ok(());
        }
    }
    let id = json::get_str(&latest, "id").unwrap_or_default().to_owned();
    let stalled = same_reason(json::get_str(&latest, "reason"), &verdict.reason) || !used_tools(app, session, &id);
    let stalls = if stalled { json::get_num(&latest, "stalls").unwrap_or(0.0) + 1.0 } else { 0.0 };
    if stalls >= MAX_STALLS {
        settle(app, session, &latest, "paused", &format!("Not making progress: {}", verdict.reason));
        return Ok(());
    }
    let next = with(
        &latest,
        &[
            ("rounds", Json::Number(json::get_num(&latest, "rounds").unwrap_or(0.0) + 1.0)),
            ("stalls", Json::Number(stalls)),
            ("reason", json::string(&verdict.reason)),
            ("updatedAt", now()),
        ],
    );
    if !replace(app, session, &latest, next.clone()) {
        return Ok(());
    }
    // No description: timelines hide the continuation, and the pinned goal shows that the loop is still running.
    let feedback = [Some(verdict.reason.clone()), verdict.next_step.as_ref().map(|step| format!("Next step: {step}"))];
    synthetic(app, session, &continuation(&next, &feedback), None, &id, "steer").map_err(|f| json::text(&f.body))
}

/// The `context` hook: until the next continuation restates the objective, a compacted history may not show it, so
/// a reminder goes near the tail (not persisted, so the cached prefix stays warm).
pub fn remind(app: &App, session: &str, messages: &mut Vec<Message>) {
    let goal = read(app, session);
    if !active(&goal) {
        return;
    }
    let objective = objective_of(goal.as_ref().unwrap()).to_owned();
    let block = objective_block(&objective);
    let shown = messages.iter().any(|message| match message {
        Message::User(parts) => parts
            .iter()
            .filter_map(|part| if let UserPart::Text(text) = part { Some(text.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("\n")
            .contains(&block),
        _ => false,
    });
    if shown {
        return;
    }
    let at = if matches!(messages.last(), Some(Message::User(_))) { messages.len() - 1 } else { messages.len() };
    messages.insert(at, Message::User(vec![UserPart::Text(render(REMINDER, &[("objective", &escape(&objective))]))]));
}

// ---------------------------------------------------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------------------------------------------------

/// The `goal` command: `input` has the command payload's shape (`text`, `files?`, `agents?`, `skills?`, `delivery?`).
/// `Err` is the message of a failed command.
pub async fn command(app: &App, session: &str, input: &Json) -> std::result::Result<(), CommandError> {
    let text = unquote(crate::text::trim(json::get_str(input, "text").unwrap_or_default()));
    let control = text.to_lowercase();
    let goal = read(app, session);
    if text.is_empty() {
        return Err(CommandError::Message(match &goal {
            Some(goal) => format!("{}\n{USAGE}", describe(goal)),
            None => USAGE.to_owned(),
        }));
    }
    match control.as_str() {
        "pause" => {
            let Some(goal) = goal.filter(|g| json::get_str(g, "status") == Some("active")) else {
                return Err(CommandError::Message("No active goal to pause.".into()));
            };
            // The user's command wins over the loop, so it writes without the loop's guard.
            let paused = with(&goal, &[("status", json::string("paused")), ("reason", json::string("Paused by the user")), ("updatedAt", now())]);
            write(app, session, Some(paused));
            return Ok(());
        }
        "resume" => {
            let Some(goal) = goal.filter(|g| json::get_str(g, "status") == Some("paused")) else {
                return Err(CommandError::Message("No paused goal to resume.".into()));
            };
            let mut resumed = Obj::new();
            resumed
                .set("id", json::get(&goal, "id").cloned().unwrap_or(Json::Null))
                .set("objective", json::get(&goal, "objective").cloned().unwrap_or(Json::Null))
                .str("status", "active")
                // The resume continuation is the first round of the new budget.
                .set("rounds", Json::Number(1.0))
                .set("maxRounds", json::get(&goal, "maxRounds").cloned().unwrap_or(Json::Number(MAX_ROUNDS)))
                .str("boot", boot())
                .set("updatedAt", now());
            let resumed = resumed.done();
            write(app, session, Some(resumed.clone()));
            let id = json::get_str(&resumed, "id").unwrap_or_default().to_owned();
            let text = continuation(&resumed, &[Some("The user resumed the goal.".to_owned())]);
            return synthetic(app, session, &text, None, &id, "steer").map_err(CommandError::Failure);
        }
        "clear" => {
            if goal.is_none() {
                return Err(CommandError::Message("No goal is set.".into()));
            }
            write(app, session, None);
            return Ok(());
        }
        _ => {}
    }
    let length = text.encode_utf16().count();
    if length > MAX_OBJECTIVE {
        return Err(CommandError::Message(format!("A goal objective is limited to {MAX_OBJECTIVE} characters (got {length}).")));
    }
    let id = crate::id::uuid();
    let mut created = Obj::new();
    created
        .str("id", &id)
        .str("objective", &text)
        .str("status", "active")
        .set("rounds", Json::Number(0.0))
        .set("maxRounds", Json::Number(MAX_ROUNDS))
        .str("boot", boot())
        .set("updatedAt", now());
    write(app, session, Some(created.done()));
    // Titles come from the first user message, and a notice kickoff is not one, so name the session here.
    let stored = crate::session::require(&app.db.lock().unwrap(), session).map_err(CommandError::Failure)?;
    // `isExactRootFallback`: no title, or the root fallback for this session.
    if crate::session::title::untitled(&stored.info) {
        let mut data = Obj::new();
        data.str("sessionID", session).str("title", &truncate(&text, 60));
        crate::session::publish_event(app, "session.renamed", data.done(), Some(stored.location.clone()), None)
            .map_err(|e| CommandError::Failure(crate::app::fatal(e)))?;
    }
    let kickoff = render(KICKOFF, &[("objective", &escape(&text))]);
    let delivery = json::get_str(input, "delivery").unwrap_or("steer");
    let attached = ["files", "agents", "skills"]
        .iter()
        .any(|k| matches!(json::get(input, k), Some(Json::Array(items)) if !items.is_empty()));
    // Timelines show a described synthetic message as one notice row instead of the whole kickoff. Only a prompt can
    // carry attachments, so those still go through as the user's message.
    if attached {
        let mut prompt = Obj::new();
        prompt.str("text", &kickoff);
        for k in ["files", "agents", "skills"] {
            prompt.opt(k, json::get(input, k).filter(|v| !matches!(v, Json::Null)).cloned());
        }
        let mut metadata = Obj::new();
        metadata.str("source", "goal").str("goalID", &id);
        prompt.set("metadata", metadata.done()).str("delivery", delivery);
        crate::session::prompt::submit(app, session, &prompt.done()).await.map_err(CommandError::Failure)?;
        return Ok(());
    }
    synthetic(app, session, &kickoff, Some(&format!("Goal: {}", truncate(&text, 120))), &id, delivery).map_err(CommandError::Failure)
}

/// How the command failed: its own message (`Command.ExecutionError`), or an API failure passed through.
pub enum CommandError {
    Message(String),
    Failure(Failure),
}

// ---------------------------------------------------------------------------------------------------------------------
// RPC and events
// ---------------------------------------------------------------------------------------------------------------------

fn rpc_error(ty: &str, message: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "RpcError").str("type", ty).str("message", message);
    Failure::new(400, o.done())
}

/// `POST /api/rpc/:rpcID/:method` (`Rpc.call`): the goal plugin registers `opencode.goal` with one method, `get`.
pub fn rpc(app: &App, rpc_id: &str, method: &str, payload: &Json) -> Result<Json> {
    if rpc_id != PLUGIN {
        return Err(rpc_error("rpc.unavailable", &format!("RPC is unavailable: {rpc_id}")));
    }
    if method != "get" {
        return Err(rpc_error("rpc.method_not_found", &format!("Unknown RPC method: {rpc_id}.{method}")));
    }
    let input = json::get(payload, "input").filter(|v| !matches!(v, Json::Null));
    let Some(input @ Json::Object(_)) = input else { return Err(rpc_error("rpc.invalid_input", "Expected object")) };
    let session = match json::get(input, "sessionID") {
        None => return Err(rpc_error("rpc.invalid_input", "Missing key\n  at [\"sessionID\"]")),
        Some(Json::String(session)) => session.clone(),
        Some(_) => return Err(rpc_error("rpc.invalid_input", "Expected string\n  at [\"sessionID\"]")),
    };
    let mut o = Obj::new();
    o.set("output", read(app, &session).unwrap_or(Json::Null));
    Ok(o.done())
}

fn timers() -> &'static Mutex<HashMap<String, tokio::task::AbortHandle>> {
    static TIMERS: OnceLock<Mutex<HashMap<String, tokio::task::AbortHandle>>> = OnceLock::new();
    TIMERS.get_or_init(Default::default)
}

fn remove_timer(session: &str) {
    if let Some(timer) = timers().lock().unwrap().remove(session) {
        timer.abort();
    }
}

/// `watch`: an active goal only sits idle while it waits on background work, whose notice wakes the session. If
/// nothing runs in the session or below it for the wait limit, the goal pauses.
fn watch(app: &Arc<App>, session: &str) {
    let Some(goal) = read(app, session).filter(|g| json::get_str(g, "status") == Some("active")) else { return };
    let (app, owned) = (app.clone(), session.to_owned());
    let task = tokio::spawn(async move {
        tokio::time::sleep(WAIT_LIMIT).await;
        timers().lock().unwrap().remove(&owned);
        let minutes = WAIT_LIMIT.as_secs() / 60;
        settle(&app, &owned, &goal, "paused", &format!("Nothing ran for {minutes} minutes while the goal waited on background work"));
    });
    if let Some(previous) = timers().lock().unwrap().insert(session.to_owned(), task.abort_handle()) {
        previous.abort();
    }
}

/// `touch`: a step anywhere below a waiting goal restarts its timer.
fn touch(app: &Arc<App>, session: &str) {
    if timers().lock().unwrap().is_empty() {
        return;
    }
    let parent = |id: &str| {
        crate::session::get(&app.db.lock().unwrap(), id)
            .ok()
            .flatten()
            .and_then(|s| json::get_str(&s.info, "parentID").map(str::to_owned))
    };
    let mut current = parent(session);
    while let Some(id) = current {
        if timers().lock().unwrap().contains_key(&id) {
            watch(app, &id);
            return;
        }
        current = parent(&id);
    }
}

fn on_event(app: &Arc<App>, event: &crate::bus::Event) {
    let Some(session) = json::get_str(&event.data, "sessionID").map(str::to_owned) else { return };
    match event.ty {
        "session.deleted" => {
            remove_timer(&session);
            store(app, &session, None);
        }
        "session.step.started" | "session.step.ended" => touch(app, &session),
        "session.execution.started" => remove_timer(&session),
        "session.execution.succeeded" => watch(app, &session),
        // Shutdown keeps the goal active; the next process sees the boot change and pauses it.
        "session.execution.interrupted" if json::get_str(&event.data, "reason") != Some("user") => {}
        "session.execution.interrupted" => pause_active(app, &session, "Stopped by the user"),
        "session.execution.failed" => {
            let message = json::get(&event.data, "error").and_then(|e| json::get_str(e, "message")).unwrap_or_default();
            pause_active(app, &session, &format!("The session failed: {message}"));
        }
        _ => {}
    }
}

/// Starts the plugin once per process: it follows the session events it reacts to, and pauses the goals an earlier
/// process left active (it assumes one server process per database). Goals of sessions deleted meanwhile are dropped.
pub fn start(app: &App) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    let Some(app) = app.me.upgrade() else { return };
    STARTED.call_once(|| {
        let mut events = app.bus.subscribe();
        let listener = app.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => on_event(&listener, &event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let prefix = format!("{}{PREFIX}", namespace());
        let entries: Vec<(String, String)> = {
            let db = app.db.lock().unwrap();
            let statement = db.prepare("SELECT key, value FROM kv WHERE substr(key, 1, ?2) = ?1 ORDER BY key");
            match statement {
                Ok(mut statement) => statement
                    .query_map(rusqlite::params![prefix, prefix.len() as i64], |row| Ok((row.get(0)?, row.get(1)?)))
                    .map(|rows| rows.flatten().collect())
                    .unwrap_or_default(),
                Err(_) => vec![],
            }
        };
        for (key, value) in entries {
            let session = &key[prefix.len()..];
            let exists = crate::session::get(&app.db.lock().unwrap(), session).ok().flatten().is_some();
            if !exists {
                store(&app, session, None);
                continue;
            }
            let goal = json::parse(&value).and_then(decode);
            if let Some(goal) = goal.filter(|g| json::get_str(g, "status") == Some("active") && json::get_str(g, "boot") != Some(boot())) {
                settle(&app, session, &goal, "paused", "The server restarted while the goal was active");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_helpers() {
        assert_eq!(namespace(), "plugin:006f00700065006e0063006f00640065002e0067006f0061006c:");
        assert_eq!(unquote("\"Ship it\""), "Ship it");
        assert_eq!(unquote("“Ship it”"), "Ship it");
        assert_eq!(unquote("\"Ship it'"), "\"Ship it'");
        assert_eq!(render("a ${x} ${y} $ {x}", &[("x", "${y}")]), "a ${y} ${y} $ {x}");
        assert_eq!(truncate("a  b\nc", 4), "a b…");
        assert!(same_reason(Some("Read it again."), "  read it AGAIN. "));
        assert!(parse_verdict("```json\n{\"verdict\":\"done\",\"reason\":\"x\"}\n```").is_some());
        assert!(parse_verdict("{\"verdict\":\"maybe\",\"reason\":\"x\"}").is_none());
    }
}
