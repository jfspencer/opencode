//! Server authentication (packages/server/src/auth.ts, middleware/authorization.ts): HTTP Basic credentials, or the
//! same credentials base64-encoded in an `auth_token` query parameter. The user name is `opencode`.

use base64::Engine;
use hyper::header::HeaderMap;

use crate::app::App;
use crate::http::Response;

const USERNAME: &str = "opencode";

pub fn authorized(app: &App, headers: &HeaderMap, query: &[(String, String)]) -> bool {
    let Some(password) = app.password.as_deref().filter(|p| !p.is_empty()) else { return true };
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
    let Some(encoded) = encoded else { return false };
    let Some((user, pass)) = decode(&encoded) else { return false };
    // TODO(port): pairing session tokens (ServerAuth.verifySession) and the session cookie.
    user == USERNAME && pass == password
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

/// The 401 the TS server sends; non-browser clients and page navigations also get a Basic challenge.
pub fn unauthorized(headers: &HeaderMap) -> Response {
    let mode = headers.get("sec-fetch-mode").and_then(|v| v.to_str().ok());
    let mut response = Response::json(401, r#"{"_tag":"UnauthorizedError","message":"Authentication required"}"#.into());
    if mode.is_none() || mode == Some("navigate") {
        response.headers.push(("www-authenticate", r#"Basic realm="Secure Area""#.into()));
    }
    response
}
