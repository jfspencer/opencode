//! Stored credentials (packages/core/src/credential.ts, credential/sql.ts, server/src/handlers/credential.ts) and how
//! integrations use them (integration.ts `resolveConnections`, `connection.active`, `connection.resolve`,
//! `connection.key`, `createCredential`; model-resolver.ts's credential settings).
//!
//! Credentials are global: one `credential` table, each row an integration's key, OAuth tokens or external source,
//! one of them selected per integration (`active`). An integration's connections are its stored credentials, the
//! selected one first, then the environment variables of its `env` methods that are set; the first is the active
//! connection, whose key a provider request authenticates with (over a configured `apiKey`).
//!
//! TODO(port): refreshing an expiring OAuth credential (no OAuth login is in scope), `integration.use` accounts.

use std::sync::{OnceLock, Weak};

use opencode_protocol::types as t;
use opencode_protocol::{Json, Wire};
use rusqlite::{Connection, OptionalExtension, params};

use crate::app::App;
use crate::bus::PublishOptions;
use crate::json::{self, Obj};

/// `Credential.Info`.
#[derive(Debug, Clone)]
pub struct Info {
    pub id: String,
    pub integration_id: String,
    pub label: String,
    pub value: Json,
}

static APP: OnceLock<Weak<App>> = OnceLock::new();

/// Makes the server's store reachable where models are resolved (they read the active credential).
pub fn install(app: &std::sync::Arc<App>) {
    let _ = APP.set(std::sync::Arc::downgrade(app));
}

/// `CredentialLegacy.Value`: a stored value of either shape as the current `Credential.Value` (a key's
/// pre-v2.1 `configuration` answers merge over its `metadata`).
fn decode_value(text: &str) -> Option<Json> {
    let mut value = json::parse(text)?;
    if json::get_str(&value, "type") == Some("key") {
        if let Some(Json::Object(configuration)) = json::remove(&mut value, "configuration") {
            let mut metadata = json::get(&value, "metadata").cloned().filter(|m| matches!(m, Json::Object(_))).unwrap_or_else(json::object);
            for (key, item) in configuration {
                json::set(&mut metadata, &key, item);
            }
            json::set(&mut value, "metadata", metadata);
        }
    }
    t::CredentialValue::decode(&value).ok().map(|value| json::of(&value))
}

fn rows(db: &Connection, integration: Option<&str>) -> rusqlite::Result<Vec<Info>> {
    let mut sql = String::from("SELECT id, integration_id, label, value FROM credential");
    if integration.is_some() {
        sql.push_str(" WHERE integration_id = ?1");
    }
    sql.push_str(" ORDER BY active ASC, time_created ASC, id ASC");
    let mut statement = db.prepare(&sql)?;
    let map = |row: &rusqlite::Row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?))
    };
    let found: Vec<(String, Option<String>, String, String)> = match integration {
        Some(id) => statement.query_map(params![id], map)?.collect::<rusqlite::Result<_>>()?,
        None => statement.query_map([], map)?.collect::<rusqlite::Result<_>>()?,
    };
    Ok(found
        .into_iter()
        .filter_map(|(id, integration_id, label, value)| {
            Some(Info { id, integration_id: integration_id?, label, value: decode_value(&value)? })
        })
        .collect())
}

/// `Credential.all`: every stored credential, each integration's selected one after its others.
pub fn all(db: &Connection) -> rusqlite::Result<Vec<Info>> {
    rows(db, None)
}

/// `Credential.list`: one integration's credentials.
pub fn list(db: &Connection, integration: &str) -> rusqlite::Result<Vec<Info>> {
    rows(db, Some(integration))
}

