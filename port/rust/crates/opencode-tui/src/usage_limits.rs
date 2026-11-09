//! `util/usage-limits.ts`: the OpenCode Console limits on a model (what is left, how soon it runs out) and how they
//! read (dollars, reset times, scopes).
use opencode_protocol::Json;
use opencode_tui_sync::json::JsonExt;

/// `ModelLimit`: one limit on a model, with what has been spent against it and what is left.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelLimit {
    pub budget: Json,
    pub spent: f64,
    pub left: f64,
}

/// `ModelUsage`: what a model has left. The limit closest to running out sets it, since any limit that runs out
/// stops the model.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelUsage {
    pub left: f64,
    pub reached: bool,
    pub resets_at: f64,
    /// Every limit on the model, the one that sets `left` first.
    pub limits: Vec<ModelLimit>,
}

/// `UsageLevel`: `Low` means what's left won't cover three requests that fill the model's context, `Out` that it
/// won't cover one. Both depend on the model: a few dollars is plenty for a cheap model and almost nothing for an
/// expensive one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageLevel {
    Plenty,
    Low,
    Out,
    Reached,
}

/// `servedByConsole(provider)`: OpenCode Console serves the model, so the workspace's limits can apply; OpenCode Go
/// bills its own allowance.
pub fn served_by_console(provider: Option<&Json>) -> bool {
    provider.is_some_and(|p| p.str("integrationID") == Some("opencode") && p.str("id") != Some("opencode-go"))
}

fn limit(budget: &Json, spent: f64) -> ModelLimit {
    ModelLimit { budget: budget.clone(), spent, left: (budget.num("limit").unwrap_or(f64::NAN) - spent).max(0.0) }
}

/// `modelUsage(info, model, provider)`: the limits on a paid model, or nothing when none applies; free models stay
/// usable whatever is spent.
pub fn model_usage(info: &Json, model: &Json, provider: Option<&Json>) -> Option<ModelUsage> {
    let cost = model.arr("cost").map(Vec::as_slice).unwrap_or_default();
    if !cost.is_empty() && cost.iter().all(|c| c.num("input") == Some(0.0)) {
        return None;
    }
    let mut limits: Vec<ModelLimit> = info
        .arr("budgets")
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .flat_map(|budget| -> Vec<ModelLimit> {
            let spent = budget.num("spent").unwrap_or(f64::NAN);
            if budget.str("coverage") == Some("all") {
                return if served_by_console(provider) { vec![limit(budget, spent)] } else { Vec::new() };
            }
            let entry = budget.arr("models").and_then(|models| {
                models.iter().find(|item| {
                    item.str("providerID") == model.str("providerID") && item.str("modelID") == model.str("id")
                })
            });
            let Some(entry) = entry else { return Vec::new() };
            vec![limit(budget, if budget.str("per") == Some("model") { entry.num("spent").unwrap_or(0.0) } else { spent })]
        })
        .collect();
    // `toSorted((a, b) => a.left - b.left)`: stable, NaN comparing equal.
    limits.sort_by(|a, b| a.left.partial_cmp(&b.left).unwrap_or(std::cmp::Ordering::Equal));
    let binding = limits.first()?;
    Some(ModelUsage {
        left: binding.left,
        reached: limits.iter().any(|item| item.spent >= item.budget.num("limit").unwrap_or(f64::NAN)),
        resets_at: limits.iter().map(|item| item.budget.num("resetsAt").unwrap_or(f64::NAN)).fold(f64::INFINITY, js_min),
        limits,
    })
}

/// `Math.min` of two numbers: NaN wins.
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}

/// `usageLevel(usage, model)`.
pub fn usage_level(usage: &ModelUsage, model: &Json) -> UsageLevel {
    if usage.reached || usage.left <= 0.0 {
        return UsageLevel::Reached;
    }
    let context = model.get("limit").and_then(|l| l.num("context")).unwrap_or(f64::NAN);
    let input = model.arr("cost").and_then(|c| c.first()).and_then(|c| c.num("input")).unwrap_or(0.0);
    let request = (context / 1_000_000.0) * input;
    if usage.left < request {
        return UsageLevel::Out;
    }
    if usage.left < request * 3.0 {
        return UsageLevel::Low;
    }
    UsageLevel::Plenty
}

