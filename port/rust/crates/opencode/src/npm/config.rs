//! The npm config an install runs with (`NpmConfig.load(dir)`: @npmcli/config 10.8.1 with `--prefix <dir>`), as
//! far as it decides what is fetched and installed: the layers (defaults, the global `<prefix>/etc/npmrc`, the user
//! `~/.npmrc`, the project `<dir>/.npmrc`, `npm_config_*` variables, the CLI prefix) with their precedence, ini 6.0.0's
//! parsing, `${VAR}` substitution and the field coercions of `parseField`, then the flattened options arborist,
//! pacote and npm-registry-fetch read: the registry and scoped registries, credentials (`//host/path/:_authToken`,
//! `:_auth`, `:username` + `:_password`), `before`, the peer dependency modes, `install-strategy` (and the deprecated
//! `global-style` / `legacy-bundling`), `prefer-dedupe`, `engine-strict`, `force`, `omit` / `include` / `optional`,
//! `os` / `cpu` / `libc`, `replace-registry-host` and `umask`.
//!
//! The transport options (`proxy`, `https-proxy`, `noproxy`, `strict-ssl`, `ca`, `cafile`, `cert`/`key`, timeouts,
//! retries, `maxsockets`, the `cache` and `offline` modes) are not read: the port's HTTP client takes its proxy and
//! roots from the server's environment like every other outbound request of the server, and keeps no HTTP cache.

use std::path::Path;

use indexmap::IndexMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Bool(bool),
    Null,
    /// A list field (`omit[]=dev`, or a list type's value split on blank lines).
    List(Vec<Value>),
    /// An ini section, which no supported key reads.
    Section,
}

impl Value {
    /// JS truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Str(s) => !s.is_empty(),
            Value::Bool(b) => *b,
            Value::Null => false,
            Value::List(_) | Value::Section => true,
        }
    }

    pub fn text(&self) -> Option<String> {
        match self {
            Value::Str(s) => Some(s.clone()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Null => Some("null".into()),
            Value::List(items) => Some(items.iter().filter_map(Value::text).collect::<Vec<_>>().join(",")),
            Value::Section => None,
        }
    }
}

type Layer = IndexMap<String, Value>;

/// The type of a definition, as far as `parseField` and the flattening use it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Type {
    /// No definition (scoped registries, nerf-darted credentials, unknown keys).
    None,
    Boolean,
    String,
    Path,
    Url,
    Date,
    Umask,
    List,
    Enum,
}

fn type_of(key: &str) -> Type {
    match key {
        "engine-strict" | "legacy-peer-deps" | "strict-peer-deps" | "force" | "prefer-dedupe" | "global-style"
        | "legacy-bundling" | "install-links" | "optional" | "production" | "dev" => Type::Boolean,
        "os" | "cpu" | "libc" | "replace-registry-host" | "only" | "also" => Type::String,
        "userconfig" | "globalconfig" | "prefix" | "cache" => Type::Path,
        "registry" => Type::Url,
        "before" => Type::Date,
        "umask" => Type::Umask,
        "omit" | "include" => Type::List,
        "install-strategy" => Type::Enum,
        _ => Type::None,
    }
}

/// ini 6.0.0's `unsafe(val)`: quotes removed (and JSON-unescaped), else cut at the first unescaped `;` or `#`.
fn unsafe_value(val: &str) -> String {
    let val = val.trim();
    let quoted = val.len() >= 2 && ((val.starts_with('"') && val.ends_with('"')) || (val.starts_with('\'') && val.ends_with('\'')));
    if quoted || val == "\"" || val == "'" {
        let inner = if val.starts_with('\'') && val.len() >= 2 { &val[1..val.len() - 1] } else { val };
        return match crate::json::parse(inner) {
            Some(opencode_protocol::Json::String(s)) => s,
            Some(other) => crate::json::text(&other),
            None => inner.to_owned(),
        };
    }
    let mut out = String::new();
    let mut escaped = false;
    for c in val.chars() {
        if escaped {
            if "\\;#".contains(c) {
                out.push(c);
            } else {
                out.push('\\');
                out.push(c);
            }
            escaped = false;
        } else if c == ';' || c == '#' {
            break;
        } else if c == '\\' {
            escaped = true;
        } else {
            out.push(c);
        }
    }
    if escaped {
        out.push('\\');
    }
    out.trim().to_owned()
}

