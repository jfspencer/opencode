//! The server's shared state: the database, the event bus and the per-location services.

use std::sync::Mutex;

use rusqlite::Connection;

use crate::bus::Bus;
use crate::http::Failure;
use crate::location::Locations;
use crate::project::Projects;

/// The build identity (`OPENCODE_VERSION`, `OPENCODE_CHANNEL` at compile time, as the TS build defines them).
pub const VERSION: &str = match option_env!("OPENCODE_VERSION") {
    Some(v) => v,
    None => "local",
};
pub const CHANNEL: &str = match option_env!("OPENCODE_CHANNEL") {
    Some(v) => v,
    None => "local",
};

pub struct App {
    /// The server itself, for work that outlives a request (session execution, title generation).
    pub me: std::sync::Weak<App>,
    pub password: Option<String>,
    /// The server's working directory: where requests without a location are placed.
    pub cwd: String,
    pub db: Mutex<Connection>,
    pub bus: Bus,
    pub projects: Projects,
    pub locations: Locations,
    pub sessions: crate::session::Sessions,
}

impl App {
    pub fn arc(&self) -> std::sync::Arc<App> {
        self.me.upgrade().expect("the server outlives its work")
    }
}

/// An unexpected storage failure: the TS server dies on these (`Effect.orDie`), answering 500.
pub fn fatal(error: rusqlite::Error) -> Failure {
    defect(format!("database error: {error}"))
}

/// Any failure the TS server dies on (`Effect.orDie`): an empty 500.
pub fn defect(error: impl std::fmt::Display) -> Failure {
    eprintln!("{error}");
    Failure::defect()
}

static URLS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Records the addresses the server listens on (`ServerInfo.urls`), once it is bound.
pub fn set_urls(urls: Vec<String>) {
    let _ = URLS.set(urls);
}

pub fn urls() -> Vec<String> {
    URLS.get().cloned().unwrap_or_default()
}
