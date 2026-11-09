//! Remote access through OpenTunnel (packages/cli/src/services/remote-tunnel.ts `ensure` and `hostname`) and the
//! parts of the OpenTunnel SDK 0.6.1 they reach (@opentunnel/client `OpenTunnelClient`: `tunnel.get`, `ensure`,
//! `create`, `provision`, `completePending`; its XDG storage; the API client with `withRetries`; the P-256 certificate
//! request), against the API contract and `ApiRetry` of @opentunnel/protocol 0.6.1.
//!
//! The device keeps one tunnel in the storage's `default` profile: `ensure` creates it (a key made here, a
//! certificate request for the tunnel hostname and its wildcard, the certificate the API issues) or checks the
//! certificate of the one it has. Attaching the service's route to the tunnel (`RemoteTunnel.run`, the bridge) is
//! the service's: see `serve.rs`.

use opencode_protocol::Json;

use crate::json::{self, Obj};

const API: &str = "https://opentunnel.xyz";
const USER_AGENT: &str = "opentunnel-sdk/0.6.1";
const PROFILE: &str = "default";
/// `ApiRetry`: six retries, 250 ms doubling up to 8 s.
const RETRIES: u32 = 6;
const RETRY_BASE_MS: f64 = 250.0;
const RETRY_MAX_MS: f64 = 8000.0;

/// A failure of the SDK as `Cause.pretty` prints it: its tagged error class and message, the error it wraps (its
/// `[cause]` block), and a second failure of the same cause (an error body that did not decode).
#[derive(Debug)]
pub struct Failure {
    pub tag: &'static str,
    pub message: String,
    cause: Option<String>,
    next: Option<Box<Failure>>,
}

impl Failure {
    /// What follows `<tag>: `.
    pub fn text(&self) -> String {
        let mut out = self.message.clone();
        if let Some(cause) = &self.cause {
            out.push_str(&format!(" {{\n  [cause]: {cause}\n}}"));
        }
        if let Some(next) = &self.next {
            out.push_str(&format!("\n{}: {}", next.tag, next.text()));
        }
        out
    }

    pub fn into_fail(self) -> super::out::Fail {
        super::out::Fail::named(self.tag, self.text())
    }
}

fn failure(tag: &'static str, message: impl Into<String>) -> Failure {
    Failure { tag, message: message.into(), cause: None, next: None }
}

fn client_error(message: impl Into<String>) -> Failure {
    failure("OpenTunnelClientError", message)
}

fn storage_error(message: impl Into<String>) -> Failure {
    failure("OpenTunnelStorageError", message)
}

/// A tunnel identity as the storage keeps it.
#[derive(Clone)]
pub struct Identity {
    id: String,
    pub hostname: String,
    token: String,
    private_key: String,
    certificate: String,
    chain: String,
    /// `certificateExpiry` in epoch milliseconds (NaN when unreadable).
    expiry: f64,
}

struct Pending {
    id: String,
    hostname: String,
    token: String,
    private_key: String,
    csr: String,
}

// --- storage (`OpenTunnelStorage.xdg()`) ---

fn profile_root() -> String {
    let data = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{}/.local/share", super::paths::home()));
    format!("{data}/opentunnel/{PROFILE}")
}

fn read_optional(path: &str) -> Result<Option<String>, Failure> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(storage_error(format!("Failed to read {path}"))),
    }
}

fn parse_metadata(text: &str) -> Result<Json, Failure> {
    json::parse(text).ok_or_else(|| failure("SyntaxError", "JSON Parse error: Unexpected identifier"))
}

fn string_field(value: &Json, key: &str) -> String {
    json::get_str(value, key).unwrap_or("").to_owned()
}

