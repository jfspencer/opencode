//! Arborist's `reify` of an ideal tree into a fresh staging directory, and the registry the tree's manifests come
//! from: pacote 21.5.1's registry and remote-tarball manifests (full metadata, `accept: application/json`; a
//! packument kept in memory only when its response had a length, as arborist's `PackumentCache` does), npm-registry-
//! fetch's credentials, tarballs checked against their integrity and unpacked as pacote's extractor does (regular
//! files only, the first path component stripped, `.gitignore` renamed `.npmignore`, modes `(mode | 0o666) & ~umask`),
//! bin-links (each package's bins linked into its own `node_modules/.bin`, the target made executable and a CRLF
//! shebang fixed), failed optional dependencies dropped, then the root `package.json`, `package-lock.json` and the
//! hidden `node_modules/.package-lock.json` written as arborist writes them (json-stringify-nice key order).

use std::collections::HashMap;
use std::future::Future;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::pin::Pin;

use base64::Engine;
use indexmap::IndexMap;
use opencode_protocol::Json;
use sha2::Digest;

use super::config::Flat;
use super::pick::{self, truthy};
use super::spec::{Kind, Spec};
use super::tree::{self, Id, Source, Tree, locale_compare, relpath};

fn get<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    match value {
        Json::Object(map) => map.get(key),
        _ => None,
    }
}

pub struct Fetched {

    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A GET through the server's HTTP client, following redirects (make-fetch-happen follows up to 20).
pub async fn fetch(url: &str, accept: &str, authorization: Option<String>) -> Result<Fetched, String> {
    let mut url = url.to_owned();
    for _ in 0..=20 {
        let mut headers = vec![("accept".to_owned(), accept.to_owned())];
        if let Some(auth) = &authorization {
            headers.push(("authorization".to_owned(), auth.clone()));
        }
        let response = crate::llm::http::send("GET", &url, &headers, String::new()).await.map_err(|f| f.message)?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            if let Some(location) = response.headers.iter().find(|(k, _)| k == "location").map(|(_, v)| v.clone()) {
                url = url::Url::parse(&url).and_then(|base| base.join(&location)).map(|u| u.to_string()).unwrap_or(location);
                continue;
            }
        }
        let status = response.status;
        let headers = response.headers.clone();
        let mut body = response.body;
        let mut out = Vec::new();
        while let Some(chunk) = body.next().await {
            out.extend_from_slice(&chunk.map_err(|f| f.message)?);
        }
        if status >= 400 {
            return Err(format!("{status} {url}"));
        }
        return Ok(Fetched { headers, body: out });
    }
    Err(format!("too many redirects: {url}"))
}

/// The manifests of a tree, from the configured registry.
pub struct Registry {
    pub flat: Flat,
    packuments: std::sync::Mutex<HashMap<String, Json>>,
}

impl Registry {
    pub fn new(flat: Flat) -> Registry {
        Registry { flat, packuments: std::sync::Mutex::new(HashMap::new()) }
    }

    async fn packument(&self, name: &str) -> Result<Json, String> {
        let registry = self.flat.registry_for(name);
        let url = format!("{}/{}", registry.trim_end_matches('/'), name.replacen('/', "%2f", 1));
        if let Some(cached) = self.packuments.lock().unwrap().get(&url).cloned() {
            return Ok(cached);
        }
        let auth = self.flat.authorization(&url, Some(name));
        let response = fetch(&url, "application/json", auth).await?;
        let packument = crate::json::parse(&String::from_utf8_lossy(&response.body)).ok_or("invalid json response body")?;
        // PackumentCache sizes an entry by its response's content-length; without one it is not kept.
        if response.headers.iter().any(|(k, _)| k == "content-length") {
            self.packuments.lock().unwrap().insert(url, packument.clone());
        }
        Ok(packument)
    }

