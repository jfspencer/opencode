//! Round-trip harness for the generated wire types, driven by port/rust/gen/src/roundtrip.ts.
//!
//! Reads lines `<root type>\t<JSON text>` from stdin and answers each with `ok\t<JSON written back>` or
//! `err\t<reason>`. `--roots` lists the root type names instead.

use opencode_protocol::{roots, wire};
use std::io::{BufRead, BufWriter, Write};

fn main() {
    if std::env::args().any(|a| a == "--roots") {
        for name in roots::ROOTS {
            println!("{name}");
        }
        return;
    }
    let stdin = std::io::stdin().lock();
    let mut out = BufWriter::new(std::io::stdout().lock());
    for line in stdin.lines() {
        let line = line.expect("stdin is UTF-8");
        let (name, json) = line.split_once('\t').expect("a tab after the type name");
        let answer = match wire::parse(json) {
            Err(e) => Err(e.to_string()),
            Ok(v) => match roots::roundtrip(name, &v) {
                None => Err(format!("unknown root type {name}")),
                Some(r) => r.map_err(|e| e.to_string()),
            },
        };
        match answer {
            Ok(text) => writeln!(out, "ok\t{text}"),
            Err(e) => writeln!(out, "err\t{}", e.replace(['\n', '\t'], " ")),
        }
        .expect("write to stdout");
    }
}
