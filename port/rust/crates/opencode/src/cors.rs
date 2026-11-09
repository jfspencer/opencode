//! Cross-origin handling around the API (packages/server/src/cors.ts, request-site.ts, and effect's
//! `HttpMiddleware.cors` with `isAllowedCorsOrigin` and a 24-hour max age): preflights answer before
//! authentication, other answers carry the allowed origin, and a browser's cross-site request to `/api` from an
//! origin that is not allowed is refused.

use std::sync::OnceLock;

use hyper::header::HeaderMap;

static EXTRA: OnceLock<Vec<String>> = OnceLock::new();

/// The origins `--cors` (or the service's `cors` setting) adds.
pub fn set_origins(origins: Vec<String>) {
    let _ = EXTRA.set(origins);
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// `isAllowedCorsOrigin`.
pub fn allowed(origin: Option<&str>) -> bool {
    let Some(input) = origin.filter(|o| !o.is_empty()) else { return true };
    if input.starts_with("http://localhost:") || input.starts_with("http://127.0.0.1:") || input.starts_with("oc://renderer") {
        return true;
    }
    if matches!(input, "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost") {
        return true;
    }
    // /^https:\/\/([a-z0-9-]+\.)*opencode\.ai$/
    if let Some(host) = input.strip_prefix("https://").and_then(|h| h.strip_suffix("opencode.ai")) {
        if host.is_empty()
            || (host.ends_with('.')
                && host[..host.len() - 1]
                    .split('.')
                    .all(|label| !label.is_empty() && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')))
        {
            return true;
        }
    }
    EXTRA.get().is_some_and(|extra| extra.iter().any(|o| o == input))
}

/// The answer to an `OPTIONS` request.
pub fn preflight(headers: &HeaderMap) -> Vec<(&'static str, String)> {
    let origin = header(headers, "origin");
    let mut out = vec![];
    if let Some(origin) = origin.filter(|o| allowed(Some(o))) {
        out.push(("access-control-allow-origin", origin.to_owned()));
    }
    out.push(("vary", "Origin".to_owned()));
    out.push(("access-control-allow-methods", "GET, HEAD, PUT, PATCH, POST, DELETE".to_owned()));
    out.push(("access-control-max-age", "86400".to_owned()));
    match header(headers, "access-control-request-headers").filter(|h| !h.is_empty()) {
        Some(requested) => {
            out.push(("access-control-allow-headers", requested.to_owned()));
            out.retain(|(name, _)| *name != "vary");
            out.push(("vary", "Origin, Access-Control-Request-Headers".to_owned()));
        }
        // No configured headers and none requested: effect sends the empty list.
        None => out.push(("access-control-allow-headers", String::new())),
    }
    out
}

/// The headers every other answer gets.
pub fn decorate(headers: &HeaderMap) -> Vec<(&'static str, String)> {
    let mut out = vec![];
    if let Some(origin) = header(headers, "origin").filter(|o| allowed(Some(o))) {
        out.push(("access-control-allow-origin", origin.to_owned()));
    }
    out.push(("vary", "Origin".to_owned()));
    out
}

/// `RequestSite.rejected`: a browser's cross-site call to the API from an origin that is not allowed.
pub fn rejected(path: &str, headers: &HeaderMap) -> bool {
    let site = header(headers, "sec-fetch-site");
    if site != Some("cross-site") && site != Some("same-site") {
        return false;
    }
    if path != "/api" && !path.starts_with("/api/") {
        return false;
    }
    if let Some(rest) = path.strip_prefix("/api/experimental/artifacts/") {
        let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
        if !id.is_empty() && (tail == "view" || tail.starts_with("view/")) {
            return false;
        }
    }
    if header(headers, "sec-fetch-mode") == Some("navigate") {
        return true;
    }
    let Some(origin) = header(headers, "origin") else { return false };
    let host = header(headers, "host");
    if host.is_some_and(|host| origin_host(origin).as_deref() == Some(host)) {
        return false;
    }
    !allowed(Some(origin))
}

fn origin_host(origin: &str) -> Option<String> {
    let (_, rest) = origin.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    Some(authority.rsplit_once('@').map_or(authority, |(_, host)| host).to_ascii_lowercase())
}
