//! Well-known sources (packages/core/src/wellknown.ts, wellknown/plugin.ts): an origin whose
//! `<origin>/.well-known/opencode` manifest names an authentication command becomes an integration (named after the
//! origin's host) with one command login, `login`. Sources are global: their origins are kept in the `kv` table
//! (`wellknown:sources`), each manifest too (`wellknown:manifest:<origin>`), so a source whose manifest cannot be
//! fetched later keeps its last one.
//!
//! The configuration a source delivers (the manifest's `config` and `remote_config`) is config documents'
//! (`inventory::config`).

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use indexmap::IndexMap;
use opencode_protocol::Json;
use rusqlite::{Connection, OptionalExtension, params};

use crate::app::App;
use crate::json::{self, Obj};

const SOURCES: &str = "wellknown:sources";

/// A registered source: its origin (also its integration's ID) and its decoded manifest.
#[derive(Clone, Debug)]
pub struct Entry {
    pub origin: String,
    pub manifest: Json,
}

#[derive(Default)]
struct State {
    /// The entries loaded so far (`cache`), by origin.
    cache: IndexMap<String, Entry>,
    loaded: bool,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default)
}

/// `WellKnown.entries`: the registered sources, their manifests fetched the first time (by the plugin, as a location
/// boots). A source that cannot be loaded is left out.
pub async fn load(app: &App) {
    if state().lock().unwrap().loaded {
        return;
    }
    let origins = origins(&app.db.lock().unwrap());
    let mut loaded = vec![];
    for origin in origins {
        if let Some(entry) = state().lock().unwrap().cache.get(&origin).cloned() {
            loaded.push(entry);
            continue;
        }
        if let Ok(entry) = load_entry(app, &origin).await {
            loaded.push(entry);
        }
    }
    let mut state = state().lock().unwrap();
    state.cache = loaded.into_iter().map(|entry| (entry.origin.clone(), entry)).collect();
    state.loaded = true;
}

/// `WellKnown.snapshot`: the loaded sources.
pub fn snapshot() -> Vec<Entry> {
    state().lock().unwrap().cache.values().cloned().collect()
}

/// `WellKnown.add`: fetches the origin's manifest (trailing slashes dropped) and registers it; a manifest without an
/// authentication command is refused.
pub async fn add(app: &App, value: &str) -> Result<Entry, String> {
    load(app).await;
    let origin = value.trim_end_matches('/').to_owned();
    let entry = load_entry(app, &origin).await?;
    if json::get(&entry.manifest, "auth").is_none() {
        return Err(format!("No authentication method found at {origin}"));
    }
    {
        let db = app.db.lock().unwrap();
        let mut origins = origins(&db);
        if !origins.contains(&origin) {
            origins.push(origin.clone());
        }
        kv_set(&db, SOURCES, &Json::Array(origins.into_iter().map(Json::String).collect())).map_err(|e| e.to_string())?;
    }
    state().lock().unwrap().cache.insert(origin, entry.clone());
    Ok(entry)
}

/// What the source registers (`WellKnownPlugin`): the integration's name (the origin's host) and its command login.
pub fn integration(entry: &Entry) -> Option<(String, Json)> {
    let auth = json::get(&entry.manifest, "auth")?;
    let name = crate::url::Url::parse(&entry.origin).and_then(|url| url.host).unwrap_or_default();
    let mut method = Obj::new();
    method.str("id", "login").str("type", "command").str("label", "Log in");
    method.set("command", json::get(auth, "command").cloned().unwrap_or_else(|| Json::Array(vec![])));
    Some((name, method.done()))
}

