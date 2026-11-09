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
//! Code Mode tools are registered here; descriptions that depend on the inventory (the subagent list, skills) are
//! the reference configuration's. `ToolOutput.truncate`, plugin `tool.execute.before/after` hooks, input repair.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use opencode_protocol::Json;

use crate::json;

/// One tool as a model request lists it (`ToolDefinition`): `parameters` is the JSON schema, in TS's key order.
#[derive(Debug, Clone)]
pub struct Definition {
    pub name: String,
    pub description: String,
    pub parameters: Json,
}

/// What a tool call receives besides its input (`Tool.Context`).
#[derive(Clone)]
pub struct Context {
    pub session_id: String,
    pub agent: String,
    pub message_id: String,
    pub call_id: String,
    /// The location the session runs in.
    pub directory: String,
    /// Publishes `session.tool.progress` with the given metadata.
    pub progress: Arc<dyn Fn(Json) + Send + Sync>,
}

/// A successful execution (`Tool.NormalizedResult`): `content` is a non-empty array of `Tool.Content`
/// (`{type: "text", text}` or `{type: "file", ...}`), `metadata` an optional record.
#[derive(Debug, Clone)]
pub struct Output {
    pub content: Vec<Json>,
    pub metadata: Option<Json>,
}

/// A failed execution (`Tool.Error`), shown to the model as the tool's error.
#[derive(Debug, Clone)]
pub struct ToolError {
    pub message: String,
    pub metadata: Option<Json>,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), metadata: None }
    }
}

/// Why a call ended without a result: a tool error, or the user declined a permission or dismissed a question
/// (`Permission.DeclinedError`, `QuestionTool.CancelledError`), which stops the turn.
#[derive(Debug, Clone)]
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
        .map(|(name, text)| Arc::new(Unported { definition: definition(name, text) }) as Arc<dyn Tool>)
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

/// `Wildcard.match` (packages/core/src/util/wildcard.ts): `*` is any run, `?` any character, `[*]`/`[?]` literal;
/// a trailing ` *` also matches the bare prefix.
pub fn wildcard(input: &str, pattern: &str) -> bool {
    let input: Vec<char> = input.replace('\\', "/").chars().collect();
    let pattern = pattern.replace('\\', "/");
    #[derive(Clone, Copy, PartialEq)]
    enum Token {
        Any,
        One,
        Char(char),
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' && i + 2 < chars.len() && (chars[i + 1] == '*' || chars[i + 1] == '?') && chars[i + 2] == ']' {
            tokens.push(Token::Char(chars[i + 1]));
            i += 3;
            continue;
        }
        tokens.push(match chars[i] {
            '*' => Token::Any,
            '?' => Token::One,
            c => Token::Char(c),
        });
        i += 1;
    }
    let optional_tail = tokens.len() >= 2 && tokens[tokens.len() - 1] == Token::Any && tokens[tokens.len() - 2] == Token::Char(' ');
    fn matches(input: &[char], tokens: &[Token]) -> bool {
        match tokens.split_first() {
            None => input.is_empty(),
            Some((Token::Any, rest)) => (0..=input.len()).any(|skip| matches(&input[skip..], rest)),
            Some((Token::One, rest)) => !input.is_empty() && matches(&input[1..], rest),
            Some((Token::Char(c), rest)) => input.first() == Some(c) && matches(&input[1..], rest),
        }
    }
    matches(&input, &tokens) || (optional_tail && matches(&input, &tokens[..tokens.len() - 2]))
}

/// `whollyDisabled`: the last rule for the action denies it on every resource.
pub fn wholly_disabled(action: &str, rules: &[Json]) -> bool {
    rules
        .iter()
        .rev()
        .find(|rule| json::get_str(rule, "action").is_some_and(|pattern| wildcard(action, pattern)))
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
    fn wildcards() {
        assert!(wildcard("read", "*"));
        assert!(wildcard("read", "read"));
        assert!(!wildcard("read", "edit"));
        assert!(wildcard("a.env.b", "*.env.*"));
        assert!(wildcard("git", "git *"));
        assert!(wildcard("git status", "git *"));
        assert!(wildcard("x*", "x[*]"));
        assert!(!wildcard("xy", "x[*]"));
    }

    #[test]
    fn snapshot_order() {
        let rules: Vec<Json> = vec![];
        let names = registry().snapshot(&rules).names();
        assert_eq!(names, ["edit", "glob", "grep", "question", "read", "shell", "skill", "subagent", "webfetch", "write", "execute"]);
    }
}
