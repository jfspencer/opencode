//! File system (handlers/fs.ts, server/src/content-headers.ts): file reads are served like static files (validators,
//! conditional requests, one byte range), always as a download.

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use bytes::Bytes;
use opencode_protocol::Json;
use opencode_protocol::Wire;
use opencode_protocol::types as t;

use super::project::service_unavailable;
use crate::api::Decoded;
use crate::app::{App, defect};
use crate::fs::{Error, FindInput, NotFound, parse_date, utc_string};
use crate::http::{Body, Failure, Request, Response, Result};
use crate::json::Obj;
use crate::location::Location;

pub async fn handle(app: &App, request: &Request, decoded: Decoded, located: Option<Arc<Location>>) -> Result<Response> {
    let location = located.expect("located endpoint");
    match request.route.operation {
        "fs.read" => read(request, &location),
        "fs.list" => {
            let query = t::FsListQuery::decode(&decoded.query).map_err(|issue| Failure::schema("Query", &issue))?;
            let entries = crate::fs::list(&location, query.path.as_deref()).map_err(|e| failure(e, "Directory not found"))?;
            Ok(super::located(request, &location, Json::Array(entries.iter().map(|e| e.to_json()).collect())))
        }
        "fs.find" => {
            let query = t::FsFindQuery::decode(&decoded.query).map_err(|issue| Failure::schema("Query", &issue))?;
            let input = FindInput {
                query: query.query,
                directories: query.r#type.map(|kind| matches!(kind, t::FsFindQueryType::Directory)),
                limit: query.limit.map(|l| l.0.max(0) as usize),
            };
            let entries = location.search.find(&location, &input).await.map_err(|e| defect(e.0))?;
            Ok(super::located(request, &location, Json::Array(entries.iter().map(|e| e.to_json()).collect())))
        }
        "experimental.fs.write" => {
            let query = t::ExperimentalFsWriteQuery::decode(&decoded.query).map_err(|issue| Failure::schema("Query", &issue))?;
            let path = crate::fs::write(&location, &query.path, &request.body).map_err(|e| defect(e.0))?;
            let mut data = Obj::new();
            data.str("path", &path);
            Ok(super::located(request, &location, data.done()))
        }
        "experimental.fs.watch" => {
            let payload = t::ExperimentalFsWatchPayload::decode(&decoded.payload).map_err(|issue| Failure::schema("Payload", &issue))?;
            // Same confinement as read: the file must exist inside the location.
            crate::fs::read(&location, &payload.path).map_err(|e| failure(e, "File not found"))?;
            let file = crate::fs::resolve(&location.directory, &payload.path);
            crate::fs::watch(app, &location, &file)
                .map_err(|_| service_unavailable(crate::fs::WatchDisabled::MESSAGE, "filewatcher"))?;
            Ok(Response::no_content())
        }
        "experimental.fs.unwatch" => {
            let payload = t::ExperimentalFsWatchPayload::decode(&decoded.payload).map_err(|issue| Failure::schema("Payload", &issue))?;
            crate::fs::unwatch(&location, &crate::fs::resolve(&location.directory, &payload.path));
            Ok(Response::no_content())
        }
        _ => super::fallback(app, request),
    }
}

/// `FileNotFoundError` (404), or the 500 of a failure the TS server dies on.
fn failure(error: Error, what: &str) -> Failure {
    match error {
        Error::NotFound(NotFound(path)) => not_found(&path, what),
        Error::Defect(defect_error) => defect(defect_error.0),
    }
}

fn not_found(path: &str, what: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "FileNotFoundError").str("path", path).str("message", &format!("{what}: {path}"));
    Failure::new(404, o.done())
}

/// `GET /api/fs/read/*`: the path is the rest of the URL, percent-decoded as one component.
fn read(request: &Request, location: &Location) -> Result<Response> {
    let raw = request.path.get("/api/fs/read/".len()..).unwrap_or("");
    let path = crate::text::decode_uri_component(raw).ok_or_else(|| not_found(raw, "File not found"))?;
    let file = crate::fs::read(location, &path).map_err(|e| failure(e, "File not found"))?;
    serve(request, &file, &path)
}

fn strip_weak(etag: &str) -> &str {
    let etag = crate::text::trim(etag);
    if etag.len() >= 2 && etag[..2].eq_ignore_ascii_case("w/") { &etag[2..] } else { etag }
}

fn is_not_modified(request: &Request, etag: &str, last_modified: Option<&str>) -> bool {
    if let Some(if_none_match) = request.header("if-none-match") {
        return if_none_match.split(',').any(|tag| crate::text::trim(tag) == "*" || strip_weak(tag) == strip_weak(etag));
    }
    let (Some(since), Some(last)) = (request.header("if-modified-since"), last_modified) else { return false };
    matches!((parse_date(last), parse_date(since)), (Some(a), Some(b)) if a <= b)
}

fn matches_if_range(if_range: Option<&str>, etag: &str, last_modified: Option<&str>) -> bool {
    let Some(if_range) = if_range else { return true };
    let rest = if_range.trim_start_matches(crate::text::is_js_whitespace);
    let tagged = rest.starts_with('"') || (rest.len() >= 3 && rest[..2].eq_ignore_ascii_case("w/") && rest[2..].starts_with('"'));
    if tagged {
        return strip_weak(if_range) == strip_weak(etag);
    }
    matches!((last_modified.and_then(parse_date), parse_date(if_range)), (Some(a), Some(b)) if a == b)
}

enum Range {
    Satisfiable(u64, u64),
    Unsatisfiable,
}