/// `loadEntry`: the manifest, fetched (and kept) or else the one kept from before.
async fn load_entry(app: &App, origin: &str) -> Result<Entry, String> {
    let key = format!("wellknown:manifest:{origin}");
    let manifest = match inspect(origin).await {
        Ok(manifest) => {
            let _ = kv_set(&app.db.lock().unwrap(), &key, &manifest);
            manifest
        }
        Err(error) => {
            let kept = kv_get(&app.db.lock().unwrap(), &key).and_then(|value| decode(&value));
            match kept {
                Some(manifest) => {
                    eprintln!("failed to load wellknown manifest {origin}: {error}");
                    manifest
                }
                None => return Err(error),
            }
        }
    };
    Ok(Entry { origin: origin.to_owned(), manifest })
}

/// `WellKnown.inspect`: `GET <origin>/.well-known/opencode` (JSON, a 2xx answer, within 15 seconds), decoded as a
/// `WellKnown.Manifest`.
async fn inspect(origin: &str) -> Result<Json, String> {
    let url = format!("{}/.well-known/opencode", origin.trim_end_matches('/'));
    let failed = || format!("Failed to load wellknown manifest from {url}");
    let fetch = async {
        let headers = [("accept".to_owned(), "application/json".to_owned())];
        let response = crate::llm::http::send("GET", &url, &headers, String::new()).await.ok()?;
        if !(200..300).contains(&response.status) {
            return None;
        }
        let text = response.text().await.ok()?;
        decode(&json::parse(&text)?)
    };
    tokio::time::timeout(Duration::from_secs(15), fetch).await.ok().flatten().ok_or_else(failed)
}

/// `WellKnown.Manifest`: `auth` (`{command: string[], env: string}`), `config` (an object or null) and
/// `remote_config` (`{url, headers?}`), each optional; other keys are dropped.
fn decode(value: &Json) -> Option<Json> {
    let Json::Object(fields) = value else { return None };
    let strings = |value: &Json| matches!(value, Json::Array(items) if items.iter().all(|i| matches!(i, Json::String(_))));
    let mut out = Obj::new();
    if let Some(auth) = fields.get("auth") {
        let command = json::get(auth, "command").filter(|c| strings(c))?;
        let env = json::get(auth, "env").filter(|e| matches!(e, Json::String(_)))?;
        let mut decoded = Obj::new();
        decoded.set("command", command.clone()).set("env", env.clone());
        out.set("auth", decoded.done());
    }
    if let Some(config) = fields.get("config") {
        if !matches!(config, Json::Null | Json::Object(_)) {
            return None;
        }
        out.set("config", config.clone());
    }
    if let Some(remote) = fields.get("remote_config") {
        let url = json::get(remote, "url").filter(|u| matches!(u, Json::String(_)))?;
        let mut decoded = Obj::new();
        decoded.set("url", url.clone());
        if let Some(headers) = json::get(remote, "headers") {
            let Json::Object(items) = headers else { return None };
            if !items.values().all(|v| matches!(v, Json::String(_))) {
                return None;
            }
            decoded.set("headers", headers.clone());
        }
        out.set("remote_config", decoded.done());
    }
    Some(out.done())
}

fn origins(db: &Connection) -> Vec<String> {
    match kv_get(db, SOURCES) {
        Some(Json::Array(items)) if items.iter().all(|i| matches!(i, Json::String(_))) => {
            items.into_iter().filter_map(|i| json::as_str(&i).map(str::to_owned)).collect()
        }
        _ => vec![],
    }
}

fn kv_get(db: &Connection, key: &str) -> Option<Json> {
    let text: Option<String> =
        db.query_row("SELECT value FROM kv WHERE key = ?1", params![key], |row| row.get(0)).optional().ok().flatten();
    text.and_then(|text| json::parse(&text))
}

fn kv_set(db: &Connection, key: &str, value: &Json) -> rusqlite::Result<()> {
    let now = crate::id::now();
    db.execute(
        "INSERT INTO kv (key, value, time_created, time_updated) VALUES (?1, ?2, ?3, ?3) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, time_updated = excluded.time_updated",
        params![key, json::text(value), now],
    )?;
    Ok(())
}
