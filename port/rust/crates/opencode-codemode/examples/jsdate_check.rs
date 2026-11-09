//! The Rust side of the jsdate differential check: reads the cases `harness/jsdate/oracle.ts` printed (stdin), recomputes
//! each with `opencode_codemode::jsdate`, and reports every difference. Run under the TZ the oracle ran under.
//!
//! usage: TZ=UTC bun harness/jsdate/oracle.ts | TZ=UTC cargo run --offline --example jsdate_check [-- max-shown]

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use opencode_codemode::jsdate::{self, Getter, Setter};

#[derive(Debug, Clone)]
enum Value {
    Num(f64),
    Str(Vec<u16>),
    Null,
    Arr(Vec<Value>),
}

struct Json<'a> {
    s: &'a [u8],
    at: usize,
}

impl Json<'_> {
    fn ws(&mut self) {
        while self.at < self.s.len() && matches!(self.s[self.at], b' ' | b'\t' | b'\n' | b'\r') {
            self.at += 1;
        }
    }
    fn value(&mut self) -> Result<Value, String> {
        self.ws();
        match self.s.get(self.at) {
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(Value::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    self.ws();
                    match self.s.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Value::Arr(items));
                        }
                        _ => return Err(format!("bad array at {}", self.at)),
                    }
                }
            }
            Some(b'"') => self.string().map(Value::Str),
            Some(b'n') if self.s[self.at..].starts_with(b"null") => {
                self.at += 4;
                Ok(Value::Null)
            }
            Some(_) => {
                let start = self.at;
                while self.at < self.s.len() && matches!(self.s[self.at], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.at += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.at]).map_err(|e| e.to_string())?;
                text.parse::<f64>().map(Value::Num).map_err(|e| format!("bad number {text:?}: {e}"))
            }
            None => Err("unexpected end".into()),
        }
    }
    fn hex4(&mut self) -> Result<u16, String> {
        let text = std::str::from_utf8(self.s.get(self.at..self.at + 4).ok_or("short escape")?).map_err(|e| e.to_string())?;
        self.at += 4;
        u16::from_str_radix(text, 16).map_err(|e| e.to_string())
    }
    fn string(&mut self) -> Result<Vec<u16>, String> {
        self.at += 1;
        let mut units = Vec::new();
        loop {
            let c = *self.s.get(self.at).ok_or("unterminated string")?;
            match c {
                b'"' => {
                    self.at += 1;
                    return Ok(units);
                }
                b'\\' => {
                    let e = *self.s.get(self.at + 1).ok_or("bad escape")?;
                    self.at += 2;
                    match e {
                        b'"' => units.push(b'"' as u16),
                        b'\\' => units.push(b'\\' as u16),
                        b'/' => units.push(b'/' as u16),
                        b'b' => units.push(8),
                        b'f' => units.push(12),
                        b'n' => units.push(10),
                        b'r' => units.push(13),
                        b't' => units.push(9),
                        b'u' => units.push(self.hex4()?),
                        _ => return Err("bad escape".into()),
                    }
                }
                _ => {
                    // A run of raw UTF-8 up to the next quote or escape.
                    let start = self.at;
                    while self.at < self.s.len() && self.s[self.at] != b'"' && self.s[self.at] != b'\\' {
                        self.at += 1;
                    }
                    let text = std::str::from_utf8(&self.s[start..self.at]).map_err(|e| e.to_string())?;
                    units.extend(text.encode_utf16());
                }
            }
        }
    }
}

fn num(v: &Value) -> f64 {
    match v {
        Value::Num(n) => *n,
        Value::Str(s) => match String::from_utf16_lossy(s).as_str() {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            "-0" => -0.0,
            other => panic!("not a number: {other}"),
        },
        _ => panic!("not a number: {v:?}"),
    }
}

