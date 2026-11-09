//! IANA time zones as Effect's `DateTime` and JS `Intl` see them (effect/src/internal/dateTime.ts `zoneMakeNamed`,
//! `zoneMakeLocal`, `makeZonedUnsafe` with `adjustForTimeZone`), read from the system's tzdata: the TZif files under
//! `$TZDIR` or /usr/share/zoneinfo, each with its POSIX footer rule for instants after its last transition. Also the
//! civil-date arithmetic the schedules and statistics need, and `new Date(string)` for the ISO forms.

use std::path::PathBuf;

pub const DAY_MS: i64 = 86_400_000;

/// A time zone: a named IANA zone or a fixed offset (`+01:00`, which `Intl` also accepts by name).
#[derive(Debug, Clone)]
pub struct Zone {
    /// `DateTime.zoneToString`: the zone's name as `Intl` resolves it.
    pub id: String,
    kind: Kind,
}

#[derive(Debug, Clone)]
enum Kind {
    Fixed(i64),
    Rules { transitions: Vec<(i64, i64)>, initial: i64, footer: Option<Posix> },
}

impl Zone {
    pub fn utc() -> Zone {
        Zone { id: "UTC".into(), kind: Kind::Fixed(0) }
    }

    /// `DateTime.zoneMakeNamed`: a zone `Intl.DateTimeFormat` accepts, under the name it resolves (the IANA name in
    /// its canonical case; `utc` is `UTC`), or `None`.
    pub fn named(name: &str) -> Option<Zone> {
        if name.eq_ignore_ascii_case("utc") {
            return Some(Zone::utc());
        }
        if let Some(offset) = parse_offset_zone(name) {
            return Some(Zone { id: name.to_owned(), kind: Kind::Fixed(offset) });
        }
        if name.is_empty() || name.starts_with('/') || name.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
            return None;
        }
        let (path, id) = find(name)?;
        let bytes = std::fs::read(path).ok()?;
        let (transitions, initial, footer) = parse_tzif(&bytes)?;
        Some(Zone { id, kind: Kind::Rules { transitions, initial, footer } })
    }

    /// `DateTime.zoneMakeLocal`: the process's zone (`TZ`, else /etc/localtime), UTC when it has no IANA name.
    pub fn local() -> Zone {
        let name = match std::env::var("TZ") {
            Ok(tz) if !tz.is_empty() => Some(tz.trim_start_matches(':').to_owned()),
            _ => std::fs::read_link("/etc/localtime")
                .ok()
                .and_then(|target| target.to_string_lossy().split_once("zoneinfo/").map(|(_, name)| name.to_owned())),
        };
        name.and_then(|name| Zone::named(&name)).unwrap_or_else(Zone::utc)
    }

    /// The zone's offset from UTC at an instant, in milliseconds.
    pub fn offset_ms(&self, utc_ms: i64) -> i64 {
        match &self.kind {
            Kind::Fixed(offset) => offset * 1000,
            Kind::Rules { transitions, initial, footer } => {
                let seconds = utc_ms.div_euclid(1000);
                let index = transitions.partition_point(|(at, _)| *at <= seconds);
                let offset = match footer {
                    Some(rule) if index == transitions.len() => rule.offset(seconds),
                    _ if index == 0 => *initial,
                    _ => transitions[index - 1].1,
                };
                offset * 1000
            }
        }
    }

    /// The instant of a wall-clock time in this zone (`makeZonedFromAdjusted`, `"compatible"` disambiguation): the
    /// earlier instant of a repeated time, and for a time in a gap the instant the time before the gap continues to.
    pub fn from_wall(&self, wall_ms: i64) -> i64 {
        let before = self.offset_ms(wall_ms - DAY_MS);
        let after = self.offset_ms(wall_ms + DAY_MS);
        let valid: Vec<i64> =
            [before, after].into_iter().filter(|offset| self.offset_ms(wall_ms - offset) == *offset).map(|offset| wall_ms - offset).collect();
        valid.into_iter().min().unwrap_or(wall_ms - before)
    }

    /// The wall-clock time of an instant in this zone, as epoch milliseconds of the same civil time in UTC.
    pub fn to_wall(&self, utc_ms: i64) -> i64 {
        utc_ms + self.offset_ms(utc_ms)
    }
}

