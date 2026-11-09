//! The background service: its configuration file (packages/cli/src/services/service-config.ts), its registration
//! (service-registration.ts) and the client side of its lifecycle (packages/client/src/effect/service.ts,
//! service-probe.ts, service-contender.ts, pty-handoff.ts): discover, ensure (spawn contenders until one
//! registers), stop. Every path is under the process's XDG directories, named after the build's channel.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opencode_protocol::Json;

use super::client::{Endpoint, fetch};
use super::out::{Fail, Result, pretty};
use super::paths;
use crate::json::{self, Obj};

const CHANNEL: &str = crate::app::CHANNEL;
const VERSION: &str = crate::app::VERSION;

/// `filename(channel)`.
pub fn filename() -> String {
    if matches!(CHANNEL, "latest" | "dev" | "beta" | "next") {
        return "service.json".into();
    }
    let channel: String = CHANNEL.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '-' }).collect();
    format!("service-{channel}.json")
}

fn sha1_hex(text: &str) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// `defaultPort(channel)` on Linux and macOS.
pub fn default_port() -> u16 {
    match CHANNEL {
        "latest" | "dev" | "beta" | "next" => 0xc0de,
        "local" => 0xc0df,
        channel => {
            let prefix = u64::from_str_radix(&sha1_hex(channel)[..8], 16).unwrap_or(0);
            (10_000 + prefix % 50_000) as u16
        }
    }
}

fn legacy_filename() -> Option<String> {
    if matches!(CHANNEL, "latest" | "local") {
        return None;
    }
    Some(format!("service-{}.json", sha1_hex(CHANNEL)))
}

/// The registration file clients discover the service by.
pub fn registration_file() -> String {
    format!("{}/{}", paths::state(), filename())
}

fn config_file() -> String {
    format!("{}/{}", paths::config(), filename())
}

// --- the configuration file ---

fn decode_config(value: &Json, legacy: bool) -> Option<Json> {
    let Json::Object(object) = value else { return None };
    let mut out = Obj::new();
    let get = |key: &str| object.get(key);
    if let Some(v) = get("disabled") {
        let Json::Bool(_) = v else { return None };
        out.set("disabled", v.clone());
    }
    match (get("remote"), legacy) {
        (Some(Json::Bool(b)), true) => {
            out.set("remote", Json::Bool(*b));
        }
        (_, true) => return None,
        (Some(v), false) => {
            let route = json::get_str(v, "route")?;
            let mut remote = Obj::new();
            remote.str("route", route);
            out.set("remote", remote.done());
        }
        (None, false) => {}
    }
    if let Some(v) = get("remoteAccepted") {
        let Json::Bool(true) = v else { return None };
        out.set("remoteAccepted", Json::Bool(true));
    }
    if let Some(v) = get("hostname") {
        let Json::String(_) = v else { return None };
        out.set("hostname", v.clone());
    }
    if let Some(v) = get("port") {
        let Json::Number(n) = v else { return None };
        if n.trunc() != *n || *n < 1.0 || *n > 65_535.0 {
            return None;
        }
        out.set("port", v.clone());
    }
    if let Some(v) = get("password") {
        let Json::String(_) = v else { return None };
        out.set("password", v.clone());
    }
    if let Some(v) = get("cors") {
        let Json::Array(items) = v else { return None };
        if !items.iter().all(|i| matches!(i, Json::String(_))) {
            return None;
        }
        out.set("cors", v.clone());
    }
    if let Some(v) = get("env") {
        let Json::Object(entries) = v else { return None };
        if !entries.values().all(|i| matches!(i, Json::String(_))) {
            return None;
        }
        out.set("env", v.clone());
    }
    Some(out.done())
}

