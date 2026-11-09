//! A location's config documents (packages/core/src/config.ts, config/discovery.ts, config/variable.ts,
//! config/normalize.ts): which sources load, in which order, how each document is substituted, parsed, normalized and
//! validated. `GET /api/config` answers with these entries as they are.
//!
//! Sources, lowest priority first: managed policies, the global config directory (`$XDG_CONFIG_HOME/opencode`, or
//! `OPENCODE_CONFIG_DIR`), the explicit file (`OPENCODE_CONFIG`), the project files `opencode.json` / `opencode.jsonc`
//! of every directory from the filesystem root down to the location, the project `.opencode` directories, the
//! `OPENCODE_CONFIG_CONTENT` document, and the managed settings of the system directory (`/etc/opencode`). A config
//! directory contributes its documents and then a `directory` entry (markdown agents, commands and skills are read from
//! there). `OPENCODE_DISABLE_PROJECT_CONFIG` skips the project files and directories.
//!
//! A document is rejected as a whole (and left out) when it is not valid JSONC or its root is not an object; a
//! recognized value that does not decode is dropped and the rest of the document kept. Only the current (v2) format is
//! read: legacy v1 keys (`agent`, `mode`, `command`, `provider`, `permission`, `tools`, `plugin`, `snapshot`,
//! `autoupdate`, `autoshare`, `attachment`, `reference`, `small_model`, `enabled_providers`, `disabled_providers`, an
//! MCP server listed directly under `mcp`) are out of scope and ignored.
//!
//! Documents hold the plain (config file) side of `Config.Info` ([`t::ConfigInfoPlain`]); [`wire`] writes the entries
//! as the HTTP API's JSON codec does.
//!
//! TODO(port): wellknown and integration documents (Console), macOS managed preferences, the file watcher on config
//! roots; `~` in `{file:~/...}` expands with `HOME` (TS uses `os.homedir()`, which reads the password database).

use std::sync::OnceLock;

use opencode_protocol::types as t;
use opencode_protocol::wire::Object;
use opencode_protocol::{Json, Wire};

use super::jsonc;
use crate::json::Obj;
use crate::project::{absolutize, normalize, resolve_path};

/// The config file names of a config directory and of every project directory.
pub const NAMES: [&str; 2] = ["opencode.json", "opencode.jsonc"];

/// The platform system configuration directory (`ConfigDiscovery.systemDirectory`).
pub const SYSTEM: &str = "/etc/opencode";

/// One config entry (`Config.Entry`).
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Document { path: Option<String>, info: t::ConfigInfoPlain },
    Directory(String),
}

/// The server's config options, read once from the environment as `opencode serve` reads them
/// (packages/cli/src/server-process.ts, packages/util/src/global.ts).
#[derive(Debug)]
pub struct Options {
    pub project: bool,
    pub claude: bool,
    pub file: Option<String>,
    pub content: Option<String>,
    /// `Global.Path.config`.
    pub global: String,
    /// `Global.Path.home`.
    pub home: String,
    /// `Global.Path.cache`.
    pub cache: String,
    /// `Global.Path.repos` (repository checkouts).
    pub repos: String,
}

fn truthy(value: Option<String>) -> bool {
    value.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// `os.homedir()` as the server sees it.
fn homedir() -> String {
    env("HOME").unwrap_or_default()
}

impl Options {
    pub fn get() -> &'static Options {
        static OPTIONS: OnceLock<Options> = OnceLock::new();
        OPTIONS.get_or_init(|| {
            let home = homedir();
            let xdg = |name: &str, fallback: &str| {
                env(name).filter(|v| !v.is_empty()).unwrap_or_else(|| format!("{home}/{fallback}"))
            };
            Options {
                project: !truthy(env("OPENCODE_CONFIG_PROJECT_DISABLE").or_else(|| env("OPENCODE_DISABLE_PROJECT_CONFIG"))),
                claude: !(truthy(env("OPENCODE_DISABLE_CLAUDE_CODE")) || truthy(env("OPENCODE_DISABLE_CLAUDE_CODE_SKILLS"))),
                file: env("OPENCODE_CONFIG"),
                content: env("OPENCODE_CONFIG_CONTENT"),
                global: env("OPENCODE_CONFIG_DIR")
                    .unwrap_or_else(|| normalize(&format!("{}/opencode", xdg("XDG_CONFIG_HOME", ".config")))),
                home: env("OPENCODE_TEST_HOME").unwrap_or_else(|| home.clone()),
                cache: normalize(&format!("{}/opencode", xdg("XDG_CACHE_HOME", ".cache"))),
                repos: normalize(&format!("{}/opencode/repos", xdg("XDG_DATA_HOME", ".local/share"))),
            }
        })
    }
}

