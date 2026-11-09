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
use super::jsvalue::{self, Shape, Value};
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
    /// Why the config agent plugin failed to load, if it did.
    pub failure: Option<Failure>,
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

    // config/plugin/agent.ts; none of it applies when its load failed.
    let documents = documents(entries).unwrap_or_else(|failure| {
        agents.failure = Some(failure);
        Vec::new()
    });
    let permissions: Vec<Json> = documents
        .iter()
        .flat_map(|d| d.info.permissions.iter().flat_map(|p| p.0.iter()))
        .map(|r| expand(crate::json::of(r), home))
        .collect();
    if agents.failure.is_none() {
        agents.configured_default = config::documents(entries).rev().find_map(|d| d.default_agent.clone());
    }
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

/// The agent plugin failed to load (TS: a defect in its document load, such as `JSON.stringify` throwing while a
/// legacy agent file migrates): none of its documents, `default_agent` or permissions apply, and the plugin list
/// shows it failed with this text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure(pub String);

/// Config documents in order, each config directory replaced by the documents of its markdown agent files.
fn documents(entries: &[Entry]) -> Result<Vec<Document>, Failure> {
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
                    let Some(content) = config::read(&file).filter(|c| !c.is_empty()) else { continue };
                    if let Some(info) = markdown_agent(directory, &file, primary, &content)? {
                        out.push(Document { info });
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Fields of a v2 agent (and `variant`); any other key makes a file without v2-only fields a legacy file.
const AGENT_KEYS: [&str; 11] =
    ["variant", "model", "request", "system", "description", "mode", "hidden", "color", "steps", "disabled", "permissions"];
const NATIVE_KEYS: [&str; 4] = ["permissions", "request", "system", "disabled"];

/// `ConfigAgent.Info` as Effect decodes it from a frontmatter's values (see [`jsvalue::lower`]).
const AGENT: Shape = Shape::Struct(&[
    ("model", Shape::Selection),
    ("request", Shape::Struct(&[("headers", Shape::Record(&Shape::Leaf)), ("body", Shape::Record(&Shape::Json))])),
    ("system", Shape::Leaf),
    ("description", Shape::Leaf),
    ("mode", Shape::Leaf),
    ("hidden", Shape::Leaf),
    ("color", Shape::Caught),
    ("steps", Shape::Leaf),
    ("disabled", Shape::Leaf),
    ("permissions", Shape::Array(&Shape::Struct(&[("action", Shape::Leaf), ("resource", Shape::Leaf), ("effect", Shape::Leaf)]))),
]);

/// The document of one markdown agent file (`decode` in config/plugin/agent.ts): its name is its path below the
/// agents directory, its body the system prompt; a file whose frontmatter does not decode is skipped.
pub(super) fn markdown_agent(directory: &str, file: &str, primary: bool, content: &str) -> Result<Option<t::ConfigInfoPlain>, Failure> {
    let Some(parsed) = markdown::parse_option(content) else { return Ok(None) };
    let relative = file.strip_prefix(directory).unwrap_or(file).trim_start_matches('/');
    let name = ["agent/", "agents/", "mode/", "modes/"]
        .iter()
        .find_map(|p| relative.strip_prefix(p))
        .unwrap_or(relative);
    let name = name.strip_suffix(".md").unwrap_or(name);
    let body = config::js_trim(&parsed.content);
    let data = &parsed.data.root;
    // `Object.keys(markdown.data)`: a string's are its indexes, a `Date`'s none.
    let keys = jsvalue::keys(data);
    let unsupported = keys.iter().any(|k| !AGENT_KEYS.contains(&k.as_str()));
    let legacy = unsupported && !keys.iter().any(|k| NATIVE_KEYS.contains(&k.as_str()));
    let agent = if legacy {
        let input = jsvalue::Obj::plain();
        input.define("name", Value::string(name));
        for (key, value) in jsvalue::entries(data) {
            input.define(&key, value);
        }
        input.define("prompt", Value::string(body));
        legacy_agent(&input)?
    } else {
        native_agent(data, body)
    };
    let Some(mut agent) = agent else { return Ok(None) };
    if primary {
        agent.mode = Some(t::ConfigAgentMode::Primary);
    }
    let mut agents = Object::new();
    agents.insert(name.to_owned(), crate::json::of(&agent));
    let mut info = Object::new();
    info.insert("agents".into(), Json::Object(agents));
    Ok(t::ConfigInfoPlain::decode(&Json::Object(info)).ok())
}

/// A v2 agent file: `model` and `variant` join into one selection (`{ ...data, model }`); the body is the system
/// prompt (`{ ...data, system: body }`).
fn native_agent(data: &Value, body: &str) -> Option<t::ConfigAgent> {
    let input = jsvalue::spread(data);
    if let (Some(Value::String(model)), Some(Value::String(variant))) = (jsvalue::get(data, "model"), jsvalue::get(data, "variant")) {
        if !model.contains('#') && !variant.is_empty() && !variant.contains('#') {
            input.define("model", Value::string(&format!("{model}#{variant}")));
        }
    }
    input.define("system", Value::string(body));
    let lowered = jsvalue::lower(&Value::Object(input), &AGENT).ok()??;
    t::ConfigAgent::decode(&lowered).ok()
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

/// `ConfigAgentV1.Info` decoded, normalized and migrated to a v2 agent (`ConfigMigrateV1.migrateAgent`); `None` when
/// it does not decode. Its fields take `undefined` (`Schema.optional`); `options` and unknown fields are any values,
/// which the migration's `JSON.stringify` serializes, and a value it cannot serialize fails the agent plugin.
fn legacy_agent(input: &jsvalue::Obj) -> Result<Option<t::ConfigAgent>, Failure> {
    const KNOWN: [&str; 17] = [
        "name", "model", "variant", "temperature", "top_p", "prompt", "tools", "disable", "description", "mode", "hidden",
        "options", "color", "steps", "maxSteps", "permission", "name",
    ];
    let Some(decoded) = decode_legacy(input) else { return Ok(None) };
    // `normalize`: `{ ...agent.options }`, then every unknown field assigned (`options[key] = value`).
    let options = jsvalue::Obj::plain();
    for (key, value) in decoded.options {
        options.define(&key, value);
    }
    for (key, value) in input.entries() {
        if !KNOWN.contains(&key.as_str()) {
            options.assign(&key, value);
        }
    }
    // migrateAgent: `{ ...info.options, temperature, top_p }`, written with `JSON.stringify`.
    let body = jsvalue::spread(&Value::Object(options));
    if let Some(t) = decoded.temperature {
        body.define("temperature", Value::Number(t));
    }
    if let Some(p) = decoded.top_p {
        body.define("top_p", Value::Number(p));
    }
    let has_body = !body.entries().is_empty();
    let body = jsvalue::stringify_at(&Value::Object(body), 2).map_err(|thrown| Failure(thrown.to_string()))?;
    let rules: Vec<Json> = decoded
        .permission
        .iter()
        .flat_map(|(key, value)| {
            let action = match key.as_str() {
                "write" | "patch" | "apply_patch" => "edit",
                "task" => "subagent",
                "bash" => "shell",
                other => other,
            };
            match value {
                Rule::Action(effect) => vec![rule(action, "*", effect)],
                Rule::Resources(entries) => entries.iter().map(|(resource, effect)| rule(action, resource, effect)).collect(),
            }
        })
        .collect();
    let mut o = Obj::new();
    if let Some(selection) = decoded.model.as_deref().and_then(|m| model_selection(m, decoded.variant.as_deref())) {
        o.set("model", selection);
    }
    if has_body {
        let mut request = Obj::new();
        request.set("body", body.unwrap_or_default());
        o.set("request", request.done());
    }
    let color = decoded.color.as_deref().map(|c| if c.starts_with('#') { c } else { "#aaaaaa" });
    o.opt_str("system", decoded.prompt.as_deref())
        .opt_str("description", decoded.description.as_deref())
        .opt_str("mode", decoded.mode.as_deref());
    o.opt("hidden", decoded.hidden.map(Json::Bool));
    o.opt_str("color", color);
    o.opt("steps", decoded.steps.or(decoded.max_steps).map(Json::Number));
    o.opt("disabled", decoded.disable.map(Json::Bool));
    if !rules.is_empty() {
        o.set("permissions", Json::Array(rules));
    }
    // `Schema.decodeUnknownSync`: an agent the migration cannot decode throws.
    t::ConfigAgent::decode(&o.done()).map(Some).map_err(|_| Failure("the migrated agent does not decode".to_owned()))
}

/// A V1 permission rule: an action, or actions by resource pattern.
enum Rule {
    Action(String),
    Resources(Vec<(String, String)>),
}

/// The fields of `ConfigAgentV1.Info` the migration reads.
#[derive(Default)]
struct Legacy {
    model: Option<String>,
    variant: Option<String>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    prompt: Option<String>,
    disable: Option<bool>,
    description: Option<String>,
    mode: Option<String>,
    hidden: Option<bool>,
    color: Option<String>,
    steps: Option<f64>,
    max_steps: Option<f64>,
    options: Vec<(String, Value)>,
    /// `tools` (as rules) then `permission`, as a `Map` keeps them: a later key takes the first one's place.
    permission: Vec<(String, Rule)>,
}

/// `ConfigAgentV1.Info`'s decode of the input object; `None` where it fails.
fn decode_legacy(input: &jsvalue::Obj) -> Option<Legacy> {
    // A missing field and an `undefined` one are alike (`Schema.optional`).
    let field = |key: &str| input.own(key).filter(|v| !matches!(v, Value::Undefined));
    let string = |key: &str| match field(key) {
        None => Some(None),
        Some(Value::String(s)) => Some(Some(s.to_string())),
        Some(_) => None,
    };
    let finite = |key: &str| match field(key) {
        None => Some(None),
        Some(Value::Number(n)) if n.is_finite() => Some(Some(n)),
        Some(_) => None,
    };
    let boolean = |key: &str| match field(key) {
        None => Some(None),
        Some(Value::Bool(b)) => Some(Some(b)),
        Some(_) => None,
    };
    let positive = |key: &str| match field(key) {
        None => Some(None),
        Some(Value::Number(n)) if n.fract() == 0.0 && n > 0.0 && n <= 9_007_199_254_740_991.0 => Some(Some(n)),
        Some(_) => None,
    };
    let action = |v: &Value| match v {
        Value::String(s) if ["ask", "allow", "deny"].contains(&&**s) => Some(s.to_string()),
        _ => None,
    };
    // `Schema.ObjectKeyword` without arrays (`Entries`): an object or a function, read with `Object.entries`.
    let entries = |v: &Value| match v {
        Value::Object(o) if !o.is_array() => Some(o.entries()),
        _ => None,
    };
    let mut out = Legacy {
        model: string("model")?,
        variant: string("variant")?,
        temperature: finite("temperature")?,
        top_p: finite("top_p")?,
        prompt: string("prompt")?,
        disable: boolean("disable")?,
        description: string("description")?,
        mode: string("mode")?,
        hidden: boolean("hidden")?,
        color: string("color")?,
        steps: positive("steps")?,
        max_steps: positive("maxSteps")?,
        ..Legacy::default()
    };
    if out.mode.as_deref().is_some_and(|m| !["subagent", "primary", "all"].contains(&m)) {
        return None;
    }
    if let Some(color) = &out.color {
        let hex = color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit());
        let theme = ["primary", "secondary", "accent", "success", "warning", "error", "info"].contains(&color.as_str());
        if !hex && !theme {
            return None;
        }
    }
    out.options = match field("options") {
        None => Vec::new(),
        Some(options) if jsvalue::is_record(&options) => jsvalue::entries(&options),
        Some(_) => return None,
    };
    let mut permission: Vec<(String, Rule)> = Vec::new();
    let mut set = |key: String, rule: Rule| match permission.iter_mut().find(|(k, _)| *k == key) {
        Some(entry) => entry.1 = rule,
        None => permission.push((key, rule)),
    };
    if let Some(tools) = field("tools") {
        for (tool, enabled) in entries(&tools)? {
            let Value::Bool(enabled) = enabled else { return None };
            let key = if tool == "write" || tool == "patch" { "edit".to_owned() } else { tool };
            set(key, Rule::Action(if enabled { "allow" } else { "deny" }.to_owned()));
        }
    }
    match field("permission") {
        None => {}
        Some(Value::String(s)) => set("*".to_owned(), Rule::Action(action(&Value::String(s))?)),
        Some(value) => {
            for (key, rule) in entries(&value)? {
                let rule = match action(&rule) {
                    Some(effect) => Rule::Action(effect),
                    None if ["question", "websearch", "doom_loop"].contains(&key.as_str()) => return None,
                    None => Rule::Resources(
                        entries(&rule)?.into_iter().map(|(resource, effect)| action(&effect).map(|e| (resource, e))).collect::<Option<_>>()?,
                    ),
                };
                set(key, rule);
            }
        }
    }
    out.permission = permission;
    Some(out)
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