/// `storage.load(profile)`.
fn load() -> Result<Option<Identity>, Failure> {
    let root = profile_root();
    let Some(metadata) = read_optional(&format!("{root}/tunnel.json"))? else { return Ok(None) };
    let token = read_optional(&format!("{root}/token"))?;
    let private_key = read_optional(&format!("{root}/private-key.pem"))?;
    let certificate = read_optional(&format!("{root}/certificate.pem"))?;
    let chain = read_optional(&format!("{root}/chain.pem"))?;
    let (Some(token), Some(private_key), Some(certificate), Some(chain)) = (token, private_key, certificate, chain) else {
        return Err(storage_error(format!("Incomplete tunnel identity for {PROFILE}")));
    };
    if token.is_empty() || private_key.is_empty() || certificate.is_empty() {
        return Err(storage_error(format!("Incomplete tunnel identity for {PROFILE}")));
    }
    let value = parse_metadata(&metadata)?;
    let expiry = json::get_str(&value, "certificateExpiry")
        .and_then(crate::zone::parse_date)
        .map_or(f64::NAN, |ms| ms as f64);
    Ok(Some(Identity {
        id: string_field(&value, "id"),
        hostname: string_field(&value, "hostname"),
        token: token.trim().to_owned(),
        private_key,
        certificate,
        chain,
        expiry,
    }))
}

/// `storage.loadPending(profile)`.
fn load_pending() -> Result<Option<Pending>, Failure> {
    let root = profile_root();
    let Some(metadata) = read_optional(&format!("{root}/pending.json"))? else { return Ok(None) };
    let token = read_optional(&format!("{root}/token"))?;
    let private_key = read_optional(&format!("{root}/private-key.pem"))?;
    let (Some(token), Some(private_key)) = (token, private_key) else {
        return Err(storage_error(format!("Incomplete pending tunnel identity for {PROFILE}")));
    };
    if token.is_empty() || private_key.is_empty() {
        return Err(storage_error(format!("Incomplete pending tunnel identity for {PROFILE}")));
    }
    let value = parse_metadata(&metadata)?;
    Ok(Some(Pending {
        id: string_field(&value, "id"),
        hostname: string_field(&value, "hostname"),
        token: token.trim().to_owned(),
        private_key,
        csr: string_field(&value, "csr"),
    }))
}

fn write_files(root: &str, files: &[(&str, &str)]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(root)?;
    for (name, content) in files {
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(format!("{root}/{name}"))?;
        file.write_all(content.as_bytes())?;
    }
    Ok(())
}

/// `storage.savePending(profile, tunnel)`.
fn save_pending(pending: &Pending) -> Result<(), Failure> {
    let mut metadata = Obj::new();
    metadata.str("id", &pending.id).str("hostname", &pending.hostname).str("csr", &pending.csr);
    let metadata = super::out::pretty(&metadata.done());
    let token = format!("{}\n", pending.token);
    write_files(
        &profile_root(),
        &[("pending.json", &metadata), ("token", &token), ("private-key.pem", &pending.private_key)],
    )
    .map_err(|_| storage_error(format!("Failed to save pending tunnel identity for {PROFILE}")))
}

/// `storage.save(profile, tunnel)`: the identity, then the pending request removed.
fn save(identity: &Identity) -> Result<(), Failure> {
    let failed = || storage_error(format!("Failed to save tunnel identity for {PROFILE}"));
    if !identity.expiry.is_finite() {
        return Err(failed());
    }
    let mut metadata = Obj::new();
    metadata
        .str("id", &identity.id)
        .str("hostname", &identity.hostname)
        .str("certificateExpiry", &crate::zone::iso(identity.expiry as i64));
    let metadata = super::out::pretty(&metadata.done());
    let token = format!("{}\n", identity.token);
    let root = profile_root();
    write_files(
        &root,
        &[
            ("tunnel.json", &metadata),
            ("token", &token),
            ("private-key.pem", &identity.private_key),
            ("certificate.pem", &identity.certificate),
            ("chain.pem", &identity.chain),
        ],
    )
    .map_err(|_| failed())?;
    match std::fs::remove_file(format!("{root}/pending.json")) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(failed()),
        _ => Ok(()),
    }
}

// --- the API client (`OpenTunnelApiClient` with `withRetries`) ---

