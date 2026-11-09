//! `opencode serve` (packages/cli/src/commands/handlers/serve.ts, server-process.ts): the API server in the
//! foreground, as a standalone server leased over stdio, or as the background service that registers itself
//! for discovery.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::TcpListener;

use super::out::{Fail, Result, stdout};
use super::parse::Input;
use super::{paths, service};
use crate::app::App;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Default,
    Service,
    Stdio,
}

fn random_password() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    crate::id::random(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn format_address(address: SocketAddr) -> String {
    match address {
        SocketAddr::V4(a) => format!("http://{}:{}", a.ip(), a.port()),
        SocketAddr::V6(a) => format!("http://[{}]:{}", a.ip(), a.port()),
    }
}

fn service_url(hostname: &str, port: u16) -> String {
    if hostname.contains(':') { format!("http://[{hostname}]:{port}") } else { format!("http://{hostname}:{port}") }
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

fn set_env(key: &str, value: &str) {
    // SAFETY: the CLI sets its environment before it starts any thread that reads it.
    unsafe { std::env::set_var(key, value) }
}

fn remove_env(key: &str) {
    // SAFETY: as for `set_env`.
    unsafe { std::env::remove_var(key) }
}

/// Once registered, the service outlives the client that started it, whose end of the stderr pipe closes: later
/// writes go nowhere, as they do for the TS service.
fn detach_stderr() {
    // SAFETY: plain descriptor calls on a path literal.
    unsafe {
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
        if null >= 0 {
            libc::dup2(null, 2);
            libc::close(null);
        }
    }
}

enum Ended {
    Signal(i32),
    Stdin,
    Shutdown,
}

async fn signals() -> i32 {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate()).expect("signal handler");
    let mut interrupt = signal(SignalKind::interrupt()).expect("signal handler");
    tokio::select! {
        _ = terminate.recv() => 143,
        _ = interrupt.recv() => 130,
    }
}

/// Resolves when stdin reaches its end (the lease of a standalone server). Read on a thread of its own: a blocking
/// read is the only portable way to see EOF on any kind of descriptor.
async fn stdin_closed() {
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buffer = [0u8; 1024];
        while let Ok(n) = stdin.read(&mut buffer) {
            if n == 0 {
                break;
            }
        }
        let _ = sender.send(());
    });
    let _ = receiver.await;
}

