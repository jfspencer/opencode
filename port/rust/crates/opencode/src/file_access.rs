//! Path access for tools (packages/core/src/file-access.ts): a path a tool works with is resolved lexically against
//! the location (`~` expanded), then through its symlinks; a path outside both the location's directory and its
//! project's worktree needs `external_directory` approval for the directory that holds it. The approval's saved
//! pattern is the enclosing repository's root (or the directory itself), with `*` and `?` escaped.
//!
//! `authorizeRead` asks for the external directory, then for the read itself; a recovered file name in the same
//! directory (the read tool's near-miss recovery) shares the directory's approval. The editing tools ask through
//! [`authorize_external_for`] and [`crate::session::plan::assert`], which reword the plan agent's denials.

use opencode_protocol::Json;

use crate::location::Location;
use crate::tool::{Context, Failure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}

/// `ExternalDirectoryAuthorization`.
#[derive(Clone, Debug)]
pub struct External {
    /// The lexical directory the approval is for.
    #[allow(dead_code)]
    pub directory: String,
    pub resource: String,
    pub save: String,
}

/// `FileAccess.Target`.
#[derive(Clone, Debug)]
pub struct Target {
    pub absolute: String,
    /// Location-relative inside the location, absolute outside it.
    pub resource: String,
    pub external: Option<External>,
}

/// The process's home, user database and environment, as `os.homedir()` and friends give them (read once).
pub fn host() -> &'static opencode_shell::SystemHost {
    static HOST: std::sync::OnceLock<opencode_shell::SystemHost> = std::sync::OnceLock::new();
    HOST.get_or_init(opencode_shell::SystemHost::new)
}

/// `Global.Path.home`.
pub fn home() -> String {
    use opencode_shell::Host;
    host().home()
}

/// `FileAccess.resolvePath(directory, input)`: `~` expanded, then resolved against `directory`.
pub fn resolve_path(directory: &str, input: &str) -> String {
    opencode_shell::resolve_path(directory, input, &home())
}

