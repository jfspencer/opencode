//! Session media stored once in the deduplicated `blob` table (packages/core/src/session/blob.ts, session/stored.ts)
//! and assistant parts stored as `session_part` rows (`withParts` in session/history.ts).
//!
//! A stored message may name a blob as `media:<sha256>` in place of its bytes: a user file (`uri` instead of
//! `data`) or a tool result file. Reads restore the bytes ([`hydrate`]), or for clients that fetch media themselves
//! keep the refs ([`keep_refs`]); a ref whose blob is gone (or no longer referenced) reads as an unavailable
//! placeholder either way. `blob_ref` rows keep a blob alive for the messages that name it.
//!
//! With `experimental.media_blob` on where a session lives, media moves into `blob` as it is admitted (inbox user
//! files, [`externalize_payload`]) and as it is projected (user files and tool `data:` files, [`externalize`],
//! [`externalize_part`]). TS reads that flag and `experimental.part_rows` from the Config in scope while a projection
//! runs; here the publisher puts the session's [`Storage`] in scope ([`with_storage`]) and projections read it
//! ([`storage`]).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use base64::Engine;
use opencode_protocol::Json;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use sha2::Digest;

use crate::json::{self, Obj};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn is_media_ref(uri: &str) -> bool {
    uri.strip_prefix("media:").is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// `SessionStored.FileRef`: a user file whose bytes live in `blob`.
fn is_file_ref(file: &Json) -> bool {
    json::get_str(file, "uri").is_some_and(is_media_ref)
        && json::get_str(file, "mime").is_some()
        && matches!(json::get(file, "source"), Some(Json::Object(_)))
}

/// `SessionStored.ToolFileRef`: a tool result file whose bytes live in `blob`.
fn is_tool_file_ref(item: &Json) -> bool {
    json::get_str(item, "type") == Some("file")
        && json::get_str(item, "uri").is_some_and(is_media_ref)
        && json::get_str(item, "mime").is_some()
        && !matches!(json::get(item, "name"), Some(value) if !matches!(value, Json::String(_)))
}

fn ref_hash(uri: &str) -> &str {
    &uri["media:".len()..]
}

/// The tool result items of a settled tool part (`toolItems`).
fn tool_items(part: &Json) -> Option<&Vec<Json>> {
    if json::get_str(part, "type") != Some("tool") {
        return None;
    }
    let state = json::get(part, "state")?;
    if !matches!(json::get_str(state, "status"), Some("completed" | "error")) {
        return None;
    }
    match json::get(state, "content") {
        Some(Json::Array(items)) => Some(items),
        _ => None,
    }
}

fn tool_items_mut(part: &mut Json) -> Option<&mut Vec<Json>> {
    if json::get_str(part, "type") != Some("tool") {
        return None;
    }
    let state = json::get_mut(part, "state")?;
    if !matches!(json::get_str(state, "status"), Some("completed" | "error")) {
        return None;
    }
    match json::get_mut(state, "content") {
        Some(Json::Array(items)) => Some(items),
        _ => None,
    }
}

/// `collect`: the blob hashes a stored message (or payload) names.
pub fn hashes(data: &Json) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Some(Json::Array(files)) = json::get(data, "files") {
        for file in files.iter().filter(|f| is_file_ref(f)) {
            out.insert(ref_hash(json::get_str(file, "uri").unwrap_or_default()).to_owned());
        }
    }
    let parts: &[Json] = match json::get(data, "content") {
        Some(Json::Array(content)) => content,
        // One `session_part` row.
        _ => std::slice::from_ref(data),
    };
    for part in parts {
        for item in tool_items(part).into_iter().flatten().filter(|i| is_tool_file_ref(i)) {
            out.insert(ref_hash(json::get_str(item, "uri").unwrap_or_default()).to_owned());
        }
    }
    out
}

/// `unavailable`: what a ref whose blob is gone reads as.
fn unavailable(mime: &str, name: Option<&str>) -> String {
    match name {
        Some(name) => format!("[media unavailable: {name} ({mime})]"),
        None => format!("[media unavailable: {mime}]"),
    }
}