fn nums(v: &Value) -> Vec<f64> {
    match v {
        Value::Arr(items) => items.iter().map(num).collect(),
        _ => panic!("not an array: {v:?}"),
    }
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => Some(String::from_utf16_lossy(s)),
        Value::Null => None,
        _ => panic!("not a string: {v:?}"),
    }
}

fn same(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}

fn show(x: f64) -> String {
    if x == 0.0 && x.is_sign_negative() { "-0".into() } else { format!("{x}") }
}

const GETTERS: [(Getter, &str); 17] = [
    (Getter::FullYear, "getFullYear"),
    (Getter::Month, "getMonth"),
    (Getter::Date, "getDate"),
    (Getter::Day, "getDay"),
    (Getter::Hours, "getHours"),
    (Getter::Minutes, "getMinutes"),
    (Getter::Seconds, "getSeconds"),
    (Getter::Milliseconds, "getMilliseconds"),
    (Getter::UtcFullYear, "getUTCFullYear"),
    (Getter::UtcMonth, "getUTCMonth"),
    (Getter::UtcDate, "getUTCDate"),
    (Getter::UtcDay, "getUTCDay"),
    (Getter::UtcHours, "getUTCHours"),
    (Getter::UtcMinutes, "getUTCMinutes"),
    (Getter::UtcSeconds, "getUTCSeconds"),
    (Getter::UtcMilliseconds, "getUTCMilliseconds"),
    (Getter::TimezoneOffset, "getTimezoneOffset"),
];

const SETTERS: [(Setter, &str); 15] = [
    (Setter::Time, "setTime"),
    (Setter::Milliseconds, "setMilliseconds"),
    (Setter::UtcMilliseconds, "setUTCMilliseconds"),
    (Setter::Seconds, "setSeconds"),
    (Setter::UtcSeconds, "setUTCSeconds"),
    (Setter::Minutes, "setMinutes"),
    (Setter::UtcMinutes, "setUTCMinutes"),
    (Setter::Hours, "setHours"),
    (Setter::UtcHours, "setUTCHours"),
    (Setter::Date, "setDate"),
    (Setter::UtcDate, "setUTCDate"),
    (Setter::Month, "setMonth"),
    (Setter::UtcMonth, "setUTCMonth"),
    (Setter::FullYear, "setFullYear"),
    (Setter::UtcFullYear, "setUTCFullYear"),
];

struct Report {
    checked: BTreeMap<&'static str, u64>,
    failed: BTreeMap<&'static str, u64>,
    shown: BTreeMap<&'static str, usize>,
    max_shown: usize,
}

impl Report {
    fn check(&mut self, kind: &'static str, ok: bool, describe: impl FnOnce() -> String) {
        *self.checked.entry(kind).or_default() += 1;
        if !ok {
            *self.failed.entry(kind).or_default() += 1;
            let shown = self.shown.entry(kind).or_default();
            if *shown < self.max_shown {
                *shown += 1;
                println!("MISMATCH {kind}: {}", describe());
            }
        }
    }
}

fn args_text(args: &[f64]) -> String {
    args.iter().map(|&x| show(x)).collect::<Vec<_>>().join(", ")
}

