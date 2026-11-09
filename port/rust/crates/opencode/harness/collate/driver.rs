//! The Rust side of the collation harness (run.sh): `src/collate.rs` compiled on its own (it needs only std).
//! Strings travel as lines of hex code points separated by spaces (an empty line is the empty string).
//!   driver elements   each line's collation elements, `primary:secondary:tertiary` in hex
//!   driver compare    each line `a<TAB>b`: -1, 0 or 1 for `compare(a, b)`
//!   driver sort       the lines, stably sorted with `compare`
#[path = "../../src/collate.rs"]
#[allow(dead_code)]
mod collate;

use std::io::{BufRead, BufWriter, Write};

fn decode(field: &str) -> String {
    field
        .split_whitespace()
        .map(|hex| char::from_u32(u32::from_str_radix(hex, 16).expect("hex")).expect("a scalar value"))
        .collect()
}

fn main() {
    let mode = std::env::args().nth(1).expect("usage: driver elements|compare|sort");
    let stdin = std::io::stdin();
    let lines = stdin.lock().lines().map(|line| line.expect("stdin"));
    let mut out = BufWriter::new(std::io::stdout().lock());
    match mode.as_str() {
        "elements" => {
            for line in lines {
                let items: Vec<String> = collate::debug_elements(&decode(&line))
                    .iter()
                    .map(|(p, lower)| format!("{p:x}:{:x}:{:x}", lower >> 16, lower & 0xffff))
                    .collect();
                writeln!(out, "{}", items.join(" ")).unwrap();
            }
        }
        "compare" => {
            for line in lines {
                let (a, b) = line.split_once('\t').expect("a<TAB>b");
                let order = collate::compare(&decode(a), &decode(b)) as i8;
                writeln!(out, "{order}").unwrap();
            }
        }
        "sort" => {
            let mut items: Vec<(String, String)> = lines.map(|line| (decode(&line), line)).collect();
            items.sort_by(|a, b| collate::compare(&a.0, &b.0));
            for (_, line) in items {
                writeln!(out, "{line}").unwrap();
            }
        }
        other => panic!("unknown mode {other}"),
    }
}
