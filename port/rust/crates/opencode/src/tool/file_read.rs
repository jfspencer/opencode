//! What the read tool reads (packages/core/src/tool/read-filesystem.ts `ReadToolFileSystem.read`): a directory as a
//! page of entries; an image or PDF (sniffed from its first bytes) whole, as base64, up to 20 MiB; a small text file
//! whole; anything larger than 50 KiB, or read with an offset or limit, as a page of at most 2,000 lines and 50 KiB,
//! read in 256 KiB chunks until the page settles. A NUL anywhere before the end of what is shown makes the file
//! binary. Lines longer than 2,000 characters are clamped.
//!
//! Where TS keeps every chunk up to the requested line, chunks wholly before it are dropped once scanned (their
//! newlines counted and checked for NUL), so a deep offset into a large file holds at most the page's chunks.

use std::io::{Read, Seek, SeekFrom};

use crate::session::attachment::detect;

pub const MAX_READ_LINES: usize = 2_000;
pub const MAX_READ_BYTES: usize = 50 * 1024;
pub const MAX_MEDIA_INGEST_BYTES: u64 = 20 * 1024 * 1024;
const FIRST_CHUNK: usize = 256 * 1024;
const MAX_LINE_LENGTH: usize = 2_000;
const MEDIA_MIMES: [&str; 5] = ["image/png", "image/jpeg", "image/gif", "image/webp", "application/pdf"];

/// `Environment.FileType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
    Symlink,
    Other,
}

/// `Environment.NotFound | WrongKind | Failed`.
#[derive(Debug)]
pub enum FsError {
    NotFound,
    WrongKind(FileType),
    Failed,
}

/// The environment's files (`Environment.Files`): content operations follow a final symlink, listed entries keep
/// their own type.
pub trait Files {
    /// The file's size and up to `length` bytes from `offset` (the whole file without a range).
    fn read(&self, path: &str, range: Option<(u64, usize)>) -> Result<(u64, Vec<u8>), FsError>;
    fn list(&self, path: &str) -> Result<Vec<(String, FileType)>, FsError>;
}

fn kind_of(file_type: std::fs::FileType) -> FileType {
    if file_type.is_file() {
        FileType::File
    } else if file_type.is_dir() {
        FileType::Directory
    } else if file_type.is_symlink() {
        FileType::Symlink
    } else {
        FileType::Other
    }
}

/// `isMissing`: `ENOENT` or `ENOTDIR`.
fn missing(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(20)
}

fn io(error: std::io::Error) -> FsError {
    if missing(&error) { FsError::NotFound } else { FsError::Failed }
}

/// The host's files (environment/local.ts).
pub struct Local;

impl Files for Local {
    fn read(&self, path: &str, range: Option<(u64, usize)>) -> Result<(u64, Vec<u8>), FsError> {
        let info = std::fs::metadata(path).map_err(io)?;
        let kind = kind_of(info.file_type());
        if kind != FileType::File {
            return Err(FsError::WrongKind(kind));
        }
        let mut file = std::fs::File::open(path).map_err(io)?;
        let bytes = match range {
            None => {
                let mut bytes = Vec::with_capacity(info.len() as usize);
                file.read_to_end(&mut bytes).map_err(io)?;
                bytes
            }
            Some((offset, length)) => {
                file.seek(SeekFrom::Start(offset)).map_err(io)?;
                let mut bytes = Vec::with_capacity(length.min(info.len().saturating_sub(offset) as usize));
                file.take(length as u64).read_to_end(&mut bytes).map_err(io)?;
                bytes
            }
        };
        Ok((info.len(), bytes))
    }

    fn list(&self, path: &str) -> Result<Vec<(String, FileType)>, FsError> {
        let info = std::fs::metadata(path).map_err(io)?;
        let kind = kind_of(info.file_type());
        if kind != FileType::Directory {
            return Err(FsError::WrongKind(kind));
        }
        let entries = std::fs::read_dir(path).map_err(io)?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io)?;
            let kind = entry.file_type().map(kind_of).unwrap_or(FileType::Other);
            out.push((entry.file_name().to_string_lossy().into_owned(), kind));
        }
        Ok(out)
    }
}

/// An in-memory tree of files (environment/memory.ts), for a skill's virtual files: paths are absolute, directories
/// implied by the files under them.
pub struct Memory {
    pub files: Vec<(String, Vec<u8>)>,
}

impl Memory {
    fn directory(&self, path: &str) -> bool {
        path == "/" || self.files.iter().any(|(file, _)| file.starts_with(&format!("{path}/")))
    }
}

