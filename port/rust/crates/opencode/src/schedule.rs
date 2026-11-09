//! Scheduled prompts (packages/core/src/schedule.ts, schedule/sql.ts, packages/schema/src/schedule.ts): a schedule
//! wakes its Session with a prompt `in` a duration, `at` a time, `every` interval or on a `cron` expression, optionally
//! gated by a condition ([`check`]) checked with no model call: `if` gates every slot, `until` polls and wakes once.
//! Schedules follow `experimental.schedules` of the config where their Session lives.
//!
//! [`run`] is the loop that fires them (`Schedule.run`): at most one process per database leads it, through a lease
//! row in `kv`; it sleeps until the next schedule falls due (at most 30 s), or until a create or resume rings
//! [`doorbell`]. A firing admits the prompt with an ID derived from the run count (`msg_<schedule>_<run>`), so a retry
//! after a crash reconciles with the earlier admission.
//!
//! Out of scope: the schedule tools' execution (tool/plugin/schedule.ts, `schedule_*` in the Code Mode catalog) and
//! the `/schedule` command, which exist only while the experimental `schedules` flag is on.

pub mod check;
pub mod cron;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use opencode_protocol::Json;
use opencode_protocol::wire::codec::Duration;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row as SqlRow, params, params_from_iter};
use tokio::sync::Notify;

use crate::app::{App, fatal};
use crate::http::Failure;
use crate::json::{self, Obj};
use crate::session::{self, Stored};
use crate::zone::Zone;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const MIN_INTERVAL: i64 = MINUTE;
const MIN_CHECK_INTERVAL: i64 = 15_000;
const DEFAULT_UNTIL_AFTER: i64 = 24 * HOUR;
/// A firing this far past its due time is marked late.
const LATE_AFTER: i64 = 60_000;
/// The loop re-reads the table at least this often, so clock jumps and missed doorbells are caught.
const MAX_SLEEP: i64 = 30_000;
/// How many due schedules a tick fires at once.
const TICK_CONCURRENCY: usize = 4;
const LEADER_KEY: &str = "schedule:leader";
const LEADER_TTL: i64 = 2 * MAX_SLEEP;
/// How long a due schedule waits before its location's flag is read again, while schedules are off there.
const DISABLED_RETRY: i64 = HOUR;
/// How long a schedule whose prompt failed waits before trying again.
const FAILED_RETRY: i64 = 5 * MINUTE;
/// How long a schedule whose check failed waits before trying again, up to MAX_CHECK_FAILURES.
const CHECK_FAILED_RETRY: i64 = MINUTE;
const MAX_CHECK_FAILURES: i64 = 3;

/// Why a schedule operation failed (`Schedule.NotFoundError`, `InvalidError`, `DisabledError`, and what the
/// Session and permission services fail with).
#[derive(Debug)]
pub enum Error {
    NotFound(String),
    Invalid(String),
    Disabled,
    Session(Failure),
    Permission(crate::permission::Error),
}

pub const DISABLED: &str = "Scheduled tasks are disabled; set experimental.schedules in config";

type Result<T> = std::result::Result<T, Error>;

fn storage(error: rusqlite::Error) -> Error {
    Error::Session(fatal(error))
}

/// A `schedule` row.
#[derive(Debug, Clone)]
pub struct Row {
    pub id: String,
    pub session_id: Option<String>,
    name: Option<String>,
    action: Json,
    check: Option<Json>,
    when: Json,
    tz: String,
    status: String,
    next_due_at: Option<i64>,
    ends_at: Option<i64>,
    remaining_runs: Option<i64>,
    run_count: i64,
    checks: i64,
    failures: i64,
    last_fired_at: Option<i64>,
    last_message_id: Option<String>,
    last_outcome: Option<String>,
    state: Option<Json>,
    time_created: i64,
    time_updated: i64,
}

const COLUMNS: &str = "id, session_id, name, action, \"check\", \"when\", tz, status, next_due_at, ends_at, \
    remaining_runs, run_count, checks, failures, last_fired_at, last_message_id, last_outcome, state, time_created, \
    time_updated";

impl Row {
    fn read(row: &SqlRow) -> rusqlite::Result<Row> {
        let parsed = |index: usize| -> rusqlite::Result<Option<Json>> {
            Ok(row.get::<_, Option<String>>(index)?.and_then(|text| json::parse(&text)))
        };
        Ok(Row {
            id: row.get(0)?,
            session_id: row.get(1)?,
            name: row.get(2)?,
            action: parsed(3)?.unwrap_or(Json::Null),
            check: parsed(4)?.filter(|check| !matches!(check, Json::Null)),
            when: parsed(5)?.unwrap_or(Json::Null),
            tz: row.get(6)?,
            status: row.get(7)?,
            next_due_at: row.get(8)?,
            ends_at: row.get(9)?,
            remaining_runs: row.get(10)?,
            run_count: row.get(11)?,
            checks: row.get(12)?,
            failures: row.get(13)?,
            last_fired_at: row.get(14)?,
            last_message_id: row.get(15)?,
            last_outcome: row.get(16)?,
            state: parsed(17)?,
            time_created: row.get(18)?,
            time_updated: row.get(19)?,
        })
    }

