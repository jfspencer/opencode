//! Prompt attachments (packages/core/src/session/prompt.ts `materializeAttachment`, packages/core/src/mime.ts): a
//! prompt's file inputs (`file:` URIs, directories, `data:` URLs) become `Prompt.FileAttachment`s with their bytes
//! inline (base64), images normalized as tool results' are ([`crate::tool::file_media`]); a `media:` ref a client read
//! back re-admits its stored blob. Storing them as blobs is the inbox's (`experimental.media_blob`).

use base64::Engine;
use opencode_protocol::Json;

use crate::json::{self, Obj};

const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;

fn base64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn is_text(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    if bytes.contains(&0) {
        return false;
    }
    // `TextDecoder(fatal, stream)`: an incomplete sequence at the end is accepted.
    match std::str::from_utf8(bytes) {
        Ok(_) => {}
        Err(error) if error.error_len().is_none() => {}
        Err(_) => return false,
    }
    let controls = bytes.iter().filter(|b| **b < 9 || (**b > 13 && **b < 32)).count();
    controls as f64 / bytes.len() as f64 <= 0.3
}

/// `Mime.detect`: the type of a file from its first bytes.
pub fn detect(bytes: &[u8]) -> &'static str {
    let starts = |prefix: &[u8]| bytes.starts_with(prefix);
    if starts(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return "image/png";
    }
    if starts(&[0xff, 0xd8, 0xff]) {
        return "image/jpeg";
    }
    if starts(&[0x47, 0x49, 0x46, 0x38]) {
        return "image/gif";
    }
    if starts(&[0x42, 0x4d]) {
        return "image/bmp";
    }
    if starts(&[0x25, 0x50, 0x44, 0x46, 0x2d]) {
        return "application/pdf";
    }
    if starts(&[0x52, 0x49, 0x46, 0x46]) && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let brand = &bytes[8..12];
        let box_end = (u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize).min(bytes.len());
        let compatible: Vec<&[u8]> = (0..box_end.saturating_sub(16) / 4).map(|i| &bytes[16 + i * 4..20 + i * 4]).collect();
        let generic = brand == b"mif1" || brand == b"msf1";
        if brand == b"avif" || brand == b"avis" || (generic && compatible.iter().any(|c| *c == b"avif" || *c == b"avis")) {
            return "image/avif";
        }
        if generic || [b"heic", b"heix", b"hevc", b"hevx"].iter().any(|b| brand == *b) {
            return "image/heic";
        }
        if brand == b"qt  " {
            return "video/quicktime";
        }
        if brand == b"M4A " {
            return "audio/mp4";
        }
        return "video/mp4";
    }
    if is_text(bytes) { "text/plain" } else { "application/octet-stream" }
}

/// `decodeURIComponent` as bytes; `None` for a malformed escape.
fn decode_uri_component(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out.clone()).ok()?;
    Some(out)
}

struct Resolved {
    bytes: Vec<u8>,
    source: Json,
    start: Option<usize>,
    end: Option<usize>,
    name: Option<String>,
    mime: Option<&'static str>,
}

fn source(ty: &str, uri: Option<&str>) -> Json {
    let mut o = Obj::new();
    o.str("type", ty);
    o.opt_str("uri", uri);
    o.done()
}

fn positive_int(value: Option<&str>) -> Option<usize> {
    let n: f64 = value?.trim().parse().ok()?;
    (n.fract() == 0.0 && n > 0.0).then_some(n as usize)
}

/// The scheme of a URL (lower-cased), as the WHATWG parser reads it; `None` when there is none.
fn scheme(uri: &str) -> Option<String> {
    let (scheme, _) = uri.split_once(':')?;
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then(|| scheme.to_ascii_lowercase())
}

/// `fileURLToPath` of a `file:` URL (its query and fragment dropped); `None` when it names no local path.
pub fn file_url_path(uri: &str) -> Option<String> {
    if scheme(uri).as_deref() != Some("file") {
        return None;
    }
    let rest = &uri[5..];
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let path = match rest.strip_prefix("//") {
        Some(authority) => {
            let (host, path) = authority.find('/').map(|i| authority.split_at(i)).unwrap_or((authority, "/"));
            if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
                return None;
            }
            path
        }
        None => rest,
    };
    let path = if path.starts_with('/') { path.to_owned() } else { format!("/{path}") };
    if path.to_ascii_lowercase().contains("%2f") {
        return None;
    }
    decode_uri_component(&path).and_then(|bytes| String::from_utf8(bytes).ok())
}

