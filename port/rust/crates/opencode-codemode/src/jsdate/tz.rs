//! The local time zone, as JavaScriptCore sees it through ICU.
//!
//! Which zone: ICU's `uprv_tzname` logic. `TZ` is used when it looks like an Olson ID (a leading `:` and a `posix/`
//! or `right/` prefix are dropped); otherwise, or when `TZ` is unset, the target of the `/etc/localtime` link names
//! the zone. An ID ICU does not know makes JavaScriptCore fall back to UTC. ICU matches IDs without regard to case
//! and knows legacy names (`EST5EDT`, `US/Eastern`, `CET`) that a system zoneinfo may lack, so the ID is resolved
//! through the table of ICU's zones (`zone_names.rs`, generated from bun), which also holds the display names.
//!
//! The rules: the system's TZif file for the zone's canonical ID (ICU bundles its own copy of the same tz database).
//! Instants after the file's last transition follow its POSIX TZ footer. A local time maps to UTC the way JSC asks
//! ICU to (`ucal_getTimeZoneOffsetFromLocal` with `UCAL_TZ_LOCAL_FORMER` for both skipped and repeated times): with
//! the offset in effect before the transition.
//!
//! ICU's daylight flag (which picks the display name) comes from the tz "rearguard" format, where daylight saving
//! is never negative. Where the TZif data uses negative DST (Europe/Dublin, Africa/Casablanca, ...), the flags are
//! flipped back: a negative-DST period is standard time, and a standard period between two of them is daylight.

use std::sync::OnceLock;

use super::civil::{civil_from_days, days_from_civil};
use super::zone_names;

const ZONEINFO_DIRS: [&str; 3] = ["/usr/share/zoneinfo", "/usr/lib/zoneinfo", "/usr/share/lib/zoneinfo"];

#[derive(Clone, Copy, Debug)]
struct LocalType {
    /// Seconds east of UTC.
    offset: i32,
    dst: bool,
}

#[derive(Clone, Copy, Debug)]
enum RuleDay {
    /// `Jn`: day 1..=365, February 29 never counted.
    Julian1(i64),
    /// `n`: day 0..=365, February 29 counted in leap years.
    Julian0(i64),
    /// `Mm.w.d`: day `d` (0 = Sunday) of week `w` (5 = last) of month `m`.
    MonthWeekDay(u32, u32, u32),
}

#[derive(Clone, Copy, Debug)]
struct PosixDst {
    offset: i32,
    start: RuleDay,
    /// Seconds after local midnight, in standard time.
    start_time: i64,
    end: RuleDay,
    /// Seconds after local midnight, in daylight time.
    end_time: i64,
}

/// A POSIX TZ string (a TZif footer).
#[derive(Clone, Copy, Debug)]
struct Posix {
    std: i32,
    dst: Option<PosixDst>,
}

pub(super) struct Zone {
    /// UTC seconds of each transition, ascending.
    times: Vec<i64>,
    /// The local time type that starts at each transition.
    kinds: Vec<usize>,
    types: Vec<LocalType>,
    /// ICU's daylight flag for the period starting at each transition.
    icu_dst: Vec<bool>,
    footer: Option<Posix>,
    standard_name: String,
    daylight_name: String,
}

/// The zone JSC would use in this process, read once.
pub(super) fn zone() -> &'static Zone {
    static ZONE: OnceLock<Zone> = OnceLock::new();
    ZONE.get_or_init(load)
}

impl Zone {
    fn utc() -> Zone {
        Zone {
            times: Vec::new(),
            kinds: Vec::new(),
            types: vec![LocalType { offset: 0, dst: false }],
            icu_dst: Vec::new(),
            footer: None,
            standard_name: "Coordinated Universal Time".to_string(),
            daylight_name: "Coordinated Universal Time".to_string(),
        }
    }

    pub(super) fn name(&self, dst: bool) -> &str {
        if dst { &self.daylight_name } else { &self.standard_name }
    }

    fn initial(&self) -> LocalType {
        self.types[0]
    }

    /// The offset (milliseconds east of UTC) and ICU's daylight flag at the UTC time `t` (finite milliseconds).
    pub(super) fn offset_at_utc(&self, t: f64) -> (f64, bool) {
        let secs = (t / 1000.0).floor();
        let (offset, dst) = self.at_utc_secs(clamp_secs(secs));
        (offset as f64 * 1000.0, dst)
    }