fn unavailable_file(file: &mut Json) {
    let text = unavailable(json::get_str(file, "mime").unwrap_or_default(), json::get_str(file, "name"));
    json::remove(file, "uri");
    json::set(file, "data", json::string(&B64.encode(text)));
    json::set(file, "mime", json::string("text/plain"));
}

fn unavailable_item(item: &mut Json) {
    let text = unavailable(json::get_str(item, "mime").unwrap_or_default(), json::get_str(item, "name"));
    let mut o = Obj::new();
    o.str("type", "text").str("text", &text);
    *item = o.done();
}

/// Rewrites the ref files of a message (`rewriteMessage`).
fn rewrite(message: &mut Json, file: &dyn Fn(&mut Json), item: &dyn Fn(&mut Json)) {
    if let Some(Json::Array(files)) = json::get_mut(message, "files") {
        if files.iter().any(is_file_ref) {
            files.iter_mut().filter(|f| is_file_ref(f)).for_each(file);
            return;
        }
    }
    if let Some(Json::Array(content)) = json::get_mut(message, "content") {
        for part in content.iter_mut() {
            if let Some(items) = tool_items_mut(part) {
                items.iter_mut().filter(|i| is_tool_file_ref(i)).for_each(item);
            }
        }
    }
}

fn placeholders(count: usize) -> String {
    (1..=count).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ")
}

/// `load`: the base64 bytes of the named blobs.
fn load(db: &Connection, hashes: &HashSet<String>) -> rusqlite::Result<HashMap<String, String>> {
    let list: Vec<&String> = hashes.iter().collect();
    let mut out = HashMap::new();
    for batch in list.chunks(500) {
        let sql = format!("SELECT hash, data FROM blob WHERE hash IN ({})", placeholders(batch.len()));
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(batch.iter()), |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))?;
        for row in rows {
            let (hash, data) = row?;
            out.insert(hash, B64.encode(data));
        }
    }
    Ok(out)
}