    async fn registry_manifest(&self, spec: &Spec) -> Result<Json, String> {
        let name = spec.name.clone().unwrap_or_default();
        let packument = self.packument(&name).await?;
        let wanted = spec.fetch_spec.clone().unwrap_or_default();
        let options = pick::PickOptions { before: self.flat.before, ..Default::default() };
        let mut manifest = pick::pick(&packument, &wanted, &options)?;
        pick::normalize_manifest(&mut manifest);
        let version = get(&manifest, "version").map(pick::js_string).unwrap_or_default();
        let time = get(&packument, "time").and_then(|t| get(t, &version)).cloned();
        let Json::Object(map) = &mut manifest else { return Err("invalid manifest".into()) };
        if let Some(time) = time.filter(|t| truthy(Some(t))) {
            map.insert("_time".into(), time);
        }
        if let Some(dist) = map.get("dist").cloned().filter(|d| truthy(Some(d))) {
            if let Some(Json::String(tarball)) = get(&dist, "tarball") {
                map.insert("_resolved".into(), Json::String(tarball.clone()));
            }
            map.insert("_from".into(), Json::String(spec.raw.clone()));
            let integrity = match (get(&dist, "integrity"), get(&dist, "shasum")) {
                (Some(Json::String(sri)), _) if !sri.is_empty() => Some(sri.clone()),
                (_, Some(Json::String(hex))) if !hex.is_empty() => hex_sri(hex),
                _ => None,
            };
            if let Some(integrity) = integrity {
                map.insert("_integrity".into(), Json::String(integrity));
            }
        }
        Ok(manifest)
    }

    async fn remote_manifest(&self, spec: &Spec) -> Result<Json, String> {
        let url = spec.fetch_spec.clone().unwrap_or_default();
        let auth = self.flat.authorization(&url, spec.name.as_deref());
        let tarball = fetch(&url, "*/*", auth).await?.body;
        let files = read_tarball(&tarball)?;
        let package = files.iter().find(|(path, _)| path == "package.json").map(|(_, data)| data.clone()).ok_or("no package.json")?;
        let mut manifest = crate::json::parse(&String::from_utf8_lossy(&package)).ok_or("invalid package.json")?;
        pick::normalize_manifest(&mut manifest);
        let Json::Object(map) = &mut manifest else { return Err("invalid package.json".into()) };
        // a remote tarball's manifest carries no integrity (nothing to check it against)
        map.insert("_resolved".into(), Json::String(url));
        map.insert("_from".into(), Json::String(spec.raw.clone()));
        Ok(manifest)
    }
}

impl Source for Registry {
    fn manifest<'a>(&'a self, spec: &'a Spec) -> Pin<Box<dyn Future<Output = Result<Json, String>> + Send + 'a>> {
        Box::pin(async move {
            match &spec.kind {
                Kind::Version | Kind::Range | Kind::Tag => self.registry_manifest(spec).await,
                Kind::Alias(sub) => self.registry_manifest(sub).await,
                Kind::Remote => self.remote_manifest(spec).await,
                Kind::Git { .. } => Err("a Git dependency".into()),
                Kind::File | Kind::Directory => Err("a file dependency".into()),
            }
        })
    }
}

/// `ssri.fromHex(shasum, 'sha1')`.
fn hex_sri(hex: &str) -> Option<String> {
    let bytes: Option<Vec<u8>> = (0..hex.len()).step_by(2).map(|i| hex.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok())).collect();
    Some(format!("sha1-{}", base64::engine::general_purpose::STANDARD.encode(bytes?)))
}

/// ssri's check of data against an integrity string: the strongest algorithm it names must match.
fn integrity_ok(integrity: &str, data: &[u8]) -> bool {
    const ORDER: [&str; 4] = ["sha1", "sha256", "sha384", "sha512"];
    let mut entries: Vec<(&str, &str)> = Vec::new();
    for item in integrity.split_whitespace() {
        let Some((algorithm, rest)) = item.split_once('-') else { continue };
        let digest = rest.split('?').next().unwrap_or("");
        if ORDER.contains(&algorithm) {
            entries.push((algorithm, digest));
        }
    }
    let Some(best) = entries.iter().map(|(a, _)| *a).max_by_key(|a| ORDER.iter().position(|o| o == a)) else { return true };
    let engine = base64::engine::general_purpose::STANDARD;
    let actual = match best {
        "sha1" => engine.encode(sha1::Sha1::digest(data)),
        "sha256" => engine.encode(sha2::Sha256::digest(data)),
        "sha384" => engine.encode(sha2::Sha384::digest(data)),
        _ => engine.encode(sha2::Sha512::digest(data)),
    };
    entries.iter().any(|(a, d)| *a == best && *d == actual)
}

/// A gzipped (or plain) tarball's regular files, the first path component stripped, in archive order.
fn read_tarball(tarball: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    Ok(tar_entries(tarball)?.into_iter().filter(|e| e.file).map(|e| (e.path, e.data)).collect())
}

struct TarEntry {
    path: String,
    mode: u32,
    file: bool,
    data: Vec<u8>,
}

