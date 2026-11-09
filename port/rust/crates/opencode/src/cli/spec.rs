//! The command tree (packages/cli/src/commands/commands.ts), as effect/cli 4.1 sees it: every command with its
//! description, alias, parameters in declaration order and declared global flags. Help, parsing and the shell
//! completions all follow from it.

/// A value's primitive type (effect/cli `Primitive`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Bool,
    Str,
    Int,
    /// `Flag.Literals` / `Flag.ChoiceWithValue`: the accepted keys.
    Choice(&'static [&'static str]),
    KeyValue,
}

/// How many values a parameter takes and what its absence means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Exactly one value (`Missing required ...` without it).
    Required,
    /// `Flag.optional` / `Argument.optional`.
    Optional,
    /// `withDefault(value)`: the default as its raw text.
    Default(&'static str),
    /// `variadic` / `atMost` / `atLeast`, possibly wrapped in `optional`.
    Many { min: Option<usize>, max: Option<usize>, optional: bool },
}

/// A schema check applied after the primitive (`Flag.withSchema`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Check {
    None,
    AtLeast(i64),
    Between(i64, i64),
    NonEmpty,
    /// `pair --url`'s `Flag.mapTryCatch`: an HTTP(S) URL without credentials, query or fragment, its `href` without
    /// trailing slashes.
    PairingUrl,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub flag: bool,
    pub name: &'static str,
    /// `withAlias` (each flag here has at most one).
    pub alias: Option<&'static str>,
    pub kind: Kind,
    pub description: Option<&'static str>,
    pub hidden: bool,
    /// `Flag.withOptionalValue`: the value a bare flag takes; the next word is never consumed.
    pub optional_value: Option<&'static str>,
    /// `withMetavar`: the type name shown in help.
    pub metavar: Option<&'static str>,
    pub mode: Mode,
    pub check: Check,
}

impl Param {
    /// The long name that sets a boolean flag to false.
    pub fn negation(&self) -> Option<String> {
        (self.kind == Kind::Bool).then(|| format!("no-{}", self.name))
    }

    pub fn type_name(&self) -> &'static str {
        if let Some(metavar) = self.metavar {
            return metavar;
        }
        match self.kind {
            Kind::Bool => "boolean",
            Kind::Str => "string",
            Kind::Int => "integer",
            Kind::Choice(_) => "choice",
            Kind::KeyValue => "key=value",
        }
    }

    pub fn is_optional(&self) -> bool {
        matches!(self.mode, Mode::Optional | Mode::Default(_) | Mode::Many { optional: true, .. })
    }

    pub fn is_variadic(&self) -> bool {
        matches!(self.mode, Mode::Many { .. })
    }
}

/// A global flag (effect/cli `GlobalFlag`): built-in actions, built-in settings and opencode's settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Global {
    Help,
    Version,
    Wizard,
    Completions,
    LogLevel,
    PrintLogs,
    Standalone,
    Server,
}

pub const BUILT_INS: [Global; 5] = [Global::Help, Global::Version, Global::Wizard, Global::Completions, Global::LogLevel];

pub const SHELLS: &[&str] = &["bash", "zsh", "fish", "powershell", "sh", "pwsh"];
const LOG_LEVELS: &[&str] = &["all", "trace", "debug", "info", "warn", "warning", "error", "fatal", "none"];

impl Global {
    pub fn is_action(self) -> bool {
        matches!(self, Global::Help | Global::Version | Global::Wizard | Global::Completions)
    }

    pub fn param(self) -> Param {
        match self {
            Global::Help => flag("help", Kind::Bool).alias("h").describe("Show help information").default("false"),
            Global::Version => flag("version", Kind::Bool).alias("v").describe("Show version information").default("false"),
            Global::Wizard => flag("wizard", Kind::Bool).describe("Start wizard mode for a command").default("false"),
            Global::Completions => flag("completions", Kind::Choice(SHELLS))
                .optional()
                .metavar("<bash|zsh|fish|powershell|sh|pwsh>")
                .describe("Print shell completion script"),
            Global::LogLevel => flag("log-level", Kind::Choice(LOG_LEVELS))
                .optional()
                .describe("Sets the minimum log level")
                .metavar("<all|trace|debug|info|warn|warning|error|fatal|none>"),
            Global::PrintLogs => flag("print-logs", Kind::Bool)
                .describe("Print logs to stderr (server logs require --standalone)")
                .default("false"),
            Global::Standalone => flag("standalone", Kind::Bool)
                .describe("Run with a private server instead of the background service")
                .default("false"),
            Global::Server => {
                flag("server", Kind::Str).describe("Connect to a server URL instead of the background service").optional()
            }
        }
    }
}