    fn when_type(&self) -> &str {
        json::get_str(&self.when, "type").unwrap_or_default()
    }

    fn check_mode(&self) -> Option<&str> {
        self.check.as_ref().and_then(|check| json::get_str(check, "mode"))
    }

    fn prompt(&self) -> &str {
        json::get_str(&self.action, "prompt").unwrap_or_default()
    }

    /// `toInfo`: `Schedule.Info` (times as epoch milliseconds, the JSON codec's `DateTimeUtcFromMillis`).
    pub fn info(&self, last_output: Option<&str>) -> Json {
        let condition = |mode: &str| {
            self.check
                .as_ref()
                .filter(|check| json::get_str(check, "mode") == Some(mode))
                .and_then(|check| json::get(check, "condition").cloned())
        };
        let mut time = Obj::new();
        time.int("created", self.time_created).int("updated", self.time_updated);
        let mut o = Obj::new();
        o.str("id", &self.id);
        o.opt_str("sessionID", self.session_id.as_deref());
        o.opt_str("name", self.name.as_deref());
        o.set("when", self.when.clone()).str("tz", &self.tz);
        o.opt("if", condition("if")).opt("until", condition("until"));
        o.set("action", self.action.clone()).str("status", &self.status);
        o.opt("nextDueAt", self.next_due_at.map(number)).opt("endsAt", self.ends_at.map(number));
        o.opt("remainingRuns", self.remaining_runs.map(number)).int("runCount", self.run_count);
        o.opt("lastFiredAt", self.last_fired_at.map(number));
        o.opt_str("lastOutcome", self.last_outcome.as_deref()).opt_str("lastOutput", last_output);
        o.set("time", time.done());
        o.done()
    }
}

fn number(value: i64) -> Json {
    Json::Number(value as f64)
}

fn find(db: &Connection, id: &str) -> Result<Row> {
    db.query_row(&format!("SELECT {COLUMNS} FROM schedule WHERE id = ?1"), params![id], Row::read)
        .optional()
        .map_err(storage)?
        .ok_or_else(|| Error::NotFound(id.to_owned()))
}

fn sql_value(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}

fn sql_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |text| Value::Text(text.to_owned()))
}

fn sql_json(value: Option<&Json>) -> Value {
    match value {
        None | Some(Json::Null) => Value::Null,
        Some(value) => Value::Text(json::text(value)),
    }
}

/// `UPDATE schedule SET ... WHERE id = ?` (with `AND status = 'active'` for [`update`]).
fn set(app: &App, id: &str, active_only: bool, values: Vec<(&str, Value)>) {
    let assignments: Vec<String> = values.iter().enumerate().map(|(i, (column, _))| format!("\"{column}\" = ?{}", i + 1)).collect();
    let mut bound: Vec<Value> = values.into_iter().map(|(_, value)| value).collect();
    bound.push(Value::Text(id.to_owned()));
    let guard = if active_only { " AND status = 'active'" } else { "" };
    let sql = format!("UPDATE schedule SET {} WHERE id = ?{}{guard}", assignments.join(", "), bound.len());
    if let Err(error) = app.db.lock().unwrap().execute(&sql, params_from_iter(bound)) {
        let _ = fatal(error);
    }
}

fn update(app: &App, row: &Row, values: Vec<(&str, Value)>) {
    set(app, &row.id, true, values)
}

/// A duration string's milliseconds (`durationMillis`), `None` when it is not one.
fn duration_millis(value: &str) -> Option<f64> {
    Some(match Duration::parse(value)? {
        Duration::Millis(millis) => millis,
        Duration::Nanos(nanos) => nanos as f64 / 1e6,
        Duration::Infinity => f64::INFINITY,
        Duration::NegativeInfinity => f64::NEG_INFINITY,
    })
}

fn duration(value: &str) -> Result<i64> {
    match duration_millis(value) {
        None => Err(Error::Invalid(format!("Invalid duration: {value}"))),
        Some(millis) if millis > 0.0 => Ok(if millis.is_finite() { millis as i64 } else { i64::MAX / 4 }),
        Some(_) => Err(Error::Invalid(format!("Duration must be positive: {value}"))),
    }
}

/// Stored durations were validated on create.
fn millis(value: &str) -> i64 {
    duration(value).unwrap_or(MINUTE)
}

/// `OFFSET`: `/\d(?:z|[+-]\d{2}:?\d{2})$/i`.
fn has_offset(input: &str) -> bool {
    let b = input.as_bytes();
    let n = b.len();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let sign = |i: usize| matches!(b.get(i), Some(b'+' | b'-'));
    (n >= 2 && matches!(b[n - 1], b'z' | b'Z') && digit(n - 2))
        || (n >= 6 && digit(n - 6) && sign(n - 5) && (n - 4..n).all(digit))
        || (n >= 7 && digit(n - 7) && sign(n - 6) && digit(n - 5) && digit(n - 4) && b[n - 3] == b':' && digit(n - 2) && digit(n - 1))
}

