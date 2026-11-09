//! Locations (packages/core/src/location*.ts): the services of one directory, booted on first use and kept until they
//! are invalidated (`location.reload`, `debug.location.evict`, a repository initialized in them).
//!
//! Booting resolves and activates the directory's project, then activates the location's plugins, which announce
//! what they registered. TODO(port): the services themselves (config, agents, commands, skills, ...): the boot
//! announces them in the order the TS server's built-in plugins do. What they register is the location's
//! [`crate::inventory::Inventory`]. Shutting a location down (`LocationLifecycle.shutdown`) announces
//! `location.shutdown`; TODO(port): it also closes the location's permission requests, forms and RPC clients.

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
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
    /// The repository the project resolved (`Location.Interface.vcs`).
    pub vcs: Option<crate::project::Vcs>,
    #[allow(dead_code)]
    pub inventory: crate::inventory::Inventory,
    /// The directory with symlinks resolved (`FileSystem`'s confinement root).
    pub root: String,
    /// The location's VCS service (`Vcs.Service`).
    pub version_control: crate::vcs::Service,
    /// The location's file search (`FileSystemSearch.Service`).
    pub search: crate::fs::Search,
}

#[derive(Debug, Clone)]
pub struct ProjectRef {
    pub id: String,
    pub directory: String,
    pub canonical: String,
}

impl Location {
    /// `Location.Info` (`{directory, project: {id, directory, canonical}}`).
    pub fn info(&self) -> Json {
        let mut project = Obj::new();
        project.str("id", &self.project.id).str("directory", &self.project.directory).str("canonical", &self.project.canonical);
        let mut o = Obj::new();
        o.str("directory", &self.directory);
        o.opt_str("workspaceID", self.workspace_id.as_deref());
        o.set("project", project.done());
        o.done()
    }

    /// `Location.Ref` of this location.
    pub fn reference(&self) -> LocationRef {
        LocationRef { directory: self.directory.clone(), workspace_id: self.workspace_id.clone() }
    }
}

#[derive(Default)]
pub struct Locations {
    /// Booted locations, in boot order (the order `debug.location.list` reports).
    booted: Mutex<IndexMap<LocationRef, Arc<Location>>>,
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
        let location = Arc::new(boot(app, &key).await?);
        self.booted.lock().unwrap().insert(key, location.clone());
        Ok(location)
    }

    /// The booted locations' references (`RcMap.keys`).
    pub fn keys(&self) -> Vec<LocationRef> {
        self.booted.lock().unwrap().keys().cloned().collect()
    }

    /// Shuts a booted location down and forgets it (`LayerMap.invalidate`); the next request boots it again.
    pub fn invalidate(&self, app: &App, reference: &LocationRef) {
        let removed = self.booted.lock().unwrap().shift_remove(&canonical(reference));
        if let Some(location) = removed {
            app.bus.emit(
                "location.shutdown",
                Obj::new().done(),
                PublishOptions { id: None, location: Some(location.reference()) },
            );
        }
    }

    /// `LocationServiceMap.reload`: every booted location is shut down, then booted again.
    pub async fn reload(&self, app: &App) -> Result<(), Failure> {
        let references = self.keys();
        for reference in &references {
            self.invalidate(app, reference);
        }
        let mut failure = None;
        for reference in &references {
            if let Err(error) = self.get(app, reference).await {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

async fn boot(app: &App, key: &LocationRef) -> Result<Location, Failure> {
    let directory = key.directory.clone();
    let is_dir = std::path::Path::new(&directory).is_dir();
    // Discovery spawns git; it runs off the runtime's thread.
    let resolved = tokio::task::spawn_blocking(move || crate::project::discover(&directory)).await.expect("project discovery");
    // A missing directory is not persisted as a project: FileSystem rejects the location right after.
    if !is_dir {
        return Err(not_found(&key.directory));
    }
    let Ok(root) = std::fs::canonicalize(&key.directory).map(|p| p.to_string_lossy().into_owned()) else {
        return Err(not_found(&key.directory));
    };
    {
        let mut db = app.db.lock().unwrap();
        app.projects.persist(&mut db, &app.bus, &resolved).map_err(crate::app::fatal)?;
    }
    let version_control =
        crate::vcs::Service::new(resolved.vcs.as_ref().map(|v| v.kind), &key.directory, &resolved.directory);
    let location = Location {
        directory: key.directory.clone(),
        workspace_id: key.workspace_id.clone(),
        project: ProjectRef { id: resolved.id.clone(), directory: resolved.directory, canonical: resolved.canonical },
        vcs: resolved.vcs,
        inventory: crate::inventory::Inventory::load(key)?,
        search: crate::fs::Search::new(&root),
        root,
        version_control,
    };
    {
        let db = app.db.lock().unwrap();
        // LocationLifecycle activates the project when the location boots.
        app.projects.activate(&db, &app.bus, &location.project.id, Some(key.clone())).map_err(crate::app::fatal)?;
        // Icon discovery runs beside the boot; it lands before the plugins announce anything.
        crate::project_icon::refresh(&db, &app.bus, &location.project.id, None).map_err(crate::app::fatal)?;
    }
    let branch = location.version_control.refresh().await;
    let options = || PublishOptions { id: None, location: Some(key.clone()) };
    for ty in ANNOUNCED {
        app.bus.emit(ty, Obj::new().done(), options());
        if *ty == "model.updated" {
            if let Some(branch) = &branch {
                let mut data = Obj::new();
                data.opt_str("branch", branch.as_deref());
                app.bus.emit("vcs.branch.updated", data.done(), options());
            }
        }
    }
    Ok(location)
}
