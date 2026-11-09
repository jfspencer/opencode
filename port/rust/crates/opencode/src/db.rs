//! The SQLite database (packages/core/src/database): opened and bootstrapped exactly like the TS server's, so either
//! implementation can open a database the other wrote.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

#[path = "generated/database.rs"]
mod generated;

/// Opens (creating if needed) the database at `path`, or an in-memory one for `:memory:`.
pub fn open(path: &str) -> Result<Connection, String> {
    let memory = path == ":memory:";
    if !memory {
        restrict_to_owner(Path::new(path))?;
    }
    let db = if memory { Connection::open_in_memory() } else { Connection::open(path) }
        .map_err(|e| format!("failed to open database {path}: {e}"))?;
    let run = |sql: &str| -> Result<(), String> {
        // Some pragmas return a row (journal_mode, wal_checkpoint).
        db.query_row(sql, [], |_| Ok(())).optional().map(|_| ()).map_err(|e| format!("{sql}: {e}"))
    };
    run("PRAGMA journal_mode = WAL")?;
    run("PRAGMA synchronous = NORMAL")?;
    run("PRAGMA busy_timeout = 5000")?;
    run("PRAGMA cache_size = -64000")?;
    run("PRAGMA wal_checkpoint(PASSIVE)")?;
    run("PRAGMA foreign_keys = ON")?;
    bootstrap(&db)?;
    Ok(db)
}

