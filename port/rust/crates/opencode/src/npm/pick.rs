//! npm-pick-manifest 11.0.3 (the version a registry spec resolves to in a packument) and the manifest normalization
//! pacote 21.5.1 applies to the result (`PackageJson.normalizeSteps` without `_attributes`: `_id`, bundled
//! dependencies, `optionalDedupe`, scripts, funding, bin). Checked against the reference by `harness/npm/run.sh`.

use indexmap::IndexMap;
use opencode_protocol::Json;
use opencode_protocol::wire::number::js_number_string;

use super::semver::{self, Options};
use super::spec::{self, Kind};

/// `process.version` of the Bun the reference runs on (npm-pick-manifest's and arborist's `nodeVersion`).
pub const NODE_VERSION: &str = "v26.3.0";

/// The options arborist hands npm-pick-manifest: the default tag, `before` (ms) and the engine versions.
#[derive(Debug, Clone)]
pub struct PickOptions {
    pub default_tag: String,
    pub before: Option<f64>,
    pub node_version: String,
    pub npm_version: Option<String>,
}

impl Default for PickOptions {
    fn default() -> Self {
        PickOptions { default_tag: "latest".into(), before: None, node_version: NODE_VERSION.into(), npm_version: None }
    }
}

/// JS truthiness of a JSON value.
pub fn truthy(value: Option<&Json>) -> bool {
    match value {
        None | Some(Json::Null) => false,
        Some(Json::Bool(b)) => *b,
        Some(Json::Number(n)) => *n != 0.0 && !n.is_nan(),
        Some(Json::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `String(value)` for the values a packument holds.
pub fn js_string(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        Json::Number(n) => js_number_string(*n),
        Json::Bool(b) => b.to_string(),
        Json::Null => "null".into(),
        Json::Array(items) => items.iter().map(|v| if matches!(v, Json::Null) { String::new() } else { js_string(v) }).collect::<Vec<_>>().join(","),
        Json::Object(_) => "[object Object]".into(),
    }
}

fn get<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    match value {
        Json::Object(map) => map.get(key),
        _ => None,
    }
}

fn object(value: Option<&Json>) -> IndexMap<String, Json> {
    match value {
        Some(Json::Object(map)) => map.clone(),
        _ => IndexMap::new(),
    }
}

/// npm-install-checks `checkEngine` (without `force`): whether the manifest's `engines` allow these versions.
pub fn engine_ok(manifest: &Json, npm_version: Option<&str>, node_version: Option<&str>) -> bool {
    let engines = get(manifest, "engines");
    if !truthy(engines) {
        return true;
    }
    let engines = engines.unwrap();
    let range_fails = |version: Option<&str>, key: &str| {
        let Some(version) = version.filter(|v| !v.is_empty()) else { return false };
        let range = get(engines, key);
        if !truthy(range) {
            return false;
        }
        match range {
            Some(Json::String(range)) => !semver::satisfies(version, range, Options { loose: false, include_prerelease: true }).unwrap_or(false),
            // A non-string range throws inside `new Range`, which `satisfies` catches: false.
            _ => true,
        }
    };
    !(range_fails(node_version, "node") || range_fails(npm_version, "npm"))
}

fn is_before(times: Option<&Json>, version: &str, time: f64) -> bool {
    let Some(times) = times.filter(|t| truthy(Some(t))) else { return true };
    let entry = get(times, version);
    if !truthy(entry) {
        return true;
    }
    let text = js_string(entry.unwrap());
    let parsed = opencode_codemode::jsdate::parse(&text.encode_utf16().collect::<Vec<u16>>());
    parsed <= time
}

/// `pickManifest(packument, wanted, opts)` and the module wrapper's ETARGET: the chosen version's manifest (bin
/// normalized), or why there is none.
pub fn pick(packument: &Json, wanted: &str, opts: &PickOptions) -> Result<Json, String> {
    let name = get(packument, "name").map(js_string).unwrap_or_default();
    let picked = pick_inner(packument, &name, wanted, opts)?;
    let restricted = object(get(packument, "policyRestrictions").and_then(|p| get(p, "versions")));
    match picked {
        Some(mut manifest) => {
            normalize_bin_npm(&mut manifest);
            let version = get(&manifest, "version").map(js_string).unwrap_or_default();
            if restricted.contains_key(&version) {
                return Err(format!("Could not download {name}@{wanted} due to policy violations"));
            }
            Ok(manifest)
        }
        None => Err(format!("No matching version found for {name}@{wanted}.")),
    }
}

fn pick_inner(packument: &Json, name: &str, wanted: &str, opts: &PickOptions) -> Result<Option<Json>, String> {
    let times = get(packument, "time");
    let versions = object(get(packument, "versions"));
    let restricted = object(get(packument, "policyRestrictions").and_then(|p| get(p, "versions")));
    let staged: IndexMap<String, Json> = IndexMap::new();
    let time = match opts.before {
        Some(before) if truthy(times) => before,
        _ => f64::INFINITY,
    };
    let wanted_or_tag = if wanted.is_empty() { opts.default_tag.as_str() } else { wanted };
    let resolved = spec::resolve(if name.is_empty() { "x" } else { name }, wanted_or_tag, "/")?;
    let kind = resolved.kind.clone();
    if !matches!(kind, Kind::Tag | Kind::Version | Kind::Range) {
        return Err("Only tag, version, and range are supported".into());
    }
    let dist_tags = object(get(packument, "dist-tags"));
    let lookup = |version: &str| versions.get(version).or_else(|| staged.get(version)).or_else(|| restricted.get(version)).cloned();

    if !wanted.is_empty() && kind == Kind::Tag {
        let Some(version) = dist_tags.get(wanted).map(js_string) else { return Ok(None) };
        if is_before(times, &version, time) {
            return Ok(lookup(&version).filter(|m| truthy(Some(m))));
        }
        return pick_inner(packument, name, &format!("<={version}"), opts);
    }

    if !wanted.is_empty() && kind == Kind::Version {
        let Some(version) = semver::clean(wanted, Options::LOOSE) else { return Ok(None) };
        let manifest = lookup(&version).filter(|m| truthy(Some(m)));
        return Ok(if is_before(times, &version, time) { manifest } else { None });
    }

    let range = if kind == Kind::Range { wanted } else { "*" };
    if let Some(default) = dist_tags.get(&opts.default_tag).filter(|v| truthy(Some(v))) {
        let satisfied = range == "*"
            || match default {
                Json::String(version) => semver::satisfies(version, range, Options::LOOSE)?,
                // `semver.satisfies` of a non-string version throws in `new SemVer`.
                _ => return Err("Invalid version. Must be a string.".into()),
            };
        let default = js_string(default);
        if satisfied && !restricted.contains_key(&default) {
            if let Some(manifest) = versions.get(&default).filter(|m| truthy(Some(m))) {
                let ok = is_before(times, &default, time)
                    && engine_ok(manifest, opts.npm_version.as_deref(), Some(&opts.node_version))
                    && !truthy(get(manifest, "deprecated"))
                    && !staged.contains_key(&default);
                if ok {
                    return Ok(Some(manifest.clone()));
                }
            }
        }
    }

    let all: Vec<(String, Json)> = versions
        .iter()
        .chain(staged.iter())
        .chain(restricted.iter())
        .filter(|(version, _)| is_before(times, version, time))
        .map(|(v, m)| (v.clone(), m.clone()))
        .collect();
    if all.is_empty() {
        return Err(format!("No versions available for {name}"));
    }
    let mut entries = Vec::new();
    for (version, manifest) in all {
        if semver::satisfies(&version, range, Options::LOOSE)? {
            entries.push((version, manifest));
        }
    }
    struct Rank {
        not_restricted: i32,
        not_staged: i32,
        not_deprecated: i32,
        engine: i32,
    }
    let rank = |(version, manifest): &(String, Json)| Rank {
        not_restricted: i32::from(!restricted.contains_key(version)),
        not_staged: i32::from(!staged.contains_key(version)),
        not_deprecated: i32::from(!truthy(get(manifest, "deprecated"))),
        engine: i32::from(engine_ok(manifest, opts.npm_version.as_deref(), Some(&opts.node_version))),
    };
    let mut failure = None;
    let mut ranked: Vec<(Rank, (String, Json))> = entries.into_iter().map(|e| (rank(&e), e)).collect();
    ranked.sort_by(|(ra, (va, _)), (rb, (vb, _))| {
        let order = (rb.not_restricted - ra.not_restricted)
            .cmp(&0)
            .then((rb.not_staged - ra.not_staged).cmp(&0))
            .then(((rb.not_deprecated & rb.engine) - (ra.not_deprecated & ra.engine)).cmp(&0))
            .then((rb.engine - ra.engine).cmp(&0))
            .then((rb.not_deprecated - ra.not_deprecated).cmp(&0));
        order.then_with(|| match semver::rcompare(va, vb, Options::LOOSE) {
            Ok(value) => value.cmp(&0),
            Err(error) => {
                failure.get_or_insert(error);
                std::cmp::Ordering::Equal
            }
        })
    });
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(ranked.into_iter().next().map(|(_, (_, manifest))| manifest).filter(|m| truthy(Some(m))))
}

/// `path.basename` (posix).
fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() { "" } else { "/" };
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// `path.join('/', p)` then dropping the leading `/`: the normalized relative form npm-normalize-package-bin uses.
fn joined_relative(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let mut out = parts.join("/");
    if path.ends_with('/') && !out.is_empty() {
        out.push('/');
    }
    out
}

/// npm-normalize-package-bin 5.0.0 (the pick-manifest wrapper's `normalizeBin`).
pub fn normalize_bin_npm(manifest: &mut Json) {
    let Json::Object(map) = manifest else { return };
    let bin = map.get("bin").cloned();
    let name = map.get("name").cloned();
    let object = match bin {
        None => None,
        Some(value) if !truthy(Some(&value)) => None,
        Some(Json::String(path)) => match name.filter(|n| truthy(Some(n))) {
            Some(name) => Some(IndexMap::from([(js_string(&name), Json::String(path))])),
            None => None,
        },
        Some(Json::Array(items)) => {
            let mut out = IndexMap::new();
            for item in items {
                let text = js_string(&item);
                out.insert(basename(&text).to_owned(), item);
            }
            Some(out)
        }
        Some(Json::Object(entries)) => Some(entries),
        Some(_) => None,
    };
    let Some(entries) = object else {
        map.shift_remove("bin");
        return;
    };
    let mut clean = IndexMap::new();
    for (key, value) in entries {
        let base = joined_relative(&format!("/{}", basename(&key.replace(['\\', ':'], "/"))));
        let Json::String(target) = value else { continue };
        if base.is_empty() {
            continue;
        }
        let target = joined_relative(&format!("/{}", target.replace('\\', "/"))).replace('\\', "/");
        if target.is_empty() {
            continue;
        }
        clean.insert(base, Json::String(target));
    }
    if clean.is_empty() {
        map.shift_remove("bin");
    } else {
        map.insert("bin".into(), Json::Object(clean));
    }
}

/// `secureAndUnixifyPath` (@npmcli/package-json).
fn secure_path(reference: &str) -> String {
    let unixified = reference.replace(['\\', ':'], "/");
    let joined = joined_relative(&format!("/{unixified}"));
    let secured = if joined.is_empty() { "./".to_owned() } else { joined.replace(['\\', ':'], "/") };
    if secured.starts_with("./") { String::new() } else { secured }
}

/// @npmcli/package-json's `normalizePackageBin`.
fn normalize_bin_package(map: &mut IndexMap<String, Json>) {
    let name = map.get("name").cloned();
    let bin = map.get("bin").cloned();
    if !truthy(bin.as_ref()) {
        map.shift_remove("bin");
        return;
    }
    let mut bin = bin.unwrap();
    match &bin {
        Json::String(path) if truthy(name.as_ref()) => {
            bin = Json::Object(IndexMap::from([(js_string(name.as_ref().unwrap()), Json::String(path.clone()))]));
        }
        Json::Array(items) => {
            let mut out = IndexMap::new();
            for item in items {
                let text = js_string(item);
                out.insert(basename(&text).to_owned(), item.clone());
            }
            bin = Json::Object(out);
        }
        _ => {}
    }
    let Json::Object(entries) = bin else {
        map.shift_remove("bin");
        return;
    };
    let mut out = entries.clone();
    for (key, value) in entries {
        let Json::String(target) = value else {
            out.shift_remove(&key);
            continue;
        };
        let base = basename(&secure_path(&key)).to_owned();
        if base.is_empty() {
            out.shift_remove(&key);
            continue;
        }
        let secured = secure_path(&target);
        if secured.is_empty() {
            out.shift_remove(&key);
            continue;
        }
        if base != key {
            out.shift_remove(&key);
        }
        out.insert(base, Json::String(secured));
    }
    if out.is_empty() {
        map.shift_remove("bin");
    } else {
        map.insert("bin".into(), Json::Object(out));
    }
}

/// The normalization pacote's `manifest()` applies (normalizeSteps without `_attributes`).
pub fn normalize_manifest(manifest: &mut Json) {
    let Json::Object(map) = manifest else { return };
    if truthy(map.get("name")) && truthy(map.get("version")) {
        let id = format!("{}@{}", js_string(&map["name"]), js_string(&map["version"]));
        map.insert("_id".into(), Json::String(id));
    }
    if !map.contains_key("bundleDependencies") {
        if let Some(value) = map.get("bundledDependencies").cloned() {
            map.insert("bundleDependencies".into(), value);
        }
    }
    map.shift_remove("bundledDependencies");
    match map.get("bundleDependencies").cloned() {
        Some(Json::Bool(false)) => {
            map.insert("bundleDependencies".into(), Json::Array(vec![]));
        }
        Some(Json::Bool(true)) => {
            let keys = object(map.get("dependencies")).keys().map(|k| Json::String(k.clone())).collect();
            map.insert("bundleDependencies".into(), Json::Array(keys));
        }
        Some(Json::Object(entries)) => {
            map.insert("bundleDependencies".into(), Json::Array(entries.keys().map(|k| Json::String(k.clone())).collect()));
        }
        Some(Json::Array(_)) => {}
        Some(_) => {
            map.shift_remove("bundleDependencies");
        }
        None => {}
    }
    if truthy(map.get("dependencies")) {
        if let Some(Json::Object(optional)) = map.get("optionalDependencies").cloned() {
            if let Some(Json::Object(deps)) = map.get_mut("dependencies") {
                for name in optional.keys() {
                    deps.shift_remove(name);
                }
                if deps.is_empty() {
                    map.shift_remove("dependencies");
                }
            }
        }
    }
    match map.get("scripts").cloned() {
        None | Some(Json::Null) => {}
        Some(Json::Object(scripts)) => {
            let kept = scripts.into_iter().filter(|(_, v)| matches!(v, Json::String(_))).collect();
            map.insert("scripts".into(), Json::Object(kept));
        }
        Some(_) => {
            map.shift_remove("scripts");
        }
    }
    if let Some(Json::String(url)) = map.get("funding").cloned() {
        if !url.is_empty() {
            map.insert("funding".into(), Json::Object(IndexMap::from([("url".to_owned(), Json::String(url))])));
        }
    }
    normalize_bin_package(map);
}
