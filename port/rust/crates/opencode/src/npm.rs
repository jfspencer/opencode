//! Packages installed into opencode's cache (packages/util/src/npm.ts `Npm.which`, `add`, npm-config.ts's registry):
//! a registry package lives under `$XDG_CACHE_HOME/opencode/npm/<name>@<spec>/<generation>/`, a directory npm's
//! layout fills (`package.json`, `node_modules/<name>`, `node_modules/.bin`), the newest numbered generation being
//! current. [`which`] finds the package's executable there, installing the package first when the cache lacks it.
//!
//! TS installs through @npmcli/arborist. The port installs what the formatters need the same way into the same
//! layout: the spec resolved against the registry's packument, the package and its dependencies (optional ones for
//! this platform only) unpacked flat after their integrity is checked, executables linked under `.bin`, the staging
//! directory published as the next generation. Without network access an install fails and the caller goes on
//! without the package, as in TS.
//!
//! TODO(port): Git specs, nested installs of conflicting dependency versions, the full semver range grammar (`^`,
//! `~`, `x`, comparators and `||` are understood), `.npmrc` beyond `registry=`, registry credentials, the
//! cross-process install lock (`EffectFlock`; installs of one package in this process are serialized).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use base64::Engine;
use opencode_protocol::Json;
use sha2::Digest;

use crate::json;

/// A registry spec (`npm-package-arg` for registry packages): its name and the version, range or tag asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    name: String,
    spec: String,
}

/// `parse`: `name`, `name@spec`, `@scope/name[@spec]`; `None` for anything else (Git, files, URLs).
fn parse(pkg: &str) -> Option<Target> {
    let (name, spec) = match pkg.strip_prefix('@') {
        Some(rest) => {
            let (scope_name, spec) = match rest.split_once('@') {
                Some((name, spec)) => (name, Some(spec)),
                None => (rest, None),
            };
            (format!("@{scope_name}"), spec)
        }
        None => match pkg.split_once('@') {
            Some((name, spec)) => (name.to_owned(), Some(spec)),
            None => (pkg.to_owned(), None),
        },
    };
    let valid = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c));
    let parts: Vec<&str> = name.trim_start_matches('@').split('/').collect();
    let scoped = name.starts_with('@');
    if (scoped && parts.len() != 2) || (!scoped && parts.len() != 1) || !parts.iter().all(|p| valid(p)) {
        return None;
    }
    if pkg.contains(':') || pkg.contains('#') {
        return None;
    }
    let spec = spec.filter(|s| !s.is_empty()).unwrap_or("latest").to_owned();
    Some(Target { name, spec })
}

fn cache_root() -> PathBuf {
    PathBuf::from(crate::inventory::config::Options::get().cache.clone()).join("npm")
}

/// `generations`: the numbered generations of a package's cache directory, oldest first.
fn generations(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()))
        .collect();
    found.sort_by(|a, b| a.parse::<f64>().unwrap_or(0.0).total_cmp(&b.parse::<f64>().unwrap_or(0.0)));
    found
}

fn current(dir: &Path) -> Option<PathBuf> {
    generations(dir).last().map(|latest| dir.join(latest))
}

/// `pick`: the package's executable in a generation (`bin` when given, the only one, or the one its manifest names).
fn pick(dir: &Path, name: &str, bin: Option<&str>) -> Option<String> {
    let bin_dir = dir.join("node_modules").join(".bin");
    let mut files: Vec<String> = std::fs::read_dir(&bin_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    let path = |file: &str| Some(bin_dir.join(file).to_string_lossy().into_owned());
    if files.is_empty() {
        return None;
    }
    if let Some(bin) = bin {
        return if files.iter().any(|f| f == bin) { path(bin) } else { None };
    }
    if files.len() == 1 {
        return path(&files[0]);
    }
    let manifest = std::fs::read_to_string(dir.join("node_modules").join(name).join("package.json")).ok().and_then(|t| json::parse(&t));
    if let Some(bins) = manifest.as_ref().and_then(|m| json::get(m, "bin")) {
        let unscoped = name.rsplit('/').next().unwrap_or(name);
        match bins {
            Json::String(_) => return path(unscoped),
            Json::Object(entries) => {
                let selected = if entries.contains_key(unscoped) { Some(unscoped.to_owned()) } else { entries.keys().next().cloned() };
                return selected.and_then(|s| path(&s));
            }
            _ => {}
        }
    }
    path(&files[0])
}

/// `Npm.which`: the executable of a cached package, installing it when the cache has none; `None` when that fails.
pub async fn which(pkg: &str, bin: Option<&str>) -> Option<String> {
    let target = parse(pkg)?;
    let root = cache_root().join(format!("{}@{}", target.name, target.spec));
    if let Some(found) = current(&root).and_then(|generation| pick(&generation, &target.name, bin)) {
        return Some(found);
    }
    if let Err(error) = install(&target, &root).await {
        eprintln!("npm install of {pkg} failed: {error}");
        return None;
    }
    current(&root).and_then(|generation| pick(&generation, &target.name, bin))
}

/// Installs of one package directory are serialized (`flock.acquire("npm-install:<dir>")`).
fn lock_for(dir: &Path) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
        std::sync::OnceLock::new();
    LOCKS.get_or_init(Default::default).lock().unwrap().entry(dir.to_owned()).or_default().clone()
}