pub struct Command {
    pub name: &'static str,
    pub alias: Option<&'static str>,
    pub description: &'static str,
    pub params: Vec<Param>,
    pub globals: Vec<Global>,
    pub subcommands: Vec<Command>,
    /// Whether a handler is registered (group commands without one show their help).
    pub handled: bool,
}

impl Command {
    pub fn flags(&self) -> impl Iterator<Item = &Param> {
        self.params.iter().filter(|p| p.flag)
    }

    pub fn arguments(&self) -> impl Iterator<Item = &Param> {
        self.params.iter().filter(|p| !p.flag)
    }

    /// The subcommand a word names (name or alias).
    pub fn sub(&self, word: &str) -> Option<&Command> {
        self.subcommands.iter().find(|c| c.name == word || c.alias == Some(word))
    }

    /// The subcommand index keys in effect/cli's order: each name, then its alias.
    pub fn sub_keys(&self) -> Vec<&'static str> {
        self.subcommands.iter().flat_map(|c| std::iter::once(c.name).chain(c.alias)).collect()
    }
}

fn flag(name: &'static str, kind: Kind) -> Param {
    Param {
        flag: true,
        name,
        alias: None,
        kind,
        description: None,
        hidden: false,
        optional_value: None,
        metavar: None,
        mode: Mode::Required,
        check: Check::None,
    }
}

fn arg(name: &'static str, kind: Kind) -> Param {
    Param { flag: false, ..flag(name, kind) }
}

impl Param {
    fn alias(mut self, alias: &'static str) -> Self {
        self.alias = Some(alias);
        self
    }
    fn describe(mut self, text: &'static str) -> Self {
        self.description = Some(text);
        self
    }
    fn optional(mut self) -> Self {
        self.mode = match self.mode {
            Mode::Many { min, max, .. } => Mode::Many { min, max, optional: true },
            _ => Mode::Optional,
        };
        self
    }
    fn default(mut self, value: &'static str) -> Self {
        self.mode = Mode::Default(value);
        self
    }
    fn many(mut self, min: Option<usize>, max: Option<usize>) -> Self {
        self.mode = Mode::Many { min, max, optional: false };
        self
    }
    fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }
    fn metavar(mut self, metavar: &'static str) -> Self {
        self.metavar = Some(metavar);
        self
    }
    fn check(mut self, check: Check) -> Self {
        self.check = check;
        self
    }
}

fn command(name: &'static str, description: &'static str) -> Command {
    Command { name, alias: None, description, params: vec![], globals: vec![], subcommands: vec![], handled: true }
}

impl Command {
    fn params(mut self, params: Vec<Param>) -> Self {
        self.params = params;
        self
    }
    fn globals(mut self, globals: &[Global]) -> Self {
        self.globals = globals.to_vec();
        self
    }
    fn group(mut self, subcommands: Vec<Command>) -> Self {
        self.subcommands = subcommands;
        self.handled = false;
        self
    }
    fn alias(mut self, alias: &'static str) -> Self {
        self.alias = Some(alias);
        self
    }
}

const SERVER_FLAGS: &[Global] = &[Global::Standalone, Global::Server];
const UPGRADE_METHODS: &[&str] = &["curl", "npm", "pnpm", "bun", "yarn", "vp", "brew", "winget"];
const PATH_NAMES: &[&str] = &["db", "home", "data", "config", "cache", "state", "tmp", "bin", "log", "repos"];

fn permission_params() -> Vec<Param> {
    vec![
        flag("auto", Kind::Bool).describe("Auto-approve permissions that are not explicitly denied").default("false"),
        flag("yolo", Kind::Bool).default("false").hidden(),
        flag("dangerously-skip-permissions", Kind::Bool).default("false").hidden(),
    ]
}

fn continue_flag() -> Param {
    flag("continue", Kind::Bool).alias("c").describe("Continue the last session").default("false")
}

fn session_flag() -> Param {
    flag("session", Kind::Str).alias("s").describe("Session ID to continue, or to create if it does not exist").optional()
}