/// Why an API call failed.
enum ApiError {
    /// An `HttpClientError` with the response: `StatusCode error` for a declared error status whose body did not
    /// decode (that decoding's `SchemaError` is a second failure), `Decode: undeclared status code` otherwise.
    Status { status: u16, reason: &'static str, request: String, schema: Option<String> },
    /// An error the API declared for the status: its tag, message and `name` (the tag, unless a field named `name`
    /// replaces it).
    Declared { tag: String, message: String, name: String },
    /// A success whose body did not decode.
    Schema(String),
    /// No answer (`HttpClientError` with a `TransportError`).
    Transport { request: String },
}

impl ApiError {
    /// `describeApiError`.
    fn describe(&self) -> String {
        match self {
            ApiError::Status { status, .. } => {
                let message = status_message(*status);
                if message.starts_with("HTTP ") { message } else { format!("{message} (HTTP {status})") }
            }
            ApiError::Declared { tag, message, .. } => format!("{tag}: {message}"),
            ApiError::Schema(message) => format!("SchemaError: {message}"),
            ApiError::Transport { request } => format!("Transport error ({request})"),
        }
    }

    /// The wrapped error as `Cause.pretty` names it.
    fn pretty(&self) -> String {
        match self {
            ApiError::Status { status, reason, request, .. } => format!("HttpClientError: {reason} ({status} {request})"),
            ApiError::Declared { name, message, .. } => format!("{name}: {message}"),
            ApiError::Schema(message) => format!("SchemaError: {message}"),
            ApiError::Transport { request } => format!("HttpClientError: Transport error ({request})"),
        }
    }

    /// `isTransient`: unreachable, rate limited, or failing on the server side (an answer that is not an API error).
    fn transient(&self) -> bool {
        match self {
            ApiError::Status { status, .. } => retryable_status(*status, true),
            ApiError::Transport { .. } => true,
            ApiError::Declared { .. } | ApiError::Schema(_) => false,
        }
    }
}

const STATUS_CODE: &str = "StatusCode error";
const UNDECLARED: &str = "Decode: undeclared status code";
/// Effect Schema's message for a body that is not JSON.
const NOT_JSON: &str = "Expected a valid JSON string";

/// `ApiRetry.statusMessage(status, "")`.
fn status_message(status: u16) -> String {
    let reason = match status {
        520 => "the server returned an unknown error",
        521 => "the server refused the connection",
        522 => "the connection to the server timed out",
        523 => "the server is unreachable",
        524 => "the server did not answer in time",
        525 => "the SSL handshake with the server failed",
        526 => "the server's certificate is invalid",
        527 => "the connection to the server failed",
        _ => return format!("HTTP {status}"),
    };
    format!("Cloudflare {status}: {reason}")
}

/// `ApiRetry.retryableStatus(status, repeat)`.
fn retryable_status(status: u16, idempotent: bool) -> bool {
    if idempotent {
        status == 429 || (500..=599).contains(&status)
    } else {
        [429, 503, 521, 522, 523, 525].contains(&status)
    }
}

/// `ApiRetry.retryDelayMs(attempt, retryAfter)`: a range to wait in.
fn retry_delay(attempt: u32, retry_after: Option<&str>) -> (f64, f64) {
    if let Some(seconds) = retry_after.map(str::trim).filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())) {
        let delay = (seconds.parse::<f64>().unwrap_or(f64::INFINITY) * 1000.0).min(RETRY_MAX_MS);
        return (delay, delay);
    }
    let delay = (RETRY_BASE_MS * 2f64.powi(attempt.min(16) as i32)).min(RETRY_MAX_MS);
    (delay / 2.0, delay)
}

fn jitter() -> f64 {
    let mut bytes = [0u8; 8];
    crate::id::random(&mut bytes);
    (u64::from_le_bytes(bytes) >> 11) as f64 / (1u64 << 53) as f64
}

/// A response: its status and body.
struct Answer {
    status: u16,
    body: String,
    /// `METHOD url`, as the client's errors name the request.
    request: String,
}

