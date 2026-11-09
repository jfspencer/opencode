//! Prompt content to and from ACP (packages/cli/src/acp/content.ts, prompt.ts): a prompt's content blocks as the
//! text, files and synthetic context of a submission (with its slash command), and a message's files as replayed
//! content chunks.

use opencode_protocol::Json;

use crate::json::{self, Obj, get, get_str};

/// `PromptPart`.
#[derive(Clone, Debug)]
pub enum Part {
    Text { text: String, synthetic: bool, ignored: bool },
    File { url: String, filename: String },
}

fn text(value: String) -> Part {
    Part::Text { text: value, synthetic: false, ignored: false }
}

/// `ACPPrompt.Prepared`.
pub struct Prepared {
    pub id: String,
    pub compaction: bool,
    pub text: String,
    pub files: Vec<Json>,
    pub synthetic: Vec<String>,
    pub slash: Option<(String, String)>,
    pub command: bool,
}

/// `ACPPrompt.prepare`.
pub async fn prepare(commands: &[Json], content: &[Json]) -> Result<Prepared, super::RpcError> {
    if content.iter().any(|block| get_str(block, "type") == Some("image") && !truthy_str(block, "data") && !truthy_str(block, "uri")) {
        return Err(super::RpcError::invalid_request("image content has no data or uri", Some("prompt")));
    }
    let mut parts = vec![];
    for part in content.iter().flat_map(block_parts) {
        parts.push(reference_unreadable(part).await);
    }
    let visible: Vec<&Part> = parts.iter().filter(|p| !matches!(p, Part::Text { synthetic: true, .. } | Part::Text { ignored: true, .. })).collect();
    let text = visible
        .iter()
        .filter_map(|p| match p {
            Part::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let slash = slash_command(&text);
    let compaction = slash.as_ref().is_some_and(|(name, _)| name == "compact");
    let command = slash.as_ref().is_some_and(|(name, _)| commands.iter().any(|c| get_str(c, "name") == Some(name.as_str())));
    Ok(Prepared {
        id: format!("msg_{}", crate::id::ascending()),
        compaction,
        files: visible
            .iter()
            .filter_map(|p| match p {
                Part::File { url, filename, .. } => {
                    let mut o = Obj::new();
                    o.str("uri", url).str("name", filename);
                    Some(o.done())
                }
                _ => None,
            })
            .collect(),
        synthetic: parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text, synthetic: true, .. } => Some(text.clone()),
                _ => None,
            })
            .collect(),
        text,
        slash,
        command,
    })
}

fn truthy_str(block: &Json, key: &str) -> bool {
    get_str(block, key).is_some_and(|v| !v.is_empty())
}

/// `detectSlashCommand`.
fn slash_command(text: &str) -> Option<(String, String)> {
    let value = text.trim_matches(crate::text::is_js_whitespace);
    let rest = value.strip_prefix('/')?;
    let mut words = rest.split(crate::text::is_js_whitespace).filter({
        // `split(/\s+/)`: runs of whitespace are one separator, but a leading run yields an empty first word.
        let mut first = true;
        move |word: &&str| {
            let keep = first || !word.is_empty();
            first = false;
            keep
        }
    });
    let name = words.next()?.to_owned();
    if name.is_empty() {
        return None;
    }
    let args = words.collect::<Vec<_>>().join(" ");
    Some((name, args.trim_matches(crate::text::is_js_whitespace).to_owned()))
}

/// `referenceUnreadableFile`: a local file the agent cannot read becomes a link in the text.
async fn reference_unreadable(part: Part) -> Part {
    let Part::File { url, filename, .. } = &part else { return part };
    if !url.starts_with("file://") {
        return part;
    }
    let readable = match file_url_to_path(url) {
        Some(path) => tokio::task::spawn_blocking(move || std::fs::File::open(path).is_ok()).await.unwrap_or(false),
        None => false,
    };
    if readable { part } else { link_reference(Some(filename), url) }
}

fn link_reference(name: Option<&str>, uri: &str) -> Part {
    match name.filter(|n| !n.is_empty()) {
        Some(name) => text(format!("[{name}]({uri})")),
        None => text(uri.to_owned()),
    }
}