/// `value` with `digits` fraction digits, integer digits grouped by thousands. ICU rounds the shortest decimal that
/// reads back as the double, half away from zero ("halfExpand"): 2.675 rounds to 2.68 though the double is a little
/// below it.
fn grouped_fixed(value: f64, digits: usize) -> String {
    // Rust's `Display` for f64 is that shortest round-trip decimal, never in exponent form.
    let shortest = format!("{}", value.abs());
    let (int, frac) = shortest.split_once('.').unwrap_or((&shortest, ""));
    let mut number: Vec<u8> = int.bytes().chain(frac.bytes().chain(std::iter::repeat(b'0')).take(digits)).collect();
    if frac.as_bytes().get(digits).is_some_and(|d| *d >= b'5') {
        let mut i = number.len();
        loop {
            if i == 0 {
                number.insert(0, b'1');
                break;
            }
            i -= 1;
            if number[i] == b'9' {
                number[i] = b'0';
            } else {
                number[i] += 1;
                break;
            }
        }
    }
    let split = number.len() - digits;
    let (int, frac) = number.split_at(split);
    let mut out = String::new();
    for (i, c) in int.iter().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c as char);
    }
    if digits > 0 {
        out.push('.');
        out.push_str(std::str::from_utf8(frac).unwrap_or_default());
    }
    out
}

/// `formatDollars(value)`: dollars as limits read: whole amounts without cents, and a sliver as "<$0.01" rather than
/// "$0.00".
pub fn format_dollars(value: f64) -> String {
    if value > 0.0 && value < 0.01 {
        return "<$0.01".into();
    }
    if value.is_nan() {
        return "$NaN".into();
    }
    if value.is_infinite() {
        return format!("{}$∞", if value < 0.0 { "-" } else { "" });
    }
    // `Intl.NumberFormat("en-US", { style: "currency", currency: "USD", minimumFractionDigits: integer ? 0 : 2,
    // maximumFractionDigits: 2 })`.
    let digits = if value.fract() == 0.0 { 0 } else { 2 };
    // The sign shows for every negative value, -0 and what rounds to zero included.
    format!("{}${}", if value.is_sign_negative() { "-" } else { "" }, grouped_fixed(value, digits))
}

/// A local calendar time (`new Date(at)`'s getters; `month` from 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hours: u32,
    pub minutes: u32,
    pub seconds: u32,
}

/// `new Date(at)` in the process's time zone (`TZ`), or `None` for an invalid date.
pub(crate) fn local_time(at_ms: f64) -> Option<LocalTime> {
    if !at_ms.is_finite() || at_ms.abs() > 8.64e15 {
        return None;
    }
    let seconds = (at_ms / 1000.0).floor() as libc::time_t;
    // SAFETY: `localtime_r` writes only the `tm` it is given, which outlives the call.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&seconds, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(LocalTime {
        year: tm.tm_year as i64 + 1900,
        month: tm.tm_mon as u32,
        day: tm.tm_mday as u32,
        hours: tm.tm_hour as u32,
        minutes: tm.tm_min as u32,
        seconds: tm.tm_sec as u32,
    })
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `Date.now()`.
pub(crate) fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

/// `formatReset(at)`: when limits reset: the day, with the time unless it is midnight here, or just the time when
/// that is today.
pub fn format_reset(at: f64) -> String {
    let Some(date) = local_time(at) else { return "Invalid Date".into() };
    // `toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" })`, the narrow no-break space before
    // AM/PM written as a space.
    let hour = match date.hours % 12 {
        0 => 12,
        h => h,
    };
    let time = format!("{hour}:{:02} {}", date.minutes, if date.hours < 12 { "AM" } else { "PM" });
    let today = local_time(now_ms());
    if today.is_some_and(|t| (t.year, t.month, t.day) == (date.year, date.month, date.day)) {
        return format!("{time} today");
    }
    let day = format!("{} {}", MONTHS[date.month as usize], date.day);
    if date.hours == 0 && date.minutes == 0 { day } else { format!("{day}, {time}") }
}

