//! Cron expressions (effect/src/Cron.ts `parse` and `next`): five or six fields (seconds first when six), each a
//! list of values, ranges, steps, `*` and month or weekday names. A day of the month and a day of the week both
//! restrict the date when either is a wildcard with a step; otherwise either may match.

use crate::zone::{self, DAY_MS, Zone};

#[derive(Debug, Clone)]
pub struct Cron {
    pub seconds: Vec<i64>,
    minutes: Vec<i64>,
    hours: Vec<i64>,
    days: Vec<i64>,
    months: Vec<i64>,
    weekdays: Vec<i64>,
    and: bool,
}

struct Options {
    min: i64,
    max: i64,
    aliases: &'static [(&'static str, i64)],
    sunday: bool,
}

const MONTHS: &[(&str, i64)] = &[
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];
const WEEKDAYS: &[(&str, i64)] = &[("sun", 0), ("mon", 1), ("tue", 2), ("wed", 3), ("thu", 4), ("fri", 5), ("sat", 6)];

/// `/^\d+$/` then `Number`, else `NaN` (`None`).
fn decimal(text: &str) -> Option<i64> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())).then(|| text.parse().unwrap_or(i64::MAX))
}

fn alias_or_value(field: &str, options: &Options) -> Option<i64> {
    let lower = field.to_lowercase();
    options.aliases.iter().find(|(name, _)| *name == lower).map(|(_, value)| *value).or_else(|| decimal(field))
}

/// `[value, step]`: `None` for a missing step, `Some(None)` for one that is not a number.
fn split_step(input: &str) -> (&str, Option<Option<i64>>) {
    match input.find('/') {
        Some(at) => (&input[..at], Some(decimal(&input[at + 1..]))),
        None => (input, None),
    }
}

/// `parseSegment`: the values and whether the field starts with a wildcard.
fn segment(input: &str, options: &Options) -> Result<(Vec<i64>, bool), String> {
    let mut values: Vec<i64> = Vec::new();
    let fields: Vec<&str> = input.split(',').collect();
    let first = split_step(fields[0]);
    let wildcard = first.0 == "*";
    let ignore = wildcard && matches!(first.1, None | Some(Some(1)));
    let mut add = |value: i64| {
        if !ignore {
            let value = if options.sunday && value == 7 { 0 } else { value };
            if !values.contains(&value) {
                values.push(value);
            }
        }
    };
    for (index, field) in fields.iter().enumerate() {
        let (raw, step) = if index == 0 { first } else { split_step(field) };
        let step = match step {
            None => None,
            Some(None) => return Err("Expected step value to be a positive integer".into()),
            Some(Some(step)) if step < 1 => return Err("Expected step value to be greater than 0".into()),
            Some(Some(step)) if step > options.max => {
                return Err(format!("Expected step value to be less than or equal to {}", options.max));
            }
            Some(Some(step)) => Some(step),
        };
        if raw == "*" {
            if index == 0 && matches!(step, None | Some(1)) {
                continue;
            }
            let mut i = options.min;
            while i <= options.max {
                add(i);
                i += step.unwrap_or(1);
            }
            continue;
        }
        let (left, right) = match raw.find('-') {
            Some(at) => (alias_or_value(&raw[..at], options), Some(alias_or_value(&raw[at + 1..], options))),
            None => (alias_or_value(raw, options), None),
        };
        let range = || format!("Expected a value between {} and {}", options.min, options.max);
        let Some(left) = left else { return Err("Expected a positive integer".into()) };
        if left < options.min || left > options.max {
            return Err(range());
        }
        let end = match right {
            None => {
                if step.is_none() {
                    left
                } else {
                    options.max
                }
            }
            Some(None) => return Err("Expected a positive integer".into()),
            Some(Some(right)) if right < options.min || right > options.max => return Err(range()),
            Some(Some(right)) if left > right => return Err("Invalid value range".into()),
            Some(Some(right)) => right,
        };
        let mut i = left;
        while i <= end {
            add(i);
            i += step.unwrap_or(1);
        }
    }
    values.sort_unstable();
    Ok((values, wildcard))
}