/// `+01:00`, `-0530`, `+01`: the offset zones `Intl` accepts by name, in seconds.
fn parse_offset_zone(name: &str) -> Option<i64> {
    let sign = match name.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = name[1..].chars().filter(|c| *c != ':').collect();
    if !digits.bytes().all(|b| b.is_ascii_digit()) || !matches!(digits.len(), 2 | 4) || (name.contains(':') && name.len() != 6) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = if digits.len() == 4 { digits[2..].parse().ok()? } else { 0 };
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 3600 + minutes * 60))
}

fn zoneinfo() -> PathBuf {
    std::env::var_os("TZDIR").filter(|dir| !dir.is_empty()).map(PathBuf::from).unwrap_or_else(|| "/usr/share/zoneinfo".into())
}

/// The zone file of a name, matching each path segment case-insensitively as `Intl` does, and its canonical name.
fn find(name: &str) -> Option<(PathBuf, String)> {
    let root = zoneinfo();
    let exact = root.join(name);
    if exact.is_file() {
        return Some((exact, name.to_owned()));
    }
    let mut path = root;
    let mut parts = Vec::new();
    for part in name.split('/') {
        let found = std::fs::read_dir(&path).ok()?.filter_map(|entry| entry.ok()).find_map(|entry| {
            let file = entry.file_name().to_string_lossy().into_owned();
            file.eq_ignore_ascii_case(part).then_some(file)
        })?;
        path = path.join(&found);
        parts.push(found);
    }
    path.is_file().then(|| (path, parts.join("/")))
}

type Tzif = (Vec<(i64, i64)>, i64, Option<Posix>);

/// A TZif file (RFC 8536): its transitions as `(instant, offset)` in seconds, the offset before the first one, and
/// the footer rule of version 2 and later files.
fn parse_tzif(bytes: &[u8]) -> Option<Tzif> {
    fn be(bytes: &[u8], at: usize, size: usize) -> Option<i64> {
        let slice = bytes.get(at..at + size)?;
        Some(match size {
            4 => i32::from_be_bytes(slice.try_into().ok()?) as i64,
            _ => i64::from_be_bytes(slice.try_into().ok()?),
        })
    }
    fn counts(bytes: &[u8], at: usize) -> Option<[usize; 6]> {
        if bytes.get(at..at + 4)? != b"TZif" {
            return None;
        }
        let mut out = [0usize; 6];
        for (i, count) in out.iter_mut().enumerate() {
            *count = u32::from_be_bytes(bytes.get(at + 20 + i * 4..at + 24 + i * 4)?.try_into().ok()?) as usize;
        }
        Some(out)
    }
    let version = *bytes.get(4)?;
    let [isut, isstd, leap, time, types, chars] = counts(bytes, 0)?;
    let v1 = time * 5 + types * 6 + chars + leap * 8 + isstd + isut;
    let (start, size, header) = if version >= b'2' { (44 + v1, 8, counts(bytes, 44 + v1)?) } else { (0, 4, [isut, isstd, leap, time, types, chars]) };
    let [isut, isstd, leap, time, types, chars] = header;
    let data = start + 44;
    let times = data;
    let indices = times + time * size;
    let infos = indices + time;
    let offset_of = |index: usize| be(bytes, infos + index * 6, 4);
    let mut transitions = Vec::with_capacity(time);
    for i in 0..time {
        let at = be(bytes, times + i * size, size)?;
        let index = *bytes.get(indices + i)? as usize;
        transitions.push((at, offset_of(index)?));
    }
    let initial = offset_of(0)?;
    let footer = if version >= b'2' {
        let end = infos + types * 6 + chars + leap * 12 + isstd + isut;
        let rest = bytes.get(end..)?;
        let text = std::str::from_utf8(rest).ok()?.trim_matches('\n');
        if text.is_empty() { None } else { Posix::parse(text) }
    } else {
        None
    };
    Some((transitions, initial, footer))
}

