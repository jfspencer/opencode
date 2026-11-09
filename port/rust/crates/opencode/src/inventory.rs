//! A location's inventory: its resolved config and what the config and the built-in plugins register in it
//! (packages/core/src/config*.ts, agent.ts, provider.ts, model.ts, command.ts, skill.ts and the plugins under
//! packages/core/src/plugin and packages/core/src/config/plugin that fill them). Built when the location boots and
//! rebuilt when its config changes; the wire types are the protocol's.
//!
//! # Interface (for the execution loop and the handlers)
//!
//! A [`crate::location::Location`] holds its inventory behind [`Shared`]; `location.inventory()` hands out the current
//! snapshot (`Arc<Inventory>`), which never changes once built: hold it for the duration of a step.
//!
//! - Agents: [`Inventory::agents`] in `Agent.list` order (the selected default first); [`Inventory::agent`] by ID
//!   (`Agent.get`); [`Inventory::select`] is `Agent.select` (the requested agent, or the default: the configured
//!   `default_agent` when selectable, else `build`, else the first selectable one; `None` when nothing is selectable,
//!   in which case TS still names `build`). An [`t::AgentInfo`] carries `system` (the agent's own prompt; built-ins
//!   without one use the default system prompt), `model` (`{providerID, id, variant?}` from config `agents.<id>.model`,
//!   unset when none is configured), `steps`, and `permissions`: the complete ruleset in evaluation order (the last
//!   matching rule wins): `Agent.Info` defaults, the built-in's rules, every config document's `permissions` (with `~`
//!   / `$HOME` expanded for `read`, `edit` and `external_directory`), the agent's own config rules, and the browser
//!   plugin's denies. `request` (`headers`, `body` from config `agents.<id>.request`) is shown on the agent but TS never
//!   applies it to provider requests: the loop must not either (see the goldens of project-agents' "a custom agent
//!   shapes the provider request" and agent-loop-agents' "request overlays from provider, model and agent config").
//! - Providers and models: [`Inventory::provider`] (`Provider.get`: every provider the policies allow, with `settings`
//!   such as `baseURL`, `timeout`, `headerTimeout`, `chunkTimeout`, and request `headers` / `body` overlays);
//!   [`Inventory::model`] (`Model.get`: chat models, disabled ones included) and [`Inventory::available_models`]
//!   (`Model.available`); [`Inventory::default_model`] (`Model.default`). A [`t::ModelInfo`] is already merged with its
//!   provider: `package`, `settings` (provider settings minus the provider-only timeouts and transport, under the
//!   model's), `headers` and `body` (the provider's under the model's), `capabilities`, `limit`, and `variants` (each
//!   with its own `settings` / `headers` / `body` overlay, applied over the model's for the selected variant).
//!   [`Inventory::api_key`] is the provider's key from the environment (`env` names of config `providers.<id>.env`, the
//!   first one set). A provider request is built from: the provider's `settings.baseURL`, the key, headers = provider
//!   headers < model headers < variant headers (case-insensitive names, `Provider.mergeHeaders`), body = provider body <
//!   model body < variant body (deep merge, [`providers::merge_overlay`]), and settings = model settings < variant
//!   settings (e.g. OpenRouter's `reasoning`, `usage`, `provider`, `transforms`, `user`): [`providers::with_variant`]
//!   applies a variant (`ModelResolver.withVariant`, failing for an unknown one). The provider's own timeouts
//!   (`timeout`, `headerTimeout` default 300000 ms, `chunkTimeout` default 900000 ms) are read from the provider's
//!   settings, not the model's. Non-chat models are in `catalog.other` (listed by type only).
//! - Reloads: [`Shared::reload`] rediscovers the config and announces what changed (`PATCH /api/experimental/config`
//!   calls it). `location.reload` shuts the location down and boots it again, which rebuilds the inventory with it.
//!   The skill sources and instruction files are watched: a change rescans the skills (announcing `skill.updated`)
//!   or rediscovers the instructions, replacing the snapshot.
//! - Commands: [`Inventory::command`] / [`Inventory::commands`] (`Command.get` / `Command.list`); a
//!   [`commands::Command`] is a built-in (`init`, `review`, `goal`) or a config/markdown command with its template,
//!   agent, model and subagent flag. Running one is the execution loop's.
//! - Skills: [`Inventory::skills`] (`Skill.list`, content included) and [`Inventory::skill`]; references:
//!   [`Inventory::references`] (`Reference.list`); instructions: [`Inventory::instructions`] (AGENTS.md files and
//!   config `instructions`, see [`crate::instruction_discovery`]).
//! - Config: [`Inventory::entries`] (the documents, lowest priority first) and [`Inventory::latest`] for one setting
//!   (`Config.latest`), e.g. `shell`, `snapshots`, `tool_output`, `instructions`, `compaction`.