fn read_text(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Writes a private file atomically (temp file then rename), as the TS writers do.
fn write_private(path: &str, temp: &str, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(temp)?;
    file.write_all(text.as_bytes())?;
    drop(file);
    std::fs::rename(temp, path)
}

/// `flag: "wx"`: only when the file does not exist yet.
fn write_new(path: &str, text: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Ok(mut file) = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}

fn write_config(value: &Json) -> Result<()> {
    let file = config_file();
    let temp = format!("{file}.{}.tmp", std::process::id());
    write_private(&file, &temp, &(pretty(value) + "\n")).map_err(|e| Fail::error(e.to_string()))
}

fn random_route() -> String {
    let mut bytes = [0u8; 8];
    crate::id::random(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `ServiceConfig.read()`: the decoded file, or `{}`; a legacy boolean `remote` is repaired in place.
pub fn read_config() -> Json {
    let file = config_file();
    if let Some(legacy) = legacy_filename() {
        let legacy = format!("{}/{legacy}", paths::config());
        if let Some(text) = read_text(&legacy) {
            if json::parse(&text).and_then(|v| decode_config(&v, false)).is_some() {
                write_new(&file, &text);
            }
        }
    }
    let Some(text) = read_text(&file) else { return json::object() };
    let Some(value) = json::parse(&text) else { return json::object() };
    if let Some(info) = decode_config(&value, false) {
        return info;
    }
    let Some(mut legacy) = decode_config(&value, true) else { return json::object() };
    let enabled = json::remove(&mut legacy, "remote") == Some(Json::Bool(true));
    if enabled {
        let mut remote = Obj::new();
        remote.str("route", &random_route());
        json::set(&mut legacy, "remote", remote.done());
    }
    let _ = write_config(&legacy);
    legacy
}

/// `ServiceConfig.processEnv`: the saved environment without `XDG_STATE_HOME`.
pub fn process_env(config: &Json) -> Vec<(String, String)> {
    let Some(Json::Object(env)) = json::get(config, "env") else { return vec![] };
    env.iter()
        .filter(|(k, _)| k.as_str() != "XDG_STATE_HOME")
        .filter_map(|(k, v)| json::as_str(v).map(|v| (k.clone(), v.to_owned())))
        .collect()
}

fn random_secret() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    crate::id::random(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `ServiceConfig.password(value?)`: the stored password, created when missing; `value` replaces it.
pub fn config_password(value: Option<&str>) -> Result<String> {
    let mut existing = read_config();
    if value.is_none() {
        if let Some(password) = json::get_str(&existing, "password").filter(|p| !p.is_empty()) {
            return Ok(password.to_owned());
        }
    }
    let next = value.map(String::from).unwrap_or_else(random_secret);
    json::set(&mut existing, "password", Json::String(next.clone()));
    write_config(&existing)?;
    Ok(next)
}

fn config_key(key: &str) -> Result<&str> {
    match key {
        "disabled" | "remote" | "hostname" | "port" | "password" | "cors" | "env" => Ok(key),
        _ => Err(Fail::error(format!("Unknown service config key: {key}"))),
    }
}

/// `opencode service get`.
pub fn get(key: Option<&str>, name: Option<&str>) -> Result<String> {
    let Some(key) = key else {
        let mut safe = read_config();
        json::remove(&mut safe, "password");
        if json::get(&safe, "remote").is_some() {
            json::set(&mut safe, "remote", json::object());
        }
        return Ok(pretty(&safe));
    };
    let selected = config_key(key)?;
    if selected != "env" && name.is_some() {
        return Err(Fail::error(format!("Usage: opencode service get {selected}")));
    }
    let config = read_config();
    Ok(match selected {
        "disabled" => json::get_bool(&config, "disabled").unwrap_or(false).to_string(),
        "remote" => json::get(&config, "remote").is_some().to_string(),
        "hostname" => json::get_str(&config, "hostname").unwrap_or("").to_owned(),
        "port" => json::get(&config, "port").map(json::text).unwrap_or_default(),
        "password" => config_password(None)?,
        "cors" => pretty(json::get(&config, "cors").unwrap_or(&Json::Array(vec![]))),
        _ => {
            let env = json::get(&config, "env").cloned().unwrap_or_else(json::object);
            match name {
                None => pretty(&env),
                Some(name) => json::get_str(&env, name).unwrap_or("").to_owned(),
            }
        }
    })
}

fn valid_origin(origin: &str) -> bool {
    // URL.parse(origin).origin === origin, for http(s).
    let Some((scheme, rest)) = origin.split_once("://") else { return false };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ']) || rest.chars().any(|c| c.is_ascii_uppercase()) {
        return false;
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(']') || rest.starts_with('[') => (host, Some(port)),
        _ => (rest, None),
    };
    if host.is_empty() {
        return false;
    }
    match port {
        None => true,
        Some(port) => {
            let Ok(number) = port.parse::<u32>() else { return false };
            let default = if scheme == "http" { 80 } else { 443 };
            !port.is_empty() && number <= 65_535 && number != default && !port.starts_with('0')
        }
    }
}

/// `opencode service set`.
pub async fn set(key: &str, value: &str, nested: Option<&str>) -> Result<()> {
    let selected = config_key(key)?;
    if selected != "env" && nested.is_some() {
        return Err(Fail::error(format!("Usage: opencode service set {selected} <value>")));
    }
    match selected {
        "disabled" => {
            if value != "true" && value != "false" {
                return Err(Fail::error("Disabled must be true or false"));
            }
            if value == "true" {
                stop(false).await?;
            }
            let mut config = read_config();
            json::set(&mut config, "disabled", Json::Bool(value == "true"));
            write_config(&config)
        }
        "remote" => {
            if value != "true" && value != "false" {
                return Err(Fail::error("Remote must be true or false"));
            }
            if value == "true" {
                // TODO(port): remote access through OpenTunnel (RemoteTunnel.ensure) is not ported.
                return Err(Fail::error("Remote access is not available in this build"));
            }
            stop(false).await?;
            disable_remote()
        }
        "hostname" => {
            stop(false).await?;
            let mut config = read_config();
            json::set(&mut config, "hostname", Json::String(value.into()));
            write_config(&config)
        }
        "port" => {
            let port = opencode_protocol::wire::number::js_string_to_number(value);
            if port.trunc() != port || !(1.0..=65_535.0).contains(&port) {
                return Err(Fail::error("Port must be between 1 and 65535"));
            }
            stop(false).await?;
            let mut config = read_config();
            json::set(&mut config, "port", Json::Number(port));
            write_config(&config)
        }
        "password" => {
            stop(false).await?;
            config_password(Some(value)).map(|_| ())
        }
        "env" => {
            let Some(nested) = nested else {
                return Err(Fail::error("Usage: opencode service set env <key> <value>"));
            };
            stop(false).await?;
            let mut config = read_config();
            let mut env = json::get(&config, "env").cloned().unwrap_or_else(json::object);
            json::set(&mut env, value, Json::String(nested.into()));
            json::set(&mut config, "env", env);
            write_config(&config)
        }
        _ => {
            let cors: Vec<String> = value.split(',').map(|o| o.trim().to_owned()).collect();
            if !cors.iter().all(|o| valid_origin(o)) {
                return Err(Fail::error(
                    "CORS must be a comma-separated list of HTTP(S) origins without paths or trailing slashes",
                ));
            }
            stop(false).await?;
            let mut config = read_config();
            json::set(&mut config, "cors", Json::Array(cors.into_iter().map(Json::String).collect()));
            write_config(&config)
        }
    }
}

fn disable_remote() -> Result<()> {
    let mut config = read_config();
    if json::remove(&mut config, "remote").is_some() {
        json::set(&mut config, "remoteAccepted", Json::Bool(true));
    }
    write_config(&config)
}

/// `opencode service unset`.
pub async fn unset(key: &str, name: Option<&str>) -> Result<()> {
    let selected = config_key(key)?;
    if selected != "env" && name.is_some() {
        return Err(Fail::error(format!("Usage: opencode service unset {selected}")));
    }
    match selected {
        "disabled" => {
            let mut config = read_config();
            json::remove(&mut config, "disabled");
            write_config(&config)
        }
        "remote" => {
            stop(false).await?;
            disable_remote()
        }
        "env" => {
            let Some(name) = name else { return Err(Fail::error("Usage: opencode service unset env <key>")) };
            stop(false).await?;
            let mut config = read_config();
            let mut env = json::get(&config, "env").cloned().unwrap_or_else(json::object);
            json::remove(&mut env, name);
            json::remove(&mut config, "env");
            if env.as_object().is_some_and(|o| !o.is_empty()) {
                json::set(&mut config, "env", env);
            }
            write_config(&config)
        }
        other => {
            stop(false).await?;
            let mut config = read_config();
            json::remove(&mut config, other);
            write_config(&config)
        }
    }
}

// --- the registration ---

/// `Service.Info`: what the registration file holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub id: Option<String>,
    pub version: Option<String>,
    pub url: String,
    pub pid: i64,
    pub password: Option<String>,
}

impl Info {
    fn decode(text: &str) -> Option<Info> {
        let value = json::parse(text)?;
        let optional = |key: &str| match json::get(&value, key) {
            None => Some(None),
            Some(Json::String(s)) => Some(Some(s.clone())),
            Some(_) => None,
        };
        let pid = json::get_num(&value, "pid")?;
        if pid.trunc() != pid || pid <= 0.0 || pid > 9_007_199_254_740_991.0 {
            return None;
        }
        Some(Info { id: optional("id")?, version: optional("version")?, url: json::get_str(&value, "url")?.to_owned(), pid: pid as i64, password: optional("password")? })
    }

    pub fn encode(&self) -> String {
        let mut o = Obj::new();
        o.opt_str("id", self.id.as_deref())
            .opt_str("version", self.version.as_deref())
            .str("url", &self.url)
            .int("pid", self.pid)
            .opt_str("password", self.password.as_deref());
        json::text(&o.done())
    }

    /// `same()`: the same service instance.
    pub fn same(&self, other: &Info) -> bool {
        self.id == other.id && self.version == other.version && self.url == other.url && self.pid == other.pid
    }

    pub fn endpoint(&self) -> Endpoint {
        Endpoint { url: self.url.clone(), password: self.password.clone() }
    }
}

pub fn read_registration(file: &str) -> Option<Info> {
    Info::decode(&read_text(file)?)
}

fn version_belongs_to_channel(version: Option<&str>) -> bool {
    let Some(version) = version else { return false };
    if version == VERSION {
        return true;
    }
    let prefix = format!("0.0.0-{CHANNEL}-");
    let Some(rest) = version.strip_prefix(&prefix) else { return false };
    let mut parts = rest.splitn(2, '.');
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next();
    !whole.is_empty() && whole.bytes().all(|b| b.is_ascii_digit()) && fraction.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

/// `ServiceConfig.options()`: migrates legacy registrations, then names the registration file.
pub fn migrate_registrations() -> String {
    let file = registration_file();
    let state = paths::state();
    let mut legacy = vec![];
    if let Some(name) = legacy_filename() {
        legacy.push(format!("{state}/{name}"));
    }
    if filename() != "service.json" && CHANNEL != "local" {
        legacy.push(format!("{state}/service.json"));
    }
    for path in legacy {
        let Some(text) = read_text(&path) else { continue };
        let Some(info) = Info::decode(&text) else { continue };
        if version_belongs_to_channel(info.version.as_deref()) {
            write_new(&file, &text);
        }
    }
    file
}

/// What a registered owner's health answer says (`LocalService`).
#[derive(Clone, Debug)]
pub struct Local {
    pub info: Info,
    pub version: Option<String>,
    pub state: State,
    pub compatible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    Ready,
    Waiting,
    Failed,
}

/// `probeResult`: asks the owner at `url` for its health. Returns the service and whether the probe timed out.
pub async fn probe(info: &Info, url: &str, timeout: Duration) -> (Option<Local>, bool) {
    let endpoint = Endpoint { url: url.to_owned(), password: info.password.clone() };
    let target = format!("{}/api/info", url.trim_end_matches('/'));
    let answer = match tokio::time::timeout(timeout, fetch("GET", &target, &endpoint.headers(), None)).await {
        Err(_) => return (None, true),
        Ok(Err(_)) => return (None, false),
        Ok(Ok(answer)) => answer,
    };
    if answer.status == 404 {
        return (Some(Local { info: info.clone(), version: info.version.clone(), state: State::Ready, compatible: false }), false);
    }
    let Some(body) = json::parse(&answer.body) else { return (None, false) };
    let (Some(version), Some(pid)) = (json::get_str(&body, "version"), json::get_num(&body, "pid")) else {
        return (None, false);
    };
    if pid.trunc() != pid || pid < 0.0 || pid as i64 != info.pid {
        return (None, false);
    }
    if info.version.as_deref().is_some_and(|v| v != version) {
        return (None, false);
    }
    let state = match answer.status {
        200..=299 => State::Ready,
        500 => State::Failed,
        _ => State::Waiting,
    };
    (Some(Local { info: info.clone(), version: Some(version.to_owned()), state, compatible: true }), false)
}

fn matches_version(version: Option<&str>, required: Option<&str>) -> bool {
    match required {
        None => true,
        Some(required) => version == Some(required),
    }
}

/// `Service.discover`: a healthy, compatible registered service, never started here.
pub async fn discover(file: &str, version: Option<&str>) -> Option<Endpoint> {
    let info = read_registration(file)?;
    let (found, _) = probe(&info, &info.url, Duration::from_secs(2)).await;
    let found = found?;
    if found.state != State::Ready || !found.compatible || !matches_version(found.version.as_deref(), version) {
        return None;
    }
    Some(info.endpoint())
}

/// `Service.incumbent`: an authenticated compatible service answering at `url`, in any state.
pub async fn incumbent(file: &str, url: &str) -> Option<Endpoint> {
    let info = read_registration(file)?;
    let (found, _) = probe(&info, url, Duration::from_secs(2)).await;
    let found = found.filter(|f| f.compatible)?;
    Some(Endpoint { url: url.to_owned(), password: found.info.password })
}

fn alive(pid: i64) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn signal(pid: i64, signal: libc::c_int) {
    // SAFETY: sending a signal has no memory effects here.
    unsafe {
        libc::kill(pid as libc::pid_t, signal);
    }
}

async fn wait_stopped(pid: i64) -> bool {
    for _ in 0..=100 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !alive(pid)
}

/// `terminate`: SIGTERM the registered owner (SIGKILL after ~5 s), then drop its registration if still current.
async fn terminate(info: &Info, file: &str) -> Result<()> {
    let Some(current) = read_registration(file) else { return Ok(()) };
    if !current.same(info) {
        return Ok(());
    }
    signal(info.pid, libc::SIGTERM);
    if !wait_stopped(info.pid).await {
        signal(info.pid, libc::SIGKILL);
        if !wait_stopped(info.pid).await {
            return Err(Fail::error(format!("Server process {} is still running", info.pid)));
        }
    }
    if read_registration(file).is_some_and(|latest| latest.same(info)) {
        let _ = std::fs::remove_file(file);
    }
    Ok(())
}

fn handoff_file(file: &str) -> String {
    format!("{file}.pty-handoff")
}

/// `Service.stop` (with `pty: "clear"`, or `"handoff"` for a restart). TODO(port): persistent terminal handoff
/// (`PtyHandoff.prepare`) belongs to the shell slice; a replaced service's terminals start fresh.
pub async fn stop_file(file: &str) -> Result<()> {
    let info = read_registration(file);
    let _ = std::fs::remove_file(handoff_file(file));
    if let Some(info) = info {
        terminate(&info, file).await?;
    }
    Ok(())
}

/// `ServerConnection.shutdownPersistentPty`, then `Service.stop(ServiceConfig.options())`.
pub async fn stop(shutdown_pty: bool) -> Result<()> {
    let file = migrate_registrations();
    if shutdown_pty {
        if let Some(endpoint) = discover(&file, None).await {
            let client = super::client::Client::new(&endpoint);
            let _ = client.call("experimental.persistentPty.shutdown", &[], None, None).await;
        }
    }
    stop_file(&file).await
}

/// One startup attempt (`ServiceContender`).
struct Contender {
    child: tokio::process::Child,
    stderr: Arc<Mutex<Vec<u8>>>,
    eof: Arc<std::sync::atomic::AtomicBool>,
    status: Option<std::process::ExitStatus>,
}

impl Contender {
    fn finished(&mut self) -> bool {
        if self.status.is_none() {
            self.status = self.child.try_wait().ok().flatten();
        }
        self.status.is_some() && self.eof.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap()).trim().to_owned()
    }

    fn failure(&self) -> Option<String> {
        use std::os::unix::process::ExitStatusExt;
        let status = self.status?;
        let with = |message: String| {
            let stderr = self.stderr();
            if stderr.is_empty() { message } else { format!("{message}\n{stderr}") }
        };
        if let Some(code) = status.code().filter(|c| *c != 0) {
            return Some(with(format!("Server process exited with code {code}")));
        }
        status.signal().map(|s| with(format!("Server process terminated by {}", signal_name(s))))
    }
}

pub fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGHUP => "SIGHUP".into(),
        libc::SIGINT => "SIGINT".into(),
        libc::SIGQUIT => "SIGQUIT".into(),
        libc::SIGILL => "SIGILL".into(),
        libc::SIGABRT => "SIGABRT".into(),
        libc::SIGBUS => "SIGBUS".into(),
        libc::SIGFPE => "SIGFPE".into(),
        libc::SIGKILL => "SIGKILL".into(),
        libc::SIGSEGV => "SIGSEGV".into(),
        libc::SIGPIPE => "SIGPIPE".into(),
        libc::SIGTERM => "SIGTERM".into(),
        other => format!("signal {other}"),
    }
}

