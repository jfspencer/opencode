//! Locations (packages/core/src/location*.ts): the services of one directory, booted on first use and kept until they
//! are invalidated (`location.reload`, `debug.location.evict`, a repository initialized in them, the idle sweep of
//! [`crate::location_activity`]).
//!
//! Booting resolves and activates the directory's project, then activates the location's plugins, which announce
//! what they registered, in the order the TS server's built-in plugins do. What they register (config, agents,
//! providers, models, commands, skills, references) is the location's [`crate::inventory::Inventory`], loaded before
//! the announcements; the location watcher ([`crate::location_watch`]) starts after them. Shutting a location down
//! (`LocationLifecycle.shutdown`) rejects its pending permission requests and cancels its pending forms, then
//! announces `location.shutdown`. Between the forms and the announcement TS also closes the location's RPC registry
//! (`Rpc.close`), which fails the RPC calls still running: the only built-in RPC, the goal's `get`
//! ([`crate::session::goal`]), answers at once, so none is ever running then.

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
    /// What the location's config and plugins register (replaced as a whole when its config changes).
    pub inventory: crate::inventory::Shared,
    /// The directory with symlinks resolved (`FileSystem`'s confinement root).
    pub root: String,
    /// The location's VCS service (`Vcs.Service`).
    pub version_control: crate::vcs::Service,
    /// The location's file search (`FileSystemSearch.Service`).
    pub search: crate::fs::Search,
    /// The location's pending permission requests (`Permission.Service`).
    pub permission: crate::permission::Service,
    /// The location's forms (`Form.Service`).
    pub forms: crate::form::Service,
    /// The location's shell commands (`Shell.Service`).
    pub shell: Arc<crate::shell::Service>,
    /// The location's terminals (`Pty.Service`).
    pub pty: Arc<crate::pty::Service>,
    /// The location's snapshot store (`Snapshot.Service`).
    pub snapshot: crate::snapshot::Service,
    /// The repository metadata watch (`LocationWatcher`).
    pub watcher: crate::location_watch::Watcher,
    /// Open client connections (terminal sockets) that keep the idle sweep away (`LocationLifecycle.retain`).
    pub retained: std::sync::atomic::AtomicUsize,
    /// The location's login attempts (`Integration.Service`).
    pub integration: crate::integration::Attempts,
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

    /// `Location.Info` (`{directory, project: {id, directory, canonical}}`), for `location.get`; located
    /// answers carry only `Location.PublicRef`.
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
    /// Booted locations, in boot order (the order `debug.location.list` reports).
    booted: Mutex<IndexMap<LocationRef, Arc<Location>>>,
    /// Held while a location boots, so concurrent first requests boot it once.
    booting: tokio::sync::Mutex<()>,
    /// The idle sweep's activity record.
    pub activity: crate::location_activity::Activity,
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
        location.inventory.watch(app.arc(), Arc::downgrade(&location));
        self.booted.lock().unwrap().insert(key, location.clone());
        // The location's Git references are checked out in the background, and again every hour.
        crate::repository_cache::refresh(&app.arc(), &location.inventory().references);
        crate::repository_cache::schedule(&app.arc(), &location);
        // The location watcher starts once the plugins have activated.
        let (app, booted) = (app.arc(), location.clone());
        tokio::spawn(async move { booted.watcher.reconcile(&app, &booted).await });
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
            // `LocationLifecycle.shutdown`: permissions, then forms, then the announcement.
            location.permission.close(&app.bus, &location.reference());
            location.forms.close(&app.bus, &location.reference());
            location.shell.close();
            location.pty.close();
            location.watcher.stop();
            app.bus.emit(
                "location.shutdown",
                Obj::new().done(),
                PublishOptions { id: None, location: Some(location.reference()) },
            );
        }
    }

    /// `LocationServiceMap.reload`: every booted location is shut down, then booted again.
    /// Boots every location again; the first that failed, with its failure.
    pub async fn reload(&self, app: &App) -> Result<(), (LocationRef, Failure)> {
        let references = self.keys();
        for reference in &references {
            self.invalidate(app, reference);
        }
        let mut failure = None;
        for reference in &references {
            if let Err(error) = self.get(app, reference).await {
                failure.get_or_insert((reference.clone(), error));
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

/// `fs.access(directory, { readable: true })`.
pub fn readable(directory: &str) -> bool {
    let Ok(path) = std::ffi::CString::new(directory) else { return false };
    // SAFETY: `path` is a valid NUL-terminated string for the duration of the call.
    unsafe { libc::access(path.as_ptr(), libc::R_OK) == 0 }
}

async fn boot(app: &App, key: &LocationRef) -> Result<Location, Failure> {
    let directory = key.directory.clone();
    let is_dir = std::path::Path::new(&directory).is_dir();
    // Discovery spawns git; it runs off the runtime's thread.
    let resolved = tokio::task::spawn_blocking(move || crate::project::discover(&directory)).await.expect("project discovery");
    // A missing directory is not persisted as a project: FileSystem rejects the location right after. Its
    // lifecycle still activates the project first, which announces one already recorded (an imported session's).
    if !is_dir {
        let db = app.db.lock().unwrap();
        app.projects.activate(&db, &app.bus, &resolved.id, Some(key.clone())).map_err(crate::app::fatal)?;
        return Err(not_found(&key.directory));
    }
    let Ok(root) = std::fs::canonicalize(&key.directory).map(|p| p.to_string_lossy().into_owned()) else {
        return Err(not_found(&key.directory));
    };
    // FileSystem's boot probes the real directory's readability (Linux realpath succeeds on an unreadable one), after
    // the project is recorded and activated: `DirectoryAccessDeniedError`, which no request maps, so the server dies
    // on it. Workspace placements skip the probe.
    if key.workspace_id.is_none() && !readable(&root) {
        let mut db = app.db.lock().unwrap();
        app.projects.persist(&mut db, &app.bus, &resolved).map_err(crate::app::fatal)?;
        app.projects.activate(&db, &app.bus, &resolved.id, Some(key.clone())).map_err(crate::app::fatal)?;
        return Err(crate::app::defect(format!("Access denied to directory: {}", key.directory)));
    }
    // Read the config and plugin sources before taking the database.
    let inventory = crate::inventory::Shared::load(key, &resolved.directory).await?;
    {
        let mut db = app.db.lock().unwrap();
        app.projects.persist(&mut db, &app.bus, &resolved).map_err(crate::app::fatal)?;
    }
    let version_control =
        crate::vcs::Service::new(resolved.vcs.as_ref().map(|v| v.kind), &key.directory, &resolved.directory);
    // A location's `ToolOutput` starts the global `tool-output-cleanup` sweep; its goal plugin follows session events.
    crate::tool::output::start_cleanup();
    crate::session::goal::start(app);
    let location = Location {
        directory: key.directory.clone(),
        workspace_id: key.workspace_id.clone(),
        project: ProjectRef { id: resolved.id.clone(), directory: resolved.directory, canonical: resolved.canonical },
        vcs: resolved.vcs,
        inventory,
        search: crate::fs::Search::new(&root),
        root,
        version_control,
        permission: Default::default(),
        forms: Default::default(),
        shell: crate::shell::Service::new(app, key.clone(), &resolved.id),
        pty: crate::pty::Service::new(app, key.clone()),
        snapshot: Default::default(),
        watcher: Default::default(),
        retained: Default::default(),
        integration: Default::default(),
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