// Much of the interface is the execution loop's (built concurrently): allow it to be unused until then.
#![allow(dead_code)]

pub mod agents;
pub mod commands;
pub mod config;
pub mod config_watch;
#[cfg(test)]
mod frontmatter_check;
pub mod frontmatter_js;
pub mod jsvalue;
pub mod jsonc;
pub mod markdown;
pub mod providers;
pub mod references;
pub mod scan;
pub mod skills;
pub mod wildcard;
pub mod yaml;

use std::sync::{Arc, Mutex, RwLock};

use opencode_protocol::Json;
use opencode_protocol::types as t;

use crate::bus::{Bus, LocationRef, PublishOptions};
use crate::http::Failure;
use crate::json::Obj;

pub use config::Entry;

#[derive(Debug, Clone)]
pub struct Inventory {
    pub directory: String,
    pub sources: config::Sources,
    /// The config entries (`Config.entries`), lowest priority first.
    pub entries: Vec<Entry>,
    /// Agents in list order: the selected default first, then the rest in registration order.
    pub agents: Vec<t::AgentInfo>,
    /// Why the config agent plugin failed to load (TS lists it failed), if it did.
    pub agent_failure: Option<agents::Failure>,
    /// Whether the first of [`Inventory::agents`] is a selected default (some agent is selectable).
    selected: bool,
    pub catalog: providers::Catalog,
    pub commands: Vec<commands::Command>,
    pub skills: Vec<t::SkillInfo>,
    pub references: Vec<t::ReferenceInfo>,
    /// The project's directory (instruction discovery walks up to it).
    pub project_directory: String,
    /// What instruction discovery found (`core/instructions`).
    pub instructions: crate::instruction_discovery::Discovered,
    /// The configured instruction URLs' last fetched copies.
    pub remote_instructions: crate::instruction_discovery::Remote,
    /// The skill directories the configured skill URLs pulled.
    pub pulled_skills: skills::Pulled,
}

impl Inventory {
    fn build(
        directory: &str,
        project_directory: &str,
        sources: config::Sources,
        entries: Vec<Entry>,
        remote_instructions: crate::instruction_discovery::Remote,
        pulled_skills: skills::Pulled,
    ) -> Inventory {
        let options = config::Options::get();
        let registry = agents::build(&entries, &options.home, &config::join(&options.home, ".opencode/plan"));
        let selected = registry.selected().is_some();
        let catalog = providers::build(&entries);
        let commands = commands::build(&entries);
        let skills = skills::build(&entries, &sources, directory, &pulled_skills);
        let references = references::build(&entries, directory, &options.home, &options.repos);
        let instructions =
            crate::instruction_discovery::discover(directory, project_directory, &entries, &remote_instructions);
        Inventory {
            directory: directory.to_owned(),
            sources,
            agents: registry.list(),
            selected,
            agent_failure: registry.failure.clone(),
            catalog,
            commands,
            skills,
            references,
            entries,
            project_directory: project_directory.to_owned(),
            instructions,
            remote_instructions,
            pulled_skills,
        }
    }

    pub fn agent(&self, id: &str) -> Option<&t::AgentInfo> {
        self.agents.iter().find(|a| a.id.0 == id)
    }