/// Eligible sources in priority order (`ConfigDiscovery.Sources`).
#[derive(Debug, Clone, PartialEq)]
pub struct Sources {
    pub system: (String, bool),
    pub global: String,
    pub explicit: Option<String>,
    pub direct: Vec<String>,
    /// Project `.opencode` directories (and whether each exists), root-most first.
    pub project: Vec<(String, bool)>,
    /// Compatibility roots: `~/.claude` and project `.claude` directories.
    pub claude: Vec<String>,
    /// `~/.agents` and project `.agents` directories.
    pub agents: Vec<String>,
}

pub fn join(directory: &str, name: &str) -> String {
    normalize(&format!("{directory}/{name}"))
}

pub fn is_dir(path: &str) -> bool {
    std::path::Path::new(path).is_dir()
}

pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn dirname(path: &str) -> String {
    match path.rfind('/') {
        Some(0) => "/".to_owned(),
        Some(i) => path[..i].to_owned(),
        None => ".".to_owned(),
    }
}

/// `ConfigDiscovery.discover` for a location directory.
pub fn discover(directory: &str) -> Sources {
    let options = Options::get();
    let global_directory = options.global.clone();
    let home_agents = join(&options.home, ".agents");
    let home_claude = join(&options.home, ".claude");
    let config_directories = [SYSTEM.to_owned(), global_directory.clone()];
    let global_roots: Vec<String> =
        [SYSTEM, &global_directory, &home_claude, &home_agents].iter().map(|p| resolve_path(p)).collect();
    let location = normalize(directory);
    let current = resolve_path(directory);
    let mut directories: Vec<String> = vec![];
    if !global_roots[..2].contains(&current) && options.project {
        // `fs.up({ targets: ["."] })`: the location and each of its ancestors.
        let mut at = location.clone();
        loop {
            if std::path::Path::new(&at).exists() {
                directories.push(at.clone());
            }
            if at == "/" {
                break;
            }
            at = dirname(&at);
        }
    }
    let candidates = [".claude", ".agents", ".opencode", "opencode.jsonc", "opencode.json"];
    struct Found {
        item: String,
        resolved: String,
        missing: bool,
    }
    let mut discovered: Vec<Found> = vec![];
    for dir in &directories {
        let parent = if *dir == location { current.clone() } else { resolve_path(dir) };
        // One listing skips realpath on absent candidates (names compare case-insensitively).
        let entries: Option<Vec<String>> = std::fs::read_dir(&parent)
            .ok()
            .map(|list| list.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_lowercase()).collect());
        for name in candidates {
            let item = join(dir, name);
            if entries.as_ref().is_some_and(|entries| !entries.iter().any(|e| e == name)) {
                discovered.push(Found { item, resolved: join(&parent, name), missing: true });
            } else {
                discovered.push(Found { item, resolved: resolve_path(&join(&parent, name)), missing: false });
            }
        }
    }
    let global_files: Vec<String> =
        config_directories.iter().flat_map(|d| NAMES.iter().map(move |n| resolve_path(&join(d, n)))).collect();
    // Global sources must not re-enter through the project walk.
    let visible: Vec<&Found> =
        discovered.iter().filter(|e| !global_roots.contains(&e.resolved) && !global_files.contains(&e.resolved)).collect();
    let compat = |name: &str, home: &str| {
        let mut out = vec![home.to_owned()];
        for e in visible.iter().rev().filter(|e| basename(&e.item) == name) {
            if !out.contains(&e.item) {
                out.push(e.item.clone());
            }
        }
        out
    };
    Sources {
        system: (SYSTEM.to_owned(), is_dir(SYSTEM)),
        global: global_directory,
        explicit: options.file.as_ref().map(|f| absolutize(f)),
        direct: visible
            .iter()
            .rev()
            .filter(|e| ![".agents", ".claude", ".opencode"].contains(&basename(&e.item)))
            .map(|e| e.item.clone())
            .collect(),
        project: visible
            .iter()
            .rev()
            .filter(|e| basename(&e.item) == ".opencode")
            .map(|e| (e.item.clone(), !e.missing && is_dir(&e.item)))
            .collect(),
        claude: if options.claude { compat(".claude", &home_claude) } else { vec![] },
        agents: compat(".agents", &home_agents),
    }
}

