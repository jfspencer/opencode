//! Session statistics (packages/core/src/session/stats.ts, `GET /api/experimental/session/stats`): activity, usage
//! and tool reliability over the whole database for a time range, read in 31-day windows of the recorded history.
//! A fork's copied history (older than the fork itself) counts once, under the original; a compaction's usage counts
//! toward the totals without being a step.

use std::collections::HashMap;

use indexmap::IndexMap;
use opencode_protocol::Json;
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};

use crate::json::Obj;
use crate::zone::Zone;

const WINDOW: f64 = 31.0 * 24.0 * 60.0 * 60.0 * 1000.0;
const FORKED: &str = "(session.fork_session_id IS NULL OR message.time_created >= session.time_created)";

pub struct Input<'a> {
    pub from: Option<f64>,
    pub to: Option<f64>,
    pub project: Option<&'a str>,
    pub zone: Zone,
    pub tools: &'a str,
}

/// `SessionStats.InvalidRangeError`.
pub struct InvalidRange;

#[derive(Default, Clone, Copy)]
struct Tokens {
    input: f64,
    output: f64,
    reasoning: f64,
    read: f64,
    write: f64,
}

impl Tokens {
    fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.reasoning += other.reasoning;
        self.read += other.read;
        self.write += other.write;
    }

    fn total(&self) -> f64 {
        self.input + self.output + self.reasoning + self.read + self.write
    }

    fn json(&self) -> Json {
        let mut cache = Obj::new();
        cache.set("read", Json::Number(self.read)).set("write", Json::Number(self.write));
        let mut o = Obj::new();
        o.set("input", Json::Number(self.input)).set("output", Json::Number(self.output));
        o.set("reasoning", Json::Number(self.reasoning)).set("cache", cache.done());
        o.done()
    }
}

#[derive(Default, Clone, Copy)]
struct Counts {
    calls: f64,
    succeeded: f64,
    failed: f64,
    unfinished: f64,
}

impl Counts {
    fn add(&mut self, status: Option<&str>) {
        self.calls += 1.0;
        match status {
            Some("completed") => self.succeeded += 1.0,
            Some("error") => self.failed += 1.0,
            _ => self.unfinished += 1.0,
        }
    }

    fn fill(&self, o: &mut Obj) {
        o.set("calls", Json::Number(self.calls)).set("succeeded", Json::Number(self.succeeded));
        o.set("failed", Json::Number(self.failed)).set("unfinished", Json::Number(self.unfinished));
    }
}

struct Model {
    provider: String,
    id: String,
    variant: Option<String>,
    steps: f64,
    tokens: Tokens,
    cost: f64,
}

fn bind(values: &mut Vec<Value>, value: Value) -> String {
    values.push(value);
    format!("?{}", values.len())
}

fn project_clause(values: &mut Vec<Value>, project: Option<&str>) -> String {
    match project {
        Some(project) => format!("AND session.project_id = {}", bind(values, Value::Text(project.to_owned()))),
        None => String::new(),
    }
}

/// `windows`: 31-day ranges covering `[from, to)`.
fn windows(from: f64, to: f64) -> Vec<(f64, f64)> {
    let count = ((to - from) / WINDOW).ceil().max(0.0) as usize;
    (0..count).map(|i| (from + i as f64 * WINDOW, to.min(from + (i + 1) as f64 * WINDOW))).collect()
}

fn number(value: Value) -> Option<f64> {
    match value {
        Value::Integer(n) => Some(n as f64),
        Value::Real(n) => Some(n),
        _ => None,
    }
}

fn text(value: Value) -> Option<String> {
    match value {
        Value::Text(text) => Some(text),
        _ => None,
    }
}

/// `makeDateKey`: the date of an instant in the zone, as `YYYY-MM-DD`.
fn date_key(zone: &Zone, time: f64) -> String {
    let wall = zone.to_wall(time as i64);
    let (year, month, day) = crate::zone::civil_from_days(wall.div_euclid(crate::zone::DAY_MS));
    format!("{year}-{month:02}-{day:02}")
}

