//! `Global` (packages/util/src/global.ts, global-roots.ts) as the CLI acquires it: XDG roots under `opencode`,
//! `OPENCODE_CONFIG_DIR` for the config directory, the canonical tmp directory; and `databasePath`
//! (packages/cli/src/database-path.ts).

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `os.homedir()`.
pub fn home() -> String {
    env("HOME").unwrap_or_else(|| "/".into())
}

fn root(variable: &str, fallback: &str) -> String {
    format!("{}/opencode", env(variable).unwrap_or_else(|| format!("{}/{fallback}", home())))
}

pub fn data() -> String {
    root("XDG_DATA_HOME", ".local/share")
}

pub fn cache() -> String {
    root("XDG_CACHE_HOME", ".cache")
}

pub fn state() -> String {
    root("XDG_STATE_HOME", ".local/state")
}

/// `global.config`: `OPENCODE_CONFIG_DIR` replaces it (index.ts `Global.layerWith`).
pub fn config() -> String {
    crate::generate::global_config()
}

/// `os.tmpdir()/opencode`, canonical (the acquired service's `tmp`).
pub fn tmp() -> String {
    crate::session::instructions::global_tmp()
}

/// `Global`'s acquisition creates its directories.
pub fn create() {
    let data = data();
    for dir in [data.clone(), config(), state(), format!("{data}/log"), format!("{}/bin", cache()), format!("{data}/repos")] {
        let _ = std::fs::create_dir_all(dir);
    }
    tmp();
}

/// The global paths in `Global`'s key order, then the database (`debug paths`).
pub fn all() -> Vec<(&'static str, String)> {
    let data = data();
    let cache = cache();
    vec![
        ("home", home()),
        ("data", data.clone()),
        ("cache", cache.clone()),
        ("config", config()),
        ("state", state()),
        ("tmp", tmp()),
        ("bin", format!("{cache}/bin")),
        ("log", format!("{data}/log")),
        ("repos", format!("{data}/repos")),
        ("db", database()),
    ]
}

fn truthy(value: Option<String>) -> bool {
    value.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// `databasePath(global.data)`.
pub fn database() -> String {
    let filename = std::env::var("OPENCODE_DB").unwrap_or_else(|_| {
        let channel = crate::app::CHANNEL;
        let disabled = matches!(std::env::var("OPENCODE_DISABLE_CHANNEL_DB").as_deref(), Ok("1") | Ok("true"));
        if ["latest", "dev", "beta", "next", "prod"].contains(&channel) || disabled {
            "opencode.db".into()
        } else {
            let channel: String =
                channel.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '-' }).collect();
            format!("opencode-{channel}.db")
        }
    });
    if filename == ":memory:" {
        return filename;
    }
    if filename.starts_with('/') {
        return crate::project::absolutize(&filename);
    }
    crate::project::absolutize(&format!("{}/{filename}", data()))
}

pub fn truthy_env(name: &str) -> bool {
    truthy(std::env::var(name).ok())
}
