//! Writing a file for a tool (packages/core/src/file-write.ts, file-mutation.ts, @opencode/util/bom): the target
//! resolved and its external directory approved, the edit approved with the change as its preview, the text written
//! keeping an existing UTF-8 BOM (at most one), then formatted ([`crate::formatter`]) with the BOM synced back.
//! Mutations of one file are serialized (`FileMutation.withLock`, process-wide, by resolved path).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::file_access::{self, Kind, Target};
use super::file_diff::{FileDiff, file_diff};
use super::{Context, Failure};
use crate::permission::Ask;

const BOM: char = '\u{feff}';

/// `Bom.split`: the text without its leading BOMs, and whether it had any.
pub fn split(text: &str) -> (bool, &str) {
    let stripped = text.trim_start_matches(BOM);
    (stripped.len() != text.len(), stripped)
}

/// `Bom.join`.
pub fn join(text: &str, bom: bool) -> String {
    let (_, stripped) = split(text);
    if bom { format!("{BOM}{stripped}") } else { stripped.to_owned() }
}

/// `Bom.decodeBytes`: UTF-8 (invalid sequences replaced, a BOM kept as text), then split.
pub fn decode_bytes(bytes: &[u8]) -> (bool, String) {
    let decoded = String::from_utf8_lossy(bytes);
    let (bom, text) = split(&decoded);
    (bom, text.to_owned())
}

