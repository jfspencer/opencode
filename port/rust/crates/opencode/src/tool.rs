//! The tool registry (packages/core/src/tool.ts, tool/*.ts): what a model request advertises and how a tool call it
//! makes is executed.
//!
//! A tool is a [`Tool`]: its [`Definition`] (the name, description and JSON schema exactly as TS sends them: each
//! built-in tool's is checked in under `tool/definitions/<name>.json`, recorded from the TS reference in its key
//! order), the permission action it is filtered and checked by, whether it is a Code Mode tool (reachable only
//! through `execute`, listed in the Code Mode catalog instead of the tool list), and its execution, which returns
//! TS's tool result shape ([`Output`]: non-empty content plus optional metadata) or a [`ToolError`]. Tools are added
//! independently: implement [`Tool`] in a module under `tool/` and list it in [`builtin`].
//!
//! [`Registry::snapshot`] is `Tool.snapshot(permissions)`: tools whose action the ruleset wholly denies are dropped;
//! direct tools are sorted by name, then `execute` comes last when Code Mode is not denied; the snapshot keeps the
//! Code Mode catalog (`core/codemode` instructions) and executes calls against the tools it advertised.
//!
//! TODO(port): every tool's execution (the tools slice): until a tool is ported, a call fails with a tool error. The
//! Code Mode catalog is the reference configuration's (`codemode-catalog.json`, the `opencode.*` tools), until the
//! Code Mode tools are registered here (the subagent description is live; the skill tool's is static in TS). Plugin
//! `tool.execute.before/after` hooks, input repair. The loop bounds every
//! successful result with [`output::truncate`].

pub mod arguments;
pub mod output;
pub mod shell;
mod question;
pub mod schedule;
pub mod skill;
pub mod subagent;
pub mod webfetch;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use opencode_protocol::Json;

use crate::inventory::wildcard::wildcard_match;
use crate::json;

/// One tool as a model request lists it (`ToolDefinition`): `parameters` is the JSON schema, in TS's key order.
#[derive(Debug, Clone)]
pub struct Definition {
    pub name: String,
    pub description: String,
    pub parameters: Json,
}

/// What a tool call receives besides its input (`Tool.Context`), and the services it works with.
#[derive(Clone)]
#[allow(dead_code)] // Read by the tools themselves (the tools slice).
pub struct Context {
    pub app: Arc<crate::app::App>,
    /// The location the session runs in: its inventory (config), file system, version control and permissions.
    pub location: Arc<crate::location::Location>,
    pub session_id: String,
    pub agent: String,
    pub message_id: String,
    pub call_id: String,
    /// The location the session runs in.
    pub directory: String,
    /// Publishes `session.tool.progress` with the given metadata.
    pub progress: Arc<dyn Fn(Json) + Send + Sync>,
    /// `Permission.Unattended` while a schedule check runs the call (see [`crate::permission::Requester`]).
    pub unattended: Option<Vec<Json>>,
}

/// A successful execution (`Tool.NormalizedResult`): `content` is a non-empty array of `Tool.Content`
/// (`{type: "text", text}` or `{type: "file", ...}`), `metadata` an optional record.
#[derive(Debug, Clone)]
pub struct Output {
    pub content: Vec<Json>,
    pub metadata: Option<Json>,
}

/// A failed execution (`Tool.Error`), shown to the model as the tool's error: the tool part's error is
/// `{type: kind, message}` (`toSessionError`).
#[derive(Debug, Clone)]
pub struct ToolError {
    /// `tool.execution`, or `permission.rejected` for a denied or corrected permission request.
    pub kind: &'static str,
    pub message: String,
    pub metadata: Option<Json>,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { kind: "tool.execution", message: message.into(), metadata: None }
    }

    #[allow(dead_code)] // The tools'.
    pub fn with_metadata(mut self, metadata: Json) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