/// `parseAt`: an instant with an offset, or a wall-clock time in `zone`.
fn parse_at(input: &str, zone: &Zone) -> Option<i64> {
    if has_offset(input) {
        return crate::zone::make(input);
    }
    crate::zone::make(input).map(|wall| zone.from_wall(wall))
}

/// `nextCron`: the first slot strictly after `now`, `None` when the expression never matches a date.
fn next_cron(cron: &cron::Cron, zone: &Zone, now: i64) -> Option<i64> {
    cron.next(zone, now)
}

/// `following`: the first slot strictly after `now`, `None` for one-shots.
fn following(row: &Row, now: i64) -> Option<i64> {
    match row.when_type() {
        "every" => {
            // Keep the phase: skip every missed slot rather than firing them in a burst.
            let every = millis(json::get_str(&row.when, "every").unwrap_or_default());
            let due = row.next_due_at.unwrap_or(now);
            Some(due + ((now - due).div_euclid(every) + 1) * every)
        }
        "cron" => {
            let cron = cron::parse(json::get_str(&row.when, "cron").unwrap_or_default()).ok()?;
            let zone = Zone::named(&row.tz).unwrap_or_else(Zone::utc);
            next_cron(&cron, &zone, now)
        }
        _ => None,
    }
}

fn finished(row: &Row, next: Option<i64>, remaining: Option<i64>) -> bool {
    match next {
        None => true,
        Some(next) => remaining.is_some_and(|r| r <= 0) || row.ends_at.is_some_and(|ends| next > ends),
    }
}

fn prompt_text(prompt: &str, message: Option<&str>) -> String {
    match message.filter(|m| !m.trim_matches(crate::text::is_js_whitespace).is_empty()) {
        Some(message) => format!("{prompt}\n\n{message}"),
        None => prompt.to_owned(),
    }
}

/// The schedule settings where a Session lives (`settings`): whether schedules are on, and the configured timezone.
/// `Err` when the Session's directory is gone.
async fn settings(app: &App, session: &Stored) -> std::result::Result<(bool, Option<String>, Arc<crate::location::Location>), Failure> {
    let location = app.locations.get(app, &session.location).await?;
    let inventory = location.inventory();
    let enabled = crate::flag::enabled(&inventory.entries, "schedules");
    let timezone = inventory.latest(|info| info.timezone.as_deref()).map(str::to_owned);
    Ok((enabled, timezone, location))
}

fn require_session(app: &App, session_id: &str) -> Result<Stored> {
    session::require(&app.db.lock().unwrap(), session_id).map_err(Error::Session)
}

/// `Schedule.enabled`: whether `experimental.schedules` is on where the Session lives (off when its directory is
/// gone).
pub async fn enabled(app: &App, session_id: &str) -> Result<bool> {
    let session = require_session(app, session_id)?;
    Ok(settings(app, &session).await.map(|(enabled, ..)| enabled).unwrap_or(false))
}

pub fn get(app: &App, id: &str) -> Result<Row> {
    find(&app.db.lock().unwrap(), id)
}

/// `Schedule.list`, oldest first.
pub fn list(app: &App, session_id: Option<&str>) -> Result<Vec<Row>> {
    let db = app.db.lock().unwrap();
    let filter = if session_id.is_some() { " WHERE session_id = ?1" } else { "" };
    let sql = format!("SELECT {COLUMNS} FROM schedule{filter} ORDER BY time_created ASC, id ASC");
    let mut statement = db.prepare(&sql).map_err(storage)?;
    let rows = match session_id {
        Some(id) => statement.query_map(params![id], Row::read),
        None => statement.query_map([], Row::read),
    };
    rows.map_err(storage)?.collect::<rusqlite::Result<_>>().map_err(storage)
}

pub fn pause(app: &App, id: &str) -> Result<Row> {
    let row = get(app, id)?;
    if row.status != "active" {
        return Ok(row);
    }
    update(app, &row, vec![("status", Value::Text("paused".into())), ("time_updated", Value::Integer(crate::id::now()))]);
    get(app, id)
}

pub fn resume(app: &App, id: &str) -> Result<Row> {
    let row = get(app, id)?;
    if row.status != "paused" {
        return Ok(row);
    }
    let now = crate::id::now();
    // A one-shot that fell due while paused fires once, late; a repeating schedule resumes at its next slot.
    let next = match row.next_due_at {
        None => None,
        Some(due) if due > now => Some(due),
        Some(due) => following(&row, now).or(Some(due)),
    };
    let done = next.is_none() || finished(&row, next, row.remaining_runs);
    let sql = "UPDATE schedule SET status = ?1, next_due_at = ?2, failures = 0, retry_at = NULL, time_updated = ?3 \
               WHERE id = ?4 AND status = 'paused'";
    app.db
        .lock()
        .unwrap()
        .execute(sql, params![if done { "done" } else { "active" }, if done { None } else { next }, now, id])
        .map_err(storage)?;
    doorbell().open();
    get(app, id)
}

pub fn remove(app: &App, id: &str) -> Result<()> {
    get(app, id)?;
    app.db.lock().unwrap().execute("DELETE FROM schedule WHERE id = ?1", params![id]).map_err(storage)?;
    Ok(())
}

