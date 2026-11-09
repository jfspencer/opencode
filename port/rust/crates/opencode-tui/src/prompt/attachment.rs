//! Attachments: their labels and identity (`prompt/attachment.ts`) and reading pasted local files
//! (`component/prompt/local-attachment.ts`).
use base64::Engine;

use crate::prompt::history::PromptFile;

fn attachment_kind(uri: &str) -> Option<&'static str> {
    if uri.starts_with("data:image/") {
        return Some("Image");
    }
    if uri.starts_with("data:application/pdf;") {
        return Some("PDF");
    }
    None
}

/// `attachmentMetadata(file)`: `JSON.stringify([name ?? null, description ?? null])`.
fn metadata(name: Option<&str>, description: Option<&str>) -> (Option<String>, Option<String>) {
    (name.map(str::to_string), description.map(str::to_string))
}

/// `deduplicatePromptImages(files)`: an image pasted twice under the same label is one attachment.
pub fn deduplicate_prompt_images(files: &[PromptFile]) -> Vec<PromptFile> {
    if files.len() < 2 {
        return files.to_vec();
    }
    let mut seen: Vec<((Option<String>, Option<String>, String), Vec<String>)> = Vec::new();
    files
        .iter()
        .filter(|file| {
            let Some(text) = file.mention.as_ref().map(|m| m.text.clone()).filter(|t| !t.is_empty()) else { return true };
            if !file.uri.starts_with("data:image/") {
                return true;
            }
            let key = (file.name.clone(), file.description.clone(), text);
            match seen.iter_mut().find(|(k, _)| *k == key) {
                Some((_, payloads)) if payloads.contains(&file.uri) => false,
                Some((_, payloads)) => {
                    payloads.push(file.uri.clone());
                    true
                }
                None => {
                    seen.push((key, vec![file.uri.clone()]));
                    true
                }
            }
        })
        .cloned()
        .collect()
}

/// `preserveMentionlessPromptAttachments(files, mentioned)`: attachments without a mention stay where they were;
/// mentioned ones are replaced, in order, by the still-mentioned ones.
pub fn preserve_mentionless_prompt_attachments(files: Option<&[PromptFile]>, mentioned: Vec<PromptFile>) -> Vec<PromptFile> {
    let Some(files) = files else { return mentioned };
    let mut tracked = mentioned.into_iter();
    files
        .iter()
        .filter_map(|file| {
            if file.mention.as_ref().is_none_or(|m| m.text.is_empty()) {
                return Some(file.clone());
            }
            tracked.next()
        })
        .collect()
}

/// `promptAttachmentLabel(files, file)`: `[Image n]` or `[PDF n]`, numbered after the highest of its kind; an image
/// already attached under the same name keeps its label.
pub fn prompt_attachment_label(files: Option<&[PromptFile]>, uri: &str, name: Option<&str>, description: Option<&str>) -> String {
    let kind = attachment_kind(uri).unwrap_or("Image");
    let identity = metadata(name, description);
    if kind == "Image"
        && let Some(existing) = files.into_iter().flatten().find(|candidate| {
            candidate.uri == uri
                && metadata(candidate.name.as_deref(), candidate.description.as_deref()) == identity
                && candidate.mention.as_ref().is_some_and(|m| !m.text.is_empty())
        })
    {
        return existing.mention.as_ref().map(|m| m.text.clone()).unwrap_or_default();
    }
    let prefix = format!("[{kind} ");
    let count = files
        .into_iter()
        .flatten()
        .filter_map(|candidate| {
            let text = &candidate.mention.as_ref()?.text;
            let digits = text.strip_prefix(&prefix)?.strip_suffix(']')?;
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse::<f64>().unwrap_or(0.0))
        })
        .fold(0.0f64, f64::max);
    format!("[{kind} {}]", count as u64 + 1)
}

// -- component/prompt/local-attachment.ts ------------------------------------------------------------------------