fn spawn_contender(env: &[(String, String)]) -> Result<Contender> {
    let executable = std::env::current_exe().map_err(|e| Fail::error(format!("Failed to start server: {e}")))?;
    let mut command = tokio::process::Command::new(executable);
    command
        .args(["serve", "--service"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .env_remove("OPENCODE_PTY_HANDOFF")
        .process_group(0);
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|e| Fail::error(format!("Failed to start server: {e}")))?;
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let eof = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(mut pipe) = child.stderr.take() {
        let (stderr, eof) = (stderr.clone(), eof.clone());
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut chunk = [0u8; 4096];
            loop {
                match pipe.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut buffer = stderr.lock().unwrap();
                        buffer.extend_from_slice(&chunk[..n]);
                        let excess = buffer.len().saturating_sub(8 * 1024);
                        buffer.drain(..excess);
                    }
                }
            }
            eof.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    } else {
        eof.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(Contender { child, stderr, eof, status: None })
}

/// `contenderPool`: at most two live attempts, the first failure, a spawn delay that backs off when attempts
/// lose to another one.
struct Pool {
    contenders: Vec<Contender>,
    failure: Option<String>,
    spawn_delay: Duration,
    last_spawn: Option<Instant>,
}

const SPAWN_DELAY: Duration = Duration::from_secs(5);
const MAX_SPAWN_DELAY: Duration = Duration::from_secs(30);

