//! `opencode stats` (packages/cli/src/commands/handlers/stats.ts, util/src/activity-calendar.ts): the server's usage
//! statistics for a range of local days, as JSON or as a shareable summary with an activity calendar and detail
//! tables. Dates are local, through the C library's time zone.

use opencode_protocol::Json;

use super::client::ClientError;
use super::connection::{self, Args};
use super::out::{Fail, Result, pretty, stderr, stdout};
use super::parse::{Globals, Input};
use crate::json::{self, Obj, get, get_num, get_str};

/// Local calendar fields of a time in milliseconds.
fn local(ms: f64) -> libc::tm {
    let seconds = (ms / 1000.0).floor() as libc::time_t;
    // SAFETY: localtime_r fills the provided struct.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&seconds, &mut tm);
        tm
    }
}

/// `new Date(year, month, day).getTime()` (local midnight; out-of-range fields roll over).
fn local_midnight(year: i64, month: i64, day: i64) -> f64 {
    // SAFETY: mktime normalizes and reads the provided struct.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = (year - 1900) as libc::c_int;
        tm.tm_mon = month as libc::c_int;
        tm.tm_mday = day as libc::c_int;
        tm.tm_isdst = -1;
        libc::mktime(&mut tm) as f64 * 1000.0
    }
}

/// `Intl.DateTimeFormat().resolvedOptions().timeZone`.
fn time_zone() -> String {
    if let Some(zone) = std::env::var("TZ").ok().filter(|z| !z.is_empty()) {
        return zone.trim_start_matches(':').to_owned();
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|target| target.to_string_lossy().split_once("zoneinfo/").map(|(_, zone)| zone.to_owned()))
        .unwrap_or_else(|| "UTC".into())
}

struct Range {
    from: Option<f64>,
    to: f64,
    label: String,
}

fn stats_range(days: Option<i64>, year: Option<i64>, all: bool) -> Range {
    let now = crate::id::now() as f64;
    let to = now + 1.0;
    if all {
        return Range { from: None, to, label: "all time".into() };
    }
    let today = local(now);
    if let Some(days) = days {
        let from = local_midnight(today.tm_year as i64 + 1900, today.tm_mon as i64, today.tm_mday as i64 - (days - 1).max(0));
        let label = if days == 0 || days == 1 { "today".into() } else { format!("last {days} days") };
        return Range { from: Some(from), to, label };
    }
    let current = today.tm_year as i64 + 1900;
    let year = year.unwrap_or(current);
    Range {
        from: Some(local_midnight(year, 0, 1)),
        to: if year == current { to } else { local_midnight(year + 1, 0, 1) },
        label: if year == current { format!("{year} so far") } else { year.to_string() },
    }
}

pub async fn command(globals: &Globals, input: &Input) -> Result {
    match run(globals, input).await {
        Ok(code) => Ok(code),
        Err(fail) => {
            stderr(&format!("{}\n", fail.message));
            Ok(1)
        }
    }
}

async fn run(globals: &Globals, input: &Input) -> Result {
    let days = input.int("days");
    let year = input.int("year");
    let all = input.flag("all");
    let project = input.str("project").map(String::from);
    if [days.is_some(), year.is_some(), all].iter().filter(|b| **b).count() > 1 {
        return Err(Fail::error("--days, --year, and --all cannot be combined"));
    }
    let server = connection::resolve(globals, Args::default()).await?;
    let url = server.endpoint.url.clone();
    let reach = |error: ClientError| match error {
        ClientError::Transport(_) => Fail::error(format!("Could not reach server at {url}")),
        other => Fail::from(other),
    };
    let client = server.client();
    let timeout = Some(std::time::Duration::from_secs(30));
    let result = async {
        let range = stats_range(days, year, all);
        let project_id = match project.as_deref() {
            Some(".") => {
                let mut inner = Obj::new();
                inner.str("directory", &crate::project::absolutize("."));
                let mut query = Obj::new();
                query.set("location", inner.done());
                let located = client.call_with("location.get", &[], Some(query.done()), None, &[], timeout).await.map_err(&reach)?;
                get(&located, "project").and_then(|p| get_str(p, "id")).map(String::from)
            }
            other => other.map(String::from),
        };
        let json_output = input.flag("json");
        let details = input.flag("models") || input.flag("tools") || input.flag("cost") || input.flag("full");
        let tools = if json_output || input.flag("tools") || input.flag("full") {
            "detail"
        } else if details {
            "none"
        } else {
            "summary"
        };
        let mut query = Obj::new();
        query
            .opt("from", range.from.map(Json::Number))
            .set("to", Json::Number(range.to))
            .opt_str("project", project_id.as_deref())
            .str("timezone", &time_zone())
            .str("tools", tools);
        let response = client
            .call_with("experimental.session.stats", &[], Some(query.done()), None, &[], timeout)
            .await
            .map_err(&reach)?;
        let stats = get(&response, "data").cloned().unwrap_or(Json::Null);
        let output = if json_output {
            pretty(&stats)
        } else {
            let scope = match project.as_deref() {
                None => "all projects",
                Some(".") => "current project",
                Some(_) => "selected project",
            };
            render(
                &stats,
                &range.label,
                scope,
                input.flag("models") || input.flag("full"),
                input.flag("tools") || input.flag("full"),
                input.flag("cost") || input.flag("full"),
                input.int("limit").unwrap_or(5) as usize,
                80,
            )
        };
        stdout(&format!("{output}\n"));
        Ok(0)
    }
    .await;
    server.close().await;
    result
}

