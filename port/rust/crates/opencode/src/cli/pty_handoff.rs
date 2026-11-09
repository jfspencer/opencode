//! `PtyHandoff` (packages/client/src/pty-handoff.ts): persistent terminals survive the replacement of the background
//! service. Before a ready service is stopped for a replacement, its daemon prepares a handoff ticket, published
//! beside the registration (`<registration>.pty-handoff`) with the service it came from; the contenders started next
//! get the ticket as `OPENCODE_PTY_HANDOFF` while the sidecar still describes the registered (or no) service, and the
//! winner adopts the daemon. A client that finds another service running clears the sidecar. Terminals are best
//! effort: every failure replaces the service without a handoff.

use std::time::Duration;

use opencode_protocol::Json;

use super::service::Info;
use crate::json::{self, Obj};

/// The sidecar: the service the ticket came from, the ticket (or `null`: none to adopt), and its expiry.
struct Sidecar {
    id: Option<String>,
    pid: f64,
    url: String,
    handoff: Json,
    expires_at: f64,
}

fn sidecar_file(file: &str) -> String {
    format!("{file}.pty-handoff")
}

fn now() -> f64 {
    crate::id::now() as f64
}

/// `isHandoff`: a descriptor with string `directory`, `instanceID`, `ticket` and a finite `expiresAt`.
fn is_handoff(value: &Json) -> bool {
    let text = |key: &str| matches!(json::get(value, key), Some(Json::String(_)));
    matches!(value, Json::Object(_))
        && text("directory")
        && text("instanceID")
        && text("ticket")
        && json::get_num(value, "expiresAt").is_some_and(f64::is_finite)
}

/// `read`: the sidecar, or `None` when it is missing or malformed.
fn read(file: &str) -> Option<Sidecar> {
    let value = json::parse(&std::fs::read_to_string(sidecar_file(file)).ok()?)?;
    let source = json::get(&value, "source")?;
    if !matches!(source, Json::Object(_)) {
        return None;
    }
    let handoff = json::get(&value, "handoff")?.clone();
    let pid = json::get_num(source, "pid")?;
    let url = json::get_str(source, "url")?.to_owned();
    let id = match json::get(source, "id") {
        None => None,
        Some(Json::String(id)) => Some(id.clone()),
        Some(_) => return None,
    };
    if !matches!(handoff, Json::Null) && !is_handoff(&handoff) {
        return None;
    }
    let expires_at = json::get_num(&value, "expiresAt").filter(|n| n.is_finite())?;
    Some(Sidecar { id, pid, url, handoff, expires_at })
}

impl Sidecar {
    fn from(&self, info: &Info) -> bool {
        self.id == info.id && self.pid == info.pid as f64 && self.url == info.url
    }
    fn live(&self) -> bool {
        self.expires_at > now()
    }
}

/// `prepare`: asks the service for a handoff ticket and publishes it before the service is stopped. Never fails.
pub async fn prepare(file: &str, info: &Info, timeout: Duration) {
    if read(file).is_some_and(|existing| existing.live() && existing.from(info)) {
        return;
    }
    let client = super::client::Client::new(&info.endpoint());
    let answer = client.call_with("experimental.persistentPty.handoff", &[], None, None, &[], Some(timeout)).await;
    let handoff = match answer.map_err(|e| e.message()).and_then(parse) {
        Ok(handoff) => handoff,
        Err(cause) => {
            // Another caller may already have prepared and stopped this server.
            if read(file).is_some_and(|concurrent| concurrent.live() && concurrent.from(info)) {
                return;
            }
            super::out::stderr(&format!(
                "Background service cannot hand off persistent terminals; shutting them down before replacement {cause}\n"
            ));
            let _ = client.call_with("experimental.persistentPty.shutdown", &[], None, None, &[], Some(timeout)).await;
            Json::Null
        }
    };
    if let Err(cause) = publish(file, info, handoff) {
        super::out::stderr(&format!("Failed to publish persistent terminal handoff {cause}\n"));
    }
}

/// The handoff endpoint's answer: a live descriptor, or `null`.
fn parse(body: Json) -> Result<Json, String> {
    let Some(handoff) = json::get(&body, "handoff").filter(|_| matches!(body, Json::Object(_))) else {
        return Err("Invalid persistent terminal handoff response".into());
    };
    if matches!(handoff, Json::Null) {
        return Ok(Json::Null);
    }
    if !is_handoff(handoff) || json::get_num(handoff, "expiresAt").is_none_or(|at| at <= now()) {
        return Err("Invalid or expired persistent terminal handoff".into());
    }
    Ok(handoff.clone())
}

/// `publish`: written to a private temporary file, then renamed over the sidecar.
fn publish(file: &str, info: &Info, handoff: Json) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut source = Obj::new();
    source.opt_str("id", info.id.as_deref()).int("pid", info.pid).str("url", &info.url);
    let expires_at = json::get_num(&handoff, "expiresAt").unwrap_or_else(|| now() + 30_000.0);
    let mut record = Obj::new();
    record.set("source", source.done()).set("handoff", handoff).set("expiresAt", Json::Number(expires_at));
    let temporary = format!("{file}.pty-handoff.{}.tmp", crate::id::uuid());
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut handle| handle.write_all(json::text(&record.done()).as_bytes()))
        .and_then(|()| std::fs::rename(&temporary, sidecar_file(file)));
    let _ = std::fs::remove_file(&temporary);
    result
}

/// `environment`: the ticket a new contender adopts (`OPENCODE_PTY_HANDOFF`), while the sidecar is live and describes
/// the registered service (or none is registered).
pub fn environment(file: &str) -> Option<String> {
    let record = read(file)?;
    let current = std::fs::read_to_string(file).ok().and_then(|text| json::parse(&text));
    let matches = match &current {
        None => true,
        Some(current) => {
            let id = match json::get(current, "id") {
                Some(Json::String(id)) => Some(id.clone()),
                _ => None,
            };
            id == record.id && json::get_num(current, "pid") == Some(record.pid) && json::get_str(current, "url") == Some(record.url.as_str())
        }
    };
    if !record.live() || !matches || matches!(record.handoff, Json::Null) {
        return None;
    }
    Some(json::text(&record.handoff))
}

/// `complete`: a reused service that is not the one the ticket came from makes the sidecar stale.
pub fn complete(file: &str, info: &Info) {
    if read(file).is_some_and(|record| !record.from(info)) {
        clear(file);
    }
}

/// `clear`.
pub fn clear(file: &str) {
    let _ = std::fs::remove_file(sidecar_file(file));
}