/// `FSUtil.readFileStringSafe`: a missing or unreadable file is `None`. The text is decoded as `TextDecoder` does:
/// invalid UTF-8 becomes U+FFFD and a leading byte order mark is dropped.
pub fn read(path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    Some(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    })
}

/// JS whitespace and line terminators (`\s`).
pub fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{b}'
            | '\u{c}'
            | '\r'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// `String.prototype.trim`.
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(js_space)
}

/// `JSON.stringify(text).slice(1, -1)`.
fn escape(text: &str) -> String {
    let mut out = String::new();
    opencode_protocol::wire::out::write_json_string(&mut out, text);
    out[1..out.len() - 1].to_owned()
}

/// `ConfigVariable.substitute`: `{env:NAME}` and `{file:path}` tokens. `dir` resolves relative file references.
pub fn substitute(text: &str, dir: &str) -> String {
    let env_pass = |escaped: bool| {
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("{env:") {
            let after = &rest[start + 5..];
            match after.find('}') {
                Some(end) if end > 0 => {
                    out.push_str(&rest[..start]);
                    let value = std::env::var(&after[..end]).unwrap_or_default();
                    out.push_str(&if escaped { escape(&value) } else { value });
                    rest = &after[end + 1..];
                }
                _ => {
                    out.push_str(&rest[..start + 5]);
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    };
    let raw = substitute_files(&env_pass(false), dir);
    // Escape only when the raw values break the JSON (e.g. Windows paths), so configs that parse keep their meaning.
    if !text.contains("{env:") || jsonc::parse(&raw).is_some() {
        return raw;
    }
    substitute_files(&env_pass(true), dir)
}

fn substitute_files(text: &str, dir: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;
    let mut search = 0;
    while let Some(found) = text[search..].find("{file:") {
        let index = search + found;
        let Some(close) = text[index + 6..].find('}').filter(|&n| n > 0) else {
            search = index + 6;
            continue;
        };
        let end = index + 6 + close + 1;
        let token = &text[index..end];
        out.push_str(&text[cursor..index]);
        let line_start = text[..index].rfind('\n').map(|i| i + 1).unwrap_or(0);
        if text[line_start..index].trim_start_matches(js_space).starts_with("//") {
            out.push_str(token);
        } else {
            let file = &token[6..token.len() - 1];
            let expanded = match file.strip_prefix("~/") {
                Some(rest) => format!("{}/{rest}", homedir()),
                None => file.to_owned(),
            };
            let resolved = if expanded.starts_with('/') { expanded } else { normalize(&format!("{dir}/{expanded}")) };
            // A missing or unreadable file substitutes empty, like a missing variable.
            let content = read(&resolved).unwrap_or_default();
            out.push_str(&escape(js_trim(&content)));
        }
        cursor = end;
        search = end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// `Config.parseInfo`: JSONC, normalized, then validated as `Config.Info`. `None` rejects the document.
pub fn parse_info(text: &str) -> Option<t::ConfigInfoPlain> {
    let input = jsonc::parse(text)?;
    let encoded = normalize_document(&input)?;
    t::ConfigInfoPlain::decode(&Json::Object(encoded)).ok()
}

fn load_file(path: &str) -> Option<Entry> {
    let text = read(path)?;
    let info = parse_info(&substitute(&text, &dirname(path)))?;
    Some(Entry::Document { path: Some(path.to_owned()), info })
}

fn load_directory(directory: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = NAMES.iter().filter_map(|name| load_file(&join(directory, name))).collect();
    out.push(Entry::Directory(directory.to_owned()));
    out
}

/// `Config.load`: every entry, lowest priority first.
pub fn load(directory: &str, sources: &Sources) -> Vec<Entry> {
    let options = Options::get();
    let direct: Vec<Entry> = sources.direct.iter().filter_map(|f| load_file(f)).collect();
    let explicit: Vec<Entry> = sources.explicit.iter().filter_map(|f| load_file(f)).collect();
    let content: Vec<Entry> = options
        .content
        .as_ref()
        .and_then(|text| parse_info(&substitute(text, directory)))
        .map(|info| Entry::Document { path: None, info })
        .into_iter()
        .collect();
    let managed = if sources.system.1 { load_directory(&sources.system.0) } else { vec![] };
    // Managed policies load first (policies evaluate earliest-loaded first), the rest of the managed settings last.
    let mut managed_policies: Vec<Entry> = managed
        .iter()
        .filter_map(|entry| match entry {
            Entry::Document { path, info } if info.policies.is_some() => Some(Entry::Document {
                path: path.clone(),
                info: t::ConfigInfoPlain { policies: info.policies.clone(), ..empty_info() },
            }),
            _ => None,
        })
        .collect();
    managed_policies.reverse();
    let managed_settings = managed.into_iter().map(|entry| match entry {
        Entry::Document { path, mut info } => {
            info.policies = None;
            Entry::Document { path, info }
        }
        other => other,
    });
    let mut out = managed_policies;
    out.extend(load_directory(&sources.global));
    out.extend(explicit);
    out.extend(direct);
    for (path, _) in sources.project.iter().filter(|(_, present)| *present) {
        out.extend(load_directory(path));
    }
    out.extend(content);
    out.extend(managed_settings);
    out
}

pub fn empty_info() -> t::ConfigInfoPlain {
    t::ConfigInfoPlain::decode(&Json::Object(Object::new())).expect("an empty config is valid")
}

/// The documents of a list of entries, in order.
pub fn documents(entries: &[Entry]) -> impl DoubleEndedIterator<Item = &t::ConfigInfoPlain> {
    entries.iter().filter_map(|e| match e {
        Entry::Document { info, .. } => Some(info),
        Entry::Directory(_) => None,
    })
}

/// The config directories of a list of entries, in order.
pub fn directories(entries: &[Entry]) -> impl Iterator<Item = &str> {
    entries.iter().filter_map(|e| match e {
        Entry::Directory(path) => Some(path.as_str()),
        Entry::Document { .. } => None,
    })
}

/// `Config.latest`: the value of the last document that sets it.
pub fn latest<'a, T: ?Sized>(entries: &'a [Entry], key: impl Fn(&'a t::ConfigInfoPlain) -> Option<&'a T>) -> Option<&'a T> {
    documents(entries).rev().find_map(key)
}

/// The entries as `GET /api/config` writes them (`Config.Entry[]` under the JSON codec).
pub fn wire(entries: &[Entry]) -> Json {
    let items: Vec<Json> = entries
        .iter()
        .map(|entry| {
            let mut o = Obj::new();
            match entry {
                Entry::Document { path, info } => {
                    o.str("type", "document").opt_str("path", path.as_deref()).set("info", crate::json::of(info));
                }
                Entry::Directory(path) => {
                    o.str("type", "directory").str("path", path);
                }
            }
            o.done()
        })
        .collect();
    let plain = Json::Array(items);
    match t::ConfigGetResponse::decode(&plain) {
        Ok(response) => crate::json::of(&response),
        Err(_) => plain,
    }
}

/// `Config.update`: sets (or with `None` clears) the global config's `shell`, keeping the rest of the file. The file
/// is the last existing `opencode.json` / `opencode.jsonc` of the global directory, else a new `opencode.jsonc`.
/// `Err` when the edit is impossible (the TS server dies on it: 500).
pub fn update_shell(shell: Option<&str>) -> Result<(), String> {
    let directory = &Options::get().global;
    let path = NAMES
        .iter()
        .map(|name| join(directory, name))
        .filter(|p| std::path::Path::new(p).is_file())
        .last()
        .unwrap_or_else(|| join(directory, "opencode.jsonc"));
    let text = read(&path).unwrap_or_else(|| "{}\n".to_owned());
    let value = shell.map(|s| Json::String(s.to_owned()));
    let updated = jsonc::set_root_property(&text, "shell", value.as_ref()).ok_or("config.update")?;
    let updated = if updated.ends_with('\n') { updated } else { format!("{updated}\n") };
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    std::fs::write(&path, updated).map_err(|e| e.to_string())
}

// ---- normalization (ConfigNormalize.normalize, v2 keys) --------------------------------------------------------

fn is_record(v: Option<&Json>) -> bool {
    matches!(v, Some(Json::Object(_)))
}

fn is_array(v: Option<&Json>) -> bool {
    matches!(v, Some(Json::Array(_)))
}

/// `decodeEncoded`: the canonical encoding of a value of a wire type, or `None` when it does not decode.
pub fn canonical<T: Wire>(v: &Json) -> Option<Json> {
    let value = T::decode(v).ok()?;
    Some(crate::json::of(&value))
}

/// `decodeEncoded` of one field of a struct whose fields are all optional: decodes `{key: value}` as `T`.
fn field<T: Wire>(key: &str, v: &Json) -> Option<Json> {
    let mut o = Object::new();
    o.insert(key.to_owned(), v.clone());
    let encoded = canonical::<T>(&Json::Object(o))?;
    encoded.as_object().and_then(|o| o.get(key)).cloned()
}

/// `decodeMap`: the entries of a record that decode, in JS key order.
fn decode_map(v: Option<&Json>, decode: impl Fn(&Json) -> Option<Json>) -> Object {
    let mut out = Object::new();
    if let Some(Json::Object(o)) = v {
        for key in opencode_protocol::wire::out::js_key_order(o.keys().map(String::as_str)) {
            if let Some(value) = decode(&o[key]) {
                out.insert(key.to_owned(), value);
            }
        }
    }
    out
}

/// `decodeList`: the items of an array that decode.
fn decode_list(v: Option<&Json>, decode: impl Fn(&Json) -> Option<Json>) -> Vec<Json> {
    match v {
        Some(Json::Array(items)) => items.iter().filter_map(decode).collect(),
        _ => vec![],
    }
}

fn string(v: &Json) -> Option<Json> {
    matches!(v, Json::String(_)).then(|| v.clone())
}

/// One policy, decoded as `Config.Info` decodes the items of `policies` (with the legacy resource hook).
fn policy(v: &Json) -> Option<Json> {
    match field::<t::ConfigInfoPlain>("policies", &Json::Array(vec![v.clone()]))? {
        Json::Array(mut items) if items.len() == 1 => items.pop(),
        _ => None,
    }
}

/// One MCP server, decoded as `Config.MCP` decodes `servers`.
fn mcp_server(v: &Json) -> Option<Json> {
    let mut servers = Object::new();
    servers.insert("x".into(), v.clone());
    let servers = field::<t::ConfigMCPPlain>("servers", &Json::Object(servers))?;
    servers.as_object()?.get("x").cloned()
}

/// `ConfigNormalize.normalize` for the current format: `None` when the root is not an object.
pub fn normalize_document(input: &Json) -> Option<Object> {
    let Json::Object(input) = input else { return None };
    let get = |key: &str| input.get(key);
    let mut encoded = Object::new();
    let mut set = |key: &str, value: Json| {
        encoded.insert(key.to_owned(), value);
    };

    if let Some(v) = get("update").and_then(|v| field::<t::ConfigInfoPlain>("update", v)) {
        set("update", v);
    }
    if is_record(get("references")) {
        set("references", Json::Object(decode_map(get("references"), canonical::<t::ConfigInfoReferencesValue>)));
    }
    if is_record(get("commands")) {
        set("commands", Json::Object(decode_map(get("commands"), canonical::<t::ConfigCommand>)));
    }
    if is_record(get("agents")) {
        set("agents", Json::Object(decode_map(get("agents"), canonical::<t::ConfigAgent>)));
    }
    if is_record(get("providers")) {
        set("providers", Json::Object(decode_map(get("providers"), canonical::<t::ConfigProviderPlain>)));
    }
    let permissions = decode_list(get("permissions"), canonical::<t::PermissionRule>);
    if !permissions.is_empty() || is_array(get("permissions")) {
        set("permissions", Json::Array(permissions));
    }
    let plugins = decode_list(get("plugins"), canonical::<t::ConfigInfoPluginsItem>);
    if !plugins.is_empty() || is_array(get("plugins")) {
        set("plugins", Json::Array(plugins));
    }

    // Skills: a list of paths and URLs, or `{paths, urls}`.
    match get("skills") {
        Some(Json::Array(_)) => set("skills", Json::Array(decode_list(get("skills"), string))),
        Some(Json::Object(skills)) => {
            let mut list = decode_list(skills.get("paths"), string);
            list.extend(decode_list(skills.get("urls"), string));
            set("skills", Json::Array(list));
        }
        _ => {}
    }

    // MCP: `servers` and `timeout` (and the deprecated `experimental.mcp_timeout`).
    let mut timeout = Object::new();
    if let Some(Json::Object(experimental)) = get("experimental") {
        if let Some(v) = experimental.get("mcp_timeout").and_then(|v| field::<t::McpTimeoutConfig>("catalog", v)) {
            timeout.insert("catalog".into(), v.clone());
            timeout.insert("execution".into(), v);
        }
    }
    let mut servers = Object::new();
    if let Some(Json::Object(mcp)) = get("mcp") {
        for (name, value) in mcp {
            let direct = matches!(value, Json::Object(o) if matches!(o.get("type"), Some(Json::String(s)) if s == "local" || s == "remote"));
            let enabled_only = matches!(value, Json::Object(o) if !o.contains_key("type") && matches!(o.get("enabled"), Some(Json::Bool(_))));
            if enabled_only {
                continue;
            }
            if name == "servers" && !direct {
                for (key, server) in decode_map(Some(&expand_server_timeouts(value)), mcp_server) {
                    servers.insert(key, server);
                }
            } else if name == "timeout" && !direct {
                let value = match value {
                    Json::Number(_) => legacy_timeout(value),
                    other => other.clone(),
                };
                if let Json::Object(value) = value {
                    let recognized: Vec<&String> =
                        value.keys().filter(|k| ["startup", "catalog", "execution"].contains(&k.as_str())).collect();
                    if value.is_empty() || !recognized.is_empty() {
                        for key in recognized {
                            if let Some(leaf) = field::<t::McpTimeoutConfig>(key, &value[key.as_str()]) {
                                timeout.insert(key.clone(), leaf);
                            }
                        }
                    }
                }
            }
            // Anything else is a legacy server: out of scope.
        }
    }
    if servers.is_empty() && timeout.is_empty() {
        if matches!(get("mcp"), Some(Json::Object(o)) if o.is_empty()) {
            set("mcp", Json::Object(Object::new()));
        }
    } else {
        let mut mcp = Object::new();
        if !timeout.is_empty() {
            mcp.insert("timeout".into(), Json::Object(timeout));
        }
        if !servers.is_empty() {
            mcp.insert("servers".into(), Json::Object(servers));
        }
        set("mcp", Json::Object(mcp));
    }

    // Compaction.
    if let Some(Json::Object(compaction)) = get("compaction") {
        let mut result = Object::new();
        if let Some(v) = compaction.get("auto").and_then(|v| field::<t::ConfigCompaction>("auto", v)) {
            result.insert("auto".into(), v);
        }
        if let Some(Json::Object(keep)) = compaction.get("keep") {
            if let Some(tokens) = keep.get("tokens").and_then(|v| field::<t::ConfigCompactionKeep>("tokens", v)) {
                let mut keep = Object::new();
                keep.insert("tokens".into(), tokens);
                result.insert("keep".into(), Json::Object(keep));
            }
        }
        if let Some(v) = compaction.get("buffer").and_then(|v| field::<t::ConfigCompaction>("buffer", v)) {
            result.insert("buffer".into(), v);
        }
        if !result.is_empty() || compaction.is_empty() {
            set("compaction", Json::Object(result));
        }
    }

    // Policies: `experimental.policies` (deprecated), then top-level `policies`.
    let (experimental_policies, experimental_array) = match get("experimental") {
        Some(Json::Object(e)) => (decode_list(e.get("policies"), policy), is_array(e.get("policies"))),
        _ => (vec![], false),
    };
    let native_policies = decode_list(get("policies"), policy);
    if !experimental_policies.is_empty() || !native_policies.is_empty() || is_array(get("policies")) || experimental_array {
        let mut all = experimental_policies;
        all.extend(native_policies);
        set("policies", Json::Array(all));
    }

    // Experimental flags.
    if let Some(Json::Object(experimental)) = get("experimental") {
        let mut result = Object::new();
        for (key, value) in experimental {
            if key == "policies" {
                continue;
            }
            if let Some(v) = field::<t::ConfigExperimentalInfo>(key, value) {
                result.insert(key.clone(), v);
            }
        }
        if !experimental.contains_key("codemode_vm") {
            if let Some(v) =
                experimental.get("codemodeVm").and_then(|v| field::<t::ConfigExperimentalInfo>("codemode_vm", v))
            {
                result.insert("codemode_vm".into(), v);
            }
        }
        if !result.is_empty() || experimental.is_empty() {
            set("experimental", Json::Object(result));
        }
    }

    // Watcher.
    if let Some(Json::Object(watcher)) = get("watcher") {
        let ignore = decode_list(watcher.get("ignore"), string);
        let mut result = Object::new();
        if !ignore.is_empty() || is_array(watcher.get("ignore")) {
            result.insert("ignore".into(), Json::Array(ignore));
        }
        set("watcher", Json::Object(result));
    }

    // Formatters and language servers: `true` / `false`, or named entries.
    for key in ["formatter", "lsp"] {
        match get(key) {
            Some(v @ Json::Bool(_)) => {
                if let Some(v) = field::<t::ConfigInfoPlain>(key, v) {
                    set(key, v);
                }
            }
            Some(Json::Object(o)) => {
                let entries = decode_map(get(key), |v| {
                    if key == "formatter" {
                        canonical::<t::ConfigFormatterEntry>(v)
                    } else {
                        canonical::<t::ConfigInfoLsp1Value>(v)
                    }
                });
                if o.is_empty() || !entries.is_empty() {
                    set(key, Json::Object(entries));
                }
            }
            _ => {}
        }
    }

    // The default model (`provider/model#variant` or the object form).
    if let Some(Json::Object(mut model)) = get("model").and_then(|v| field::<t::ConfigInfoPlain>("model", v)) {
        if let Some(Json::String(provider)) = model.get("providerID") {
            let migrated = match provider.as_str() {
                "azure-cognitive-services" => "azure",
                "google-vertex-anthropic" => "google-vertex",
                other => other,
            }
            .to_owned();
            model.insert("providerID".into(), Json::String(migrated));
        }
        set("model", Json::Object(model));
    }

    for key in [
        "$schema",
        "shell",
        "default_agent",
        "share",
        "enterprise",
        "username",
        "timezone",
        "snapshots",
        "media",
        "tool_output",
        "websearch",
        "worktree",
        "warming",
    ] {
        if let Some(v) = get(key).and_then(|v| field::<t::ConfigInfoPlain>(key, v)) {
            set(key, v);
        }
    }

    let instructions = decode_list(get("instructions"), string);
    if !instructions.is_empty() || is_array(get("instructions")) {
        set("instructions", Json::Array(instructions));
    }
    Some(encoded)
}

/// `ConfigMigrateV1.migrateMcpTimeout`: a bare number is every timeout.
fn legacy_timeout(n: &Json) -> Json {
    let mut timeout = Object::new();
    for key in ["startup", "catalog", "execution"] {
        timeout.insert(key.into(), n.clone());
    }
    Json::Object(timeout)
}

/// Native MCP servers accept the v1 bare-number timeout so one v1-shaped field does not drop the whole server.
fn expand_server_timeouts(value: &Json) -> Json {
    let Json::Object(servers) = value else { return value.clone() };
    let mut out = Object::new();
    for (name, server) in servers {
        let server = match server {
            Json::Object(s) if matches!(s.get("timeout"), Some(Json::Number(_))) => {
                let mut s = s.clone();
                let timeout = legacy_timeout(&s["timeout"]);
                s.insert("timeout".into(), timeout);
                Json::Object(s)
            }
            other => other.clone(),
        };
        out.insert(name.clone(), server);
    }
    Json::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_invalid_fields_and_keeps_valid_ones() {
        let text = r#"{"username": 42, "share": "sometimes", "timezone": "Asia/Tokyo", "instructions": ["GOOD.md", 7],
            "agents": {"good": {"description": "Kept", "mode": "subagent"}, "bad": {"mode": "sideways"}}}"#;
        let info = parse_info(text).expect("a valid document");
        assert_eq!(
            crate::json::text(&crate::json::of(&info)),
            r#"{"timezone":"Asia/Tokyo","agents":{"good":{"description":"Kept","mode":"subagent"}},"instructions":["GOOD.md"]}"#
        );
        assert!(parse_info("[1, 2, 3]").is_none());
        assert!(parse_info("{ \"username\": \"broken\", ").is_none());
    }
}