fn num(value: &Json, key: &str) -> f64 {
    get_num(value, key).unwrap_or(0.0)
}

fn tokens_total(tokens: Option<&Json>) -> f64 {
    let Some(tokens) = tokens else { return 0.0 };
    let cache = get(tokens, "cache");
    num(tokens, "input")
        + num(tokens, "output")
        + num(tokens, "reasoning")
        + cache.map_or(0.0, |c| num(c, "read"))
        + cache.map_or(0.0, |c| num(c, "write"))
}

/// `Number.prototype.toFixed`.
fn fixed(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
}

fn trim_decimal(value: f64) -> String {
    let text = fixed(value, 1);
    text.strip_suffix(".0").map(String::from).unwrap_or(text)
}

/// `toLocaleString("en-US")` of a rounded integer.
fn grouped(value: f64) -> String {
    let rounded = value.round() as i64;
    let digits = rounded.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if rounded < 0 { format!("-{out}") } else { out }
}

fn format_number(value: f64) -> String {
    if value >= 1_000_000_000.0 {
        return format!("{}b", trim_decimal(value / 1_000_000_000.0));
    }
    if value >= 1_000_000.0 {
        return format!("{}m", trim_decimal(value / 1_000_000.0));
    }
    if value >= 1_000.0 {
        return format!("{}k", trim_decimal(value / 1_000.0));
    }
    grouped(value)
}

fn format_percent(value: f64) -> String {
    format!("{}%", fixed(value, if value >= 10.0 { 1 } else { 2 }))
}

fn format_duration(value: f64) -> String {
    if value < 1_000.0 {
        return format!("{}ms", value.round() as i64);
    }
    format!("{}s", trim_decimal(value / 1_000.0))
}

fn metric(value: f64, noun: &str) -> String {
    format!("{} {noun}{}", format_number(value), if value == 1.0 { "" } else { "s" })
}

fn truncate(value: &str, width: usize) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    if units.len() <= width {
        return value.to_owned();
    }
    format!("{}…", String::from_utf16_lossy(&units[..width - 1]))
}

fn pad_end(value: &str, width: usize) -> String {
    let len = value.encode_utf16().count();
    format!("{value}{}", " ".repeat(width.saturating_sub(len)))
}

fn pad_start(value: &str, width: usize) -> String {
    let len = value.encode_utf16().count();
    format!("{}{value}", " ".repeat(width.saturating_sub(len)))
}

fn table_row(label: &str, second: &str, third: &str, fourth: &str) -> String {
    format!("{}{}{}{}", pad_end(&truncate(label, 34), 34), pad_start(second, 10), pad_start(third, 12), pad_start(fourth, 12))
}

fn model_name(model: Option<&Json>) -> String {
    let model = model.unwrap_or(&Json::Null);
    let variant = get_str(model, "variant").map(|v| format!("#{v}")).unwrap_or_default();
    format!("{}/{}{variant}", get_str(model, "providerID").unwrap_or(""), get_str(model, "id").unwrap_or(""))
}

