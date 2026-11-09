//! The JavaScript Date algorithms the interpreter delegates to its host, matching JavaScriptCore as bun runs it.
//!
//! Time values are `f64` milliseconds since the epoch, NaN for an invalid date, exactly as in JavaScript. Every
//! function is pure apart from the local time zone, which is read once per process (see `tz.rs`), and `now`.
//!
//! Where JSC departs from the specification, this follows JSC (verified against bun by `harness/jsdate`):
//! - `Date.parse` is bun's port of V8's date parser (`parse.rs`).
//! - The constructor and `Date.UTC` give NaN when the year after month normalization does not fit an int32.
//! - The month and full-year setters give NaN for a year argument beyond ±275760 or a month argument beyond
//!   ±3309120 (275760 years of months).
//! - `getTimezoneOffset` and the `GMT+hhmm` of `toTimeString` truncate the offset to whole minutes (local
//!   mean time offsets have seconds).
//! - `toLocale*String` are ICU's en-US formats in UTC with the proleptic Gregorian calendar and era years
//!   (year 0 is "1", i.e. 1 BC).

mod parse;
mod tz;
mod zone_names;

use std::time::{SystemTime, UNIX_EPOCH};

const MS_PER_SECOND: f64 = 1000.0;
const MS_PER_MINUTE: f64 = 60_000.0;
const MS_PER_HOUR: f64 = 3_600_000.0;
const MS_PER_DAY: f64 = 86_400_000.0;
/// ±100,000,000 days around the epoch.
const MAX_TIME: f64 = 8.64e15;

/// `Date.now()`.
pub fn now() -> f64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_millis() as f64,
        Err(before) => -(before.duration().as_millis() as f64),
    }
}

/// TimeClip: what `new Date(number).getTime()` gives.
pub fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > MAX_TIME {
        return f64::NAN;
    }
    t.trunc() + 0.0
}

/// `Date.parse(string)`.
pub fn parse(s: &[u16]) -> f64 {
    parse::parse(s)
}

/// `Date.UTC(...args)` with the arguments already converted to numbers.
pub fn utc(args: &[f64]) -> f64 {
    time_clip(from_components(args))
}