/// A POSIX TZ rule (`EST5EDT,M3.2.0,M11.1.0`): the standard offset, and the daylight offset with its start and end.
#[derive(Debug, Clone)]
struct Posix {
    std: i64,
    dst: Option<(i64, Rule, i64, Rule, i64)>,
}

#[derive(Debug, Clone, Copy)]
enum Rule {
    /// `Jn`: day 1..=365, February 29 never counted.
    Julian(i64),
    /// `n`: day 0..=365, counting February 29.
    Day(i64),
    /// `Mm.w.d`: weekday `d` of week `w` (5 is the last) of month `m`.
    Month(i64, i64, i64),
}

impl Posix {
    fn parse(text: &str) -> Option<Posix> {
        let mut rest = text;
        name(&mut rest)?;
        let std = -offset(&mut rest)?;
        if rest.is_empty() {
            return Some(Posix { std, dst: None });
        }
        name(&mut rest)?;
        let dst = if rest.starts_with(',') || rest.is_empty() { std + 3600 } else { -offset(&mut rest)? };
        let (start, start_time, end, end_time) = if rest.is_empty() {
            (Rule::Month(3, 2, 0), 7200, Rule::Month(11, 1, 0), 7200)
        } else {
            rest = rest.strip_prefix(',')?;
            let (start, start_time) = rule(&mut rest)?;
            rest = rest.strip_prefix(',')?;
            let (end, end_time) = rule(&mut rest)?;
            (start, start_time, end, end_time)
        };
        Some(Posix { std, dst: Some((dst, start, start_time, end, end_time)) })
    }

    fn offset(&self, utc: i64) -> i64 {
        let Some((dst, start, start_time, end, end_time)) = self.dst else { return self.std };
        let (year, _, _) = civil_from_days((utc + self.std).div_euclid(86_400));
        let begins = rule_day(start, year) * 86_400 + start_time - self.std;
        let ends = rule_day(end, year) * 86_400 + end_time - dst;
        let daylight = if begins < ends { utc >= begins && utc < ends } else { !(utc >= ends && utc < begins) };
        if daylight { dst } else { self.std }
    }
}

fn name(rest: &mut &str) -> Option<()> {
    if let Some(quoted) = rest.strip_prefix('<') {
        let end = quoted.find('>')?;
        *rest = &quoted[end + 1..];
        return Some(());
    }
    let length = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    if length < 3 {
        return None;
    }
    *rest = &rest[length..];
    Some(())
}

/// `[+-]hh[:mm[:ss]]` in seconds (hours up to 167, as the time of a rule allows).
fn offset(rest: &mut &str) -> Option<i64> {
    let sign = match rest.as_bytes().first() {
        Some(b'-') => {
            *rest = &rest[1..];
            -1
        }
        Some(b'+') => {
            *rest = &rest[1..];
            1
        }
        _ => 1,
    };
    let mut total = 0;
    for (index, scale) in [3600, 60, 1].into_iter().enumerate() {
        if index > 0 {
            match rest.strip_prefix(':') {
                Some(next) => *rest = next,
                None => break,
            }
        }
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        total += rest[..digits].parse::<i64>().ok()? * scale;
        *rest = &rest[digits..];
    }
    Some(sign * total)
}

fn rule(rest: &mut &str) -> Option<(Rule, i64)> {
    let number = |rest: &mut &str| -> Option<i64> {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let value = rest.get(..digits)?.parse().ok()?;
        *rest = &rest[digits..];
        Some(value)
    };
    let parsed = if let Some(next) = rest.strip_prefix('J') {
        *rest = next;
        Rule::Julian(number(rest)?)
    } else if let Some(next) = rest.strip_prefix('M') {
        *rest = next;
        let month = number(rest)?;
        *rest = rest.strip_prefix('.')?;
        let week = number(rest)?;
        *rest = rest.strip_prefix('.')?;
        Rule::Month(month, week, number(rest)?)
    } else {
        Rule::Day(number(rest)?)
    };
    let time = match rest.strip_prefix('/') {
        Some(next) => {
            *rest = next;
            offset(rest)?
        }
        None => 7200,
    };
    Some((parsed, time))
}

