//! What a file tool may touch (packages/core/src/file-access.ts): a path resolves against the location (with `~`
//! expanded) to a [`Target`] whose permission resource is location-relative when it is inside the location (or its
//! project's worktree) both lexically and after following symlinks, else absolute (its real path) with the external
//! directory it is in, which needs `external_directory` approval before the tool's own.
//!
//! The plan agent's rewrite of a denied edit (plugin/plan.ts, `tool.execute.after`) applies to every permission
//! check of the editing tools, so they assert through [`assert`].

use opencode_protocol::Json;

use super::{Context, Failure};
use crate::json;
use crate::permission::Ask;

/// The kind of target, which selects the external approval boundary (`FileAccess.Kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}

/// `ExternalDirectoryAuthorization`.
#[derive(Debug, Clone)]
pub struct External {
    /// `<directory>/*` of the directory used as the approval boundary.
    pub resource: String,
    pub save: String,
}

/// `FileAccess.Target`.
#[derive(Debug, Clone)]
pub struct Target {
    pub absolute: String,
    /// Location-relative for internal paths, absolute for external ones.
    pub resource: String,
    pub external: Option<External>,
}

/// `Global.Path.home`.
pub fn home() -> String {
    crate::inventory::config::Options::get().home.clone()
}

/// `FileAccess.resolvePath`: a leading `~` expanded, then `path.resolve(directory, input)`.
pub fn resolve_path(directory: &str, input: &str) -> String {
    let expanded = if input == "~" {
        home()
    } else if let Some(rest) = input.strip_prefix("~/") {
        format!("{}/{rest}", home())
    } else {
        input.to_owned()
    };
    crate::fs::resolve(directory, &expanded)
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

/// `path.join(a, b)` for an absolute `a`.
pub fn join(a: &str, b: &str) -> String {
    crate::project::normalize(&format!("{a}/{b}"))
}

/// `FSUtil.contains`.
pub fn contains(parent: &str, child: &str) -> bool {
    crate::project::contains(parent, child)
}

/// `FSUtil.resolve`: the real path, or the lexical one when it cannot be resolved.
pub fn fs_resolve(path: &str) -> String {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| crate::project::normalize(path))
}

/// `realPath` of file-access.ts: symlinks followed through the part of the path that exists.
pub fn real_path(target: &str) -> String {
    match std::fs::canonicalize(target) {
        Ok(real) => real.to_string_lossy().into_owned(),
        Err(_) => {
            let parent = dirname(target);
            if parent == target {
                return target.to_owned();
            }
            join(&real_path(&parent), crate::text::basename(target))
        }
    }
}

/// `Environment.typeFollowing`: the type of what a path names, a final symlink followed; `None` when missing.
pub fn type_following(path: &str) -> Option<&'static str> {
    let info = std::fs::metadata(path).ok()?;
    Some(if info.is_file() {
        "file"
    } else if info.is_dir() {
        "directory"
    } else {
        "other"
    })
}

/// `Environment.exists`.
fn exists(path: &str) -> bool {
    std::fs::metadata(path).is_ok()
}