fn read_file(uri: &str) -> Result<Resolved, String> {
    match scheme(uri).as_deref() {
        None => return Err(format!("Invalid attachment URI: {uri}")),
        Some("file") => {}
        Some(_) => return Err(format!("Unsupported attachment URI: {uri}")),
    }
    let query = uri.split('#').next().unwrap_or_default().split_once('?').map(|(_, q)| crate::http::parse_query(q)).unwrap_or_default();
    let param = |key: &str| query.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let (start, end) = (positive_int(param("start").as_deref()), positive_int(param("end").as_deref()));
    let target = file_url_path(uri).ok_or_else(|| format!("Invalid file URI: {uri}"))?;
    let unreadable = || format!("Unable to read attachment: {uri}");
    let metadata = std::fs::metadata(&target).map_err(|_| unreadable())?;
    let name = std::path::Path::new(&target).file_name().map(|n| n.to_string_lossy().into_owned());
    if metadata.is_dir() {
        let mut entries: Vec<(bool, String)> = std::fs::read_dir(&target)
            .map_err(|_| unreadable())?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let kind = entry.file_type().ok()?;
                (kind.is_file() || kind.is_dir()).then(|| (kind.is_dir(), entry.file_name().to_string_lossy().into_owned()))
            })
            .collect();
        entries.sort_by(|a, b| match (a.0, b.0) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => crate::collate::compare(&a.1, &b.1),
        });
        let listing: Vec<String> = entries.into_iter().map(|(dir, name)| if dir { format!("{name}/") } else { name }).collect();
        return Ok(Resolved {
            bytes: listing.join("\n").into_bytes(),
            source: source("uri", Some(uri)),
            start: None,
            end: None,
            name,
            mime: Some("application/x-directory"),
        });
    }
    if !metadata.is_file() {
        return Err(format!("Attachment is not a file: {uri}"));
    }
    if metadata.len() as usize > MAX_ATTACHMENT_BYTES {
        return Err(format!("Attachment exceeds the {MAX_ATTACHMENT_BYTES} byte limit: {uri}"));
    }
    let bytes = std::fs::read(&target).map_err(|_| unreadable())?;
    Ok(Resolved { bytes, source: source("uri", Some(uri)), start, end, name, mime: None })
}

fn decode_data_url(uri: &str, label: &str) -> Result<Vec<u8>, String> {
    let invalid = || format!("Invalid attachment data URL: {label}");
    let comma = uri.find(',').ok_or_else(invalid)?;
    let metadata = &uri[5..comma];
    let payload = &uri[comma + 1..];
    if !metadata.split(';').any(|part| part.eq_ignore_ascii_case("base64")) {
        return decode_uri_component(payload).ok_or_else(invalid);
    }
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| invalid())?;
    if base64().encode(&bytes) != payload {
        return Err(invalid());
    }
    Ok(bytes)
}