impl Files for Memory {
    fn read(&self, path: &str, range: Option<(u64, usize)>) -> Result<(u64, Vec<u8>), FsError> {
        let key = crate::project::normalize(path);
        match self.files.iter().find(|(file, _)| *file == key) {
            Some((_, bytes)) => {
                let slice = match range {
                    None => bytes.clone(),
                    Some((offset, length)) => {
                        let start = (offset as usize).min(bytes.len());
                        bytes[start..(start + length).min(bytes.len())].to_vec()
                    }
                };
                Ok((bytes.len() as u64, slice))
            }
            None if self.directory(&key) => Err(FsError::WrongKind(FileType::Directory)),
            None => Err(FsError::NotFound),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<(String, FileType)>, FsError> {
        let key = crate::project::normalize(path);
        if self.files.iter().any(|(file, _)| *file == key) {
            return Err(FsError::WrongKind(FileType::File));
        }
        if !self.directory(&key) {
            return Err(FsError::NotFound);
        }
        let prefix = if key == "/" { "/".to_owned() } else { format!("{key}/") };
        let mut out: Vec<(String, FileType)> = Vec::new();
        for (file, _) in &self.files {
            let Some(rest) = file.strip_prefix(&prefix) else { continue };
            let (name, kind) = match rest.split_once('/') {
                Some((name, _)) => (name, FileType::Directory),
                None => (rest, FileType::File),
            };
            if !out.iter().any(|(seen, _)| seen == name) {
                out.push((name.to_owned(), kind));
            }
        }
        out.sort_by(|a, b| crate::collate::compare(&a.0, &b.0));
        Ok(out)
    }
}

/// `ReadToolFileSystem.PageInput`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Page {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

impl Page {
    fn offset(&self) -> usize {
        self.offset.filter(|o| *o > 0).unwrap_or(1)
    }

    fn limit(&self) -> usize {
        self.limit.filter(|l| *l > 0).unwrap_or(MAX_READ_LINES).min(MAX_READ_LINES)
    }
}

/// `ReadTool.ListEntry`.
#[derive(Debug, Clone)]
pub struct ListEntry {
    pub path: String,
    pub kind: FileType,
}

/// What a read returns (`FileContent | TextPage | ListPage`).
#[derive(Debug, Clone)]
pub enum Content {
    /// A whole file: text (`utf8`) or media (base64).
    File { content: String, base64: bool, mime: String },
    TextPage { content: String, offset: usize, next: Option<usize> },
    ListPage { entries: Vec<ListEntry>, next: Option<usize> },
}

impl Content {
    pub fn truncated(&self) -> bool {
        match self {
            Content::File { .. } => false,
            Content::TextPage { next, .. } | Content::ListPage { next, .. } => next.is_some(),
        }
    }
}

/// `ReadError`: the environment's errors and the read's own.
#[derive(Debug)]
pub enum ReadError {
    NotFound,
    Failed,
    /// `BinaryFileError` and the other model-facing errors: their messages.
    Message(String),
}

impl From<FsError> for ReadError {
    fn from(error: FsError) -> Self {
        match error {
            FsError::NotFound => ReadError::NotFound,
            _ => ReadError::Failed,
        }
    }
}

fn binary(resource: &str) -> ReadError {
    ReadError::Message(format!("Cannot read binary file: {resource}"))
}

fn path_kind(resource: &str, expected: &str) -> ReadError {
    ReadError::Message(format!("Path is not {expected}: {resource}"))
}

/// `TextDecoder().decode(bytes)`: invalid UTF-8 replaced, a leading BOM dropped.
pub fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// `clampLine`: at most 2,000 UTF-16 units, then a note.
fn clamp_line(line: &str) -> std::borrow::Cow<'_, str> {
    if line.len() <= MAX_LINE_LENGTH || line.encode_utf16().count() <= MAX_LINE_LENGTH {
        return std::borrow::Cow::Borrowed(line);
    }
    let units: Vec<u16> = line.encode_utf16().take(MAX_LINE_LENGTH).collect();
    std::borrow::Cow::Owned(format!("{}... (line truncated to {MAX_LINE_LENGTH} chars)", String::from_utf16_lossy(&units)))
}

/// `nthNewline`: the offset just past the `count`th newline.
fn nth_newline(bytes: &[u8], count: usize) -> Option<usize> {
    let mut found = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            found += 1;
            if found == count {
                return Some(index + 1);
            }
        }
    }
    None
}

fn newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| **b == b'\n').count()
}

/// `lineCount`: a trailing newline ends the last line instead of starting an empty one.
fn line_count(newlines: usize, last: Option<u8>) -> usize {
    newlines + usize::from(!matches!(last, None | Some(b'\n')))
}

struct TextPage {
    entries: Vec<String>,
    offset: usize,
    next: Option<usize>,
    consumed: usize,
}