pub async fn run(input: &Input) -> Result {
    let service = input.flag("service");
    let stdio = input.flag("stdio");
    if service && stdio {
        return Err(Fail::error("--service and --stdio cannot be combined"));
    }
    let mode = if service { Mode::Service } else if stdio { Mode::Stdio } else { Mode::Default };
    paths::create();
    if mode == Mode::Service {
        let _ = std::env::set_current_dir(paths::home());
    }
    let file = (mode == Mode::Service).then(service::migrate_registrations);
    let config = if mode == Mode::Service { service::read_config() } else { crate::json::object() };
    let hostname = input
        .str("hostname")
        .map(String::from)
        .or_else(|| crate::json::get_str(&config, "hostname").map(String::from))
        .unwrap_or_else(|| "127.0.0.1".into());
    let port: Option<u16> = input
        .int("port")
        .map(|p| p as u16)
        .or_else(|| crate::json::get_num(&config, "port").map(|p| p as u16))
        .or_else(|| (mode == Mode::Service).then(service::default_port));
    if let (Some(file), Some(port)) = (&file, port) {
        if service::incumbent(file, &service_url(&hostname, port)).await.is_some() {
            return Ok(0);
        }
    }
    for (key, value) in service::process_env(&config) {
        set_env(&key, &value);
    }
    let environment = std::env::var("OPENCODE_PASSWORD").or_else(|_| std::env::var("OPENCODE_SERVER_PASSWORD")).ok();
    if mode == Mode::Stdio {
        remove_env("OPENCODE_PASSWORD");
        remove_env("OPENCODE_SERVER_PASSWORD");
    }
    let stored = crate::json::get_str(&config, "password").filter(|p| !p.is_empty()).map(String::from);
    let password = match mode {
        Mode::Service => stored.clone().unwrap_or_else(random_password),
        _ => environment.clone().unwrap_or_else(random_password),
    };
    if password.is_empty() {
        return Err(Fail::error("Missing server password"));
    }
    let cors: Vec<String> = if !input.list("cors").is_empty() {
        input.list("cors").to_vec()
    } else {
        crate::json::get(&config, "cors")
            .and_then(crate::json::array)
            .map(|items| items.iter().filter_map(crate::json::as_str).map(String::from).collect())
            .unwrap_or_default()
    };
    crate::cors::set_origins(cors);

    let listener = match bind(&hostname, port).await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => match &file {
            None => {
                return Err(Fail::error(format!(
                    "Port {} on {hostname} is already in use. Choose another port with --port.",
                    port.unwrap_or(0)
                )));
            }
            Some(file) => match contend(file, &hostname, port.unwrap_or(0)).await? {
                Some(listener) => listener,
                None => return Ok(0),
            },
        },
        Err(e) => return Err(Fail::error(e.to_string())),
    };

    let db = crate::db::open(&paths::database()).map_err(Fail::error)?;
    let cwd = crate::project::absolutize(".");
    let app = Arc::new_cyclic(|me| App {
        me: me.clone(),
        password: Some(password.clone()),
        cwd,
        db: Mutex::new(db),
        bus: crate::bus::Bus::new(),
        projects: crate::project::Projects::default(),
        locations: crate::location::Locations::default(),
        sessions: crate::session::Sessions::default(),
    });
    let address = crate::http::local_address(&listener).expect("bound address");
    let url = format_address(address);
    // TODO(port): `connectionURLs` lists every interface's address when bound to 0.0.0.0 or ::.
    crate::app::set_urls(vec![url.clone()]);

    let registration = match &file {
        Some(file) => {
            if stored.is_none() {
                service::config_password(Some(&password))?;
            }
            let info = service::Info {
                id: Some(crate::id::uuid()),
                version: Some(crate::app::VERSION.into()),
                url: url.clone(),
                pid: std::process::id() as i64,
                password: Some(password.clone()),
            };
            service::register(file, &info).map_err(|e| Fail::error(e.to_string()))?;
            Some((file.clone(), info))
        }
        None => None,
    };

    if mode == Mode::Stdio {
        let mut o = crate::json::Obj::new();
        o.str("url", &url);
        stdout(&format!("{}\n", crate::json::text(&o.done())));
    } else {
        stdout(&format!("server listening on {url}\n"));
    }
    if mode == Mode::Default && environment.is_none() {
        stdout(&format!("server password {password}\n"));
    }
    if registration.is_some() {
        detach_stderr();
    }

    // Turns a previous process left mid-way continue (restart continuity).
    crate::session::execution::resume_suspended(&app);
    let server = tokio::spawn(crate::http::serve(app, listener));
    let ended = match (mode, &registration) {
        (Mode::Stdio, _) => tokio::select! {
            code = signals() => Ended::Signal(code),
            _ = stdin_closed() => Ended::Stdin,
        },
        (_, Some((file, info))) => tokio::select! {
            code = signals() => Ended::Signal(code),
            _ = replaced(file, info) => Ended::Shutdown,
        },
        _ => Ended::Signal(signals().await),
    };
    server.abort();
    if let Some((file, info)) = &registration {
        if service::owns(file, info) {
            let _ = std::fs::remove_file(file);
        }
    }
    Ok(match ended {
        Ended::Signal(code) => code,
        Ended::Stdin | Ended::Shutdown => 0,
    })
}

/// The registration check: every five seconds, until another instance's registration replaces this one.
async fn replaced(file: &str, info: &service::Info) {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if !service::owns(file, info) {
            return;
        }
    }
}

/// The service's port is taken: wait up to 15 s for the incumbent to answer (then leave it be), retrying the
/// bind meanwhile.
async fn contend(file: &str, hostname: &str, port: u16) -> Result<Option<TcpListener>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let url = service_url(hostname, port);
    while Instant::now() < deadline {
        if service::incumbent(file, &url).await.is_some() {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        if Instant::now() >= deadline {
            break;
        }
        if let Ok(listener) = TcpListener::bind((hostname, port)).await {
            return Ok(Some(listener));
        }
    }
    let owner = super::port_owner(port, hostname).await.unwrap_or_else(|| "another process".into());
    Err(Fail::error(format!(
        "Managed service port {port} on {hostname} is already in use by {owner}. Configure another port with `opencode service set port <port>` and start the service again."
    )))
}