/// `new Date(y, m, d?, h?, min?, s?, ms?).getTime()`.
pub fn from_local_components(args: &[f64]) -> f64 {
    time_clip(local_to_utc(from_components(args)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Getter {
    FullYear,
    Month,
    Date,
    Day,
    Hours,
    Minutes,
    Seconds,
    Milliseconds,
    UtcFullYear,
    UtcMonth,
    UtcDate,
    UtcDay,
    UtcHours,
    UtcMinutes,
    UtcSeconds,
    UtcMilliseconds,
    TimezoneOffset,
}

/// `new Date(t).getX()`.
pub fn get(t: f64, getter: Getter) -> f64 {
    if !t.is_finite() {
        return f64::NAN;
    }
    let local = || {
        let (offset, _) = tz::zone().offset_at_utc(t);
        t + offset
    };
    match getter {
        Getter::FullYear => Fields::of(local()).year as f64,
        Getter::Month => Fields::of(local()).month as f64,
        Getter::Date => Fields::of(local()).date as f64,
        Getter::Day => week_day(local()),
        Getter::Hours => hour_from_time(local()),
        Getter::Minutes => min_from_time(local()),
        Getter::Seconds => sec_from_time(local()),
        Getter::Milliseconds => ms_from_time(t),
        Getter::UtcFullYear => Fields::of(t).year as f64,
        Getter::UtcMonth => Fields::of(t).month as f64,
        Getter::UtcDate => Fields::of(t).date as f64,
        Getter::UtcDay => week_day(t),
        Getter::UtcHours => hour_from_time(t),
        Getter::UtcMinutes => min_from_time(t),
        Getter::UtcSeconds => sec_from_time(t),
        Getter::UtcMilliseconds => ms_from_time(t),
        Getter::TimezoneOffset => -(offset_minutes(t) as f64) + 0.0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setter {
    Time,
    Milliseconds,
    UtcMilliseconds,
    Seconds,
    UtcSeconds,
    Minutes,
    UtcMinutes,
    Hours,
    UtcHours,
    Date,
    UtcDate,
    Month,
    UtcMonth,
    FullYear,
    UtcFullYear,
}

/// The time value `new Date(t).setX(...args)` returns (and stores). `args.len()` is the number of arguments passed;
/// a missing first argument counts as `undefined` (NaN).
pub fn set(t: f64, setter: Setter, args: &[f64]) -> f64 {
    let arg = |i: usize| args.get(i).copied();
    let first = arg(0).unwrap_or(f64::NAN);
    if setter == Setter::Time {
        return time_clip(first);
    }
    let local = !matches!(
        setter,
        Setter::UtcMilliseconds
            | Setter::UtcSeconds
            | Setter::UtcMinutes
            | Setter::UtcHours
            | Setter::UtcDate
            | Setter::UtcMonth
            | Setter::UtcFullYear
    );
    let full_year = matches!(setter, Setter::FullYear | Setter::UtcFullYear);
    let base = if t.is_nan() {
        if !full_year {
            return f64::NAN;
        }
        0.0
    } else if local {
        t + tz::zone().offset_at_utc(t).0
    } else {
        t
    };
    let day = (base / MS_PER_DAY).floor();
    let within = time_within_day(base);
    let fields = Fields::of(base);
    let new = match setter {
        Setter::Time => unreachable!(),
        Setter::Milliseconds | Setter::UtcMilliseconds => set_time_fields(base, day, args, 3, 1),
        Setter::Seconds | Setter::UtcSeconds => set_time_fields(base, day, args, 2, 2),
        Setter::Minutes | Setter::UtcMinutes => set_time_fields(base, day, args, 1, 3),
        Setter::Hours | Setter::UtcHours => set_time_fields(base, day, args, 0, 4),
        Setter::Date | Setter::UtcDate => {
            make_date(make_day(fields.year as f64, fields.month as f64, first), within)
        }
        Setter::Month | Setter::UtcMonth => {
            let date = arg(1).unwrap_or(fields.date as f64);
            if !month_in_setter_range(first) {
                return f64::NAN;
            }
            make_date(make_day(fields.year as f64, first, date), within)
        }
        Setter::FullYear | Setter::UtcFullYear => {
            let month = arg(1).unwrap_or(fields.month as f64);
            let date = arg(2).unwrap_or(fields.date as f64);
            if !(first.is_finite() && first.trunc().abs() <= 275_760.0) || !month_in_setter_range(month) {
                return f64::NAN;
            }
            make_date(make_day(first, month, date), within)
        }
    };
    time_clip(if local { local_to_utc(new) } else { new })
}

/// The time setters as JSC computes them: the given components (from `first`: 0 hours, 1 minutes, 2 seconds,
/// 3 milliseconds) accumulate into a millisecond count first, which is then added to the date's remaining fields.
/// The order only shows when huge components cancel out.
fn set_time_fields(base: f64, day: f64, args: &[f64], first: usize, length: usize) -> f64 {
    const UNITS: [f64; 3] = [MS_PER_HOUR, MS_PER_MINUTE, MS_PER_SECOND];
    let mut fields = [hour_from_time(base), min_from_time(base), sec_from_time(base)];
    let mut ms = ms_from_time(base);
    let mut accumulated = 0.0;
    for (i, slot) in (first..4).take(args.len().clamp(1, length)).enumerate() {
        let value = args.get(i).copied().unwrap_or(f64::NAN);
        if !value.is_finite() {
            return f64::NAN;
        }
        if slot < 3 {
            fields[slot] = 0.0;
            accumulated += integer(value) * UNITS[slot];
        } else {
            ms = integer(value);
        }
    }
    accumulated += ms;
    make_date(day, ((fields[0] * 60.0 + fields[1]) * 60.0 + fields[2]) * MS_PER_SECOND + accumulated)
}

fn month_in_setter_range(month: f64) -> bool {
    month.is_finite() && month.trunc().abs() <= 3_309_120.0
}

/// `toISOString`, or `None` for an invalid date (which throws a RangeError).
pub fn to_iso_string(t: f64) -> Option<String> {
    if !t.is_finite() {
        return None;
    }
    let f = Fields::of(t);
    let year = if (0..=9999).contains(&f.year) {
        format!("{:04}", f.year)
    } else {
        format!("{}{:06}", if f.year < 0 { '-' } else { '+' }, f.year.unsigned_abs())
    };
    Some(format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        f.month + 1,
        f.date,
        hour_from_time(t),
        min_from_time(t),
        sec_from_time(t),
        ms_from_time(t)
    ))
}

const WEEK_DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const INVALID: &str = "Invalid Date";

/// JSC's year in date strings: at least four digits, with a minus sign for years before 1 BC.
fn year4(year: i64) -> String {
    if year < 0 { format!("-{:04}", year.unsigned_abs()) } else { format!("{year:04}") }
}

/// `toUTCString` (and `toGMTString`).
pub fn to_utc_string(t: f64) -> String {
    if !t.is_finite() {
        return INVALID.to_string();
    }
    let f = Fields::of(t);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        WEEK_DAYS[week_day(t) as usize],
        f.date,
        MONTHS[f.month as usize],
        year4(f.year),
        hour_from_time(t),
        min_from_time(t),
        sec_from_time(t)
    )
}

/// `toDateString`, in the local time zone.
pub fn to_date_string(t: f64) -> String {
    if !t.is_finite() {
        return INVALID.to_string();
    }
    let local = t + tz::zone().offset_at_utc(t).0;
    let f = Fields::of(local);
    format!("{} {} {:02} {}", WEEK_DAYS[week_day(local) as usize], MONTHS[f.month as usize], f.date, year4(f.year))
}

/// `toTimeString`, in the local time zone, e.g. "00:00:00 GMT+0000 (Coordinated Universal Time)".
pub fn to_time_string(t: f64) -> String {
    if !t.is_finite() {
        return INVALID.to_string();
    }
    let zone = tz::zone();
    let (offset, dst) = zone.offset_at_utc(t);
    let local = t + offset;
    let minutes = offset_minutes(t);
    let sign = if minutes < 0 { '-' } else { '+' };
    let abs = minutes.unsigned_abs();
    format!(
        "{:02}:{:02}:{:02} GMT{sign}{:02}{:02} ({})",
        hour_from_time(local),
        min_from_time(local),
        sec_from_time(local),
        abs / 60,
        abs % 60,
        zone.name(dst)
    )
}

/// The local offset at `t` in whole minutes, truncated toward zero as JSC's `utcOffsetInMinute`.
fn offset_minutes(t: f64) -> i64 {
    let (offset, _) = tz::zone().offset_at_utc(t);
    (offset / MS_PER_SECOND) as i64 / 60
}

fn locale_date(f: &Fields) -> String {
    let year = if f.year <= 0 { 1 - f.year } else { f.year };
    format!("{}/{}/{}", f.month + 1, f.date, year)
}

fn locale_time(t: f64) -> String {
    let hour = hour_from_time(t) as u32;
    let h12 = if hour % 12 == 0 { 12 } else { hour % 12 };
    let meridiem = if hour < 12 { "AM" } else { "PM" };
    format!("{h12}:{:02}:{:02} {meridiem}", min_from_time(t), sec_from_time(t))
}

/// `toLocaleString("en-US", { timeZone: "UTC" })`, e.g. "1/1/1970, 12:00:00 AM".
pub fn to_locale_string(t: f64) -> String {
    if !t.is_finite() {
        return INVALID.to_string();
    }
    format!("{}, {}", locale_date(&Fields::of(t)), locale_time(t))
}

/// `toLocaleDateString("en-US", { timeZone: "UTC" })`.
pub fn to_locale_date_string(t: f64) -> String {
    if !t.is_finite() {
        return INVALID.to_string();
    }
    locale_date(&Fields::of(t))
}

/// `toLocaleTimeString("en-US", { timeZone: "UTC" })`.
pub fn to_locale_time_string(t: f64) -> String {
    if !t.is_finite() {
        return INVALID.to_string();
    }
    locale_time(t)
}

// --- The specification's date arithmetic ---

/// Calendar fields of a finite time value (UTC or already shifted to local time).
struct Fields {
    year: i64,
    /// 0..=11.
    month: u32,
    /// 1..=31.
    date: u32,
}

impl Fields {
    fn of(t: f64) -> Fields {
        let day = (t / MS_PER_DAY).floor();
        let (year, month, date) = civil::civil_from_days(day as i64);
        Fields { year, month: month - 1, date }
    }
}

fn time_within_day(t: f64) -> f64 {
    t.rem_euclid(MS_PER_DAY) + 0.0
}

fn week_day(t: f64) -> f64 {
    ((t / MS_PER_DAY).floor() + 4.0).rem_euclid(7.0) + 0.0
}

fn hour_from_time(t: f64) -> f64 {
    (t / MS_PER_HOUR).floor().rem_euclid(24.0) + 0.0
}

fn min_from_time(t: f64) -> f64 {
    (t / MS_PER_MINUTE).floor().rem_euclid(60.0) + 0.0
}

fn sec_from_time(t: f64) -> f64 {
    (t / MS_PER_SECOND).floor().rem_euclid(60.0) + 0.0
}

fn ms_from_time(t: f64) -> f64 {
    t.rem_euclid(MS_PER_SECOND) + 0.0
}

/// ToIntegerOrInfinity of a finite number.
fn integer(x: f64) -> f64 {
    x.trunc() + 0.0
}

fn make_time(hour: f64, min: f64, sec: f64, ms: f64) -> f64 {
    if !(hour.is_finite() && min.is_finite() && sec.is_finite() && ms.is_finite()) {
        return f64::NAN;
    }
    integer(hour) * MS_PER_HOUR + integer(min) * MS_PER_MINUTE + integer(sec) * MS_PER_SECOND + integer(ms)
}

/// MakeDay, with JSC's limit: the normalized year must fit an int32.
fn make_day(year: f64, month: f64, date: f64) -> f64 {
    if !(year.is_finite() && month.is_finite() && date.is_finite()) {
        return f64::NAN;
    }
    let (y, m, dt) = (integer(year), integer(month), integer(date));
    let ym = y + (m / 12.0).floor();
    if !(ym >= i32::MIN as f64 && ym <= i32::MAX as f64) {
        return f64::NAN;
    }
    let mn = m.rem_euclid(12.0) as u32;
    let days = civil::days_from_civil(ym as i64, mn + 1, 1) as f64;
    days + dt - 1.0
}

fn make_date(day: f64, time: f64) -> f64 {
    if !(day.is_finite() && time.is_finite()) {
        return f64::NAN;
    }
    let tv = day * MS_PER_DAY + time;
    if tv.is_finite() { tv } else { f64::NAN }
}

/// The unclipped time value of the components of `Date.UTC` and the multi-argument constructor.
fn from_components(args: &[f64]) -> f64 {
    let arg = |i: usize, default: f64| args.get(i).copied().unwrap_or(default);
    let year = arg(0, f64::NAN);
    if !year.is_finite() {
        return f64::NAN;
    }
    let mut y = integer(year);
    if (0.0..=99.0).contains(&y) {
        y += 1900.0;
    }
    let day = make_day(y, arg(1, 0.0), arg(2, 1.0));
    make_date(day, make_time(arg(3, 0.0), arg(4, 0.0), arg(5, 0.0), arg(6, 0.0)))
}

/// UTC(t): a local time value to UTC, with the offset before a transition for skipped and repeated times.
fn local_to_utc(t: f64) -> f64 {
    if !t.is_finite() {
        return f64::NAN;
    }
    t - tz::zone().offset_for_local(t)
}

mod civil {
    //! Proleptic Gregorian calendar conversions on day numbers (days since 1970-01-01).

    /// Days since the epoch of `year`-`month`-`day` (month 1..=12).
    pub(super) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let m = month as i64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// (year, month 1..=12, day 1..=31) of a day number.
    pub(super) fn civil_from_days(days: i64) -> (i64, u32, u32) {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
        (year, month, day)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for days in [-100_000_000i64, -719_468, -1, 0, 1, 59, 365, 11_016, 100_000_000] {
            let (y, m, d) = civil::civil_from_days(days);
            assert_eq!(civil::days_from_civil(y, m, d), days);
        }
        assert_eq!(civil::civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil::civil_from_days(-100_000_000), (-271_821, 4, 20));
    }

    #[test]
    fn utc_strings() {
        assert_eq!(to_iso_string(0.0).as_deref(), Some("1970-01-01T00:00:00.000Z"));
        assert_eq!(to_iso_string(8.64e15).as_deref(), Some("+275760-09-13T00:00:00.000Z"));
        assert_eq!(to_iso_string(-62198755200000.0).as_deref(), Some("-000001-01-01T00:00:00.000Z"));
        assert_eq!(to_utc_string(-8.64e15), "Tue, 20 Apr -271821 00:00:00 GMT");
        assert_eq!(to_locale_string(-8.64e15), "4/20/271822, 12:00:00 AM");
        assert_eq!(utc(&[2020.0, 0.0, 1.0, 0.0, 0.0, 0.0, 3e9]), 1580836800000.0);
    }
}