/// ini 6.0.0's `decode` (top-level keys; a `[section]` takes the keys after it).
pub fn decode(text: &str) -> Layer {
    let mut out = Layer::new();
    let mut in_section = false;
    for line in text.split(['\r', '\n']) {
        let trimmed = line.trim_start();
        if line.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        // `^\[([^\]]*)\]\s*$`
        if let Some(rest) = line.strip_prefix('[') {
            if let Some(close) = rest.find(']') {
                if rest[close + 1..].trim().is_empty() && !rest[..close].contains(']') {
                    let section = unsafe_value(&rest[..close]);
                    if section != "__proto__" {
                        out.insert(section, Value::Section);
                    }
                    in_section = true;
                    continue;
                }
            }
        }
        // `^([^=]+)(=(.*))?$`
        let (key_raw, value_raw) = match line.find('=') {
            Some(0) => continue,
            Some(at) => (&line[..at], Some(&line[at + 1..])),
            None => (line, None),
        };
        if in_section {
            continue;
        }
        let key_raw = unsafe_value(key_raw);
        let is_array = key_raw.len() > 2 && key_raw.ends_with("[]");
        let key = if is_array { key_raw[..key_raw.len() - 2].to_owned() } else { key_raw };
        if key == "__proto__" {
            continue;
        }
        let value = match value_raw {
            Some(raw) => {
                let raw = unsafe_value(raw);
                match raw.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    "null" => Value::Null,
                    _ => Value::Str(raw),
                }
            }
            None => Value::Bool(true),
        };
        if is_array {
            match out.get_mut(&key) {
                Some(Value::List(items)) => items.push(value),
                Some(other) => {
                    let first = std::mem::replace(other, Value::Null);
                    *other = Value::List(vec![first, value]);
                }
                None => {
                    out.insert(key, Value::List(vec![value]));
                }
            }
        } else if let Some(Value::List(items)) = out.get_mut(&key) {
            items.push(value);
        } else {
            out.insert(key, value);
        }
    }
    out
}

