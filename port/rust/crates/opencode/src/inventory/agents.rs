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
//! A markdown agent file whose frontmatter has fields outside the v2 agent and none of the v2-only ones is a legacy
//! (v1) file: it decodes as `ConfigAgentV1.Info` and migrates (`ConfigMigrateV1.migrateAgent`): `prompt` (the body)
//! is the system prompt, `temperature`, `top_p`, `options` and unknown fields become the request body, `tools` and
//! `permission` become rules, `maxSteps` is `steps`, `disable` is `disabled`, a theme color becomes `#aaaaaa`.

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
                    // An empty file is skipped.
                    let content = config::read(&file).filter(|c| !c.is_empty());
                    if let Some(info) = content.and_then(|content| markdown_agent(directory, &file, primary, &content)) {
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
    let mut agent = if legacy {
        let mut input = Object::new();
        input.insert("name".into(), Json::String(name.to_owned()));
        input.extend(data);
        input.insert("prompt".into(), Json::String(body.to_owned()));
        legacy_agent(&input)?
    } else {
        native_agent(data, body)?
    };
    if primary {
        agent.mode = Some(t::ConfigAgentMode::Primary);
    }
    let mut agents = Object::new();
    agents.insert(name.to_owned(), crate::json::of(&agent));
    let mut info = Object::new();
    info.insert("agents".into(), Json::Object(agents));
    t::ConfigInfoPlain::decode(&Json::Object(info)).ok()
}