/// `install` of a registry package into its cache directory, published as a new generation.
async fn install(target: &Target, dir: &Path) -> Result<(), String> {
    let lock = lock_for(dir);
    let _held = lock.lock().await;
    if let Some(active) = current(dir) {
        if active.join("node_modules").join(&target.name).exists() {
            return Ok(());
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
    let started = crate::id::now();
    let staging = root.join(format!(".staging-{started}-{}", uuid()));
    let result = reify(&staging, target, &registry()).await;
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    let completed = crate::id::now();
    let newest = generations(dir).last().and_then(|g| g.parse::<i64>().ok()).unwrap_or(0);
    let generation = dir.join(completed.max(newest + 1).to_string());
    std::fs::rename(&staging, &generation).map_err(|e| {
        let _ = std::fs::remove_dir_all(&staging);
        e.to_string()
    })
}

fn uuid() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// `NpmConfig.registry`: `npm_config_registry`, else the user's `.npmrc` `registry=`, else npm's default; no
/// trailing slash.
fn registry() -> String {
    let from_env = ["npm_config_registry", "NPM_CONFIG_REGISTRY"].iter().find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()));
    let from_rc = || {
        let home = crate::file_access::home();
        let text = std::fs::read_to_string(Path::new(&home).join(".npmrc")).ok()?;
        text.lines().filter_map(|line| line.trim().strip_prefix("registry")).find_map(|rest| {
            rest.trim_start().strip_prefix('=').map(|value| value.trim().trim_matches('"').to_owned())
        })
    };
    let registry = from_env.or_else(from_rc).unwrap_or_else(|| "https://registry.npmjs.org".into());
    registry.trim_end_matches('/').to_owned()
}

async fn get(url: &str, accept: &str) -> Result<Vec<u8>, String> {
    let mut url = url.to_owned();
    for _ in 0..5 {
        let headers = vec![("accept".to_owned(), accept.to_owned())];
        let response = crate::llm::http::send("GET", &url, &headers, String::new()).await.map_err(|f| f.message)?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            let location = response.headers.iter().find(|(k, _)| k == "location").map(|(_, v)| v.clone());
            url = location.ok_or_else(|| "redirect without a location".to_owned())?;
            continue;
        }
        if response.status != 200 {
            return Err(format!("{url}: HTTP {}", response.status));
        }
        let mut body = response.body;
        let mut out = Vec::new();
        while let Some(chunk) = body.next().await {
            out.extend_from_slice(&chunk.map_err(|f| f.message)?);
        }
        return Ok(out);
    }
    Err(format!("{url}: too many redirects"))
}

/// One resolved package version.
struct Version {
    name: String,
    version: String,
    manifest: Json,
}

async fn resolve(registry: &str, name: &str, spec: &str) -> Result<Version, String> {
    let url = format!("{registry}/{}", name.replacen('/', "%2f", 1));
    let packument = get(&url, "application/vnd.npm.install-v1+json; q=1.0, application/json; q=0.8, */*").await?;
    let packument = json::parse(&String::from_utf8_lossy(&packument)).ok_or("the packument is not JSON")?;
    let versions = match json::get(&packument, "versions") {
        Some(Json::Object(versions)) => versions,
        _ => return Err(format!("no versions of {name}")),
    };
    let tagged = json::get(&packument, "dist-tags").and_then(|tags| json::get_str(tags, spec)).map(str::to_owned);
    let chosen = match tagged {
        Some(version) => Some(version),
        None => semver::max_satisfying(versions.keys().map(String::as_str), spec),
    }
    .ok_or_else(|| format!("no version of {name} matches {spec}"))?;
    let manifest = versions.get(&chosen).cloned().ok_or_else(|| format!("{name}@{chosen} is not in the registry"))?;
    Ok(Version { name: name.to_owned(), version: chosen, manifest })
}