    /// The offset (milliseconds east of UTC) to subtract from the local time `t` (finite milliseconds) to get UTC.
    ///
    /// Outside the time value range (±8.64e15 ms) JSC gets the offset of the zone's final rule, as if the current
    /// rules had always applied (it shows in the far past, where the real offset is local mean time).
    pub(super) fn offset_for_local(&self, t: f64) -> f64 {
        let secs = clamp_secs((t / 1000.0).floor());
        let offset = if t.abs() > super::MAX_TIME { self.final_local_secs(secs) } else { self.local_secs(secs) };
        offset as f64 * 1000.0
    }

    fn final_local_secs(&self, local: i64) -> i32 {
        match &self.footer {
            Some(footer) => footer.local_secs(local, i64::MIN).unwrap_or(footer.std),
            None => self.types[self.kinds.last().copied().unwrap_or(0)].offset,
        }
    }

    fn at_utc_secs(&self, secs: i64) -> (i32, bool) {
        let n = self.times.len();
        if n == 0 {
            return match &self.footer {
                Some(footer) => footer.at_utc(secs),
                None => (self.initial().offset, self.initial().dst),
            };
        }
        if secs < self.times[0] {
            return (self.initial().offset, self.initial().dst);
        }
        if let Some(footer) = &self.footer
            && secs >= self.times[n - 1]
        {
            return footer.at_utc(secs);
        }
        let index = self.times.partition_point(|&time| time <= secs) - 1;
        (self.types[self.kinds[index]].offset, self.icu_dst[index])
    }

    fn local_secs(&self, local: i64) -> i32 {
        let n = self.times.len();
        let last = if n == 0 { i64::MIN } else { self.times[n - 1] };
        // Transitions from the footer rule come after the explicit ones; ICU scans from the latest transition back
        // and takes the first whose local threshold (its time plus the larger of the two offsets) has passed.
        if let Some(footer) = &self.footer
            && let Some(offset) = footer.local_secs(local, last)
        {
            return offset;
        }
        if n == 0 {
            return self.initial().offset;
        }
        let start = self.times.partition_point(|&time| time <= local.saturating_add(2 * 86_400));
        for index in (0..start).rev() {
            let before = if index == 0 { self.initial().offset } else { self.types[self.kinds[index - 1]].offset };
            let after = self.types[self.kinds[index]].offset;
            if local >= self.times[index] + before.max(after) as i64 {
                return after;
            }
        }
        self.initial().offset
    }
}

/// Keeps second counts far outside any date's range from overflowing the rule arithmetic.
fn clamp_secs(secs: f64) -> i64 {
    secs.clamp(-1e15, 1e15) as i64
}

impl Posix {
    /// The offset for the local time `local` from the rule's transitions after `after` (UTC seconds), if one of
    /// them has passed: ICU scans transitions from the latest back and takes the first whose local threshold (its
    /// time plus the larger of the offsets on either side) is reached.
    fn local_secs(&self, local: i64, after: i64) -> Option<i32> {
        let dst = self.dst.as_ref()?;
        let year = civil_from_days((local + self.std as i64).div_euclid(86_400)).0;
        for &(time, to_dst) in self.transitions(dst, year).iter().rev() {
            if time <= after {
                break;
            }
            let (before, next) = if to_dst { (self.std, dst.offset) } else { (dst.offset, self.std) };
            if local >= time + before.max(next) as i64 {
                return Some(next);
            }
        }
        None
    }

    fn at_utc(&self, secs: i64) -> (i32, bool) {
        let Some(dst) = &self.dst else {
            return (self.std, false);
        };
        let year = civil_from_days((secs + self.std as i64).div_euclid(86_400)).0;
        let transitions = self.transitions(dst, year);
        let mut state = !transitions[0].1;
        for &(time, to_dst) in &transitions {
            if time <= secs {
                state = to_dst;
            }
        }
        let offset = if state { dst.offset } else { self.std };
        // Negative DST (daylight offset below standard) is standard time to ICU, and the rest daylight.
        let icu_dst = if dst.offset < self.std { !state } else { state };
        (offset, icu_dst)
    }