fn tar_entries(tarball: &[u8]) -> Result<Vec<TarEntry>, String> {
    use std::io::Read;
    let data = if tarball.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(tarball).read_to_end(&mut out).map_err(|e| e.to_string())?;
        out
    } else {
        tarball.to_vec()
    };
    let octal = |field: &[u8]| -> u64 {
        let text = String::from_utf8_lossy(field);
        u64::from_str_radix(text.trim_matches(|c: char| c == '\0' || c == ' '), 8).unwrap_or(0)
    };
    let text = |field: &[u8]| String::from_utf8_lossy(field.split(|b| *b == 0).next().unwrap_or(&[])).into_owned();
    let mut out = Vec::new();
    let mut at = 0;
    let mut long_name: Option<String> = None;
    while at + 512 <= data.len() {
        let header = &data[at..at + 512];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let size = octal(&header[124..136]) as usize;
        let kind = header[156];
        let body = data.get(at + 512..at + 512 + size).ok_or("TAR_BAD_ARCHIVE: Unrecognized archive format")?;
        at += 512 + size.div_ceil(512) * 512;
        let mut name = text(&header[0..100]);
        if &header[257..262] == b"ustar" {
            let prefix = text(&header[345..500]);
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        match kind {
            b'x' => {
                for record in String::from_utf8_lossy(body).split('\n') {
                    if let Some((_, rest)) = record.split_once(' ') {
                        if let Some(path) = rest.strip_prefix("path=") {
                            long_name = Some(path.to_owned());
                        }
                    }
                }
                continue;
            }
            b'L' => {
                long_name = Some(text(body));
                continue;
            }
            b'g' => continue,
            _ => {}
        }
        let name = long_name.take().unwrap_or(name);
        // `strip: 1`, absolute paths made relative, `..` refused.
        let name = name.trim_start_matches('/');
        let Some((_, relative)) = name.split_once('/') else { continue };
        let relative = relative.trim_start_matches('/');
        if relative.is_empty() || relative.split('/').any(|part| part == "..") {
            continue;
        }
        let file = matches!(kind, b'0' | 0 | b'7');
        out.push(TarEntry { path: relative.trim_end_matches('/').to_owned(), mode: octal(&header[100..108]) as u32, file, data: body.to_vec() });
    }
    Ok(out)
}