/// Whether an optional package's `os`, `cpu` and `libc` allow this platform.
fn platform_allows(manifest: &Json) -> bool {
    let os = if cfg!(target_os = "macos") { "darwin" } else { "linux" };
    let cpu = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    let libc = if Path::new("/lib/ld-musl-x86_64.so.1").exists() || Path::new("/lib/ld-musl-aarch64.so.1").exists() { "musl" } else { "glibc" };
    let allows = |key: &str, value: &str| match json::get(manifest, key) {
        Some(Json::Array(list)) => {
            let items: Vec<&str> = list.iter().filter_map(json::as_str).collect();
            let denied = items.iter().any(|item| item.strip_prefix('!') == Some(value));
            let allowed: Vec<&&str> = items.iter().filter(|item| !item.starts_with('!')).collect();
            !denied && (allowed.is_empty() || allowed.iter().any(|item| **item == value))
        }
        _ => true,
    };
    allows("os", os) && allows("cpu", cpu) && (os != "linux" || allows("libc", libc))
}

/// Installs `target` and its dependency tree flat into `staging/node_modules`, links the executables and records the
/// root dependency (`savePrefix: ""`).
async fn reify(staging: &Path, target: &Target, registry: &str) -> Result<(), String> {
    let modules = staging.join("node_modules");
    std::fs::create_dir_all(&modules).map_err(|e| e.to_string())?;
    let root = resolve(registry, &target.name, &target.spec).await?;
    let mut installed: BTreeMap<String, String> = BTreeMap::new();
    let mut queue: Vec<(Version, bool)> = vec![(root, false)];
    let mut top = None;
    while let Some((version, optional)) = queue.pop() {
        if installed.contains_key(&version.name) {
            continue;
        }
        if optional && !platform_allows(&version.manifest) {
            continue;
        }
        let unpacked = unpack(&modules, &version).await;
        if let Err(error) = unpacked {
            if optional {
                continue;
            }
            return Err(error);
        }
        // The registry's abbreviated manifests may leave out `libc`: the unpacked manifest decides too.
        if optional {
            let manifest = std::fs::read_to_string(modules.join(&version.name).join("package.json")).ok().and_then(|t| json::parse(&t));
            if manifest.is_some_and(|manifest| !platform_allows(&manifest)) {
                let _ = std::fs::remove_dir_all(modules.join(&version.name));
                continue;
            }
        }
        installed.insert(version.name.clone(), version.version.clone());
        if top.is_none() {
            top = Some((version.name.clone(), version.version.clone()));
        }
        for (key, optional_dependency) in [("dependencies", false), ("optionalDependencies", true)] {
            let Some(Json::Object(dependencies)) = json::get(&version.manifest, key) else { continue };
            for (name, range) in dependencies {
                if installed.contains_key(name) {
                    continue;
                }
                match resolve(registry, name, json::as_str(range).unwrap_or("*")).await {
                    Ok(dependency) => queue.push((dependency, optional_dependency)),
                    Err(error) if !optional_dependency => return Err(error),
                    Err(_) => {}
                }
            }
        }
    }
    for name in installed.keys() {
        link_bins(&modules, name)?;
    }
    let (name, version) = top.ok_or("nothing was installed")?;
    // npm writes it as `JSON.stringify(manifest, null, 2)` and a newline.
    let text = format!(
        "{{\n  \"dependencies\": {{\n    {}: {}\n  }}\n}}\n",
        json::text(&json::string(&name)),
        json::text(&json::string(&version))
    );
    std::fs::write(staging.join("package.json"), text).map_err(|e| e.to_string())
}

/// Downloads a version's tarball, checks its integrity and unpacks it into `node_modules/<name>`.
async fn unpack(modules: &Path, version: &Version) -> Result<(), String> {
    let dist = json::get(&version.manifest, "dist").ok_or("no dist")?;
    let url = json::get_str(dist, "tarball").ok_or("no tarball")?;
    let tarball = get(url, "*/*").await?;
    if let Some(integrity) = json::get_str(dist, "integrity") {
        let ok = integrity.split_whitespace().any(|sri| match sri.split_once('-') {
            Some(("sha512", expected)) => base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(&tarball)) == expected,
            Some(("sha256", expected)) => base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(&tarball)) == expected,
            _ => false,
        });
        if !ok {
            return Err(format!("{}@{}: integrity check failed", version.name, version.version));
        }
    }
    let destination = modules.join(&version.name);
    let destination_clone = destination.clone();
    tokio::task::spawn_blocking(move || untar(&tarball, &destination_clone)).await.map_err(|e| e.to_string())?
}