/// `envReplace`: `${NAME}` (and `${NAME?}`, empty when unset), with backslash escapes.
pub fn env_replace(text: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
    let bytes: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        // `(?<!\\)(\\*)\$\{([^${}?]+)(\?)?\}`: a run of backslashes not preceded by one, then the expression.
        let start = i;
        let mut j = i;
        while j < bytes.len() && bytes[j] == '\\' {
            j += 1;
        }
        let preceded = start > 0 && bytes[start - 1] == '\\';
        if !preceded && j + 1 < bytes.len() && bytes[j] == '$' && bytes[j + 1] == '{' {
            let mut k = j + 2;
            while k < bytes.len() && !"${}?".contains(bytes[k]) {
                k += 1;
            }
            let name: String = bytes[j + 2..k].iter().collect();
            let optional = k < bytes.len() && bytes[k] == '?';
            let close = if optional { k + 1 } else { k };
            if !name.is_empty() && close < bytes.len() && bytes[close] == '}' {
                let escapes = j - start;
                let original: String = bytes[start..=close].iter().collect();
                if escapes % 2 == 1 {
                    out.push_str(&original[(escapes + 1) / 2..]);
                } else {
                    let fallback = if optional { String::new() } else { format!("${{{name}}}") };
                    out.push_str(&"\\".repeat(escapes / 2));
                    out.push_str(&env(&name).unwrap_or(fallback));
                }
                i = close + 1;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}


fn host_with_port(url: &url::Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        _ => String::new(),
    }
}

/// The options an install reads (`config.flat`).
#[derive(Debug, Clone)]
pub struct Flat {
    /// The configured registry (arborist gives it exactly one trailing slash).
    pub registry: String,
    /// `@scope:registry` and `//…` keys, verbatim.
    pub verbatim: IndexMap<String, Value>,
    /// `before`, as a time (NaN for an invalid date).
    pub before: Option<f64>,
    pub engine_strict: bool,
    pub legacy_peer_deps: bool,
    pub strict_peer_deps: bool,
    pub force: bool,
    pub prefer_dedupe: bool,
    pub install_strategy: String,
    pub os: Option<String>,
    pub cpu: Option<String>,
    pub libc: Option<String>,
    pub omit: Vec<String>,
    pub replace_registry_host: String,
    pub umask: u32,
}

struct Loader {
    env: Vec<(String, String)>,
    home: String,
    /// Highest priority first: cli, env, project, user, global, default.
    layers: Vec<(&'static str, Layer)>,
}

impl Loader {
    fn env_var(&self, name: &str) -> Option<String> {
        self.env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.layers.iter().find_map(|(_, layer)| layer.get(key))
    }

    fn layer_mut(&mut self, name: &str) -> &mut Layer {
        &mut self.layers.iter_mut().find(|(n, _)| *n == name).unwrap().1
    }

    /// `parseField(value, key)`.
    fn parse_field(&self, value: Value, key: &str, list_element: bool) -> Value {
        let ty = type_of(key);
        let text = match value {
            Value::Str(s) => s,
            Value::List(items) => {
                if ty == Type::List && !list_element {
                    return Value::List(items.into_iter().map(|v| self.parse_field(v, key, true)).collect());
                }
                return Value::List(items);
            }
            other => return other,
        };
        let text = text.trim().to_owned();
        if ty == Type::List && !list_element {
            let parts = text.split("\n\n").map(|p| Value::Str(p.to_owned())).collect::<Vec<_>>();
            return Value::List(parts.into_iter().map(|v| self.parse_field(v, key, true)).collect());
        }
        if ty == Type::Boolean && text.is_empty() {
            return Value::Bool(true);
        }
        if !matches!(ty, Type::String | Type::Path) {
            match text.as_str() {
                "true" => return Value::Bool(true),
                "false" => return Value::Bool(false),
                "null" => return Value::Null,
                "undefined" => return Value::Null,
                _ => {}
            }
        }
        let env = |name: &str| self.env_var(name);
        let mut text = env_replace(&text, &env);
        if ty == Type::Path {
            text = if let Some(rest) = text.strip_prefix("~/") {
                crate::project::normalize(&format!("{}/{rest}", self.home))
            } else {
                absolute(&text)
            };
        }
        Value::Str(text)
    }

    fn load_object(&mut self, layer: &'static str, object: Layer) {
        let mut parsed = Layer::new();
        for (key, value) in object {
            let env = |name: &str| self.env_var(name);
            let key = env_replace(&key, &env);
            let value = self.parse_field(value, &key, false);
            parsed.insert(key, value);
        }
        *self.layer_mut(layer) = parsed;
    }

    fn load_file(&mut self, layer: &'static str, file: &str) {
        if let Ok(text) = std::fs::read_to_string(file) {
            let object = decode(&text);
            self.load_object(layer, object);
        }
    }

    fn path_of(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        }
    }
}

fn absolute(path: &str) -> String {
    if path.starts_with('/') {
        return crate::project::normalize(path);
    }
    let cwd = std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".into());
    crate::project::normalize(&format!("{cwd}/{path}"))
}

/// `NpmConfig.load(dir)`: the flattened config of an install whose prefix is `dir`.
pub fn load(dir: &Path) -> Flat {
    let env: Vec<(String, String)> = std::env::vars().collect();
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty()).unwrap_or_else(crate::file_access::home);
    let mut loader = Loader {
        env,
        home: home.clone(),
        layers: vec![
            ("cli", Layer::new()),
            ("env", Layer::new()),
            ("project", Layer::new()),
            ("user", Layer::new()),
            ("global", Layer::new()),
            ("default", Layer::new()),
        ],
    };
    // Defaults: the global prefix (PREFIX, else two levels above the executable, under DESTDIR).
    let global_prefix = match loader.env_var("PREFIX") {
        Some(prefix) => prefix,
        None => {
            let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let prefix = crate::file_access::dirname(&crate::file_access::dirname(&exe));
            match loader.env_var("DESTDIR") {
                Some(dest) if !dest.is_empty() => crate::project::normalize(&format!("{dest}/{prefix}")),
                _ => prefix,
            }
        }
    };
    let mut defaults = Layer::new();
    defaults.insert("registry".into(), Value::Str("https://registry.npmjs.org/".into()));
    defaults.insert("userconfig".into(), Value::Str(format!("{home}/.npmrc")));
    defaults.insert("prefix".into(), Value::Str(global_prefix));
    defaults.insert("install-strategy".into(), Value::Str("hoisted".into()));
    defaults.insert("replace-registry-host".into(), Value::Str("npmjs".into()));
    *loader.layer_mut("default") = defaults;
    // `--prefix <dir>`
    let mut cli = Layer::new();
    cli.insert("prefix".into(), Value::Str(dir.to_string_lossy().into_owned()));
    *loader.layer_mut("cli") = cli;
    // npm_config_* (empty values skipped; `_` after the first character is `-`, lower case, unless nerf-darted)
    let mut env_layer = Layer::new();
    for (key, value) in loader.env.clone() {
        if value.is_empty() || key.len() < 11 || !key[..11].eq_ignore_ascii_case("npm_config_") {
            continue;
        }
        let mut name = key[11..].to_owned();
        if !name.starts_with("//") {
            let mut out = String::new();
            for (i, c) in name.chars().enumerate() {
                out.push(if c == '_' && i > 0 { '-' } else { c });
            }
            name = out.to_lowercase();
        }
        env_layer.insert(name, Value::Str(value));
    }
    loader.load_object("env", env_layer);
    // project config: `<prefix>/.npmrc`, unless it is the user config
    let project = format!("{}/.npmrc", dir.to_string_lossy());
    if Some(project.clone()) != loader.path_of("userconfig") {
        loader.load_file("project", &project);
    }
    if let Some(user) = loader.path_of("userconfig") {
        loader.load_file("user", &user);
    }
    // `globalconfig` defaults to `<prefix>/etc/npmrc` of the effective prefix (the CLI's).
    let global = loader
        .path_of("globalconfig")
        .unwrap_or_else(|| format!("{}/etc/npmrc", loader.path_of("prefix").unwrap_or_default()));
    loader.load_file("global", &global);
    flatten(&loader)
}