/// The whole tree. The root's own global flag is `--print-logs` (`Runtime.run` adds it).
pub fn root() -> Command {
    let mut root_params = vec![Global::Standalone.param(), Global::Server.param()];
    root_params.extend(permission_params());
    root_params.extend([
        arg("directory", Kind::Str).describe("Directory to start OpenCode in").optional(),
        continue_flag(),
        session_flag(),
        flag("prompt", Kind::Str).describe("Prompt to use").optional(),
    ]);
    let mut run_params = vec![
        arg("message", Kind::Str).describe("Message to send").many(Some(0), None),
        continue_flag(),
        session_flag(),
        flag("fork", Kind::Bool).describe("Fork the session before continuing").default("false"),
        Param {
            optional_value: Some(""),
            metavar: Some("name"),
            ..flag("worktree", Kind::Str)
                .alias("w")
                .describe("Start a new session in a new git worktree, optionally with this name")
                .optional()
        },
        flag("model", Kind::Str).alias("m").describe("Model to use in the format provider/model#variant").optional(),
        flag("agent", Kind::Str).describe("Agent to use").optional(),
        flag("format", Kind::Choice(&["default", "json"])).describe("Output format").default("default"),
        flag("file", Kind::Str).alias("f").describe("File to attach to the message").many(None, Some(100)),
        flag("title", Kind::Str).describe("Session title").optional(),
        flag("archived", Kind::Bool)
            .describe("Create the session archived so it stays out of session lists (useful for scripted or CI runs)")
            .default("false"),
        flag("thinking", Kind::Bool).describe("Show thinking blocks").default("false"),
    ];
    run_params.extend(permission_params());
    Command {
        globals: vec![Global::PrintLogs],
        handled: true,
        ..command("opencode", "OpenCode command line interface").params(root_params).group(vec![
            command("upgrade", "Upgrade OpenCode to the latest or a specific version").alias("update").params(vec![
                arg("target", Kind::Str).describe("Version to upgrade to (with or without a leading v)").optional(),
                flag("method", Kind::Choice(UPGRADE_METHODS)).alias("m").describe("Installation method to use").optional(),
            ]),
            command("uninstall", "Uninstall OpenCode, keeping session data, configuration, and state").params(vec![
                flag("dry-run", Kind::Bool).describe("Show what would be removed without removing").default("false"),
                flag("force", Kind::Bool).alias("f").describe("Skip confirmation prompts").default("false"),
            ]),
            command("acp", "Start an Agent Client Protocol server").params(vec![
                flag("login", Kind::Bool).describe("Run auth login instead of starting the server").default("false"),
            ]),
            command("api", "Make a request to the running server").globals(SERVER_FLAGS).params(vec![
                arg("operation | method path", Kind::Str)
                    .describe("OpenAPI operation ID, or an HTTP method followed by a path")
                    .many(Some(1), Some(2)),
                flag("data", Kind::Str).alias("d").describe("Request body").optional(),
                flag("header", Kind::Str).alias("H").describe("Request header in name:value form").many(None, Some(100)),
                flag("param", Kind::KeyValue).describe("OpenAPI path or query parameter").many(Some(1), None).optional(),
            ]),
            command("debug", "Debugging and troubleshooting tools").group(vec![
                command("agents", "List all agents").globals(SERVER_FLAGS),
                command("config", "List configuration sources").globals(SERVER_FLAGS),
                command("heap-dump", "Write a heap snapshot of the running server and print its path")
                    .globals(&[Global::Server])
                    .params(vec![
                        flag("directory", Kind::Str)
                            .describe("Directory to write the snapshot into instead of the server's log directory")
                            .optional(),
                    ]),
                command("paths", "Show global paths (data, config, cache, state)").params(vec![
                    arg("name", Kind::Choice(PATH_NAMES))
                        .describe("Print only one path: db, home, data, config, cache, state, tmp, bin, log, repos")
                        .optional(),
                ]),
            ]),
            command("auth", "manage integrations and credentials").globals(SERVER_FLAGS).group(vec![
                command("list", "list integrations and credentials").params(vec![
                    flag("format", Kind::Choice(&["default", "json"])).describe("Output format").default("default"),
                ]),
                command("login", "connect an integration").params(vec![
                    arg("target", Kind::Str).describe("Integration ID, name, or well-known provider URL").optional(),
                    flag("method", Kind::Str).describe("Authentication method ID").optional(),
                    flag("answer", Kind::Str)
                        .describe("Provider form answer (key=value; repeat for multiple fields)")
                        .many(None, Some(100)),
                ]),
                command("logout", "log out of a saved account").params(vec![
                    arg("target", Kind::Str).describe("Integration ID or name").optional(),
                    arg("credential", Kind::Str)
                        .describe("Credential ID or label (opens an account picker when omitted)")
                        .optional(),
                ]),
                command("export", "print stored credentials, including secrets, as JSON").params(vec![
                    arg("target", Kind::Str)
                        .describe("Integration ID or name (exports every integration when omitted)")
                        .optional(),
                ]),
                command("import", "import credentials exported by auth export").params(vec![
                    arg("file", Kind::Str).describe("JSON file to import (reads stdin when omitted)").optional(),
                ]),
                command("switch", "switch the active account for an integration").params(vec![
                    arg("target", Kind::Str).describe("Integration ID or name").optional(),
                    arg("credential", Kind::Str)
                        .describe("Credential ID or label (opens an account picker when omitted)")
                        .optional(),
                ]),
            ]),
            command("mcp", "Manage MCP (Model Context Protocol) servers").group(vec![
                command("list", "List configured MCP servers and their status").globals(SERVER_FLAGS),
                command("add", "Add an MCP server to your configuration").params(vec![
                    arg("name", Kind::Str).describe("Name of the MCP server"),
                    arg("command", Kind::Str)
                        .describe("Command and arguments for a local server, passed after --")
                        .many(Some(0), None),
                    flag("url", Kind::Str).describe("URL for a remote MCP server").optional(),
                    flag("header", Kind::KeyValue)
                        .describe("HTTP header for a remote server, as name=value")
                        .many(Some(1), None)
                        .optional(),
                    flag("env", Kind::KeyValue)
                        .describe("Environment variable for a local server, as name=value")
                        .many(Some(1), None)
                        .optional(),
                    flag("global", Kind::Bool)
                        .describe("Write to the global config instead of the project config")
                        .default("false"),
                ]),
                command("auth", "Authenticate with an OAuth-capable remote MCP server")
                    .globals(SERVER_FLAGS)
                    .params(vec![arg("name", Kind::Str).describe("Name of the MCP server").optional()]),
                command("logout", "Remove stored OAuth credentials for an MCP server")
                    .globals(SERVER_FLAGS)
                    .params(vec![arg("name", Kind::Str).describe("Name of the MCP server")]),
            ]),
            command("plugin", "Manage plugins").group(vec![
                command("list", "List plugins").globals(SERVER_FLAGS).params(vec![
                    flag("builtin", Kind::Bool).describe("Include built-in server plugins").default("false"),
                ]),
                command("add", "Install a plugin and add it to the global configuration")
                    .params(vec![arg("package", Kind::Str).describe("npm registry or Git package specifier")]),
                command("check", "Check package plugins for updates")
                    .globals(SERVER_FLAGS)
                    .params(vec![arg("target", Kind::Str).describe("Configured package target").optional()]),
                command("update", "Update package plugins").globals(SERVER_FLAGS).params(vec![
                    arg("target", Kind::Str)
                        .describe("Configured package target; omit to update all outdated plugins")
                        .optional(),
                ]),
                command("remove", "Remove a plugin from global configuration")
                    .params(vec![arg("package", Kind::Str).describe("configured package specifier")]),
            ]),
            command("models", "List all available models").globals(SERVER_FLAGS),
            command("stats", "Show shareable usage statistics").globals(SERVER_FLAGS).params(vec![
                flag("days", Kind::Int).check(Check::AtLeast(0)).describe("Show the last N days; 0 means today").optional(),
                flag("year", Kind::Int).check(Check::Between(1970, 9_999)).describe("Show a calendar year").optional(),
                flag("all", Kind::Bool).describe("Show lifetime statistics").default("false"),
                flag("project", Kind::Str)
                    .describe("Filter by project ID, or use \".\" for the current project")
                    .optional(),
                flag("models", Kind::Bool).describe("Show model usage").default("false"),
                flag("tools", Kind::Bool).describe("Show tool reliability").default("false"),
                flag("cost", Kind::Bool).describe("Show cost and token details").default("false"),
                flag("full", Kind::Bool).describe("Show every detailed section").default("false"),
                flag("limit", Kind::Int).check(Check::AtLeast(1)).describe("Number of rows in detailed sections").default("5"),
                flag("json", Kind::Bool).describe("Output statistics as JSON").default("false"),
            ]),
            command("mini", "Start the minimal interactive interface").globals(SERVER_FLAGS).params(vec![
                continue_flag(),
                session_flag(),
                flag("fork", Kind::Bool).describe("Fork the session when continuing").default("false"),
                flag("replay", Kind::Bool)
                    .describe("Restore session history on resume and resize (disable with --no-replay)")
                    .optional(),
                flag("replay-limit", Kind::Int)
                    .describe("Limit replay to the newest N messages (default: 200)")
                    .optional(),
                flag("model", Kind::Str).alias("m").describe("Model to use in the format provider/model").optional(),
                flag("agent", Kind::Str).describe("Agent to use").optional(),
                flag("prompt", Kind::Str).describe("Prompt to use").optional(),
                flag("demo", Kind::Bool).default("false").hidden(),
            ]),
            command("run", "Run OpenCode with a message").globals(SERVER_FLAGS).params(run_params),
            command("session", "Manage sessions").globals(SERVER_FLAGS).group(vec![
                command("list", "List top-level sessions in the current project, newest first").params(vec![
                    flag("max-count", Kind::Int)
                        .alias("n")
                        .check(Check::AtLeast(1))
                        .describe("Limit to N most recent sessions (default: 100)")
                        .optional(),
                    flag("format", Kind::Choice(&["table", "json"])).describe("Output format").default("table"),
                ]),
                command("delete", "Delete a session and its child sessions")
                    .params(vec![arg("sessionID", Kind::Str).describe("Session ID to delete")]),
                command("export", "Export session data as JSON").params(vec![
                    arg("session", Kind::Str).describe("Session ID to export").optional(),
                    flag("sanitize", Kind::Bool).describe("Redact sensitive transcript and file data").default("false"),
                ]),
                command("import", "Import session data from a JSON file or URL").params(vec![
                    arg("file", Kind::Str).describe("JSON file or URL to import"),
                    flag("directory", Kind::Str).describe("Directory in which to import the session").optional(),
                ]),
            ]),
            command("service", "Manage the background server").group(vec![
                command("start", "Start the background server"),
                command("restart", "Restart the background server"),
                command("status", "Show background server status"),
                command("stop", "Stop the background server"),
                command("get", "Get service configuration").params(vec![
                    arg("key", Kind::Str).describe("Service setting or env").optional(),
                    arg("name", Kind::Str).describe("Environment variable name").optional(),
                ]),
                command("set", "Set service configuration").params(vec![
                    arg("key", Kind::Str).describe("Service setting or env"),
                    arg("value", Kind::Str).describe("Setting value or environment variable name"),
                    arg("env-value", Kind::Str).describe("Environment variable value").optional(),
                ]),
                command("unset", "Unset service configuration").params(vec![
                    arg("key", Kind::Str).describe("Service setting or env"),
                    arg("name", Kind::Str).describe("Environment variable name").optional(),
                ]),
            ]),
            command("reload", "Reload configuration").globals(SERVER_FLAGS),
            command("pair", "Print one-time links to connect a browser or app").params(vec![
                flag("url", Kind::Str)
                    .check(Check::PairingUrl)
                    .describe("Use an external HTTP(S) server URL in pairing links")
                    .optional(),
                flag("remote", Kind::Bool)
                    .describe(
                        "Pair through the OpenTunnel remote address, enabling remote access if needed (exposes the server to the internet)",
                    )
                    .default("false"),
                flag("yes", Kind::Bool)
                    .alias("y")
                    .describe(
                        "Accept exposing the server to the internet when --remote enables remote access for the first time (required without an interactive terminal)",
                    )
                    .default("false"),
            ]),
            command("serve", "Start the v2 API and web server").params(vec![
                flag("hostname", Kind::Str).optional(),
                flag("port", Kind::Int).optional(),
                flag("cors", Kind::Str)
                    .check(Check::NonEmpty)
                    .describe("Additional allowed CORS origin (repeat for multiple origins)")
                    .many(Some(0), None),
                flag("service", Kind::Bool).default("false"),
                flag("stdio", Kind::Bool).default("false"),
            ]),
        ])
    }
}
