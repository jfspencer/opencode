//! Poe's browser login (packages/core/src/plugin/provider/poe.ts, method `browser`): OAuth with PKCE. Authorizing
//! starts a callback server on a free loopback port and hands out Poe's authorization URL; the browser comes back to
//! `/callback` with the code, which is exchanged at Poe's token endpoint for an API key. The browser is answered with
//! the outcome's page once the exchange is done.

use std::sync::{Arc, Mutex};

use base64::Engine;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use opencode_protocol::Json;
use sha2::Digest;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::{Authorization, Guard, page};
use crate::json::{self, Obj};

pub const INTEGRATION: &str = "poe";
pub const METHOD: &str = "browser";
const CLIENT_ID: &str = "client_728290227fc048cc9262091a1ea197ea";
const ISSUER: &str = "https://poe.com";
const TOKEN: &str = "https://api.poe.com/token";
/// The latest time JS dates represent: a key that never expires.
const MAX_EXPIRY: f64 = 8_640_000_000_000_000.0;

/// What the callback hands the waiting attempt: the code, and where to send the browser's page.
type Received = Result<(String, oneshot::Sender<(u16, String)>), String>;
type Deferred = Arc<Mutex<Option<oneshot::Sender<Received>>>>;

pub async fn authorize() -> Result<Authorization, String> {
    let verifier = random_token();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    let state = random_token();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| e.to_string())?;
    let Ok(address) = listener.local_addr() else { return Err("Missing OAuth callback port".into()) };
    let redirect = format!("http://127.0.0.1:{}/callback", address.port());
    let (sender, receiver) = oneshot::channel::<Received>();
    let deferred: Deferred = Arc::new(Mutex::new(Some(sender)));
    let server = tokio::spawn(serve(listener, state.clone(), deferred));
    let url = format!(
        "{ISSUER}/oauth/authorize?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("response_type", "code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", &redirect)
            .append_pair("scope", "apikey:create")
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .finish()
    );
    let callback = async move {
        let (code, page) = match receiver.await {
            Ok(received) => received?,
            Err(_) => return Err("OAuth callback server closed".to_owned()),
        };
        let exchanged = exchange(&code, &redirect, &verifier).await;
        let answer = match &exchanged {
            Ok(_) => (200, page::success(Some("Poe"))),
            Err(error) => (400, page::error(error, Some("Poe"))),
        };
        let _ = page.send(answer);
        exchanged
    };
    Ok(Authorization {
        url,
        instructions: "Complete authorization in your browser. This window will close automatically.".into(),
        expires_at: None,
        callback: Box::pin(callback),
        resources: Guard(vec![server.abort_handle()]),
    })
}

/// 32 random bytes, base64url.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    crate::id::random(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

async fn serve(listener: TcpListener, state: String, deferred: Deferred) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let (state, deferred) = (state.clone(), deferred.clone());
        // Connections outlive the server: a callback still answers once the attempt settles and the server closes.
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let (state, deferred) = (state.clone(), deferred.clone());
                async move { answer(request, &state, &deferred).await }
            });
            let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
        });
    }
}

type Answer = hyper::Response<Full<Bytes>>;

/// Ends a connection without an answer.
#[derive(Debug)]
struct Dropped;

impl std::fmt::Display for Dropped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the OAuth attempt was cancelled")
    }
}

impl std::error::Error for Dropped {}

/// The callback: a wrong state, issuer, a denial or a missing code fails the attempt (and the browser sees why); the
/// first good callback waits for the exchange's page; later ones are refused.
async fn answer(request: hyper::Request<Incoming>, state: &str, deferred: &Deferred) -> Result<Answer, Dropped> {
    let respond = |status: u16, html: Option<String>| {
        let mut builder = hyper::Response::builder().status(status);
        if html.is_some() {
            builder = builder.header("Content-Type", "text/html");
        }
        Ok(builder.body(Full::new(Bytes::from(html.unwrap_or_default()))).unwrap())
    };
    if request.method() != hyper::Method::GET || request.uri().path() != "/callback" {
        return respond(404, None);
    }
    let params: Vec<(String, String)> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes()).into_owned().collect();
    if let Some(error) = callback_error(&params, state) {
        if let Some(sender) = deferred.lock().unwrap().take() {
            let _ = sender.send(Err(error.clone()));
        }
        return respond(400, Some(page::error(&error, Some("Poe"))));
    }
    let Some(sender) = deferred.lock().unwrap().take() else {
        return Ok(hyper::Response::builder().status(409).body(Full::new(Bytes::from("OAuth callback already received"))).unwrap());
    };
    let (page, rendered) = oneshot::channel();
    let _ = sender.send(Ok((param(&params, "code").unwrap_or("").to_owned(), page)));
    match rendered.await {
        Ok((status, html)) => respond(status, Some(html)),
        // The attempt was cancelled: the browser's connection is dropped unanswered.
        Err(_) => Err(Dropped),
    }
}