/// `contentBlockToParts`.
fn block_parts(block: &Json) -> Vec<Part> {
    match get_str(block, "type") {
        Some("text") => {
            let value = get_str(block, "text").unwrap_or("").to_owned();
            let audience: Vec<&str> =
                get(block, "annotations").and_then(|a| get(a, "audience")).and_then(json::array).unwrap_or(&[]).iter().filter_map(json::as_str).collect();
            match audience.as_slice() {
                ["assistant"] => vec![Part::Text { text: value, synthetic: true, ignored: false }],
                ["user"] => vec![Part::Text { text: value, synthetic: false, ignored: true }],
                _ => vec![text(value)],
            }
        }
        Some("image") => {
            let mime = get_str(block, "mimeType").unwrap_or("");
            let uri = get_str(block, "uri");
            let url = match get_str(block, "data").filter(|d| !d.is_empty()) {
                Some(data) => format!("data:{mime};base64,{data}"),
                None => match uri.filter(|u| !u.is_empty()) {
                    Some(uri) => uri.to_owned(),
                    None => return vec![],
                },
            };
            let filename = filename_from_uri(uri).unwrap_or_else(|| "image".into());
            if url.starts_with("data:") {
                return vec![Part::File { url, filename }];
            }
            vec![resource_link(&url, Some(&filename))]
        }
        Some("resource_link") => vec![resource_link(get_str(block, "uri").unwrap_or(""), get_str(block, "name"))],
        Some("resource") => {
            let resource = get(block, "resource").cloned().unwrap_or(Json::Null);
            let uri = get_str(&resource, "uri").unwrap_or("");
            if let Some(body) = get_str(&resource, "text") {
                return vec![text(resource_label(uri).map_or_else(|| format!("[{uri}]\n{body}"), |label| format!("[{label}]\n{body}")))];
            }
            let Some(mime) = get_str(&resource, "mimeType").filter(|m| !m.is_empty()) else { return vec![] };
            let url = if uri.starts_with("data:") { uri.to_owned() } else { format!("data:{mime};base64,{}", get_str(&resource, "blob").unwrap_or("")) };
            vec![Part::File { url, filename: filename_from_uri(Some(uri)).unwrap_or_else(|| "file".into()) }]
        }
        _ => vec![],
    }
}

/// The label of an embedded text resource: a local file's path (and `:line` from a `#L<n>` fragment); `None` for
/// any other URI.
fn resource_label(uri: &str) -> Option<String> {
    let url = Url::parse(uri)?;
    if url.scheme != "file" {
        return None;
    }
    let path = file_url_path(&url).or_else(|| decode_uri_component(&url.pathname))?;
    let line = url.hash.strip_prefix("#L").map(|l| l.chars().take_while(char::is_ascii_digit).collect::<String>()).filter(|l| !l.is_empty());
    Some(match line {
        Some(line) => format!("{path}:{line}"),
        None => path,
    })
}

/// `resourceLinkToPart`.
/// (The part's type, `mimeType` or `text/plain`, is not part of a submission.)
fn resource_link(uri: &str, name: Option<&str>) -> Part {
    let name = name.filter(|n| !n.is_empty());
    if uri.starts_with("file://") {
        let filename = name.map(String::from).or_else(|| filename_from_uri(Some(uri))).unwrap_or_else(|| "file".into());
        return Part::File { url: uri.to_owned(), filename };
    }
    if uri.starts_with("zed://") {
        if let Some(path) = Url::parse(uri).and_then(|u| u.param("path")).filter(|p| !p.is_empty()) {
            let base = basename(&path);
            let filename = name.map(String::from).or_else(|| (!base.is_empty()).then(|| base.to_owned())).unwrap_or_else(|| "file".into());
            return Part::File { url: path_to_file_url(&path), filename };
        }
    }
    link_reference(name, uri)
}

/// `filenameFromUri`.
fn filename_from_uri(uri: Option<&str>) -> Option<String> {
    let uri = uri.filter(|u| !u.is_empty() && !u.starts_with("data:"))?;
    let base = match Url::parse(uri) {
        Some(url) => {
            let path = if url.scheme == "zed" { url.param("path").filter(|p| !p.is_empty()).unwrap_or(url.pathname.clone()) } else { url.pathname.clone() };
            basename(&path).to_owned()
        }
        None => basename(uri).to_owned(),
    };
    (!base.is_empty()).then_some(base)
}

/// `path.basename`.
fn basename(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

/// The parts of a URL these conversions read, as WHATWG `URL` gives them (`pathname` and `search` still
/// percent-encoded).
struct Url {
    scheme: String,
    host: String,
    pathname: String,
    search: String,
    hash: String,
}

impl Url {
    /// `URL.canParse(text) ? new URL(text) : undefined`, for absolute URLs.
    fn parse(text: &str) -> Option<Url> {
        let text = text.trim_matches(|c: char| c <= ' ');
        let colon = text.find(':')?;
        let scheme = &text[..colon];
        if scheme.is_empty() || !scheme.starts_with(|c: char| c.is_ascii_alphabetic()) || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
            return None;
        }
        let scheme = scheme.to_ascii_lowercase();
        let rest = &text[colon + 1..];
        let (rest, hash) = match rest.find('#') {
            Some(at) => (&rest[..at], rest[at..].to_owned()),
            None => (rest, String::new()),
        };
        let (rest, search) = match rest.find('?') {
            Some(at) => (&rest[..at], rest[at..].to_owned()),
            None => (rest, String::new()),
        };
        let (host, pathname) = match rest.strip_prefix("//") {
            Some(after) => match after.find('/') {
                Some(at) => (after[..at].to_owned(), after[at..].to_owned()),
                None => (after.to_owned(), if scheme == "file" || special(&scheme) { "/".to_owned() } else { String::new() }),
            },
            None => (String::new(), rest.to_owned()),
        };
        if special(&scheme) && scheme != "file" && host.is_empty() {
            return None;
        }
        Some(Url { scheme, host, pathname: encode_path(&pathname), search, hash })
    }

    /// `searchParams.get(name)`.
    fn param(&self, name: &str) -> Option<String> {
        let query = self.search.strip_prefix('?').unwrap_or(&self.search);
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (form_decode(key) == name).then(|| form_decode(value))
        })
    }
}

