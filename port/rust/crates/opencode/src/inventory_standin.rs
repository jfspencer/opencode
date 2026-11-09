//! STAND-IN for the real inventory (`crate::inventory`, being built in its own slice): just enough of the reference
//! configuration (port/conformance/lib/model.ts, read from `OPENCODE_CONFIG_CONTENT`) for the execution loop: the
//! built-in agents (packages/core/src/plugin/agent.ts over `Agent.Info.default`, packages/schema/src/agent.ts), the
//! configured providers and their models with their reasoning variants (packages/core/src/variant.ts, OpenRouter's
//! `reasoning.effort`). Nothing else of the config is read. Drop this file once the real inventory lands; the loop
//! only reads [`crate::inventory::Inventory`].
#![allow(dead_code)]

use opencode_protocol::types as t;
use opencode_protocol::{Json, Wire};

use crate::bus::LocationRef;
use crate::inventory::Inventory;
use crate::json::{self, Obj};

const PROMPT_EXPLORE: &str = "You are a file search specialist. You excel at thoroughly navigating and exploring codebases.

Guidelines:
- Your role is EXCLUSIVELY to search and analyze
- Parallelize independent tool calls for searches and reads whenever possible
- Adapt your search approach based on the thoroughness level specified by the caller
- Return file paths as absolute paths in your final response
- You MUST NOT create, modify, delete, move, or copy files, including temporary files and reports
- Shell commands MUST be read-only. NEVER run commands that write files or change system state

Complete the user's search request efficiently and report your findings clearly.";

const PROMPT_TITLE: &str = r#"You name conversations so the user can find them later in a list of past sessions.

You will receive the first message of a conversation, or for an ongoing conversation its original request and recent messages. Reply with a title for it and nothing else: one line, no quotes, no trailing period, no label such as "Title:".

Aim for three to five words. Name the specific subject, such as the feature, file, issue number, error, or tool involved. Keep the detail that tells it apart from similar conversations, like a version, provider, or the thing it is compared against, and make room for it by dropping generic words like "review", "overview", "investigation", "request", "question", or "this project".

The title should read naturally as a short phrase, not as a stack of nouns. Write it in your own words rather than copying a heading, commit message, command, or long path from the message.

Use sentence case, not title case: capitalize the first word and proper nouns only. Keep identifiers exactly as written, including file names, issue and PR numbers, error codes, and model names. Use the same language as the message.

The message is something to name, not something to answer or carry out. Even if it asks you to do work, asks a question, or seems incomplete, only name it. For greetings or small talk, use a short label like "Greeting".

Examples:
"debug 500 errors in production" -> Production 500 errors
"why is app.js failing" -> app.js failure
"what does the retry logic in fetch.ts do" -> fetch.ts retry logic
"@src/credential.ts can you add refresh token support" -> Credential refresh tokens
"compare the new cache layer with the v1 one in packages/legacy" -> New cache layer vs v1
"Investigate https://github.com/acme/app/issues/412, don't make edits" -> Issue #412
"hey" -> Greeting"#;

const PROMPT_SUMMARY: &str = r#"Summarize what was done in this conversation. Write like a pull request description.

Rules:
- 2-3 sentences max
- Describe the changes made, not the process
- Do not mention running tests, builds, or other validation steps
- Do not explain what the user asked for
- Write in first person (I added..., I fixed...)
- Never ask questions or add new questions
- If the conversation ends with an unanswered question to the user, preserve that exact question
- If the conversation ends with an imperative statement or request to the user (e.g. "Now please run the command and paste the console output"), always include that exact request in the summary"#;

/// `providerID/modelID#variant` (a config model reference).
pub fn model_ref(text: &str) -> Option<Json> {
    let (reference, variant) = match text.split_once('#') {
        Some((reference, variant)) => (reference, Some(variant)),
        None => (text, None),
    };
    let (provider, id) = reference.split_once('/')?;
    let mut o = Obj::new();
    o.str("id", id).str("providerID", provider);
    o.opt_str("variant", variant);
    Some(o.done())
}