    /// The agent a session runs without one chosen (`Agent.select()`): `None` when no agent is selectable.
    pub fn default_agent(&self) -> Option<&t::AgentInfo> {
        self.selected.then(|| self.agents.first()).flatten()
    }

    /// `Agent.select`: the agent's ID and info (the default when `id` is `None`; `build` names a missing default).
    pub fn select(&self, id: Option<&str>) -> (String, Option<&t::AgentInfo>) {
        match id {
            Some(id) => (id.to_owned(), self.agent(id)),
            None => match self.default_agent() {
                Some(agent) => (agent.id.0.clone(), Some(agent)),
                None => (agents::DEFAULT_ID.to_owned(), None),
            },
        }
    }

    /// `Provider.get`.
    pub fn provider(&self, id: &str) -> Option<&t::ProviderInfo> {
        self.catalog.providers.iter().find(|p| p.id.0 == id)
    }

    /// `Model.get`: a chat model of an available provider, enabled or not.
    pub fn model(&self, provider: &str, id: &str) -> Option<&t::ModelInfo> {
        self.catalog.models.iter().find(|m| m.provider_id.0 == provider && m.id == id)
    }

    /// `Model.available()`: enabled chat models, newest release first.
    pub fn available_models(&self) -> impl Iterator<Item = &t::ModelInfo> {
        self.catalog.enabled()
    }

    /// `Model.default`.
    pub fn default_model(&self) -> Option<&t::ModelInfo> {
        self.catalog.default_model()
    }

    /// The provider's API key from the environment: the first set variable of its configured `env` names.
    pub fn api_key(&self, provider: &str) -> Option<String> {
        let names = config::latest(&self.entries, |info| {
            info.providers.as_ref().and_then(|p| p.0.get(provider)).and_then(|p| p.env.as_ref())
        })?;
        names.iter().find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
    }

    pub fn command(&self, name: &str) -> Option<&commands::Command> {
        self.commands.iter().find(|c| c.name == name)
    }

    pub fn skill(&self, id: &str) -> Option<&t::SkillInfo> {
        self.skills.iter().find(|s| s.id.0 == id)
    }

    /// `Config.latest`: the last document's value of one setting.
    pub fn latest<'a, T: ?Sized>(&'a self, key: impl Fn(&'a t::ConfigInfoPlain) -> Option<&'a T>) -> Option<&'a T> {
        config::latest(&self.entries, key)
    }

    /// What `Agent.list` publishes on change (`agent.updated` compares it).
    fn agent_list(&self) -> String {
        opencode_protocol::to_string(&self.agents)
    }
}

/// A location's current inventory, replaced as a whole when its config changes.
///
/// Its skill sources and instruction files are watched ([`crate::watch`]): a burst of changes rescans the skills
/// (announcing `skill.updated` when they changed) or rediscovers the instructions, 100 ms after its last change.
pub struct Shared {
    current: RwLock<Arc<Inventory>>,
    /// Serializes config reloads and rescans (`Config.reload` and the plugins' refreshes hold locks).
    reloading: tokio::sync::Mutex<()>,
    skills: Rescan,
    instructions: Rescan,
    /// The config sources' watches ([`config_watch`]): a change reloads the config.
    config: Rescan,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").field("current", &self.current).finish_non_exhaustive()
    }
}

/// What a watched change rescans.
#[derive(Clone, Copy)]
enum Kind {
    Skills,
    Instructions,
    Config,
}

/// One watched rescan: its watches (replaced on every rescan), where they send, and the task that rescans.
struct Rescan {
    watches: Mutex<Vec<crate::watch::Watch>>,
    changes: crate::watch::Sender,
    pending: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>,
    task: Mutex<Option<crate::watch::Watch>>,
}

impl Rescan {
    fn new() -> Self {
        let (changes, pending) = tokio::sync::mpsc::unbounded_channel();
        Self { watches: Mutex::new(vec![]), changes, pending: Mutex::new(Some(pending)), task: Mutex::new(None) }
    }

