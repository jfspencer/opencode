//! Agents (packages/core/src/agent.ts and the plugins that fill it, in activation order): the built-ins of
//! plugin/agent.ts (`build`, `general`, `explore`, `compaction`, `title`, `summary`), `plan` (plugin/plan.ts), then
//! config (config/plugin/agent.ts: config `agents`, markdown agent files of config directories, `default_agent`, config
//! `permissions`), then the browser plugin's denies (packages/plugin-browser/src/connection.ts), appended to every
//! agent.
//!
//! An agent's `permissions` are its ruleset in evaluation order (the last matching rule wins): the defaults of
//! `Agent.Info.default`, the built-in's own rules, the config `permissions` of every document (paths under `~` and
//! `$HOME` expanded for path actions), the agent's own config rules, and the browser denies.
//!
//! TODO(port): legacy (v1) markdown agent files (frontmatter with only v1 fields) are skipped; TS migrates them.

use opencode_protocol::types as t;
use opencode_protocol::wire::Object;
use opencode_protocol::{Json, Wire};

use super::config::{self, Entry};
use super::{markdown, scan};
use crate::json::Obj;

fn str(text: &str) -> Json {
    Json::String(text.to_owned())
}

const PROMPT_EXPLORE: &str = include_str!("builtin/explore.txt");
const PROMPT_TITLE: &str = include_str!("builtin/title.txt");
const PROMPT_SUMMARY: &str = include_str!("builtin/summary.txt");

/// `Agent.defaultID`.
pub const DEFAULT_ID: &str = "build";

/// The agent registry after every plugin's transform, in registration order, with the selected default.
#[derive(Debug, Clone, Default)]
pub struct Agents {
    /// In registration order (`Agent.list` puts the default first).
    pub agents: Vec<Json>,
    /// `default_agent` as configured (it may name no selectable agent).
    pub configured_default: Option<String>,
}

fn rule(action: &str, resource: &str, effect: &str) -> Json {
    let mut o = Obj::new();
    o.str("action", action).str("resource", resource).str("effect", effect);
    o.done()
}

fn get_mut<'a>(o: &'a mut Object, key: &str) -> &'a mut Json {
    o.get_mut(key).expect("agent field")
}

impl Agents {
    fn index(&self, id: &str) -> Option<usize> {
        self.agents.iter().position(|a| crate::json::get_str(a, "id") == Some(id))
    }

    /// `Agent.Info.default(id)`.
    fn default_info(id: &str) -> Json {
        let mut request = Obj::new();
        request.set("settings", Json::Object(Object::new()));
        request.set("headers", Json::Object(Object::new()));
        request.set("body", Json::Object(Object::new()));
        let mut o = Obj::new();
        o.str("id", id).str("name", id).set("request", request.done()).str("mode", "primary").set("hidden", Json::Bool(false));
        o.set(
            "permissions",
            Json::Array(vec![
                rule("*", "*", "allow"),
                rule("read", "*.env", "ask"),
                rule("read", "*.env.*", "ask"),
                rule("read", "*.env.example", "allow"),
            ]),
        );
        o.done()
    }

    /// `editor.update`: the agent, created from the defaults when it does not exist yet.
    fn update(&mut self, id: &str, f: impl FnOnce(&mut Object)) {
        let index = match self.index(id) {
            Some(i) => i,
            None => {
                self.agents.push(Self::default_info(id));
                self.agents.len() - 1
            }
        };
        let Json::Object(agent) = &mut self.agents[index] else { unreachable!("agents are objects") };
        f(agent);
        agent.insert("id".into(), Json::String(id.to_owned()));
    }

    fn remove(&mut self, id: &str) {
        if let Some(i) = self.index(id) {
            self.agents.remove(i);
        }
    }

    fn push_permissions(agent: &mut Object, rules: &[Json]) {
        if let Json::Array(list) = get_mut(agent, "permissions") {
            list.extend(rules.iter().cloned());
        }
    }