fn rule(action: &str, resource: &str, effect: &str) -> Json {
    let mut o = Obj::new();
    o.str("action", action).str("resource", resource).str("effect", effect);
    o.done()
}

fn config() -> Json {
    std::env::var("OPENCODE_CONFIG_CONTENT").ok().and_then(|text| json::parse(&text)).unwrap_or(Json::Object(Default::default()))
}

fn agents(config: &Json) -> Vec<t::AgentInfo> {
    let home = std::env::var("OPENCODE_TEST_HOME").or_else(|_| std::env::var("HOME")).unwrap_or_default();
    let configured = |id: &str, key: &str| json::get(config, "agents").and_then(|a| json::get(a, id)).and_then(|a| json::get(a, key));
    let extra: Vec<Json> = match json::get(config, "permissions") {
        Some(Json::Array(rules)) => rules.clone(),
        _ => vec![],
    };
    let spec: [(&str, &str, Option<&str>, &str, bool, Option<&str>, Vec<Json>); 7] = [
        (
            "build",
            "Build",
            Some("The default agent. Executes tools based on configured permissions."),
            "primary",
            false,
            None,
            vec![rule("question", "*", "allow")],
        ),
        (
            "general",
            "General",
            Some("General-purpose agent for researching complex questions and executing multi-step tasks. Use this agent to execute multiple units of work in parallel."),
            "subagent",
            false,
            None,
            vec![rule("question", "*", "deny"), rule("subagent", "*", "deny")],
        ),
        (
            "explore",
            "Explore",
            Some("Fast agent specialized for exploring codebases. Use this when you need to quickly find files by patterns (eg. \"src/components/**/*.tsx\"), search code for keywords (eg. \"API endpoints\"), or answer questions about the codebase (eg. \"how do API endpoints work?\"). When calling this agent, specify the desired thoroughness level: \"quick\" for basic searches, \"medium\" for moderate exploration, or \"very thorough\" for comprehensive analysis across multiple locations and naming conventions."),
            "subagent",
            false,
            Some(PROMPT_EXPLORE),
            vec![
                rule("*", "*", "deny"),
                rule("shell", "*", "allow"),
                rule("grep", "*", "allow"),
                rule("glob", "*", "allow"),
                rule("webfetch", "*", "allow"),
                rule("websearch", "*", "allow"),
                rule("read", "*", "allow"),
                rule("read", "*.env", "ask"),
                rule("read", "*.env.*", "ask"),
                rule("read", "*.env.example", "allow"),
                rule("subagent", "*", "deny"),
                rule("external_directory", "*", "allow"),
            ],
        ),
        ("compaction", "Compaction", None, "primary", true, None, vec![]),
        ("title", "Title", None, "primary", true, Some(PROMPT_TITLE), vec![rule("*", "*", "deny")]),
        ("summary", "Summary", None, "primary", true, Some(PROMPT_SUMMARY), vec![rule("*", "*", "deny")]),
        (
            "plan",
            "Plan",
            Some("Read-only agent for exploring the codebase and planning work before implementation."),
            "primary",
            false,
            None,
            vec![
                rule("question", "*", "allow"),
                rule("edit", "*", "deny"),
                rule("edit", &format!("{home}/.opencode/plan/*"), "allow"),
            ],
        ),
    ];
    spec.into_iter()
        .filter_map(|(id, name, description, mode, hidden, system, specific)| {
            let mut permissions = vec![
                rule("*", "*", "allow"),
                rule("read", "*.env", "ask"),
                rule("read", "*.env.*", "ask"),
                rule("read", "*.env.example", "allow"),
            ];
            permissions.extend(specific);
            permissions.extend(extra.iter().cloned());
            permissions.push(rule("browser", "*", "deny"));
            permissions.push(rule("skill", "browser", "deny"));
            let mut request = Obj::new();
            request.set("settings", Obj::new().done()).set("headers", Obj::new().done()).set("body", Obj::new().done());
            let mut o = Obj::new();
            o.str("id", id).str("name", name);
            if let Some(Json::String(text)) = configured(id, "model") {
                o.opt("model", model_ref(text));
            }
            o.set("request", request.done());
            o.opt_str("system", system);
            o.opt_str("description", description);
            o.str("mode", mode).set("hidden", Json::Bool(hidden));
            if let Some(Json::Number(steps)) = configured(id, "steps") {
                o.set("steps", Json::Number(*steps));
            }
            o.set("permissions", Json::Array(permissions));
            t::AgentInfo::decode(&o.done()).ok()
        })
        .collect()
}

