//! npm-package-arg 13.0.2 (`npa`, `npa.resolve`) for the specs a dependency can have: registry versions, ranges and
//! tags, `npm:` aliases, remote tarball URLs, Git (URLs, scp-style, hosted shortcuts) and files or directories. A Git
//! spec on a hosting service (hosted-git-info's github, gitlab, bitbucket, gist and sourcehut forms) is recognized
//! as such, but its parts are not taken apart: installing one needs that host (see `npm.rs`).

use super::semver::{self, Options};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Version,
    Range,
    Tag,
    /// `npm:<name>@<spec>`: the aliased registry spec.
    Alias(Box<Spec>),
    /// An `http(s):` tarball URL.
    Remote,
    /// A Git repository: `fetch_spec` is the URL to clone (`None` for a hosted shortcut); a hosted one is
    /// `hosted`.
    Git { hosted: bool, committish: Option<String>, range: Option<String>, subdir: Option<String> },
    /// A tarball file (`.tgz`, `.tar.gz`, `.tar`) or a directory, `fetch_spec` its absolute path.
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub name: Option<String>,
    pub raw: String,
    pub raw_spec: String,
    pub fetch_spec: Option<String>,
    pub save_spec: Option<String>,
    pub kind: Kind,
}

impl Spec {
    /// `registry`: registry specs and aliases of them.
    pub fn registry(&self) -> bool {
        matches!(self.kind, Kind::Version | Kind::Range | Kind::Tag | Kind::Alias(_))
    }
}

/// `validatePackageName(name).validForOldPackages`.
pub fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('.') || name.starts_with('-') || name.starts_with('_') {
        return false;
    }
    if opencode_protocol::wire::number::js_trim(name) != name {
        return false;
    }
    if ["node_modules", "favicon.ico"].iter().any(|x| name.to_lowercase() == *x) {
        return false;
    }
    if encode_uri_component(name) == name {
        return true;
    }
    // `^(?:@([^/]+?)[/])?([^/]+?)$`
    let (user, pkg) = match name.strip_prefix('@').and_then(|rest| rest.split_once('/')) {
        Some((user, pkg)) if !user.is_empty() && !pkg.is_empty() && !pkg.contains('/') => (Some(user), pkg),
        _ if !name.contains('/') => (None, name),
        _ => return false,
    };
    if pkg.starts_with('.') {
        return false;
    }
    match user {
        Some(user) => encode_uri_component(user) == user && encode_uri_component(pkg) == pkg,
        None => false,
    }
}