/// `path.posix.dirname`.
pub fn dirname(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.is_empty() {
        return ".".into();
    }
    let rooted = bytes[0] == b'/';
    let mut end = None;
    let mut matched_slash = true;
    for index in (1..bytes.len()).rev() {
        if bytes[index] == b'/' {
            if !matched_slash {
                end = Some(index);
                break;
            }
        } else {
            matched_slash = false;
        }
    }
    match end {
        None => (if rooted { "/" } else { "." }).into(),
        Some(1) if rooted => "//".into(),
        Some(end) => path[..end].to_owned(),
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `path.join(directory, name)`.
pub fn join(directory: &str, name: &str) -> String {
    crate::project::normalize(&format!("{directory}/{name}"))
}

/// `realPath`: symlinks resolved through the part of the path that exists.
fn real_path(target: &str) -> String {
    if let Ok(real) = std::fs::canonicalize(target) {
        return real.to_string_lossy().into_owned();
    }
    let parent = dirname(target);
    if parent == target {
        return target.to_owned();
    }
    join(&real_path(&parent), basename(target))
}

/// `inside`: within the directory, or within the worktree unless the worktree is the file system's root.
fn inside(target: &str, directory: &str, worktree: &str) -> bool {
    crate::project::contains(directory, target) || (worktree != "/" && crate::project::contains(worktree, target))
}

/// `Project.root`: the nearest directory from `start` up that holds `.git` or `.hg`.
fn project_root(start: &str) -> Option<String> {
    let mut current = start.to_owned();
    loop {
        for marker in [".git", ".hg"] {
            if std::path::Path::new(&join(&current, marker)).exists() {
                return Some(current);
            }
        }
        let parent = dirname(&current);
        if parent == current {
            return None;
        }
        current = parent;
    }
}

fn resolve_blocking(directory: String, worktree: String, lexical: bool, path: String, kind: Option<Kind>) -> Target {
    let real = |p: &str| if lexical { p.to_owned() } else { real_path(p) };
    let absolute = resolve_path(&directory, &path);
    let canonical = real(&absolute);
    let (real_directory, real_worktree) = (real(&directory), real(&worktree));
    if inside(&absolute, &directory, &worktree) && inside(&canonical, &real_directory, &real_worktree) {
        let relative = crate::session::relative(&directory, &absolute);
        let resource = if relative.is_empty() { ".".to_owned() } else { relative.replace('\\', "/") };
        return Target { absolute, resource, external: None };
    }
    let kind = kind.or_else(|| match std::fs::metadata(&absolute) {
        Ok(meta) if meta.is_dir() => Some(Kind::Directory),
        Ok(_) => Some(Kind::File),
        Err(_) => None,
    });
    let holder = if kind == Some(Kind::Directory) { canonical.clone() } else { dirname(&canonical) };
    let root = project_root(&holder).unwrap_or_else(|| holder.clone());
    let external = External {
        resource: join(&holder, "*").replace('\\', "/"),
        save: join(&opencode_shell::wildcard::escape(&root), "*").replace('\\', "/"),
        directory: holder,
    };
    Target { absolute, resource: canonical.replace('\\', "/"), external: Some(external) }
}

/// `FileAccess.resolve`: where a path leads and what approval it needs, without asking for it.
pub async fn resolve(location: &Location, path: &str, kind: Option<Kind>) -> Target {
    let directory = location.directory.clone();
    let worktree = crate::project::absolutize(&location.project.directory);
    // Workspace-placed locations keep lexical paths, as the file system does.
    let lexical = location.workspace_id.is_some();
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || resolve_blocking(directory, worktree, lexical, path, kind))
        .await
        .expect("path resolution")
}

/// The `external_directory` request for the external directories among `targets`, in first-seen order.
fn external_ask(targets: &[Target], metadata: Option<Json>) -> Option<crate::permission::Ask> {
    let mut external: Vec<&External> = Vec::new();
    for item in targets.iter().filter_map(|t| t.external.as_ref()) {
        if !external.iter().any(|seen| seen.resource == item.resource) {
            external.push(item);
        }
    }
    if external.is_empty() {
        return None;
    }
    Some(crate::permission::Ask {
        action: "external_directory".into(),
        resources: external.iter().map(|e| e.resource.clone()).collect(),
        save: Some(external.iter().map(|e| e.save.clone()).collect()),
        metadata,
    })
}

/// `FileAccess.authorizeExternal`: one approval for every external directory among `targets`, in first-seen order.
pub async fn authorize_external(context: &Context, targets: &[Target], metadata: Option<Json>) -> Result<(), Failure> {
    match external_ask(targets, metadata) {
        None => Ok(()),
        Some(ask) => context.assert(ask).await,
    }
}

/// [`authorize_external`] for the tool named `tool` (the plan agent's rewording of a denied edit applied).
pub async fn authorize_external_for(context: &Context, tool: &str, targets: &[Target], metadata: Option<Json>) -> Result<(), Failure> {
    match external_ask(targets, metadata) {
        None => Ok(()),
        Some(ask) => crate::session::plan::assert(context, tool, ask).await,
    }
}

/// `FileAccess.authorizeRead`: the external directory approved, then the read itself. A recovered file name
/// (`sibling_of`, in the same directory as the target it recovers) shares its directory approval but checks its own
/// read rules.
pub async fn authorize_read(context: &Context, file: &str, sibling_of: Option<&Target>) -> Result<Target, Failure> {
    let target = resolve(&context.location, file, sibling_of.map(|_| Kind::File)).await;
    let sibling = sibling_of.is_some_and(|other| dirname(&target.absolute) == dirname(&other.absolute));
    if !sibling {
        authorize_external(context, std::slice::from_ref(&target), None).await?;
    }
    context
        .assert(crate::permission::Ask {
            action: "read".into(),
            resources: vec![target.resource.clone()],
            save: Some(vec!["*".into()]),
            metadata: None,
        })
        .await?;
    Ok(target)
}
