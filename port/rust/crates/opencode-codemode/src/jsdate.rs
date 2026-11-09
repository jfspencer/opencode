//! TEMPORARY STUB (replaced by the jsdate work): JS Date algorithms, UTC only.

const MS_PER_DAY: f64 = 86_400_000.0;

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

pub fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > 8.64e15 {
        return f64::NAN;
    }
    t.trunc() + 0.0
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn make_day(year: f64, month: f64, date: f64) -> f64 {
    if !year.is_finite() || !month.is_finite() || !date.is_finite() {
        return f64::NAN;
    }
    let ym = year.trunc() + (month.trunc() / 12.0).floor();
    let mn = month.trunc().rem_euclid(12.0);
    let days = days_from_civil(ym as i64, mn as i64 + 1, 1) as f64;
    days + date.trunc() - 1.0
}

fn make_time(h: f64, m: f64, s: f64, ms: f64) -> f64 {
    if !h.is_finite() || !m.is_finite() || !s.is_finite() || !ms.is_finite() {
        return f64::NAN;
    }
    h.trunc() * 3_600_000.0 + m.trunc() * 60_000.0 + s.trunc() * 1000.0 + ms.trunc()
}

fn components(args: &[f64]) -> f64 {
    let year = args.first().copied().unwrap_or(f64::NAN);
    let year = if year.is_finite() && (0.0..=99.0).contains(&year.trunc()) { 1900.0 + year.trunc() } else { year };
    let get = |i: usize, d: f64| args.get(i).copied().unwrap_or(d);
    let day = make_day(year, get(1, 0.0), get(2, 1.0));
    time_clip(day * MS_PER_DAY + make_time(get(3, 0.0), get(4, 0.0), get(5, 0.0), get(6, 0.0)))
}

pub fn utc(args: &[f64]) -> f64 {
    components(args)
}

pub fn from_local_components(args: &[f64]) -> f64 {
    components(args)
}

#[derive(Clone, Copy)]
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

pub fn get(t: f64, getter: Getter) -> f64 {
    if t.is_nan() {
        return f64::NAN;
    }
    let day = (t / MS_PER_DAY).floor();
    let ms_in_day = t - day * MS_PER_DAY;
    let (y, m, d) = civil_from_days(day as i64);
    match getter {
        Getter::FullYear | Getter::UtcFullYear => y as f64,
        Getter::Month | Getter::UtcMonth => (m - 1) as f64,
        Getter::Date | Getter::UtcDate => d as f64,
        Getter::Day | Getter::UtcDay => ((day + 4.0).rem_euclid(7.0)),
        Getter::Hours | Getter::UtcHours => (ms_in_day / 3_600_000.0).floor(),
        Getter::Minutes | Getter::UtcMinutes => ((ms_in_day / 60_000.0).floor()).rem_euclid(60.0),
        Getter::Seconds | Getter::UtcSeconds => ((ms_in_day / 1000.0).floor()).rem_euclid(60.0),
        Getter::Milliseconds | Getter::UtcMilliseconds => ms_in_day.rem_euclid(1000.0),
        Getter::TimezoneOffset => 0.0,
    }
}

#[derive(Clone, Copy)]
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

pub fn set(t: f64, setter: Setter, args: &[f64]) -> f64 {
    let arg = |i: usize| args.get(i).copied();
    if let Setter::Time = setter {
        return time_clip(arg(0).unwrap_or(f64::NAN));
    }
    let base = if t.is_nan() { 0.0 } else { t };
    let (y, mo, d) = (get(base, Getter::FullYear), get(base, Getter::Month), get(base, Getter::Date));
    let (h, mi, s, ms) = (get(base, Getter::Hours), get(base, Getter::Minutes), get(base, Getter::Seconds), get(base, Getter::Milliseconds));
    let (y, mo, d, h, mi, s, ms) = match setter {
        Setter::Milliseconds | Setter::UtcMilliseconds => (y, mo, d, h, mi, s, arg(0).unwrap_or(f64::NAN)),
        Setter::Seconds | Setter::UtcSeconds => (y, mo, d, h, mi, arg(0).unwrap_or(f64::NAN), arg(1).unwrap_or(ms)),
        Setter::Minutes | Setter::UtcMinutes => (y, mo, d, h, arg(0).unwrap_or(f64::NAN), arg(1).unwrap_or(s), arg(2).unwrap_or(ms)),
        Setter::Hours | Setter::UtcHours => (y, mo, d, arg(0).unwrap_or(f64::NAN), arg(1).unwrap_or(mi), arg(2).unwrap_or(s), arg(3).unwrap_or(ms)),
        Setter::Date | Setter::UtcDate => (y, mo, arg(0).unwrap_or(f64::NAN), h, mi, s, ms),
        Setter::Month | Setter::UtcMonth => (y, arg(0).unwrap_or(f64::NAN), arg(1).unwrap_or(d), h, mi, s, ms),
        Setter::FullYear | Setter::UtcFullYear => (arg(0).unwrap_or(f64::NAN), arg(1).unwrap_or(mo), arg(2).unwrap_or(d), h, mi, s, ms),
        Setter::Time => unreachable!(),
    };
    time_clip(make_day(y, mo, d) * MS_PER_DAY + make_time(h, mi, s, ms))
}