/// The day (since the epoch) a rule names in a year.
fn rule_day(rule: Rule, year: i64) -> i64 {
    let january = days_from_civil(year, 1, 1);
    match rule {
        Rule::Julian(n) => january + n - 1 + i64::from(is_leap(year) && n >= 60),
        Rule::Day(n) => january + n,
        Rule::Month(month, week, weekday) => {
            let first = days_from_civil(year, month, 1);
            let mut day = 1 + (weekday - weekday_of(first)).rem_euclid(7) + (week - 1) * 7;
            while day > days_in_month(year, month) {
                day -= 7;
            }
            first + day - 1
        }
    }
}

pub fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The day of the week of a day since the epoch (0 is Sunday).
pub fn weekday_of(days: i64) -> i64 {
    (days + 4).rem_euclid(7)
}

/// Days since the epoch of a civil date (Howard Hinnant's algorithm; the month may run past 12 or below 1).
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year + (month - 1).div_euclid(12);
    let month = (month - 1).rem_euclid(12) + 1;
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date `(year, month, day)` of a day since the epoch.
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// `new Date(text).getTime()` for the ISO forms JavaScriptCore reads: `YYYY[-MM[-DD]]` or `±YYYYYY...`, then
/// `[T ]HH:mm[:ss[.fff]]`, then `Z` or `±HH[:]mm`. Without an offset the time is local, which callers avoid by
/// appending `Z`. Days past the end of the month carry into the next one, as JavaScriptCore does.
pub fn parse_date(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize, count: usize| -> Option<i64> {
        let slice = b.get(*i..*i + count)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *i += count;
        std::str::from_utf8(slice).ok()?.parse().ok()
    };
    let year = match b.first()? {
        b'+' | b'-' => {
            let sign = if b[0] == b'-' { -1 } else { 1 };
            i = 1;
            sign * digits(&mut i, 6)?
        }
        _ => digits(&mut i, 4)?,
    };
    let mut month = 1;
    let mut day = 1;
    if b.get(i) == Some(&b'-') {
        i += 1;
        month = digits(&mut i, 2)?;
        if b.get(i) == Some(&b'-') {
            i += 1;
            day = digits(&mut i, 2)?;
        }
    }
    let (mut hour, mut minute, mut second, mut millis) = (0, 0, 0, 0);
    if matches!(b.get(i), Some(b'T' | b't' | b' ')) {
        i += 1;
        hour = digits(&mut i, 2)?;
        if b.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        minute = digits(&mut i, 2)?;
        if b.get(i) == Some(&b':') {
            i += 1;
            second = digits(&mut i, 2)?;
            if b.get(i) == Some(&b'.') {
                i += 1;
                let start = i;
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                if i == start {
                    return None;
                }
                let fraction = format!("{:0<3}", &text[start..i.min(start + 3)]);
                millis = fraction.parse().ok()?;
            }
        }
    }
    let offset = match b.get(i) {
        None => return None,
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) => {
            let sign = if *sign == b'-' { -1 } else { 1 };
            i += 1;
            let hours = digits(&mut i, 2)?;
            if b.get(i) == Some(&b':') {
                i += 1;
            }
            let minutes = digits(&mut i, 2)?;
            if i != b.len() || hours > 23 || minutes > 59 {
                return None;
            }
            sign * (hours * 60 + minutes)
        }
        _ => return None,
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 24 || minute > 59 || second > 59 {
        return None;
    }
    if hour == 24 && (minute != 0 || second != 0 || millis != 0) {
        return None;
    }
    let ms = days_from_civil(year, month, day) * DAY_MS + ((hour * 60 + minute) * 60 + second) * 1000 + millis
        - offset * 60_000;
    (ms.abs() <= 8_640_000_000_000_000).then_some(ms)
}

