//! Packages installed into opencode's cache (packages/util/src/npm.ts `Npm.which`, `add`): a registry package lives
//! under `$XDG_CACHE_HOME/opencode/npm/<name>@<spec>/<generation>/`, a directory npm's layout fills (`package.json`,
//! `package-lock.json`, `node_modules/<name>` and its dependencies, `node_modules/.bin`, `node_modules/.package-lock.json`),
//! the newest numbered generation being current. [`which`] finds the package's executable there, installing the
//! package first when the cache lacks it.
//!
//! TS installs through @npmcli/arborist; the port installs the same tree the same way: the npm config
//! (`npm/config.rs`), the ideal tree (`npm/tree.rs`: specs from npm-package-arg in `npm/spec.rs`, versions chosen by
//! npm-pick-manifest over node-semver in `npm/pick.rs` and `npm/semver.rs`, hoisting and nesting, peer and optional
//! dependencies, platform and engine checks), then the reify (`npm/reify.rs`: tarballs, bins, lockfiles), in a
//! staging directory published as the next generation, under the cross-process install lock (`npm/flock.rs`).
//! Without network access an install fails and the caller goes on without the package, as in TS.
//!
//! Not ported, all unreachable from what the server installs (the formatters' registry packages and whatever their
//! registry manifests depend on):
//! - Git dependencies. Only `git+file:` ones can be fetched without a Git host, and fetching one means cloning it
//!   and packing it as `npm pack` would (npm-packlist's file rules, `prepare` builds); such a dependency fails the
//!   install here (an optional one is dropped).
//! - `file:` and directory dependencies of a registry package (they resolve against the cache's staging directory),
//!   shrinkwrapped packages (`npm-shrinkwrap.json` inside a tarball) and bundled dependencies, whose trees arborist
//!   reads back from the unpacked tarball: a bundled dependency is unpacked with its bundle but not listed in the
//!   lockfiles.
//! - `Npm.add` of anything but a registry spec, `Npm.check` and `Npm.update` (plugins and providers, out of scope).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use opencode_protocol::Json;

use crate::json;

mod config;
mod flock;
mod pick;
mod reify;
mod semver;
mod spec;
mod tree;

/// A registry spec (`npm-package-arg` for registry packages): its name, the spec its cache directory is named
/// after (`latest` for a bare name) and the spec written (`None` for a bare name).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    name: String,
    spec: String,
    raw_spec: Option<String>,
    kind: spec::Kind,
}

/// `parse`: a registry spec (`version`, `range` or `tag`); `None` for anything else (Git, files, URLs).
fn parse(pkg: &str) -> Option<Target> {
    let cwd = std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".into());
    let result = spec::npa(pkg, &cwd).ok()?;
    let name = result.name.clone()?;
    if !matches!(result.kind, spec::Kind::Version | spec::Kind::Range | spec::Kind::Tag) {
        return None;
    }
    let bare = result.raw == name;
    let spec = if bare { "latest".to_owned() } else { result.raw_spec.clone() };
    Some(Target { name, spec, raw_spec: (!bare).then(|| result.raw_spec.clone()), kind: result.kind })
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

/// Installs of one package directory in this process are serialized before the cross-process lock is taken.
fn lock_for(dir: &Path) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
        std::sync::OnceLock::new();
    LOCKS.get_or_init(Default::default).lock().unwrap().entry(dir.to_owned()).or_default().clone()
}