/// `materializeAttachment`: an input attachment (`PromptInput.FileAttachment`) as a `Prompt.FileAttachment`, or the
/// `AttachmentError` message. A `media:` ref (from an `experimental-media-uri` read) re-admits its stored blob, whose
/// bytes were normalized when first admitted.
pub async fn materialize(db: &std::sync::Mutex<rusqlite::Connection>, input: &Json) -> Result<Json, String> {
    let uri = json::get_str(input, "uri").unwrap_or_default();
    let name = json::get_str(input, "name");
    let given_source = json::get(input, "source").filter(|s| !matches!(s, Json::Null)).cloned();
    if let Some(hash) = uri.strip_prefix("media:").filter(|hash| hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))) {
        let blob = crate::session::blob::read(&db.lock().unwrap(), hash).ok().flatten();
        let Some((mime, data)) = blob else { return Err(format!("Attachment is no longer stored: {uri}")) };
        let mut o = Obj::new();
        o.str("data", &base64().encode(&data)).str("mime", &mime);
        o.set("source", given_source.unwrap_or_else(|| source("inline", None)));
        o.opt_str("name", name);
        o.opt("description", json::get(input, "description").filter(|v| !matches!(v, Json::Null)).cloned());
        o.opt("mention", json::get(input, "mention").filter(|v| !matches!(v, Json::Null)).cloned());
        return Ok(o.done());
    }
    let label = name.map(str::to_owned).unwrap_or_else(|| if uri.starts_with("data:") { "inline attachment".into() } else { uri.into() });
    let resolved = if uri.starts_with("data:") {
        Resolved {
            bytes: decode_data_url(uri, &label)?,
            source: given_source.clone().unwrap_or_else(|| source("inline", None)),
            start: None,
            end: None,
            name: None,
            mime: None,
        }
    } else {
        read_file(uri)?
    };
    if resolved.bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(format!("Attachment exceeds the {MAX_ATTACHMENT_BYTES} byte limit: {label}"));
    }
    let mime = resolved.mime.unwrap_or_else(|| detect(&resolved.bytes));
    let content = match (mime, resolved.start) {
        ("text/plain", Some(start)) => {
            let text = String::from_utf8_lossy(&resolved.bytes);
            let lines: Vec<&str> = text.split('\n').collect();
            let end = resolved.end.unwrap_or(lines.len()).min(lines.len());
            let from = (start - 1).min(lines.len());
            lines[from..end.max(from)].join("\n").into_bytes()
        }
        _ => resolved.bytes,
    };
    let (data, mime) = normalize_image(base64().encode(&content), mime, &label).await?;
    let mut o = Obj::new();
    o.str("data", &data).str("mime", mime).set("source", resolved.source);
    o.opt_str("name", name.or(resolved.name.as_deref()));
    o.opt("description", json::get(input, "description").filter(|v| !matches!(v, Json::Null)).cloned());
    o.opt("mention", json::get(input, "mention").filter(|v| !matches!(v, Json::Null)).cloned());
    Ok(o.done())
}

/// `normalizeImageAttachment`: an image of a type the resizer decodes, normalized like a tool's
/// ([`crate::tool::file_media`]); one that cannot be decoded or made to fit fails the prompt. Others pass unchanged.
async fn normalize_image(data: String, mime: &'static str, label: &str) -> Result<(String, &'static str), String> {
    use crate::tool::file_media::{self, Normalized};
    if !matches!(mime, "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp") {
        return Ok((data, mime));
    }
    match file_media::normalize_async(mime.to_owned(), data.clone()).await {
        Normalized::Unchanged => Ok((data, mime)),
        Normalized::Content { data, mime } => Ok((data, mime)),
        Normalized::DecodeError => Err(format!("Image could not be decoded: {label}")),
        Normalized::SizeError { width, height, bytes } => Err(file_media::size_error(label, width, height, bytes)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_types() {
        assert_eq!(detect(b"\x89PNG\r\n\x1a\nrest"), "image/png");
        assert_eq!(detect(b"%PDF-1.4"), "application/pdf");
        assert_eq!(detect(b"PK not really"), "text/plain");
        assert_eq!(detect(b"PK\x03\x04\x14\x00\x00\x00\x08\x00\x00\x00!\x00\xff\xfe"), "application/octet-stream");
    }

    #[test]
    fn file_urls() {
        assert_eq!(file_url_path("file:///tmp/a%20b.txt?start=2#x").as_deref(), Some("/tmp/a b.txt"));
        assert_eq!(file_url_path("FILE://localhost/tmp/x").as_deref(), Some("/tmp/x"));
        assert_eq!(file_url_path("file://host/tmp/x"), None);
        assert_eq!(file_url_path("file:///tmp/a%2Fb"), None);
        assert_eq!(file_url_path("https://example.com/x"), None);
        assert_eq!(read_file("relative/path").err().as_deref(), Some("Invalid attachment URI: relative/path"));
        assert_eq!(read_file("https://example.com/x").err().as_deref(), Some("Unsupported attachment URI: https://example.com/x"));
    }

    #[test]
    fn data_urls() {
        assert_eq!(decode_data_url("data:text/plain;base64,dW5uYW1lZA==", "x").unwrap(), b"unnamed");
        assert_eq!(decode_data_url("data:,a%20b", "x").unwrap(), b"a b");
        assert!(decode_data_url("data:;base64,dW5uYW1lZA", "x").is_err());
    }
}
