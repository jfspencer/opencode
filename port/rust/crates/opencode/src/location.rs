//! Locations (packages/core/src/location*.ts): the services of one directory, booted on first use and kept.
//!
//! Booting resolves and activates the directory's project, then activates the location's plugins, which announce
//! what they registered, in the order the TS server's built-in plugins do. What they register (config, agents,
//! providers, models, commands, skills, references) is the location's [`crate::inventory::Inventory`], loaded before
//! the announcements.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use opencode_protocol::Json;

use crate::app::App;
use crate::bus::{LocationRef, PublishOptions};
use crate::http::Failure;
use crate::json::Obj;

#[derive(Debug)]
pub struct Location {
    pub directory: String,
    pub workspace_id: Option<String>,
    pub project: ProjectRef,
    /// What the location's config and plugins register (replaced as a whole when its config changes).
    pub inventory: crate::inventory::Shared,
}

#[derive(Debug, Clone)]
pub struct ProjectRef {
    pub id: String,
    pub directory: String,
    pub canonical: String,
}

impl Location {
    /// The current inventory snapshot (see [`crate::inventory`]).
    pub fn inventory(&self) -> std::sync::Arc<crate::inventory::Inventory> {
        self.inventory.get()
    }

    /// The location's reference on events.
    pub fn reference(&self) -> LocationRef {
        LocationRef { directory: self.directory.clone(), workspace_id: self.workspace_id.clone() }
    }

    /// `Location.Info` (`{directory, project: {id, directory, canonical}}`), for `location.get` (TODO(port)); located
    /// answers carry only `Location.PublicRef`.
    #[allow(dead_code)]
    pub fn info(&self) -> Json {
        let mut project = Obj::new();
        project.str("id", &self.project.id).str("directory", &self.project.directory).str("canonical", &self.project.canonical);
        let mut o = Obj::new();
        o.str("directory", &self.directory);
        o.opt_str("workspaceID", self.workspace_id.as_deref());
        o.set("project", project.done());
        o.done()
    }
}

#[derive(Default)]
pub struct Locations {
    booted: Mutex<HashMap<LocationRef, Arc<Location>>>,
    /// Held while a location boots, so concurrent first requests boot it once.
    booting: tokio::sync::Mutex<()>,
}

/// `LocationServiceMap.canonical`: normalized, without a trailing separator.
pub fn canonical(reference: &LocationRef) -> LocationRef {
    LocationRef {
        directory: crate::project::normalize(&reference.directory),
        workspace_id: reference.workspace_id.clone(),
    }
}

/// What a location's services announce as they boot, in order (`vcs.branch.updated` goes after `model.updated`).
const ANNOUNCED: &[&str] = &[
    "integration.updated",
    "provider.updated",
    "model.updated",
    "agent.updated",
    "command.updated",
    "skill.updated",
    "websearch.updated",
    "reference.updated",
    "plugin.updated",
];

pub fn not_found(directory: &str) -> Failure {
    let mut location = Obj::new();
    location.str("directory", directory);
    let mut o = Obj::new();
    o.str("_tag", "LocationNotFoundError");
    o.set("location", location.done());
    o.str("message", &format!("Location not found: {directory}"));
    Failure::new(404, o.done())
}

impl Locations {
    pub fn cached(&self, reference: &LocationRef) -> Option<Arc<Location>> {
        self.booted.lock().unwrap().get(&canonical(reference)).cloned()
    }

    /// The location's services, booting them on first use. A directory that does not exist fails (and is retried
    /// by the next request).
    pub async fn get(&self, app: &App, reference: &LocationRef) -> Result<Arc<Location>, Failure> {
        let key = canonical(reference);
        if let Some(location) = self.cached(&key) {
            return Ok(location);
        }
        let _booting = self.booting.lock().await;
        if let Some(location) = self.cached(&key) {
            return Ok(location);
        }
        let location = Arc::new(boot(app, &key)?);
        self.booted.lock().unwrap().insert(key, location.clone());
        Ok(location)
    }
}

fn boot(app: &App, key: &LocationRef) -> Result<Location, Failure> {
    let is_dir = std::path::Path::new(&key.directory).is_dir();
    // A missing directory is not persisted as a project: FileSystem rejects the location right after.
    let resolved = app.projects.discover(&key.directory);
    if !is_dir {
        return Err(not_found(&key.directory));
    }
    let branch = resolved.vcs.as_ref().filter(|v| v.kind == "git").and_then(|_| crate::project::branch(&resolved.directory));
    let mut db = app.db.lock().unwrap();
    app.projects.persist(&mut db, &app.bus, &resolved).map_err(crate::app::fatal)?;
    let location = Location {
        directory: key.directory.clone(),
        workspace_id: key.workspace_id.clone(),
        project: ProjectRef { id: resolved.id.clone(), directory: resolved.directory, canonical: resolved.canonical },
        inventory: crate::inventory::Shared::new(crate::inventory::Inventory::load(key)?),
    };
    // LocationLifecycle activates the project when the location boots.
    app.projects.activate(&db, &app.bus, &resolved.id, Some(key.clone())).map_err(crate::app::fatal)?;
    drop(db);
    let options = || PublishOptions { id: None, location: Some(key.clone()) };
    for ty in ANNOUNCED {
        app.bus.emit(ty, Obj::new().done(), options());
        if *ty == "model.updated" {
            if let Some(branch) = &branch {
                let mut data = Obj::new();
                data.str("branch", branch);
                app.bus.emit("vcs.branch.updated", data.done(), options());
            }
        }
    }
    Ok(location)
}