#[allow(dead_code)] // The tools' (the tools slice).
impl Context {
    fn requester(&self) -> crate::permission::Requester {
        crate::permission::Requester {
            id: None,
            session_id: self.session_id.clone(),
            agent: Some(self.agent.clone()),
            source: Some(crate::permission::Source { message_id: self.message_id.clone(), id: self.call_id.clone() }),
            unattended: self.unattended.clone(),
        }
    }

    /// `permission.assert({...ask, ...Permission.invocation(context)})`: allowed by the rules or approved by the
    /// user, else the failure the tool returns (see [`crate::permission`]).
    pub async fn assert(&self, ask: crate::permission::Ask) -> Result<(), Failure> {
        let requester = self.requester();
        self.location.permission.assert(&self.app, &self.location, &requester, &ask).await.map_err(Failure::from)
    }

    /// `permission.assertNotDenied`: fails only when the rules deny the request; never asks.
    pub fn assert_not_denied(&self, ask: crate::permission::Ask) -> Result<(), Failure> {
        let requester = self.requester();
        self.location.permission.assert_not_denied(&self.app, &self.location, &requester, &ask).map_err(Failure::from)
    }

    /// `permission.allows`: whether the rules already allow the request; never asks.
    pub fn allows(&self, ask: crate::permission::Ask) -> Result<bool, Failure> {
        let requester = self.requester();
        self.location.permission.allows(&self.app, &self.location, &requester, &ask).map_err(Failure::from)
    }
}

impl From<crate::permission::Error> for Failure {
    fn from(error: crate::permission::Error) -> Self {
        use crate::permission::Error;
        match error {
            Error::Declined => Failure::Declined { question: false, message: "The user declined this tool call".into() },
            Error::Blocked { .. } | Error::Corrected(_) => {
                Failure::Error(ToolError { kind: "permission.rejected", message: error.message(), metadata: None })
            }
            Error::SessionNotFound(_) => Failure::Error(ToolError::new(error.message())),
        }
    }
}

/// Why a call ended without a result: a tool error, or the user declined a permission or dismissed a question
/// (`Permission.DeclinedError`, `QuestionTool.CancelledError`), which stops the turn.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `Declined` comes from the permission and question tools.
pub enum Failure {
    Error(ToolError),
    Declined { question: bool, message: String },
}

pub type Execution<'a> = Pin<Box<dyn Future<Output = Result<Output, Failure>> + Send + 'a>>;

/// A registered tool.
pub trait Tool: Send + Sync {
    fn definition(&self) -> &Definition;
    /// The permission action the snapshot filters by (`options.permission ?? name`).
    fn permission(&self) -> &str {
        &self.definition().name
    }
    /// Reachable only inside `execute` (`options.codemode !== false`).
    fn code_mode(&self) -> bool {
        false
    }
    /// Runs one call: check permissions (asking the user where the rules say so), then do the work.
    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a>;
}

/// A built-in tool whose execution is not ported yet.
struct Unported {
    definition: Definition,
}

impl Tool for Unported {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn execute<'a>(&'a self, _context: Context, _input: Json) -> Execution<'a> {
        let name = self.definition.name.clone();
        Box::pin(async move { Err(Failure::Error(ToolError::new(format!("Tool not yet supported by the Rust port: {name}")))) })
    }
}

fn definition(name: &str, text: &str) -> Definition {
    let value = json::parse(text).expect("tool definition is JSON");
    Definition {
        name: name.to_owned(),
        description: json::get_str(&value, "description").unwrap_or_default().to_owned(),
        parameters: json::get(&value, "parameters").cloned().unwrap_or_else(json::object),
    }
}

macro_rules! definitions {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("tool/definitions/", $name, ".json")))),*]
    };
}

/// The built-in tools' recorded definitions, `execute` included.
const DEFINITIONS: &[(&str, &str)] =
    definitions!["edit", "glob", "grep", "question", "read", "shell", "skill", "subagent", "webfetch", "write", "execute"];