/// pacote's extraction into `destination` (fresh): regular files only, `(mode | 0o666) & ~umask | 0o600`, created
/// under the process umask; `.gitignore` becomes `.npmignore` unless a `.npmignore` came first.
fn extract(tarball: &[u8], destination: &Path, umask: u32) -> Result<(), String> {
    std::fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    let mut saw_ignores: Vec<String> = Vec::new();
    for entry in tar_entries(tarball)? {
        if !entry.file {
            continue;
        }
        let mut path = entry.path.clone();
        let base = path.rsplit('/').next().unwrap_or(&path).to_owned();
        if base == ".npmignore" {
            saw_ignores.push(path.clone());
        } else if base == ".gitignore" {
            let renamed = format!("{}.npmignore", &path[..path.len() - ".gitignore".len()]);
            if saw_ignores.contains(&renamed) {
                continue;
            }
            path = renamed;
        }
        let target = destination.join(&path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mode = ((entry.mode | 0o666) & !umask) | 0o600;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode & 0o7777)
            .open(&target)
            .map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut file, &entry.data).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn process_umask() -> u32 {
    static UMASK: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *UMASK.get_or_init(|| {
        // SAFETY: umask(2) only swaps the process's mask; it is put back at once.
        unsafe {
            let old = libc::umask(0o022);
            libc::umask(old);
            old as u32
        }
    })
}

/// `#registryResolved(resolved)`: a tarball on the replaced host is fetched from the configured registry.
fn registry_resolved(resolved: &str, flat: &Flat) -> String {
    let replace = if flat.replace_registry_host.is_empty() || flat.replace_registry_host == "npmjs" {
        "registry.npmjs.org".to_owned()
    } else {
        flat.replace_registry_host.clone()
    };
    let Ok(mut url) = url::Url::parse(resolved) else { return resolved.to_owned() };
    if url.host_str() != Some(replace.as_str()) && replace != "always" {
        return resolved.to_owned();
    }
    let Ok(registry) = url::Url::parse(&flat.arborist_registry()) else { return resolved.to_owned() };
    let _ = url.set_scheme(registry.scheme());
    let _ = url.set_host(registry.host_str());
    let _ = url.set_port(registry.port());
    let registry_path = registry.path().trim_end_matches('/').to_owned();
    if !registry_path.is_empty() && registry_path != "/" {
        let path = url.path().to_owned();
        if path != registry_path && !path.starts_with(&format!("{registry_path}/")) {
            url.set_path(&format!("{registry_path}{path}"));
        }
    }
    url.to_string()
}

/// The nodes the reify adds: the root's tree without the root, the inert nodes and what is under them.
fn added(tree: &Tree, root: Id) -> Vec<Id> {
    let mut out = Vec::new();
    let mut stack: Vec<Id> = tree.nodes[root].children.values().copied().collect();
    while let Some(node) = stack.pop() {
        if tree.nodes[node].inert {
            continue;
        }
        out.push(node);
        stack.extend(tree.nodes[node].children.values().copied());
    }
    out
}

/// Unpacks the tree under its root and links the bins. Failed optional nodes come back inert.
pub async fn reify(tree: &mut Tree, root: Id, flat: &Flat) -> Result<(), String> {
    let nodes = added(tree, root);
    let umask = flat.umask;
    let jobs = nodes.iter().map(|&node| {
        let tree = &*tree;
        async move {
            let n = &tree.nodes[node];
            let path = n.path.clone();
            if let Some(target) = n.target {
                let dir = crate::file_access::dirname(&path);
                let rel = relpath(&dir, &tree.nodes[target].path);
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                return std::os::unix::fs::symlink(rel, &path).map_err(|e| e.to_string());
            }
            let resolved = n.resolved.clone().ok_or("invalid or damaged lockfile detected")?;
            let url = registry_resolved(&resolved, flat);
            let name = tree.package_name(node);
            let auth = flat.authorization(&url, name.as_deref());
            let tarball = fetch(&url, "*/*", auth).await?.body;
            if let Some(integrity) = &n.integrity {
                if !integrity_ok(integrity, &tarball) {
                    return Err(format!("EINTEGRITY: {url}"));
                }
            }
            let destination = std::path::PathBuf::from(&path);
            tokio::task::spawn_blocking(move || extract(&tarball, &destination, umask)).await.map_err(|e| e.to_string())?
        }
    });
    let results = join_all(jobs.collect()).await;
    let mut failed = Vec::new();
    for (node, result) in nodes.iter().zip(results) {
        if let Err(error) = result {
            if !tree.nodes[*node].optional {
                return Err(error);
            }
            failed.push(*node);
        }
    }
    let mut trashed: Vec<Id> = Vec::new();
    for node in failed {
        for member in tree.optional_set(node) {
            tree.nodes[member].inert = true;
            let _ = std::fs::remove_dir_all(&tree.nodes[member].path);
            trashed.push(member);
        }
    }
    // bin-links, in depth then path order (the first package to claim a bin keeps it)
    let mut bins: Vec<Id> = added(tree, root).into_iter().filter(|n| !trashed.contains(n)).collect();
    bins.sort_by(|a, b| tree.depth(*a).cmp(&tree.depth(*b)).then_with(|| locale_compare(&tree.nodes[*a].path, &tree.nodes[*b].path)));
    for node in bins {
        link_bins(&tree.nodes[node].package, &tree.nodes[node].path)?;
    }
    Ok(())
}

async fn join_all<F: Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut pinned: Vec<Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut out: Vec<Option<F::Output>> = (0..pinned.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (index, future) in pinned.iter_mut().enumerate() {
            if out[index].is_some() {
                continue;
            }
            match future.as_mut().poll(cx) {
                std::task::Poll::Ready(value) => out[index] = Some(value),
                std::task::Poll::Pending => pending = true,
            }
        }
        if pending { std::task::Poll::Pending } else { std::task::Poll::Ready(()) }
    })
    .await;
    out.into_iter().map(|o| o.unwrap()).collect()
}

/// bin-links' module-wide `seen`: a bin path linked once in this process is never linked again.
fn seen_bins() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    SEEN.get_or_init(Default::default)
}

