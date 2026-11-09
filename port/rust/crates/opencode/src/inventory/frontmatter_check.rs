//! The differential check of frontmatter against the reference (harness/frontmatter/run.sh): for each recorded
//! markdown file, gray-matter's data as a JS value graph and what the agent, command and skill plugins decode from
//! it must be what TS recorded.

use opencode_protocol::Json;

use super::{agents, commands, jsvalue, markdown, skills};
use crate::json;

fn of(text: &str) -> Json {
    Json::String(text.to_owned())
}

fn record(content: &str) -> Json {
    let mut out = json::Obj::new();
    out.str("content", content);
    out.set(
        "matter",
        match markdown::parse(content) {
            Ok(parsed) => {
                let mut o = json::Obj::new();
                o.set("data", jsvalue::describe(&parsed.data.root)).str("content", &parsed.content);
                o.done()
            }
            Err(_) => {
                let mut o = json::Obj::new();
                o.set("throws", Json::Bool(true));
                o.done()
            }
        },
    );
    out.set(
        "agent",
        match agents::markdown_agent("/d", "/d/agents/a.md", false, content) {
            Err(_) => of("defect"),
            Ok(None) => of("skip"),
            Ok(Some(info)) => json::get(&crate::json::of(&info), "agents").and_then(|a| json::get(a, "a")).cloned().unwrap_or_default(),
        },
    );
    out.set("command", commands::markdown_command(content).map(|c| crate::json::of(&c)).unwrap_or_else(|| of("skip")));
    out.set(
        "skill",
        match skills::parse("/d", "/d/skills/a/SKILL.md", content) {
            None => of("skip"),
            Some(skill) => {
                let skill = crate::json::of(&skill);
                let mut o = json::Obj::new();
                for key in ["id", "name", "description", "autoinvoke"] {
                    if let Some(value) = json::get(&skill, key) {
                        o.set(key, value.clone());
                    }
                }
                o.done()
            }
        },
    );
    out.done()
}

/// `FRONTMATTER_CASES=<cases.json>` (harness/frontmatter/cases.ts).
#[test]
#[ignore]
fn frontmatter_matches_the_reference() {
    let path = std::env::var("FRONTMATTER_CASES").expect("FRONTMATTER_CASES names the recorded cases");
    let show: usize = std::env::var("FRONTMATTER_SHOW").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
    // The documents nest as deep as TS loads them: the check runs on a thread with room for their JSON.
    let failures = std::thread::Builder::new()
        .stack_size(512 << 20)
        .spawn(move || {
            let cases = json::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
            let cases = json::array(&cases).unwrap();
            let mut failures = 0;
            for (index, case) in cases.iter().enumerate() {
                let content = json::get_str(case, "content").unwrap();
                let actual = record(content);
                for part in ["matter", "agent", "command", "skill"] {
                    let (want, got) = (json::get(case, part).unwrap(), json::get(&actual, part).unwrap());
                    if json::text(want) != json::text(got) {
                        failures += 1;
                        if failures <= show {
                            let clip = |t: String| if t.len() > 2000 { format!("{}…", &t[..t.floor_char_boundary(2000)]) } else { t };
                            eprintln!(
                                "case {index} {part}: {}\n  reference {}\n  port      {}",
                                clip(format!("{content:?}")),
                                clip(json::text(want)),
                                clip(json::text(got))
                            );
                        }
                    }
                }
            }
            eprintln!("{} cases, {failures} differences", cases.len());
            failures
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(failures, 0);
}