/// `parseRange`: one `bytes=start-end` range (or a suffix); anything else is no range.
fn parse_range(header: &str, size: u64) -> Option<Range> {
    let text = crate::text::trim(header);
    if text.len() < 6 || !text[..6].eq_ignore_ascii_case("bytes=") {
        return None;
    }
    let spec = &text[6..];
    let (first, second) = spec.split_once('-')?;
    let (first, second) = (crate::text::trim(first), crate::text::trim(second));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !digits(first) || !digits(second) || (first.is_empty() && second.is_empty()) {
        return None;
    }
    // `Number` of long digit strings rounds like a double.
    let number = |s: &str| s.parse::<f64>().unwrap_or(f64::INFINITY);
    let size_f = size as f64;
    if first.is_empty() {
        let suffix = number(second);
        if suffix == 0.0 || size == 0 {
            return Some(Range::Unsatisfiable);
        }
        return Some(Range::Satisfiable((size_f - suffix).max(0.0) as u64, size - 1));
    }
    let start = number(first);
    let end = if second.is_empty() { size_f - 1.0 } else { number(second) };
    if start > end || start >= size_f {
        return Some(Range::Unsatisfiable);
    }
    Some(Range::Satisfiable(start as u64, end.min(size_f - 1.0) as u64))
}

/// `ContentHeaders.download`: never rendered as a page on the server's origin.
fn download_headers(path: &str, mime: &str, headers: &mut Vec<(&'static str, String)>) {
    let lower = mime.to_ascii_lowercase();
    let word_end = |rest: &str| rest.chars().next().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
    let script = ["text/html", "text/javascript", "application/javascript", "application/x-javascript"]
        .iter()
        .any(|prefix| lower.strip_prefix(prefix).is_some_and(word_end));
    let xml = ["/xml", "+xml"].iter().any(|needle| lower.match_indices(needle).any(|(i, _)| word_end(&lower[i + needle.len()..])));
    let content_type = if script || xml { "text/plain; charset=utf-8".to_owned() } else { mime.to_owned() };
    headers.push(("content-type", content_type));
    headers.push(("content-disposition", disposition(crate::text::basename(path))));
    headers.push(("content-security-policy", "default-src 'none'; img-src 'self'; media-src 'self'; sandbox".into()));
    headers.push(("x-content-type-options", "nosniff".into()));
}

/// RFC 6266: an ASCII filename for older clients plus the exact UTF-8 name in `filename*`.
fn disposition(name: &str) -> String {
    if name.is_empty() {
        return "attachment".into();
    }
    let fallback: String = name
        .chars()
        .flat_map(|c| {
            let replaced = !(' '..='~').contains(&c) || "\"\\%".contains(c);
            let units = if replaced { c.len_utf16() } else { 1 };
            std::iter::repeat_n(if replaced { '_' } else { c }, units)
        })
        .collect();
    let encoded: String = crate::text::encode_uri_component(name)
        .chars()
        .flat_map(|c| if "'()*".contains(c) { format!("%{:X}", c as u32).chars().collect::<Vec<_>>() } else { vec![c] })
        .collect();
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// `serveFile`.
fn serve(request: &Request, file: &crate::fs::File, path: &str) -> Result<Response> {
    let etag = format!("W/\"{:x}-{}\"", file.size, file.mtime.map(|m| format!("{m:x}")).unwrap_or_else(|| "0".into()));
    let last_modified = file.mtime.map(utc_string);
    let mut headers: Vec<(&'static str, String)> = vec![("cache-control", "no-cache, no-transform".into()), ("etag", etag.clone())];
    if let Some(last) = &last_modified {
        headers.push(("last-modified", last.clone()));
    }
    if is_not_modified(request, &etag, last_modified.as_deref()) {
        download_headers(path, file.mime, &mut headers);
        return Ok(Response { status: 304, headers, body: Body::Empty });
    }
    let range = match request.header("range") {
        Some(range) if request.route.method == "GET" && matches_if_range(request.header("if-range"), &etag, last_modified.as_deref()) => {
            parse_range(range, file.size)
        }
        _ => None,
    };
    let (offset, length, status) = match range {
        Some(Range::Unsatisfiable) => {
            return Ok(Response {
                status: 416,
                headers: vec![("accept-ranges", "bytes".into()), ("content-range", format!("bytes */{}", file.size))],
                body: Body::Empty,
            });
        }
        Some(Range::Satisfiable(start, end)) => {
            headers.push(("content-range", format!("bytes {start}-{end}/{}", file.size)));
            (start, end - start + 1, 206)
        }
        None => (0, file.size, 200),
    };
    download_headers(path, file.mime, &mut headers);
    headers.push(("accept-ranges", "bytes".into()));
    headers.push(("content-length", length.to_string()));
    Ok(Response { status, headers, body: stream(&file.path, offset, length) })
}

const CHUNK: u64 = 64 * 1024;

/// The file's bytes from `offset`, read off the runtime's thread a chunk at a time.
fn stream(path: &str, offset: u64, length: u64) -> Body {
    if length == 0 {
        return Body::Bytes(Bytes::new());
    }
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let Ok(mut file) = std::fs::File::open(&path) else { return };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return;
        }
        let mut remaining = length;
        while remaining > 0 {
            let mut buffer = vec![0u8; remaining.min(CHUNK) as usize];
            let Ok(read) = file.read(&mut buffer) else { return };
            if read == 0 {
                return;
            }
            buffer.truncate(read);
            remaining -= read as u64;
            if sender.blocking_send(Bytes::from(buffer)).is_err() {
                return;
            }
        }
    });
    Body::Stream(receiver)
}