    /// The rule's transitions in the years around `year`, ascending: (UTC seconds, whether daylight time starts).
    fn transitions(&self, dst: &PosixDst, year: i64) -> Vec<(i64, bool)> {
        let mut out = Vec::with_capacity(6);
        for y in year - 1..=year + 1 {
            out.push((rule_day(dst.start, y) * 86_400 + dst.start_time - self.std as i64, true));
            out.push((rule_day(dst.end, y) * 86_400 + dst.end_time - dst.offset as i64, false));
        }
        out.sort();
        out
    }
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn month_length(year: i64, month: u32) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// Days since the epoch of the rule's local date in `year`.
fn rule_day(rule: RuleDay, year: i64) -> i64 {
    let jan1 = days_from_civil(year, 1, 1);
    match rule {
        RuleDay::Julian1(n) => jan1 + n - 1 + if is_leap(year) && n >= 60 { 1 } else { 0 },
        RuleDay::Julian0(n) => jan1 + n,
        RuleDay::MonthWeekDay(month, week, weekday) => {
            let first = days_from_civil(year, month, 1);
            let first_weekday = (first + 4).rem_euclid(7);
            let mut day = first + (weekday as i64 - first_weekday).rem_euclid(7) + 7 * (week as i64 - 1);
            let end = first + month_length(year, month);
            while day >= end {
                day -= 7;
            }
            day
        }
    }
}

// --- POSIX TZ strings ---

struct Cursor<'a> {
    s: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.at).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn number(&mut self) -> Option<i64> {
        let start = self.at;
        let mut n: i64 = 0;
        while let Some(c @ b'0'..=b'9') = self.peek() {
            n = n.checked_mul(10)?.checked_add((c - b'0') as i64)?;
            self.at += 1;
        }
        (self.at > start).then_some(n)
    }
    fn name(&mut self) -> Option<()> {
        if self.eat(b'<') {
            while self.peek()? != b'>' {
                self.at += 1;
            }
            self.at += 1;
            return Some(());
        }
        let start = self.at;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphabetic()) {
            self.at += 1;
        }
        (self.at - start >= 3).then_some(())
    }
    /// `[+-]hh[:mm[:ss]]` as signed seconds.
    fn hms(&mut self) -> Option<i64> {
        let sign = if self.eat(b'-') {
            -1
        } else {
            self.eat(b'+');
            1
        };
        let mut secs = self.number()? * 3600;
        if self.eat(b':') {
            secs += self.number()? * 60;
            if self.eat(b':') {
                secs += self.number()?;
            }
        }
        Some(sign * secs)
    }
    fn rule(&mut self) -> Option<(RuleDay, i64)> {
        let day = if self.eat(b'J') {
            RuleDay::Julian1(self.number()?)
        } else if self.eat(b'M') {
            let month = self.number()?;
            if !self.eat(b'.') {
                return None;
            }
            let week = self.number()?;
            if !self.eat(b'.') {
                return None;
            }
            let weekday = self.number()?;
            if !(1..=12).contains(&month) || !(1..=5).contains(&week) || weekday > 6 {
                return None;
            }
            RuleDay::MonthWeekDay(month as u32, week as u32, weekday as u32)
        } else {
            RuleDay::Julian0(self.number()?)
        };
        let time = if self.eat(b'/') { self.hms()? } else { 7200 };
        Some((day, time))
    }
}

fn parse_posix(s: &[u8]) -> Option<Posix> {
    let mut c = Cursor { s, at: 0 };
    c.name()?;
    let std = -(c.hms()? as i32);
    if c.peek().is_none() {
        return Some(Posix { std, dst: None });
    }
    c.name()?;
    let offset = match c.peek() {
        Some(b',') | None => std + 3600,
        _ => -(c.hms()? as i32),
    };
    let ((start, start_time), (end, end_time)) = if c.eat(b',') {
        let start = c.rule()?;
        if !c.eat(b',') {
            return None;
        }
        (start, c.rule()?)
    } else {
        // POSIX leaves the default rules to the implementation; the US rules are customary.
        ((RuleDay::MonthWeekDay(3, 2, 0), 7200), (RuleDay::MonthWeekDay(11, 1, 0), 7200))
    };
    if c.peek().is_some() {
        return None;
    }
    Some(Posix { std, dst: Some(PosixDst { offset, start, start_time, end, end_time }) })
}