/// Effect's `hasZone`: whether a date string names its zone (otherwise `DateTime.make` reads it as UTC).
pub fn has_zone(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.contains('Z')
        || text.contains("GMT")
        || text.ends_with(']')
        || lower.ends_with('z')
        || lower.ends_with("gmt")
        || {
            let b = text.as_bytes();
            let n = b.len();
            let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
            let sign = |i: usize| matches!(b.get(i), Some(b'+' | b'-'));
            (n >= 3 && sign(n - 3) && digit(n - 2) && digit(n - 1))
                || (n >= 5 && sign(n - 5) && (n - 4..n).all(digit))
                || (n >= 6 && sign(n - 6) && digit(n - 5) && digit(n - 4) && b[n - 3] == b':' && digit(n - 2) && digit(n - 1))
        }
}

/// `DateTime.make(text)` for a string: an instant, reading a string without a zone as UTC.
pub fn make(text: &str) -> Option<i64> {
    if has_zone(text) { parse_date(text) } else { parse_date(&format!("{text}Z")) }
}

/// `new Date(ms).toISOString()`.
#[allow(dead_code)] // Used by tests and the statistics.
pub fn iso(ms: i64) -> String {
    let (year, month, day) = civil_from_days(ms.div_euclid(DAY_MS));
    let rest = ms.rem_euclid(DAY_MS);
    let (h, m, s, f) = (rest / 3_600_000, rest / 60_000 % 60, rest / 1000 % 60, rest % 1000);
    let year = if (0..=9999).contains(&year) { format!("{year:04}") } else if year < 0 { format!("-{:06}", -year) } else { format!("+{year:06}") };
    format!("{year}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}.{f:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_parse_like_javascriptcore() {
        assert_eq!(parse_date("2031-03-04T05:06:07Z"), Some(1930367167000));
        assert_eq!(parse_date("2031-03-04T09:00+0100"), Some(1930377600000));
        assert_eq!(parse_date("2031-03-04T09:00+01:00"), Some(1930377600000));
        assert_eq!(parse_date("2031-02-30T00:00Z"), Some(1930176000000));
        assert_eq!(parse_date("2031-03-04T24:00Z"), Some(1930435200000));
        assert_eq!(parse_date("2031Z"), Some(1924992000000));
        assert_eq!(parse_date("+002031-03-04T09:00Z"), Some(1930381200000));
        assert_eq!(parse_date("2031-03-04T09:00:00.123456Z"), Some(1930381200123));
        for bad in ["next tuesdayZ", "2031-03-04T9:00Z", "2031-3-4T09:00Z", "2031-03-04T09Z", "2031-03-04T09:00:00+01"] {
            assert_eq!(parse_date(bad), None, "{bad}");
        }
        assert_eq!(make("2031-03-04T09:00"), Some(1930381200000));
        assert_eq!(iso(1930367167000), "2031-03-04T05:06:07.000Z");
    }

    #[test]
    fn zones_read_tzdata() {
        let Some(new_york) = Zone::named("america/new_york") else { return };
        assert_eq!(new_york.id, "America/New_York");
        // 2031-03-04T09:00 in New York is 14:00 UTC (EST); 2031-07-04T09:00 is 13:00 UTC (EDT).
        assert_eq!(new_york.from_wall(make("2031-03-04T09:00").unwrap()), make("2031-03-04T14:00Z").unwrap());
        assert_eq!(new_york.from_wall(make("2031-07-04T09:00").unwrap()), make("2031-07-04T13:00Z").unwrap());
        // A time in the spring gap moves forward; a repeated one is its first occurrence.
        assert_eq!(new_york.from_wall(make("2031-03-09T02:30").unwrap()), make("2031-03-09T07:30Z").unwrap());
        assert_eq!(new_york.from_wall(make("2031-11-02T01:30").unwrap()), make("2031-11-02T05:30Z").unwrap());
        assert!(Zone::named("Mars/Olympus").is_none());
        assert_eq!(Zone::named("+01:00").unwrap().offset_ms(0), 3_600_000);
        let posix = Posix::parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(posix.offset(make("2040-07-01T00:00Z").unwrap() / 1000), -4 * 3600);
        assert_eq!(posix.offset(make("2040-01-01T00:00Z").unwrap() / 1000), -5 * 3600);
    }
}