    fn replace(&self, watches: Vec<crate::watch::Watch>) {
        *self.watches.lock().unwrap() = watches;
    }

    /// The instruction files' watches (`ConfigInstructionPlugin`'s file subscriptions).
    fn watch_files(&self, files: &[String]) {
        self.replace(files.iter().filter_map(|file| crate::watch::file(file, &self.changes)).collect());
    }

    fn start(&self, kind: Kind, app: Arc<crate::app::App>, location: std::sync::Weak<crate::location::Location>) {
        let Some(mut pending) = self.pending.lock().unwrap().take() else { return };
        let task = tokio::spawn(async move {
            while pending.recv().await.is_some() {
                // Editor saves arrive as bursts: settle first.
                loop {
                    tokio::select! {
                        more = pending.recv() => if more.is_none() { return },
                        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => break,
                    }
                }
                let Some(location) = location.upgrade() else { return };
                let reference = location.reference();
                match kind {
                    Kind::Skills => location.inventory.rescan_skills(&app.bus, &reference).await,
                    Kind::Instructions => location.inventory.rescan_instructions().await,
                    Kind::Config => location.inventory.reload(&app.bus, &reference).await,
                }
            }
        });
        *self.task.lock().unwrap() = Some(crate::watch::Watch::task(task));
    }
}

impl Shared {
    /// Loads the inventory of a location directory (in a project rooted at `project_directory`): fetches what the
    /// config names remotely (instruction URLs, skill indexes) and watches the skill sources as they load.
    pub async fn load(location: &LocationRef, project_directory: &str) -> Result<Shared, Failure> {
        let skill_rescan = Rescan::new();
        let instruction_rescan = Rescan::new();
        let sources = config::discover(&location.directory);
        let entries = config::load(&location.directory, &sources);
        let remote = crate::instruction_discovery::fetch(&entries, &[]).await;
        let (pulled, watches) = skills::pull_all(&entries, &location.directory, Some(&skill_rescan.changes)).await;
        skill_rescan.replace(watches);
        instruction_rescan.watch_files(&crate::instruction_discovery::watched(&location.directory, project_directory, &entries));
        let config_rescan = Rescan::new();
        config_rescan.replace(config_watch::plan(&sources, &config_rescan.changes));
        let inventory = Inventory::build(&location.directory, project_directory, sources, entries, remote, pulled);
        Ok(Self {
            current: RwLock::new(Arc::new(inventory)),
            reloading: tokio::sync::Mutex::new(()),
            skills: skill_rescan,
            instructions: instruction_rescan,
            config: config_rescan,
        })
    }

    /// Starts acting on the watched changes (once the location is booted).
    pub fn watch(&self, app: Arc<crate::app::App>, location: std::sync::Weak<crate::location::Location>) {
        self.skills.start(Kind::Skills, app.clone(), location.clone());
        self.instructions.start(Kind::Instructions, app.clone(), location.clone());
        self.config.start(Kind::Config, app, location);
        // A live watch's readiness requests a reload, which recovers writes made before it attached.
        if !self.config.watches.lock().unwrap().is_empty() {
            let _ = self.config.changes.send(String::new());
        }
    }

    /// `ConfigSkillPlugin.refresh` after a watched change: the sources are loaded (and watched) again, and a change of
    /// the skills is announced.
    async fn rescan_skills(&self, bus: &Bus, location: &LocationRef) {
        let _reloading = self.reloading.lock().await;
        let previous = self.get();
        self.skills.replace(vec![]);
        let (pulled, watches) = skills::pull_all(&previous.entries, &previous.directory, Some(&self.skills.changes)).await;
        self.skills.replace(watches);
        let skills = skills::build(&previous.entries, &previous.sources, &previous.directory, &pulled);
        let changed = opencode_protocol::to_string(&skills) != opencode_protocol::to_string(&previous.skills);
        let next = Inventory { skills, pulled_skills: pulled, ..(*previous).clone() };
        *self.current.write().unwrap() = Arc::new(next);
        if changed {
            bus.emit("skill.updated", Obj::new().done(), PublishOptions { id: None, location: Some(location.clone()) });
        }
    }

