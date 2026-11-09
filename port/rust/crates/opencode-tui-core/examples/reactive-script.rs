//! Runs a reactive script (harness/reactive/gen.ts describes the language) on the Rust reactive layer and prints
//! the log, as harness/reactive/solid.ts does on solid-js. usage: reactive-script <script file>...
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use opencode_tui_core::reactive::*;

#[derive(Clone)]
struct Line {
    words: Vec<String>,
    block: Option<Rc<Vec<Line>>>,
}

fn parse(text: &str) -> Vec<Line> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    fn block(lines: &[&str], i: &mut usize) -> Vec<Line> {
        let mut out = Vec::new();
        while *i < lines.len() {
            let mut words: Vec<String> = lines[*i].split_whitespace().map(String::from).collect();
            *i += 1;
            if words[0] == "end" {
                return out;
            }
            let mut line = Line { words: words.clone(), block: None };
            if words.last().map(String::as_str) == Some("{") {
                words.pop();
                line.words = words;
                line.block = Some(Rc::new(block(lines, i)));
            }
            out.push(line);
        }
        out
    }
    let mut i = 0;
    block(&lines, &mut i)
}

#[derive(Clone, Copy)]
enum Value {
    Int(Signal<i64>),
    List(Signal<Vec<i64>>),
    Memo(Memo<i64>),
}

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static VALUES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

fn log(line: String) {
    LOG.with(|l| l.borrow_mut().push(line));
}

fn lookup(name: &str) -> Value {
    VALUES.with(|v| *v.borrow().get(name).unwrap_or_else(|| panic!("unknown {name}")))
}

fn read(name: &str) -> i64 {
    match lookup(name) {
        Value::Int(s) => s.get(),
        Value::Memo(m) => m.get(),
        Value::List(_) => panic!("list read as int"),
    }
}

fn values(deps: &[String]) -> Vec<i64> {
    deps.iter().map(|d| read(d)).collect()
}

fn sum(deps: &[String]) -> i64 {
    values(deps).iter().sum()
}

fn join(values: &[i64]) -> String {
    values.iter().map(i64::to_string).collect::<Vec<_>>().join(" ")
}

fn define(name: &str, value: Value) {
    VALUES.with(|v| v.borrow_mut().insert(name.to_string(), value));
}

fn setup(lines: &[Line]) {
    for line in lines {
        let op = line.words[0].as_str();
        let name = line.words.get(1).cloned().unwrap_or_else(|| panic!("bad line {:?}", line.words));
        let rest: Vec<String> = line.words[2..].to_vec();
        match op {
            "signal" => {
                let (s, _) = create_signal(rest[0].parse::<i64>().unwrap());
                define(&name, Value::Int(s));
            }
            "list" => {
                let items: Vec<i64> = rest.iter().map(|w| w.parse().unwrap()).collect();
                let (s, _) = create_signal_with(items, None);
                define(&name, Value::List(s));
            }
            "memo" => {
                let block = line.block.clone();
                let memo_name = name.clone();
                let memo = create_memo(move || {
                    let v = sum(&rest);
                    log(format!("memo {memo_name} = {v}"));
                    if let Some(block) = &block {
                        body(&memo_name, &rest, block);
                    }
                    v
                });
                define(&name, Value::Memo(memo));
            }
            "effect" | "render" | "computed" => {
                let block = line.block.clone();
                let kind = op.to_string();
                let f = move || {
                    log(format!("{kind} {name} {}", join(&values(&rest))));
                    if let Some(block) = &block {
                        body(&name, &rest, block);
                    }
                };
                match op {
                    "effect" => create_effect(f),
                    "render" => create_render_effect(f),
                    _ => create_computed(f),
                }
            }
            "oneffect" => {
                let defer = rest[0] == "defer";
                let deps: Vec<String> = rest[1..].to_vec();
                create_effect(on(
                    move || values(&deps),
                    move |input: &Vec<i64>, prev: Option<&Vec<i64>>| {
                        log(format!("on {name} {} prev {}", join(input), prev.map_or("-".to_string(), |p| join(p))));
                    },
                    defer,
                ));
            }
            "mount" => on_mount(move || log(format!("mount {name}"))),
            "cleanup" => on_cleanup(move || log(format!("cleanup {name}"))),
            "for" => {
                let list = rest[0].clone();
                let block = line.block.clone();
                let for_name = name.clone();
                let mapper = map_array(
                    move || match lookup(&list) {
                        Value::List(s) => s.get(),
                        _ => panic!("for over a non-list"),
                    },
                    |item: &i64| *item,
                    move |item: Signal<i64>, index: Signal<usize>| {
                        let item = item.get_untracked();
                        log(format!("row {for_name} {item} {}", index.get_untracked()));
                        let n = for_name.clone();
                        on_cleanup(move || log(format!("unrow {n} {item}")));
                        let n = for_name.clone();
                        create_render_effect(move || log(format!("index {n} {item} {}", index.get())));
                        if let Some(block) = &block {
                            setup(block);
                        }
                        item
                    },
                    None,
                );
                let rows = create_memo(mapper);
                create_render_effect(move || log(format!("rows {name} {}", join(&rows.get()))));
            }
            other => panic!("unknown setup op {other}"),
        }
    }
}

fn body(name: &str, deps: &[String], lines: &[Line]) {
    let n = name.to_string();
    on_cleanup(move || log(format!("cleanup {n}")));
    let mut nested = Vec::new();
    for line in lines {
        if line.words[0] == "write" {
            let target = &line.words[1];
            let value = sum(deps) + line.words[2].parse::<i64>().unwrap();
            untrack(|| match lookup(target) {
                Value::Int(s) => s.set(value),
                _ => panic!("write to a non-signal"),
            });
        } else {
            nested.push(line.clone());
        }
    }
    setup(&nested);
}

fn act(lines: &[Line], dispose: Dispose) {
    for line in lines {
        log(format!("> {}", line.words.join(" ")));
        match line.words[0].as_str() {
            "set" => match lookup(&line.words[1]) {
                Value::Int(s) => s.set(line.words[2].parse().unwrap()),
                _ => panic!("set of a non-signal"),
            },
            "setlist" => match lookup(&line.words[1]) {
                Value::List(s) => s.set(line.words[2..].iter().map(|w| w.parse().unwrap()).collect()),
                _ => panic!("setlist of a non-list"),
            },
            "batch" => {
                let block = line.block.clone().unwrap_or_default();
                batch(|| act(&block, dispose));
            }
            "dispose" => dispose.dispose(),
            "read" => log(format!("read {} {}", line.words[1], read(&line.words[1]))),
            other => panic!("unknown action {other}"),
        }
    }
}

fn main() {
    for file in std::env::args().skip(1) {
        let text = std::fs::read_to_string(&file).unwrap();
        let program = parse(&text);
        let split = program.iter().position(|l| l.words[0] == "actions");
        let (setup_lines, actions) = match split {
            Some(i) => (program[..i].to_vec(), program[i + 1..].to_vec()),
            None => (program.clone(), Vec::new()),
        };
        let dispose = create_root(|dispose| {
            setup(&setup_lines);
            dispose
        });
        log("> ready".to_string());
        act(&actions, dispose);
        let out = LOG.with(|l| std::mem::take(&mut *l.borrow_mut()));
        let target = format!("{file}.rust");
        std::fs::write(target, out.join("\n") + "\n").unwrap();
    }
}