/// `Project.root`: the nearest directory with a `.git` or `.hg`.
fn project_root(directory: &str) -> Option<String> {
    let mut current = directory.to_owned();
    loop {
        for marker in [".git", ".hg"] {
            if exists(&join(&current, marker)) {
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

/// `Wildcard.escape`.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '*' || c == '?' {
            out.push('[');
            out.push(c);
            out.push(']');
        } else {
            out.push(c);
        }
    }
    out
}

fn inside(target: &str, directory: &str, worktree: &str) -> bool {
    contains(directory, target) || (worktree != "/" && contains(worktree, target))
}

/// `FileAccess.resolve`: a lexical path and its permission resources, without asking for anything.
pub fn resolve(location: &crate::location::Location, path: &str, kind: Option<Kind>) -> Target {
    let absolute = resolve_path(&location.directory, path);
    let workspace = location.workspace_id.is_some();
    let real = |p: &str| if workspace { p.to_owned() } else { real_path(p) };
    let canonical = real(&absolute);
    let worktree = crate::project::normalize(&location.project.directory);
    let (real_directory, real_worktree) = (real(&location.directory), real(&worktree));
    if inside(&absolute, &location.directory, &worktree) && inside(&canonical, &real_directory, &real_worktree) {
        let relative = crate::text::relative(&location.directory, &absolute);
        return Target { absolute, resource: if relative.is_empty() { ".".into() } else { relative }, external: None };
    }
    let directory_kind = match kind {
        Some(kind) => kind == Kind::Directory,
        None => type_following(&absolute) == Some("directory"),
    };
    let directory = if directory_kind { canonical.clone() } else { dirname(&canonical) };
    let save = join(&escape(&project_root(&directory).unwrap_or_else(|| directory.clone())), "*");
    Target {
        absolute,
        resource: canonical,
        external: Some(External { resource: join(&directory, "*"), save }),
    }
}

/// The plan agent's `tool.execute.after` rewrite (plugin/plan.ts) of a permission the rules deny, for the editing
/// tools: a denial (`Permission.BlockedError`) becomes "Cannot use <tool> to modify files outside the Plan
/// directory".
fn rewrite(context: &Context, tool: &str, ask: &Ask, failure: Failure) -> Failure {
    let Failure::Error(error) = &failure else { return failure };
    if context.agent != crate::session::plan::AGENT || error.kind != "permission.rejected" {
        return failure;
    }
    if !matches!(tool, "edit" | "write" | "patch") {
        return failure;
    }
    // A rejection with feedback only comes after asking, which the rules allowed: what they deny was blocked.
    if context.assert_not_denied(ask.clone()).is_ok() {
        return failure;
    }
    Failure::Error(super::ToolError::new(crate::session::plan::blocked(tool)))
}

/// `permission.assert` for a file tool named `tool` (the plan agent's rewrite applied).
pub async fn assert(context: &Context, tool: &str, ask: Ask) -> Result<(), Failure> {
    match context.assert(ask.clone()).await {
        Ok(()) => Ok(()),
        Err(failure) => Err(rewrite(context, tool, &ask, failure)),
    }
}

/// `FileAccess.authorizeExternal`: the external directories of the targets approved in one request (first-seen
/// order, deduplicated by resource).
pub async fn authorize_external(context: &Context, tool: &str, targets: &[&Target], metadata: Option<Json>) -> Result<(), Failure> {
    let mut external: Vec<&External> = Vec::new();
    for target in targets {
        if let Some(item) = &target.external {
            if !external.iter().any(|seen| seen.resource == item.resource) {
                external.push(item);
            }
        }
    }
    if external.is_empty() {
        return Ok(());
    }
    let ask = Ask {
        action: "external_directory".into(),
        resources: external.iter().map(|item| item.resource.clone()).collect(),
        save: Some(external.iter().map(|item| item.save.clone()).collect()),
        metadata,
    };
    assert(context, tool, ask).await
}

/// `FileAccess.authorizeRead`: the external directory approved, then the read itself. A filename recovery
/// (`sibling_of`) shares the directory approval of the target it recovers but checks its own read rules.
pub async fn authorize_read(context: &Context, file: &str, sibling_of: Option<&Target>) -> Result<Target, Failure> {
    let target = resolve(&context.location, file, sibling_of.map(|_| Kind::File));
    let sibling = sibling_of.is_some_and(|other| dirname(&target.absolute) == dirname(&other.absolute));
    if !sibling {
        authorize_external(context, "read", &[&target], None).await?;
    }
    let ask = Ask { action: "read".into(), resources: vec![target.resource.clone()], save: Some(vec!["*".into()]), metadata: None };
    assert(context, "read", ask).await?;
    Ok(target)
}

/// An object of the given entries, leaving out the absent ones (as `JSON` drops `undefined`).
pub fn metadata(entries: Vec<(&str, Option<Json>)>) -> Json {
    let mut out = json::object();
    for (key, value) in entries {
        if let Some(value) = value {
            json::set(&mut out, key, value);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(dirname("/a/b/c"), "/a/b");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(dirname("/"), "/");
        assert_eq!(dirname("a/b"), "a");
        assert_eq!(dirname("a"), ".");
        assert_eq!(dirname("docs/report.txt"), "docs");
        assert_eq!(join("/", "*"), "/*");
        assert_eq!(escape("/a/b*?c"), "/a/b[*][?]c");
    }
}