/// Why a file could not be read as text (`Environment.NotFound | WrongKind | Failed`).
#[derive(Debug)]
pub enum ReadError {
    NotFound,
    /// A directory or another non-file.
    WrongKind(&'static str),
    Failed,
}

/// `FileMutation.readText`.
pub fn read_text(path: &str) -> Result<(bool, String), ReadError> {
    let info = std::fs::metadata(path).map_err(error)?;
    if !info.is_file() {
        return Err(ReadError::WrongKind(if info.is_dir() { "directory" } else { "other" }));
    }
    let bytes = std::fs::read(path).map_err(error)?;
    Ok(decode_bytes(&bytes))
}

fn error(error: std::io::Error) -> ReadError {
    if error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(20) { ReadError::NotFound } else { ReadError::Failed }
}

/// `FileMutation.readOptionalText`: `None` for a missing file.
pub fn read_optional_text(path: &str) -> Result<Option<(bool, String)>, ReadError> {
    match read_text(path) {
        Ok(text) => Ok(Some(text)),
        Err(ReadError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

/// `files.write`: the parent directories created.
fn write_bytes(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

/// `FileMutation.syncTextBom`: the file's BOM set as wanted (rewritten only when it changes); its text.
pub fn sync_text_bom(path: &str, bom: bool) -> Result<String, ReadError> {
    let bytes = std::fs::read(path).map_err(error)?;
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    let (_, text) = split(&decoded);
    let canonical = if bom { format!("{BOM}{text}") } else { text.to_owned() };
    if decoded != canonical {
        write_bytes(path, canonical.as_bytes()).map_err(|_| ReadError::Failed)?;
    }
    Ok(text.to_owned())
}

/// `FileMutation.TextWriteResult`, with the formatted text (`FileWrite.Written`).
#[derive(Debug)]
pub struct Written {
    pub existed: bool,
    pub bom: bool,
    pub formatted: Option<String>,
}

/// `FileMutation.writeTextPreservingBom`.
fn write_text_preserving_bom(target: &Target, content: &str) -> Result<Written, ReadError> {
    let (next_bom, text) = split(content);
    let current = match std::fs::metadata(&target.absolute) {
        Ok(info) if !info.is_file() => return Err(ReadError::WrongKind(if info.is_dir() { "directory" } else { "other" })),
        Ok(_) => {
            let mut head = [0u8; 3];
            let file = std::fs::File::open(&target.absolute).map_err(error)?;
            use std::io::Read;
            let count = file.take(3).read(&mut head).map_err(error)?;
            Some(head[..count].to_vec())
        }
        Err(e) => match error(e) {
            ReadError::NotFound => None,
            other => return Err(other),
        },
    };
    let bom = current.as_ref().is_some_and(|bytes| bytes.starts_with(&[0xef, 0xbb, 0xbf])) || next_bom;
    write_bytes(&target.absolute, join(text, bom).as_bytes()).map_err(|_| ReadError::Failed)?;
    Ok(Written { existed: current.is_some(), bom, formatted: None })
}

/// `FileWrite.target`: resolved as a file, its external directory approved.
pub async fn target(context: &Context, tool: &str, file: &str) -> Result<Target, Failure> {
    let target = file_access::resolve(&context.location, file, Some(Kind::File));
    file_access::authorize_external(context, tool, &[&target], None).await?;
    Ok(target)
}

/// `FileWrite.authorize`: the edit approved, showing the change when it is known.
pub async fn authorize(context: &Context, tool: &str, target: &Target, preview: Option<&FileDiff>) -> Result<(), Failure> {
    let metadata = preview.map(|diff| {
        let mut o = crate::json::Obj::new();
        o.set("files", opencode_protocol::Json::Array(vec![diff.to_json()]));
        o.done()
    });
    let ask = Ask { action: "edit".into(), resources: vec![target.resource.clone()], save: Some(vec!["*".into()]), metadata };
    file_access::assert(context, tool, ask).await
}

/// `FileWrite.commit`: approved text written keeping an existing BOM, then formatted.
pub async fn commit(context: &Context, target: &Target, content: &str) -> Result<Written, ReadError> {
    let written = {
        let (target, content) = (target.clone(), content.to_owned());
        tokio::task::spawn_blocking(move || write_text_preserving_bom(&target, &content)).await.unwrap_or(Err(ReadError::Failed))?
    };
    if !crate::formatter::file(&context.location, &target.absolute).await {
        return Ok(written);
    }
    let formatted = sync_text_bom(&target.absolute, written.bom)?;
    Ok(Written { formatted: Some(formatted), ..written })
}

/// `FileWrite.write`: resolve, approve, write and format a whole file.
pub async fn write(context: &Context, file: &str, content: &str) -> Result<(Target, Written), WriteFailure> {
    let resolved = target(context, "write", file).await.map_err(WriteFailure::Tool)?;
    let current = read_optional_text(&resolved.absolute).map_err(|_| WriteFailure::Read)?;
    let (_, after) = split(content);
    let preview = file_diff(
        &resolved.resource,
        current.as_ref().map(|(_, text)| text.as_str()).unwrap_or(""),
        after,
        if current.is_some() { "modified" } else { "added" },
    );
    authorize(context, "write", &resolved, Some(&preview)).await.map_err(WriteFailure::Tool)?;
    let written = commit(context, &resolved, content).await.map_err(|_| WriteFailure::Read)?;
    Ok((resolved, written))
}

/// A failed write: a permission (or approval) failure, or the file system's.
pub enum WriteFailure {
    Tool(Failure),
    Read,
}

type Locks = Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>;

fn locks() -> &'static Locks {
    static LOCKS: std::sync::OnceLock<Locks> = std::sync::OnceLock::new();
    LOCKS.get_or_init(Default::default)
}

/// The transaction locks a mutation holds (`FileMutation.withLock`); released when dropped.
pub struct Lock {
    keys: Vec<String>,
    guards: Vec<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for Lock {
    fn drop(&mut self) {
        self.guards.clear();
        let mut map = locks().lock().unwrap();
        for key in &self.keys {
            if map.get(key).is_some_and(|mutex| Arc::strong_count(mutex) == 1) {
                map.remove(key);
            }
        }
    }
}

/// `FileMutation.withLock`: the process-wide locks of the resolved targets, taken in sorted order.
pub async fn lock(targets: &[String]) -> Lock {
    let mut keys: Vec<String> = targets.iter().map(|target| file_access::fs_resolve(target)).collect();
    keys.sort();
    keys.dedup();
    let mut lock = Lock { keys: Vec::new(), guards: Vec::with_capacity(keys.len()) };
    for key in keys {
        let mutex = locks().lock().unwrap().entry(key.clone()).or_default().clone();
        lock.keys.push(key);
        lock.guards.push(mutex.lock_owned().await);
    }
    lock
}