/// `textPage`: the page of `bytes` (whose start is line 1), or `None` when more bytes are needed to settle it.
fn text_page(bytes: &[u8], eof: bool, offset: usize, limit: usize) -> Option<TextPage> {
    let decoded = decode(bytes);
    let mut split: Vec<&str> = decoded.split('\n').collect();
    if !eof || split.last() == Some(&"") {
        split.pop();
    }
    let available: Vec<&str> = split.into_iter().map(|line| line.strip_suffix('\r').unwrap_or(line)).collect();
    let mut entries: Vec<String> = Vec::new();
    let mut size = 0;
    let mut next = None;
    for (index, value) in available.iter().skip(offset - 1).enumerate() {
        let line = offset + index;
        if entries.len() >= limit || size >= MAX_READ_BYTES {
            next = Some(line);
            break;
        }
        let text = clamp_line(value);
        let line_size = text.len() + usize::from(!entries.is_empty());
        if size + line_size > MAX_READ_BYTES {
            next = Some(line);
            break;
        }
        entries.push(text.into_owned());
        size += line_size;
    }
    if next.is_none() && entries.len() >= limit && (!eof || offset - 1 + entries.len() < available.len()) {
        next = Some(offset + entries.len());
    }
    if !eof && next.is_none() {
        return None;
    }
    let consumed_lines = match next {
        None => available.len(),
        Some(next) => next - 1,
    };
    let consumed = if consumed_lines == 0 { 0 } else { nth_newline(bytes, consumed_lines).unwrap_or(bytes.len()) };
    Some(TextPage { entries, offset, next, consumed })
}

/// `makeTextPage`.
fn make_text_page(resource: &str, page: TextPage, binary_found: bool, lines: usize) -> Result<Content, ReadError> {
    if binary_found {
        return Err(binary(resource));
    }
    if page.entries.is_empty() && page.offset != 1 {
        let noun = if lines == 1 { "line" } else { "lines" };
        return Err(ReadError::Message(format!("Offset {} is out of range for this file ({lines} {noun})", page.offset)));
    }
    Ok(Content::TextPage { content: page.entries.join("\n"), offset: page.offset, next: page.next })
}

/// `list`: directories first, then files and links, each by `localeCompare`; special files are left out.
fn list(items: Vec<(String, FileType)>, page: Page, separator: &str) -> Content {
    let (offset, limit) = (page.offset(), page.limit());
    let mut visible: Vec<ListEntry> = items
        .into_iter()
        .filter(|(_, kind)| *kind != FileType::Other)
        .map(|(name, kind)| ListEntry { path: if kind == FileType::Directory { format!("{name}{separator}") } else { name }, kind })
        .collect();
    visible.sort_by(|a, b| match (a.kind == FileType::Directory, b.kind == FileType::Directory) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => crate::collate::compare(&a.path, &b.path),
    });
    let total = visible.len();
    let selected: Vec<ListEntry> = visible.into_iter().skip(offset - 1).take(limit).collect();
    let truncated = offset - 1 + selected.len() < total;
    let next = truncated.then(|| offset + selected.len());
    Content::ListPage { entries: selected, next }
}