fn flatten(loader: &Loader) -> Flat {
    let mut flat = Flat {
        registry: "https://registry.npmjs.org/".into(),
        verbatim: IndexMap::new(),
        before: None,
        engine_strict: false,
        legacy_peer_deps: false,
        strict_peer_deps: false,
        force: false,
        prefer_dedupe: false,
        install_strategy: "hoisted".into(),
        os: None,
        cpu: None,
        libc: None,
        omit: Vec::new(),
        replace_registry_host: "npmjs".into(),
        umask: 0,
    };
    // from least priority to highest, each layer's own keys in order
    for (_, layer) in loader.layers.iter().rev() {
        for (key, value) in layer {
            match key.as_str() {
                "registry" => flat.registry = value.text().unwrap_or_default(),
                "before" => {
                    flat.before = match value {
                        Value::Null => None,
                        other => other.text().map(|t| opencode_codemode::jsdate::parse(&t.encode_utf16().collect::<Vec<u16>>())),
                    }
                }
                "engine-strict" => flat.engine_strict = value.truthy(),
                "legacy-peer-deps" => flat.legacy_peer_deps = value.truthy(),
                "strict-peer-deps" => flat.strict_peer_deps = value.truthy(),
                "force" => flat.force = value.truthy(),
                "prefer-dedupe" => flat.prefer_dedupe = value.truthy(),
                "install-strategy" => flat.install_strategy = value.text().unwrap_or_default(),
                "global-style" if value.truthy() => flat.install_strategy = "shallow".into(),
                "legacy-bundling" if value.truthy() => flat.install_strategy = "nested".into(),
                "os" => flat.os = value.text().filter(|_| value.truthy()),
                "cpu" => flat.cpu = value.text().filter(|_| value.truthy()),
                "libc" => flat.libc = value.text().filter(|_| value.truthy()),
                "replace-registry-host" => flat.replace_registry_host = value.text().unwrap_or_default(),
                "umask" => flat.umask = value.text().and_then(|t| u32::from_str_radix(t.trim_start_matches("0o"), 8).ok()).unwrap_or(0),
                "omit" | "include" | "optional" | "production" | "only" | "dev" | "also" => flat.omit = omit_list(loader),
                _ => {
                    let lower = key.to_lowercase();
                    if (lower.starts_with('@') && lower.ends_with(":registry")) || key.starts_with("//") {
                        flat.verbatim.insert(key.clone(), value.clone());
                    }
                }
            }
        }
    }
    flat
}

/// `buildOmitList(obj)` over the effective values.
fn omit_list(loader: &Loader) -> Vec<String> {
    let list = |key: &str| -> Vec<String> {
        match loader.get(key) {
            Some(Value::List(items)) => items.iter().filter_map(Value::text).collect(),
            Some(Value::Str(s)) if !s.is_empty() => vec![s.clone()],
            _ => Vec::new(),
        }
    };
    let mut include = list("include");
    let mut omit = list("omit");
    let only = loader.get("only").and_then(Value::text).unwrap_or_default();
    let production = loader.get("production");
    if only == "prod" || only == "production" || production.is_some_and(Value::truthy) {
        omit.push("dev".into());
    } else if production == Some(&Value::Bool(false)) {
        include.push("dev".into());
    }
    if loader.get("also").and_then(Value::text).is_some_and(|a| a.starts_with("dev")) {
        include.push("dev".into());
    }
    if loader.get("dev").is_some_and(Value::truthy) {
        include.push("dev".into());
    }
    match loader.get("optional") {
        Some(Value::Bool(false)) => omit.push("optional".into()),
        Some(Value::Bool(true)) => include.push("optional".into()),
        _ => {}
    }
    let mut out: Vec<String> = Vec::new();
    for item in omit {
        if !out.contains(&item) && !include.contains(&item) {
            out.push(item);
        }
    }
    out
}

