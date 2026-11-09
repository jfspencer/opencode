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
    // TODO(port): the legacy credential import (migration/20260805200742_import_legacy_credentials.ts) reads
    // <data>/auth.json into the credential table.
    tx.execute_batch("CREATE TABLE `migration` (id TEXT PRIMARY KEY, time_completed INTEGER NOT NULL)")
        .map_err(|e| e.to_string())?;
    let now = crate::id::now();
    for id in generated::MIGRATIONS {
        tx.execute("INSERT INTO `migration` (id, time_completed) VALUES (?1, ?2)", params![id, now])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
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