/// `toCreateInput`'s conditions: `{run, is?, matches?, changes?}` or `{code}` as a `Schedule.Condition`.
fn condition(input: &Json) -> Json {
    let mut o = Obj::new();
    if let Some(code) = json::get_str(input, "code") {
        o.str("type", "code").str("code", code);
        return o.done();
    }
    o.str("type", "shell").str("run", json::get_str(input, "run").unwrap_or_default());
    for key in ["is", "matches", "changes"] {
        o.opt(key, json::get(input, key).filter(|v| !matches!(v, Json::Null)).cloned());
    }
    o.done()
}

/// `toWhen`: `{in|at|every|cron, tz?}` as a tagged `Schedule.When`.
fn when(input: &Json) -> Json {
    let mut o = Obj::new();
    for key in ["in", "at", "every", "cron"] {
        if let Some(value) = json::get_str(input, key) {
            o.str("type", key).str(key, value);
            break;
        }
    }
    o.opt_str("tz", json::get_str(input, "tz"));
    o.done()
}

fn present<'a>(input: &'a Json, key: &str) -> Option<&'a Json> {
    json::get(input, key).filter(|value| !matches!(value, Json::Null))
}

/// `Schedule.create` from a `Schedule.Request` (the HTTP payload and the tool's input) for `session_id`.
/// `unattended` settles the check's permission asks without anyone present (`Permission.Unattended`).
pub async fn create(app: &App, session_id: &str, request: &Json, unattended: Option<Vec<Json>>) -> Result<Json> {
    let session = require_session(app, session_id)?;
    let directory = session.location.directory.clone();
    let missing_directory = || Error::Invalid(format!("Session directory does not exist: {directory}"));
    let (on, timezone, location) = settings(app, &session).await.map_err(|_| missing_directory())?;
    if !on {
        return Err(Error::Disabled);
    }
    let name = json::get_str(request, "name").map(str::to_owned);
    let check = match (present(request, "if"), present(request, "until")) {
        (Some(_), Some(_)) => return Err(Error::Invalid("Choose either if or until, not both".into())),
        (Some(input), None) => Some(("if", condition(input))),
        (None, Some(input)) => Some(("until", condition(input))),
        (None, None) => None,
    };
    if let Some((_, condition)) = &check {
        check::validate(condition).map_err(Error::Invalid)?;
    }
    let until = check.as_ref().is_some_and(|(mode, _)| *mode == "until");
    let when = match present(request, "when") {
        Some(input) => self::when(input),
        None if until => {
            let mut o = Obj::new();
            o.str("type", "every").str("every", "30 seconds");
            o.done()
        }
        None => return Err(Error::Invalid("when is required unless until is set".into())),
    };
    let zone = match json::get_str(&when, "tz").map(str::to_owned).or(timezone) {
        None => Zone::local(),
        Some(tz) => Zone::named(&tz).ok_or_else(|| Error::Invalid(format!("Unknown time zone: {tz}")))?,
    };
    let now = crate::id::now();
    let due = first_due(&when, check.is_some(), &zone, now)?;
    let ends = present(request, "ends");
    if ends.and_then(|e| json::get_num(e, "times")).is_some_and(|times| times < 1.0) {
        return Err(Error::Invalid("ends.times must be at least 1".into()));
    }
    let ends_at = ends_at(ends, until, &zone, now)?;
    if ends_at.is_some_and(|ends| due > ends) {
        return Err(Error::Invalid("The schedule ends before it first fires".into()));
    }

    let id = format!("sch_{}", crate::id::ascending());
    let stored_check = check.as_ref().map(|(mode, condition)| {
        let mut o = Obj::new();
        o.str("mode", mode).set("condition", condition.clone());
        o.done()
    });
    let mut seeded = Seeded::default();
    if let (Some((mode, condition)), Some(stored)) = (&check, &stored_check) {
        check::authorize(app, &location, &session, name.as_deref(), &when, stored, unattended).await.map_err(|refused| match refused {
            check::Refused::Invalid(reason) => Error::Invalid(reason),
            check::Refused::Permission(error) => Error::Permission(error),
        })?;
        let kind = json::get_str(condition, "type").unwrap_or_default();
        let seed = *mode == "until" || kind == "code" || (kind == "shell" && json::get_bool(condition, "changes") == Some(true));
        if seed {
            let context = check::RunContext { message_id: format!("msg_{id}_check_0"), now, checks: 0 };
            match check::evaluate(app, &location, &session, stored, &Json::Null, context).await {
                check::Outcome::Failed { error, invalid } => {
                    // A code check that throws or fails at creation is broken (e.g. wrong MCP response field).
                    if invalid || kind == "code" {
                        return Err(Error::Invalid(error));
                    }
                }
                check::Outcome::Ok { wake, state, message } => {
                    seeded = Seeded { matched: *mode == "until" && wake, state, checks: 1, message: if *mode == "until" && wake { message } else { None } };
                }
            }
        }
    }

    let remaining = if until { Some(1) } else { ends.and_then(|e| json::get_num(e, "times")).map(|t| t as i64) };
    let (status, next_due_at, remaining, run_count, last_fired_at, last_outcome) = if seeded.matched {
        ("done", None, Some(0), 1, Some(now), Some("fired"))
    } else {
        ("active", Some(due), remaining, 0, None, None)
    };
    {
        let db = app.db.lock().unwrap();
        db.execute(
            "INSERT INTO schedule (id, session_id, name, \"check\", state, checks, action, \"when\", tz, next_due_at, \
             ends_at, remaining_runs, time_created, time_updated, status, run_count, last_fired_at, last_outcome) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, ?14, ?15, ?16, ?17)",
            params_from_iter([
                Value::Text(id.clone()),
                Value::Text(session.id().to_owned()),
                sql_text(name.as_deref()),
                sql_json(stored_check.as_ref()),
                sql_json(Some(&seeded.state)),
                Value::Integer(seeded.checks),
                Value::Text(json::text(&action(request))),
                Value::Text(json::text(&when)),
                Value::Text(zone.id.clone()),
                sql_value(next_due_at),
                sql_value(ends_at),
                sql_value(remaining),
                Value::Integer(now),
                Value::Text(status.into()),
                Value::Integer(run_count),
                sql_value(last_fired_at),
                sql_text(last_outcome),
            ]),
        )
        .map_err(storage)?;
    }
    let row = get(app, &id)?;
    if seeded.matched {
        return Ok(row.info(seeded.message.as_deref()));
    }
    doorbell().open();
    Ok(row.info(None))
}