impl Pool {
    fn evict(&mut self, pid: i64) {
        self.contenders.retain_mut(|c| c.child.id().map(i64::from) != Some(pid) && !c.finished());
        self.failure = None;
        self.last_spawn = None;
    }

    fn reap(&mut self) -> Option<String> {
        let mut index = 0;
        while index < self.contenders.len() {
            if self.contenders[index].finished() {
                let contender = self.contenders.remove(index);
                if self.failure.is_none() {
                    self.failure = contender.failure();
                }
                if contender.status.and_then(|s| s.code()) == Some(0) {
                    self.spawn_delay = (self.spawn_delay * 2).min(MAX_SPAWN_DELAY);
                }
            } else {
                index += 1;
            }
        }
        if self.contenders.is_empty() { self.failure.clone() } else { None }
    }

    fn should_recruit(&mut self, registered: bool) -> bool {
        if self.last_spawn.is_none() && registered {
            self.last_spawn = Some(Instant::now());
        }
        let waited = self.last_spawn.is_none_or(|last| last.elapsed() >= self.spawn_delay);
        self.failure.is_none() && self.contenders.len() < 2 && waited
    }
}

/// `Service.ensure`: reuses a healthy registered service, replaces one whose version does not match, and
/// otherwise starts contenders until one registers.
pub async fn ensure(file: &str, version: Option<&str>, env: &[(String, String)], on_start: &mut dyn FnMut(&str)) -> Result<Endpoint> {
    let mut pool = Pool { contenders: vec![], failure: None, spawn_delay: SPAWN_DELAY, last_spawn: None };
    let mut timeouts: Option<(Info, u32)> = None;
    let mut announced = false;
    let mut announce = |reason: &str| {
        if !announced {
            announced = true;
            on_start(reason);
        }
    };
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let info = read_registration(file);
        let (service, timed_out) = match &info {
            Some(info) => probe(info, &info.url, Duration::from_secs(2)).await,
            None => (None, false),
        };
        match (&info, timed_out) {
            (Some(info), true) => {
                let count = match &timeouts {
                    Some((previous, count)) if previous.same(info) => count + 1,
                    _ => 1,
                };
                timeouts = Some((info.clone(), count));
                if count >= 3 {
                    announce("missing");
                    let _ = std::fs::remove_file(handoff_file(file));
                    terminate(info, file).await?;
                    pool.evict(info.pid);
                    pool.last_spawn = Some(Instant::now() - pool.spawn_delay);
                    timeouts = None;
                }
            }
            _ => timeouts = None,
        }
        if let Some(service) = service {
            pool.spawn_delay = SPAWN_DELAY;
            if !matches_version(service.version.as_deref(), version) {
                announce("version-mismatch");
                let _ = stop_file(file).await;
                pool.evict(service.info.pid);
            } else if !service.compatible {
                return Err(Fail::error(
                    "Background service uses an incompatible health protocol. Update this client or explicitly restart the service.",
                ));
            } else if service.state == State::Ready {
                return Ok(service.info.endpoint());
            } else if service.state == State::Failed {
                return Err(Fail::error("Background service failed to start"));
            }
        } else {
            if let Some(failure) = pool.reap() {
                return Err(Fail::error(failure));
            }
            if pool.should_recruit(info.is_some()) {
                announce("missing");
                pool.contenders.push(spawn_contender(env)?);
                pool.last_spawn = Some(Instant::now());
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(Fail::error(pool.failure.unwrap_or_else(|| "Timed out waiting for the background service to start".into())))
}

/// `ensure(ServiceConfig.options())`.
pub async fn ensure_default(on_start: &mut dyn FnMut(&str)) -> Result<Endpoint> {
    let file = migrate_registrations();
    let env = process_env(&read_config());
    ensure(&file, None, &env, on_start).await
}

// --- the service's own registration (service-registration.ts) ---

/// `ServiceRegistration.register`: writes the registration atomically; the returned instance is checked every
/// five seconds and the service shuts down when another one replaces it.
pub fn register(file: &str, info: &Info) -> std::io::Result<()> {
    let temp = format!("{file}.{}.tmp", info.id.as_deref().unwrap_or("service"));
    write_private(file, &temp, &info.encode())
}

/// Whether the registration still names this instance.
pub fn owns(file: &str, info: &Info) -> bool {
    read_registration(file).is_some_and(|found| found == *info)
}

// --- commands ---

pub async fn command(name: &str, input: &super::parse::Input) -> Result {
    match name {
        "start" => {
            let endpoint = ensure_default(&mut |_| {}).await?;
            super::out::stdout(&format!("{}\n", endpoint.url));
        }
        "restart" => {
            stop(true).await?;
            let endpoint = ensure_default(&mut |_| {}).await?;
            super::out::stdout(&format!("{}\n", endpoint.url));
        }
        "status" => {
            let file = migrate_registrations();
            let found = discover(&file, None).await;
            super::out::stdout(&format!("{}\n", found.map(|e| e.url).unwrap_or_else(|| "stopped".into())));
        }
        "stop" => stop(true).await?,
        "get" => {
            let text = get(input.str("key"), input.str("name"))?;
            super::out::stdout(&format!("{text}\n"));
        }
        "set" => set(input.str("key").unwrap_or(""), input.str("value").unwrap_or(""), input.str("env-value")).await?,
        "unset" => unset(input.str("key").unwrap_or(""), input.str("name")).await?,
        _ => {}
    }
    Ok(0)
}