/// The built-in tools. A ported tool replaces its [`Unported`] entry here.
fn builtin() -> Vec<Arc<dyn Tool>> {
    DEFINITIONS
        .iter()
        .map(|(name, text)| {
            let definition = definition(name, text);
            match *name {
                "shell" => shell::tool(definition),
                "question" => Arc::new(question::Question { definition }) as Arc<dyn Tool>,
                subagent::NAME => Arc::new(subagent::Subagent::new(definition)),
                "skill" => Arc::new(skill::Skill { definition }),
                "webfetch" => Arc::new(webfetch::WebFetch { definition }),
                _ => Arc::new(Unported { definition }),
            }
        })
        .collect()
}

pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
}

/// The process-wide registry (the built-in tools are the same for every location).
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry { tools: builtin() })
}

/// `Permission.evaluate`'s effect: the last rule matching the action and resource, `ask` without one.
pub fn evaluate<'a>(action: &str, resource: &str, rules: &'a [Json]) -> &'a str {
    crate::permission::evaluate(action, resource, rules)
}

/// `whollyDisabled`: the last rule for the action denies it on every resource.
pub fn wholly_disabled(action: &str, rules: &[Json]) -> bool {
    rules
        .iter()
        .rev()
        .find(|rule| json::get_str(rule, "action").is_some_and(|pattern| wildcard_match(action, pattern)))
        .is_some_and(|rule| json::get_str(rule, "resource") == Some("*") && json::get_str(rule, "effect") == Some("deny"))
}

/// The tools of one request (`Tool.Snapshot`).
#[derive(Clone)]
pub struct Snapshot {
    /// Direct tools by name, then `execute`.
    pub tools: Vec<Arc<dyn Tool>>,
    /// The Code Mode catalog summary (`CodeModeCatalog.Summary` as JSON), when Code Mode is available.
    pub code_mode_catalog: Option<Json>,
}

impl Snapshot {
    pub fn definitions(&self) -> impl Iterator<Item = &Definition> {
        self.tools.iter().map(|tool| tool.definition())
    }

    pub fn names(&self) -> Vec<String> {
        self.definitions().map(|d| d.name.clone()).collect()
    }

    /// Runs a call against the tools this snapshot advertised.
    pub async fn execute(&self, name: &str, context: Context, input: Json) -> Result<Output, Failure> {
        match self.tools.iter().find(|tool| tool.definition().name == name) {
            Some(tool) => tool.execute(context, input).await,
            None => Err(Failure::Error(ToolError::new(format!(
                "No tool named \"{name}\" is currently available. Please use a tool from the available tool list."
            )))),
        }
    }
}

impl Registry {
    /// `Tool.snapshot(permissions)`.
    pub fn snapshot(&self, rules: &[Json]) -> Snapshot {
        let code_mode = !wholly_disabled("execute", rules);
        let mut direct: Vec<Arc<dyn Tool>> = self
            .tools
            .iter()
            .filter(|tool| tool.definition().name != "execute")
            .filter(|tool| !wholly_disabled(tool.permission(), rules))
            .filter(|tool| !(code_mode && tool.code_mode()))
            .cloned()
            .collect();
        direct.sort_by(|a, b| a.definition().name.cmp(&b.definition().name));
        let execute = self.tools.iter().find(|tool| tool.definition().name == "execute").cloned();
        if code_mode {
            direct.extend(execute);
        }
        Snapshot { tools: direct, code_mode_catalog: code_mode.then(code_mode_catalog) }
    }
}

/// STAND-IN: the reference configuration's Code Mode catalog (the `opencode.*` tools of
/// packages/core/src/tool/plugin/opencode.ts, summarized by `CodeModeCatalog.summarize`).
fn code_mode_catalog() -> Json {
    static CATALOG: OnceLock<Json> = OnceLock::new();
    CATALOG.get_or_init(|| json::parse(include_str!("tool/codemode-catalog.json")).expect("catalog is JSON")).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_order() {
        let rules: Vec<Json> = vec![];
        let names = registry().snapshot(&rules).names();
        assert_eq!(names, ["edit", "glob", "grep", "question", "read", "shell", "skill", "subagent", "webfetch", "write", "execute"]);
    }
}
