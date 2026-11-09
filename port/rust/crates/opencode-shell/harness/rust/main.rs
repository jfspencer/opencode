//! The Rust side of the differential harness (harness/driver.ts). Reads a header line (the TS process's home,
//! user and environment), then one JSON case per line, and answers each with one JSON line in the canonical form
//! the driver writes for TS.

mod json;

use std::io::{BufRead, BufWriter, Write};

use json::{Value, object, string, strings, text};
use opencode_shell::js::{self, JsString};
use opencode_shell::permission::{self, Effect, Rule};
use opencode_shell::result::{self, Capture, Info, NotificationState, Status};
use opencode_shell::scan::{self, Command, Dialect, Scan};
use opencode_shell::{Host, parse, path, select, wildcard};

struct HarnessHost {
    home: String,
    current: Option<(String, String)>,
    passwd: Vec<(String, String)>,
    env: Vec<(String, String)>,
}

impl Host for HarnessHost {
    fn home(&self) -> String {
        self.home.clone()
    }

    fn user_home(&self, user: &str) -> Option<String> {
        if let Some((name, home)) = &self.current
            && name == user
            && !home.is_empty()
        {
            return Some(home.clone());
        }
        self.passwd.iter().rev().find(|(name, _)| name == user).map(|(_, home)| home.clone())
    }

    fn env(&self, name: &str) -> Option<String> {
        self.env.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
    }
}

fn host(header: &Value) -> HarnessHost {
    let passwd = std::fs::read_to_string("/etc/passwd")
        .unwrap_or_default()
        .split('\n')
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let name = fields.first().copied().unwrap_or("");
            let home = fields.get(5).copied().unwrap_or("");
            (!name.is_empty() && !home.is_empty()).then(|| (name.to_string(), home.to_string()))
        })
        .collect();
    let current = match header.get("user") {
        Some(user @ Value::Object(_)) => Some((user.text("username"), user.text("homedir"))),
        _ => None,
    };
    let env = match header.get("env") {
        Some(Value::Object(entries)) => {
            entries.iter().map(|(key, value)| (key.clone(), js::text(&value.as_str()))).collect()
        }
        _ => Vec::new(),
    };
    HarnessHost { home: header.text("home"), current, passwd, env }
}

fn command_json(command: &Command) -> String {
    let number = |n: Option<isize>| n.map(|n| n.to_string());
    object(&[
        ("resource", Some(string(&command.resource))),
        ("words", Some(strings(&command.words))),
        ("rawWords", Some(strings(&command.raw_words))),
        (
            "wordEnds",
            command
                .word_ends
                .as_ref()
                .map(|ends| format!("[{}]", ends.iter().map(|e| e.to_string()).collect::<Vec<_>>().join(","))),
        ),
        ("statementHead", command.statement_head.then(|| "true".to_string())),
        ("declaration", command.declaration.then(|| "true".to_string())),
        ("redirectWordCount", number(command.redirect_word_count)),
        ("redirectPrefixEnd", number(command.redirect_prefix_end)),
    ])
}

fn scan_json(result: &Scan) -> String {
    match result {
        Scan::Opaque(reason) => object(&[("kind", Some(text("opaque"))), ("reason", Some(text(reason.as_str())))]),
        Scan::Scanned(commands) => object(&[
            ("kind", Some(text("scanned"))),
            ("commands", Some(format!("[{}]", commands.iter().map(command_json).collect::<Vec<_>>().join(",")))),
        ]),
    }
}

fn effect(value: &str) -> Effect {
    match value {
        "allow" => Effect::Allow,
        "deny" => Effect::Deny,
        _ => Effect::Ask,
    }
}

fn rules(values: &[Value]) -> Vec<Rule> {
    values
        .iter()
        .map(|rule| Rule { action: rule.text("action"), resource: rule.text("resource"), effect: effect(&rule.text("effect")) })
        .collect()
}

