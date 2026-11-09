//! The Rust side of the RegExp differential check (see harness/jsregex/oracle.ts): reads the oracle's cases on
//! stdin, recomputes every result with `opencode_codemode::jsregex`, and reports the differences.
//!
//! usage: bun harness/jsregex/oracle.ts | cargo run --release --offline --example jsregex_check [-- max-reports]

use opencode_codemode::jsregex::{self, JsRegExp, Match, RegExpError};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};

fn enc(units: &[u16]) -> String {
    let mut out = String::with_capacity(units.len());
    for &u in units {
        match u {
            0x22 => out.push_str("\\\""),
            0x5C => out.push_str("\\\\"),
            0x21..=0x7E => out.push(u as u8 as char),
            _ => out.push_str(&format!("\\u{u:04x}")),
        }
    }
    out
}

fn dec(text: &str) -> Vec<u16> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'u' => {
                    out.push(u16::from_str_radix(&text[i + 2..i + 6], 16).expect("hex escape"));
                    i += 6;
                }
                other => {
                    out.push(other as u16);
                    i += 2;
                }
            }
        } else {
            out.push(bytes[i] as u16);
            i += 1;
        }
    }
    out
}

fn s(units: Option<&[u16]>) -> String {
    match units {
        None => "null".to_string(),
        Some(u) => format!("\"{}\"", enc(u)),
    }
}

fn num(n: f64) -> String {
    if n == 0.0 && n.is_sign_negative() {
        return "-0".to_string();
    }
    if n.fract() == 0.0 && n.abs() < 1e21 {
        return format!("{}", n as i64);
    }
    format!("{n}")
}