    /// `Agent.select` without an agent: the configured default if selectable, else `build`, else the first selectable.
    pub fn selected(&self) -> Option<&Json> {
        let selectable = |agent: &&Json| {
            crate::json::get_str(agent, "mode") != Some("subagent") && !matches!(crate::json::get(agent, "hidden"), Some(Json::Bool(true)))
        };
        let by_id = |id: &str| self.agents.iter().find(|a| crate::json::get_str(a, "id") == Some(id));
        self.configured_default
            .as_deref()
            .and_then(by_id)
            .filter(selectable)
            .or_else(|| by_id(DEFAULT_ID).filter(selectable))
            .or_else(|| self.agents.iter().find(selectable))
    }

    /// `Agent.list`: the selected default first, then the rest in registration order.
    pub fn list(&self) -> Vec<t::AgentInfo> {
        let selected = self.selected().and_then(|a| crate::json::get_str(a, "id")).map(str::to_owned);
        let mut out: Vec<&Json> = vec![];
        if let Some(id) = &selected {
            out.extend(self.agents.iter().filter(|a| crate::json::get_str(a, "id") == Some(id)));
        }
        out.extend(self.agents.iter().filter(|a| crate::json::get_str(a, "id") != selected.as_deref()));
        out.into_iter().map(|a| t::AgentInfo::decode(a).expect("agents are valid Agent.Info")).collect()
    }
}