/// One request, sent again after transient failures as far as it allows (`withRetries`); the last failure or
/// response is returned.
async fn send(method: &str, path: &str, token: Option<&str>, payload: Option<&Json>) -> Result<Answer, ApiError> {
    let url = format!("{API}{path}");
    let request = format!("{method} {url}");
    // Reads and binding the same request (which answers with the current certificate) may be repeated freely.
    let idempotent = method == "GET" || (method == "POST" && url.ends_with("/certificate"));
    let mut headers = vec![("user-agent".to_owned(), USER_AGENT.to_owned())];
    if let Some(token) = token {
        headers.push(("authorization".into(), format!("Bearer {token}")));
    }
    if payload.is_some() {
        headers.push(("content-type".into(), "application/json".into()));
    }
    let body = payload.map(json::text).unwrap_or_default();
    let mut attempt = 0;
    loop {
        let (min, max) = match crate::llm::http::send(method, &url, &headers, body.clone()).await {
            Ok(response) => {
                let status = response.status;
                let retry_after = response.headers.iter().find(|(n, _)| n == "retry-after").map(|(_, v)| v.clone());
                if attempt < RETRIES && retryable_status(status, idempotent) {
                    retry_delay(attempt, retry_after.as_deref())
                } else {
                    let body = response.text().await.map_err(|_| ApiError::Transport { request: request.clone() })?;
                    return Ok(Answer { status, body, request });
                }
            }
            Err(failure) => {
                // A request that could not connect never left, so even a single-shot request is sent again.
                let connect = matches!(failure.kind, crate::llm::http::FailureKind::Refused | crate::llm::http::FailureKind::Unresolved);
                if attempt < RETRIES && (idempotent || connect) {
                    retry_delay(attempt, None)
                } else {
                    return Err(ApiError::Transport { request });
                }
            }
        };
        tokio::time::sleep(std::time::Duration::from_secs_f64((min + jitter() * (max - min)) / 1000.0)).await;
        attempt += 1;
    }
}