/// `install` of a registry package into its cache directory, published as a new generation.
async fn install(target: &Target, dir: &Path) -> Result<(), String> {
    let lock = lock_for(dir);
    let _held = lock.lock().await;
    let _locked = flock::acquire(&format!("npm-install:{}", dir.display())).await?;
    if let Some(active) = current(dir) {
        if active.join("node_modules").join(&target.name).exists() {
            return Ok(());
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
    let started = crate::id::now();
    let staging = root.join(format!(".staging-{started}-{}", uuid()));
    let result = reify_into(&staging, target, dir).await;
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

pub(crate) fn uuid() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// `arborist.reify({ add: [pkg], save: true, saveType: "prod" })` into the empty `staging` directory, with the
/// config of `config_dir` (the package's cache directory).
async fn reify_into(staging: &Path, target: &Target, config_dir: &Path) -> Result<(), String> {
    let flat = config::load(config_dir);
    let registry = reify::Registry::new(flat.clone());
    let options = tree::TreeOptions {
        legacy_peer_deps: flat.legacy_peer_deps,
        strict_peer_deps: flat.strict_peer_deps,
        force: flat.force,
        prefer_dedupe: flat.prefer_dedupe,
        install_strategy: flat.install_strategy.clone(),
        engine_strict: flat.engine_strict,
        omit: flat.omit.clone(),
        os: flat.os.clone(),
        cpu: flat.cpu.clone(),
        libc: flat.libc.clone(),
    };
    // `#add`: a bare name is `*`; an explicit tag is first resolved to the version it names.
    let root_spec = match (&target.raw_spec, &target.kind) {
        (None, _) => "*".to_owned(),
        (Some(raw), spec::Kind::Tag) => {
            let spec = spec::resolve(&target.name, raw, "/")?;
            let manifest = tree::Source::manifest(&registry, &spec).await?;
            json::get_str(&manifest, "version").unwrap_or_default().to_owned()
        }
        (Some(raw), _) => raw.clone(),
    };
    let staging_path = staging.to_string_lossy().into_owned();
    let mut builder = tree::Builder::new(&staging_path, &registry, options);
    builder.build(&target.name, &root_spec).await?;
    let root = builder.root;
    std::fs::create_dir_all(staging).map_err(|e| e.to_string())?;
    // `omit`: the omitted nodes stay in the ideal tree's lockfile but are not unpacked
    for node in builder.tree.inventory(root) {
        if node != root && builder.should_omit(node) {
            builder.tree.nodes[node].inert = true;
        }
    }
    reify::reify(&mut builder.tree, root, &flat).await?;
    let tree = &mut builder.tree;
    // `_saveIdealTree`: the root's dependency at the installed version (`savePrefix: ""`), then the lockfile
    let child = tree.edge_to(root, &target.name);
    let saved = match child {
        Some(child) => {
            let version = tree.version(child);
            let range = if version.is_empty() { "*".to_owned() } else { version };
            match tree.package_name(child) {
                Some(name) if name != target.name => format!("npm:{name}@{range}"),
                _ => range,
            }
        }
        None => root_spec.clone(),
    };
    let mut dependencies = IndexMap::new();
    dependencies.insert(target.name.clone(), Json::String(saved));
    let mut package = IndexMap::new();
    package.insert("dependencies".to_owned(), Json::Object(dependencies));
    let package = Json::Object(package);
    // the root's saved dependency is also what the lockfile's root entry lists
    tree.nodes[root].package = package.clone();
    std::fs::write(staging.join("package.json"), format!("{}\n", reify::pretty(&package))).map_err(|e| e.to_string())?;
    std::fs::write(staging.join("package-lock.json"), reify::lockfile(tree, root, false)).map_err(|e| e.to_string())?;
    for node in tree.inventory(root) {
        if node != root && tree.nodes[node].inert && tree.root(node) == root {
            tree.detach(node);
        }
    }
    let hidden = staging.join("node_modules").join(".package-lock.json");
    if std::fs::write(&hidden, reify::lockfile(tree, root, true)).is_err() {
        let _ = std::fs::remove_file(&hidden);
    }
    let installed = tree.edge_to(root, &target.name).map(|c| PathBuf::from(&tree.nodes[c].path));
    let directory = installed.unwrap_or_else(|| staging.join("node_modules").join(&target.name));
    if !directory.is_dir() {
        return Err(format!("Failed to install {}", target.name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs() {
        let short = |pkg: &str| parse(pkg).map(|t| (t.name, t.spec, t.raw_spec));
        assert_eq!(short("prettier"), Some(("prettier".into(), "latest".into(), None)));
        assert_eq!(short("@biomejs/biome"), Some(("@biomejs/biome".into(), "latest".into(), None)));
        assert_eq!(short("oxfmt@1.2.3"), Some(("oxfmt".into(), "1.2.3".into(), Some("1.2.3".into()))));
        assert_eq!(short("github:a/b"), None);
    }

    /// The cases `harness/npm/semvercases.ts` records from the reference's node-semver, npm-pick-manifest and
    /// npm-package-arg (`NPM_SEMVER_CASES=<file>`, see `harness/npm/run.sh`).
    #[test]
    #[ignore]
    fn recorded_semver_cases() {
        let path = std::env::var("NPM_SEMVER_CASES").expect("NPM_SEMVER_CASES");
        let cases = json::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        let list = json::array(&cases).unwrap();
        let mut wrong = Vec::new();
        for case in list {
            let (actual, expected) = check_case(case);
            if actual != expected {
                wrong.push(format!("{}\n  rust: {}\n  bun:  {}", json::text(case), json::text(&actual), json::text(&expected)));
            }
        }
        eprintln!("{} of {} semver cases differ", wrong.len(), list.len());
        assert!(wrong.is_empty(), "{}", wrong.iter().take(15).cloned().collect::<Vec<_>>().join("\n"));
    }

    fn check_case(case: &Json) -> (Json, Json) {
        use semver::Options;
        let text = |key: &str| json::get_str(case, key).unwrap_or_default().to_owned();
        let options = match json::get(case, "options") {
            Some(Json::Bool(true)) => Options::LOOSE,
            Some(Json::Object(o)) => Options {
                loose: matches!(o.get("loose"), Some(Json::Bool(true))),
                include_prerelease: matches!(o.get("includePrerelease"), Some(Json::Bool(true))),
            },
            _ => Options::STRICT,
        };
        let value = |v: Option<String>| v.map(Json::String).unwrap_or(Json::Null);
        let outcome = |r: Result<Json, String>| match r {
            Ok(v) => crate::json::Obj::new().set("value", v).done(),
            Err(_) => crate::json::Obj::new().set("throws", Json::Bool(true)).done(),
        };
        let expected = json::get(case, "result").cloned().unwrap_or(Json::Null);
        let actual = match json::get_str(case, "op").unwrap() {
            "satisfies" => outcome(semver::satisfies(&text("version"), &text("range"), options).map(Json::Bool)),
            "validRange" => outcome(Ok(value(semver::valid_range(&text("range"), options)))),
            "valid" => outcome(Ok(value(semver::valid(&text("version"), options)))),
            "clean" => outcome(Ok(value(semver::clean(&text("version"), options)))),
            "compare" => outcome(semver::compare(&text("a"), &text("b"), options).map(|n| Json::Number(n as f64))),
            "npa" => outcome(spec::resolve(&text("name"), &text("spec"), "/w").map(|s| {
                let kind = match s.kind {
                    spec::Kind::Version => "version",
                    spec::Kind::Range => "range",
                    spec::Kind::Tag => "tag",
                    spec::Kind::Alias(_) => "alias",
                    spec::Kind::Remote => "remote",
                    spec::Kind::Git { .. } => "git",
                    spec::Kind::File => "file",
                    spec::Kind::Directory => "directory",
                };
                if let spec::Kind::Git { hosted: true, .. } = s.kind {
                    return crate::json::Obj::new().str("type", kind).set("hosted", Json::Bool(true)).done();
                }
                crate::json::Obj::new()
                    .str("type", kind)
                    .set("fetchSpec", value(s.fetch_spec.clone()))
                    .set("saveSpec", value(s.save_spec.clone()))
                    .done()
            })),
            "pick" => {
                let before = json::get_str(case, "before")
                    .map(|b| opencode_codemode::jsdate::parse(&b.encode_utf16().collect::<Vec<u16>>()));
                let opts = pick::PickOptions { before, ..Default::default() };
                outcome(pick::pick(json::get(case, "packument").unwrap(), &text("wanted"), &opts))
            }
            other => panic!("unknown op {other}"),
        };
        (actual, expected)
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

    /// One install of `harness/npm/treecases.ts` (`NPM_DRIVER_PKG=<spec>`, in an environment whose caches and npm
    /// config the harness prepared): prints what `which` answers.
    #[test]
    #[ignore]
    fn install_driver() {
        let package = std::env::var("NPM_DRIVER_PKG").expect("NPM_DRIVER_PKG");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let bin = runtime.block_on(which(&package, None));
        println!("WHICH:{}", bin.unwrap_or_else(|| "undefined".into()));
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