    /// `ConfigInstructionPlugin.refresh` after a watched change (the URLs keep their copies).
    async fn rescan_instructions(&self) {
        let _reloading = self.reloading.lock().await;
        let previous = self.get();
        let (directory, project) = (&previous.directory, &previous.project_directory);
        self.instructions.watch_files(&crate::instruction_discovery::watched(directory, project, &previous.entries));
        let instructions =
            crate::instruction_discovery::discover(directory, project, &previous.entries, &previous.remote_instructions);
        if instructions != previous.instructions {
            *self.current.write().unwrap() = Arc::new(Inventory { instructions, ..(*previous).clone() });
        }
    }

    pub fn get(&self) -> Arc<Inventory> {
        self.current.read().unwrap().clone()
    }

    /// `Config.reload` and what observes it: rediscovers and reloads the config documents and rebuilds the inventory.
    /// When the documents (or the compatibility roots) changed, publishes `config.updated`, then the providers and
    /// models are rebuilt twice (by the provider and policy plugins, each announcing); agents, commands, skills and
    /// references announce only when their listings changed (a markdown file edit changes them without
    /// `config.updated`).
    pub async fn reload(&self, bus: &Bus, location: &LocationRef) {
        let _reloading = self.reloading.lock().await;
        let previous = self.get();
        let sources = config::discover(&previous.directory);
        let compatibility_changed = sources.claude != previous.sources.claude || sources.agents != previous.sources.agents;
        let entries = config::load(&previous.directory, &sources);
        // `reconcileWatches`: the watches follow the sources just discovered.
        self.config.replace(config_watch::plan(&sources, &self.config.changes));
        let config_changed = entries != previous.entries || compatibility_changed;
        // A config change fetches the instruction and skill URLs again and renews the watches (`reconfigure`,
        // `refresh`).
        let (remote, pulled) = if config_changed {
            self.skills.replace(vec![]);
            let remote = crate::instruction_discovery::fetch(&entries, &previous.remote_instructions).await;
            let (pulled, watches) = skills::pull_all(&entries, &previous.directory, Some(&self.skills.changes)).await;
            self.skills.replace(watches);
            let (directory, project) = (&previous.directory, &previous.project_directory);
            self.instructions.watch_files(&crate::instruction_discovery::watched(directory, project, &entries));
            (remote, pulled)
        } else {
            (previous.remote_instructions.clone(), previous.pulled_skills.clone())
        };
        let next = Inventory::build(&previous.directory, &previous.project_directory, sources, entries, remote, pulled);
        let options = || PublishOptions { id: None, location: Some(location.clone()) };
        if config_changed {
            bus.emit("config.updated", Obj::new().done(), options());
            for _ in 0..2 {
                bus.emit("provider.updated", Obj::new().done(), options());
                bus.emit("model.updated", Obj::new().done(), options());
            }
        }
        let agents_changed = next.agent_list() != previous.agent_list();
        let commands_changed = commands::listing(&next.commands) != commands::listing(&previous.commands);
        let skills_changed = opencode_protocol::to_string(&next.skills) != opencode_protocol::to_string(&previous.skills);
        let references_changed =
            opencode_protocol::to_string(&next.references) != opencode_protocol::to_string(&previous.references);
        *self.current.write().unwrap() = Arc::new(next);
        if agents_changed {
            bus.emit("agent.updated", Obj::new().done(), options());
        }
        if commands_changed {
            bus.emit("command.updated", Obj::new().done(), options());
        }
        if skills_changed {
            bus.emit("skill.updated", Obj::new().done(), options());
        }
        if references_changed {
            bus.emit("reference.updated", Obj::new().done(), options());
        }
    }
}

/// The `{data}`-less JSON of a wire value list.
pub fn list<T: opencode_protocol::Wire>(items: &[T]) -> Json {
    Json::Array(items.iter().map(crate::json::of).collect())
}
