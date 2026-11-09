//! Server authentication (packages/server/src/auth.ts, middleware/authorization.ts): HTTP Basic credentials, or the
//! same credentials base64-encoded in an `auth_token` query parameter. The user name is `opencode`; the password is
//! the server's, or a session token a pairing link issued (also accepted as the `opencode_session[_<port>]` cookie
//! of same-origin requests). Pairing links themselves need no credentials.
//! Ticketed terminal connects skip credentials too: their handlers consume the ticket. TODO(port): the ticketed MCP
//! client connect.

use base64::Engine;
use hyper::header::HeaderMap;

use crate::app::App;
use crate::http::Response;

const USERNAME: &str = "opencode";

/// `ServerAuth.SESSION_TTL_SECONDS`.
pub const SESSION_TTL_SECONDS: i64 = 30 * 24 * 60 * 60;

pub fn authorized(app: &App, path: &str, headers: &HeaderMap, query: &[(String, String)]) -> bool {
    let Some(password) = app.password.as_deref().filter(|p| !p.is_empty()) else { return true };
    // PAIRING_CONNECT_PATH (`/auth/connect/:code`): its handler consumes and validates the code.
    if path.strip_prefix("/auth/connect/").is_some_and(|code| !code.is_empty() && !code.contains('/')) {
        return true;
    }
    if crate::pty::ticket::ticketed(path, query) {
        return true;
    }
    let token = query.iter().find(|(k, _)| k == "auth_token").map(|(_, v)| v.as_str()).filter(|v| !v.is_empty());
    let encoded = match token {
        Some(token) => Some(token.to_owned()),
        None => headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                let (scheme, rest) = v.split_once(char::is_whitespace)?;
                scheme.eq_ignore_ascii_case("basic").then(|| rest.trim_start().to_owned())
            })
            .filter(|v| !v.is_empty()),
    };
    let credential = encoded.as_deref().and_then(decode);
    if credential.is_some_and(|(user, pass)| user == USERNAME && (pass == password || verify_session(&pass, password))) {
        return true;
    }
    session_cookie(headers, password)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// `authorizedSessionCookie`: only same-origin requests may use the cookie (same-site pages on other ports send it).
fn session_cookie(headers: &HeaderMap, password: &str) -> bool {
    let host = header(headers, "host");
    let name = session_cookie_name(host);
    let Some(token) = header(headers, "cookie").and_then(|cookies| {
        cookies.split(';').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.trim() == name).then(|| crate::text::decode_uri_component(value.trim()).unwrap_or_else(|| value.trim().to_owned()))
        })
    }) else {
        return false;
    };
    if token.is_empty() {
        return false;
    }
    if let Some(origin) = header(headers, "origin") {
        let origin_host = origin.split_once("://").map(|(_, rest)| rest.split('/').next().unwrap_or(""));
        if origin_host != host {
            return false;
        }
    }
    verify_session(&token, password)
}

/// `sessionCookieName`: browsers share cookies across ports on the same host, so the name carries the port.
pub fn session_cookie_name(host: Option<&str>) -> String {
    let port = host.and_then(|host| host.rsplit_once(':')).map(|(_, port)| port).filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    match port {
        Some(port) => format!("opencode_session_{port}"),
        None => "opencode_session".into(),
    }
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let inner: Vec<u8> = block.iter().map(|b| b ^ 0x36).collect();
    let outer: Vec<u8> = block.iter().map(|b| b ^ 0x5c).collect();
    let inner_hash = Sha256::new().chain_update(&inner).chain_update(message).finalize();
    Sha256::new().chain_update(&outer).chain_update(inner_hash).finalize().into()
}

/// `sign`: sessions are signed with a key derived from the server password, so rotating it revokes them all.
fn sign(password: &str, payload: &str) -> String {
    let key = hmac_sha256(password.as_bytes(), b"opencode-session-v1");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hmac_sha256(&key, payload.as_bytes()))
}

/// `ServerAuth.issueSession`: `<expiry in seconds>.<signature>`.
pub fn issue_session(password: &str) -> String {
    let expires = (crate::id::now() / 1000 + SESSION_TTL_SECONDS).to_string();
    let signature = sign(password, &expires);
    format!("{expires}.{signature}")
}

/// `ServerAuth.verifySession`.
fn verify_session(token: &str, password: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    let [expires, signature] = parts[..] else { return false };
    let seconds = opencode_protocol::wire::number::js_string_to_number(expires);
    if !opencode_protocol::wire::number::is_safe_integer(seconds) || seconds * 1000.0 <= crate::id::now() as f64 {
        return false;
    }
    let expected = sign(password, expires);
    // Constant time over equal lengths (`timingSafeEqual`).
    expected.len() == signature.len() && expected.bytes().zip(signature.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn decode(input: &str) -> Option<(String, String)> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let bytes = LENIENT.decode(input).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let (user, pass) = text.split_once(':')?;
    Some((user.to_owned(), pass.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_tokens() {
        // RFC 4231 test case 2.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        let token = issue_session("secret");
        assert!(verify_session(&token, "secret"));
        assert!(!verify_session(&token, "other"));
        assert!(!verify_session("1.abc", "secret"));
        assert_eq!(session_cookie_name(Some("127.0.0.1:4096")), "opencode_session_4096");
        assert_eq!(session_cookie_name(Some("localhost")), "opencode_session");
    }
}

/// The 401 the TS server sends; non-browser clients and page navigations also get a Basic challenge.
pub fn unauthorized(headers: &HeaderMap) -> Response {
    let mode = headers.get("sec-fetch-mode").and_then(|v| v.to_str().ok());
    let mut response = Response::json(401, r#"{"_tag":"UnauthorizedError","message":"Authentication required"}"#.into());
    if mode.is_none() || mode == Some("navigate") {
        response.headers.push(("www-authenticate", r#"Basic realm="Secure Area""#.into()));
    }
    response
}