/// `SessionBlob.hydrate`: restores the inline media of the refs in stored messages (`{id, type, ...data}`).
pub fn hydrate(db: &Connection, messages: &mut [Json]) -> rusqlite::Result<()> {
    let mut wanted = HashSet::new();
    for message in messages.iter() {
        wanted.extend(hashes(message));
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let blobs = load(db, &wanted)?;
    for message in messages.iter_mut() {
        let file = |file: &mut Json| match blobs.get(ref_hash(json::get_str(file, "uri").unwrap_or_default())) {
            None => unavailable_file(file),
            Some(data) => {
                json::remove(file, "uri");
                json::set(file, "data", json::string(data));
            }
        };
        let item = |item: &mut Json| match blobs.get(ref_hash(json::get_str(item, "uri").unwrap_or_default())) {
            None => unavailable_item(item),
            Some(data) => {
                let mime = json::get_str(item, "mime").unwrap_or_default().to_owned();
                json::set(item, "uri", json::string(&format!("data:{mime};base64,{data}")));
            }
        };
        rewrite(message, &file, &item);
    }
    Ok(())
}

/// What a client that fetches bytes itself learns about a blob (`Meta`).
pub struct Meta {
    bytes: i64,
    width: Option<i64>,
    height: Option<i64>,
}

/// `SessionBlob.hydrateInbox`: [`hydrate`] for pending inbox payloads.
pub fn hydrate_payloads(db: &Connection, payloads: &mut [&mut Json]) -> rusqlite::Result<()> {
    let mut wanted = HashSet::new();
    for payload in payloads.iter() {
        wanted.extend(hashes(payload));
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let blobs = load(db, &wanted)?;
    for payload in payloads.iter_mut() {
        let file = |file: &mut Json| match blobs.get(ref_hash(json::get_str(file, "uri").unwrap_or_default())) {
            None => unavailable_file(file),
            Some(data) => {
                json::remove(file, "uri");
                json::set(file, "data", json::string(data));
            }
        };
        rewrite(payload, &file, &|_| {});
    }
    Ok(())
}

/// `SessionBlob.keepInboxRefs` and `describe`: [`keep_refs`] for pending inbox user payloads.
pub fn keep_payload_refs(db: &Connection, payloads: &mut [&mut Json]) -> rusqlite::Result<()> {
    let mut wanted = HashSet::new();
    for payload in payloads.iter() {
        wanted.extend(hashes(payload));
    }
    let meta = load_meta(db, &wanted)?;
    for payload in payloads.iter_mut() {
        let file = |file: &mut Json| {
            if !meta.contains_key(ref_hash(json::get_str(file, "uri").unwrap_or_default())) {
                unavailable_file(file);
            }
        };
        rewrite(payload, &file, &|_| {});
        describe(payload, &meta);
    }
    Ok(())
}

/// `describe`: each kept ref's blob size and dimensions on the files of a user message or payload.
fn describe(owner: &mut Json, meta: &HashMap<String, Meta>) {
    let Some(Json::Array(files)) = json::get_mut(owner, "files") else { return };
    for file in files.iter_mut() {
        let Some(blob) = json::get_str(file, "uri").and_then(|uri| uri.strip_prefix("media:")).and_then(|h| meta.get(h)) else {
            continue;
        };
        json::set(file, "bytes", Json::Number(blob.bytes as f64));
        if let Some(width) = blob.width {
            json::set(file, "width", Json::Number(width as f64));
        }
        if let Some(height) = blob.height {
            json::set(file, "height", Json::Number(height as f64));
        }
    }
}

/// `loadMeta`: the size and dimensions of the named blobs that something still references.
fn load_meta(db: &Connection, wanted: &HashSet<String>) -> rusqlite::Result<HashMap<String, Meta>> {
    let mut meta: HashMap<String, Meta> = HashMap::new();
    let list: Vec<&String> = wanted.iter().collect();
    for batch in list.chunks(500) {
        let sql = format!(
            "SELECT hash, bytes, width, height FROM blob WHERE hash IN ({}) AND EXISTS (SELECT 1 FROM blob_ref WHERE blob_ref.hash = blob.hash)",
            placeholders(batch.len())
        );
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(batch.iter()), |row| {
            Ok((row.get::<_, String>(0)?, Meta { bytes: row.get(1)?, width: row.get(2)?, height: row.get(3)? }))
        })?;
        for row in rows {
            let (hash, info) = row?;
            meta.insert(hash, info);
        }
    }
    Ok(meta)
}

/// `SessionBlob.keepRefs`: refs stay, except those whose blob is gone or no longer referenced; user files that keep
/// their ref are described with the blob's size and dimensions (`describe`).
pub fn keep_refs(db: &Connection, messages: &mut [Json]) -> rusqlite::Result<()> {
    let mut wanted = HashSet::new();
    for message in messages.iter() {
        wanted.extend(hashes(message));
    }
    let meta = load_meta(db, &wanted)?;
    for message in messages.iter_mut() {
        let known = |uri: &str| meta.contains_key(ref_hash(uri));
        let file = |file: &mut Json| {
            if !known(json::get_str(file, "uri").unwrap_or_default()) {
                unavailable_file(file);
            }
        };
        let item = |item: &mut Json| {
            if !known(json::get_str(item, "uri").unwrap_or_default()) {
                unavailable_item(item);
            }
        };
        rewrite(message, &file, &item);
        if json::get_str(message, "type") == Some("user") {
            describe(message, &meta);
        }
    }
    Ok(())
}

/// `SessionBlob.read`: one blob's type and bytes, while some message or inbox item still references it.
pub fn read(db: &Connection, hash: &str) -> rusqlite::Result<Option<(String, Vec<u8>)>> {
    db.query_row(
        "SELECT mime, data FROM blob WHERE hash = ?1 AND EXISTS (SELECT 1 FROM blob_ref WHERE blob_ref.hash = blob.hash)",
        params![hash],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// A blob taken out of a row: its hash, type and raw bytes.
pub struct Pending {
    pub hash: String,
    pub mime: String,
    pub data: Vec<u8>,
}

fn take(pending: &mut Vec<Pending>, raw: Vec<u8>, mime: &str) -> String {
    let hash: String = sha2::Sha256::digest(&raw).iter().map(|b| format!("{b:02x}")).collect();
    if !pending.iter().any(|p| p.hash == hash) {
        pending.push(Pending { hash: hash.clone(), mime: mime.to_owned(), data: raw });
    }
    format!("media:{hash}")
}

/// `/^data:[^,]*;base64,(.*)$/s`: the payload of a base64 data URI.
fn data_uri(uri: &str) -> Option<&str> {
    let rest = uri.strip_prefix("data:")?;
    let comma = rest.find(',')?;
    rest[..comma].ends_with(";base64").then(|| &rest[comma + 1..])
}

fn decode(data: &str) -> Vec<u8> {
    // Node's lenient base64: invalid characters are skipped.
    let clean: String = data.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_')).collect();
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    engine.decode(clean.replace('-', "+").replace('_', "/")).unwrap_or_default()
}

/// `fileRef`: an inline user file's bytes taken into `pending`, a ref in their place (a ref passes through).
fn file_ref(pending: &mut Vec<Pending>, file: &mut Json) {
    if is_file_ref(file) {
        return;
    }
    let raw = decode(json::get_str(file, "data").unwrap_or_default());
    let mime = json::get_str(file, "mime").unwrap_or_default().to_owned();
    let uri = take(pending, raw, &mime);
    json::remove(file, "data");
    json::set(file, "uri", json::string(&uri));
}

/// `toolPartRefs`: the `data:` files of a settled tool part taken into `pending`.
fn tool_part_refs(pending: &mut Vec<Pending>, part: &mut Json) {
    let Some(items) = tool_items_mut(part) else { return };
    for item in items.iter_mut().filter(|i| json::get_str(i, "type") == Some("file")) {
        let Some(payload) = json::get_str(item, "uri").and_then(data_uri).map(str::to_owned) else { continue };
        let mime = json::get_str(item, "mime").unwrap_or_default().to_owned();
        let uri = take(pending, decode(&payload), &mime);
        json::set(item, "uri", json::string(&uri));
    }
}

/// `SessionBlob.split`: moves a stored message's inline media into blobs, whatever the flag says. Returns whether
/// anything moved (the message is rewritten in place) and the blobs to save.
pub fn split(message: &mut Json) -> Vec<Pending> {
    let mut pending = Vec::new();
    let user_files = matches!(json::get(message, "files"), Some(Json::Array(files)) if files.iter().any(|f| !is_file_ref(f)));
    if user_files {
        if let Some(Json::Array(files)) = json::get_mut(message, "files") {
            files.iter_mut().for_each(|file| file_ref(&mut pending, file));
        }
        return pending;
    }
    if let Some(Json::Array(content)) = json::get_mut(message, "content") {
        content.iter_mut().for_each(|part| tool_part_refs(&mut pending, part));
    }
    pending
}

/// Where a session's projections store media and assistant parts: its location's `experimental.media_blob`
/// (`SessionBlob.enabled`) and `experimental.part_rows` (`partRowsEnabled`). Both are off without a config in scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Storage {
    pub media: bool,
    pub part_rows: bool,
}

thread_local! {
    static STORAGE: Cell<Storage> = const { Cell::new(Storage { media: false, part_rows: false }) };
}

/// Runs a projection with `storage` in scope (the single-threaded runtime projects on this thread).
pub fn with_storage<R>(storage: Storage, run: impl FnOnce() -> R) -> R {
    let previous = STORAGE.replace(storage);
    let out = run();
    STORAGE.set(previous);
    out
}

/// The storage in scope (see [`with_storage`]).
pub fn storage() -> Storage {
    STORAGE.get()
}

/// The storage a location's configuration selects.
pub fn storage_of(location: &crate::location::Location) -> Storage {
    let inventory = location.inventory();
    Storage {
        media: crate::flag::enabled(&inventory.entries, "media_blob"),
        part_rows: crate::flag::part_rows(&inventory.entries),
    }
}

/// The storage in scope while a session event projects: its location's (the event's, else the session's), when that
/// location is loaded. A location that is not loaded has no config in scope.
pub fn storage_for(app: &crate::app::App, db: &Connection, event: &crate::bus::Event) -> Storage {
    let location = event.location.clone().or_else(|| {
        let session = json::get_str(&event.data, "sessionID")?;
        db.query_row("SELECT directory, workspace_id FROM session_v2 WHERE id = ?1", params![session], |row| {
            Ok(crate::bus::LocationRef { directory: row.get(0)?, workspace_id: row.get(1)? })
        })
        .optional()
        .ok()
        .flatten()
    });
    location.and_then(|location| app.locations.cached(&location)).map(|located| storage_of(&located)).unwrap_or_default()
}

/// `SessionBlob.externalizePayload`: with media stored as blobs, a user message's or inbox payload's inline files
/// move into `blob` (saved here) and refs take their place. Returns whether anything moved.
pub fn externalize_payload(db: &Connection, payload: &mut Json) -> rusqlite::Result<bool> {
    if !storage().media {
        return Ok(false);
    }
    let Some(Json::Array(files)) = json::get_mut(payload, "files") else { return Ok(false) };
    let mut pending = Vec::new();
    files.iter_mut().for_each(|file| file_ref(&mut pending, file));
    save(db, &pending)?;
    Ok(!pending.is_empty())
}

/// `SessionBlob.externalize`: [`externalize_payload`] for a user message, and an assistant's tool `data:` files.
pub fn externalize(db: &Connection, message: &mut Json) -> rusqlite::Result<bool> {
    match json::get_str(message, "type") {
        Some("user") => externalize_payload(db, message),
        Some("assistant") if storage().media => {
            let mut pending = Vec::new();
            if let Some(Json::Array(content)) = json::get_mut(message, "content") {
                content.iter_mut().for_each(|part| tool_part_refs(&mut pending, part));
            }
            save(db, &pending)?;
            Ok(!pending.is_empty())
        }
        _ => Ok(false),
    }
}

/// `SessionBlob.externalizePart`: [`externalize`] for one assistant part.
pub fn externalize_part(db: &Connection, part: &mut Json) -> rusqlite::Result<bool> {
    if !storage().media {
        return Ok(false);
    }
    let mut pending = Vec::new();
    tool_part_refs(&mut pending, part);
    save(db, &pending)?;
    Ok(!pending.is_empty())
}

/// `release`: deletes the matching refs (`sql` returns their hashes), then every blob among theirs no ref names.
fn release(db: &Connection, sql: &str, args: impl rusqlite::Params) -> rusqlite::Result<()> {
    let released: Vec<String> = {
        let mut statement = db.prepare(sql)?;
        statement.query_map(args, |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    let mut seen = HashSet::new();
    for hash in released.into_iter().filter(|hash| seen.insert(hash.clone())) {
        db.execute(
            "DELETE FROM blob WHERE hash = ?1 AND NOT EXISTS (SELECT 1 FROM blob_ref WHERE blob_ref.hash = blob.hash)",
            params![hash],
        )?;
    }
    Ok(())
}

/// `SessionBlob.releaseMessage`: the blobs a removed message or cancelled inbox row referenced.
pub fn release_message(db: &Connection, session: &str, message: &str) -> rusqlite::Result<()> {
    release(db, "DELETE FROM blob_ref WHERE session_id = ?1 AND message_id = ?2 RETURNING hash", params![session, message])
}

/// `SessionBlob.save`: inserts blobs, keeping any that already exist.
pub fn save(db: &Connection, blobs: &[Pending]) -> rusqlite::Result<()> {
    let now = crate::id::now();
    for blob in blobs {
        db.execute(
            "INSERT INTO blob (hash, mime, bytes, data, time_created) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING",
            params![blob.hash, blob.mime, blob.data.len() as i64, blob.data, now],
        )?;
    }
    Ok(())
}

/// `SessionBlob.recordHashes`: the message references each of these blobs that exists.
pub fn record_hashes(db: &Connection, session: &str, message: &str, seq: i64, hashes: &HashSet<String>) -> rusqlite::Result<()> {
    for hash in hashes {
        let exists = db.query_row("SELECT 1 FROM blob WHERE hash = ?1", params![hash], |_| Ok(())).optional()?.is_some();
        if !exists {
            continue;
        }
        db.execute(
            "INSERT INTO blob_ref (session_id, message_id, hash, seq) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (session_id, message_id, hash) DO UPDATE SET seq = ?4",
            params![session, message, hash, seq],
        )?;
    }
    Ok(())
}

/// `SessionBlob.recordRefs`: the message references every blob its stored data names.
pub fn record_refs(db: &Connection, session: &str, message: &str, seq: i64, data: &Json) -> rusqlite::Result<()> {
    record_hashes(db, session, message, seq, &hashes(data))
}

/// `SessionBlob.releaseSession`: forgets a deleted session's refs, then every blob no ref names anymore.
pub fn release_session(db: &Connection, session: &str) -> rusqlite::Result<()> {
    release(db, "DELETE FROM blob_ref WHERE session_id = ?1 RETURNING hash", params![session])
}

/// `withParts`: each assistant message's `session_part` rows into its content, in ordinal order, ahead of any
/// inline content an older build left.
pub fn with_parts(db: &Connection, messages: &mut [Json]) -> rusqlite::Result<()> {
    let ids: Vec<String> = messages
        .iter()
        .filter(|m| json::get_str(m, "type") == Some("assistant"))
        .filter_map(|m| json::get_str(m, "id").map(str::to_owned))
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let mut parts: HashMap<String, Vec<Json>> = HashMap::new();
    for batch in ids.chunks(500) {
        let sql = format!(
            "SELECT message_id, type, data FROM session_part WHERE message_id IN ({}) ORDER BY message_id ASC, ordinal ASC",
            placeholders(batch.len())
        );
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(batch.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?;
        for row in rows {
            let (message, ty, data) = row?;
            let mut part = json::parse(&data).unwrap_or(Json::Null);
            if json::get(&part, "type").is_none() {
                json::set(&mut part, "type", json::string(&ty));
            }
            parts.entry(message).or_default().push(crate::session::transfer::legacy_part(part));
        }
    }
    if parts.is_empty() {
        return Ok(());
    }
    for message in messages.iter_mut() {
        let Some(found) = json::get_str(message, "id").and_then(|id| parts.remove(id)) else { continue };
        let mut content = found;
        if let Some(Json::Array(inline)) = json::get(message, "content") {
            content.extend(inline.iter().cloned());
        }
        json::set(message, "content", Json::Array(content));
    }
    Ok(())
}

/// Whether media is stored as blobs where a session lives (`SessionBlob.enabledFor`): its location's
/// `experimental.media_blob`; off when the location cannot boot.
pub async fn enabled_for(app: &crate::app::App, location: &crate::bus::LocationRef) -> bool {
    match app.locations.get(app, location).await {
        Ok(located) => crate::flag::enabled(&located.inventory().entries, "media_blob"),
        Err(_) => false,
    }
}

/// [`hydrate`] or [`keep_refs`] for a read, after [`with_parts`].
pub fn prepare(db: &Connection, messages: &mut [Json], refs: bool) -> rusqlite::Result<()> {
    with_parts(db, messages)?;
    if refs { keep_refs(db, messages) } else { hydrate(db, messages) }
}

/// `wantsMediaRefs`: the client opted into `media:` refs (`x-opencode-capabilities: experimental-media-uri`).
pub fn wants_refs(header: Option<&str>) -> bool {
    header.is_some_and(|value| value.split(',').any(|token| token.trim().eq_ignore_ascii_case("experimental-media-uri")))
}