/// One error an endpoint declares: its status, tag and required string fields besides `message`.
type Declared = (u16, &'static str, &'static [&'static str]);

/// Decodes an answer against an endpoint's declared statuses: the success status's body, or one of the errors the
/// endpoint declares for the status (`_tag` and its fields); any other status is undeclared.
fn decode(answer: Answer, success: u16, errors: &[Declared]) -> Result<Json, ApiError> {
    let value = json::parse(&answer.body);
    if answer.status == success {
        return value.ok_or_else(|| ApiError::Schema(NOT_JSON.into()));
    }
    let declared: Vec<&Declared> = errors.iter().filter(|(status, _, _)| *status == answer.status).collect();
    let status_error = |schema: &str| ApiError::Status {
        status: answer.status,
        reason: STATUS_CODE,
        request: answer.request.clone(),
        schema: Some(schema.to_owned()),
    };
    if declared.is_empty() {
        return Err(ApiError::Status { status: answer.status, reason: UNDECLARED, request: answer.request, schema: None });
    }
    let Some(value) = value else { return Err(status_error(NOT_JSON)) };
    for (_, tag, fields) in &declared {
        if json::get_str(&value, "_tag") != Some(*tag) {
            continue;
        }
        if !std::iter::once(&"message").chain(fields.iter()).all(|field| json::get_str(&value, field).is_some()) {
            continue;
        }
        let name = if *tag == "HostnameUnavailableError" { string_field(&value, "name") } else { (*tag).to_owned() };
        return Err(ApiError::Declared { tag: (*tag).to_owned(), message: string_field(&value, "message"), name });
    }
    let tags = declared.iter().map(|(_, tag, _)| format!("{{ readonly \"_tag\": \"{tag}\", ... }}")).collect::<Vec<_>>();
    Err(status_error(&format!("Expected {}", tags.join(" | "))))
}

const SERVICE_UNAVAILABLE: Declared = (503, "ServiceUnavailableError", &[]);
const UNAUTHORIZED: Declared = (401, "UnauthorizedError", &[]);
const TUNNEL_NOT_FOUND: Declared = (404, "TunnelNotFoundError", &["tunnelID"]);

/// `Missing key` at a path, as Effect Schema reports a missing field.
fn missing(path: &[&str]) -> ApiError {
    let at: String = path.iter().map(|key| format!("[\"{key}\"]")).collect();
    ApiError::Schema(format!("Missing key\n  at {at}"))
}

/// `tunnel.create`: `POST /api/tunnel` with `{}`.
async fn create_tunnel() -> Result<(String, String, String), ApiError> {
    let answer = send("POST", "/api/tunnel", None, Some(&json::object())).await?;
    let value = decode(
        answer,
        201,
        &[(400, "InvalidRequestError", &[]), (409, "HostnameUnavailableError", &["name"]), SERVICE_UNAVAILABLE],
    )?;
    let tunnel = json::get(&value, "tunnel").ok_or_else(|| missing(&["tunnel"]))?;
    let field = |key: &'static str| json::get_str(tunnel, key).map(str::to_owned).ok_or_else(|| missing(&["tunnel", key]));
    let (id, hostname) = (field("id")?, field("hostname")?);
    if !matches!(json::get_str(tunnel, "state"), Some("offline" | "online")) {
        return Err(ApiError::Schema("Expected \"offline\" | \"online\"".into()));
    }
    let token = json::get_str(&value, "token").ok_or_else(|| missing(&["token"]))?;
    Ok((id, hostname, token.to_owned()))
}

/// A certificate's state (`Certificate.State`).
enum State {
    Challenge,
    Issuing,
    Ready { certificate: String, chain: String, expiry: String },
    Failed { reason: String },
}

/// `Certificate.Info`.
fn certificate_state(value: Json) -> Result<State, ApiError> {
    if json::get_str(&value, "id").is_none() {
        return Err(missing(&["id"]));
    }
    let state = json::get(&value, "state").ok_or_else(|| missing(&["state"]))?;
    let union = || {
        ApiError::Schema(
            "Expected { readonly \"type\": \"challenge\", ... } | { readonly \"type\": \"issuing\", ... } | { readonly \"type\": \"ready\", ... } | { readonly \"type\": \"failed\", ... }"
                .into(),
        )
    };
    let text = |key: &str| json::get_str(state, key).map(str::to_owned).ok_or_else(union);
    match json::get_str(state, "type") {
        Some("challenge") => {
            text("token")?;
            text("key")?;
            Ok(State::Challenge)
        }
        Some("issuing") => Ok(State::Issuing),
        Some("ready") => Ok(State::Ready { certificate: text("certificate")?, chain: text("chain")?, expiry: text("expiry")? }),
        Some("failed") => Ok(State::Failed { reason: text("reason")? }),
        _ => Err(union()),
    }
}

fn certificate_path(id: &str) -> String {
    format!("/api/tunnel/{}/certificate", super::client::encode_component(id))
}

/// `tunnel.getCertificate`: `GET /api/tunnel/:id/certificate`.
async fn get_certificate(id: &str, token: &str) -> Result<State, ApiError> {
    let answer = send("GET", &certificate_path(id), Some(token), None).await?;
    let value = decode(
        answer,
        200,
        &[TUNNEL_NOT_FOUND, (404, "CertificateNotFoundError", &["tunnelID"]), SERVICE_UNAVAILABLE, UNAUTHORIZED],
    )?;
    certificate_state(value)
}

/// `tunnel.bindCertificate`: `POST /api/tunnel/:id/certificate` with the request.
async fn bind_certificate(id: &str, token: &str, csr: &str) -> Result<(), ApiError> {
    let mut payload = Obj::new();
    payload.str("csr", csr);
    let answer = send("POST", &certificate_path(id), Some(token), Some(&payload.done())).await?;
    let value = decode(
        answer,
        202,
        &[
            (400, "InvalidRequestError", &[]),
            (400, "InvalidHostnameError", &["provided", "expected"]),
            TUNNEL_NOT_FOUND,
            (409, "CertificateInProgressError", &["tunnelID"]),
            SERVICE_UNAVAILABLE,
            UNAUTHORIZED,
        ],
    )?;
    certificate_state(value).map(|_| ())
}

// --- the client (`OpenTunnelClient.layer()`) ---

/// `clientError(message, cause)`: the description of the cause after the message, the cause itself, and a second
/// failure for an error body that did not decode.
fn failed(message: &str, cause: &ApiError) -> Failure {
    let mut out = client_error(format!("{message}: {}", cause.describe()));
    out.cause = Some(cause.pretty());
    if let ApiError::Status { schema: Some(schema), .. } = cause {
        let mut next = client_error(format!("{message}: SchemaError: {schema}"));
        next.cause = Some(format!("SchemaError: {schema}"));
        out.next = Some(Box::new(next));
    }
    out
}


/// `completePending`: binds the request (an issuance already in flight is fine), waits for the certificate, saves
/// the identity.
async fn complete_pending(pending: &Pending) -> Result<Identity, Failure> {
    match bind_certificate(&pending.id, &pending.token, &pending.csr).await {
        Err(ApiError::Declared { tag, .. }) if tag == "CertificateInProgressError" => {}
        Err(cause) => return Err(failed("Failed to start certificate issuance", &cause)),
        Ok(()) => {}
    }
    let (certificate, chain, expiry) = loop {
        match get_certificate(&pending.id, &pending.token).await {
            Ok(State::Ready { certificate, chain, expiry }) => break (certificate, chain, expiry),
            Ok(State::Failed { reason }) => return Err(client_error(format!("Certificate issuance failed: {reason}"))),
            Ok(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
            Err(cause) => return Err(failed("Failed to read certificate", &cause)),
        }
    };
    let identity = Identity {
        id: pending.id.clone(),
        hostname: pending.hostname.clone(),
        token: pending.token.clone(),
        private_key: pending.private_key.clone(),
        certificate,
        chain,
        expiry: crate::zone::parse_date(&expiry).map_or(f64::NAN, |ms| ms as f64),
    };
    save(&identity)?;
    Ok(identity)
}

/// `provision`: a new P-256 key and certificate request for the hostname, kept as the pending identity, then
/// completed.
async fn provision(id: &str, hostname: &str, token: &str) -> Result<Identity, Failure> {
    let (private_key, csr) = certificate_request(hostname)?;
    let pending = Pending { id: id.into(), hostname: hostname.into(), token: token.into(), private_key, csr };
    save_pending(&pending)?;
    complete_pending(&pending).await
}

/// `tunnel.create`: resumes a pending identity, else creates a tunnel and provisions it.
async fn create() -> Result<Identity, Failure> {
    if load()?.is_some() {
        return Err(client_error(format!("Profile '{PROFILE}' already has a tunnel")));
    }
    if let Some(pending) = load_pending()? {
        return complete_pending(&pending).await;
    }
    let (id, hostname, token) = create_tunnel().await.map_err(|cause| failed("Failed to create tunnel", &cause))?;
    provision(&id, &hostname, &token).await
}

/// `tunnel.ensure`: the stored tunnel with a current certificate (renewed while offline, or provisioned again when
/// it failed), else a new one.
async fn ensure_tunnel() -> Result<Identity, Failure> {
    let Some(existing) = load()? else { return create().await };
    let state = match get_certificate(&existing.id, &existing.token).await {
        Ok(state) => state,
        // The API is unreachable even after retries: a certificate that has not expired still serves.
        Err(cause) if cause.transient() && existing.expiry > now_ms() => return Ok(existing),
        Err(cause) => return Err(failed("Failed to read certificate", &cause)),
    };
    match state {
        State::Ready { certificate, chain, expiry } if certificate != existing.certificate => {
            let renewed = Identity { certificate, chain, expiry: crate::zone::parse_date(&expiry).map_or(f64::NAN, |ms| ms as f64), ..existing };
            save(&renewed)?;
            Ok(renewed)
        }
        State::Failed { .. } => provision(&existing.id, &existing.hostname, &existing.token).await,
        _ => Ok(existing),
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64)
}

/// `RemoteTunnel.ensure`: a device without a tunnel creates its shared one here, in the foreground (issuing the
/// certificate takes a while), within five minutes. Returns the tunnel hostname.
pub async fn ensure() -> Result<String, Failure> {
    if load()?.is_none() {
        super::out::stderr("Setting up remote access; this can take a minute...\n");
    }
    match tokio::time::timeout(std::time::Duration::from_secs(300), ensure_tunnel()).await {
        Ok(result) => result.map(|identity| identity.hostname),
        Err(_) => Err(failure("Error", "Timed out creating the remote access tunnel; run the command again to resume")),
    }
}

/// `RemoteTunnel.hostname(route)`: the route's public hostname once the tunnel's certificate is ready.
pub fn hostname(route: &str) -> Result<Option<String>, Failure> {
    Ok(load()?.map(|identity| format!("{route}.{}", identity.hostname)))
}

// --- the certificate request (`csr.ts` `certificateRequest`) ---

fn der(tag: u8, contents: &[&[u8]]) -> Vec<u8> {
    let body: Vec<u8> = contents.concat();
    let mut out = vec![tag];
    if body.len() < 0x80 {
        out.push(body.len() as u8);
    } else {
        let bytes: Vec<u8> = body.len().to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend(body);
    out
}

fn oid(dotted: &str) -> Vec<u8> {
    let arcs: Vec<u64> = dotted.split('.').map(|a| a.parse().unwrap_or(0)).collect();
    let mut bytes = vec![(arcs[0] * 40 + arcs[1]) as u8];
    for &arc in &arcs[2..] {
        let mut encoded = vec![(arc & 0x7f) as u8];
        let mut n = arc >> 7;
        while n > 0 {
            encoded.insert(0, ((n & 0x7f) | 0x80) as u8);
            n >>= 7;
        }
        bytes.extend(encoded);
    }
    der(0x06, &[&bytes])
}

/// Base64 in lines of 64 characters.
fn lines(bytes: &[u8]) -> Vec<String> {
    use base64::Engine;
    let text = base64::engine::general_purpose::STANDARD.encode(bytes);
    text.as_bytes().chunks(64).map(|chunk| String::from_utf8_lossy(chunk).into_owned()).collect()
}

/// A new P-256 key (as PKCS#8 PEM) and the PEM certificate request it signs: CN and SANs for the hostname and its
/// wildcard.
fn certificate_request(hostname: &str) -> Result<(String, String), Failure> {
    use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};
    let random = ring::rand::SystemRandom::new();
    let document = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &random)
        .map_err(|_| client_error("Failed to generate certificate key"))?;
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, document.as_ref(), &random)
        .map_err(|_| client_error("Failed to generate certificate key"))?;
    let spki = der(
        0x30,
        &[
            &der(0x30, &[&oid("1.2.840.10045.2.1"), &oid("1.2.840.10045.3.1.7")]),
            &der(0x03, &[&[0], pair.public_key().as_ref()]),
        ],
    );
    let utf8 = |value: &str| der(0x0c, &[value.as_bytes()]);
    let dns = |value: &str| der(0x82, &[value.as_bytes()]);
    let names = der(0x30, &[&dns(hostname), &dns(&format!("*.{hostname}"))]);
    let extension = der(0x30, &[&oid("2.5.29.17"), &der(0x04, &[&names])]);
    let info = der(
        0x30,
        &[
            &der(0x02, &[&[0]]),
            &der(0x30, &[&der(0x31, &[&der(0x30, &[&oid("2.5.4.3"), &utf8(hostname)])])]),
            &spki,
            &der(0xa0, &[&der(0x30, &[&oid("1.2.840.113549.1.9.14"), &der(0x31, &[&der(0x30, &[&extension])])])]),
        ],
    );
    let signature = pair.sign(&random, &info).map_err(|_| client_error("Failed to generate certificate request"))?;
    let request = der(
        0x30,
        &[&info, &der(0x30, &[&oid("1.2.840.10045.4.3.2")]), &der(0x03, &[&[0], signature.as_ref()])],
    );
    let csr = format!(
        "-----BEGIN CERTIFICATE REQUEST-----\n{}-----END CERTIFICATE REQUEST-----",
        lines(&request).iter().map(|line| format!("{line}\n")).collect::<String>()
    );
    let key = format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", lines(document.as_ref()).join("\n"));
    Ok((key, csr))
}