/// Builds the registry from the location's config entries.
pub fn build(entries: &[Entry], home: &str, plan_directory: &str) -> Agents {
    let mut agents = Agents::default();
    // plugin/agent.ts
    agents.update("build", |a| {
        a.insert("name".into(), str("Build"));
        a.insert("description".into(), str("The default agent. Executes tools based on configured permissions."));
        a.insert("mode".into(), str("primary"));
        Agents::push_permissions(a, &[rule("question", "*", "allow")]);
    });
    agents.update("general", |a| {
        a.insert("name".into(), str("General"));
        a.insert(
            "description".into(),
            str("General-purpose agent for researching complex questions and executing multi-step tasks. Use this agent to execute multiple units of work in parallel."),
        );
        a.insert("mode".into(), str("subagent"));
        Agents::push_permissions(a, &[rule("question", "*", "deny"), rule("subagent", "*", "deny")]);
    });
    agents.update("explore", |a| {
        a.insert("name".into(), str("Explore"));
        a.insert(
            "description".into(),
            str("Fast agent specialized for exploring codebases. Use this when you need to quickly find files by patterns (eg. \"src/components/**/*.tsx\"), search code for keywords (eg. \"API endpoints\"), or answer questions about the codebase (eg. \"how do API endpoints work?\"). When calling this agent, specify the desired thoroughness level: \"quick\" for basic searches, \"medium\" for moderate exploration, or \"very thorough\" for comprehensive analysis across multiple locations and naming conventions."),
        );
        a.insert("system".into(), str(PROMPT_EXPLORE));
        a.insert("mode".into(), str("subagent"));
        Agents::push_permissions(
            a,
            &[
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
        );
    });
    agents.update("compaction", |a| {
        a.insert("name".into(), str("Compaction"));
        a.insert("mode".into(), str("primary"));
        a.insert("hidden".into(), Json::Bool(true));
    });
    agents.update("title", |a| {
        a.insert("name".into(), str("Title"));
        a.insert("mode".into(), str("primary"));
        a.insert("hidden".into(), Json::Bool(true));
        a.insert("system".into(), str(PROMPT_TITLE));
        Agents::push_permissions(a, &[rule("*", "*", "deny")]);
    });
    agents.update("summary", |a| {
        a.insert("name".into(), str("Summary"));
        a.insert("mode".into(), str("primary"));
        a.insert("hidden".into(), Json::Bool(true));
        a.insert("system".into(), str(PROMPT_SUMMARY));
        Agents::push_permissions(a, &[rule("*", "*", "deny")]);
    });
    // plugin/plan.ts
    agents.update("plan", |a| {
        a.insert("name".into(), str("Plan"));
        a.insert("description".into(), str("Read-only agent for exploring the codebase and planning work before implementation."));
        a.insert("mode".into(), str("primary"));
        Agents::push_permissions(
            a,
            &[
                rule("question", "*", "allow"),
                rule("edit", "*", "deny"),
                rule("edit", &config::join(plan_directory, "*"), "allow"),
            ],
        );
    });

    // config/plugin/agent.ts
    let documents = documents(entries);
    let permissions: Vec<Json> = documents
        .iter()
        .flat_map(|d| d.info.permissions.iter().flat_map(|p| p.0.iter()))
        .map(|r| expand(crate::json::of(r), home))
        .collect();
    agents.configured_default = config::documents(entries).rev().find_map(|d| d.default_agent.clone());
    // Markdown documents never set `default_agent`, so the latest of the config documents is the same value.
    for agent in agents.agents.iter_mut() {
        let Json::Object(agent) = agent else { continue };
        Agents::push_permissions(agent, &permissions);
    }
    for document in &documents {
        let Some(configured) = &document.info.agents else { continue };
        for (id, item) in &configured.0 {
            if item.disabled == Some(true) {
                agents.remove(id);
                continue;
            }
            let exists = agents.index(id).is_some();
            agents.update(id, |agent| {
                if !exists {
                    Agents::push_permissions(agent, &permissions);
                }
                if let Some(model) = &item.model {
                    let mut m = Obj::new();
                    m.str("id", &model.model).str("providerID", &model.provider_id).opt_str("variant", model.variant.as_deref());
                    agent.insert("model".into(), m.done());
                }
                if let Some(request) = &item.request {
                    let target = get_mut(agent, "request");
                    let Json::Object(target) = target else { return };
                    for (key, overlay) in [("headers", request.headers.as_ref().map(crate::json::of)), ("body", request.body.as_ref().map(crate::json::of))] {
                        if let (Some(Json::Object(existing)), Some(Json::Object(overlay))) = (target.get_mut(key), overlay) {
                            // `Object.assign`: existing keys keep their position.
                            for (k, v) in overlay {
                                existing.insert(k, v);
                            }
                        }
                    }
                }
                if let Some(system) = &item.system {
                    agent.insert("system".into(), Json::String(system.clone()));
                }
                if let Some(description) = &item.description {
                    agent.insert("description".into(), Json::String(description.clone()));
                }
                if let Some(mode) = &item.mode {
                    agent.insert("mode".into(), crate::json::of(mode));
                }
                if let Some(hidden) = item.hidden {
                    agent.insert("hidden".into(), Json::Bool(hidden));
                }
                if let Some(color) = &item.color {
                    agent.insert("color".into(), Json::String(color.clone()));
                }
                if let Some(steps) = item.steps {
                    agent.insert("steps".into(), Json::Number(steps as f64));
                }
                if let Some(rules) = &item.permissions {
                    let rules: Vec<Json> = rules.0.iter().map(|r| expand(crate::json::of(r), home)).collect();
                    Agents::push_permissions(agent, &rules);
                }
            });
        }
    }

    // plugin-browser: every agent hides the browser until an attachment allows it.
    let browser = [rule("browser", "*", "deny"), rule("skill", "browser", "deny")];
    for agent in agents.agents.iter_mut() {
        let Json::Object(agent) = agent else { continue };
        Agents::push_permissions(agent, &browser);
    }
    agents
}

/// A document the agent plugin reads: a config document, or one built from a markdown agent file.
struct Document {
    info: t::ConfigInfoPlain,
}

/// Config documents in order, each config directory replaced by the documents of its markdown agent files.
fn documents(entries: &[Entry]) -> Vec<Document> {
    let mut out = vec![];
    for entry in entries {
        match entry {
            Entry::Document { info, .. } => out.push(Document { info: info.clone() }),
            Entry::Directory(directory) => {
                let files = scan::markdown(directory, &["agent", "agents"], true)
                    .into_iter()
                    .map(|f| (f, false))
                    .chain(scan::markdown(directory, &["mode", "modes"], false).into_iter().map(|f| (f, true)));
                for (file, primary) in files {
                    if let Some(info) = config::read(&file).and_then(|content| markdown_agent(directory, &file, primary, &content)) {
                        out.push(Document { info });
                    }
                }
            }
        }
    }
    out
}

/// Fields of a v2 agent (and `variant`); any other key makes a file without v2-only fields a legacy file.
const AGENT_KEYS: [&str; 11] =
    ["variant", "model", "request", "system", "description", "mode", "hidden", "color", "steps", "disabled", "permissions"];
const NATIVE_KEYS: [&str; 4] = ["permissions", "request", "system", "disabled"];

/// The document of one markdown agent file (`decode` in config/plugin/agent.ts): its name is its path below the
/// agents directory, its body the system prompt; a file whose frontmatter does not decode is skipped.
fn markdown_agent(directory: &str, file: &str, primary: bool, content: &str) -> Option<t::ConfigInfoPlain> {
    let parsed = markdown::parse_option(content)?;
    let relative = file.strip_prefix(directory).unwrap_or(file).trim_start_matches('/');
    let name = ["agent/", "agents/", "mode/", "modes/"]
        .iter()
        .find_map(|p| relative.strip_prefix(p))
        .unwrap_or(relative);
    let name = name.strip_suffix(".md").unwrap_or(name);
    let body = config::js_trim(&parsed.content);
    let data = match &parsed.data {
        Json::Object(o) => o.clone(),
        _ => Object::new(),
    };
    let unsupported = data.keys().any(|k| !AGENT_KEYS.contains(&k.as_str()));
    let legacy = unsupported && !data.keys().any(|k| NATIVE_KEYS.contains(&k.as_str()));
    if legacy {
        // TODO(port): ConfigAgentV1 decoding and migration.
        return None;
    }
    let mut data = data;
    if let (Some(Json::String(model)), Some(Json::String(variant))) = (data.get("model"), data.get("variant")) {
        if !model.contains('#') && !variant.is_empty() && !variant.contains('#') {
            let joined = format!("{model}#{variant}");
            data.insert("model".into(), Json::String(joined));
        }
    }
    data.insert("system".into(), Json::String(body.to_owned()));
    let mut agent = t::ConfigAgent::decode(&Json::Object(data)).ok()?;
    if primary {
        agent.mode = Some(t::ConfigAgentMode::Primary);
    }
    let mut agents = Object::new();
    agents.insert(name.to_owned(), crate::json::of(&agent));
    let mut info = Object::new();
    info.insert("agents".into(), Json::Object(agents));
    t::ConfigInfoPlain::decode(&Json::Object(info)).ok()
}

/// `expandPermissions` for one rule: `~`, `~/x`, `$HOME` and `$HOME/x` resources of path actions (`external_directory`,
/// `read`, `edit`) become paths under the home directory. Shell resources are raw shell text and stay as written.
fn expand(rule: Json, home: &str) -> Json {
    let Json::Object(mut o) = rule else { return rule };
    let action = o.get("action").and_then(|v| match v {
        Json::String(s) => Some(s.as_str()),
        _ => None,
    });
    if !matches!(action, Some("external_directory" | "read" | "edit")) {
        return Json::Object(o);
    }
    if let Some(Json::String(resource)) = o.get("resource") {
        let expanded = if resource == "~" || resource == "$HOME" {
            Some(home.to_owned())
        } else {
            resource
                .strip_prefix("~/")
                .or_else(|| resource.strip_prefix("$HOME/"))
                .or_else(|| resource.strip_prefix("$HOME\\"))
                .map(|relative| {
                    // `path.join` keeps a trailing separator.
                    let joined = crate::project::normalize(&format!("{home}/{relative}"));
                    if relative.ends_with('/') && !joined.ends_with('/') { format!("{joined}/") } else { joined }
                })
        };
        if let Some(expanded) = expanded {
            o.insert("resource".into(), Json::String(expanded));
        }
    }
    Json::Object(o)
}