// --- TZif files ---

fn be32(b: &[u8], at: usize) -> Option<i64> {
    Some(i32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?) as i64)
}

fn be64(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

struct Tzif {
    times: Vec<i64>,
    kinds: Vec<usize>,
    types: Vec<LocalType>,
    footer: Option<Posix>,
}

fn parse_tzif(b: &[u8]) -> Option<Tzif> {
    if b.get(0..4)? != b"TZif" {
        return None;
    }
    let version = *b.get(4)?;
    let counts = |at: usize| -> Option<[usize; 6]> {
        let mut c = [0usize; 6];
        for (i, slot) in c.iter_mut().enumerate() {
            *slot = usize::try_from(be32(b, at + 20 + 4 * i)?).ok()?;
        }
        Some(c)
    };
    let [isut, isstd, leap, timecnt, typecnt, charcnt] = counts(0)?;
    let v1_len = timecnt * 5 + typecnt * 6 + charcnt + leap * 8 + isstd + isut;
    let (base, wide, [_, _, _, timecnt, typecnt, charcnt]) = if version >= b'2' {
        let header = 44 + v1_len;
        let c = counts(header)?;
        (header + 44, true, c)
    } else {
        (44, false, [isut, isstd, leap, timecnt, typecnt, charcnt])
    };
    let width = if wide { 8 } else { 4 };
    let mut times = Vec::with_capacity(timecnt);
    for i in 0..timecnt {
        times.push(if wide { be64(b, base + i * 8)? } else { be32(b, base + i * 4)? });
    }
    let kinds_at = base + timecnt * width;
    let mut kinds = Vec::with_capacity(timecnt);
    for i in 0..timecnt {
        let kind = *b.get(kinds_at + i)? as usize;
        if kind >= typecnt {
            return None;
        }
        kinds.push(kind);
    }
    let types_at = kinds_at + timecnt;
    let mut types = Vec::with_capacity(typecnt);
    for i in 0..typecnt {
        let at = types_at + i * 6;
        types.push(LocalType { offset: be32(b, at)? as i32, dst: *b.get(at + 4)? != 0 });
    }
    if types.is_empty() {
        return None;
    }
    let mut footer = None;
    if wide {
        let (leapcnt, isstdcnt, isutcnt) = {
            let c = counts(base - 44)?;
            (c[2], c[1], c[0])
        };
        let end = types_at + typecnt * 6 + charcnt + leapcnt * 12 + isstdcnt + isutcnt;
        if let Some(rest) = b.get(end..)
            && rest.first() == Some(&b'\n')
            && let Some(close) = rest[1..].iter().position(|&c| c == b'\n')
        {
            footer = parse_posix(&rest[1..1 + close]);
        }
    }
    Some(Tzif { times, kinds, types, footer })
}

fn icu_dst_flags(tzif: &Tzif) -> Vec<bool> {
    let n = tzif.times.len();
    let ty = |i: usize| tzif.types[tzif.kinds[i]];
    let negative = |i: usize| {
        ty(i).dst
            && ((i > 0 && !ty(i - 1).dst && ty(i - 1).offset > ty(i).offset)
                || (i + 1 < n && !ty(i + 1).dst && ty(i + 1).offset > ty(i).offset))
    };
    let footer_negative = matches!(&tzif.footer, Some(Posix { std, dst: Some(d) }) if d.offset < *std);
    (0..n)
        .map(|i| {
            if ty(i).dst {
                !negative(i)
            } else {
                let left = i > 0 && negative(i - 1);
                let right = if i + 1 < n { negative(i + 1) } else { footer_negative };
                left && right
            }
        })
        .collect()
}

// --- Which zone ---

/// ICU's `isValidOlsonID`: rejects POSIX-style values such as `CST6CDT5,J129,J131/19:30`.
fn is_valid_olson_id(id: &[u8]) -> bool {
    let mut at = 0;
    while at < id.len() && !id[at].is_ascii_digit() && id[at] != b',' {
        at += 1;
    }
    let max = at + 2;
    while at < id.len() && id[at].is_ascii_digit() && at < max {
        at += 1;
    }
    at == id.len() || matches!(id, b"PST8PDT" | b"MST7MDT" | b"CST6CDT" | b"EST5EDT")
}

fn skip_prefix(id: &str) -> &str {
    id.strip_prefix("posix/").or_else(|| id.strip_prefix("right/")).unwrap_or(id)
}

/// The zone ID ICU would pick for this process (before checking that ICU knows it).
fn default_zone_id() -> Option<String> {
    if let Some(tz) = std::env::var_os("TZ") {
        let tz = tz.to_string_lossy().into_owned();
        if is_valid_olson_id(tz.as_bytes()) {
            let tz = tz.strip_prefix(':').unwrap_or(&tz);
            return Some(skip_prefix(tz).to_string());
        }
    }
    let localtime = "/etc/localtime";
    if let Ok(target) = std::fs::canonicalize(localtime) {
        let target = target.to_string_lossy().into_owned();
        if target != localtime {
            if let Some(at) = target.find("/zoneinfo/") {
                let id = skip_prefix(&target[at + "/zoneinfo/".len()..]);
                if is_valid_olson_id(id.as_bytes()) {
                    return Some(id.to_string());
                }
            }
            return None;
        }
    }
    // Not a link: Debian's /etc/timezone, else the zoneinfo file with the same contents.
    if let Ok(text) = std::fs::read_to_string("/etc/timezone") {
        let id = text.trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    let contents = std::fs::read(localtime).ok()?;
    zone_names::ZONES.iter().find_map(|entry| {
        let file = read_zoneinfo(entry.id)?;
        (file == contents).then(|| entry.id.to_string())
    })
}

fn read_zoneinfo(id: &str) -> Option<Vec<u8>> {
    if id.is_empty() || id.starts_with('/') || id.split('/').any(|part| part == "..") {
        return None;
    }
    ZONEINFO_DIRS.iter().find_map(|dir| std::fs::read(format!("{dir}/{id}")).ok())
}

fn load() -> Zone {
    let Some(id) = default_zone_id() else {
        return Zone::utc();
    };
    let Some(entry) = zone_names::lookup(&id) else {
        return Zone::utc();
    };
    let tzif = [entry.canonical, entry.id].iter().find_map(|id| parse_tzif(&read_zoneinfo(id)?));
    let Some(tzif) = tzif else {
        return Zone::utc();
    };
    let icu_dst = icu_dst_flags(&tzif);
    let standard_name = entry.standard.to_string();
    let daylight_name = entry.daylight.to_string();
    Zone { times: tzif.times, kinds: tzif.kinds, types: tzif.types, icu_dst, footer: tzif.footer, standard_name, daylight_name }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn olson_ids() {
        assert!(is_valid_olson_id(b"America/New_York"));
        assert!(is_valid_olson_id(b"UTC+3"));
        assert!(is_valid_olson_id(b"Etc/GMT-14"));
        assert!(is_valid_olson_id(b"EST5EDT"));
        assert!(!is_valid_olson_id(b"<+0530>-5:30"));
        assert!(!is_valid_olson_id(b"CST6CDT5,J129,J131/19:30"));
    }

    #[test]
    fn posix_rules() {
        let p = parse_posix(b"EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(p.std, -18000);
        assert_eq!(p.dst.unwrap().offset, -14400);
        // 2021-03-14 07:00 UTC: DST starts in New York.
        assert_eq!(p.at_utc(1615705200 - 1).0, -18000);
        assert_eq!(p.at_utc(1615705200).0, -14400);
        let dublin = parse_posix(b"IST-1GMT0,M10.5.0,M3.5.0/1").unwrap();
        // Summer: offset +1, daylight to ICU; winter: GMT, standard.
        assert_eq!(dublin.at_utc(1625097600), (3600, true));
        assert_eq!(dublin.at_utc(1609459200), (0, false));
        let lord_howe = parse_posix(b"<+1030>-10:30<+11>-11,M10.1.0,M4.1.0").unwrap();
        assert_eq!(lord_howe.at_utc(1609459200).0, 39600);
    }
}