fn status(value: &str) -> Status {
    match value {
        "running" => Status::Running,
        "exited" => Status::Exited,
        "timeout" => Status::Timeout,
        _ => Status::Killed,
    }
}

fn info(value: &Value) -> Info {
    Info { status: status(&value.text("status")), exit: value.opt_num("exit").map(|n| n as i64), signal: value.opt_text("signal") }
}

fn capture(value: &Value) -> Option<Capture> {
    match value.get("capture") {
        Some(capture @ Value::Object(_)) => {
            Some(Capture { output: capture.text("output"), truncated: capture.bool("truncated").unwrap_or(false) })
        }
        _ => None,
    }
}

fn metadata_fields(m: &result::Metadata) -> Vec<(&'static str, Option<String>)> {
    vec![
        ("truncated", Some(m.truncated.to_string())),
        ("exit", m.exit.map(|e| e.to_string())),
        ("signal", m.signal.as_deref().map(text)),
        ("timeout", m.timeout.map(|t| t.to_string())),
    ]
}

fn output_json(o: &result::Output) -> String {
    object(&[
        ("output", Some(text(&o.output))),
        ("truncated", Some(o.truncated.to_string())),
        ("exit", o.exit.map(|e| e.to_string())),
        ("signal", o.signal.as_deref().map(text)),
        ("timeout", o.timeout.map(|t| t.to_string())),
    ])
}

fn notification_json(n: &result::Notification) -> String {
    let mut fields = vec![
        ("source", Some(text("shell"))),
        ("shellID", Some(text(&n.shell_id))),
        ("jobID", n.job_id.as_deref().map(text)),
        ("state", Some(text(n.state.as_str()))),
    ];
    if let Some(m) = &n.output {
        fields.extend(metadata_fields(m));
    }
    object(&[("text", Some(text(&n.text))), ("metadata", Some(object(&fields)))])
}

fn state(value: &str) -> NotificationState {
    match value {
        "completed" => NotificationState::Completed,
        "cancelled" => NotificationState::Cancelled,
        _ => NotificationState::Error,
    }
}

fn bytes(case: &Value, key: &str) -> Vec<u8> {
    case.array(key)
        .iter()
        .map(|b| match b {
            Value::Number(n) => *n as u8,
            _ => panic!("a byte"),
        })
        .collect()
}