/// `Cron.parse`: the expression, or the parse error's message.
pub fn parse(text: &str) -> Result<Cron, String> {
    let mut segments: Vec<&str> = text.split(|c: char| c.is_whitespace()).filter(|s| !s.is_empty()).collect();
    if segments.len() != 5 && segments.len() != 6 {
        return Err("Invalid number of segments in cron expression".into());
    }
    if segments.len() == 5 {
        segments.insert(0, "0");
    }
    let plain = |min, max| Options { min, max, aliases: &[], sunday: false };
    let (seconds, _) = segment(segments[0], &plain(0, 59))?;
    let (minutes, _) = segment(segments[1], &plain(0, 59))?;
    let (hours, _) = segment(segments[2], &plain(0, 23))?;
    let (days, days_wildcard) = segment(segments[3], &plain(1, 31))?;
    let (months, _) = segment(segments[4], &Options { min: 1, max: 12, aliases: MONTHS, sunday: false })?;
    let (weekdays, weekdays_wildcard) = segment(segments[5], &Options { min: 0, max: 7, aliases: WEEKDAYS, sunday: true })?;
    let and = (days_wildcard || weekdays_wildcard) && !days.is_empty() && !weekdays.is_empty();
    Ok(Cron { seconds, minutes, hours, days, months, weekdays, and })
}

fn any(values: &[i64], value: i64) -> bool {
    values.is_empty() || values.contains(&value)
}

impl Cron {
    fn date_matches(&self, day: i64, weekday: i64) -> bool {
        if self.and || self.days.is_empty() || self.weekdays.is_empty() {
            return any(&self.days, day) && any(&self.weekdays, weekday);
        }
        self.days.contains(&day) || self.weekdays.contains(&weekday)
    }

    /// `Cron.next`: the first matching instant strictly after `now` (whole seconds), in `zone`'s wall-clock time
    /// (repeated times resolve to their first occurrence, times in a gap to the instant after it), or `None` when the
    /// expression never matches a date (within a 400-year Gregorian cycle).
    pub fn next(&self, zone: &Zone, now: i64) -> Option<i64> {
        let range = |values: &[i64], max: i64| -> Vec<i64> { if values.is_empty() { (0..=max).collect() } else { values.to_vec() } };
        let (seconds, minutes, hours) = (range(&self.seconds, 59), range(&self.minutes, 59), range(&self.hours, 23));
        let start = zone.to_wall(now);
        let first_day = start.div_euclid(DAY_MS) - 1;
        for day in first_day..first_day + 146_098 {
            let (_, month, date) = zone::civil_from_days(day);
            if !any(&self.months, month) || !self.date_matches(date, zone::weekday_of(day)) {
                continue;
            }
            for hour in &hours {
                for minute in &minutes {
                    let base = day * DAY_MS + (hour * 60 + minute) * 60_000;
                    // Offsets move by less than a day: anything a day before the start cannot be after it.
                    if base + 60_000 + DAY_MS < start {
                        continue;
                    }
                    for second in &seconds {
                        let at = zone.from_wall(base + second * 1000);
                        if at > now {
                            return Some(at);
                        }
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_errors_match_effect() {
        assert_eq!(parse("not a cron").unwrap_err(), "Invalid number of segments in cron expression");
        assert_eq!(parse("61 * * * *").unwrap_err(), "Expected a value between 0 and 59");
        assert_eq!(parse("*/0 * * * *").unwrap_err(), "Expected step value to be greater than 0");
        assert_eq!(parse("x * * * *").unwrap_err(), "Expected a positive integer");
        assert_eq!(parse("5-1 * * * *").unwrap_err(), "Invalid value range");
        assert_eq!(parse("*/10 * * * * *").unwrap().seconds.len(), 6);
        assert_eq!(parse("0 9 * * 1-5").unwrap().seconds, vec![0]);
    }

    #[test]
    fn next_slots() {
        let utc = Zone::utc();
        let now = zone::make("2021-01-01T00:00:00Z").unwrap();
        let cron = parse("0 0 4 8-14 * *").unwrap();
        assert_eq!(cron.next(&utc, now).map(zone::iso).as_deref(), Some("2021-01-08T04:00:00.000Z"));
        let weekdays = parse("0 9 * * 1-5").unwrap();
        // 2021-01-01 was a Friday.
        assert_eq!(weekdays.next(&utc, now).map(zone::iso).as_deref(), Some("2021-01-01T09:00:00.000Z"));
        assert_eq!(weekdays.next(&utc, zone::make("2021-01-01T09:00:00Z").unwrap()).map(zone::iso).as_deref(), Some("2021-01-04T09:00:00.000Z"));
        assert!(parse("0 0 30 2 *").unwrap().next(&utc, now).is_none());
    }
}
