//! The opencode executable (packages/cli). TODO(port): every command but `serve`.

mod api;
mod app;
mod auth;
mod bus;
mod collate;
mod db;
mod form;
mod fs;
mod fuzzysort;
mod generate;
mod handlers;
mod inventory;
mod http;
mod id;
mod json;
mod llm;
mod location;
mod permission;
mod process;
mod project;
mod project_icon;
mod session;
mod text;
mod tool;
mod vcs;
mod worktree;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::net::TcpListener;

use crate::app::App;

struct Serve {
    hostname: String,
    port: Option<u16>,
}

fn usage(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

fn parse_serve(args: &[String]) -> Serve {
    let mut serve = Serve { hostname: "127.0.0.1".into(), port: None };
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let mut value = || {
            inline.clone().unwrap_or_else(|| {
                index += 1;
                args.get(index).cloned().unwrap_or_else(|| usage(&format!("{name} needs a value")))
            })
        };
        match name {
            "--hostname" => serve.hostname = value(),
            "--port" => {
                let port = value();
                serve.port = Some(port.parse().unwrap_or_else(|_| usage(&format!("invalid port {port}"))));
            }
            other => usage(&format!("unknown option {other}")),
        }
        index += 1;
    }
    serve
}

fn truthy(value: Option<String>) -> bool {
    value.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// `Global.Path.data` (packages/util/src/global.ts): `$XDG_DATA_HOME/opencode`.
fn data_directory() -> String {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    format!("{base}/opencode")
}

/// `databasePath` (packages/cli/src/database-path.ts).
fn database_path() -> String {
    let filename = std::env::var("OPENCODE_DB").unwrap_or_else(|_| {
        let shared = ["latest", "dev", "beta", "next", "prod"].contains(&app::CHANNEL)
            || truthy(std::env::var("OPENCODE_DISABLE_CHANNEL_DB").ok());
        if shared {
            "opencode.db".into()
        } else {
            let channel: String = app::CHANNEL
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '-' })
                .collect();
            format!("opencode-{channel}.db")
        }
    });
    if filename == ":memory:" { filename } else { project::absolutize(&format!("{}/{filename}", data_directory())) }
}

fn random_password() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    id::random(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

async fn bind(hostname: &str, port: Option<u16>) -> std::io::Result<TcpListener> {
    if let Some(port) = port {
        return TcpListener::bind((hostname, port)).await;
    }
    // Without a port, the first free one from 4096 up.
    let mut port = 4096u16;
    loop {
        match TcpListener::bind((hostname, port)).await {
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && port < 65_535 => port += 1,
            other => return other,
        }
    }
}

fn format_address(address: SocketAddr) -> String {
    match address {
        SocketAddr::V4(a) => format!("http://{}:{}", a.ip(), a.port()),
        SocketAddr::V6(a) => format!("http://[{}]:{}", a.ip(), a.port()),
    }
}

/// `Global` (packages/util/src/global.ts) creates its directories when the server starts.
fn create_global_directories() {
    let home = std::env::var("HOME").unwrap_or_default();
    let xdg = |name: &str, fallback: &str| {
        std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| format!("{home}/{fallback}"))
    };
    let data = format!("{}/opencode", xdg("XDG_DATA_HOME", ".local/share"));
    let cache = format!("{}/opencode", xdg("XDG_CACHE_HOME", ".cache"));
    let state = format!("{}/opencode", xdg("XDG_STATE_HOME", ".local/state"));
    let config = generate::global_config();
    for dir in [data.clone(), config, state, format!("{data}/log"), format!("{cache}/bin"), format!("{data}/repos")] {
        let _ = std::fs::create_dir_all(dir);
    }
    session::instructions::global_tmp();
}

fn serve(options: Serve) {
    create_global_directories();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    runtime.block_on(async move {
        let environment_password = std::env::var("OPENCODE_PASSWORD")
            .or_else(|_| std::env::var("OPENCODE_SERVER_PASSWORD"))
            .ok();
        let password = environment_password.clone().unwrap_or_else(random_password);
        let db = db::open(&database_path()).unwrap_or_else(|e| usage(&e));
        let cwd = project::absolutize(".");
        let app = Arc::new_cyclic(|me| App {
            me: me.clone(),
            password: Some(password.clone()),
            cwd,
            db: Mutex::new(db),
            bus: bus::Bus::new(),
            projects: project::Projects::default(),
            locations: location::Locations::default(),
            sessions: session::Sessions::default(),
        });
        let listener = match bind(&options.hostname, options.port).await {
            Ok(listener) => listener,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => usage(&format!(
                "Port {} on {} is already in use. Choose another port with --port.",
                options.port.unwrap_or(0),
                options.hostname
            )),
            Err(e) => usage(&e.to_string()),
        };
        let address = http::local_address(&listener).expect("bound address");
        // TODO(port): `connectionURLs` lists every interface's address when bound to 0.0.0.0 or ::.
        app::set_urls(vec![format_address(address)]);
        println!("server listening on {}", format_address(address));
        if environment_password.is_none() {
            println!("server password {password}");
        }
        // Turns a previous process left mid-way continue (restart continuity).
        session::execution::resume_suspended(&app);
        let server = tokio::spawn(http::serve(app, listener));
        shutdown_signal().await;
        server.abort();
    });
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate()).expect("signal handler");
    let mut interrupt = signal(SignalKind::interrupt()).expect("signal handler");
    tokio::select! {
        _ = terminate.recv() => {}
        _ = interrupt.recv() => {}
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve(parse_serve(&args[1..])),
        Some("--version") | Some("-v") => println!("{}", app::VERSION),
        _ => usage("usage: opencode serve [--hostname <host>] [--port <port>]"),
    }
}