fn answer(case: &Value, host: &HarnessHost) -> String {
    let function = case.text("fn");
    match function.as_str() {
        "scan" => {
            let dialect = match case.text("dialect").as_str() {
                "bash" => Dialect::Bash,
                "zsh" => Dialect::Zsh,
                _ => Dialect::Posix,
            };
            scan_json(&scan::scan(&case.str("input"), dialect))
        }
        "scanPowerShell" => scan_json(&scan::scan_powershell(&case.str("input"))),
        "parse" => match parse::analyze_units(&case.str("command"), &case.str("shell"), &case.str("cwd"), host) {
            Err(reason) => object(&[("error", Some(text(&parse::error_message(reason))))]),
            Ok(analysis) => {
                let commands: Vec<String> = analysis
                    .commands
                    .iter()
                    .map(|c| object(&[("resource", Some(string(&c.resource))), ("save", Some(string(&c.save)))]))
                    .collect();
                let body = object(&[
                    ("commands", Some(format!("[{}]", commands.join(",")))),
                    ("directories", Some(strings(&analysis.directories))),
                ]);
                object(&[("ok", Some(body))])
            }
        },
        "match" => wildcard::matches_units(&case.str("input"), &case.str("pattern")).to_string(),
        "escape" => string(&wildcard::escape_units(&case.str("text"))),
        "path" => {
            let args: Vec<JsString> = case.array("args").iter().map(Value::as_str).collect();
            let refs: Vec<&[u16]> = args.iter().map(|a| a.as_slice()).collect();
            let out = match case.text("op").as_str() {
                "join" => path::join(&refs),
                "normalize" => path::normalize(refs[0]),
                "dirname" => path::dirname(refs[0]),
                "basename" => path::basename(refs[0]),
                "resolve" => path::resolve(&refs, &js::units("/")),
                other => panic!("unknown path op {other}"),
            };
            string(&out)
        }
        "resolvePath" => string(&parse::resolve_path_units(&case.str("directory"), &case.str("input"), &case.str("home"))),
        "select" => {
            let file = case.text("file");
            object(&[
                ("name", Some(text(&select::name(&file)))),
                ("ps", Some(select::ps(&file).to_string())),
                ("login", Some(select::login(&file).to_string())),
                ("args", Some(format!("[{}]", select::args(&file, &case.text("command")).iter().map(|a| text(a)).collect::<Vec<_>>().join(",")))),
            ])
        }
        "evaluate" => {
            let rule = permission::evaluate(&case.text("action"), &case.text("resource"), &rules(case.array("rules")));
            object(&[
                ("action", Some(text(&rule.action))),
                ("resource", Some(text(&rule.resource))),
                ("effect", Some(text(rule.effect.as_str()))),
            ])
        }
        "output" => output_json(&result::output(&info(case.get("info").unwrap()), capture(case).as_ref())),
        "notice" => match result::notice(case.opt_num("exit").map(|n| n as i64), case.opt_text("signal").as_deref(), case.bool("timeout")) {
            Some(notice) => text(&notice),
            None => "null".into(),
        },
        "notification" => {
            let output = match case.get("output") {
                Some(o @ Value::Object(_)) => Some(result::Output {
                    output: o.text("output"),
                    truncated: o.bool("truncated").unwrap_or(false),
                    exit: o.opt_num("exit").map(|n| n as i64),
                    signal: o.opt_text("signal"),
                    timeout: o.bool("timeout"),
                }),
                _ => None,
            };
            notification_json(&result::notification(
                &case.text("shellID"),
                case.opt_text("jobID").as_deref(),
                &case.text("command"),
                state(&case.text("state")),
                &case.text("text"),
                output.as_ref(),
            ))
        }
        "userNotification" => {
            let information = case.get("info").unwrap();
            notification_json(&result::user_notification(
                &information.text("id"),
                &information.text("command"),
                &info(information),
                capture(case).as_ref(),
            ))
        }
        "utf8" => text(&String::from_utf8_lossy(&bytes(case, "bytes"))),
        "capture" => {
            let bytes = bytes(case, "tail");
            let c = result::capture(
                &bytes,
                case.num("size") as u64,
                case.num("newlines") as u64,
                case.num("maxLines") as u64,
                case.num("maxBytes") as u64,
                &case.text("file"),
            );
            object(&[("output", Some(text(&c.output))), ("truncated", Some(c.truncated.to_string()))])
        }
        "page" => {
            let bytes = bytes(case, "bytes");
            let page = result::page(
                case.num("size") as u64,
                case.opt_num("cursor").map(|n| n as u64),
                case.opt_num("limit").map(|n| n as u64),
                |start, length| bytes.iter().skip(start as usize).take(length as usize).copied().collect(),
            );
            object(&[
                ("output", Some(text(&page.output))),
                ("cursor", Some(page.cursor.to_string())),
                ("size", Some(page.size.to_string())),
                ("truncated", Some(page.truncated.to_string())),
            ])
        }
        other => panic!("unknown function {other}"),
    }
}

fn main() {
    let stdin = std::io::stdin().lock();
    let mut out = BufWriter::new(std::io::stdout().lock());
    let mut lines = stdin.lines();
    let header = json::parse(&lines.next().expect("a header line").expect("stdin is UTF-8"));
    let host = host(&header);
    for line in lines {
        let line = line.expect("stdin is UTF-8");
        let case = json::parse(&line);
        writeln!(out, "{}", answer(&case, &host)).expect("write to stdout");
    }
}