/// `URLSearchParams.get`: the first value of the key.
fn param<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn callback_error(params: &[(String, String)], state: &str) -> Option<String> {
    let get = |key: &str| param(params, key);
    if get("state") != Some(state) {
        return Some("Invalid OAuth state".into());
    }
    // Poe's client pins this issuer but does not require iss; its documented callbacks may omit it.
    if get("iss").is_some_and(|iss| iss != ISSUER) {
        return Some("Invalid OAuth issuer".into());
    }
    if get("error").is_some() {
        let detail = [get("error_description"), get("error")]
            .into_iter()
            .flatten()
            .find(|d| !d.is_empty())
            .unwrap_or("Authorization denied");
        return Some(if detail.contains(state) { "Poe authorization failed".into() } else { detail.to_owned() });
    }
    if get("code").is_some_and(|code| !js_trim(code).is_empty()) { None } else { Some("Missing authorization code".into()) }
}

/// `exchangeCode`: the code and verifier for an API key (`Credential.OAuth`, refreshed by logging in again).
async fn exchange(code: &str, redirect: &str, verifier: &str) -> Result<Json, String> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code", code)
        .append_pair("redirect_uri", redirect)
        .append_pair("code_verifier", verifier)
        .finish();
    let headers = [("content-type".to_owned(), "application/x-www-form-urlencoded".to_owned())];
    let response =
        crate::llm::http::post(TOKEN, &headers, body).await.map_err(|_| "Poe token exchange request failed".to_owned())?;
    let status = response.status;
    let text = response.text().await;
    if !(200..300).contains(&status) {
        let detail = text.ok().as_deref().and_then(error_detail);
        return Err(match detail {
            Some(detail) if ![code, verifier].iter().any(|secret| detail.contains(secret)) => {
                format!("Poe token exchange failed: {detail}")
            }
            _ => format!("Poe token exchange failed ({status})"),
        });
    }
    let invalid = || "Invalid Poe token response".to_owned();
    let token = text.ok().as_deref().and_then(json::parse).ok_or_else(invalid)?;
    let key = match json::get(&token, "api_key") {
        Some(Json::String(key)) => js_trim(key).to_owned(),
        _ => return Err(invalid()),
    };
    if key.is_empty() || key.chars().any(|c| c.is_whitespace() || c == '\u{feff}') {
        return Err(invalid());
    }
    let expires = match json::get(&token, "api_key_expires_in") {
        None | Some(Json::Null) => MAX_EXPIRY,
        Some(Json::Number(seconds)) if seconds.fract() == 0.0 && *seconds >= 0.0 => {
            crate::id::now() as f64 + seconds * 1000.0
        }
        _ => return Err(invalid()),
    };
    if expires > MAX_EXPIRY || expires.fract() != 0.0 || expires.abs() > 9_007_199_254_740_991.0 {
        return Err("Invalid Poe API key expiry".into());
    }
    let mut value = Obj::new();
    value.str("type", "oauth").str("methodID", METHOD).str("access", &key).str("refresh", "");
    value.set("expires", Json::Number(expires));
    Ok(value.done())
}

/// The token endpoint's `{error, error_description}`, the description first.
fn error_detail(text: &str) -> Option<String> {
    let Some(Json::Object(fields)) = json::parse(text) else { return None };
    let field = |key: &str| match fields.get(key) {
        None => Some(None),
        Some(Json::String(value)) => Some(Some(value.clone())),
        Some(_) => None,
    };
    let (description, error) = (field("error_description")?, field("error")?);
    [description, error].into_iter().flatten().find(|d| !d.is_empty())
}

/// `String.prototype.trim`.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}
