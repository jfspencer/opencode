//! References (packages/core/src/reference.ts, config/plugin/reference.ts): named directories or Git repositories
//! from config `references`, document by document (a later document's name replaces an earlier one in place). A
//! string starting with `.`, `/` or `~` (or an object with `path`) is a local directory, resolved against the
//! document's directory (the location's for `OPENCODE_CONFIG_CONTENT`); names with `/`, whitespace, `` ` `` or `,` are
//! skipped.
//!
//! TODO(port): Git references (listed under the repository cache once the repository parses as a remote, and cloned
//! in the background); none is listed here.

use opencode_protocol::types as t;
use opencode_protocol::{Json, Wire};

use super::config::{self, Entry};
use crate::json::Obj;

fn valid_alias(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c == '/' || c == '`' || c == ',' || config::js_space(c))
}

pub fn build(entries: &[Entry], location: &str, home: &str) -> Vec<t::ReferenceInfo> {
    // (name, source): `None` stands for a Git source, which keeps its place but is not listed.
    let mut sources: Vec<(String, Option<Json>)> = vec![];
    for entry in entries {
        let Entry::Document { path, info } = entry else { continue };
        let directory = path.as_deref().map(config::dirname).unwrap_or_else(|| location.to_owned());
        for (name, value) in info.references.iter().flat_map(|r| r.0.iter()) {
            if !valid_alias(name) {
                continue;
            }
            let (local, description, hidden) = match value {
                t::ConfigInfoReferencesValue::String(s) => {
                    ((s.starts_with('.') || s.starts_with('/') || s.starts_with('~')).then(|| s.clone()), None, None)
                }
                t::ConfigInfoReferencesValue::ConfigReferenceLocal(l) => (Some(l.path.clone()), l.description.clone(), l.hidden),
                t::ConfigInfoReferencesValue::ConfigReferenceGit(_) => (None, None, None),
            };
            let source = local.map(|value| {
                let path = match value.strip_prefix("~/") {
                    Some(rest) => config::join(home, rest),
                    None if value.starts_with('/') => value,
                    None => config::join(&directory, &value),
                };
                let mut o = Obj::new();
                o.str("type", "local").str("path", &path).opt_str("description", description.as_deref());
                o.opt("hidden", hidden.map(Json::Bool));
                o.done()
            });
            match sources.iter_mut().find(|(n, _)| n == name) {
                Some(existing) => existing.1 = source,
                None => sources.push((name.clone(), source)),
            }
        }
    }
    sources
        .into_iter()
        .filter_map(|(name, source)| {
            let source = source?;
            let mut o = Obj::new();
            o.str("name", &name).set("source", source.clone());
            o.opt("description", crate::json::get(&source, "description").cloned());
            o.opt("hidden", crate::json::get(&source, "hidden").cloned());
            o.opt("path", crate::json::get(&source, "path").cloned());
            t::ReferenceInfo::decode(&o.done()).ok()
        })
        .collect()
}
