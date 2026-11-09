//! `Date.parse`: bun's JavaScriptCore parses date strings with a port of V8's date parser (src/date/dateparser*),
//! so this follows V8's algorithm: an ES5 ISO 8601 reader first, and where it stops on something it does not
//! handle, the legacy token-by-token reader takes over with what has been read so far.
//!
//! Quirks kept on purpose (they are what bun does): a NUL character ends the string; numbers keep only their first
//! nine significant digits; fraction digits beyond the first three are dropped (counting leading zeros against the
//! nine-digit cap); words are matched on their first three letters; a UTC offset's seconds wrap modulo 2^32.

use super::{MAX_TIME, MS_PER_DAY, civil, local_to_utc, time_clip};

const NONE: i32 = i32::MAX;
const MAX_SIGNIFICANT_DIGITS: usize = 9;

/// The whitespace of bun's port: ASCII whitespace without vertical tab, the Unicode space separators, and BOM.
fn is_white_space(c: u32) -> bool {
    matches!(c, 0x09 | 0x0A | 0x0C | 0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000 | 0xFEFF)
}

struct Reader<'a> {
    s: &'a [u16],
    index: usize,
    ch: u32,
}

impl<'a> Reader<'a> {
    fn new(s: &'a [u16]) -> Self {
        let mut reader = Reader { s, index: 0, ch: 0 };
        reader.next();
        reader
    }
    fn next(&mut self) {
        self.ch = self.s.get(self.index).map_or(0, |&c| c as u32);
        self.index += 1;
    }
    fn position(&self) -> usize {
        self.index
    }
    fn is_end(&self) -> bool {
        self.ch == 0
    }
    fn is_digit(&self) -> bool {
        (b'0' as u32..=b'9' as u32).contains(&self.ch)
    }
    fn is_alpha_or_above(&self) -> bool {
        self.ch >= b'A' as u32
    }
    fn skip(&mut self, c: u8) -> bool {
        if self.ch == c as u32 {
            self.next();
            true
        } else {
            false
        }
    }
    fn read_unsigned_numeral(&mut self) -> i32 {
        let mut n = 0i32;
        let mut digits = 0;
        while self.ch == b'0' as u32 {
            self.next();
        }
        while self.is_digit() {
            if digits < MAX_SIGNIFICANT_DIGITS {
                n = n * 10 + (self.ch - b'0' as u32) as i32;
            }
            digits += 1;
            self.next();
        }
        n
    }
    /// Reads a word, returning its length and its first three characters lowercased (zero padded).
    fn read_word(&mut self) -> (usize, [u32; 3]) {
        let mut prefix = [0u32; 3];
        let mut len = 0;
        while self.is_alpha_or_above() && !is_white_space(self.ch) {
            if len < 3 {
                prefix[len] = self.ch | 0x20;
            }
            self.next();
            len += 1;
        }
        (len, prefix)
    }
    fn skip_parentheses(&mut self) -> bool {
        if self.ch != b'(' as u32 {
            return false;
        }
        let mut balance = 0;
        loop {
            if self.ch == b')' as u32 {
                balance -= 1;
            } else if self.ch == b'(' as u32 {
                balance += 1;
            }
            self.next();
            if !(balance > 0 && self.ch != 0) {
                break;
            }
        }
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kw {
    Invalid,
    MonthName,
    TimeZoneName,
    TimeSeparator,
    AmPm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    Invalid,
    Unknown,
    WhiteSpace,
    Number { value: i32, len: usize },
    Symbol(u8),
    Keyword { kind: Kw, value: i32, len: usize },
    End,
}

impl Token {
    fn is_number(self) -> bool {
        matches!(self, Token::Number { .. })
    }
    fn is_fixed_length_number(self, length: usize) -> bool {
        matches!(self, Token::Number { len, .. } if len == length)
    }
    fn number(self) -> i32 {
        match self {
            Token::Number { value, .. } => value,
            _ => 0,
        }
    }
    fn is_symbol(self, c: u8) -> bool {
        self == Token::Symbol(c)
    }
    fn is_sign(self) -> bool {
        matches!(self, Token::Symbol(b'+' | b'-'))
    }
    fn sign(self) -> i32 {
        if self == Token::Symbol(b'-') { -1 } else { 1 }
    }
    fn is_keyword_z(self) -> bool {
        matches!(self, Token::Keyword { kind: Kw::TimeZoneName, value: 0, len: 1 })
    }
    fn is_time_separator(self) -> bool {
        matches!(self, Token::Keyword { kind: Kw::TimeSeparator, .. })
    }
}

const KEYWORDS: [([u8; 3], Kw, i32); 27] = [
    (*b"jan", Kw::MonthName, 1),
    (*b"feb", Kw::MonthName, 2),
    (*b"mar", Kw::MonthName, 3),
    (*b"apr", Kw::MonthName, 4),
    (*b"may", Kw::MonthName, 5),
    (*b"jun", Kw::MonthName, 6),
    (*b"jul", Kw::MonthName, 7),
    (*b"aug", Kw::MonthName, 8),
    (*b"sep", Kw::MonthName, 9),
    (*b"oct", Kw::MonthName, 10),
    (*b"nov", Kw::MonthName, 11),
    (*b"dec", Kw::MonthName, 12),
    (*b"am\0", Kw::AmPm, 0),
    (*b"pm\0", Kw::AmPm, 12),
    (*b"ut\0", Kw::TimeZoneName, 0),
    (*b"utc", Kw::TimeZoneName, 0),
    (*b"z\0\0", Kw::TimeZoneName, 0),
    (*b"gmt", Kw::TimeZoneName, 0),
    (*b"cdt", Kw::TimeZoneName, -5),
    (*b"cst", Kw::TimeZoneName, -6),
    (*b"edt", Kw::TimeZoneName, -4),
    (*b"est", Kw::TimeZoneName, -5),
    (*b"mdt", Kw::TimeZoneName, -6),
    (*b"mst", Kw::TimeZoneName, -7),
    (*b"pdt", Kw::TimeZoneName, -7),
    (*b"pst", Kw::TimeZoneName, -8),
    (*b"t\0\0", Kw::TimeSeparator, 0),
];

fn lookup_keyword(prefix: [u32; 3], len: usize) -> (Kw, i32) {
    for (word, kind, value) in KEYWORDS {
        let matches = (0..3).all(|i| prefix[i] == word[i] as u32);
        // A word longer than the keyword only matches month names.
        if matches && (len <= 3 || kind == Kw::MonthName) {
            return (kind, value);
        }
    }
    (Kw::Invalid, 0)
}

struct Scanner<'a> {
    reader: Reader<'a>,
    next: Token,
}

impl<'a> Scanner<'a> {
    fn new(s: &'a [u16]) -> Self {
        let mut reader = Reader::new(s);
        let next = scan(&mut reader);
        Scanner { reader, next }
    }
    fn next(&mut self) -> Token {
        let result = self.next;
        self.next = scan(&mut self.reader);
        result
    }
    fn peek(&self) -> Token {
        self.next
    }
    fn skip_symbol(&mut self, c: u8) -> bool {
        if self.next.is_symbol(c) {
            self.next = scan(&mut self.reader);
            true
        } else {
            false
        }
    }
}

fn scan(r: &mut Reader) -> Token {
    let start = r.position();
    if r.is_end() {
        return Token::End;
    }
    if r.is_digit() {
        let value = r.read_unsigned_numeral();
        return Token::Number { value, len: r.position() - start };
    }
    for &c in b":-+.)" {
        if r.skip(c) {
            return Token::Symbol(c);
        }
    }
    if r.is_alpha_or_above() && !is_white_space(r.ch) {
        let (len, prefix) = r.read_word();
        let (kind, value) = lookup_keyword(prefix, len);
        return Token::Keyword { kind, value, len };
    }
    if is_white_space(r.ch) {
        r.next();
        return Token::WhiteSpace;
    }
    if r.skip_parentheses() {
        return Token::Unknown;
    }
    r.next();
    Token::Unknown
}

fn between(x: i32, lo: i32, hi: i32) -> bool {
    lo <= x && x <= hi
}

fn is_month(x: i32) -> bool {
    between(x, 1, 12)
}
fn is_day(x: i32) -> bool {
    between(x, 1, 31)
}
fn is_hour(x: i32) -> bool {
    between(x, 0, 23)
}
fn is_minute(x: i32) -> bool {
    between(x, 0, 59)
}
fn is_second(x: i32) -> bool {
    between(x, 0, 59)
}
fn is_hour12(x: i32) -> bool {
    between(x, 0, 12)
}
fn is_millisecond(x: i32) -> bool {
    between(x, 0, 999)
}

struct DayComposer {
    comp: [i32; 3],
    index: usize,
    named_month: i32,
    is_iso_date: bool,
}

impl DayComposer {
    fn is_empty(&self) -> bool {
        self.index == 0
    }
    fn add(&mut self, n: i32) -> bool {
        if self.index < 3 {
            self.comp[self.index] = n;
            self.index += 1;
            true
        } else {
            false
        }
    }
    /// (year, month 0..=11, day).
    fn write(&mut self) -> Option<(i32, i32, i32)> {
        if self.index < 1 {
            return None;
        }
        // Day and month default to 1.
        while self.index < 3 {
            self.comp[self.index] = 1;
            self.index += 1;
        }
        let mut year = 0;
        let (month, day);
        if self.named_month == NONE {
            if self.is_iso_date || (self.index == 3 && !is_day(self.comp[0])) {
                year = self.comp[0];
                month = self.comp[1];
                day = self.comp[2];
            } else {
                month = self.comp[0];
                day = self.comp[1];
                if self.index == 3 {
                    year = self.comp[2];
                }
            }
        } else {
            month = self.named_month;
            if self.index == 1 {
                day = self.comp[0];
            } else if !is_day(self.comp[0]) {
                year = self.comp[0];
                day = self.comp[1];
            } else {
                day = self.comp[0];
                year = self.comp[1];
            }
        }
        if !self.is_iso_date {
            if between(year, 0, 49) {
                year += 2000;
            } else if between(year, 50, 99) {
                year += 1900;
            }
        }
        if !is_month(month) || !is_day(day) {
            return None;
        }
        Some((year, month - 1, day))
    }
}

struct TimeComposer {
    comp: [i32; 4],
    index: usize,
    hour_offset: i32,
}

impl TimeComposer {
    fn is_empty(&self) -> bool {
        self.index == 0
    }
    fn is_expecting(&self, n: i32) -> bool {
        (self.index == 1 && is_minute(n)) || (self.index == 2 && is_second(n)) || (self.index == 3 && is_millisecond(n))
    }
    fn add(&mut self, n: i32) -> bool {
        if self.index < 4 {
            self.comp[self.index] = n;
            self.index += 1;
            true
        } else {
            false
        }
    }
    fn add_final(&mut self, n: i32) -> bool {
        if !self.add(n) {
            return false;
        }
        while self.index < 4 {
            self.comp[self.index] = 0;
            self.index += 1;
        }
        true
    }
    fn write(&mut self) -> Option<[i32; 4]> {
        while self.index < 4 {
            self.comp[self.index] = 0;
            self.index += 1;
        }
        let [mut hour, minute, second, ms] = self.comp;
        if self.hour_offset != NONE {
            if !is_hour12(hour) {
                return None;
            }
            hour %= 12;
            hour += self.hour_offset;
        }
        if !is_hour(hour) || !is_minute(minute) || !is_second(second) || !is_millisecond(ms) {
            // A 24th hour is allowed when the rest is zero.
            if hour != 24 || minute != 0 || second != 0 || ms != 0 {
                return None;
            }
        }
        Some([hour, minute, second, ms])
    }
}

struct TimeZoneComposer {
    sign: i32,
    hour: i32,
    minute: i32,
}

impl TimeZoneComposer {
    fn set(&mut self, offset_in_hours: i32) {
        self.sign = if offset_in_hours < 0 { -1 } else { 1 };
        self.hour = offset_in_hours * self.sign;
        self.minute = 0;
    }
    fn set_sign(&mut self, sign: i32) {
        self.sign = if sign < 0 { -1 } else { 1 };
    }
    fn is_expecting(&self, n: i32) -> bool {
        self.hour != NONE && self.minute == NONE && is_minute(n)
    }
    fn is_utc(&self) -> bool {
        self.hour == 0 && self.minute == 0
    }
    fn is_empty(&self) -> bool {
        self.hour == NONE
    }
    /// Seconds east of UTC, `Some(None)` for local time.
    fn write(&mut self) -> Option<Option<i32>> {
        if self.sign == NONE {
            return Some(None);
        }
        if self.hour == NONE {
            self.hour = 0;
        }
        if self.minute == NONE {
            self.minute = 0;
        }
        let total = (self.hour as u32).wrapping_mul(3600).wrapping_add((self.minute as u32).wrapping_mul(60));
        if total > i32::MAX as u32 {
            return None;
        }
        let total = total as i32;
        Some(Some(if self.sign < 0 { -total } else { total }))
    }
}

fn read_milliseconds(token: Token) -> i32 {
    let Token::Number { value, len } = token else {
        return 0;
    };
    let mut number = value;
    let mut length = len;
    if length == 1 {
        number *= 100;
    } else if length == 2 {
        number *= 10;
    } else if length > 3 {
        if length > MAX_SIGNIFICANT_DIGITS {
            length = MAX_SIGNIFICANT_DIGITS;
        }
        let mut factor = 1;
        loop {
            factor *= 10;
            length -= 1;
            if length <= 3 {
                break;
            }
        }
        number /= factor;
    }
    number
}

struct Composers {
    day: DayComposer,
    time: TimeComposer,
    tz: TimeZoneComposer,
}

/// V8's ParseES5DateTime: the next token for the legacy reader, `End` when done, `Invalid` to fail.
fn parse_es5(scanner: &mut Scanner, c: &mut Composers) -> Token {
    let (day, time, tz) = (&mut c.day, &mut c.time, &mut c.tz);
    // Date: [('-'|'+')yy]yyyy['-'MM['-'DD]]
    if scanner.peek().is_sign() {
        // Keep the sign token, so invalid dates can be detected later.
        let sign_token = scanner.next();
        if !scanner.peek().is_fixed_length_number(6) {
            return sign_token;
        }
        let sign = sign_token.sign();
        let year = scanner.next().number();
        if sign < 0 && year == 0 {
            return sign_token;
        }
        day.add(sign * year);
    } else if scanner.peek().is_fixed_length_number(4) {
        day.add(scanner.next().number());
    } else {
        return scanner.next();
    }
    if scanner.skip_symbol(b'-') {
        if !scanner.peek().is_fixed_length_number(2) || !is_month(scanner.peek().number()) {
            return scanner.next();
        }
        day.add(scanner.next().number());
        if scanner.skip_symbol(b'-') {
            if !scanner.peek().is_fixed_length_number(2) || !is_day(scanner.peek().number()) {
                return scanner.next();
            }
            day.add(scanner.next().number());
        }
    }
    // Time: 'T'HH':'mm[':'ss['.'sss]][Z|(+|-)hh[':']mm]
    if !scanner.peek().is_time_separator() {
        if scanner.peek() != Token::End {
            return scanner.next();
        }
    } else {
        scanner.next();
        if !scanner.peek().is_fixed_length_number(2) || !between(scanner.peek().number(), 0, 24) {
            return Token::Invalid;
        }
        // 24:00[:00[.000]] only.
        let hour_is_24 = scanner.peek().number() == 24;
        time.add(scanner.next().number());
        if !scanner.skip_symbol(b':') {
            return Token::Invalid;
        }
        if !scanner.peek().is_fixed_length_number(2)
            || !is_minute(scanner.peek().number())
            || (hour_is_24 && scanner.peek().number() > 0)
        {
            return Token::Invalid;
        }
        time.add(scanner.next().number());
        if scanner.skip_symbol(b':') {
            if !scanner.peek().is_fixed_length_number(2)
                || !is_second(scanner.peek().number())
                || (hour_is_24 && scanner.peek().number() > 0)
            {
                return Token::Invalid;
            }
            time.add(scanner.next().number());
            if scanner.skip_symbol(b'.') {
                if !scanner.peek().is_number() || (hour_is_24 && scanner.peek().number() > 0) {
                    return Token::Invalid;
                }
                time.add(read_milliseconds(scanner.next()));
            }
        }
        if scanner.peek().is_keyword_z() {
            scanner.next();
            tz.set(0);
        } else if scanner.peek().is_sign() {
            tz.set_sign(scanner.next().sign());
            if scanner.peek().is_fixed_length_number(4) {
                // hhmm extension syntax.
                let hourmin = scanner.next().number();
                let (hour, min) = (hourmin / 100, hourmin % 100);
                if !is_hour(hour) || !is_minute(min) {
                    return Token::Invalid;
                }
                tz.hour = hour;
                tz.minute = min;
            } else {
                if !scanner.peek().is_fixed_length_number(2) || !is_hour(scanner.peek().number()) {
                    return Token::Invalid;
                }
                tz.hour = scanner.next().number();
                if !scanner.skip_symbol(b':') {
                    return Token::Invalid;
                }
                if !scanner.peek().is_fixed_length_number(2) || !is_minute(scanner.peek().number()) {
                    return Token::Invalid;
                }
                tz.minute = scanner.next().number();
            }
        }
        if scanner.peek() != Token::End {
            return Token::Invalid;
        }
    }
    // Without an offset, date-only forms are UTC and date-time forms local time.
    if tz.is_empty() && time.is_empty() {
        tz.set(0);
    }
    day.is_iso_date = true;
    Token::End
}

/// The parsed fields: (year, month 0..=11, day, [hour, minute, second, ms], offset seconds east or None for local).
type Parsed = (i32, i32, i32, [i32; 4], Option<i32>);

fn parse_fields(s: &[u16]) -> Option<Parsed> {
    let mut scanner = Scanner::new(s);
    let mut c = Composers {
        day: DayComposer { comp: [0; 3], index: 0, named_month: NONE, is_iso_date: false },
        time: TimeComposer { comp: [0; 4], index: 0, hour_offset: NONE },
        tz: TimeZoneComposer { sign: NONE, hour: NONE, minute: NONE },
    };
    let next_unhandled = parse_es5(&mut scanner, &mut c);
    if next_unhandled == Token::Invalid {
        return None;
    }
    let Composers { day, time, tz } = &mut c;
    let mut has_read_number = !day.is_empty();
    let mut token = next_unhandled;
    while token != Token::End {
        match token {
            Token::Number { value: n, .. } => {
                has_read_number = true;
                if scanner.skip_symbol(b':') {
                    if scanner.skip_symbol(b':') {
                        // n + "::"
                        if !time.is_empty() {
                            return None;
                        }
                        time.add(n);
                        time.add(0);
                    } else {
                        // n + ":"
                        if !time.add(n) {
                            return None;
                        }
                        if scanner.peek().is_symbol(b'.') {
                            scanner.next();
                        }
                    }
                } else if scanner.skip_symbol(b'.') && time.is_expecting(n) {
                    time.add(n);
                    if !scanner.peek().is_number() {
                        return None;
                    }
                    let ms = read_milliseconds(scanner.next());
                    if ms < 0 {
                        return None;
                    }
                    time.add_final(ms);
                } else if tz.is_expecting(n) {
                    tz.minute = n;
                } else if time.is_expecting(n) {
                    time.add_final(n);
                    // The time must be followed by the end, whitespace, "Z", "+" or "-".
                    let peek = scanner.peek();
                    if peek != Token::End && peek != Token::WhiteSpace && !peek.is_keyword_z() && !peek.is_sign() {
                        return None;
                    }
                } else {
                    if !day.add(n) {
                        return None;
                    }
                    scanner.skip_symbol(b'-');
                }
            }
            Token::Keyword { kind, value, .. } => {
                if kind == Kw::AmPm && !time.is_empty() {
                    time.hour_offset = value;
                } else if kind == Kw::MonthName {
                    day.named_month = value;
                    scanner.skip_symbol(b'-');
                } else if kind == Kw::TimeZoneName && has_read_number {
                    tz.set(value);
                } else {
                    // Garbage words are illegal once a number has been read, and must be separated from the first
                    // number.
                    if has_read_number || scanner.peek().is_number() {
                        return None;
                    }
                }
            }
            _ if token.is_sign() && (tz.is_utc() || !time.is_empty()) => {
                // A UTC offset, only after UTC or a time.
                tz.set_sign(token.sign());
                let (mut n, mut length) = (0, 0);
                if let Token::Number { value, len } = scanner.peek() {
                    scanner.next();
                    n = value;
                    length = len;
                }
                has_read_number = true;
                if scanner.peek().is_symbol(b':') {
                    tz.hour = n;
                    tz.minute = NONE;
                } else if length == 1 || length == 2 {
                    tz.hour = n;
                    tz.minute = 0;
                } else if length == 3 || length == 4 {
                    tz.hour = n / 100;
                    tz.minute = n % 100;
                } else {
                    return None;
                }
            }
            _ if (token.is_sign() || token.is_symbol(b')')) && has_read_number => return None,
            _ => {}
        }
        token = scanner.next();
    }
    let (year, month, date) = day.write()?;
    let time = time.write()?;
    let offset = tz.write()?;
    Some((year, month, date, time, offset))
}

pub(super) fn parse(s: &[u16]) -> f64 {
    let Some((year, month, date, [hour, minute, second, ms], offset)) = parse_fields(s) else {
        return f64::NAN;
    };
    let days = civil::days_from_civil(year as i64, month as u32 + 1, 1) + date as i64 - 1;
    let time = hour as f64 * 3_600_000.0 + minute as f64 * 60_000.0 + second as f64 * 1000.0 + ms as f64;
    let t = days as f64 * MS_PER_DAY + time;
    match offset {
        None => {
            if t.abs() > MAX_TIME + 864_000_000.0 {
                return f64::NAN;
            }
            time_clip(local_to_utc(t))
        }
        Some(offset) => time_clip(t - offset as f64 * 1000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> f64 {
        parse(&s.encode_utf16().collect::<Vec<_>>())
    }

    #[test]
    fn iso_and_legacy() {
        assert_eq!(p("2020-01-01"), 1577836800000.0);
        assert_eq!(p("2020-01-01T00:00:00.1239Z"), 1577836800123.0);
        assert_eq!(p("2020-02-30"), 1583020800000.0);
        assert_eq!(p("+275760-09-13T00:00:00.000Z"), 8.64e15);
        assert!(p("+275760-09-13T00:00:00.001Z").is_nan());
        assert_eq!(p("Tue, 01 Mar 2022 10:00:00 GMT+0100"), 1646125200000.0);
        assert_eq!(p("Jan 1 2020 GMT+123456789:00"), Date::from_iso("1954-08-19T21:31:28Z"));
        assert!(p("2020-01-01T00:00:60Z").is_nan());
        assert!(p("31").is_nan());
        assert!(p("").is_nan());
    }

    struct Date;
    impl Date {
        fn from_iso(s: &str) -> f64 {
            p(s)
        }
    }
}