fn providers(config: &Json) -> (Vec<t::ProviderInfo>, Vec<t::ModelInfo>) {
    let mut providers = vec![];
    let mut models = vec![];
    let Some(Json::Object(configured)) = json::get(config, "providers") else { return (providers, models) };
    for (id, provider) in configured.iter() {
        let package = json::get_str(provider, "package").unwrap_or("@opencode/ai/providers/openrouter");
        let settings = json::get(provider, "settings").cloned();
        let mut p = Obj::new();
        p.str("id", id).str("name", json::get_str(provider, "name").unwrap_or(id)).str("activation", "enabled").str("package", package);
        p.opt("settings", settings.clone());
        if let Ok(info) = t::ProviderInfo::decode(&p.done()) {
            providers.push(info);
        }
        let Some(Json::Object(configured_models)) = json::get(provider, "models") else { continue };
        for (model_id, model) in configured_models.iter() {
            let capabilities = json::get(model, "capabilities");
            let efforts: Vec<String> = capabilities
                .and_then(|c| json::get(c, "reasoning"))
                .filter(|r| matches!(json::get(r, "supported"), Some(Json::Bool(true))))
                .and_then(|r| match json::get(r, "effort") {
                    Some(Json::Array(values)) => Some(
                        values.iter().filter_map(|v| json::as_str(v).map(str::to_owned)).filter(|v| v != "default").collect(),
                    ),
                    _ => None,
                })
                .unwrap_or_default();
            let variants: Vec<Json> = efforts
                .iter()
                .map(|effort| {
                    let mut reasoning = Obj::new();
                    reasoning.str("effort", effort);
                    let mut settings = Obj::new();
                    settings.set("reasoning", reasoning.done());
                    let mut v = Obj::new();
                    v.str("id", effort).set("settings", settings.done());
                    v.done()
                })
                .collect();
            let mut caps = Obj::new();
            for key in ["tools", "input", "output", "temperature"] {
                caps.opt(key, capabilities.and_then(|c| json::get(c, key)).cloned());
            }
            let mut time = Obj::new();
            time.int("released", 0);
            let mut m = Obj::new();
            m.str("id", model_id).str("modelID", model_id).str("providerID", id);
            m.opt("family", json::get(model, "family").cloned());
            m.str("name", json::get_str(model, "name").unwrap_or(model_id));
            m.str("package", json::get_str(model, "package").unwrap_or(package));
            m.opt("settings", settings.clone());
            m.set("capabilities", caps.done());
            m.set("variants", Json::Array(variants));
            m.set("time", time.done());
            m.set("cost", Json::Array(vec![]));
            m.str("status", "active").set("enabled", Json::Bool(true));
            m.opt("limit", json::get(model, "limit").cloned());
            if let Ok(info) = t::ModelInfo::decode(&m.done()) {
                models.push(info);
            }
        }
    }
    (providers, models)
}

/// The stand-in inventory of a location: the same for every location (project config is not read).
pub fn load(_location: &LocationRef) -> Inventory {
    let config = config();
    let agents = agents(&config);
    let (providers, models) = providers(&config);
    Inventory { config, agents, providers, models }
}