/// `ReadToolFileSystem.read`.
pub fn read(files: &dyn Files, input: &str, resource: &str, page: Page) -> Result<Content, ReadError> {
    let virtual_files = resource.starts_with("skill://");
    let (size, first) = match files.read(input, Some((0, FIRST_CHUNK))) {
        Ok(read) => read,
        Err(FsError::WrongKind(FileType::Directory)) => {
            let entries = match files.list(input) {
                Ok(entries) => entries,
                Err(FsError::WrongKind(_)) => return Err(path_kind(resource, "a file or directory")),
                Err(error) => return Err(error.into()),
            };
            return Ok(list(entries, page, "/"));
        }
        Err(FsError::WrongKind(_)) => return Err(path_kind(resource, "a file or directory")),
        Err(error) => return Err(error.into()),
    };
    let _ = virtual_files;
    let mime_type = || crate::fs::mime_type(input).to_owned();
    let media = detect(&first);
    if MEDIA_MIMES.contains(&media) {
        if size > MAX_MEDIA_INGEST_BYTES {
            return Err(ReadError::Message(format!(
                "Media exceeds {MAX_MEDIA_INGEST_BYTES} byte ingestion limit: {resource}"
            )));
        }
        let whole = read_file(files, input, resource, None)?;
        use base64::Engine;
        return Ok(Content::File {
            content: base64::engine::general_purpose::STANDARD.encode(whole),
            base64: true,
            mime: media.to_owned(),
        });
    }
    let paged = size > MAX_READ_BYTES as u64 || page.offset.is_some() || page.limit.is_some();
    if !paged {
        if first.contains(&0) {
            return Err(binary(resource));
        }
        let decoded = decode(&first);
        let content = decoded.split('\n').map(clamp_line).collect::<Vec<_>>().join("\n");
        return Ok(Content::File { content, base64: false, mime: mime_type() });
    }
    let (offset, limit) = (page.offset(), page.limit());
    if first.len() as u64 >= size {
        let result = text_page(&first, true, offset, limit).expect("a complete page settles");
        let binary_found = first[..result.consumed].contains(&0);
        let lines = line_count(newlines(&first), first.last().copied());
        return make_text_page(resource, result, binary_found, lines);
    }
    // Chunks wholly before the requested line are dropped once counted.
    let mut kept: Vec<u8> = first;
    let mut dropped_newlines = 0usize;
    let mut dropped_nul = false;
    let mut read_bytes = kept.len() as u64;
    let mut lines = newlines(&kept);
    let mut last = kept.last().copied();
    let mut ended = false;
    loop {
        let eof = ended || read_bytes >= size;
        if lines + 1 >= offset || eof {
            let start = if offset - 1 == 0 {
                0
            } else {
                nth_newline(&kept, offset - 1 - dropped_newlines).unwrap_or(kept.len())
            };
            if let Some(result) = text_page(&kept[start..], eof, 1, limit) {
                let consumed = start + result.consumed;
                let binary_found = dropped_nul || kept[..consumed].contains(&0);
                let translated = TextPage {
                    entries: result.entries,
                    offset,
                    next: result.next.map(|next| offset + next - 1),
                    consumed: result.consumed,
                };
                return make_text_page(resource, translated, binary_found, line_count(lines, last));
            }
        } else {
            dropped_newlines += newlines(&kept);
            dropped_nul = dropped_nul || kept.contains(&0);
            kept.clear();
        }
        let next = read_file(files, input, resource, Some((read_bytes, FIRST_CHUNK)))?;
        if next.is_empty() {
            ended = true;
            continue;
        }
        read_bytes += next.len() as u64;
        lines += newlines(&next);
        last = next.last().copied();
        kept.extend_from_slice(&next);
    }
}

/// `readFile`: a file read, a non-file being `PathKindError("a file")`.
fn read_file(files: &dyn Files, input: &str, resource: &str, range: Option<(u64, usize)>) -> Result<Vec<u8>, ReadError> {
    match files.read(input, range) {
        Ok((_, bytes)) => Ok(bytes),
        Err(FsError::WrongKind(_)) => Err(path_kind(resource, "a file")),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(text: &str) -> Memory {
        Memory { files: vec![("/a.txt".into(), text.as_bytes().to_vec())] }
    }

    fn page(content: Result<Content, ReadError>) -> (String, usize, Option<usize>) {
        match content {
            Ok(Content::TextPage { content, offset, next }) => (content, offset, next),
            other => panic!("not a page: {other:?}"),
        }
    }

    #[test]
    fn pages() {
        let files = memory("one\r\ntwo\r\nthree\r\n");
        assert_eq!(page(read(&files, "/a.txt", "a.txt", Page { offset: Some(2), limit: None })), ("two\nthree".into(), 2, None));
        let files = memory("alpha\nbeta");
        match read(&files, "/a.txt", "a.txt", Page { offset: Some(3), limit: None }) {
            Err(ReadError::Message(message)) => assert_eq!(message, "Offset 3 is out of range for this file (2 lines)"),
            other => panic!("{other:?}"),
        }
        let files = memory("\n\n\n");
        assert_eq!(page(read(&files, "/a.txt", "a.txt", Page { offset: Some(2), limit: None })), ("\n".into(), 2, None));
    }

    #[test]
    fn chunks() {
        let text: String = (1..=80000).map(|i| format!("line {i:05}\n")).collect();
        let files = memory(&text);
        let (content, offset, next) = page(read(&files, "/a.txt", "a.txt", Page { offset: Some(40000), limit: Some(3) }));
        assert_eq!((content.as_str(), offset, next), ("line 40000\nline 40001\nline 40002", 40000, Some(40003)));
        let (content, _, next) = page(read(&files, "/a.txt", "a.txt", Page { offset: Some(79999), limit: None }));
        assert_eq!((content.as_str(), next), ("line 79999\nline 80000", None));
        match read(&files, "/a.txt", "a.txt", Page { offset: Some(90000), limit: None }) {
            Err(ReadError::Message(message)) => assert_eq!(message, "Offset 90000 is out of range for this file (80000 lines)"),
            other => panic!("{other:?}"),
        }
    }
}