fn ordinal(date: &str) -> i64 {
    let parts: Vec<i64> = date.split('-').map(|p| p.parse().unwrap_or(0)).collect();
    crate::zone::days_from_civil(parts[0], parts[1], parts[2])
}

fn longest_streak(days: &[String]) -> f64 {
    let (mut current, mut longest) = (0, 0);
    for (index, day) in days.iter().enumerate() {
        current = if index > 0 && ordinal(day) - ordinal(&days[index - 1]) == 1 { current + 1 } else { 1 };
        longest = longest.max(current);
    }
    longest as f64
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 0 { (values[middle - 1] + values[middle]) / 2.0 } else { values[middle] })
}

/// `SessionStats.get`.
pub fn get(db: &Connection, input: &Input, now: f64) -> rusqlite::Result<Result<Json, InvalidRange>> {
    let to = input.to.unwrap_or(now);
    if input.from.is_some_and(|from| from >= to) {
        return Ok(Err(InvalidRange));
    }
    let from = match input.from {
        Some(from) => from,
        None => {
            let mut values = vec![];
            let to_bind = bind(&mut values, Value::Real(to));
            let project = project_clause(&mut values, input.project);
            let sql = format!(
                "SELECT min(message.time_created) FROM session_message AS message JOIN session_v2 AS session ON session.id = message.session_id \
                 WHERE message.type IN ('user', 'assistant') AND message.time_created < {to_bind} AND {FORKED} {project}"
            );
            db.query_row(&sql, params_from_iter(values), |row| row.get::<_, Value>(0)).map(number)?.unwrap_or(to)
        }
    };
    let range_clause = |values: &mut Vec<Value>, from: f64, to: f64| {
        let from = bind(values, Value::Real(from));
        let to = bind(values, Value::Real(to));
        format!("message.time_created >= {from} AND message.time_created < {to}")
    };
    // Only recorded history needs windows, so a wide range costs no more than the history inside it.
    let recorded = {
        let mut values = vec![];
        let range = range_clause(&mut values, from, to);
        let project = project_clause(&mut values, input.project);
        let sql = format!(
            "SELECT min(message.time_created), max(message.time_created) FROM session_message AS message \
             JOIN session_v2 AS session ON session.id = message.session_id \
             WHERE message.type IN ('user', 'assistant', 'compaction') AND {range} AND {FORKED} {project}"
        );
        db.query_row(&sql, params_from_iter(values), |row| Ok((number(row.get(0)?), number(row.get(1)?))))?
    };
    let ranges = match recorded {
        (Some(first), Some(last)) => windows(first, last + 1.0),
        _ => vec![],
    };
    let mut sessions = std::collections::HashSet::new();
    let mut subagents = std::collections::HashSet::new();
    let mut activity: HashMap<String, f64> = HashMap::new();
    let mut models: IndexMap<String, Model> = IndexMap::new();
    let mut tools: IndexMap<String, (Counts, Vec<f64>)> = IndexMap::new();
    let (mut prompts, mut steps, mut cost) = (0.0, 0.0, 0.0);
    let mut tokens = Tokens::default();
    let mut tool_totals = Counts::default();

    for (start, end) in &ranges {
        let mut values = vec![];
        let range = range_clause(&mut values, *start, *end);
        let project = project_clause(&mut values, input.project);
        let sql = format!(
            "SELECT message.session_id, session.parent_id, message.type, message.time_created, \
             json_extract(message.data, '$.model.providerID'), json_extract(message.data, '$.model.id'), \
             json_extract(message.data, '$.model.variant'), json_extract(message.data, '$.tokens.input'), \
             json_extract(message.data, '$.tokens.output'), json_extract(message.data, '$.tokens.reasoning'), \
             json_extract(message.data, '$.tokens.cache.read'), json_extract(message.data, '$.tokens.cache.write'), \
             json_extract(message.data, '$.cost') \
             FROM session_message AS message JOIN session_v2 AS session ON session.id = message.session_id \
             WHERE message.type IN ('user', 'assistant', 'compaction') AND {range} AND {FORKED} {project}"
        );
        let mut statement = db.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(values))?;
        while let Some(row) = rows.next()? {
            let session: String = row.get(0)?;
            let parent: Option<String> = row.get(1)?;
            let ty: String = row.get(2)?;
            let created = number(row.get(3)?).unwrap_or(0.0);
            let n = |index: usize| -> rusqlite::Result<f64> { Ok(number(row.get(index)?).unwrap_or(0.0)) };
            let row_tokens = Tokens { input: n(7)?, output: n(8)?, reasoning: n(9)?, read: n(10)?, write: n(11)? };
            let row_cost = n(12)?;
            // A compaction's message carries the usage of all of its model calls; it is not a step.
            if ty == "compaction" {
                tokens.add(&row_tokens);
                cost += row_cost;
                continue;
            }
            if parent.is_none() {
                sessions.insert(session);
            } else {
                subagents.insert(session);
            }
            if ty == "user" {
                if parent.is_none() {
                    prompts += 1.0;
                }
                continue;
            }
            steps += 1.0;
            tokens.add(&row_tokens);
            cost += row_cost;
            *activity.entry(date_key(&input.zone, created)).or_insert(0.0) += 1.0;
            let (Some(provider), Some(id)) = (text(row.get(4)?).filter(|p| !p.is_empty()), text(row.get(5)?).filter(|m| !m.is_empty())) else {
                continue;
            };
            let variant = text(row.get(6)?);
            let key = format!("{provider}/{id}#{}", variant.as_deref().unwrap_or(""));
            let model = models.entry(key).or_insert_with(|| Model {
                provider,
                id,
                variant: variant.filter(|v| !v.is_empty()),
                steps: 0.0,
                tokens: Tokens::default(),
                cost: 0.0,
            });
            model.steps += 1.0;
            model.cost += row_cost;
            model.tokens.add(&row_tokens);
        }
    }

    if input.tools != "none" {
        for (start, end) in &ranges {
            let mut values = vec![];
            let parts_range = range_clause(&mut values, *start, *end);
            let parts_project = project_clause(&mut values, input.project);
            let inline_range = range_clause(&mut values, *start, *end);
            let inline_project = project_clause(&mut values, input.project);
            let columns = |source: &str| {
                if input.tools == "summary" {
                    format!("json_extract({source}, '$.state.status') AS status")
                } else {
                    format!(
                        "json_extract({source}, '$.name') AS name, json_extract({source}, '$.state.status') AS status, \
                         CASE WHEN json_extract({source}, '$.time.completed') IS NULL THEN NULL \
                         ELSE json_extract({source}, '$.time.completed') - coalesce(json_extract({source}, '$.time.ran'), json_extract({source}, '$.time.created')) END AS duration"
                    )
                }
            };
            let calls = format!(
                "SELECT {} FROM session_part AS part JOIN session_message AS message ON message.id = part.message_id \
                 JOIN session_v2 AS session ON session.id = message.session_id \
                 WHERE message.type = 'assistant' AND {parts_range} AND {FORKED} AND part.type = 'tool' {parts_project} \
                 UNION ALL \
                 SELECT {} FROM session_message AS message JOIN session_v2 AS session ON session.id = message.session_id, \
                 json_each(message.data, '$.content') AS content \
                 WHERE message.type = 'assistant' AND {inline_range} AND {FORKED} \
                 AND json_array_length(message.data, '$.content') > 0 AND json_extract(content.value, '$.type') = 'tool' {inline_project}",
                columns("part.data"),
                columns("content.value"),
            );
            if input.tools == "summary" {
                let sql = format!(
                    "WITH calls AS MATERIALIZED ({calls}) SELECT count(*), count(*) FILTER (WHERE status = 'completed'), \
                     count(*) FILTER (WHERE status = 'error'), \
                     count(*) FILTER (WHERE status IS NULL OR status NOT IN ('completed', 'error')) FROM calls"
                );
                let row = db.query_row(&sql, params_from_iter(values), |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))
                })?;
                tool_totals.calls += row.0 as f64;
                tool_totals.succeeded += row.1 as f64;
                tool_totals.failed += row.2 as f64;
                tool_totals.unfinished += row.3 as f64;
                continue;
            }
            let mut statement = db.prepare(&calls)?;
            let mut rows = statement.query(params_from_iter(values))?;
            while let Some(row) = rows.next()? {
                let name = text(row.get(0)?);
                let status = text(row.get(1)?);
                let duration = number(row.get(2)?);
                tool_totals.add(status.as_deref());
                let Some(name) = name.filter(|n| !n.is_empty()) else { continue };
                let tool = tools.entry(name).or_default();
                tool.0.add(status.as_deref());
                if let Some(duration) = duration {
                    tool.1.push(duration);
                }
            }
        }
    }

    let mut days: Vec<(String, f64)> = activity.into_iter().collect();
    days.sort_by(|a, b| crate::collate::compare(&a.0, &b.0));
    let names: Vec<String> = days.iter().map(|(date, _)| date.clone()).collect();
    let mut range = Obj::new();
    range.set("from", Json::Number(from)).set("to", Json::Number(to));
    let mut tool_info = Obj::new();
    tool_info.str("mode", input.tools);
    if input.tools != "none" {
        let mut totals = Obj::new();
        tool_totals.fill(&mut totals);
        tool_info.set("totals", totals.done());
    }
    if input.tools == "detail" {
        let mut usage: Vec<(String, (Counts, Vec<f64>))> = tools.into_iter().collect();
        usage.sort_by(|a, b| b.1.0.calls.total_cmp(&a.1.0.calls));
        let usage = usage
            .into_iter()
            .map(|(name, (counts, mut durations))| {
                let mut o = Obj::new();
                o.str("name", &name);
                counts.fill(&mut o);
                o.opt("durationP50", median(&mut durations).map(Json::Number));
                o.done()
            })
            .collect();
        tool_info.set("usage", Json::Array(usage));
    }
    let mut models: Vec<Model> = models.into_values().collect();
    models.sort_by(|a, b| b.tokens.total().total_cmp(&a.tokens.total()));
    let models = models
        .into_iter()
        .map(|model| {
            let mut reference = Obj::new();
            reference.str("providerID", &model.provider).str("id", &model.id).opt_str("variant", model.variant.as_deref());
            let mut o = Obj::new();
            o.set("model", reference.done()).set("steps", Json::Number(model.steps)).set("tokens", model.tokens.json());
            o.set("cost", Json::Number(model.cost));
            o.done()
        })
        .collect();
    let mut o = Obj::new();
    o.set("range", range.done());
    o.set("sessions", Json::Number(sessions.len() as f64)).set("subagents", Json::Number(subagents.len() as f64));
    o.set("prompts", Json::Number(prompts)).set("steps", Json::Number(steps)).set("tokens", tokens.json());
    o.set("cost", Json::Number(cost)).set("tools", tool_info.done());
    o.set("activeDays", Json::Number(days.len() as f64)).set("streak", Json::Number(longest_streak(&names)));
    o.set(
        "activity",
        Json::Array(
            days.iter()
                .map(|(date, steps)| {
                    let mut o = Obj::new();
                    o.str("date", date).set("steps", Json::Number(*steps));
                    o.done()
                })
                .collect(),
        ),
    );
    o.set("models", Json::Array(models));
    Ok(Ok(o.done()))
}