/// Bound filesystem work per terminal paste; the byte budget also bounds staged data.
const MAX_PASTED_FILEPATHS: usize = 32;
pub const MAX_LOCAL_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;

/// A pasted file read as an attachment (`resolvePastedAttachments`' items).
#[derive(Clone, Debug, PartialEq)]
pub enum Pasted {
    /// An SVG: its text, as a pasted placeholder.
    Text { content: String, filename: String },
    /// An image or PDF as a `data:` URI.
    File { uri: String, filename: String, path: String },
}

fn mime_of(path: &str) -> &'static str {
    let ext = std::path::Path::new(path).extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase()));
    match ext.as_deref() {
        Some(".avif") => "image/avif",
        Some(".gif") => "image/gif",
        Some(".jpeg") | Some(".jpg") => "image/jpeg",
        Some(".pdf") => "application/pdf",
        Some(".png") => "image/png",
        Some(".svg") => "image/svg+xml",
        Some(".webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

enum Local {
    Text(String),
    Binary(&'static str, Vec<u8>),
}

/// `readFileBounded(file, maxBytes)`.
fn read_bounded(file: &str, max: usize) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(file).ok()?;
    if !meta.is_file() || meta.len() as usize > max {
        return None;
    }
    let content = std::fs::read(file).ok()?;
    (content.len() <= max).then_some(content)
}

/// `readLocalAttachment(file, maxBytes)`: images and PDFs (SVGs as text).
fn read_local_attachment(file: &str, max: usize) -> Option<Local> {
    let mime = mime_of(file);
    if !mime.starts_with("image/") && mime != "application/pdf" {
        return None;
    }
    let content = read_bounded(file, max)?;
    if mime == "image/svg+xml" {
        let text = String::from_utf8_lossy(&content).into_owned();
        if text.is_empty() || text.len() > max {
            return None;
        }
        return Some(Local::Text(text));
    }
    Some(Local::Binary(mime, content))
}

/// `decodeFileURL(value)` (POSIX).
fn decode_file_url(value: &str) -> Option<String> {
    let rest = value.strip_prefix("file://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let lower = path.to_lowercase();
    if lower.contains("%2f") || lower.contains("%5c") {
        return None;
    }
    let decoded = percent_decode(path)?;
    if !host.is_empty() && host != "localhost" {
        return None;
    }
    Some(decoded)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// `normalizePastedFilepath(value)` (POSIX): quotes trimmed, a `file://` URL decoded, backslash escapes removed.
pub fn normalize_pasted_filepath(value: &str) -> String {
    let raw = value.trim_start_matches(['\'', '"']).trim_end_matches(['\'', '"']);
    if let Some(url) = decode_file_url(raw) {
        return url;
    }
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => out.push(next),
                None => out.push(c),
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// `parsePastedFilepaths(value)` (POSIX): whitespace-separated paths, with quotes and backslash escapes.
pub fn parse_pasted_filepaths(value: &str) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let push = |current: &mut String, result: &mut Vec<String>| {
        if current.is_empty() {
            return;
        }
        result.push(decode_file_url(current).unwrap_or_else(|| current.clone()));
        current.clear();
    };
    let input = if value.contains("file://") {
        value.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).filter(|l| !l.trim_start().starts_with('#')).collect::<Vec<_>>().join("\n")
    } else {
        value.to_string()
    };
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if let Some(q) = quote {
            if character == q {
                quote = None;
                index += 1;
                continue;
            }
            if character == '\\' && q == '"' && index + 1 < chars.len() {
                index += 1;
                current.push(chars[index]);
                index += 1;
                continue;
            }
            current.push(character);
            index += 1;
            continue;
        }
        if character == '\'' || character == '"' {
            quote = Some(character);
            index += 1;
            continue;
        }
        if character == '\\' && index + 1 < chars.len() {
            index += 1;
            current.push(chars[index]);
            index += 1;
            continue;
        }
        if crate::prompt::text::is_js_whitespace(character) {
            push(&mut current, &mut result);
            if result.len() > MAX_PASTED_FILEPATHS {
                return vec![];
            }
            index += 1;
            continue;
        }
        current.push(character);
        index += 1;
    }
    if quote.is_some() {
        return vec![];
    }
    push(&mut current, &mut result);
    if result.len() > MAX_PASTED_FILEPATHS {
        return vec![];
    }
    result
}

fn basename(path: &str) -> String {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string()
}

/// `resolvePastedAttachments(text)`: the pasted text as attachments when it is one local image, PDF or SVG, or
/// several that all are; `None` otherwise (blocking: run it off the runtime thread).
pub fn resolve_pasted_attachments(text: &str, cwd: &str) -> Option<Vec<Pasted>> {
    let pasted = crate::prompt::text::trim(text);
    let filepath = normalize_pasted_filepath(pasted);
    if filepath.starts_with("http://") || filepath.starts_with("https://") {
        return None;
    }
    let mut attachments: Vec<(String, Local)> = Vec::new();
    match read_local_attachment(&filepath, MAX_LOCAL_ATTACHMENT_BYTES) {
        Some(attachment) => attachments.push((filepath, attachment)),
        None => {
            let filepaths = parse_pasted_filepaths(pasted);
            if filepaths.len() <= 1 {
                return None;
            }
            let mut remaining = MAX_LOCAL_ATTACHMENT_BYTES;
            for candidate in filepaths {
                let next = read_local_attachment(&candidate, remaining)?;
                remaining = remaining.saturating_sub(match &next {
                    Local::Text(text) => text.len(),
                    Local::Binary(_, bytes) => bytes.len(),
                });
                attachments.push((candidate, next));
            }
        }
    }
    Some(
        attachments
            .into_iter()
            .map(|(filepath, attachment)| {
                let filename = basename(&filepath);
                match attachment {
                    Local::Text(content) => Pasted::Text { content, filename },
                    Local::Binary(mime, bytes) => Pasted::File {
                        uri: format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)),
                        filename,
                        path: crate::prompt::frecency::resolve_path(cwd, &filepath),
                    },
                }
            })
            .collect(),
    )
}