#[allow(clippy::too_many_arguments)]
fn render(stats: &Json, label: &str, scope: &str, models: bool, tools: bool, cost: bool, limit: usize, width: usize) -> String {
    let tool_stats = get(stats, "tools");
    let totals = tool_stats.filter(|t| get_str(t, "mode") != Some("none")).and_then(|t| get(t, "totals"));
    let terminal = totals.map_or(0.0, |t| num(t, "succeeded") + num(t, "failed"));
    let rate = totals.filter(|_| terminal != 0.0).map(|t| num(t, "succeeded") / terminal * 100.0);
    let mut session_line = vec![metric(num(stats, "sessions"), "session")];
    if num(stats, "subagents") > 0.0 {
        session_line.push(metric(num(stats, "subagents"), "subagent"));
    }
    let tool_summary = match (totals, rate) {
        (None, _) => "tool stats unavailable".to_owned(),
        (Some(_), None) => "no tool calls".to_owned(),
        (Some(_), Some(rate)) => format!("{} tool success", format_percent(rate)),
    };
    let details = models || tools || cost;
    let empty = num(stats, "sessions") == 0.0 && num(stats, "prompts") == 0.0 && num(stats, "steps") == 0.0;
    let heading = format!("opencode stats · {label} · {scope}");
    let mut lines: Vec<String> = if details {
        vec![format!("{label} · {scope}")]
    } else if empty {
        vec![heading, String::new(), "no activity in this range".into(), String::new(), "opencode.ai".into()]
    } else {
        let range = get(stats, "range");
        let mut lines = vec![heading, String::new()];
        lines.extend(activity(
            get(stats, "activity").and_then(json::array).unwrap_or(&[]),
            range.map_or(0.0, |r| num(r, "from")),
            range.map_or(0.0, |r| num(r, "to")),
            width,
        ));
        let streak = num(stats, "streak");
        lines.extend([
            String::new(),
            session_line.join(" · "),
            format!(
                "{} · {} · {}",
                metric(num(stats, "prompts"), "prompt"),
                metric(num(stats, "steps"), "step"),
                metric(tokens_total(get(stats, "tokens")), "token")
            ),
            format!(
                "{tool_summary} · {} · best streak {} day{}",
                metric(num(stats, "activeDays"), "active day"),
                json::text(&Json::Number(streak)),
                if streak == 1.0 { "" } else { "s" }
            ),
            String::new(),
            "opencode.ai".into(),
        ]);
        lines
    };
    let separate = |lines: &mut Vec<String>| {
        if !lines.is_empty() {
            lines.push(String::new());
        }
    };
    if cost {
        separate(&mut lines);
        let tokens = get(stats, "tokens").cloned().unwrap_or(Json::Null);
        let cache = get(&tokens, "cache").cloned().unwrap_or(Json::Null);
        let input = num(&tokens, "input") + num(&cache, "read") + num(&cache, "write");
        let cached = if input == 0.0 { 0.0 } else { num(&cache, "read") / input * 100.0 };
        let row = |label: &str, value: String| format!("  {}{value}", pad_end(label, 20));
        lines.extend([
            "COST & TOKENS".to_owned(),
            row("cost", format!("${}", fixed(num(stats, "cost"), 2))),
            row("input", format_number(num(&tokens, "input"))),
            row("output", format_number(num(&tokens, "output"))),
            row("reasoning", format_number(num(&tokens, "reasoning"))),
            row("cache read", format_number(num(&cache, "read"))),
            row("cache write", format_number(num(&cache, "write"))),
            row("cached input", format_percent(cached)),
        ]);
    }
    if models {
        separate(&mut lines);
        let all = get(stats, "models").and_then(json::array).unwrap_or(&[]);
        if all.is_empty() {
            lines.extend(["MODELS".to_owned(), "  no model usage".to_owned()]);
        } else {
            let shown = &all[..all.len().min(limit)];
            let more = all.len() - shown.len();
            lines.push("MODELS".into());
            lines.push(table_row("model", "tokens", "steps", "cost"));
            for item in shown {
                lines.push(table_row(
                    &model_name(get(item, "model")),
                    &format_number(tokens_total(get(item, "tokens"))),
                    &format_number(num(item, "steps")),
                    &format!("${}", fixed(num(item, "cost"), 2)),
                ));
            }
            if more > 0 {
                lines.extend([String::new(), format!("+{} more model{}", grouped(more as f64), if more == 1 { "" } else { "s" })]);
            }
        }
    }
    if tools {
        separate(&mut lines);
        match tool_stats {
            Some(detail) if get_str(detail, "mode") == Some("detail") => {
                let usage = get(detail, "usage").and_then(json::array).unwrap_or(&[]);
                if usage.is_empty() {
                    lines.extend(["TOOL RELIABILITY".to_owned(), "  no tool calls".to_owned()]);
                } else {
                    let shown = &usage[..usage.len().min(limit)];
                    let more = usage.len() - shown.len();
                    lines.push("TOOL RELIABILITY".into());
                    lines.push(table_row("tool", "calls", "error", "p50"));
                    for tool in shown {
                        let terminal = num(tool, "succeeded") + num(tool, "failed");
                        let error = if terminal == 0.0 { "-".into() } else { format_percent(num(tool, "failed") / terminal * 100.0) };
                        let p50 = get_num(tool, "durationP50").map(format_duration).unwrap_or_else(|| "-".into());
                        lines.push(table_row(get_str(tool, "name").unwrap_or(""), &format_number(num(tool, "calls")), &error, &p50));
                    }
                    let totals = get(detail, "totals").cloned().unwrap_or(Json::Null);
                    lines.push(String::new());
                    lines.push(format!(
                        "{} finished calls · {} unfinished",
                        format_number(num(&totals, "succeeded") + num(&totals, "failed")),
                        format_number(num(&totals, "unfinished"))
                    ));
                    if more > 0 {
                        lines.push(format!("+{} more tool{}", grouped(more as f64), if more == 1 { "" } else { "s" }));
                    }
                }
            }
            _ => lines.extend(["TOOL RELIABILITY".to_owned(), "  tool details unavailable".to_owned()]),
        }
    }
    lines.join("\n")
}