impl Flat {
    /// `pickRegistry(spec, opts)`: the scope's registry, else the configured one.
    pub fn registry_for(&self, name: &str) -> String {
        if let Some(scope) = name.strip_prefix('@').and_then(|rest| rest.split_once('/')).map(|(scope, _)| format!("@{scope}:registry")) {
            if let Some(value) = self.verbatim.get(&scope).filter(|v| v.truthy()) {
                return value.text().unwrap_or_default();
            }
        }
        self.arborist_registry()
    }

    /// The registry arborist hands on: one trailing slash.
    pub fn arborist_registry(&self) -> String {
        let registry = if self.registry.is_empty() { "https://registry.npmjs.org" } else { &self.registry };
        format!("{}/", registry.trim_end_matches('/'))
    }

    fn has(&self, key: &str) -> Option<&Value> {
        self.verbatim.get(key).filter(|v| v.truthy())
    }

    /// npm-registry-fetch `getAuth(uri, opts)` → the `authorization` header it sends, if any.
    pub fn authorization(&self, uri: &str, spec_name: Option<&str>) -> Option<String> {
        let reg_from = |uri: &str| -> Option<(String, &'static str)> {
            let parsed = url::Url::parse(uri).ok()?;
            let mut key = format!("//{}{}", host_with_port(&parsed), parsed.path());
            while key.len() > 2 {
                let kind = if self.has(&format!("{key}:_authToken")).is_some() {
                    Some("_authToken")
                } else if self.has(&format!("{key}:_auth")).is_some() {
                    Some("_auth")
                } else if self.has(&format!("{key}:username")).is_some() && self.has(&format!("{key}:_password")).is_some() {
                    Some("username")
                } else {
                    None
                };
                if let Some(kind) = kind {
                    return Some((key, kind));
                }
                // `regKey.replace(/([^/]+|\/)$/, '')`
                if key.ends_with('/') {
                    key.pop();
                } else {
                    let cut = key.rfind('/').map(|i| i + 1).unwrap_or(0);
                    key.truncate(cut);
                }
            }
            None
        };
        let found = reg_from(uri);
        let key = match found {
            Some((key, _)) => key,
            None => {
                let registry = match spec_name.and_then(|n| n.strip_prefix('@')).and_then(|r| r.split_once('/')) {
                    Some((scope, _)) => self.verbatim.get(&format!("@{scope}:registry")).and_then(Value::text).unwrap_or_else(|| self.registry.clone()),
                    None => self.registry.clone(),
                };
                let same_host = |a: &str, b: &str| match (url::Url::parse(a), url::Url::parse(b)) {
                    (Ok(a), Ok(b)) => host_with_port(&a) == host_with_port(&b),
                    _ => false,
                };
                if !registry.is_empty() && uri != registry && same_host(uri, &registry) {
                    return self.authorization(&registry, spec_name);
                }
                return None;
            }
        };
        if let Some(token) = self.has(&format!("{key}:_authToken")).and_then(Value::text) {
            return Some(format!("Bearer {token}"));
        }
        if let Some(auth) = self.has(&format!("{key}:_auth")).and_then(Value::text) {
            return Some(format!("Basic {auth}"));
        }
        let username = self.has(&format!("{key}:username")).and_then(Value::text);
        let password = self.has(&format!("{key}:_password")).and_then(Value::text);
        if let (Some(username), Some(password)) = (username, password) {
            use base64::Engine;
            let engine = base64::engine::general_purpose::STANDARD;
            let decoded = engine.decode(password.trim()).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            return Some(format!("Basic {}", engine.encode(format!("{username}:{decoded}"))));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini() {
        let layer = decode("registry = https://r.test/\n; comment\n//r.test/:_authToken=\"tok\"\nomit[]=dev\nomit[]=optional\nflag\nx=a;b\n");
        assert_eq!(layer.get("registry"), Some(&Value::Str("https://r.test/".into())));
        assert_eq!(layer.get("//r.test/:_authToken"), Some(&Value::Str("tok".into())));
        assert_eq!(layer.get("omit"), Some(&Value::List(vec![Value::Str("dev".into()), Value::Str("optional".into())])));
        assert_eq!(layer.get("flag"), Some(&Value::Bool(true)));
        assert_eq!(layer.get("x"), Some(&Value::Str("a".into())));
    }

    #[test]
    fn substitution() {
        let env = |name: &str| (name == "T").then(|| "v".to_owned());
        assert_eq!(env_replace("a${T}b${U}c${U?}", &env), "avb${U}c");
        assert_eq!(env_replace("\\${T}", &env), "${T}");
        assert_eq!(env_replace("\\\\${T}", &env), "\\v");
    }

}