/// `encodeURIComponent` (on well-formed strings).
pub fn encode_uri_component(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn is_url(text: &str) -> bool {
    // `/^(?:git[+])?[a-z]+:/i`
    let rest = if text.len() >= 4 && text[..4].eq_ignore_ascii_case("git+") { &text[4..] } else { text };
    let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    letters > 0 && rest.as_bytes().get(letters) == Some(&b':')
}

/// `/^[^@]+@[^:.]+\.[^:]+:.+$/i`
fn is_scp_git(text: &str) -> bool {
    let Some((user, rest)) = text.split_once('@') else { return false };
    if user.is_empty() {
        return false;
    }
    let Some((host, path)) = rest.split_once(':') else { return false };
    let Some((first, after)) = host.split_once('.') else { return false };
    !first.is_empty() && !first.contains('@') && !after.is_empty() && !path.is_empty()
}

fn is_file_type(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.ends_with(".tgz") || lower.ends_with(".tar.gz") || lower.ends_with(".tar")
}

/// `npa(arg, where)`.
pub fn npa(arg: &str, where_: &str) -> Result<Spec, String> {
    let name_end = arg.get(1..).and_then(|rest| rest.find('@')).map(|i| i + 1);
    let name_part = match name_end {
        Some(end) => &arg[..end],
        None => arg,
    };
    let (name, spec) = if is_url(arg) {
        (None, arg.to_owned())
    } else if is_scp_git(arg) {
        (None, format!("git+ssh://{arg}"))
    } else if !name_part.starts_with('@') && (name_part.contains('/') || is_file_type(name_part)) {
        (None, arg.to_owned())
    } else if let Some(end) = name_end {
        let spec = &arg[end + 1..];
        (Some(name_part.to_owned()), if spec.is_empty() { "*".to_owned() } else { spec.to_owned() })
    } else if valid_name(arg) {
        (Some(arg.to_owned()), "*".to_owned())
    } else {
        (None, arg.to_owned())
    };
    resolve_with(name.as_deref(), &spec, where_, Some(arg))
}

/// `npa.resolve(name, spec, where)`.
pub fn resolve(name: &str, spec: &str, where_: &str) -> Result<Spec, String> {
    resolve_with(Some(name), spec, where_, None)
}

fn resolve_with(name: Option<&str>, spec: &str, where_: &str, arg: Option<&str>) -> Result<Spec, String> {
    let raw = match arg {
        Some(arg) => arg.to_owned(),
        None => match name {
            Some(name) => format!("{name}@{spec}"),
            None => spec.to_owned(),
        },
    };
    if let Some(name) = name {
        if !valid_name(name) {
            return Err(format!("Invalid package name \"{name}\" of package \"{raw}\""));
        }
    }
    let base = Spec {
        name: name.map(str::to_owned),
        raw: raw.clone(),
        raw_spec: spec.to_owned(),
        fetch_spec: None,
        save_spec: None,
        kind: Kind::Tag,
    };
    let lower = spec.to_ascii_lowercase();
    if lower.starts_with("file:") || spec.starts_with('.') || spec.starts_with("~/") || spec.starts_with('/') || is_drive(spec) {
        return from_file(base, where_);
    }
    if lower.starts_with("npm:") {
        let sub = npa(&spec[4..], where_)?;
        if matches!(sub.kind, Kind::Alias(_)) {
            return Err("nested aliases not supported".into());
        }
        if !sub.registry() {
            return Err("aliases only work for registry deps".into());
        }
        if sub.name.is_none() {
            return Err("aliases must have a name".into());
        }
        return Ok(Spec { kind: Kind::Alias(Box::new(sub)), ..base });
    }
    if let Some(hosted) = hosted(spec) {
        return Ok(Spec { kind: Kind::Git { hosted: true, committish: hosted, range: None, subdir: None }, ..base });
    }
    if !spec.is_empty() && is_url(spec) {
        return from_url(base);
    }
    if !spec.is_empty() && (spec.contains('/') || is_file_type(spec)) {
        return from_file(base, where_);
    }
    from_registry(base)
}

fn is_drive(spec: &str) -> bool {
    let b = spec.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// hosted-git-info's `fromUrl`: `Some(committish)` for a spec on a known Git host.
fn hosted(spec: &str) -> Option<Option<String>> {
    let (body, committish) = match spec.split_once('#') {
        Some((body, hash)) => (body, (!hash.is_empty()).then(|| hash.to_owned())),
        None => (spec, None),
    };
    let lower = body.to_ascii_lowercase();
    for shortcut in ["github:", "gitlab:", "bitbucket:", "gist:", "sourcehut:"] {
        if lower.starts_with(shortcut) {
            return Some(committish);
        }
    }
    // `user/repo`: a GitHub shortcut.
    if !is_url(body) && !body.starts_with('@') && !body.contains(':') {
        let parts: Vec<&str> = body.split('/').collect();
        if parts.len() == 2 && parts.iter().all(|p| !p.is_empty() && !p.contains(' ')) && !body.starts_with('.') {
            return Some(committish);
        }
    }
    const HOSTS: [&str; 5] = ["github.com", "gitlab.com", "bitbucket.org", "gist.github.com", "git.sr.ht"];
    let host_of = |rest: &str| -> String {
        let rest = rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest);
        rest.split([':', '/']).next().unwrap_or("").to_ascii_lowercase().trim_start_matches("www.").to_owned()
    };
    if let Some((_, rest)) = lower.split_once("://") {
        let authority = rest.split('/').next().unwrap_or("");
        if HOSTS.contains(&host_of(authority).as_str()) {
            return Some(committish);
        }
    } else if is_scp_git(body) {
        let host = body.split_once('@').map(|(_, rest)| rest.split(':').next().unwrap_or("")).unwrap_or("");
        if HOSTS.contains(&host.to_ascii_lowercase().as_str()) {
            return Some(committish);
        }
    }
    None
}

fn from_registry(mut spec: Spec) -> Result<Spec, String> {
    let fetch = opencode_protocol::wire::number::js_trim(&spec.raw_spec).to_owned();
    spec.kind = if semver::valid(&fetch, Options::LOOSE).is_some() {
        Kind::Version
    } else if semver::valid_range(&fetch, Options::LOOSE).is_some() {
        Kind::Range
    } else {
        if encode_uri_component(&fetch) != fetch {
            return Err(format!("Invalid tag name \"{fetch}\" of package \"{}\"", spec.raw));
        }
        Kind::Tag
    };
    spec.fetch_spec = Some(fetch);
    Ok(spec)
}

fn from_url(mut spec: Spec) -> Result<Spec, String> {
    let raw = spec.raw_spec.clone();
    spec.save_spec = Some(raw.clone());
    let protocol = raw.split_once(':').map(|(p, _)| p.to_ascii_lowercase()).unwrap_or_default();
    match protocol.as_str() {
        "http" | "https" => {
            spec.kind = Kind::Remote;
            spec.fetch_spec = Some(raw);
            Ok(spec)
        }
        "git" | "git+http" | "git+https" | "git+rsync" | "git+ftp" | "git+file" | "git+ssh" => {
            let (url, hash) = match raw.split_once('#') {
                Some((url, hash)) => (url.to_owned(), hash.to_owned()),
                None => (raw.clone(), String::new()),
            };
            let (committish, range, subdir) = git_attrs(&hash)?;
            let fetch = match url.strip_prefix("git+") {
                Some(rest) => rest.to_owned(),
                None => url,
            };
            spec.fetch_spec = Some(fetch);
            spec.kind = Kind::Git { hosted: false, committish, range, subdir };
            Ok(spec)
        }
        other => Err(format!("Unsupported URL Type \"{other}:\": {raw}")),
    }
}

/// `setGitAttrs`: `#committish`, `#semver:<range>`, `#path:<dir>`, joined by `::`.
fn git_attrs(hash: &str) -> Result<(Option<String>, Option<String>, Option<String>), String> {
    let (mut committish, mut range, mut subdir) = (None, None, None);
    if hash.is_empty() {
        return Ok((None, None, None));
    }
    for part in hash.split("::") {
        match part.split_once(':') {
            None => {
                if range.is_some() {
                    return Err("cannot override existing semver range with a committish".into());
                }
                if committish.is_some() {
                    return Err("cannot override existing committish with a second committish".into());
                }
                committish = Some(part.to_owned());
            }
            Some(("semver", value)) => {
                if committish.is_some() {
                    return Err("cannot override existing committish with a semver range".into());
                }
                if range.is_some() {
                    return Err("cannot override existing semver range with a second semver range".into());
                }
                let value = value.split(':').next().unwrap_or("");
                range = Some(crate::text::decode_uri_component(value).ok_or("URIError: URI malformed")?);
            }
            Some(("path", value)) => {
                if subdir.is_some() {
                    return Err("cannot override existing path with a second path".into());
                }
                subdir = Some(format!("/{}", value.split(':').next().unwrap_or("")));
            }
            Some(_) => {}
        }
    }
    Ok((committish, range, subdir))
}

fn from_file(mut spec: Spec, where_: &str) -> Result<Spec, String> {
    spec.kind = if is_file_type(&spec.raw_spec) { Kind::File } else { Kind::Directory };
    let raw = spec.raw_spec.strip_prefix("file:").or_else(|| spec.raw_spec.strip_prefix("FILE:")).unwrap_or(&spec.raw_spec);
    let raw = raw.trim_start_matches("//");
    let path = if let Some(rest) = raw.strip_prefix("~/") {
        spec.save_spec = Some(format!("file:~/{rest}"));
        format!("{}/{rest}", crate::file_access::home())
    } else if raw.starts_with('/') {
        let resolved = crate::project::normalize(raw);
        spec.save_spec = Some(format!("file:{resolved}"));
        resolved
    } else {
        let resolved = crate::project::normalize(&format!("{where_}/{raw}"));
        spec.save_spec = Some(format!("file:{}", crate::text::relative(where_, &resolved)));
        resolved
    };
    spec.fetch_spec = Some(path);
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        let kind = |arg: &str| npa(arg, "/w").map(|s| s.kind);
        assert_eq!(kind("prettier").unwrap(), Kind::Range);
        assert_eq!(kind("a@1.2.3").unwrap(), Kind::Version);
        assert_eq!(kind("a@latest").unwrap(), Kind::Tag);
        assert_eq!(kind("@s/a@^1").unwrap(), Kind::Range);
        assert!(matches!(kind("a@npm:b@^1").unwrap(), Kind::Alias(_)));
        assert_eq!(kind("a@https://x.test/a.tgz").unwrap(), Kind::Remote);
        assert!(matches!(kind("a@github:x/y").unwrap(), Kind::Git { hosted: true, .. }));
        assert!(matches!(kind("a@x/y").unwrap(), Kind::Git { hosted: true, .. }));
        assert!(matches!(kind("a@git+file:///r#main").unwrap(), Kind::Git { hosted: false, .. }));
        assert_eq!(kind("a@file:../b").unwrap(), Kind::Directory);
        assert!(kind("a@not a tag").is_err());
    }
}