/// Days since the epoch of a local date (`Date.UTC(y, m, d) / 86400000` of the local fields).
fn ordinal(ms: f64) -> i64 {
    let tm = local(ms);
    days_from_civil(tm.tm_year as i64 + 1900, tm.tm_mon as i64 + 1, tm.tm_mday as i64)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn iso_date(days: i64) -> String {
    let (y, m, d) = super::commands::civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `renderActivity` over `activityCalendar` (no colors).
fn activity(entries: &[Json], from: f64, to: f64, width: usize) -> Vec<String> {
    let first = ordinal(from);
    let last = ordinal(to - 1.0);
    let weekday = (first + 4).rem_euclid(7); // 1970-01-01 was a Thursday; 0 = Sunday
    let monday = first - (weekday + 6) % 7;
    let total = if to <= from { 0 } else { (last - monday).div_euclid(7) + 1 };
    let count = total.min(53).min((width as i64 - 4).max(1));
    let start = monday + (total - count).max(0) * 7;
    let values: Vec<(String, f64)> =
        entries.iter().map(|day| (get_str(day, "date").unwrap_or("").to_owned(), num(day, "steps"))).collect();
    let mut levels: Vec<f64> = vec![];
    for (_, steps) in &values {
        if *steps > 0.0 && !levels.contains(steps) {
            levels.push(*steps);
        }
    }
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let level = |date: i64| -> i64 {
        if date < first || date > last {
            return -1;
        }
        let key = iso_date(date);
        let steps = values.iter().rev().find(|(d, _)| *d == key).map_or(0.0, |(_, s)| *s);
        if steps == 0.0 {
            return 0;
        }
        let index = levels.iter().position(|l| *l == steps).map_or(0, |i| i + 1);
        ((index as f64 / levels.len() as f64 * 4.0).ceil() as i64).max(1)
    };
    let mut months: Vec<(i64, &str)> = vec![];
    for week in 0..count {
        let day = (start + week * 7 + 3).max(first).min(last);
        let (_, month, _) = super::commands::civil(day);
        let label = MONTHS[(month - 1) as usize];
        if months.last().is_none_or(|(_, previous)| *previous != label) {
            months.push((week, label));
        }
    }
    let row: String = months
        .iter()
        .enumerate()
        .map(|(index, (week, label))| {
            let span = months.get(index + 1).map_or(count, |(next, _)| *next) - week;
            let shown = if index == months.len() - 1 || label.len() as i64 <= span { *label } else { "" };
            format!("{shown:<width$}", width = span.max(0) as usize)
        })
        .collect::<String>()
        .trim_end()
        .to_owned();
    let glyphs = ["·", "░", "▒", "▓", "█"];
    let mut lines = vec![
        if count < total { format!("activity · last {count} weeks") } else { "activity".into() },
        format!("   {row}"),
    ];
    let weekdays = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
    for (day, label) in weekdays.iter().enumerate() {
        let cells: String = (0..count)
            .map(|week| match level(start + week * 7 + day as i64) {
                l if l < 0 => " ",
                l => glyphs[l as usize],
            })
            .collect();
        lines.push(format!("{label} {cells}"));
        if day < weekdays.len() - 1 {
            lines.push(String::new());
        }
    }
    lines.push(String::new());
    lines.push(format!("   less {} more", glyphs.join("")));
    lines
}