/// A gzipped tarball's `package/` entries written under `destination` (regular files with their modes, and
/// directories; pax and GNU long names honored).
fn untar(tarball: &[u8], destination: &Path) -> Result<(), String> {
    use std::io::Read;
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(tarball).read_to_end(&mut data).map_err(|e| e.to_string())?;
    let mut at = 0;
    let mut long_name: Option<String> = None;
    let octal = |field: &[u8]| -> u64 {
        let text = String::from_utf8_lossy(field);
        u64::from_str_radix(text.trim_matches(|c: char| c == '\0' || c == ' '), 8).unwrap_or(0)
    };
    let text = |field: &[u8]| String::from_utf8_lossy(field.split(|b| *b == 0).next().unwrap_or(&[])).into_owned();
    while at + 512 <= data.len() {
        let header = &data[at..at + 512];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let size = octal(&header[124..136]) as usize;
        let kind = header[156];
        let body = data.get(at + 512..at + 512 + size).ok_or("truncated tarball")?;
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
                // A pax header: its `path` names the next entry.
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
        // npm strips the first path component (`package/`).
        let Some((_, relative)) = name.split_once('/') else { continue };
        if relative.is_empty() || relative.split('/').any(|part| part == ".." ) || relative.starts_with('/') {
            continue;
        }
        let path = destination.join(relative);
        match kind {
            b'5' => std::fs::create_dir_all(&path).map_err(|e| e.to_string())?,
            b'0' | 0 | b'7' => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(&path, body).map_err(|e| e.to_string())?;
                let mode = octal(&header[100..108]) as u32;
                // npm keeps the executable bits and makes files readable.
                use std::os::unix::fs::PermissionsExt;
                let mode = (mode & 0o777) | 0o644;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode));
            }
            _ => {}
        }
    }
    Ok(())
}

/// `binLinks`: a package's executables linked into `node_modules/.bin` (made executable).
fn link_bins(modules: &Path, name: &str) -> Result<(), String> {
    let manifest = std::fs::read_to_string(modules.join(name).join("package.json")).ok().and_then(|t| json::parse(&t));
    let Some(manifest) = manifest else { return Ok(()) };
    let unscoped = name.rsplit('/').next().unwrap_or(name);
    let bins: Vec<(String, String)> = match json::get(&manifest, "bin") {
        Some(Json::String(path)) => vec![(unscoped.to_owned(), path.clone())],
        Some(Json::Object(entries)) => entries.iter().filter_map(|(k, v)| json::as_str(v).map(|v| (k.clone(), v.to_owned()))).collect(),
        _ => vec![],
    };
    if bins.is_empty() {
        return Ok(());
    }
    let bin_dir = modules.join(".bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    for (bin, path) in bins {
        let bin = bin.rsplit('/').next().unwrap_or(&bin).to_owned();
        let target = Path::new("..").join(name).join(path.trim_start_matches("./"));
        let link = bin_dir.join(&bin);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        let resolved = bin_dir.join(&target);
        if let Ok(meta) = std::fs::metadata(&resolved) {
            let _ = std::fs::set_permissions(&resolved, std::fs::Permissions::from_mode(meta.permissions().mode() | 0o755));
        }
    }
    Ok(())
}

/// The part of node-semver dependency ranges use.
mod semver {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    struct V(u64, u64, u64, bool);