fn action(request: &Json) -> Json {
    let mut o = Obj::new();
    o.str("type", "prompt").str("prompt", json::get_str(request, "prompt").unwrap_or_default());
    o.done()
}

#[derive(Default)]
struct Seeded {
    matched: bool,
    state: Json,
    checks: i64,
    message: Option<String>,
}

/// `firstDue`.
fn first_due(when: &Json, has_check: bool, zone: &Zone, now: i64) -> Result<i64> {
    let field = |key: &str| json::get_str(when, key).unwrap_or_default();
    match json::get_str(when, "type").unwrap_or_default() {
        "in" => Ok(now.saturating_add(duration(field("in"))?)),
        "at" => {
            let at = field("at");
            match parse_at(at, zone) {
                None => Err(Error::Invalid(format!("Invalid time: {at}"))),
                Some(value) if value <= now => Err(Error::Invalid(format!("Time is in the past: {at}"))),
                Some(value) => Ok(value),
            }
        }
        "every" => {
            let every = duration(field("every"))?;
            let minimum = if has_check { MIN_CHECK_INTERVAL } else { MIN_INTERVAL };
            if every < minimum {
                return Err(Error::Invalid(
                    if has_check { "every must be at least 15 seconds" } else { "every must be at least 1 minute" }.into(),
                ));
            }
            Ok(now.saturating_add(every))
        }
        _ => {
            let expression = field("cron");
            let cron = cron::parse(expression).map_err(Error::Invalid)?;
            if cron.seconds.len() != 1 {
                return Err(Error::Invalid("cron must fire at most once a minute".into()));
            }
            next_cron(&cron, zone, now).ok_or_else(|| Error::Invalid(format!("cron never fires: {expression}")))
        }
    }
}

/// `endsAt`.
fn ends_at(ends: Option<&Json>, until: bool, zone: &Zone, now: i64) -> Result<Option<i64>> {
    let after = match ends.and_then(|e| json::get_str(e, "after")) {
        Some(after) => Some(now.saturating_add(duration(after)?)),
        None => None,
    };
    let Some(at) = ends.and_then(|e| json::get_str(e, "at")) else {
        return Ok(after.or(until.then_some(now + DEFAULT_UNTIL_AFTER)));
    };
    let value = parse_at(at, zone).ok_or_else(|| Error::Invalid(format!("Invalid time: {at}")))?;
    Ok(Some(value.min(after.unwrap_or(value))))
}

// --- The loop -------------------------------------------------------------------------------------------------

/// Rung when a schedule may have become due sooner than the loop's sleep (a `Latch` in TS: open until the loop
/// closes it at the start of its next round).
pub struct Doorbell {
    open: AtomicBool,
    notify: Notify,
}

impl Doorbell {
    pub fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
    }

    /// Waits until the doorbell opens, at most `millis`.
    async fn wait(&self, millis: i64) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.open.load(Ordering::SeqCst) {
            return;
        }
        let _ = tokio::time::timeout(std::time::Duration::from_millis(millis.max(0) as u64), notified).await;
    }
}

pub fn doorbell() -> &'static Doorbell {
    static DOORBELL: std::sync::OnceLock<Doorbell> = std::sync::OnceLock::new();
    DOORBELL.get_or_init(|| Doorbell { open: AtomicBool::new(false), notify: Notify::new() })
}

fn owner() -> &'static str {
    static OWNER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OWNER.get_or_init(crate::id::uuid)
}