/// `Credential.get`.
pub fn get(db: &Connection, id: &str) -> rusqlite::Result<Option<Info>> {
    let row: Option<(Option<String>, String, String)> = db
        .query_row("SELECT integration_id, label, value FROM credential WHERE id = ?1", params![id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    Ok(row.and_then(|(integration_id, label, value)| {
        Some(Info { id: id.to_owned(), integration_id: integration_id?, label, value: decode_value(&value)? })
    }))
}

/// The credential listing (`entries` in handlers/credential.ts): each integration's selected credential is its last.
pub fn entries(credentials: &[Info]) -> Vec<Json> {
    credentials
        .iter()
        .map(|item| {
            let selected = credentials.iter().rev().find(|c| c.integration_id == item.integration_id).map(|c| c.id.as_str());
            let mut o = Obj::new();
            o.str("id", &item.id).str("integrationID", &item.integration_id).str("label", &item.label);
            o.set("active", Json::Bool(selected == Some(item.id.as_str()))).set("value", item.value.clone());
            o.done()
        })
        .collect()
}

fn publish_updated(app: &App) {
    app.bus.emit("credential.updated", json::object(), PublishOptions::default());
}

fn publish_switched(app: &App, integration: &str, credential: Option<&str>) {
    let mut data = Obj::new();
    data.str("integrationID", integration).set("credentialID", credential.map(json::string).unwrap_or(Json::Null));
    app.bus.emit("credential.switched", data.done(), PublishOptions::default());
}

/// The integration's selected credential (`currentActive`): the active one, else the newest.
fn current_active(db: &Connection, integration: &str) -> rusqlite::Result<Option<(String, Option<bool>)>> {
    db.query_row(
        "SELECT id, active FROM credential WHERE integration_id = ?1 ORDER BY active DESC, time_created DESC, id DESC LIMIT 1",
        params![integration],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// `Credential.create` input.
pub struct Create {
    pub id: Option<String>,
    pub integration_id: String,
    pub value: Json,
    pub label: Option<String>,
    pub activate: Option<bool>,
}

/// `Credential.create`: the new credential becomes its integration's selection unless `activate` is false and the
/// integration already has one.
pub fn create(app: &App, input: Create) -> rusqlite::Result<Info> {
    let credential = Info {
        id: input.id.unwrap_or_else(|| format!("cred_{}", crate::id::ascending())),
        integration_id: input.integration_id,
        label: input.label.unwrap_or_else(|| "default".into()),
        value: input.value,
    };
    let activated = {
        let mut db = app.db.lock().unwrap();
        let tx = db.transaction()?;
        let current = current_active(&tx, &credential.integration_id)?;
        let activate = input.activate != Some(false) || current.is_none();
        if activate {
            tx.execute("UPDATE credential SET active = 0, time_updated = ?2 WHERE integration_id = ?1", params![credential.integration_id, crate::id::now()])?;
        }
        // Legacy rows have no active flag and the newest is selected: pin it before inserting a newer row.
        if let Some((id, active)) = current.filter(|(_, active)| !activate && *active != Some(true)) {
            let _ = active;
            tx.execute("UPDATE credential SET active = 1, time_updated = ?2 WHERE id = ?1", params![id, crate::id::now()])?;
        }
        let now = crate::id::now();
        tx.execute(
            "INSERT INTO credential (id, integration_id, label, value, active, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![credential.id, credential.integration_id, credential.label, json::text(&credential.value), activate, now],
        )?;
        tx.commit()?;
        activate
    };
    publish_updated(app);
    if activated {
        publish_switched(app, &credential.integration_id, Some(&credential.id));
    }
    Ok(credential)
}

/// `Credential.activate`: selects a stored credential for its integration.
pub fn activate(app: &App, id: &str) -> rusqlite::Result<()> {
    let integration = {
        let mut db = app.db.lock().unwrap();
        let tx = db.transaction()?;
        let integration: Option<Option<String>> =
            tx.query_row("SELECT integration_id FROM credential WHERE id = ?1", params![id], |row| row.get(0)).optional()?;
        let Some(Some(integration)) = integration else { return Ok(()) };
        if current_active(&tx, &integration)?.is_some_and(|(active, _)| active == id) {
            return Ok(());
        }
        switch_active(&tx, &integration, id)?;
        tx.commit()?;
        integration
    };
    publish_switched(app, &integration, Some(id));
    Ok(())
}

fn switch_active(db: &Connection, integration: &str, id: &str) -> rusqlite::Result<()> {
    let now = crate::id::now();
    db.execute("UPDATE credential SET active = 0, time_updated = ?2 WHERE integration_id = ?1", params![integration, now])?;
    db.execute("UPDATE credential SET active = 1, time_updated = ?2 WHERE id = ?1", params![id, now])?;
    Ok(())
}

/// `Credential.update` of a label (the endpoint changes only labels).
pub fn update_label(app: &App, id: &str, label: Option<&str>) -> rusqlite::Result<()> {
    let Some(label) = label else { return Ok(()) };
    let changed = {
        let db = app.db.lock().unwrap();
        let current: Option<(Option<String>, String)> = db
            .query_row("SELECT integration_id, label FROM credential WHERE id = ?1", params![id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let Some((Some(_), current)) = current else { return Ok(()) };
        if current == label {
            return Ok(());
        }
        db.execute("UPDATE credential SET label = ?1, time_updated = ?2 WHERE id = ?3", params![label, crate::id::now(), id])? > 0
    };
    if changed {
        publish_updated(app);
    }
    Ok(())
}

/// `Credential.remove`: a removed selection passes to the integration's newest other credential.
pub fn remove(app: &App, id: &str) -> rusqlite::Result<()> {
    let removed = {
        let mut db = app.db.lock().unwrap();
        let tx = db.transaction()?;
        let integration: Option<Option<String>> =
            tx.query_row("SELECT integration_id FROM credential WHERE id = ?1", params![id], |row| row.get(0)).optional()?;
        let Some(integration) = integration else { return Ok(()) };
        let active = match &integration {
            Some(integration) => current_active(&tx, integration)?,
            None => None,
        };
        tx.execute("DELETE FROM credential WHERE id = ?1", params![id])?;
        let switched = match integration.as_deref() {
            Some(integration) if active.as_ref().is_some_and(|(active, _)| active == id) => {
                let replacement: Option<String> = tx
                    .query_row(
                        "SELECT id FROM credential WHERE integration_id = ?1 ORDER BY time_created DESC, id DESC LIMIT 1",
                        params![integration],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(replacement) = &replacement {
                    switch_active(&tx, integration, replacement)?;
                }
                Some((integration.to_owned(), replacement))
            }
            _ => None,
        };
        tx.commit()?;
        switched
    };
    publish_updated(app);
    if let Some((integration, replacement)) = removed {
        publish_switched(app, &integration, replacement.as_deref());
    }
    Ok(())
}

/// `resolveConnections`: an integration's stored credentials (the selected one first), then the variables of its
/// `env` methods that are set.
pub fn connections(saved: &[Info], env_names: &[String]) -> Vec<Json> {
    let mut out: Vec<Json> = saved
        .iter()
        .rev()
        .map(|credential| {
            let mut o = Obj::new();
            o.str("type", "credential").str("id", &credential.id).str("label", &credential.label);
            o.str("method", json::get_str(&credential.value, "type").unwrap_or_default());
            o.done()
        })
        .collect();
    out.extend(env_names.iter().filter(|name| std::env::var(name).is_ok_and(|v| !v.is_empty())).map(|name| {
        let mut o = Obj::new();
        o.str("type", "env").str("name", name);
        o.done()
    }));
    out
}

/// `createCredential`: without a label, the integration's name, else `<name> 2`, `<name> 3`... (the first free one).
pub fn create_labeled(app: &App, integration: &str, name: &str, value: Json, label: Option<String>) -> rusqlite::Result<Info> {
    let label = match label {
        Some(label) => label,
        None => {
            let labels: Vec<String> = list(&app.db.lock().unwrap(), integration)?.into_iter().map(|c| c.label).collect();
            (0..=labels.len())
                .map(|index| if index == 0 { name.to_owned() } else { format!("{name} {}", index + 1) })
                .find(|candidate| !labels.contains(candidate))
                .unwrap_or_else(|| name.to_owned())
        }
    };
    create(app, Create { id: None, integration_id: integration.to_owned(), value, label: Some(label), activate: None })
}

/// The key a provider request authenticates with (`ModelResolver.prepare` and `loadCatalogPackage`): the active
/// connection's (a stored key, else a set `env` variable of the integration), over the configured `apiKey`.
pub fn api_key(inventory: &crate::inventory::Inventory, provider: &str, configured: Option<&str>) -> Option<String> {
    let integration = inventory
        .provider(provider)
        .and_then(|p| p.integration_id.as_ref().map(|id| id.0.clone()))
        .unwrap_or_else(|| provider.to_owned());
    let stored = APP.get().and_then(Weak::upgrade).and_then(|app| list(&app.db.lock().unwrap(), &integration).ok());
    // `connection.active` is the first connection: the selected stored credential (an OAuth or external one carries
    // no key setting here), else the environment's.
    if let Some(selected) = stored.as_ref().and_then(|saved| saved.last()) {
        return match json::get_str(&selected.value, "type") {
            Some("key") => json::get_str(&selected.value, "key").map(str::to_owned),
            Some("oauth") => json::get_str(&selected.value, "access").map(str::to_owned),
            _ => None,
        };
    }
    inventory.api_key(provider).or_else(|| configured.map(str::to_owned))
}