    fn version(text: &str) -> Option<V> {
        let text = text.trim().trim_start_matches(['v', '=']);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, !pre.is_empty()),
            None => (text.split('+').next()?, false),
        };
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        Some(V(parts.next()??, parts.next()??, parts.next()??, !pre))
    }

    /// A partial version (`1`, `1.2`, `1.x`): its given numbers.
    fn partial(text: &str) -> Vec<u64> {
        text.trim_start_matches(['v', '=']).split('.').map_while(|p| p.parse::<u64>().ok()).take(3).collect()
    }

    fn satisfies_one(v: V, comparator: &str) -> bool {
        let c = comparator.trim();
        if c.is_empty() || c == "*" || c == "x" || c == "latest" {
            return v.3;
        }
        if let Some(rest) = c.strip_prefix('^') {
            let p = partial(rest);
            let low = V(p.first().copied().unwrap_or(0), p.get(1).copied().unwrap_or(0), p.get(2).copied().unwrap_or(0), false);
            let high = match p.as_slice() {
                [0, 0, patch] => V(0, 0, patch + 1, false),
                [0, minor, ..] => V(0, minor + 1, 0, false),
                [major, ..] => V(major + 1, 0, 0, false),
                [] => return v.3,
            };
            return v >= low && v < high && v.3;
        }
        if let Some(rest) = c.strip_prefix('~') {
            let p = partial(rest.trim_start_matches('>'));
            let low = V(p.first().copied().unwrap_or(0), p.get(1).copied().unwrap_or(0), p.get(2).copied().unwrap_or(0), false);
            let high = match p.as_slice() {
                [major] => V(major + 1, 0, 0, false),
                [major, minor, ..] => V(*major, minor + 1, 0, false),
                [] => return v.3,
            };
            return v >= low && v < high && v.3;
        }
        for (op, test) in [(">=", 0u8), ("<=", 1), (">", 2), ("<", 3)] {
            if let Some(rest) = c.strip_prefix(op) {
                let Some(bound) = version(rest).or_else(|| {
                    let p = partial(rest);
                    (!p.is_empty()).then(|| V(p[0], p.get(1).copied().unwrap_or(0), p.get(2).copied().unwrap_or(0), true))
                }) else {
                    return false;
                };
                let bound = V(bound.0, bound.1, bound.2, true);
                let ok = match test {
                    0 => v >= bound,
                    1 => v <= bound,
                    2 => v > bound,
                    _ => v < bound,
                };
                return ok && v.3;
            }
        }
        if let Some(exact) = version(c) {
            return v == exact;
        }
        // `1`, `1.2`, `1.x`.
        let p = partial(c);
        !p.is_empty() && p.iter().enumerate().all(|(i, n)| [v.0, v.1, v.2][i] == *n) && v.3
    }

    /// `semver.maxSatisfying`: the newest version the range allows (`||` alternatives of space-separated comparators).
    pub fn max_satisfying<'a>(versions: impl Iterator<Item = &'a str>, range: &str) -> Option<String> {
        versions
            .filter_map(|text| version(text).map(|v| (v, text)))
            .filter(|(v, _)| range.split("||").any(|alternative| alternative.split_whitespace().all(|c| satisfies_one(*v, c)) || (alternative.trim().is_empty() && v.3)))
            .max_by_key(|(v, _)| *v)
            .map(|(_, text)| text.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs() {
        assert_eq!(parse("prettier"), Some(Target { name: "prettier".into(), spec: "latest".into() }));
        assert_eq!(parse("@biomejs/biome"), Some(Target { name: "@biomejs/biome".into(), spec: "latest".into() }));
        assert_eq!(parse("oxfmt@1.2.3"), Some(Target { name: "oxfmt".into(), spec: "1.2.3".into() }));
        assert_eq!(parse("github:a/b"), None);
    }

    #[test]
    fn ranges() {
        let versions = ["1.0.0", "1.2.3", "1.9.0", "2.0.0", "2.1.0-beta.1"];
        let max = |range: &str| semver::max_satisfying(versions.iter().copied(), range);
        assert_eq!(max("^1.2.0").as_deref(), Some("1.9.0"));
        assert_eq!(max("~1.2.0").as_deref(), Some("1.2.3"));
        assert_eq!(max("*").as_deref(), Some("2.0.0"));
        assert_eq!(max("1.0.0").as_deref(), Some("1.0.0"));
        assert_eq!(max(">=1.5.0 <2.0.0").as_deref(), Some("1.9.0"));
        assert_eq!(max("^3").as_deref(), None);
    }

    /// A real install from the registry (`NPM_INSTALL_CHECK=<package>`, with network access): the package's
    /// executable resolves and runs.
    #[test]
    #[ignore]
    fn installs_from_the_registry() {
        let package = std::env::var("NPM_INSTALL_CHECK").expect("NPM_INSTALL_CHECK names a package");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let bin = runtime.block_on(which(&package, None)).expect("installed");
        let again = runtime.block_on(which(&package, None)).expect("cached");
        assert_eq!(bin, again);
        let output = std::process::Command::new(&bin).arg("--version").output().unwrap();
        eprintln!("{bin}: {}", String::from_utf8_lossy(&output.stdout).trim());
        assert!(output.status.success());
    }

    #[test]
    fn picks_a_generation() {
        let dir = std::env::temp_dir().join(format!("oc-npm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("12/node_modules/.bin")).unwrap();
        std::fs::create_dir_all(dir.join("9")).unwrap();
        std::fs::write(dir.join("12/node_modules/.bin/prettier"), "").unwrap();
        let generation = current(&dir).unwrap();
        assert!(generation.ends_with("12"));
        assert!(pick(&generation, "prettier", None).unwrap().ends_with(".bin/prettier"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