// packages/core/src/database/migration.ts `apply`.
fn bootstrap(db: &Connection) -> Result<(), String> {
    let tables: Vec<String> = {
        let mut statement = db
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND substr(name, 1, 1) <> '_'",
            )
            .map_err(|e| e.to_string())?;
        statement
            .query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?
    };
    if tables.iter().any(|t| t == "session" || t == "session_v2") {
        return check_migrations(db);
    }
    if !tables.is_empty() {
        return Err("Database is not empty and has no session table".into());
    }
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    for statement in generated::SCHEMA {
        tx.execute_batch(statement).map_err(|e| format!("bootstrap: {e}"))?;
    }
    import_legacy_credentials(&tx, &format!("{}/auth.json", crate::cli::paths::data())).map_err(|e| e.to_string())?;
    tx.execute_batch("CREATE TABLE `migration` (id TEXT PRIMARY KEY, time_completed INTEGER NOT NULL)")
        .map_err(|e| e.to_string())?;
    let now = crate::id::now();
    for id in generated::MIGRATIONS {
        tx.execute("INSERT INTO `migration` (id, time_completed) VALUES (?1, ?2)", params![id, now])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

/// The legacy credential import (migration/20260805200742_import_legacy_credentials.ts), part of bootstrapping: the
/// entries of `<data>/auth.json` (API keys, OAuth tokens, wellknown tokens) become credentials of their integration
/// (the key without trailing slashes) unless it has one already; wellknown origins are added to the
/// `wellknown:sources` list. Entries that do not decode are skipped; a file that is not a JSON object is ignored.
fn import_legacy_credentials(tx: &rusqlite::Transaction, path: &str) -> rusqlite::Result<()> {
    use opencode_protocol::Json;
    let Ok(content) = std::fs::read_to_string(path) else { return Ok(()) };
    // A file that is not a JSON object is skipped (TS logs a warning).
    let Some(Json::Object(input)) = crate::json::parse(&content) else { return Ok(()) };
    let mut origins: Vec<String> = vec![];
    for id in opencode_protocol::wire::out::js_key_order(input.keys().map(String::as_str)) {
        let Some(value) = legacy_value(&input[id]) else { continue };
        let integration_id = id.trim_end_matches('/');
        if integration_id.is_empty() {
            continue;
        }
        if let Legacy::WellKnown { .. } = value {
            origins.push(integration_id.to_owned());
        }
        let exists = tx
            .query_row("SELECT id FROM credential WHERE integration_id = ?1", params![integration_id], |_| Ok(()))
            .optional()?
            .is_some();
        if exists {
            continue;
        }
        let mut credential = crate::json::Obj::new();
        let label = match value {
            Legacy::Key { key, metadata } => {
                credential.str("type", "key").str("key", key);
                if let Some(metadata) = metadata {
                    credential.set("metadata", metadata.clone());
                }
                "API key"
            }
            Legacy::WellKnown { token } => {
                credential.str("type", "key").str("key", token);
                "API key"
            }
            Legacy::OAuth { refresh, access, expires, account_id, enterprise_url } => {
                let method = match integration_id {
                    "openai" => "chatgpt-browser",
                    "github-copilot" | "opencode" | "xai" => "device",
                    _ => "oauth",
                };
                credential.str("type", "oauth").str("methodID", method).str("refresh", refresh).str("access", access);
                credential.set("expires", Json::Number(expires));
                let account = account_id.filter(|s| !s.is_empty());
                let enterprise = enterprise_url.filter(|s| !s.is_empty());
                if account.is_some() || enterprise.is_some() {
                    let mut metadata = crate::json::Obj::new();
                    metadata.opt_str("accountID", account).opt_str("enterpriseUrl", enterprise);
                    credential.set("metadata", metadata.done());
                }
                "OAuth"
            }
        };
        let now = crate::id::now();
        tx.execute(
            "INSERT INTO credential (id, integration_id, label, value, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![format!("cred_{}", crate::id::ascending()), integration_id, label, crate::json::text(&credential.done()), now],
        )?;
    }
    if origins.is_empty() {
        return Ok(());
    }
    let stored: Option<String> =
        tx.query_row("SELECT value FROM kv WHERE key = 'wellknown:sources'", [], |row| row.get(0)).optional()?;
    let current: Vec<String> = match stored.as_deref().and_then(crate::json::parse) {
        Some(Json::Array(items)) => items.iter().filter_map(|item| crate::json::as_str(item).map(str::to_owned)).collect(),
        _ => vec![],
    };
    // `new Set([...current, ...origins])`.
    let mut sources: Vec<String> = vec![];
    for source in current.into_iter().chain(origins) {
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    let value = crate::json::text(&Json::Array(sources.into_iter().map(Json::String).collect()));
    tx.execute(
        "INSERT INTO kv (key, value, time_created, time_updated) VALUES ('wellknown:sources', ?1, ?2, ?2) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, time_updated = excluded.time_updated",
        params![value, crate::id::now()],
    )?;
    Ok(())
}

/// A decoded `auth.json` entry (`LegacyValue`).
enum Legacy<'a> {
    OAuth { refresh: &'a str, access: &'a str, expires: f64, account_id: Option<&'a str>, enterprise_url: Option<&'a str> },
    Key { key: &'a str, metadata: Option<&'a opencode_protocol::Json> },
    WellKnown { token: &'a str },
}

/// `Schema.decodeUnknownOption(LegacyValue)`: `None` when the entry is none of the legacy shapes.
fn legacy_value(raw: &opencode_protocol::Json) -> Option<Legacy<'_>> {
    use opencode_protocol::Json;
    let Json::Object(o) = raw else { return None };
    let string = |key: &str| match o.get(key) {
        Some(Json::String(s)) => Some(s.as_str()),
        _ => None,
    };
    // An optional field may be missing; anything present must decode.
    let optional = |key: &str| match o.get(key) {
        None => Some(None),
        Some(Json::String(s)) => Some(Some(s.as_str())),
        Some(_) => None,
    };
    match string("type")? {
        "oauth" => {
            let expires = match o.get("expires") {
                Some(Json::Number(n)) if n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0 && *n >= 0.0 => *n + 0.0,
                _ => return None,
            };
            Some(Legacy::OAuth {
                refresh: string("refresh")?,
                access: string("access")?,
                expires,
                account_id: optional("accountId")?,
                enterprise_url: optional("enterpriseUrl")?,
            })
        }
        "api" => {
            let metadata = match o.get("metadata") {
                None => None,
                Some(value @ Json::Object(record)) if record.values().all(|v| matches!(v, Json::String(_))) => Some(value),
                Some(_) => return None,
            };
            Some(Legacy::Key { key: string("key")?, metadata })
        }
        "wellknown" => {
            string("key")?;
            Some(Legacy::WellKnown { token: string("token")? })
        }
        _ => None,
    }
}

/// An existing database must be at the schema this build knows: the TS migrations themselves are not ported, so an
/// older database is opened by the TS server first.
fn check_migrations(db: &Connection) -> Result<(), String> {
    let applied: Vec<String> = {
        let mut statement = db.prepare("SELECT id FROM `migration`").map_err(|e| e.to_string())?;
        statement
            .query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?
    };
    let missing: Vec<&str> =
        generated::MIGRATIONS.iter().copied().filter(|id| !applied.iter().any(|a| a == id)).collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!("the database needs migrations this build does not run: {}", missing.join(", ")))
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    if !path.exists() {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    for file in [path.to_path_buf(), sidecar(path, "-wal"), sidecar(path, "-shm")] {
        match std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{}: {e}", file.display())),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_to_owner(_: &Path) -> Result<(), String> {
    Ok(())
}

fn sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}
