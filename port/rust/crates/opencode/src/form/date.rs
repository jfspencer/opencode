//! Whether `new Date(text)` is a valid date: the engine's date parser (V8's `DateParser`, whose rules the reference's
//! engine follows): the ECMAScript date-time format first, then the legacy rules for what is left (garbage words
//! before the first number, month names, `m/d/y` and `y/m/d` numbers, times, AM/PM, time zones, parenthesized
//! comments). Only validity is computed; the value itself is not needed.

const NONE: i64 = i64::MIN;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Keyword {
    Month(i64),
    AmPm(i64),
    Zone(i64),
    TimeSeparator,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Token {
    Number { value: i64, length: usize },
    Symbol(char),
    Word { keyword: Keyword, length: usize },
    WhiteSpace,
    Unknown,
    End,
    /// `DateToken::Invalid()`: the ECMAScript format failed past the point of no return.
    Invalid,
}

impl Token {
    fn is_number(&self) -> bool {
        matches!(self, Token::Number { .. })
    }

    fn fixed(&self, n: usize) -> Option<i64> {
        match self {
            Token::Number { value, length } if *length == n => Some(*value),
            _ => None,
        }
    }

    fn is_sign(&self) -> bool {
        matches!(self, Token::Symbol('+' | '-'))
    }

    fn is_z(&self) -> bool {
        matches!(self, Token::Word { keyword: Keyword::Zone(0), length: 1 })
    }
}

/// JS WhiteSpace and LineTerminator.
fn white(c: char) -> bool {
    crate::text::is_js_whitespace(c) || matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `KeywordTable::Lookup`: a word matches by its first three (lowercased) characters; only month names may be longer.
fn keyword(prefix: [char; 3], length: usize) -> Keyword {
    const TABLE: &[([char; 3], Keyword)] = &[
        (['j', 'a', 'n'], Keyword::Month(1)),
        (['f', 'e', 'b'], Keyword::Month(2)),
        (['m', 'a', 'r'], Keyword::Month(3)),
        (['a', 'p', 'r'], Keyword::Month(4)),
        (['m', 'a', 'y'], Keyword::Month(5)),
        (['j', 'u', 'n'], Keyword::Month(6)),
        (['j', 'u', 'l'], Keyword::Month(7)),
        (['a', 'u', 'g'], Keyword::Month(8)),
        (['s', 'e', 'p'], Keyword::Month(9)),
        (['o', 'c', 't'], Keyword::Month(10)),
        (['n', 'o', 'v'], Keyword::Month(11)),
        (['d', 'e', 'c'], Keyword::Month(12)),
        (['a', 'm', '\0'], Keyword::AmPm(0)),
        (['p', 'm', '\0'], Keyword::AmPm(12)),
        (['u', 't', '\0'], Keyword::Zone(0)),
        (['u', 't', 'c'], Keyword::Zone(0)),
        (['z', '\0', '\0'], Keyword::Zone(0)),
        (['g', 'm', 't'], Keyword::Zone(0)),
        (['c', 'd', 't'], Keyword::Zone(-5)),
        (['c', 's', 't'], Keyword::Zone(-6)),
        (['e', 'd', 't'], Keyword::Zone(-4)),
        (['e', 's', 't'], Keyword::Zone(-5)),
        (['m', 'd', 't'], Keyword::Zone(-6)),
        (['m', 's', 't'], Keyword::Zone(-7)),
        (['p', 'd', 't'], Keyword::Zone(-7)),
        (['p', 's', 't'], Keyword::Zone(-8)),
        (['t', '\0', '\0'], Keyword::TimeSeparator),
    ];
    TABLE
        .iter()
        .find(|(word, kind)| *word == prefix && (length <= 3 || matches!(kind, Keyword::Month(_))))
        .map_or(Keyword::Invalid, |(_, kind)| *kind)
}

/// `DateStringTokenizer`.
struct Scanner {
    chars: Vec<char>,
    at: usize,
    next: Token,
}

impl Scanner {
    fn new(text: &str) -> Self {
        let mut scanner = Scanner { chars: text.chars().collect(), at: 0, next: Token::End };
        scanner.next = scanner.scan();
        scanner
    }

    fn ch(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn scan(&mut self) -> Token {
        let Some(c) = self.ch() else { return Token::End };
        if c.is_ascii_digit() {
            let start = self.at;
            // `ReadUnsignedNumeral`: leading zeros are skipped, then digits beyond the ninth are read but not counted.
            while self.ch() == Some('0') {
                self.at += 1;
            }
            let (mut value, mut significant) = (0i64, 0usize);
            while let Some(d) = self.ch().filter(char::is_ascii_digit) {
                if significant < 9 {
                    value = value * 10 + (d as i64 - '0' as i64);
                }
                significant += 1;
                self.at += 1;
            }
            return Token::Number { value, length: self.at - start };
        }
        if matches!(c, ':' | '-' | '+' | '.' | ')') {
            self.at += 1;
            return Token::Symbol(c);
        }
        if c >= 'A' && !white(c) {
            let mut prefix = ['\0'; 3];
            let mut length = 0;
            while let Some(c) = self.ch().filter(|c| *c >= 'A' && !white(*c)) {
                if length < 3 {
                    // `AsciiAlphaToLower`: `c | 0x20`.
                    prefix[length] = char::from_u32(c as u32 | 0x20).unwrap_or(c);
                }
                length += 1;
                self.at += 1;
            }
            return Token::Word { keyword: keyword(prefix, length), length };
        }
        if white(c) {
            while self.ch().is_some_and(white) {
                self.at += 1;
            }
            return Token::WhiteSpace;
        }
        if c == '(' {
            let mut balance = 0;
            loop {
                match self.ch() {
                    Some(')') => balance -= 1,
                    Some('(') => balance += 1,
                    _ => {}
                }
                self.at += 1;
                if balance <= 0 || self.ch().is_none() {
                    break;
                }
            }
            return Token::Unknown;
        }
        self.at += 1;
        Token::Unknown
    }

    fn peek(&self) -> Token {
        self.next
    }

    fn next(&mut self) -> Token {
        let token = self.next;
        self.next = self.scan();
        token
    }

    fn skip_symbol(&mut self, symbol: char) -> bool {
        if self.next == Token::Symbol(symbol) {
            self.next();
            return true;
        }
        false
    }
}

fn between(x: i64, low: i64, high: i64) -> bool {
    x >= low && x <= high
}

#[derive(Default)]
struct Day {
    comp: Vec<i64>,
    named_month: Option<i64>,
    iso: bool,
}

impl Day {
    fn add(&mut self, n: i64) -> bool {
        if self.comp.len() < 3 {
            self.comp.push(n);
            return true;
        }
        false
    }

    /// `DayComposer::Write`: the year, month and day, when they form a date.
    fn write(&self) -> Option<(i64, i64, i64)> {
        if self.comp.is_empty() {
            return None;
        }
        let mut comp = self.comp.clone();
        comp.resize(3, 1);
        let is_day = |x: i64| between(x, 1, 31);
        let (mut year, month, day) = match self.named_month {
            None if self.iso || !is_day(comp[0]) => (comp[0], comp[1], comp[2]),
            None => (comp[2], comp[0], comp[1]),
            Some(month) if !is_day(comp[0]) => (comp[0], month, comp[1]),
            Some(month) => (comp[1], month, comp[0]),
        };
        if !self.iso {
            if between(year, 0, 49) {
                year += 2000;
            } else if between(year, 50, 99) {
                year += 1900;
            }
        }
        (between(month, 1, 12) && is_day(day)).then_some((year, month, day))
    }
}

#[derive(Default)]
struct Time {
    comp: Vec<i64>,
    hour_offset: Option<i64>,
}

impl Time {
    fn is_empty(&self) -> bool {
        self.comp.is_empty()
    }

    fn add(&mut self, n: i64) -> bool {
        if self.comp.len() < 4 {
            self.comp.push(n);
            return true;
        }
        false
    }

    fn add_final(&mut self, n: i64) -> bool {
        if !self.add(n) {
            return false;
        }
        self.comp.resize(4, 0);
        true
    }

    fn is_expecting(&self, n: i64) -> bool {
        match self.comp.len() {
            1 => between(n, 0, 59),
            2 => between(n, 0, 59),
            3 => between(n, 0, 999),
            _ => false,
        }
    }

    fn write(&self) -> bool {
        let mut comp = self.comp.clone();
        comp.resize(4, 0);
        let (mut hour, minute, second, millisecond) = (comp[0], comp[1], comp[2], comp[3]);
        if let Some(offset) = self.hour_offset {
            if !between(hour, 0, 12) {
                return false;
            }
            hour = hour % 12 + offset;
        }
        let valid = between(hour, 0, 23) && between(minute, 0, 59) && between(second, 0, 59) && between(millisecond, 0, 999);
        valid || (hour == 24 && minute == 0 && second == 0 && millisecond == 0)
    }
}

struct Zone {
    sign: Option<i64>,
    hour: i64,
    minute: i64,
}

impl Default for Zone {
    fn default() -> Self {
        Zone { sign: None, hour: NONE, minute: NONE }
    }
}

impl Zone {
    fn set(&mut self, offset: i64) {
        self.sign = Some(if offset < 0 { -1 } else { 1 });
        self.hour = offset.abs();
        self.minute = 0;
    }

    fn is_expecting(&self, n: i64) -> bool {
        self.hour != NONE && self.minute == NONE && between(n, 0, 59)
    }

    fn is_utc(&self) -> bool {
        self.hour == 0 && self.minute == 0
    }

    fn is_empty(&self) -> bool {
        self.sign.is_none()
    }

    /// `TimeZoneComposer::Write`: an explicit offset, in seconds computed with wrapping unsigned 32-bit arithmetic,
    /// must fit a signed 32-bit integer (as the engine does it).
    fn write(&self) -> bool {
        if self.sign.is_none() {
            return true;
        }
        let hour = if self.hour == NONE { 0 } else { self.hour as u32 };
        let minute = if self.minute == NONE { 0 } else { self.minute as u32 };
        hour.wrapping_mul(3600).wrapping_add(minute.wrapping_mul(60)) <= i32::MAX as u32
    }
}

/// `ReadMilliseconds`: the three most significant digits.
fn milliseconds(value: i64, length: usize) -> i64 {
    match length {
        1 => value * 100,
        2 => value * 10,
        3 => value,
        n => value / 10i64.pow((n.min(9) - 3) as u32),
    }
}

/// `ParseES5DateTime`: the token the legacy rules continue with, `End` when the whole text was ES5, `Invalid` when
/// it failed for good.
fn es5(scanner: &mut Scanner, day: &mut Day, time: &mut Time, tz: &mut Zone) -> Token {
    if scanner.peek().is_sign() {
        let sign = scanner.next();
        let Some(year) = scanner.peek().fixed(6) else { return sign };
        // The year is consumed even when `-000000` is refused.
        scanner.next();
        if sign == Token::Symbol('-') && year == 0 {
            return sign;
        }
        day.add(if sign == Token::Symbol('-') { -year } else { year });
    } else if let Some(year) = scanner.peek().fixed(4) {
        scanner.next();
        day.add(year);
    } else {
        return scanner.next();
    }
    if scanner.skip_symbol('-') {
        match scanner.peek().fixed(2) {
            Some(month) if between(month, 1, 12) => {
                scanner.next();
                day.add(month);
            }
            _ => return scanner.next(),
        }
        if scanner.skip_symbol('-') {
            match scanner.peek().fixed(2) {
                Some(d) if between(d, 1, 31) => {
                    scanner.next();
                    day.add(d);
                }
                _ => return scanner.next(),
            }
        }
    }
    if !matches!(scanner.peek(), Token::Word { keyword: Keyword::TimeSeparator, .. }) {
        if scanner.peek() != Token::End {
            return scanner.next();
        }
    } else {
        scanner.next();
        let Some(hour) = scanner.peek().fixed(2).filter(|h| between(*h, 0, 24)) else { return Token::Invalid };
        let hour_is_24 = hour == 24;
        scanner.next();
        if !scanner.skip_symbol(':') {
            return Token::Invalid;
        }
        let Some(minute) = scanner.peek().fixed(2).filter(|m| between(*m, 0, 59) && !(hour_is_24 && *m > 0)) else {
            return Token::Invalid;
        };
        scanner.next();
        time.add(hour);
        time.add(minute);
        if scanner.skip_symbol(':') {
            let Some(second) = scanner.peek().fixed(2).filter(|s| between(*s, 0, 59) && !(hour_is_24 && *s > 0)) else {
                return Token::Invalid;
            };
            scanner.next();
            time.add(second);
            if scanner.skip_symbol('.') {
                let Token::Number { value, length } = scanner.peek() else { return Token::Invalid };
                if hour_is_24 && value > 0 {
                    return Token::Invalid;
                }
                scanner.next();
                time.add(milliseconds(value, length));
            }
        }
        if scanner.peek().is_z() {
            scanner.next();
            tz.set(0);
        } else if scanner.peek().is_sign() {
            tz.sign = Some(if scanner.next() == Token::Symbol('+') { 1 } else { -1 });
            if let Some(hourmin) = scanner.peek().fixed(4) {
                scanner.next();
                let (h, m) = (hourmin / 100, hourmin % 100);
                if !between(h, 0, 23) || !between(m, 0, 59) {
                    return Token::Invalid;
                }
                tz.hour = h;
                tz.minute = m;
            } else {
                let Some(h) = scanner.peek().fixed(2).filter(|h| between(*h, 0, 23)) else { return Token::Invalid };
                scanner.next();
                tz.hour = h;
                if !scanner.skip_symbol(':') {
                    return Token::Invalid;
                }
                let Some(m) = scanner.peek().fixed(2).filter(|m| between(*m, 0, 59)) else { return Token::Invalid };
                scanner.next();
                tz.minute = m;
            }
        }
        if scanner.peek() != Token::End {
            return Token::Invalid;
        }
    }
    if tz.is_empty() && time.is_empty() {
        tz.set(0);
    }
    day.iso = true;
    Token::End
}

/// `DateParser::Parse`, then `MakeDate` and `TimeClip`: whether the text names a time value.
pub fn parse(text: &str) -> bool {
    let mut scanner = Scanner::new(text);
    let (mut day, mut time, mut tz) = (Day::default(), Time::default(), Zone::default());
    let mut token = es5(&mut scanner, &mut day, &mut time, &mut tz);
    if token == Token::Invalid {
        return false;
    }
    let mut has_read_number = !day.comp.is_empty();
    while token != Token::End {
        match token {
            Token::Number { value: n, length: _ } => {
                has_read_number = true;
                if scanner.skip_symbol(':') {
                    if scanner.skip_symbol(':') {
                        if !time.is_empty() {
                            return false;
                        }
                        time.add(n);
                        time.add(0);
                    } else {
                        if !time.add(n) {
                            return false;
                        }
                        if scanner.peek() == Token::Symbol('.') {
                            scanner.next();
                        }
                    }
                } else if scanner.skip_symbol('.') && time.is_expecting(n) {
                    time.add(n);
                    let Token::Number { value, length } = scanner.peek() else { return false };
                    scanner.next();
                    time.add_final(milliseconds(value, length));
                } else if tz.is_expecting(n) {
                    tz.minute = n;
                } else if time.is_expecting(n) {
                    time.add_final(n);
                    let peek = scanner.peek();
                    if !matches!(peek, Token::End | Token::WhiteSpace) && !peek.is_z() && !peek.is_sign() {
                        return false;
                    }
                } else {
                    if !day.add(n) {
                        return false;
                    }
                    scanner.skip_symbol('-');
                }
            }
            Token::Word { keyword, .. } => match keyword {
                Keyword::AmPm(offset) if !time.is_empty() => time.hour_offset = Some(offset),
                Keyword::Month(month) => {
                    day.named_month = Some(month);
                    scanner.skip_symbol('-');
                }
                Keyword::Zone(offset) if has_read_number => tz.set(offset),
                _ => {
                    // Garbage words are illegal once a number was read, and must be separated from the first one.
                    if has_read_number || scanner.peek().is_number() {
                        return false;
                    }
                }
            },
            sign @ Token::Symbol('+' | '-') if tz.is_utc() || !time.is_empty() => {
                tz.sign = Some(if sign == Token::Symbol('+') { 1 } else { -1 });
                let (n, length) = match scanner.peek() {
                    Token::Number { value, length } => {
                        scanner.next();
                        (value, length)
                    }
                    _ => (0, 0),
                };
                has_read_number = true;
                if scanner.peek() == Token::Symbol(':') {
                    tz.hour = n;
                    tz.minute = NONE;
                } else if length == 1 || length == 2 {
                    tz.hour = n;
                    tz.minute = 0;
                } else if length == 3 || length == 4 {
                    tz.hour = n / 100;
                    tz.minute = n % 100;
                } else {
                    return false;
                }
            }
            Token::Symbol('+' | '-' | ')') if has_read_number => return false,
            _ => {}
        }
        token = scanner.next();
    }
    let Some((year, month, day)) = day.write() else { return false };
    // `TimeClip`: within 8.64e15 ms of the epoch, 100,000,000 days either way (the time of day and the offset only
    // matter on the boundary days, which are not distinguished).
    time.write() && tz.write() && days_from_civil(year, month, day).abs() <= 100_000_000
}

/// Days since 1970-01-01 of a proleptic Gregorian date (days beyond the month's length roll over, as `MakeDay` does).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn engine_cases() {
        // Recorded from the reference's engine (`!Number.isNaN(new Date(text).getTime())`).
        let valid = [
            "2026-02-28T10:00:00Z", "2026-02-28", "2026-02", "2026", "+002026-02-28", "-000000-01-01", "2026-02-28T10:00",
            "2026-02-28T10:00:00.5", "2026-02-28T24:00:00", "2026-02-28T10:00:00+0200", "2026-02-28T10:00:00+02:00",
            "2026-02-28 10:00:00", "2026-02-30", "2026-02-30T00:00:00Z", "Feb 28 2026", "Tue, 01 Jan 2026 00:00:00 GMT",
            "12/25/1995", "1995/12/25", "December 25, 1995", "25 December 1995", "10", "2026-1-5", "Jan 2026", "1/2",
            "Feb 30 2026", "2026-02-28T10:00:00z", "2026-02-28t10:00:00Z", "abc 2026",
            "Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)", "1 2 3", "2026 Feb 28", "28 Feb",
            "Sept 5 2020", "5 Sept 2020", "2020-06-15T12:00:00.000-07:00", "-000000-01-01", "-000000 feb00",
            "-000000+0200 ", "-000000z-271821", "12:60+2757611:2:3", "1/1/2020 10:00 +596523:00", "1/1/2020 10:00 +1193046:30",
        ];
        let invalid = [
            "yesterday", "2026-02-28T24:00:01", "now", "", " ", "13/1/2020", "2020/13/1", "Mon", "10:00",
            "2026-02-28T10:00:00.Z", "2026-02-28T1:00", "2026-02-28T10:00:00 GMT", "-000000", "-000000 Sep ",
            "-000000  1:2:3 +02:00PM", "z Sep0000000502026-13-01", "1/1/2020 10:00 +596523:30", "1/1/2020 10:00 -1193046:00",
        ];
        for text in valid {
            assert!(parse(text), "{text} is a date");
        }
        for text in invalid {
            assert!(!parse(text), "{text} is not a date");
        }
    }
}