fn response_body(error: Option<&Json>) -> &str {
    error.and_then(|e| e.get("response")).and_then(|r| r.str("body")).unwrap_or("")
}

fn capture(pattern: &str, text: &str) -> Option<String> {
    regex::Regex::new(pattern).ok()?.captures(text)?.get(1).map(|m| m.as_str().to_string())
}

/// `Date.parse` of the ISO 8601 forms (`YYYY`, `YYYY-MM`, `YYYY-MM-DD`, then `THH:mm`, `:ss`, `.sss` and `Z` or
/// `±HH:mm`); date-only forms are UTC, date-times without an offset local time, as in JS.
fn parse_date(text: &str) -> Option<f64> {
    let re = regex::Regex::new(
        r"^([+-]\d{6}|\d{4})(?:-(\d{2})(?:-(\d{2}))?)?(?:[T ](\d{2}):(\d{2})(?::(\d{2})(?:[.,](\d+))?)?)?\s*(Z|[+-]\d{2}(?::?\d{2})?)?$",
    )
    .ok()?;
    let c = re.captures(text.trim())?;
    let num = |i: usize| c.get(i).map(|m| m.as_str().parse::<i64>().ok()).unwrap_or(Some(0));
    let year = c.get(1)?.as_str().parse::<i64>().ok()?;
    let month = c.get(2).map(|m| m.as_str().parse::<i64>().ok()).unwrap_or(Some(1))?;
    let day = c.get(3).map(|m| m.as_str().parse::<i64>().ok()).unwrap_or(Some(1))?;
    let (hour, minute, second) = (num(4)?, num(5)?, num(6)?);
    let millis = c.get(7).map(|m| format!("{:0<3}", &m.as_str()[..m.as_str().len().min(3)]).parse::<i64>().unwrap_or(0)).unwrap_or(0);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 24 || minute > 59 || second > 59 {
        return None;
    }
    if hour == 24 && (minute != 0 || second != 0 || millis != 0) {
        return None;
    }
    // Days from the civil date (Howard Hinnant's days_from_civil).
    let (y, m) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let utc = ((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + millis;
    let offset_ms = match c.get(8).map(|m| m.as_str()) {
        Some("Z") => 0,
        Some(offset) => {
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let digits: String = offset[1..].chars().filter(char::is_ascii_digit).collect();
            let hours = digits.get(..2)?.parse::<i64>().ok()?;
            let minutes = digits.get(2..4).map(|m| m.parse::<i64>().unwrap_or(0)).unwrap_or(0);
            sign * (hours * 60 + minutes) * 60_000
        }
        // A date-time without an offset is local time; a date alone is UTC.
        None if c.get(4).is_some() => local_offset_ms(utc as f64)?,
        None => 0,
    };
    Some((utc - offset_ms) as f64)
}

/// The local zone's offset from UTC at about `utc_ms` (`tm_gmtoff`).
fn local_offset_ms(utc_ms: f64) -> Option<i64> {
    let seconds = (utc_ms / 1000.0).floor() as libc::time_t;
    // SAFETY: as in `local_time`.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&seconds, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(tm.tm_gmtoff as i64 * 1000)
}

/// `refusalReset(error)`: when a refused request's limit resets; OpenCode Console names it `resetsAt` in every
/// protocol's error envelope.
pub fn refusal_reset(error: Option<&Json>) -> Option<f64> {
    capture(r#""resetsAt"\s*:\s*"([^"]+)""#, response_body(error)).and_then(|at| parse_date(&at))
}

/// `retryReset(error, base)`: when a refused request may retry, from its `retry-after` counted from `base`; nothing
/// past 45 days, as a bad header.
pub fn retry_reset(error: Option<&Json>, base: f64) -> Option<f64> {
    let delay = error.and_then(|e| e.num("retryAfterMs"))?;
    if !delay.is_finite() || delay < 0.0 || delay > 45.0 * 86_400_000.0 {
        return None;
    }
    let at = base + delay;
    (at.is_finite() && at.abs() <= 8.64e15).then_some(at)
}

/// `goLimitName(error)`: which Go limit refused a request (5 hour, weekly or monthly), as OpenCode Console names it in
/// the metadata.
pub fn go_limit_name(error: Option<&Json>) -> Option<String> {
    capture(r#""limitName"\s*:\s*"([^"]+)""#, response_body(error))
}

/// `goLimitLink(error)`: the OpenCode Console Go page for the workspace a Go usage limit names, where it can turn on
/// Extra Usage.
pub fn go_limit_link(error: Option<&Json>) -> String {
    match capture(r#""workspace"\s*:\s*"((?:org|wrk)_[A-Za-z0-9_-]+)""#, response_body(error)) {
        Some(workspace) => format!("https://opencode.ai/console/{workspace}/go"),
        None => "https://opencode.ai/console".into(),
    }
}

/// `limitScope(budget, workspace)`: who spends from a limit and, when it pools spend across models, which ones. A
/// limit on each model separately names only who spends from it.
pub fn limit_scope(budget: &Json, workspace: Option<&str>) -> String {
    let who = if budget.str("scope") == Some("each") {
        "Only you".to_string()
    } else if budget.bool("shared") == Some(true) {
        format!("Everyone in {}", workspace.unwrap_or("the workspace"))
    } else {
        "You and others".to_string()
    };
    if budget.str("per") == Some("model") {
        return who;
    }
    format!("{who} · {}", limit_models(budget))
}

fn limit_models(budget: &Json) -> String {
    if budget.str("coverage") == Some("all") {
        return "All models".into();
    }
    let vendors: Vec<String> =
        budget.arr("vendors").map(|v| v.iter().map(opencode_tui_sync::js::string).collect()).unwrap_or_default();
    let vendors = (!vendors.is_empty()).then(|| list(&vendors));
    let price = budget.num("inputPriceAtLeast").map(|p| format!("{} per 1M", format_dollars(p)));
    match (vendors, price) {
        (Some(vendors), Some(price)) => format!("{vendors} models over {price}"),
        (Some(vendors), None) => format!("All {vendors} models"),
        (None, Some(price)) => format!("Models over {price}"),
        (None, None) => {
            let names: Vec<String> = budget
                .arr("models")
                .map(|models| models.iter().map(|m| m.get("name").map(opencode_tui_sync::js::string).unwrap_or_else(|| "undefined".into())).collect())
                .unwrap_or_default();
            if names.len() > 3 {
                return format!("{} +{} more", names[..3].join(", "), names.len() - 3);
            }
            names.join(", ")
        }
    }
}

/// `new Intl.ListFormat("en-US", { style: "long", type: "conjunction" }).format(items)`.
fn list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollars_read_as_intl_formats_them() {
        assert_eq!(format_dollars(5.0), "$5");
        assert_eq!(format_dollars(1234.5), "$1,234.50");
        assert_eq!(format_dollars(0.125), "$0.13");
        assert_eq!(format_dollars(0.005), "<$0.01");
        assert_eq!(format_dollars(0.0), "$0");
        assert_eq!(format_dollars(-2.5), "-$2.50");
        assert_eq!(format_dollars(999.999), "$1,000.00");
        assert_eq!(format_dollars(1_000_000.0), "$1,000,000");
        assert_eq!(format_dollars(2.675), "$2.68");
        assert_eq!(format_dollars(1.005), "$1.01");
        assert_eq!(format_dollars(-0.001), "-$0.00");
        assert_eq!(format_dollars(-0.0), "-$0");
    }

    #[test]
    fn lists_join_as_intl_does() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(list(&s(&["Anthropic"])), "Anthropic");
        assert_eq!(list(&s(&["Anthropic", "OpenAI"])), "Anthropic and OpenAI");
        assert_eq!(list(&s(&["A", "B", "C"])), "A, B, and C");
    }

    #[test]
    fn utc_dates_parse() {
        assert_eq!(parse_date("2026-01-05T10:00:00Z"), Some(1_767_607_200_000.0));
        assert_eq!(parse_date("2026-01-05T10:00:00.250+01:00"), Some(1_767_603_600_250.0));
        assert_eq!(parse_date("2026-01-05"), Some(1_767_571_200_000.0));
        assert_eq!(parse_date("soon"), None);
    }
}
