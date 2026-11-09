//! The Rust side of the parser check (harness/parser/check.sh): parses every program of a JSONL corpus (one JSON
//! string per line) and prints one JSON line per program, the same text harness/parser/acorn-dump.ts prints.
//!
//! usage: cargo run --release --example parse_dump -- corpus.jsonl > rust.jsonl
//!        cargo run --release --example parse_dump -- --bench corpus.jsonl   (parse only, timed)

use opencode_codemode::parser;
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bench = args.iter().any(|arg| arg == "--bench");
    let path = args.iter().find(|arg| !arg.starts_with("--")).expect("usage: parse_dump [--bench] corpus.jsonl");
    let text = std::fs::read_to_string(path).expect("read corpus");
    let sources: Vec<Vec<u16>> =
        text.lines().filter(|line| !line.is_empty()).map(|line| decode_json_string(line).expect("corpus line is a JSON string")).collect();
    if bench {
        let units: usize = sources.iter().map(Vec::len).sum();
        let started = std::time::Instant::now();
        let accepted = sources.iter().filter(|source| parser::parse(source).is_ok()).count();
        let elapsed = started.elapsed();
        println!(
            "{} programs ({accepted} accepted), {units} code units, parsed in {:.3}s ({:.1} MB/s of UTF-16)",
            sources.len(),
            elapsed.as_secs_f64(),
            units as f64 * 2.0 / 1e6 / elapsed.as_secs_f64()
        );
        return;
    }
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for source in &sources {
        let result = parser::parse(source);
        writeln!(out, "{}", parser::estree::result_to_json(&result)).unwrap();
    }
}

/// Decodes one JSON string literal into UTF-16 code units (lone surrogate escapes kept as they are).
fn decode_json_string(text: &str) -> Option<Vec<u16>> {
    let mut chars = text.trim().chars();
    if chars.next()? != '"' {
        return None;
    }
    let mut out = Vec::new();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push(0x22),
                '\\' => out.push(0x5c),
                '/' => out.push(0x2f),
                'b' => out.push(0x08),
                'f' => out.push(0x0c),
                'n' => out.push(0x0a),
                'r' => out.push(0x0d),
                't' => out.push(0x09),
                'u' => {
                    let hex: String = (0..4).map(|_| chars.next()).collect::<Option<String>>()?;
                    out.push(u16::from_str_radix(&hex, 16).ok()?);
                }
                _ => return None,
            },
            c => {
                let mut buf = [0u16; 2];
                out.extend_from_slice(c.encode_utf16(&mut buf));
            }
        }
    }
}