fn special(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss" | "ftp" | "file")
}

fn form_decode(text: &str) -> String {
    crate::http::percent_decode(&text.replace('+', " "))
}

/// `decodeURIComponent`, `None` where it throws.
fn decode_uri_component(text: &str) -> Option<String> {
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
    String::from_utf8(out).ok()
}

/// `fileURLToPath(url)` on POSIX: `None` where it throws (a remote host, an encoded `/`).
fn file_url_path(url: &Url) -> Option<String> {
    if !url.host.is_empty() && url.host != "localhost" {
        return None;
    }
    let lower = url.pathname.to_ascii_lowercase();
    if lower.contains("%2f") {
        return None;
    }
    Some(crate::http::percent_decode(&url.pathname))
}

/// `fileURLToPath` of a `file://` URL string.
fn file_url_to_path(text: &str) -> Option<String> {
    let url = Url::parse(text)?;
    if url.scheme != "file" {
        return None;
    }
    file_url_path(&url)
}

/// The WHATWG path percent-encode set (and `%`, which `pathToFileURL` encodes too).
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            ' ' | '"' | '<' | '>' | '`' | '{' | '}' => out.push_str(&format!("%{:02X}", c as u32)),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("%{:02X}", c as u32)),
            c if !c.is_ascii() => {
                let mut buffer = [0u8; 4];
                for byte in c.encode_utf8(&mut buffer).bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// `pathToFileURL(path).href`: resolved against the working directory, percent-encoded.
fn path_to_file_url(path: &str) -> String {
    let resolved = crate::project::absolutize(path);
    let mut out = String::from("file://");
    for c in resolved.chars() {
        match c {
            '%' => out.push_str("%25"),
            '\\' => out.push_str("%5C"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            c => out.push_str(&encode_path(&c.to_string())),
        }
    }
    out
}

/// `partsToContentChunks` of a message's files.
pub fn file_chunks(files: &[(String, Option<String>, String)]) -> Vec<Json> {
    files
        .iter()
        .filter_map(|(url, filename, mime)| {
            let content = if url.starts_with("file://") {
                let mut o = Obj::new();
                o.str("type", "resource_link").str("uri", url).str("name", filename.as_deref().unwrap_or("file")).str("mimeType", mime);
                o.done()
            } else {
                let rest = url.strip_prefix("data:")?;
                let (mime, data) = rest.split_once(";base64,")?;
                if mime.is_empty() || mime.contains(';') {
                    return None;
                }
                let mut o = Obj::new();
                if mime.starts_with("image/") {
                    o.str("type", "image").str("mimeType", mime).str("data", data).str("uri", &path_to_file_url(filename.as_deref().unwrap_or("image")));
                } else {
                    let mut resource = Obj::new();
                    resource.str("uri", &path_to_file_url(filename.as_deref().unwrap_or("file"))).str("mimeType", mime);
                    if mime.starts_with("text/") || mime == "application/json" {
                        resource.str("text", &String::from_utf8_lossy(&base64_lenient(data)));
                    } else {
                        resource.str("blob", data);
                    }
                    o.str("type", "resource").set("resource", resource.done());
                }
                o.done()
            };
            let mut chunk = Obj::new();
            chunk.set("content", content);
            Some(chunk.done())
        })
        .collect()
}

/// `Buffer.from(data, "base64")`: characters outside the alphabet are skipped, padding is optional.
fn base64_lenient(data: &str) -> Vec<u8> {
    use base64::Engine;
    let clean: String = data
        .chars()
        .filter_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' => Some(c),
            '-' => Some('+'),
            '_' => Some('/'),
            _ => None,
        })
        .collect();
    // A lone trailing character carries no whole byte.
    let usable = if clean.len() % 4 == 1 { &clean[..clean.len() - 1] } else { &clean[..] };
    let config = base64::engine::GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true);
    base64::engine::GeneralPurpose::new(&base64::alphabet::STANDARD, config).decode(usable).unwrap_or_default()
}