/// `linkBins({ path, pkg })` with `linkGently` and `fixBin`.
fn link_bins(package: &Json, path: &str) -> Result<(), String> {
    let mut package = package.clone();
    pick::normalize_bin_npm(&mut package);
    let Some(Json::Object(bins)) = get(&package, "bin").cloned() else { return Ok(()) };
    // getNodeModules(path) + '/.bin'
    let scope_or_nm = crate::file_access::dirname(path);
    let nm = if crate::text::basename(&scope_or_nm) == "node_modules" { scope_or_nm } else { crate::file_access::dirname(&scope_or_nm) };
    let target = format!("{nm}/.bin");
    for (key, value) in bins {
        let Json::String(value) = value else { continue };
        let to = crate::project::normalize(&format!("{target}/{key}"));
        let abs_from = crate::project::normalize(&format!("{path}/{value}"));
        let from = relpath(&crate::file_access::dirname(&to), &abs_from);
        if !seen_bins().lock().unwrap().insert(to.clone()) {
            continue;
        }
        if std::fs::symlink_metadata(&abs_from).is_err() {
            continue;
        }
        match std::fs::symlink_metadata(&to) {
            Ok(meta) if !meta.file_type().is_symlink() => continue,
            Ok(_) => {
                let current = std::fs::read_link(&to).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                if current == from {
                    continue;
                }
                let resolved = crate::project::normalize(&format!("{}/{current}", crate::file_access::dirname(&to)));
                if resolved == path || resolved.starts_with(&format!("{path}/")) {
                    let _ = std::fs::remove_file(&to);
                } else {
                    continue;
                }
            }
            Err(_) => std::fs::create_dir_all(crate::file_access::dirname(&to)).map_err(|e| e.to_string())?,
        }
        std::os::unix::fs::symlink(&from, &to).map_err(|e| e.to_string())?;
        fix_bin(&abs_from)?;
    }
    Ok(())
}