/// A v2 agent file: `model` and `variant` join into one selection; the body is the system prompt.
fn native_agent(data: Object, body: &str) -> Option<t::ConfigAgent> {
    let mut data = data;
    if let (Some(Json::String(model)), Some(Json::String(variant))) = (data.get("model"), data.get("variant")) {
        if !model.contains('#') && !variant.is_empty() && !variant.contains('#') {
            let joined = format!("{model}#{variant}");
            data.insert("model".into(), Json::String(joined));
        }
    }
    data.insert("system".into(), Json::String(body.to_owned()));
    t::ConfigAgent::decode(&Json::Object(data)).ok()
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

/// `ConfigAgentV1.Info` decoded, normalized and migrated to a v2 agent; `None` when it does not decode.
fn legacy_agent(input: &Object) -> Option<t::ConfigAgent> {
    const KNOWN: [&str; 17] = [
        "name", "model", "variant", "temperature", "top_p", "prompt", "tools", "disable", "description", "mode", "hidden",
        "options", "color", "steps", "maxSteps", "permission", "name",
    ];
    let string = |key: &str| match input.get(key) {
        None => Some(None),
        Some(Json::String(s)) => Some(Some(s.clone())),
        Some(_) => None,
    };
    let finite = |key: &str| match input.get(key) {
        None => Some(None),
        Some(Json::Number(n)) if n.is_finite() => Some(Some(*n)),
        Some(_) => None,
    };
    let boolean = |key: &str| match input.get(key) {
        None => Some(None),
        Some(Json::Bool(b)) => Some(Some(*b)),
        Some(_) => None,
    };
    let positive = |key: &str| match input.get(key) {
        None => Some(None),
        Some(Json::Number(n)) if n.fract() == 0.0 && *n > 0.0 && n.abs() <= 9_007_199_254_740_991.0 => Some(Some(*n)),
        Some(_) => None,
    };
    let model = string("model")?;
    let variant = string("variant")?;
    let temperature = finite("temperature")?;
    let top_p = finite("top_p")?;
    let prompt = string("prompt")?;
    let disable = boolean("disable")?;
    let description = string("description")?;
    let mode = string("mode")?;
    if mode.as_deref().is_some_and(|m| !["subagent", "primary", "all"].contains(&m)) {
        return None;
    }
    let hidden = boolean("hidden")?;
    let color = string("color")?;
    if let Some(color) = &color {
        let hex = color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit());
        let theme = ["primary", "secondary", "accent", "success", "warning", "error", "info"].contains(&color.as_str());
        if !hex && !theme {
            return None;
        }
    }
    let steps = positive("steps")?;
    let max_steps = positive("maxSteps")?;
    let options = match input.get("options") {
        None => Object::new(),
        Some(Json::Object(o)) => o.clone(),
        Some(_) => return None,
    };
    // `tools`: an object of booleans; `permission`: an action, or an object of actions / objects of actions.
    let tools: Vec<(String, bool)> = match input.get("tools") {
        None => vec![],
        Some(Json::Object(o)) => {
            let mut out = vec![];
            for (k, v) in o {
                let Json::Bool(b) = v else { return None };
                out.push((k.clone(), *b));
            }
            out
        }
        Some(_) => return None,
    };
    let action = |v: &Json| match v {
        Json::String(s) if ["ask", "allow", "deny"].contains(&s.as_str()) => Some(s.clone()),
        _ => None,
    };
    let mut permission: Vec<(String, Json)> = vec![];
    for (tool, enabled) in &tools {
        let key = if tool == "write" || tool == "patch" { "edit".to_owned() } else { tool.clone() };
        let value = Json::String(if *enabled { "allow" } else { "deny" }.into());
        set_entry(&mut permission, key, value);
    }
    match input.get("permission") {
        None => {}
        Some(Json::String(s)) => {
            action(&Json::String(s.clone()))?;
            set_entry(&mut permission, "*".into(), Json::String(s.clone()));
        }
        Some(Json::Object(o)) => {
            for (key, rule) in o {
                match rule {
                    Json::String(_) => {
                        action(rule)?;
                    }
                    Json::Object(entries) if !["question", "websearch", "doom_loop"].contains(&key.as_str()) => {
                        if !entries.values().all(|e| action(e).is_some()) {
                            return None;
                        }
                    }
                    _ => return None,
                }
                set_entry(&mut permission, key.clone(), rule.clone());
            }
        }
        Some(_) => return None,
    }

    // migrateAgent
    let mut body = options;
    for (key, value) in input {
        if !KNOWN.contains(&key.as_str()) {
            body.insert(key.clone(), value.clone());
        }
    }
    if let Some(t) = temperature {
        body.insert("temperature".into(), Json::Number(t));
    }
    if let Some(p) = top_p {
        body.insert("top_p".into(), Json::Number(p));
    }
    let rules: Vec<Json> = permission
        .iter()
        .flat_map(|(key, value)| {
            let action = match key.as_str() {
                "write" | "patch" | "apply_patch" => "edit",
                "task" => "subagent",
                "bash" => "shell",
                other => other,
            };
            match value {
                Json::String(effect) => vec![rule(action, "*", effect)],
                Json::Object(entries) => entries
                    .iter()
                    .filter_map(|(resource, effect)| match effect {
                        Json::String(effect) => Some(rule(action, resource, effect)),
                        _ => None,
                    })
                    .collect(),
                _ => vec![],
            }
        })
        .collect();
    let mut o = Obj::new();
    if let Some(selection) = model.as_deref().and_then(|m| model_selection(m, variant.as_deref())) {
        o.set("model", selection);
    }
    if !body.is_empty() {
        let mut request = Obj::new();
        request.set("body", Json::Object(body));
        o.set("request", request.done());
    }
    o.opt_str("system", prompt.as_deref()).opt_str("description", description.as_deref()).opt_str("mode", mode.as_deref());
    o.opt("hidden", hidden.map(Json::Bool));
    o.opt_str("color", color.as_deref().map(|c| if c.starts_with('#') { c } else { "#aaaaaa" }));
    o.opt("steps", steps.or(max_steps).map(Json::Number));
    o.opt("disabled", disable.map(Json::Bool));
    if !rules.is_empty() {
        o.set("permissions", Json::Array(rules));
    }
    t::ConfigAgent::decode(&o.done()).ok()
}

/// A `Map.set`: a later key keeps the first one's position.
fn set_entry(entries: &mut Vec<(String, Json)>, key: String, value: Json) {
    match entries.iter_mut().find(|(k, _)| *k == key) {
        Some(entry) => entry.1 = value,
        None => entries.push((key, value)),
    }
}

/// `ConfigMigrateV1.modelSelection`: a `provider/model#variant` reference (or one with a separate variant).
fn model_selection(input: &str, variant: Option<&str>) -> Option<Json> {
    let mut wrapper = Object::new();
    wrapper.insert("model".into(), Json::String(input.to_owned()));
    let decoded = t::ConfigAgent::decode(&Json::Object(wrapper)).ok()?.model?;
    let selected = decoded.variant.clone().or(variant.map(str::to_owned));
    let provider = match decoded.provider_id.as_str() {
        "azure-cognitive-services" => "azure",
        "google-vertex-anthropic" => "google-vertex",
        other => other,
    };
    let mut o = Obj::new();
    o.str("providerID", provider).str("model", &decoded.model);
    if let Some(v) = selected.filter(|v| !v.is_empty() && !v.contains('#')) {
        o.str("variant", &v);
    }
    Some(o.done())
}
