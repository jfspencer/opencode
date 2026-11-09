//! Differential check of `opencode_codemode::numconv` against bun: reads the JSON lines that
//! harness/numconv/oracle.ts prints on stdin, recomputes each case and reports the differences.
//!
//!   bun harness/numconv/oracle.ts toFixed 1000000 1 | cargo run --release --example numconv_check
//!
//! Exit status 1 when any case differs. `NUMCONV_SHOW=<n>` sets how many differences to print (20).
//! `numconv_check bench` instead times the common cases (ns per call).

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

/// Recomputes one case: (what numconv returns, a description of the input).
fn run(case: &Case) -> (String, impl FnOnce() -> String + '_) {
    let d_num = || case.d.as_deref().map(|d| d.parse::<u32>().expect("integer d"));
    let got = match case.f.as_str() {
        "stringToNumber" | "parseFloat" | "parseInt" => {
            let s = units(case.s.as_deref().expect("s"));
            result_bits(match case.f.as_str() {
                "stringToNumber" => numconv::string_to_number(&s),
                "parseFloat" => numconv::parse_float(&s),
                _ => numconv::parse_int(&s, bits(case.d.as_deref().expect("radix"))),
            })
        }
        f => {
            let x = bits(case.x.as_deref().expect("x"));
            match f {
                "toString" => numconv::to_string(x),
                "toStringRadix" => numconv::to_string_radix(x, d_num().expect("radix")),
                "toFixed" => numconv::to_fixed(x, d_num().expect("digits")),
                "toExponential" => numconv::to_exponential(x, if case.d_is_num { d_num() } else { None }),
                "toPrecision" => numconv::to_precision(x, d_num().expect("precision")),
                "toLocaleString" => numconv::to_locale_string_en_us(x),
                "toInt32" => numconv::to_int32(x).to_string(),
                "toUint32" => numconv::to_uint32(x).to_string(),
                "indexToString" => numconv::index_to_string(x as u64),
                other => panic!("unknown function {other}"),
            }
        }
    };
    let describe = move || {
        let arg = case.d.as_deref().map(|d| if case.f == "parseInt" { format!(", radix {:?}", bits(d)) } else { format!(", {d}") });
        let arg = arg.unwrap_or_default();
        match (&case.x, &case.s) {
            (Some(x), _) => format!("{:?} ({x}){arg}", bits(x)),
            (_, Some(s)) => {
                let u = units(s);
                let hex: Vec<String> = u.iter().map(|c| format!("{c:04x}")).collect();
                format!("{:?} [{}]{arg}", String::from_utf16_lossy(&u), hex.join(" "))
            }
            _ => String::new(),
        }
    };
    (got, describe)
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("bench") {
        bench();
        return;
    }
    let show: usize = std::env::var("NUMCONV_SHOW").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    let mut totals: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut shown = 0;
    let mut bad_lines = 0u64;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in std::io::stdin().lock().lines() {
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
        let (got, describe) = run(&case);
        let entry = totals.entry(case.f.clone()).or_default();
        entry.0 += 1;
        if got != case.r {
            entry.1 += 1;
            if shown < show {
                shown += 1;
                let _ = writeln!(out, "DIFF {}({})\n  bun:  {}\n  rust: {}", case.f, describe(), case.r, got);
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

/// `numconv_check bench`: nanoseconds per call for the common cases.
fn bench() {
    fn time<T>(name: &str, iters: u32, mut f: impl FnMut(u32) -> T) {
        let start = std::time::Instant::now();
        for i in 0..iters {
            std::hint::black_box(f(std::hint::black_box(i)));
        }
        let ns = start.elapsed().as_nanos() as f64 / iters as f64;
        println!("{name:<44} {ns:>8.1} ns");
    }
    let n = 2_000_000;
    let decimals: Vec<f64> = (0..1024).map(|i| (i as f64 * 7.31 + 0.1) / 3.0).collect();
    let utf16 = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let (s123, s314, s_px, s_hex, s_long) =
        (utf16("123"), utf16("3.14159"), utf16("12.5px"), utf16("0x1F"), utf16("12345678901234567890.123e-5"));
    time("to_string(integer)", n, |i| numconv::to_string(i as f64));
    time("to_string(decimal)", n, |i| numconv::to_string(decimals[i as usize & 1023]));
    time("to_string_radix(integer, 16)", n, |i| numconv::to_string_radix(i as f64, 16));
    time("to_string_radix(decimal, 2)", n / 4, |i| numconv::to_string_radix(decimals[i as usize & 1023], 2));
    time("to_string_radix(decimal, 36)", n / 4, |i| numconv::to_string_radix(decimals[i as usize & 1023], 36));
    time("to_fixed(decimal, 2)", n, |i| numconv::to_fixed(decimals[i as usize & 1023], 2));
    time("to_fixed(decimal, 30)", n / 4, |i| numconv::to_fixed(decimals[i as usize & 1023], 30));
    time("to_precision(decimal, 4)", n, |i| numconv::to_precision(decimals[i as usize & 1023], 4));
    time("to_precision(decimal, 20)", n / 4, |i| numconv::to_precision(decimals[i as usize & 1023], 20));
    time("to_exponential(decimal, None)", n, |i| numconv::to_exponential(decimals[i as usize & 1023], None));
    time("to_exponential(decimal, 3)", n, |i| numconv::to_exponential(decimals[i as usize & 1023], Some(3)));
    time("to_locale_string_en_us(decimal)", n, |i| numconv::to_locale_string_en_us(decimals[i as usize & 1023] * 1000.0));
    time("index_to_string", n, |i| numconv::index_to_string(i as u64));
    time("string_to_number(\"123\")", n, |_| numconv::string_to_number(&s123));
    time("string_to_number(\"3.14159\")", n, |_| numconv::string_to_number(&s314));
    time("string_to_number(\"0x1F\")", n, |_| numconv::string_to_number(&s_hex));
    time("string_to_number(long decimal)", n, |_| numconv::string_to_number(&s_long));
    time("parse_float(\"12.5px\")", n, |_| numconv::parse_float(&s_px));
    time("parse_int(\"123\", NaN)", n, |_| numconv::parse_int(&s123, f64::NAN));
    time("to_int32(decimal)", n, |i| numconv::to_int32(decimals[i as usize & 1023] * 1e9));
}