pub fn to_iso_string(t: f64) -> Option<String> {
    if !t.is_finite() {
        return None;
    }
    let y = get(t, Getter::UtcFullYear) as i64;
    let year = if (0..=9999).contains(&y) { format!("{y:04}") } else if y < 0 { format!("-{:06}", -y) } else { format!("+{y:06}") };
    Some(format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        get(t, Getter::UtcMonth) as i64 + 1,
        get(t, Getter::UtcDate) as i64,
        get(t, Getter::UtcHours) as i64,
        get(t, Getter::UtcMinutes) as i64,
        get(t, Getter::UtcSeconds) as i64,
        get(t, Getter::UtcMilliseconds) as i64
    ))
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

pub fn to_utc_string(t: f64) -> String {
    if !t.is_finite() {
        return "Invalid Date".into();
    }
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        DAYS[get(t, Getter::UtcDay) as usize],
        get(t, Getter::UtcDate) as i64,
        MONTHS[get(t, Getter::UtcMonth) as usize],
        get(t, Getter::UtcFullYear) as i64,
        get(t, Getter::UtcHours) as i64,
        get(t, Getter::UtcMinutes) as i64,
        get(t, Getter::UtcSeconds) as i64
    )
}

pub fn to_date_string(t: f64) -> String {
    if !t.is_finite() {
        return "Invalid Date".into();
    }
    format!(
        "{} {} {:02} {:04}",
        DAYS[get(t, Getter::Day) as usize],
        MONTHS[get(t, Getter::Month) as usize],
        get(t, Getter::Date) as i64,
        get(t, Getter::FullYear) as i64
    )
}

pub fn to_time_string(t: f64) -> String {
    if !t.is_finite() {
        return "Invalid Date".into();
    }
    format!(
        "{:02}:{:02}:{:02} GMT+0000 (Coordinated Universal Time)",
        get(t, Getter::Hours) as i64,
        get(t, Getter::Minutes) as i64,
        get(t, Getter::Seconds) as i64
    )
}

pub fn to_locale_string(t: f64) -> String {
    if !t.is_finite() {
        return "Invalid Date".into();
    }
    format!("{}, {}", to_locale_date_string(t), to_locale_time_string(t))
}

pub fn to_locale_date_string(t: f64) -> String {
    if !t.is_finite() {
        return "Invalid Date".into();
    }
    format!("{}/{}/{}", get(t, Getter::UtcMonth) as i64 + 1, get(t, Getter::UtcDate) as i64, get(t, Getter::UtcFullYear) as i64)
}

pub fn to_locale_time_string(t: f64) -> String {
    if !t.is_finite() {
        return "Invalid Date".into();
    }
    let h = get(t, Getter::UtcHours) as i64;
    let (hour, suffix) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{hour}:{:02}:{:02}\u{202f}{suffix}", get(t, Getter::UtcMinutes) as i64, get(t, Getter::UtcSeconds) as i64)
}

pub fn parse(s: &[u16]) -> f64 {
    let text = String::from_utf16_lossy(s);
    let text = text.trim();
    let digits = |s: &str| s.parse::<f64>().ok();
    let date_part = text.split(['T', ' ']).next().unwrap_or("");
    let pieces: Vec<&str> = date_part.split('-').collect();
    if pieces.len() >= 1 && pieces.len() <= 3 && pieces[0].len() == 4 {
        let y = digits(pieces[0]);
        let m = pieces.get(1).and_then(|p| digits(p)).unwrap_or(1.0);
        let d = pieces.get(2).and_then(|p| digits(p)).unwrap_or(1.0);
        if let Some(y) = y {
            let mut time = 0.0;
            if let Some(rest) = text.get(date_part.len() + 1..) {
                let rest = rest.trim_end_matches('Z');
                let parts: Vec<&str> = rest.split(':').collect();
                let h = parts.first().and_then(|p| digits(p)).unwrap_or(0.0);
                let mi = parts.get(1).and_then(|p| digits(p)).unwrap_or(0.0);
                let sec = parts.get(2).and_then(|p| digits(p)).unwrap_or(0.0);
                time = h * 3_600_000.0 + mi * 60_000.0 + (sec * 1000.0).round();
            }
            return time_clip(make_day(y, m - 1.0, d) * MS_PER_DAY + time);
        }
    }
    f64::NAN
}
