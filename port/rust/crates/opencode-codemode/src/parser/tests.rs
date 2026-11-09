//! Unit tests of the parser's own behavior; agreement with acorn is checked by harness/parser/check.sh.

use super::state::Parser;
use super::{STACK_OVERFLOW_MESSAGE, parse};

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn nested(shape: &str, depth: usize) -> String {
    match shape {
        "parens" => format!("{}1{}", "(".repeat(depth), ")".repeat(depth)),
        "arrays" => format!("{}{}", "[".repeat(depth), "]".repeat(depth)),
        "objects" => format!("x = {}1{}", "{a:".repeat(depth), "}".repeat(depth)),
        "blocks" => format!("{}{}", "{".repeat(depth), "}".repeat(depth)),
        "unary" => format!("{}1", "- ".repeat(depth)),
        "ifs" => format!("{};", "if (1) ".repeat(depth)),
        "calls" => format!("{}{}", "f(".repeat(depth), ")".repeat(depth)),
        "arrows" => format!("{}1", "x=>".repeat(depth)),
        "functions" => format!("{}{}", "function f(){".repeat(depth), "}".repeat(depth)),
        "templates" => format!("{}1{}", "`${".repeat(depth), "}`".repeat(depth)),
        "conditionals" => format!("{}c", "a?b:".repeat(depth)),
        "assignments" => format!("{}1", "a=".repeat(depth)),
        "exponents" => format!("1{}", "**1".repeat(depth)),
        "news" => format!("{}x", "new ".repeat(depth)),
        "patterns" => format!("var {}a{} = x", "[".repeat(depth), "]".repeat(depth)),
        "classes" => format!("x = {}1{}", "class extends (".repeat(depth), ") {}".repeat(depth)),
        _ => unreachable!(),
    }
}

const SHAPES: [&str; 16] = [
    "parens", "arrays", "objects", "blocks", "unary", "ifs", "calls", "arrows", "functions", "templates", "conditionals",
    "assignments", "exponents", "news", "patterns", "classes",
];

#[test]
fn parses_a_program() {
    let program = parse(&units("const a = [1, 2].map((x) => x * 2)\nreturn a")).unwrap();
    assert_eq!(program.body.len(), 2);
    let error = parse(&units("let a = ;")).unwrap_err();
    assert_eq!(error.message, "Unexpected token (1:8)");
    assert_eq!(error.pos, 8);
}

/// Deep nesting fails with an error, on a thread with 2 MiB of stack, instead of overflowing it.
#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let handle = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            for shape in SHAPES {
                let source = units(&nested(shape, 100_000));
                match parse(&source) {
                    Err(error) => assert!(error.message == STACK_OVERFLOW_MESSAGE || error.message.contains("(1:"), "{shape}: {}", error.message),
                    Ok(_) => {}
                }
                // moderate nesting parses, even in a debug build
                parse(&units(&nested(shape, 40))).unwrap_or_else(|error| panic!("{shape}: {}", error.message));
            }
            // long chains that the parser builds in a loop, and the drop of the tree they would make
            for chain in [format!("1{}", "+1".repeat(200_000)), format!("a{}", ".b".repeat(200_000)), format!("a{}", "()".repeat(200_000))] {
                assert_eq!(parse(&units(&chain)).unwrap_err().message, STACK_OVERFLOW_MESSAGE);
            }
            parse(&units(&format!("1{}", "+1".repeat(2_000)))).unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

/// Prints the stack each nesting level takes (`cargo test -- --ignored --nocapture stack_per_level`).
#[test]
#[ignore]
fn stack_per_level() {
    for shape in SHAPES {
        let depth = 50;
        let source = units(&nested(shape, depth));
        let mut parser = Parser::new(&source, usize::MAX);
        parser.parse_program().ok().expect("parses");
        let per_level = parser.max_stack_used / depth;
        println!("{shape:>14}: {per_level:>6} bytes per level, {:>6} levels in the default limit", super::DEFAULT_STACK_LIMIT / per_level.max(1));
    }
}

/// Prints the most stack any program of a corpus takes (`CORPUS=corpus.jsonl cargo test -- --ignored --nocapture
/// corpus_stack`).
#[test]
#[ignore]
fn corpus_stack() {
    let Ok(path) = std::env::var("CORPUS") else { return };
    let text = std::fs::read_to_string(path).unwrap();
    let mut worst = (0, String::new());
    let mut over = 0;
    for line in text.lines() {
        let source: Vec<u16> = json_string(line).encode_utf16().collect();
        let mut parser = Parser::new(&source, usize::MAX);
        let _ = parser.parse_program();
        if parser.max_stack_used > super::DEFAULT_STACK_LIMIT {
            over += 1;
        }
        if parser.max_stack_used > worst.0 {
            worst = (parser.max_stack_used, String::from_utf16_lossy(&source).chars().take(300).collect());
        }
    }
    println!("most stack: {} bytes, {over} programs over the default limit, for {:?}", worst.0, worst.1);
}

/// Parses `$PROGRAM` (`cargo test -- --ignored --nocapture trace_program`), for stack instrumentation.
#[test]
#[ignore]
fn trace_program() {
    let Ok(program) = std::env::var("PROGRAM") else { return };
    let source = units(&program);
    let mut parser = Parser::new(&source, usize::MAX);
    let _ = parser.parse_program();
    println!("most stack: {}", parser.max_stack_used);
}

/// Parses and drops a left-deep chain of `$CHAIN_N` operators on a thread with `$CHAIN_STACK` KiB of stack.
#[test]
#[ignore]
fn chain_drop() {
    let (Ok(n), Ok(stack)) = (std::env::var("CHAIN_N"), std::env::var("CHAIN_STACK")) else { return };
    let n: usize = n.parse().unwrap();
    let stack: usize = stack.parse().unwrap();
    let kind = std::env::var("CHAIN_KIND").unwrap_or_default();
    let source = units(&match kind.as_str() {
        "member" => format!("a{}", ".b".repeat(n)),
        "call" => format!("a{}", "()".repeat(n)),
        _ => format!("1{}", "+1".repeat(n)),
    });
    std::thread::Builder::new()
        .stack_size(stack * 1024)
        .spawn(move || {
            let result = parse(&source);
            println!("parsed: {}", result.as_ref().map(|_| "ok".to_string()).unwrap_or_else(|e| e.message.clone()));
            drop(result);
            println!("dropped");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A JSON string literal's text (enough for the corpus: escapes other than lone surrogates).
fn json_string(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.trim().trim_start_matches('"').trim_end_matches('"').chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap_or('\u{fffd}'));
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}