/// `claimLeader`: takes or renews the lease when it is ours or expired.
fn claim_leader(app: &App, now: i64) -> bool {
    let mut value = Obj::new();
    value.str("owner", owner()).int("until", now + LEADER_TTL);
    let value = json::text(&value.done());
    let claimed = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "INSERT INTO kv (key, value, time_created, time_updated) VALUES (?1, ?2, ?3, ?3) \
             ON CONFLICT (key) DO UPDATE SET value = ?2, time_updated = ?3 \
             WHERE json_extract(kv.value, '$.owner') = ?4 OR json_extract(kv.value, '$.until') <= ?3 RETURNING key",
            params![LEADER_KEY, value, now, owner()],
            |row| row.get::<_, String>(0),
        )
        .optional();
    match claimed {
        Ok(claimed) => claimed.is_some(),
        Err(error) => {
            let _ = fatal(error);
            false
        }
    }
}

fn release_leader(app: &App, state: &mut LoopState) {
    state.leading = false;
    let _ = app.db.lock().unwrap().execute(
        "DELETE FROM kv WHERE key = ?1 AND json_extract(value, '$.owner') = ?2",
        params![LEADER_KEY, owner()],
    );
}

fn has_active_rows(app: &App) -> bool {
    app.db
        .lock()
        .unwrap()
        .query_row("SELECT id FROM schedule WHERE status = 'active' LIMIT 1", [], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

/// `nextWake`: how long to sleep before the next schedule after `from` falls due, at most MAX_SLEEP.
fn next_wake(app: &App, from: i64) -> i64 {
    let at: Option<i64> = app
        .db
        .lock()
        .unwrap()
        .query_row("SELECT min(next_due_at) FROM schedule WHERE status = 'active' AND next_due_at > ?1", params![from], |row| row.get(0))
        .ok()
        .flatten();
    match at {
        None => MAX_SLEEP,
        Some(at) => (at - crate::id::now()).clamp(0, MAX_SLEEP),
    }
}

/// `rewake`: wakes every Session whose last scheduled prompt is still pending (a crash between admitting a prompt and
/// the Session claiming the work leaves it asleep).
fn rewake(app: &App) {
    let sessions: Vec<String> = {
        let db = app.db.lock().unwrap();
        let mut statement = match db.prepare(
            "SELECT DISTINCT schedule.session_id FROM schedule INNER JOIN session_inbox ON session_inbox.id = schedule.last_message_id",
        ) {
            Ok(statement) => statement,
            Err(error) => {
                eprintln!("schedule rewake failed: {error}");
                return;
            }
        };
        statement
            .query_map([], |row| row.get::<_, Option<String>>(0))
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .collect()
    };
    for session in sessions {
        app.sessions.executions.wake(app, &session, session::inbox::Promotable::Input);
    }
}

#[derive(Default)]
struct LoopState {
    leading: bool,
    boot_checked: bool,
}

/// `Schedule.run`: fires schedules as they fall due, until the process ends.
pub fn start(app: &Arc<App>) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut state = LoopState::default();
        loop {
            round(&app, &mut state).await;
        }
    });
}

/// The loop ends with the server: a lease it holds is released (`Effect.ensuring(releaseLeader)`), so another
/// process sharing the database can lead at once.
pub fn stop(app: &App) {
    release_leader(app, &mut LoopState::default());
}

async fn round(app: &Arc<App>, state: &mut LoopState) {
    let bell = doorbell();
    bell.close();
    let active = has_active_rows(app);
    if !active && state.boot_checked {
        if state.leading {
            release_leader(app, state);
        }
        bell.wait(MAX_SLEEP).await;
        return;
    }
    let now = crate::id::now();
    if !claim_leader(app, now) {
        state.leading = false;
        state.boot_checked = true;
        bell.wait(MAX_SLEEP).await;
        return;
    }
    if !state.leading {
        state.leading = true;
        state.boot_checked = true;
        rewake(app);
    }
    if !active {
        release_leader(app, state);
        bell.wait(MAX_SLEEP).await;
        return;
    }
    // The lease is renewed while a tick runs.
    let heartbeat = {
        let app = app.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis((LEADER_TTL / 3) as u64)).await;
                claim_leader(&app, crate::id::now());
            }
        })
    };
    tick_at(app, now).await;
    heartbeat.abort();
    if !has_active_rows(app) {
        release_leader(app, state);
        bell.wait(MAX_SLEEP).await;
        return;
    }
    let wait = next_wake(app, now);
    if wait > 0 {
        bell.wait(wait).await;
    }
}

/// Why a firing failed.
enum Fault {
    /// The Session's folder (e.g. a removed worktree) is gone.
    DirectoryNotFound,
    Other(String),
}

/// `tickAt`: fires every schedule due at `now`, at most once each.
async fn tick_at(app: &App, now: i64) {
    let due: Vec<Row> = {
        let db = app.db.lock().unwrap();
        let sql = format!(
            "SELECT {COLUMNS} FROM schedule WHERE status = 'active' AND next_due_at <= ?1 AND (retry_at IS NULL OR retry_at <= ?1) \
             ORDER BY next_due_at ASC"
        );
        let rows = db.prepare(&sql).and_then(|mut statement| {
            statement.query_map(params![now], Row::read).and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        });
        match rows {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("schedule tick failed: {error}");
                return;
            }
        }
    };
    // Up to four fire at once, in due order.
    let concurrency = if due.len() > 1 { TICK_CONCURRENCY } else { 1 };
    let mut firing = tokio::task::JoinSet::new();
    for row in due {
        if firing.len() >= concurrency {
            firing.join_next().await;
        }
        let app = app.arc();
        firing.spawn(async move { fire_due(&app, &row, now).await });
    }
    while firing.join_next().await.is_some() {}
}

