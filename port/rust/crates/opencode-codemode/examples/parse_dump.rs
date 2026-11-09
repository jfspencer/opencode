//! The Rust side of the parser check (harness/parser/check.sh): parses every program of a JSONL corpus (one JSON
//! string per line) and prints one JSON line per program, the same text harness/parser/acorn-dump.ts prints.
//!
//! usage: cargo run --release --example parse_dump -- corpus.jsonl > rust.jsonl

use opencode_codemode::parser;
use std::io::Write;

fn main() {
    let path = std::env::args().nth(1).expect("usage: parse_dump corpus.jsonl");
    let text = std::fs::read_to_string(&path).expect("read corpus");
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for line in text.lines().filter(|line| !line.is_empty()) {
        let source = decode_json_string(line).expect("corpus line is a JSON string");
        let result = parser::parse(&source);
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
