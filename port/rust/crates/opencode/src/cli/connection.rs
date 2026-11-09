//! Which server a command talks to (packages/cli/src/services/server-connection.ts, standalone.ts): an explicit
//! `--server`, a private server started for the command (`--standalone`, or the service disabled), or the
//! background service (started on demand unless the command must not leave one behind).

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};

use super::client::{Client, ClientError, Endpoint};
use super::out::{Fail, Result, stderr};
use super::parse::Globals;
use super::service;

/// A private server started for this command; it ends with the command (its stdin is the lease).
pub struct Private {
    child: Child,
    lease: Option<ChildStdin>,
}

#[allow(dead_code)] // `acp`, next
impl Private {
    /// Ends the lease and waits for the server, as the command's scope does on exit (SIGTERM, SIGKILL after 3 s).
    pub async fn close(mut self) {
        self.lease.take();
        if let Some(pid) = self.child.id() {
            // SAFETY: signalling our own child.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
        if tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await.is_err() {
            let _ = self.child.kill().await;
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Resolves when the private server exits on its own: its exit code, or the terminating signal's name.
    pub async fn exited(&mut self) -> std::result::Result<i32, String> {
        use std::os::unix::process::ExitStatusExt;
        match self.child.wait().await {
            Ok(status) => match status.code() {
                Some(code) => Ok(code),
                None => Err(service::signal_name(status.signal().unwrap_or(0))),
            },
            Err(error) => Err(error.to_string()),
        }
    }
}

pub struct Resolved {
    pub endpoint: Endpoint,
    /// The background service (it can be reconnected to or restarted).
    #[allow(dead_code)] // `run`, next
    pub service: bool,
    pub private: Option<Private>,
}

impl Resolved {
    pub fn client(&self) -> Client {
        Client::new(&self.endpoint)
    }

    pub async fn close(self) {
        if let Some(private) = self.private {
            private.close().await;
        }
    }
}

/// What the server-choosing flags and options of a command say.
#[derive(Default)]
pub struct Args {
    pub server: Option<String>,
    pub standalone: Option<bool>,
    /// `mismatch: "replace" | "error"`; "ignore" otherwise.
    pub check_version: bool,
    /// When false, a missing service means a private server instead of starting the service.
    pub no_autostart: bool,
}

fn random_password() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    crate::id::random(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `Env.password`.
pub fn env_password() -> Option<String> {
    std::env::var("OPENCODE_PASSWORD").or_else(|_| std::env::var("OPENCODE_SERVER_PASSWORD")).ok()
}

/// `Standalone.start`: `opencode serve --stdio --port 0` with a fresh password, ready once it prints its URL.
pub async fn standalone() -> Result<(Endpoint, Private)> {
    let password = random_password();
    let executable = std::env::current_exe().map_err(|e| Fail::error(e.to_string()))?;
    let mut command = tokio::process::Command::new(executable);
    command
        .args(["serve", "--stdio", "--port", "0"])
        .current_dir(super::startup_directory())
        .env("OPENCODE_PASSWORD", &password)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(if std::env::var("OPENCODE_PRINT_LOGS").as_deref() == Ok("1") {
            std::process::Stdio::inherit()
        } else {
            std::process::Stdio::null()
        });
    let mut child = command.spawn().map_err(|e| Fail::error(e.to_string()))?;
    let lease = child.stdin.take();
    let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let ready = lines.next_line().await.ok().flatten();
    // Keep draining stdout after readiness so later writes cannot hit a closed pipe.
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    let private = Private { child, lease };
    let Some(line) = ready else {
        return Err(Fail::error("Standalone server exited before reporting readiness"));
    };
    let url = crate::json::parse(&line)
        .and_then(|v| crate::json::get_str(&v, "url").map(String::from))
        .ok_or_else(|| Fail::named("SchemaError", "Expected a valid JSON string"))?;
    Ok((Endpoint { url, password: Some(password) }, private))
}

/// `connectError`.
fn connect_error(endpoint: &Endpoint, error: &ClientError) -> Fail {
    if matches!(error, ClientError::Declared { status: 401, .. }) {
        return Fail::error(match endpoint.password {
            None => format!("Server at {} requires a password; set OPENCODE_PASSWORD", endpoint.url),
            Some(_) => format!("Server at {} rejected the password", endpoint.url),
        });
    }
    if let Some(cause) = error.transport_cause() {
        return Fail::error(format!("Could not reach server at {}: {cause}", endpoint.url));
    }
    Fail::error(format!(
        "Server at {} did not provide a compatible V2 health response: {}",
        endpoint.url,
        error.message()
    ))
}

/// `ServerConnection.resolve`.
pub async fn resolve(globals: &Globals, args: Args) -> Result<Resolved> {
    let server = args.server.or_else(|| globals.server.clone());
    let standalone_flag = args.standalone.unwrap_or(globals.standalone);
    if server.is_some() && standalone_flag {
        return Err(Fail::error("--server and --standalone cannot be combined"));
    }
    if let Some(url) = server {
        let endpoint = Endpoint { url, password: env_password() };
        let client = Client::new(&endpoint);
        let health = client
            .call_with("server.info", &[], None, None, &[], Some(Duration::from_secs(5)))
            .await
            .map_err(|error| connect_error(&endpoint, &error))?;
        let version = crate::json::get_str(&health, "version").unwrap_or("");
        if version != crate::app::VERSION {
            stderr(&format!(
                "Warning: Server at {} has version {version}; this client is {}. Continuing anyway.\n",
                endpoint.url,
                crate::app::VERSION
            ));
        }
        return Ok(Resolved { endpoint, service: false, private: None });
    }
    if standalone_flag || crate::json::get_bool(&service::read_config(), "disabled") == Some(true) {
        let (endpoint, private) = standalone().await?;
        return Ok(Resolved { endpoint, service: false, private: Some(private) });
    }
    let file = service::migrate_registrations();
    if args.no_autostart {
        return match service::discover(&file, None).await {
            Some(endpoint) => Ok(Resolved { endpoint, service: true, private: None }),
            None => {
                let (endpoint, private) = standalone().await?;
                Ok(Resolved { endpoint, service: false, private: Some(private) })
            }
        };
    }
    let env = service::process_env(&service::read_config());
    let version = args.check_version.then_some(crate::app::VERSION);
    let endpoint = service::ensure(&file, version, &env, &mut |_| {}).await?;
    Ok(Resolved { endpoint, service: true, private: None })
}