/// `fixBin`: mode `0o777 & ~umask`, and a `#!…\r\n` first line made `\n`.
fn fix_bin(file: &str) -> Result<(), String> {
    let mode = 0o777 & !process_umask();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
    let Ok(bytes) = std::fs::read(file) else { return Ok(()) };
    let head = &bytes[..bytes.len().min(2048)];
    if head.starts_with(b"#!") {
        let first = head.iter().position(|b| *b == b'\n');
        if let Some(nl) = first {
            if nl > 2 && head[nl - 1] == b'\r' && !head[2..nl - 1].contains(&b'\n') {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let fixed = format!("{}{}", &text[..nl - 1], &text[nl..]);
                std::fs::write(file, fixed).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

/// `JSON.stringify(value, null, 2)`.
pub fn pretty(value: &Json) -> String {
    fn write(out: &mut String, value: &Json, indent: usize) {
        match value {
            Json::Array(items) if !items.is_empty() => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    out.push_str(&" ".repeat(indent + 2));
                    write(out, item, indent + 2);
                }
                out.push('\n');
                out.push_str(&" ".repeat(indent));
                out.push(']');
            }
            Json::Object(map) if !map.is_empty() => {
                out.push('{');
                for (i, (key, item)) in map.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    out.push_str(&" ".repeat(indent + 2));
                    out.push_str(&crate::json::text(&Json::String(key.clone())));
                    out.push_str(": ");
                    write(out, item, indent + 2);
                }
                out.push('\n');
                out.push_str(&" ".repeat(indent));
                out.push('}');
            }
            other => out.push_str(&crate::json::text(other)),
        }
    }
    let mut out = String::new();
    write(&mut out, value, 0);
    out
}

const SW_KEY_ORDER: [&str; 8] = ["name", "version", "lockfileVersion", "resolved", "integrity", "requires", "packages", "dependencies"];

/// json-stringify-nice's key order, applied to every object: scalars before objects, then the preferred keys in
/// order, then `localeCompare`.
fn nice(value: &Json) -> Json {
    match value {
        Json::Object(map) => {
            let mut entries: Vec<(String, Json)> = map.iter().map(|(k, v)| (k.clone(), nice(v))).collect();
            let is_obj = |v: &Json| matches!(v, Json::Object(_));
            let rank = |k: &str| SW_KEY_ORDER.iter().position(|p| *p == k);
            entries.sort_by(|(ak, av), (bk, bv)| {
                if is_obj(av) != is_obj(bv) {
                    return if is_obj(av) { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less };
                }
                match (rank(ak), rank(bk)) {
                    (Some(a), Some(b)) => a.cmp(&b),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => locale_compare(ak, bk),
                }
            });
            Json::Object(entries.into_iter().collect())
        }
        Json::Array(items) => Json::Array(items.iter().map(nice).collect()),
        other => other.clone(),
    }
}

/// `metaFieldFromPkg(pkg, key)`.
fn meta_field(package: &Json, key: &str) -> Option<Json> {
    let value = get(package, key)?;
    if !truthy(Some(value)) {
        return None;
    }
    if key == "license" {
        if let Some(t) = get(value, "type").filter(|t| truthy(Some(t))) {
            return Some(t.clone());
        }
    }
    match value {
        Json::Object(map) if map.is_empty() => None,
        Json::Array(items) if items.is_empty() => None,
        other => Some(other.clone()),
    }
}

/// `Shrinkwrap.metaFromNode(node, path)`.
fn meta_from_node(tree: &Tree, node: Id, lock_path: &str) -> Json {
    let n = &tree.nodes[node];
    let mut meta: IndexMap<String, Json> = IndexMap::new();
    if let Some(target) = n.target {
        meta.insert("resolved".into(), Json::String(relpath(lock_path, &tree.nodes[target].path)));
        meta.insert("link".into(), Json::Bool(true));
        return Json::Object(meta);
    }
    const KEYS: [&str; 19] = [
        "version", "dependencies", "peerDependencies", "peerDependenciesMeta", "optionalDependencies", "bundleDependencies",
        "acceptDependencies", "funding", "engines", "os", "cpu", "libc", "_integrity", "license", "_hasShrinkwrap",
        "hasInstallScript", "bin", "deprecated", "workspaces",
    ];
    for key in KEYS {
        if let Some(value) = meta_field(&n.package, key) {
            meta.insert(key.trim_start_matches('_').to_owned(), value);
        }
    }
    let is_root = tree.root(node) == node;
    if let Some(name) = tree.package_name(node) {
        let folder = crate::text::basename(&n.path).to_owned();
        let scoped_folder = if name.starts_with('@') {
            let parent = crate::file_access::dirname(&n.path);
            format!("{}/{folder}", crate::text::basename(&parent))
        } else {
            folder
        };
        if is_root || name != n.name || scoped_folder != name {
            meta.insert("name".into(), Json::String(name));
        }
    }
    if let Some(integrity) = &n.integrity {
        meta.insert("integrity".into(), Json::String(integrity.clone()));
    }
    if n.has_shrinkwrap {
        meta.insert("hasShrinkwrap".into(), Json::Bool(true));
    }
    let scripts = get(&n.package, "scripts");
    let install_script = truthy(get(&n.package, "hasInstallScript"))
        || ["install", "preinstall", "postinstall"].iter().any(|s| scripts.is_some_and(|sc| truthy(get(sc, s))));
    if install_script {
        meta.insert("hasInstallScript".into(), Json::Bool(true));
    }
    if let Some(resolved) = &n.resolved {
        if let Some(resolved) = tree::consistent_resolve(resolved) {
            meta.insert("resolved".into(), Json::String(resolved));
        }
    }
    if n.extraneous {
        meta.insert("extraneous".into(), Json::Bool(true));
    } else {
        if n.peer {
            meta.insert("peer".into(), Json::Bool(true));
        }
        if n.dev {
            meta.insert("dev".into(), Json::Bool(true));
        }
        if n.optional {
            meta.insert("optional".into(), Json::Bool(true));
        }
        if n.dev_optional && !n.dev && !n.optional {
            meta.insert("devOptional".into(), Json::Bool(true));
        }
    }
    Json::Object(meta)
}

/// A lockfile of the root's tree (`hidden`: `node_modules/.package-lock.json`, without the root's entry).
pub fn lockfile(tree: &Tree, root: Id, hidden: bool) -> String {
    let lock_path = tree.nodes[root].path.clone();
    let mut packages: IndexMap<String, Json> = IndexMap::new();
    if !hidden {
        let meta = meta_from_node(tree, root, &lock_path);
        if meta.as_object().is_some_and(|m| !m.is_empty()) {
            packages.insert(String::new(), meta);
        }
    }
    for node in tree.inventory(root) {
        if node == root {
            continue;
        }
        packages.insert(relpath(&lock_path, &tree.nodes[node].path), meta_from_node(tree, node, &lock_path));
    }
    let name = tree.package_name(root).unwrap_or_else(|| tree.nodes[root].name.clone());
    let mut data: IndexMap<String, Json> = IndexMap::new();
    data.insert("name".into(), Json::String(name));
    data.insert("lockfileVersion".into(), Json::Number(3.0));
    data.insert("requires".into(), Json::Bool(true));
    data.insert("packages".into(), Json::Object(packages));
    format!("{}\n", pretty(&nice(&Json::Object(data))))
}