/// One due schedule of a tick: fired, else paused (its folder is gone) or backed off.
async fn fire_due(app: &App, row: &Row, now: i64) {
    match step(app, row, now).await {
        Ok(()) => {}
        Err(Fault::DirectoryNotFound) => update(
            app,
            row,
            vec![
                ("status", Value::Text("paused".into())),
                ("last_outcome", Value::Text("failed".into())),
                ("retry_at", Value::Null),
                ("time_updated", Value::Integer(now)),
            ],
        ),
        Err(Fault::Other(cause)) => {
            // Back off rather than retrying a failing prompt on every tick.
            eprintln!("schedule failed to fire: {} {cause}", row.id);
            update(
                app,
                row,
                vec![
                    ("last_outcome", Value::Text("failed".into())),
                    ("retry_at", Value::Integer(now + FAILED_RETRY)),
                    ("time_updated", Value::Integer(now)),
                ],
            );
        }
    }
}

fn pending(app: &App, id: &str) -> bool {
    app.db
        .lock()
        .unwrap()
        .query_row("SELECT id FROM session_inbox WHERE id = ?1", params![id], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

async fn step(app: &App, row: &Row, now: i64) -> std::result::Result<(), Fault> {
    let (Some(_), Some(session_id)) = (row.next_due_at, row.session_id.as_deref()) else { return Ok(()) };
    if !claim_leader(app, crate::id::now()) {
        return Ok(());
    }
    let session = session::require(&app.db.lock().unwrap(), session_id).map_err(|_| Fault::Other(format!("Session not found: {session_id}")))?;
    let (on, _, location) = settings(app, &session).await.map_err(|_| Fault::DirectoryNotFound)?;
    if !on {
        // Reading the flag boots the Session's location, so check again in an hour rather than every tick. The row
        // keeps its due time, so once the flag is back on it catches up like any missed slot.
        update(
            app,
            row,
            vec![
                ("last_outcome", Value::Text("disabled".into())),
                ("retry_at", Value::Integer(now + DISABLED_RETRY)),
                ("time_updated", Value::Integer(now)),
            ],
        );
        return Ok(());
    }
    if row.last_message_id.as_deref().is_some_and(|id| pending(app, id)) {
        // The wake that followed admission may have been lost; waking again is harmless.
        app.sessions.executions.wake(app, session.id(), session::inbox::Promotable::Input);
        let next = following(row, now);
        let done = finished(row, next, row.remaining_runs);
        update(
            app,
            row,
            vec![
                ("last_outcome", Value::Text("skipped".into())),
                ("retry_at", Value::Null),
                ("status", Value::Text(if done { "done" } else { "active" }.into())),
                ("next_due_at", sql_value(if done { None } else { next })),
                ("time_updated", Value::Integer(now)),
            ],
        );
        return Ok(());
    }
    fire(app, row, &session, &location, now).await
}

/// `admitWake`: the schedule's prompt, admitted to steer under an ID derived from the run.
async fn admit_wake(
    app: &App,
    row: &Row,
    session_id: &str,
    run: i64,
    late: bool,
    text: &str,
    check: Option<Json>,
) -> std::result::Result<String, Fault> {
    let id = format!("msg_{}_{run}", row.id);
    let mut schedule = Obj::new();
    schedule.str("id", &row.id).opt_str("name", row.name.as_deref()).int("run", run);
    schedule.int("dueAt", row.next_due_at.unwrap_or_default()).set("late", Json::Bool(late)).opt("check", check);
    let mut metadata = Obj::new();
    metadata.set("schedule", schedule.done());
    let mut input = Obj::new();
    input.str("text", text).str("id", &id).str("delivery", "steer").set("metadata", metadata.done());
    session::prompt::submit(app, session_id, &input.done()).await.map_err(|failure| Fault::Other(json::text(&failure.body)))?;
    Ok(id)
}

async fn fire(app: &App, row: &Row, session: &Stored, location: &crate::location::Location, now: i64) -> std::result::Result<(), Fault> {
    let due = row.next_due_at.unwrap_or(now);
    let late = now - due >= LATE_AFTER;
    let next = following(row, now);
    let outcome = match &row.check {
        None => check::Outcome::Ok { wake: true, state: Json::Null, message: None },
        Some(stored) => {
            let context = check::RunContext { message_id: format!("msg_{}_check_{}", row.id, row.checks + 1), now, checks: row.checks };
            check::evaluate(app, location, session, stored, row.state.as_ref().unwrap_or(&Json::Null), context).await
        }
    };
    let (wake, state, message) = match outcome {
        check::Outcome::Failed { error, invalid } => {
            let strikes = row.failures + 1;
            if !invalid && strikes < MAX_CHECK_FAILURES {
                update(
                    app,
                    row,
                    vec![
                        ("failures", Value::Integer(strikes)),
                        ("last_outcome", Value::Text("failed".into())),
                        ("retry_at", Value::Integer(now + CHECK_FAILED_RETRY)),
                        ("time_updated", Value::Integer(now)),
                    ],
                );
                return Ok(());
            }
            let run = row.run_count + 1;
            let note = if invalid {
                format!("[Schedule paused after invalid check: {error}]")
            } else {
                format!("[Schedule paused after {strikes} failed checks: {error}]")
            };
            let mut meta = Obj::new();
            meta.str("error", &error).int("failures", strikes);
            let id = admit_wake(app, row, session.id(), run, late, &format!("{}\n\n{note}", row.prompt()), Some(meta.done())).await?;
            set(
                app,
                &row.id,
                false,
                vec![
                    ("status", Value::Text("paused".into())),
                    ("failures", Value::Integer(strikes)),
                    ("run_count", Value::Integer(run)),
                    ("last_message_id", Value::Text(id)),
                    ("last_fired_at", Value::Integer(now)),
                    ("last_outcome", Value::Text("failed".into())),
                    ("retry_at", Value::Null),
                    ("time_updated", Value::Integer(now)),
                ],
            );
            return Ok(());
        }
        check::Outcome::Ok { wake, state, message } => (wake, state, message),
    };

    if let (Some(stored), false) = (&row.check, wake) {
        let checks = row.checks + 1;
        let ended = next.is_none_or(|next| row.ends_at.is_some_and(|ends| next > ends));
        if row.check_mode() == Some("until") && ended {
            let run = row.run_count + 1;
            let condition = json::get(stored, "condition").cloned().unwrap_or(Json::Null);
            let text = format!("{}\n\n[Schedule timed out without matching: {}]", row.prompt(), check::describe(&condition));
            let mut meta = Obj::new();
            meta.set("expired", Json::Bool(true));
            let id = admit_wake(app, row, session.id(), run, late, &text, Some(meta.done())).await?;
            set(
                app,
                &row.id,
                false,
                vec![
                    ("status", Value::Text("done".into())),
                    ("checks", Value::Integer(checks)),
                    ("failures", Value::Integer(0)),
                    ("state", sql_json(Some(&state))),
                    ("run_count", Value::Integer(run)),
                    ("last_message_id", Value::Text(id)),
                    ("last_fired_at", Value::Integer(now)),
                    ("last_outcome", Value::Text("expired".into())),
                    ("next_due_at", Value::Null),
                    ("remaining_runs", Value::Integer(0)),
                    ("retry_at", Value::Null),
                    ("time_updated", Value::Integer(now)),
                ],
            );
            return Ok(());
        }
        let mut values = vec![
            ("checks", Value::Integer(checks)),
            ("failures", Value::Integer(0)),
            ("state", sql_json(Some(&state))),
            ("last_outcome", Value::Text("checked".into())),
            ("retry_at", Value::Null),
        ];
        if ended {
            values.push(("status", Value::Text("done".into())));
            values.push(("next_due_at", Value::Null));
        } else {
            values.push(("next_due_at", sql_value(next)));
        }
        values.push(("time_updated", Value::Integer(now)));
        update(app, row, values);
        return Ok(());
    }

    let run = row.run_count + 1;
    let check_meta = row.check.as_ref().map(|_| {
        let mut o = Obj::new();
        o.opt_str("message", message.as_deref());
        o.done()
    });
    let id = admit_wake(app, row, session.id(), run, late, &prompt_text(row.prompt(), message.as_deref()), check_meta).await?;
    let until = row.check_mode() == Some("until");
    let remaining = if until { Some(0) } else { row.remaining_runs.map(|r| r - 1) };
    let done = until || finished(row, next, remaining);
    // Record the run even if a pause landed during the prompt, or the next firing would reuse this message id.
    let mut values = vec![("run_count", Value::Integer(run))];
    if row.check.is_some() {
        values.push(("checks", Value::Integer(row.checks + 1)));
        values.push(("failures", Value::Integer(0)));
        values.push(("state", sql_json(Some(&state))));
    }
    values.extend([
        ("last_message_id", Value::Text(id)),
        ("last_fired_at", Value::Integer(now)),
        ("last_outcome", Value::Text(if late { "late" } else { "fired" }.into())),
        ("retry_at", Value::Null),
        ("remaining_runs", sql_value(remaining)),
    ]);
    if done {
        values.push(("status", Value::Text("done".into())));
    }
    values.push(("next_due_at", sql_value(if done { None } else { next })));
    values.push(("time_updated", Value::Integer(now)));
    set(app, &row.id, false, values);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets() {
        assert!(has_offset("2031-03-04T05:06:07Z"));
        assert!(has_offset("2031-03-04T05:06:07+01:00"));
        assert!(has_offset("2031-03-04T05:06:07-0100"));
        assert!(!has_offset("2031-03-04T09:00"));
        assert_eq!(duration("2 hours").unwrap(), 7_200_000);
        assert!(matches!(duration("soon"), Err(Error::Invalid(m)) if m == "Invalid duration: soon"));
        assert!(matches!(duration("0 seconds"), Err(Error::Invalid(m)) if m == "Duration must be positive: 0 seconds"));
    }
}
