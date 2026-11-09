//! Differential check of `opencode_codemode::numconv` against bun: reads the JSON lines that
//! harness/numconv/oracle.ts prints on stdin, recomputes each case and reports the differences.
//!
//!   bun harness/numconv/oracle.ts toFixed 1000000 1 | cargo run --release --example numconv_check
//!
//! Exit status 1 when any case differs. `NUMCONV_SHOW=<n>` sets how many differences to print (20).

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use opencode_codemode::numconv;

#[derive(Default)]
struct Case {
    f: String,
    x: Option<String>,
    s: Option<String>,
    d: Option<String>,
    d_is_num: bool,
    r: String,
}

/// Parses the flat objects the oracle prints: string, integer and null values only.
fn parse_line(line: &str) -> Option<Case> {
    let b = line.as_bytes();
    let mut i = 0;
    let mut case = Case::default();
    let skip_ws = |i: &mut usize| {
        while *i < b.len() && (b[*i] == b' ' || b[*i] == b'\t') {
            *i += 1;
        }
    };
    // A JSON string, collected as UTF-16 so that `\u` escapes of surrogate pairs combine.
    let parse_string = |i: &mut usize| -> Option<String> {
        if b[*i] != b'"' {
            return None;
        }
        *i += 1;
        let mut units: Vec<u16> = Vec::new();
        loop {
            match *b.get(*i)? {
                b'"' => {
                    *i += 1;
                    return Some(String::from_utf16_lossy(&units));
                }
                b'\\' => {
                    let e = *b.get(*i + 1)?;
                    *i += 2;
                    let unit = match e {
                        b'u' => {
                            let h = std::str::from_utf8(b.get(*i..*i + 4)?).ok()?;
                            *i += 4;
                            u16::from_str_radix(h, 16).ok()?
                        }
                        b'n' => 0x0a,
                        b't' => 0x09,
                        b'r' => 0x0d,
                        b'b' => 0x08,
                        b'f' => 0x0c,
                        other => other as u16,
                    };
                    units.push(unit);
                }
                _ => {
                    let ch = std::str::from_utf8(&b[*i..]).ok()?.chars().next()?;
                    let mut buf = [0u16; 2];
                    units.extend_from_slice(ch.encode_utf16(&mut buf));
                    *i += ch.len_utf8();
                }
            }
        }
    };
    skip_ws(&mut i);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        skip_ws(&mut i);
        if b.get(i) == Some(&b'}') {
            break;
        }
        let key = parse_string(&mut i)?;
        skip_ws(&mut i);
        if b.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        skip_ws(&mut i);
        let (value, is_num) = match b.get(i)? {
            b'"' => (Some(parse_string(&mut i)?), false),
            b'n' => {
                i += 4;
                (None, false)
            }
            _ => {
                let start = i;
                while i < b.len() && (b[i] == b'-' || b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                (Some(line[start..i].to_string()), true)
            }
        };
        match key.as_str() {
            "f" => case.f = value?,
            "x" => case.x = value,
            "s" => case.s = value,
            "d" => {
                case.d = value;
                case.d_is_num = is_num;
            }
            "r" => case.r = value?,
            _ => {}
        }
        skip_ws(&mut i);
        match b.get(i) {
            Some(b',') => i += 1,
            Some(b'}') => break,
            _ => return None,
        }
    }
    Some(case)
}

fn bits(h: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(h, 16).expect("hex double"))
}

fn units(h: &str) -> Vec<u16> {
    (0..h.len() / 4).map(|k| u16::from_str_radix(&h[4 * k..4 * k + 4], 16).expect("hex unit")).collect()
}

fn result_bits(x: f64) -> String {
    if x.is_nan() { "NaN".to_string() } else { format!("{:016x}", x.to_bits()) }
}

fn describe_double(x: f64) -> String {
    format!("{:?} ({:016x})", x, x.to_bits())
}

fn describe_units(u: &[u16]) -> String {
    format!("{:?} [{}]", String::from_utf16_lossy(u), u.iter().map(|c| format!("{c:04x}")).collect::<Vec<_>>().join(" "))
}

fn main() {
    let show: usize = std::env::var("NUMCONV_SHOW").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    let stdin = std::io::stdin();
    let mut totals: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut shown = 0;
    let mut bad_lines = 0u64;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line.expect("read stdin");
        if line.is_empty() {
            continue;
        }
        let Some(case) = parse_line(&line) else {
            bad_lines += 1;
            if bad_lines <= 5 {
                eprintln!("unparsable line: {}", &line[..line.len().min(200)]);
            }
            continue;
        };
        let d_num = || case.d.as_deref().map(|d| d.parse::<u32>().expect("integer d"));
        let (got, input) = match case.f.as_str() {
            "toString" | "toStringRadix" | "toFixed" | "toExponential" | "toPrecision" | "toLocaleString" | "toInt32"
            | "toUint32" | "indexToString" => {
                let x = bits(case.x.as_deref().expect("x"));
                let got = match case.f.as_str() {
                    "toString" => numconv::to_string(x),
                    "toStringRadix" => numconv::to_string_radix(x, d_num().expect("radix")),
                    "toFixed" => numconv::to_fixed(x, d_num().expect("digits")),
                    "toExponential" => numconv::to_exponential(x, if case.d_is_num { d_num() } else { None }),
                    "toPrecision" => numconv::to_precision(x, d_num().expect("precision")),
                    "toLocaleString" => numconv::to_locale_string_en_us(x),
                    "toInt32" => numconv::to_int32(x).to_string(),
                    "toUint32" => numconv::to_uint32(x).to_string(),
                    _ => numconv::index_to_string(x as u64),
                };
                let d = case.d.as_deref().map(|d| format!(", {d}")).unwrap_or_default();
                (got, format!("{}{d}", describe_double(x)))
            }
            "stringToNumber" | "parseFloat" | "parseInt" => {
                let s = units(case.s.as_deref().expect("s"));
                let got = match case.f.as_str() {
                    "stringToNumber" => numconv::string_to_number(&s),
                    "parseFloat" => numconv::parse_float(&s),
                    _ => numconv::parse_int(&s, bits(case.d.as_deref().expect("radix"))),
                };
                let d = case.d.as_deref().map(|d| format!(", radix {}", describe_double(bits(d)))).unwrap_or_default();
                (result_bits(got), format!("{}{d}", describe_units(&s)))
            }
            other => panic!("unknown function {other}"),
        };
        let entry = totals.entry(case.f.clone()).or_default();
        entry.0 += 1;
        if got != case.r {
            entry.1 += 1;
            if shown < show {
                shown += 1;
                let _ = writeln!(out, "DIFF {}({input})\n  bun:  {}\n  rust: {}", case.f, case.r, got);
            }
        }
    }
    let mut any = bad_lines > 0;
    for (f, (n, bad)) in &totals {
        let _ = writeln!(out, "{f}: {n} cases, {bad} differences");
        any |= *bad > 0;
    }
    if bad_lines > 0 {
        let _ = writeln!(out, "{bad_lines} unparsable lines");
    }
    if totals.is_empty() {
        let _ = writeln!(out, "no cases read");
        any = true;
    }
    std::process::exit(if any { 1 } else { 0 });
}
