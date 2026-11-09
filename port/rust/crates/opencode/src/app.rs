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
    eprintln!("database error: {error}");
    let mut o = crate::json::Obj::new();
    o.str("_tag", "UnknownError");
    o.str("message", "Internal server error");
    Failure::new(500, o.done())
}