fn match_of(m: Option<&Match>, text: &[u16]) -> String {
    let Some(m) = m else { return "null".to_string() };
    let caps: Vec<String> = m.captures.iter().map(|c| s(c.map(|(a, b)| &text[a..b]))).collect();
    let groups = match &m.groups {
        None => "null".to_string(),
        Some(groups) => {
            let entries: Vec<String> = groups
                .iter()
                .map(|(name, range)| format!("{}:{}", s(Some(name)), s(range.map(|(a, b)| &text[a..b]))))
                .collect();
            format!("{{{}}}", entries.join(","))
        }
    };
    format!("[{},[{}],{}]", m.index(), caps.join(","), groups)
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

const REPLACEMENTS: [&str; 6] = ["[$&]", "$1|$2", "$<n>$<m>", "$$-$`-$'", "$0$00$01$10$11$99$", "x"];
const LIMITS: [Option<u32>; 5] = [None, Some(0), Some(1), Some(2), Some(3)];

fn operations(pattern: &[u16], flags: &[u16], text: &[u16], full: bool) -> String {
    let fresh = || JsRegExp::new(pattern, flags).expect("valid regex");
    let mut results: Vec<String> = Vec::new();
    let len = text.len() as f64;
    let last_indexes: Vec<f64> = if full { vec![0.0, 1.0, 2.0, len, len + 1.0, -1.0, 2.5] } else { vec![0.0, 1.0, len] };
    for at in last_indexes {
        let re = fresh();
        re.set_last_index(at);
        let m = re.exec(text);
        results.push(format!("{}@{}", match_of(m.as_ref(), text), num(re.last_index())));
    }
    {
        let re = fresh();
        for _ in 0..3 {
            let m = re.exec(text);
            results.push(format!("{}@{}", match_of(m.as_ref(), text), num(re.last_index())));
        }
    }
    let replacements = if full { &REPLACEMENTS[..] } else { &REPLACEMENTS[..2] };
    for r in replacements {
        let re = fresh();
        re.set_last_index(1.0);
        let out = jsregex::replace(&re, text, &units(r));
        results.push(format!("{}@{}", s(Some(&out)), num(re.last_index())));
    }
    {
        let re = fresh();
        let calls: Vec<String> = jsregex::replace_matches(&re, text).iter().map(|m| match_of(Some(m), text)).collect();
        results.push(format!("[{}]@{}", calls.join(","), num(re.last_index())));
    }
    let limits = if full { &LIMITS[..] } else { &LIMITS[..2] };
    for limit in limits {
        let re = fresh();
        re.set_last_index(1.0);
        let parts: Vec<String> = jsregex::split(&re, text, *limit).iter().map(|p| s(p.as_deref())).collect();
        results.push(format!("[{}]@{}", parts.join(","), num(re.last_index())));
    }
    let mut global_flags = flags.to_vec();
    if !global_flags.contains(&(b'g' as u16)) {
        global_flags.push(b'g' as u16);
    }
    {
        let re = JsRegExp::new(pattern, &global_flags).expect("valid regex");
        re.set_last_index(1.0);
        let m = match jsregex::match_global(&re, text) {
            None => "null".to_string(),
            Some(list) => format!("[{}]", list.iter().map(|m| s(Some(m))).collect::<Vec<_>>().join(",")),
        };
        results.push(format!("{}@{}", m, num(re.last_index())));
    }
    {
        let re = JsRegExp::new(pattern, &global_flags).expect("valid regex");
        re.set_last_index(1.0);
        let all: Vec<String> = jsregex::match_all(&re, text).iter().map(|m| match_of(Some(m), text)).collect();
        results.push(format!("[{}]@{}", all.join(","), num(re.last_index())));
    }
    for at in [3.0, -0.0] {
        let re = fresh();
        re.set_last_index(at);
        let index = jsregex::search(&re, text);
        results.push(format!("{}@{}", index, num(re.last_index())));
    }
    results.join(" ")
}

fn constructor(pattern: &[u16], flags: &[u16]) -> String {
    match JsRegExp::new(pattern, flags) {
        Ok(re) => format!("OK:{}:{}", enc(&re.source()), re.flags()),
        Err(RegExpError::Flags(m)) => format!("ERR:F:{}", enc(&units(&m))),
        Err(RegExpError::Pattern(m)) => format!("ERR:P:{}", enc(&units(&m))),
    }
}

fn main() {
    // `--try <pattern> <flags> [subject]`: the constructor result (and the operations on a subject), for debugging.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--try") {
        let (pattern, flags) = (units(&args[2]), units(args.get(3).map_or("", String::as_str)));
        println!("{}", constructor(&pattern, &flags));
        if let Some(subject) = args.get(4) {
            println!("{}", operations(&pattern, &flags, &units(subject), true).replace(' ', "\n"));
        }
        return;
    }
    let max_reports: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(40);
    let stdin = std::io::stdin();
    let mut cases = 0u64;
    let mut differences = 0u64;
    let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
    let mut reports = 0usize;
    let mut stderr = std::io::stderr();
    let mut current: Option<(Vec<u16>, Vec<u16>, bool)> = None;
    let mut report = |kind: String, context: String, expected: &str, actual: &str, differences: &mut u64| {
        *differences += 1;
        *by_kind.entry(kind.clone()).or_insert(0) += 1;
        if reports < max_reports {
            reports += 1;
            let readable = |t: &str| t.replace("\\u0020", " ");
            let _ = writeln!(
                stderr,
                "DIFF {kind}: {}\n  expected: {}\n  actual:   {}",
                readable(&context),
                readable(expected),
                readable(actual)
            );
        }
    };
    for line in stdin.lock().lines() {
        let line = line.expect("stdin");
        let fields: Vec<&str> = line.split('\t').collect();
        match fields[0] {
            "R" => {
                let pattern = dec(fields[1]);
                let flags = dec(fields[2]);
                let expected = fields[3];
                let ok = expected.starts_with("OK:");
                if !ok {
                    cases += 1;
                }
                let actual = std::panic::catch_unwind(|| constructor(&pattern, &flags))
                    .unwrap_or_else(|_| "PANIC".to_string());
                let valid = actual.starts_with("OK:");
                if actual != expected {
                    let context = format!("/{}/{}", fields[1], fields[2]);
                    report("constructor".to_string(), context, expected, &actual, &mut differences);
                }
                current = Some((pattern, flags, ok && valid));
            }
            "S" => {
                cases += 1;
                let Some((pattern, flags, valid)) = &current else { continue };
                if !*valid {
                    continue;
                }
                let text = dec(fields[1]);
                let expected = fields[2];
                let full = expected.split(' ').count() > 20;
                if std::env::var_os("JSREGEX_TRACE").is_some() {
                    eprintln!("case /{}/{} on \"{}\"", enc(pattern), enc(flags), fields[1]);
                }
                let started = std::time::Instant::now();
                let actual = std::panic::catch_unwind(|| operations(pattern, flags, &text, full))
                    .unwrap_or_else(|_| "PANIC".to_string());
                let elapsed = started.elapsed();
                if elapsed.as_millis() > 200 {
                    eprintln!("SLOW {elapsed:?}: /{}/{} on \"{}\"", enc(pattern), enc(flags), fields[1]);
                }
                if actual != expected {
                    let e: Vec<&str> = expected.split(' ').collect();
                    let a: Vec<&str> = actual.split(' ').collect();
                    let first = (0..e.len().max(a.len())).find(|&k| e.get(k) != a.get(k)).unwrap_or(0);
                    let context = format!("/{}/{} on \"{}\" (operation {first})", enc(pattern), enc(flags), fields[1]);
                    let kind = if actual == "PANIC" { "panic".to_string() } else { format!("op{first}") };
                    report(kind, context, e.get(first).unwrap_or(&""), a.get(first).unwrap_or(&""), &mut differences);
                }
            }
            "X" => {
                cases += 1;
                let input = dec(fields[1]);
                let actual = s(Some(&jsregex::escape(&input)));
                if actual != fields[2] {
                    report("escape".to_string(), fields[1].to_string(), fields[2], &actual, &mut differences);
                }
            }
            "T" => {
                cases += 1;
                let (t, search, r) = (dec(fields[1]), dec(fields[2]), dec(fields[3]));
                let positions: Vec<String> =
                    jsregex::string_replace_positions(&t, &search, true).iter().map(|p| p.to_string()).collect();
                let actual = format!(
                    "{}\t{}\t[{}]",
                    s(Some(&jsregex::string_replace(&t, &search, &r, false))),
                    s(Some(&jsregex::string_replace(&t, &search, &r, true))),
                    positions.join(",")
                );
                let expected = format!("{}\t{}\t{}", fields[4], fields[5], fields[6]);
                if actual != expected {
                    let context = format!("{:?} {:?} {:?}", fields[1], fields[2], fields[3]);
                    report("string-replace".to_string(), context, &expected, &actual, &mut differences);
                }
            }
            _ => {}
        }
    }
    println!("jsregex_check: {cases} cases, {differences} differences");
    for (kind, count) in &by_kind {
        println!("  {kind}: {count}");
    }
    if differences > 0 {
        std::process::exit(1);
    }
}
