//! References (packages/core/src/reference.ts, repository.ts, config/plugin/reference.ts): named directories or Git
//! repositories from config `references`, document by document (a later document's name replaces an earlier one in
//! place). A string starting with `.`, `/` or `~` (or an object with `path`) is a local directory, resolved against
//! the document's directory (the location's for `OPENCODE_CONFIG_CONTENT`); anything else names a Git repository,
//! listed under the repository cache (`$XDG_DATA_HOME/opencode/repos/<host>/<path>[@<branch>]`) when it parses as a
//! remote (`Repository.parse`; `file:` repositories and invalid branches are not listed). Names with `/`, whitespace,
//! `` ` `` or `,` are skipped.
//!
//! TODO(port): cloning Git references into the cache (background, daily refresh).

use opencode_protocol::types as t;
use opencode_protocol::{Json, Wire};

use super::config::{self, Entry};
use crate::json::Obj;

fn valid_alias(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c == '/' || c == '`' || c == ',' || config::js_space(c))
}

enum Source {
    Local { path: String },
    Git { repository: String, branch: Option<String> },
}

struct Reference {
    name: String,
    source: Source,
    description: Option<String>,
    hidden: Option<bool>,
}

pub fn build(entries: &[Entry], location: &str, home: &str, repos: &str) -> Vec<t::ReferenceInfo> {
    let mut references: Vec<Reference> = vec![];
    for entry in entries {
        let Entry::Document { path, info } = entry else { continue };
        let directory = path.as_deref().map(config::dirname).unwrap_or_else(|| location.to_owned());
        for (name, value) in info.references.iter().flat_map(|r| r.0.iter()) {
            if !valid_alias(name) {
                continue;
            }
            let local = |value: &str| {
                let path = match value.strip_prefix("~/") {
                    Some(rest) => config::join(home, rest),
                    None if value.starts_with('/') => value.to_owned(),
                    None => config::join(&directory, value),
                };
                Source::Local { path }
            };
            let (source, description, hidden) = match value {
                t::ConfigInfoReferencesValue::String(s) if s.starts_with('.') || s.starts_with('/') || s.starts_with('~') => {
                    (local(s), None, None)
                }
                t::ConfigInfoReferencesValue::String(s) => (Source::Git { repository: s.clone(), branch: None }, None, None),
                t::ConfigInfoReferencesValue::ConfigReferenceLocal(l) => (local(&l.path), l.description.clone(), l.hidden),
                t::ConfigInfoReferencesValue::ConfigReferenceGit(g) => (
                    Source::Git { repository: g.repository.clone(), branch: g.branch.clone() },
                    g.description.clone(),
                    g.hidden,
                ),
            };
            let reference = Reference { name: name.clone(), source, description, hidden };
            match references.iter_mut().find(|r| r.name == *name) {
                Some(existing) => *existing = reference,
                None => references.push(reference),
            }
        }
    }
    references
        .into_iter()
        .filter_map(|r| {
            let mut source = Obj::new();
            let path = match &r.source {
                Source::Local { path } => {
                    source.str("type", "local").str("path", path);
                    path.clone()
                }
                Source::Git { repository, branch } => {
                    source.str("type", "git").str("repository", repository).opt_str("branch", branch.as_deref());
                    let parsed = parse(repository)?;
                    if branch.as_deref().is_some_and(|b| !valid_branch(b)) {
                        return None;
                    }
                    let mut path = repos.to_owned();
                    for part in parsed.host.split(':').chain(parsed.segments.iter().map(String::as_str)) {
                        path = config::join(&path, part);
                    }
                    match branch {
                        Some(branch) => format!("{path}@{}", encode_uri_component(branch)),
                        None => path,
                    }
                }
            };
            let mut o = Obj::new();
            o.str("name", &r.name).set("source", source.done()).opt_str("description", r.description.as_deref());
            o.opt("hidden", r.hidden.map(Json::Bool)).str("path", &path);
            t::ReferenceInfo::decode(&o.done()).ok()
        })
        .collect()
}

/// `Repository.validateBranch`.
fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.chars().all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))
        && !branch.starts_with('-')
        && !branch.contains("..")
}

/// `encodeURIComponent`.
fn encode_uri_component(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A remote repository reference (`Repository.parse` when it is not a `file:` repository).
struct Remote {
    host: String,
    segments: Vec<String>,
}

fn trim_git(text: &str) -> &str {
    text.strip_suffix(".git").unwrap_or(text)
}

fn parts(text: &str) -> Vec<String> {
    text.split('/').map(|p| trim_git(config::js_trim(p)).to_owned()).filter(|p| !p.is_empty()).collect()
}

fn safe_segment(s: &str) -> bool {
    s != "." && s != ".." && !s.contains(':') && !s.chars().any(|c| c == '/' || c == '\\' || config::js_space(c))
}

fn build_remote(host: &str, segments: Vec<String>) -> Option<Remote> {
    let segments: Vec<String> = segments.iter().map(|s| trim_git(s).to_owned()).filter(|s| !s.is_empty()).collect();
    let safe_host = !host.is_empty() && !host.starts_with('-') && host.split(':').all(safe_segment);
    if !safe_host || segments.is_empty() || !segments.iter().all(|s| safe_segment(s)) {
        return None;
    }
    Some(Remote { host: host.to_lowercase(), segments })
}

/// `Repository.parse` for what the reference listing needs (host and path segments of a remote).
fn parse(input: &str) -> Option<Remote> {
    let mut cleaned = config::js_trim(input).strip_prefix("git+").unwrap_or(config::js_trim(input)).to_owned();
    if let Some(hash) = cleaned.find('#') {
        cleaned.truncate(hash);
    }
    let cleaned = cleaned.trim_end_matches('/');
    if cleaned.is_empty() {
        return None;
    }
    if let Some(rest) = cleaned.strip_prefix("github:") {
        let mut split = rest.splitn(2, '/');
        let (owner, repo) = (split.next()?, split.next()?);
        let plain = |s: &str| !s.is_empty() && !s.chars().any(|c| c == '/' || config::js_space(c));
        if plain(owner) && plain(repo) {
            return build_remote("github.com", vec![owner.to_owned(), repo.to_owned()]);
        }
    }
    if !cleaned.contains("://") {
        // scp-like `[user@]host:path`.
        let rest = match cleaned.find('@') {
            Some(at) if !cleaned[..at].contains('/') && !cleaned[..at].chars().any(config::js_space) && at > 0 => &cleaned[at + 1..],
            _ => cleaned,
        };
        if let Some(colon) = rest.find(':') {
            let host = &rest[..colon];
            if !host.is_empty() && !host.contains('/') && !host.chars().any(config::js_space) && colon + 1 < rest.len() {
                return build_remote(host, parts(&rest[colon + 1..]));
            }
        }
        let direct = parts(cleaned);
        let host_like = |s: &str| s.contains('.') || s.contains(':') || s == "localhost";
        if direct.len() >= 2 && host_like(&direct[0]) {
            return build_remote(&direct[0], direct[1..].to_vec());
        }
        if direct.len() == 2 {
            return build_remote("github.com", direct);
        }
    }
    // A URL: `scheme://[user@]host[:port]/path`.
    let (scheme, rest) = cleaned.split_once("://")?;
    let valid_scheme = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
    if !valid_scheme || scheme.eq_ignore_ascii_case("file") {
        return None;
    }
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let path = rest[end..].split('?').next().unwrap_or("");
    build_remote(host, parts(path))
}