/// `pathToFileURL(path).href` (POSIX).
pub fn path_to_file_url(path: &str) -> String {
    let mut out = String::from("file://");
    for b in path.bytes() {
        let keep = b.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&b);
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::history::Range;

    fn image(text: &str) -> PromptFile {
        PromptFile { uri: "data:image/png;base64,AA".into(), mention: Some(Range { start: 0, end: 0, text: text.into() }), ..PromptFile::default() }
    }

    #[test]
    fn labels_number_after_the_highest() {
        assert_eq!(prompt_attachment_label(None, "data:image/png;base64,BB", Some("a.png"), None), "[Image 1]");
        let files = [image("[Image 3]")];
        assert_eq!(prompt_attachment_label(Some(&files), "data:image/png;base64,BB", Some("a.png"), None), "[Image 4]");
        assert_eq!(prompt_attachment_label(Some(&files), "data:application/pdf;base64,BB", Some("d.pdf"), None), "[PDF 1]");
    }

    #[test]
    fn pasted_paths() {
        assert_eq!(parse_pasted_filepaths("/a/b.png /c/d.png"), vec!["/a/b.png", "/c/d.png"]);
        assert_eq!(parse_pasted_filepaths("'/a b.png' /c\\ d.png"), vec!["/a b.png", "/c d.png"]);
        assert_eq!(parse_pasted_filepaths("\"/unterminated"), Vec::<String>::new());
        assert_eq!(normalize_pasted_filepath("file:///tmp/x%20y.png"), "/tmp/x y.png");
        assert_eq!(normalize_pasted_filepath("'/tmp/a\\ b.png'"), "/tmp/a b.png");
    }
}