fn main() {
    let max_shown = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let mut report = Report { checked: BTreeMap::new(), failed: BTreeMap::new(), shown: BTreeMap::new(), max_shown };
    let stdin = std::io::stdin();
    for line in stdin.lock().split(b'\n') {
        let line = line.expect("read stdin");
        if line.is_empty() {
            continue;
        }
        let row = match (Json { s: &line, at: 0 }).value() {
            Ok(Value::Arr(row)) => row,
            other => panic!("bad line {:?}: {other:?}", String::from_utf8_lossy(&line)),
        };
        let kind = match &row[0] {
            Value::Str(s) => String::from_utf16_lossy(s),
            _ => panic!("bad kind"),
        };
        match kind.as_str() {
            "g" => {
                let t = num(&row[1]);
                for (i, (getter, name)) in GETTERS.iter().enumerate() {
                    let want = num(&row[2 + i]);
                    let got = jsdate::get(t, *getter);
                    report.check("getter", same(got, want), || {
                        format!("new Date({}).{name}() = {} (bun {})", show(t), show(got), show(want))
                    });
                }
            }
            "s" => {
                let t = num(&row[1]);
                let (setter, name) = SETTERS[num(&row[2]) as usize];
                let args = nums(&row[3]);
                let want = num(&row[4]);
                let got = jsdate::set(t, setter, &args);
                report.check("setter", same(got, want), || {
                    format!("new Date({}).{name}({}) = {} (bun {})", show(t), args_text(&args), show(got), show(want))
                });
            }
            "u" => {
                let args = nums(&row[1]);
                let want = num(&row[2]);
                let got = jsdate::utc(&args);
                report.check("Date.UTC", same(got, want), || {
                    format!("Date.UTC({}) = {} (bun {})", args_text(&args), show(got), show(want))
                });
            }
            "l" => {
                let args = nums(&row[1]);
                let want = num(&row[2]);
                let got = jsdate::from_local_components(&args);
                report.check("new Date(components)", same(got, want), || {
                    format!("new Date({}) = {} (bun {})", args_text(&args), show(got), show(want))
                });
            }
            "c" => {
                let x = num(&row[1]);
                let want = num(&row[2]);
                let got = jsdate::time_clip(x);
                report.check("TimeClip", same(got, want), || {
                    format!("new Date({}) = {} (bun {})", show(x), show(got), show(want))
                });
            }
            "f" => {
                let t = num(&row[1]);
                let names = [
                    "toISOString",
                    "toUTCString",
                    "toDateString",
                    "toTimeString",
                    "toLocaleString",
                    "toLocaleDateString",
                    "toLocaleTimeString",
                ];
                let got = [
                    jsdate::to_iso_string(t),
                    Some(jsdate::to_utc_string(t)),
                    Some(jsdate::to_date_string(t)),
                    Some(jsdate::to_time_string(t)),
                    Some(jsdate::to_locale_string(t)),
                    Some(jsdate::to_locale_date_string(t)),
                    Some(jsdate::to_locale_time_string(t)),
                ];
                for (i, name) in names.iter().enumerate() {
                    let want = text(&row[2 + i]);
                    let kind = if *name == "toTimeString" { "toTimeString" } else { "strings" };
                    report.check(kind, got[i] == want, || {
                        format!("new Date({}).{name}() = {:?} (bun {:?})", show(t), got[i], want)
                    });
                }
            }
            "p" => {
                let s = match &row[1] {
                    Value::Str(s) => s.clone(),
                    _ => panic!("bad parse input"),
                };
                let want = num(&row[2]);
                let got = jsdate::parse(&s);
                report.check("Date.parse", same(got, want), || {
                    let escaped: String = char::decode_utf16(s.iter().copied())
                        .map(|c| match c {
                            Ok(c) if (' '..='~').contains(&c) => c.to_string(),
                            Ok(c) => format!("\\u{{{:x}}}", c as u32),
                            Err(e) => format!("\\u{{{:x}}}", e.unpaired_surrogate()),
                        })
                        .collect();
                    format!("Date.parse(\"{escaped}\") = {} (bun {})", show(got), show(want))
                });
            }
            other => panic!("unknown case kind {other}"),
        }
    }
    let total: u64 = report.checked.values().sum();
    let failed: u64 = report.failed.values().sum();
    let mut out = std::io::stdout().lock();
    for (kind, n) in &report.checked {
        writeln!(out, "{kind:>22}: {n:>8} checked, {:>6} different", report.failed.get(kind).copied().unwrap_or(0)).ok();
    }
    writeln!(
        out,
        "jsdate_check TZ={}: {total} checked, {failed} different",
        std::env::var("TZ").unwrap_or_else(|_| "(unset)".into())
    )
    .ok();
    if failed > 0 {
        std::process::exit(1);
    }
}
