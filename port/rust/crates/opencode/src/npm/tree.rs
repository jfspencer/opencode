//! @npmcli/arborist 9.4.0's ideal tree for one added package (`buildIdealTree` with `add: [pkg]` into an empty root,
//! as `Npm.add` runs it): the `Node`/`Edge` graph (node.js, edge.js, dep-valid.js), the dependency queue and its
//! problem edges, virtual roots and peer sets (build-ideal-tree.js), placement (place-dep.js, can-place-dep.js:
//! hoisting as high as no conflict forbids, KEEP / REPLACE / CONFLICT, nesting under the dependent, dedupe pruning,
//! cycle links), the prefetch of a placed node's problem edges, the dependency flags (calc-dep-flags.js), failed
//! optional dependencies and the engine and platform checks.
//!
//! Nodes live in an arena; a node's tree is its `root`. Edges are evaluated lazily (`to` is the name resolved from
//! the dependent upward, `error` from that), which is what arborist's eager reloads keep its cached edges equal to.
//!
//! TODO(port): peer groups that conflict through a cycle of peer dependencies: arborist can fail such an install
//! (ERESOLVE) where this places the tree without the conflicted dependency (`harness/npm/run.sh` with seed 2 shows
//! one such case in 60; the formatters' real packages have no peer dependencies).

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use indexmap::IndexMap;
use opencode_protocol::Json;

use super::pick::{self, truthy};
use super::semver::{self, Options};
use super::spec::{self, Kind, Spec};

pub type Id = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Prod,
    Optional,
    Peer,
    PeerOptional,
}

impl EdgeKind {
    pub fn peer(self) -> bool {
        matches!(self, EdgeKind::Peer | EdgeKind::PeerOptional)
    }
    pub fn optional(self) -> bool {
        matches!(self, EdgeKind::Optional | EdgeKind::PeerOptional)
    }
}

#[derive(Debug, Clone)]
pub struct EdgeData {
    pub kind: EdgeKind,
    pub spec: String,
    pub accept: Option<String>,
    pub peer_conflicted: bool,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub package: Json,
    pub path: String,
    pub parent: Option<Id>,
    pub children: IndexMap<String, Id>,
    root: Id,
    pub resolved: Option<String>,
    pub integrity: Option<String>,
    pub errors: Vec<String>,
    /// A Link's target.
    pub target: Option<Id>,
    pub source_reference: Option<Id>,
    pub edges: IndexMap<String, EdgeData>,
    pub dev: bool,
    pub optional: bool,
    pub dev_optional: bool,
    pub peer: bool,
    pub extraneous: bool,
    pub inert: bool,
    pub has_shrinkwrap: bool,
}

/// What the tree's install options decide (the flattened npm config arborist gets).
#[derive(Debug, Clone, Default)]
pub struct TreeOptions {
    pub legacy_peer_deps: bool,
    pub strict_peer_deps: bool,
    pub force: bool,
    pub prefer_dedupe: bool,
    /// `hoisted` (default), `nested`, `shallow`.
    pub install_strategy: String,
    pub engine_strict: bool,
    pub omit: Vec<String>,
    pub os: Option<String>,
    pub cpu: Option<String>,
    pub libc: Option<String>,
}

/// The manifests a tree is built from (pacote.manifest through the registry, with arborist's options).
pub trait Source: Sync {
    fn manifest<'a>(&'a self, spec: &'a Spec) -> Pin<Box<dyn Future<Output = Result<Json, String>> + Send + 'a>>;
}

#[derive(Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
}

fn object<'a>(value: Option<&'a Json>) -> Option<&'a IndexMap<String, Json>> {
    match value {
        Some(Json::Object(map)) => Some(map),
        _ => None,
    }
}

fn get<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    match value {
        Json::Object(map) => map.get(key),
        _ => None,
    }
}

/// `path.relative` for absolute, normalized paths.
pub fn relpath(from: &str, to: &str) -> String {
    crate::text::relative(from, to)
}

pub fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    crate::collate::compare(a, b)
}

impl Tree {
    /// `new Node({ name, pkg, parent | path, ... })`: dependency flags start set (a root starts with them unset).
    pub fn add(&mut self, name: &str, package: Json, parent: Option<Id>, path: Option<&str>, legacy_peer_deps: bool) -> Id {
        let id = self.nodes.len();
        let resolved = get(&package, "_resolved").and_then(|r| match r {
            Json::String(r) => consistent_resolve(r),
            _ => None,
        });
        let integrity = match get(&package, "_integrity") {
            Some(Json::String(i)) if !i.is_empty() => Some(i.clone()),
            _ => None,
        };
        let has_shrinkwrap = truthy(get(&package, "_hasShrinkwrap"));
        let path = path.map(str::to_owned).unwrap_or_default();
        self.nodes.push(Node {
            name: name.to_owned(),
            package,
            path,
            parent: None,
            children: IndexMap::new(),
            root: id,
            resolved,
            integrity,
            errors: Vec::new(),
            target: None,
            source_reference: None,
            edges: IndexMap::new(),
            dev: true,
            optional: true,
            dev_optional: true,
            peer: true,
            extraneous: true,
            inert: false,
            has_shrinkwrap,
        });
        self.load_deps(id, legacy_peer_deps);
        if let Some(parent) = parent {
            self.set_parent(id, parent);
        }
        id
    }

    /// `[_loadDeps]`: peer, peerOptional, prod and optional edges, in that order (a later one of the same name
    /// replaces an earlier one and moves to the end).
    fn load_deps(&mut self, id: Id, legacy_peer_deps: bool) {
        let package = self.nodes[id].package.clone();
        let accept = object(get(&package, "acceptDependencies")).cloned().unwrap_or_default();
        let add = |tree: &mut Tree, deps: Vec<(String, Json)>, kind: EdgeKind| {
            for (name, spec) in deps {
                // `new Edge` requires a string spec; anything else throws (the manifest cannot be used).
                let Json::String(spec) = spec else { continue };
                let accept = match accept.get(&name) {
                    Some(Json::String(a)) => Some(if a.is_empty() { "*".to_owned() } else { a.clone() }),
                    _ => None,
                };
                tree.nodes[id].edges.shift_remove(&name);
                tree.nodes[id].edges.insert(name, EdgeData { kind, spec, accept, peer_conflicted: false });
            }
        };
        if let Some(peers) = object(get(&package, "peerDependencies")).filter(|_| !legacy_peer_deps) {
            let meta = object(get(&package, "peerDependenciesMeta")).cloned().unwrap_or_default();
            let (mut required, mut optional) = (Vec::new(), Vec::new());
            for (name, spec) in peers {
                let is_optional = meta.get(name).and_then(|m| get(m, "optional")).is_some_and(|v| truthy(Some(v)));
                if is_optional { optional.push((name.clone(), spec.clone())) } else { required.push((name.clone(), spec.clone())) }
            }
            add(self, required, EdgeKind::Peer);
            add(self, optional, EdgeKind::PeerOptional);
        }
        let deps = |key: &str| object(get(&package, key)).map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        add(self, deps("dependencies"), EdgeKind::Prod);
        add(self, deps("optionalDependencies"), EdgeKind::Optional);
    }

    pub fn root(&self, id: Id) -> Id {
        self.nodes[id].root
    }

    pub fn is_link(&self, id: Id) -> bool {
        self.nodes[id].target.is_some()
    }

    pub fn is_top(&self, id: Id) -> bool {
        self.nodes[id].parent.is_none()
    }

    pub fn is_project_root(&self, id: Id) -> bool {
        id == self.root(id)
    }

    pub fn depth(&self, id: Id) -> usize {
        match self.nodes[id].parent {
            None => 0,
            Some(parent) => self.depth(parent) + 1,
        }
    }

    pub fn location(&self, id: Id) -> String {
        relpath(&self.nodes[self.root(id)].path, &self.nodes[id].path)
    }

    pub fn version(&self, id: Id) -> String {
        match get(&self.nodes[id].package, "version") {
            Some(Json::String(v)) => v.clone(),
            Some(v) if truthy(Some(v)) => pick::js_string(v),
            _ => String::new(),
        }
    }

    pub fn package_name(&self, id: Id) -> Option<String> {
        match get(&self.nodes[id].package, "name") {
            Some(Json::String(n)) if !n.is_empty() => Some(n.clone()),
            _ => None,
        }
    }

    /// `resolve(name)`: the child of that name here or up the tree.
    pub fn resolve(&self, from: Id, name: &str) -> Option<Id> {
        let mut at = Some(from);
        while let Some(node) = at {
            if let Some(child) = self.nodes[node].children.get(name) {
                return Some(*child);
            }
            at = self.nodes[node].parent;
        }
        None
    }

    pub fn ancestry(&self, id: Id) -> Vec<Id> {
        let mut out = vec![id];
        let mut at = self.nodes[id].parent;
        while let Some(node) = at {
            out.push(node);
            at = self.nodes[node].parent;
        }
        out
    }

    pub fn is_descendant_of(&self, id: Id, of: Id) -> bool {
        self.ancestry(id).contains(&of)
    }

    fn descendants(&self, id: Id) -> Vec<Id> {
        let mut out = vec![id];
        let mut index = 0;
        while index < out.len() {
            let node = out[index];
            out.extend(self.nodes[node].children.values().copied());
            index += 1;
        }
        out
    }

    /// The nodes of a root's tree (its inventory), in creation order.
    pub fn inventory(&self, root: Id) -> Vec<Id> {
        (0..self.nodes.len()).filter(|id| self.nodes[*id].root == root).collect()
    }

    fn set_root_of(&mut self, id: Id, root: Id) {
        for node in self.descendants(id) {
            self.nodes[node].root = root;
        }
    }

    fn refresh_paths(&mut self, id: Id) {
        for node in self.descendants(id) {
            if let Some(parent) = self.nodes[node].parent {
                let path = format!("{}/node_modules/{}", self.nodes[parent].path, self.nodes[node].name);
                self.nodes[node].path = path;
            }
        }
    }

    /// `node.parent = parent`: the node (and what is under it) moves to `parent/node_modules/<name>`, replacing a
    /// node already there.
    pub fn set_parent(&mut self, id: Id, parent: Id) {
        let parent = self.nodes[parent].target.unwrap_or(parent);
        if id == parent || self.nodes[id].parent == Some(parent) {
            return;
        }
        if let Some(old) = self.nodes[id].parent.take() {
            let name = self.nodes[id].name.clone();
            if self.nodes[old].children.get(&name) == Some(&id) {
                self.nodes[old].children.shift_remove(&name);
            }
        }
        let name = self.nodes[id].name.clone();
        if let Some(current) = self.nodes[parent].children.get(&name).copied() {
            if current != id {
                self.detach(current);
            }
        }
        self.nodes[parent].children.insert(name, id);
        self.nodes[id].parent = Some(parent);
        let root = self.root(parent);
        self.set_root_of(id, root);
        self.refresh_paths(id);
    }

    /// `node.root = null`: out of its tree, with what is under it.
    pub fn detach(&mut self, id: Id) {
        if let Some(parent) = self.nodes[id].parent.take() {
            let name = self.nodes[id].name.clone();
            if self.nodes[parent].children.get(&name) == Some(&id) {
                self.nodes[parent].children.shift_remove(&name);
            }
        }
        self.set_root_of(id, id);
    }

    pub fn edge(&self, from: Id, name: &str) -> Option<&EdgeData> {
        self.nodes[from].edges.get(name)
    }

    pub fn edge_to(&self, from: Id, name: &str) -> Option<Id> {
        self.edge(from, name)?;
        self.resolve(from, name)
    }

    /// `edge.error`: `MISSING`, `PEER LOCAL`, `INVALID`, or `None` when valid.
    pub fn edge_error(&self, from: Id, name: &str) -> Option<&'static str> {
        let edge = self.edge(from, name)?;
        match self.resolve(from, name) {
            None => (!edge.kind.optional()).then_some("MISSING"),
            Some(to) => {
                if edge.kind.peer() && self.nodes[to].parent == Some(from) && !self.is_top(from) {
                    Some("PEER LOCAL")
                } else if !self.satisfied_by(from, name, to) {
                    Some("INVALID")
                } else {
                    None
                }
            }
        }
    }

    pub fn edge_valid(&self, from: Id, name: &str) -> bool {
        self.edge_error(from, name).is_none()
    }

    /// `edge.satisfiedBy(node)`.
    pub fn satisfied_by(&self, from: Id, name: &str, node: Id) -> bool {
        let Some(edge) = self.edge(from, name) else { return false };
        if self.nodes[node].name != name {
            return false;
        }
        dep_valid(self, node, &edge.spec, edge.accept.as_deref(), from)
    }

    /// `node.satisfies(edge)`.
    pub fn satisfies(&self, node: Id, from: Id, name: &str) -> bool {
        self.nodes[node].name == name && self.satisfied_by(from, name, node)
    }

    /// `edgesIn`: the edges of the node's tree that resolve to it, as (from, name).
    pub fn edges_in(&self, id: Id) -> Vec<(Id, String)> {
        let root = self.root(id);
        let mut out = Vec::new();
        for from in self.inventory(root) {
            if self.is_link(from) {
                continue;
            }
            for name in self.nodes[from].edges.keys() {
                if self.resolve(from, name) == Some(id) {
                    out.push((from, name.clone()));
                }
            }
        }
        out
    }

    /// `matches(node)`.
    pub fn matches(&self, a: Id, b: Id) -> bool {
        if a == b {
            return true;
        }
        let (x, y) = (&self.nodes[a], &self.nodes[b]);
        if x.name != y.name {
            return false;
        }
        if let Some(target) = x.target {
            return y.target.is_some_and(|other| self.matches(target, other));
        }
        if self.is_project_root(a) && self.is_project_root(b) {
            return x.path == y.path;
        }
        if let (Some(i), Some(j)) = (&x.integrity, &y.integrity) {
            return i == j;
        }
        if let (Some(i), Some(j)) = (&x.resolved, &y.resolved) {
            return i == j;
        }
        let (pa, pb) = (self.package_name(a), self.package_name(b));
        let (va, vb) = (self.version(a), self.version(b));
        pa.is_some() && pa == pb && !va.is_empty() && va == vb
    }

    /// `gatherDepSet(set, edgeFilter)`.
    pub fn gather_dep_set(&self, set: &[Id], filter: &dyn Fn(&Tree, Id, &str) -> bool) -> Vec<Id> {
        let mut deps: Vec<Id> = Vec::new();
        for id in set {
            if !deps.contains(id) {
                deps.push(*id);
            }
        }
        let mut index = 0;
        while index < deps.len() {
            let node = deps[index];
            for name in self.nodes[node].edges.keys() {
                if let Some(to) = self.resolve(node, name) {
                    if filter(self, node, name) && !deps.contains(&to) {
                        deps.push(to);
                    }
                }
            }
            index += 1;
        }
        let mut changed = true;
        while changed && !deps.is_empty() {
            changed = false;
            let mut index = 0;
            while index < deps.len() {
                let dep = deps[index];
                let outside = self.edges_in(dep).into_iter().any(|(from, name)| !deps.contains(&from) && filter(self, from, &name));
                if outside {
                    changed = true;
                    deps.remove(index);
                } else {
                    index += 1;
                }
            }
        }
        deps
    }

    /// `canReplaceWith(node, ignorePeers)`: `this` may be replaced by `node`.
    pub fn can_replace_with(&self, this: Id, node: Id, ignore_peers: &[String]) -> bool {
        if self.nodes[node].name != self.nodes[this].name || self.package_name(node) != self.package_name(this) {
            return false;
        }
        let dep_set = self.gather_dep_set(&[this], &|t, from, name| t.resolve(from, name) != Some(this) && t.edge_valid(from, name));
        for (from, name) in self.edges_in(this) {
            let edge = self.edge(from, &name).unwrap();
            if !self.is_top(this)
                && self.nodes[from].parent == self.nodes[this].parent
                && edge.kind.peer()
                && ignore_peers.contains(&self.nodes[from].name)
            {
                continue;
            }
            if !dep_set.contains(&from) && !self.satisfied_by(from, &name, node) {
                return false;
            }
        }
        true
    }

    /// `canDedupe(preferDedupe, explicitRequest)`.
    pub fn can_dedupe(&self, id: Id, prefer_dedupe: bool, explicit_request: bool) -> bool {
        let Some(parent) = self.nodes[id].parent else { return false };
        let Some(grand) = self.nodes[parent].parent else { return false };
        if self.edges_in(id).is_empty() {
            return true;
        }
        let Some(other) = self.resolve(grand, &self.nodes[id].name) else { return false };
        if self.matches(other, id) {
            return true;
        }
        if !self.can_replace_with(id, other, &[]) {
            return false;
        }
        let (ov, tv) = (self.version(other), self.version(id));
        let eq = semver::compare(&ov, &tv, Options::STRICT).map(|c| c == 0).unwrap_or(false);
        if prefer_dedupe || eq {
            return true;
        }
        if semver::compare(&ov, &tv, Options::STRICT).map(|c| c > 0).unwrap_or(false) {
            return true;
        }
        explicit_request
    }

    /// `replace(node)`: this node takes `old`'s place (and its children).
    pub fn replace(&mut self, this: Id, old: Id) {
        let kids: Vec<Id> = self.nodes[old].children.values().copied().collect();
        for kid in kids {
            self.set_parent(kid, this);
        }
        match self.nodes[old].parent {
            Some(parent) => {
                self.detach(old);
                self.set_parent(this, parent);
            }
            None => self.detach(old),
        }
    }

    /// `deepestNestingTarget(start, name)`.
    pub fn deepest_nesting_target(&self, start: Id, name: &str) -> Id {
        for target in self.ancestry(start) {
            if self.is_project_root(target) || self.nodes[target].parent.is_none() {
                return target;
            }
            match self.edge(target, name) {
                Some(edge) if edge.kind.peer() => {}
                _ => return target,
            }
        }
        start
    }

    /// `peerEntrySets(node)`: each entry edge into the node's peer group, with the group it brings in.
    pub fn peer_entry_sets(&self, node: Id) -> Vec<((Id, String), Vec<Id>)> {
        let mut union = vec![node];
        let mut index = 0;
        while index < union.len() {
            let at = union[index];
            for name in self.nodes[at].edges.keys() {
                let edge = &self.nodes[at].edges[name];
                if edge.kind.peer() && self.edge_valid(at, name) {
                    if let Some(to) = self.resolve(at, name) {
                        if !union.contains(&to) {
                            union.push(to);
                        }
                    }
                }
            }
            for (from, name) in self.edges_in(at) {
                if self.edge(from, &name).unwrap().kind.peer() && self.edge_valid(from, &name) && !union.contains(&from) {
                    union.push(from);
                }
            }
            index += 1;
        }
        let mut sets: Vec<((Id, String), Vec<Id>)> = Vec::new();
        for peer in union {
            for (from, name) in self.edges_in(peer) {
                if !self.edge_valid(from, &name) {
                    continue;
                }
                if !self.edge(from, &name).unwrap().kind.peer() || self.is_top(from) {
                    let mut sub = vec![peer];
                    let mut i = 0;
                    while i < sub.len() {
                        let at = sub[i];
                        for n in self.nodes[at].edges.keys() {
                            if self.nodes[at].edges[n].kind.peer() && self.edge_valid(at, n) {
                                if let Some(to) = self.resolve(at, n) {
                                    if !sub.contains(&to) {
                                        sub.push(to);
                                    }
                                }
                            }
                        }
                        i += 1;
                    }
                    if sub.contains(&node) {
                        sets.retain(|(key, _)| key != &(from, name.clone()));
                        sets.push(((from, name.clone()), sub));
                    }
                }
            }
        }
        sets
    }

    /// `optionalSet(node)`: what failing to install an optional node takes out with it.
    pub fn optional_set(&self, node: Id) -> Vec<Id> {
        let mut set = vec![node];
        let mut index = 0;
        while index < set.len() {
            let at = set[index];
            for (from, name) in self.edges_in(at) {
                if !self.edge(from, &name).unwrap().kind.optional() && !set.contains(&from) {
                    set.push(from);
                }
            }
            index += 1;
        }
        let members = set.clone();
        self.gather_dep_set(&set, &|t, from, name| !t.resolve(from, name).is_some_and(|to| members.contains(&to)))
    }

    /// `calcDepFlags(tree)`.
    pub fn calc_dep_flags(&mut self, tree: Id) {
        {
            let root = &mut self.nodes[tree];
            root.dev = false;
            root.optional = false;
            root.dev_optional = false;
            root.peer = false;
            root.extraneous = false;
        }
        let mut seen: Vec<Id> = Vec::new();
        let mut queue = vec![tree];
        while let Some(node) = queue.pop() {
            if !seen.contains(&node) {
                seen.push(node);
            }
            if !self.nodes[node].extraneous {
                let mut at = self.nodes[node].parent;
                while let Some(p) = at.filter(|p| self.nodes[*p].extraneous) {
                    self.nodes[p].extraneous = false;
                    at = self.nodes[p].parent;
                }
            }
            if let Some(target) = self.nodes[node].target {
                let (dev, optional, dev_optional, peer, extraneous) = {
                    let n = &self.nodes[node];
                    (n.dev, n.optional, n.dev_optional, n.peer, n.extraneous)
                };
                let t = &mut self.nodes[target];
                t.dev = dev;
                t.optional = optional;
                t.dev_optional = dev_optional;
                t.peer = peer;
                t.extraneous = extraneous;
                queue.push(target);
                continue;
            }
            let names: Vec<String> = self.nodes[node].edges.keys().cloned().collect();
            for name in names {
                let kind = self.nodes[node].edges[&name].kind;
                let Some(to) = self.resolve(node, &name) else { continue };
                let (n_extraneous, n_dev, n_optional, n_dev_optional, n_peer) = {
                    let n = &self.nodes[node];
                    (n.extraneous, n.dev, n.optional, n.dev_optional, n.peer)
                };
                let (peer, optional, dev) = (kind.peer(), kind.optional(), false);
                let t = &mut self.nodes[to];
                let mut changed = false;
                if t.extraneous && !n_extraneous && !(peer && optional) {
                    t.extraneous = false;
                    changed = true;
                }
                if t.dev && !n_dev && !dev {
                    t.dev = false;
                    changed = true;
                }
                if t.optional && !n_optional && !optional {
                    t.optional = false;
                    changed = true;
                }
                if t.dev_optional && !n_dev_optional && !n_dev && !n_optional && !dev && !optional {
                    t.dev_optional = false;
                    changed = true;
                }
                if t.peer && !n_peer && !peer {
                    t.peer = false;
                    changed = true;
                }
                if changed {
                    queue.push(to);
                }
            }
        }
        for node in seen {
            if node == tree {
                continue;
            }
            let n = &mut self.nodes[node];
            if n.dev_optional && (n.dev || n.optional) {
                n.dev_optional = false;
            }
        }
    }
}

/// `consistentResolve(resolved)` for the forms a manifest's `_resolved` takes.
pub fn consistent_resolve(resolved: &str) -> Option<String> {
    if resolved.is_empty() {
        return None;
    }
    match spec::npa(resolved, "/") {
        Ok(parsed) => match &parsed.kind {
            Kind::File | Kind::Directory => parsed.fetch_spec.map(|p| format!("file:{p}")),
            Kind::Git { .. } => parsed.save_spec.or(Some(resolved.to_owned())),
            _ if parsed.raw_spec == "*" => Some(parsed.raw),
            _ => Some(parsed.raw_spec),
        },
        Err(_) => Some(resolved.to_owned()),
    }
}

/// `depValid(child, requested, accept, requestor)`.
fn dep_valid(tree: &Tree, child: Id, requested: &str, accept: Option<&str>, requestor: Id) -> bool {
    dep_valid_one(tree, child, requested, requestor) || accept.is_some_and(|a| dep_valid_one(tree, child, a, requestor))
}

fn dep_valid_one(tree: &Tree, child: Id, requested: &str, requestor: Id) -> bool {
    let requested = if requested.is_empty() { "*" } else { requested };
    let name = tree.nodes[child].name.clone();
    let Ok(spec) = spec::resolve(&name, requested, &tree.nodes[requestor].path) else { return false };
    spec_valid(tree, child, &spec)
}

fn spec_valid(tree: &Tree, child: Id, spec: &Spec) -> bool {
    let node = &tree.nodes[child];
    match &spec.kind {
        Kind::Range if spec.fetch_spec.as_deref() == Some("*") => true,
        Kind::Range | Kind::Version => {
            semver::satisfies(&tree.version(child), spec.fetch_spec.as_deref().unwrap_or(""), Options::LOOSE).unwrap_or(false)
        }
        Kind::Alias(sub) => spec_valid(tree, child, sub),
        Kind::Tag => node.resolved.as_deref().is_some_and(|r| spec::npa(r, "/").is_ok_and(|s| s.kind == Kind::Remote)),
        Kind::Remote => node.resolved.as_deref() == spec.fetch_spec.as_deref(),
        Kind::Git { hosted, range, .. } => {
            if *hosted {
                return false;
            }
            let resolved = node.resolved.clone().unwrap_or_default();
            let Ok(repo) = spec::npa(&resolved, "/") else { return false };
            if repo.fetch_spec != spec.fetch_spec {
                return false;
            }
            match range {
                None => true,
                Some(range) => semver::satisfies(&tree.version(child), range, Options::LOOSE).unwrap_or(false),
            }
        }
        Kind::Directory => tree.is_link(child) && tree.nodes[child].path == spec.fetch_spec.clone().unwrap_or_default(),
        Kind::File => {
            !tree.is_link(child) && node.resolved.as_deref() == spec.fetch_spec.as_deref().map(|p| format!("file:{p}")).as_deref()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Conflict,
    Ok,
    Replace,
    Keep,
}

/// One `CanPlaceDep` check and the checks of the peers it brings.
struct CanPlace {
    dep: Id,
    target: Id,
    edge: (Id, String),
    can_place: Placement,
    can_place_self: Placement,
    children: Vec<CanPlace>,
}

struct CanPlaceInput<'a> {
    dep: Id,
    target: Id,
    edge: (Id, String),
    prefer_dedupe: bool,
    explicit_request: bool,
    /// The chain of parents: (their dep, their edge), outermost first.
    parents: &'a [(Id, (Id, String))],
    peer_path: Vec<Id>,
}

fn can_place(tree: &Tree, input: CanPlaceInput) -> CanPlace {
    let CanPlaceInput { dep, target, edge, prefer_dedupe, explicit_request, parents, peer_path } = input;
    let prefer_dedupe = prefer_dedupe || tree.edge(edge.0, &edge.1).is_some_and(|e| e.kind.peer());
    let name = edge.1.clone();
    let current = tree.nodes[target].children.get(&name).copied();
    let target_edge = tree.edge(target, &name).cloned();
    let edge_override = !tree.satisfies(dep, edge.0, &edge.1);
    let mut check = CanPlace { dep, target, edge: edge.clone(), can_place: Placement::Ok, can_place_self: Placement::Ok, children: Vec::new() };
    let mut self_state: Option<Placement> = None;
    let mut peers_result: Option<Placement> = None;

    // `canPlacePeers(state)`
    let mut can_place_peers = |check: &mut CanPlace, self_state: &mut Option<Placement>, state: Placement| -> Placement {
        *self_state = Some(state);
        if let Some(done) = peers_result {
            return done;
        }
        let mut path = peer_path.clone();
        path.push(dep);
        let mut chain: Vec<(Id, (Id, String))> = parents.to_vec();
        chain.push((dep, edge.clone()));
        let mut saw_conflict = false;
        let names: Vec<String> = tree.nodes[dep].edges.keys().cloned().collect();
        for peer_name in names {
            let peer_edge = &tree.nodes[dep].edges[&peer_name];
            if !peer_edge.kind.peer() {
                continue;
            }
            let Some(peer) = tree.resolve(dep, &peer_name) else { continue };
            if path.contains(&peer) {
                continue;
            }
            let peer_target = tree.deepest_nesting_target(target, &tree.nodes[peer].name);
            let child = can_place(
                tree,
                CanPlaceInput {
                    dep: peer,
                    target: peer_target,
                    edge: (dep, peer_name.clone()),
                    prefer_dedupe: true,
                    explicit_request: false,
                    parents: &chain,
                    peer_path: path.clone(),
                },
            );
            if child.can_place == Placement::Conflict {
                saw_conflict = true;
            }
            check.children.push(child);
        }
        let result = if saw_conflict { Placement::Conflict } else { state };
        peers_result = Some(result);
        result
    };

    let deepest = || {
        // `get deepestNestingTarget`: from the top check's edge.from, through each check's edge name.
        let mut start = parents.first().map(|(_, e)| e.0).unwrap_or(edge.0);
        for (_, e) in parents {
            start = tree.deepest_nesting_target(start, &e.1);
        }
        tree.deepest_nesting_target(start, &name)
    };

    let result = (|| {
        if !tree.nodes[dep].errors.is_empty() {
            return if current.is_some() { Placement::Replace } else { Placement::Ok };
        }
        if target_edge.as_ref().is_some_and(|e| e.kind.peer()) && !tree.is_top(target) {
            return Placement::Conflict;
        }
        if current.is_none() && target_edge.is_some() && !tree.satisfies(dep, target, &name) && (target, name.clone()) != edge {
            return Placement::Conflict;
        }
        match current {
            Some(current) => {
                if tree.matches(dep, current) && (tree.satisfies(current, edge.0, &edge.1) || edge_override) {
                    return if explicit_request { Placement::Replace } else { Placement::Keep };
                }
                let (cur_ver, new_ver) = (tree.version(current), tree.version(dep));
                let try_replace = !cur_ver.is_empty()
                    && !new_ver.is_empty()
                    && semver::compare(&new_ver, &cur_ver, Options::STRICT).map(|c| c >= 0).unwrap_or(false);
                if try_replace && tree.can_replace_with(current, dep, &[]) {
                    let cpp = can_place_peers(&mut check, &mut self_state, Placement::Replace);
                    if cpp != Placement::Conflict {
                        return cpp;
                    }
                }
                if tree.satisfies(current, edge.0, &edge.1) && (!explicit_request || prefer_dedupe) {
                    return Placement::Keep;
                }
                if prefer_dedupe && !try_replace && tree.can_replace_with(current, dep, &[]) {
                    let cpp = can_place_peers(&mut check, &mut self_state, Placement::Replace);
                    if cpp != Placement::Conflict {
                        return cpp;
                    }
                }
                if target != deepest() {
                    return Placement::Conflict;
                }
                let edge_is_peer = tree.edge(edge.0, &edge.1).is_some_and(|e| e.kind.peer());
                if !edge_is_peer && target == edge.0 {
                    return can_place_peers(&mut check, &mut self_state, Placement::Replace);
                }
                if parents.is_empty() && !edge_is_peer {
                    return Placement::Conflict;
                }
                let entry_edge = parents.first().map(|(_, e)| e.clone()).unwrap_or(edge.clone());
                let dep_parent = tree.nodes[dep].parent;
                let mut can_replace = true;
                for (entry, current_peers) in tree.peer_entry_sets(current) {
                    if entry == edge || entry == entry_edge {
                        continue;
                    }
                    let Some(entry_node) = tree.resolve(entry.0, &entry.1) else { continue };
                    let entry_rep = dep_parent.and_then(|p| tree.nodes[p].children.get(&tree.nodes[entry_node].name).copied());
                    if let Some(rep) = entry_rep {
                        let siblings: Vec<String> = dep_parent.map(|p| tree.nodes[p].children.keys().cloned().collect()).unwrap_or_default();
                        if tree.can_replace_with(entry_node, rep, &siblings) {
                            continue;
                        }
                    }
                    let mut can_clobber = entry_rep.is_none();
                    if entry_rep.is_none() {
                        let mut walk = vec![entry_node];
                        let mut i = 0;
                        'outer: while i < walk.len() {
                            let current_peer = walk[i];
                            for n in tree.nodes[current_peer].edges.keys() {
                                if !tree.nodes[current_peer].edges[n].kind.peer() || !tree.edge_valid(current_peer, n) {
                                    continue;
                                }
                                let rep = dep_parent.and_then(|p| tree.nodes[p].children.get(n).copied());
                                match rep {
                                    None => {
                                        if let Some(to) = tree.resolve(current_peer, n) {
                                            if !walk.contains(&to) {
                                                walk.push(to);
                                            }
                                        }
                                    }
                                    Some(rep) => {
                                        if !tree.satisfies(rep, current_peer, n) {
                                            can_clobber = false;
                                            break 'outer;
                                        }
                                    }
                                }
                            }
                            i += 1;
                        }
                    }
                    if can_clobber {
                        continue;
                    }
                    for current_peer in current_peers {
                        let cur_deep = tree.deepest_nesting_target(entry.0, &tree.nodes[current_peer].name);
                        if cur_deep == target || tree.is_descendant_of(target, cur_deep) {
                            can_replace = false;
                            break;
                        }
                    }
                }
                if can_replace {
                    return can_place_peers(&mut check, &mut self_state, Placement::Replace);
                }
                Placement::Conflict
            }
            None => {
                // checkCanPlaceNoCurrent
                let entry_from = parents.first().map(|(_, e)| e.0).unwrap_or(edge.0);
                let resolved = if target != entry_from { tree.resolve(target, &name) } else { None };
                if let Some(current) = resolved {
                    for (from, n) in tree.edges_in(current) {
                        if tree.is_descendant_of(from, target) && tree.edge_valid(from, &n) && !tree.satisfies(dep, from, &n) {
                            return Placement::Conflict;
                        }
                    }
                }
                can_place_peers(&mut check, &mut self_state, Placement::Ok)
            }
        }
    })();
    check.can_place = result;
    check.can_place_self = self_state.unwrap_or(result);
    check
}

/// What placing one dependency (and its peers) did.
struct Placed {
    placed: Option<Id>,
    can_place_self: Option<Placement>,
    edge: (Id, String),
    need_evaluation: Vec<Id>,
    children: Vec<Placed>,
}

pub struct Builder<'a> {
    pub tree: Tree,
    pub root: Id,
    source: &'a dyn Source,
    options: TreeOptions,
    queue: Vec<Id>,
    queue_sorted: bool,
    seen: HashSet<Id>,
    explicit: Vec<(Id, String)>,
    load_failures: Vec<Id>,
    manifests: HashMap<String, Json>,
    peer_set_source: HashMap<Id, Id>,
    virtual_roots: HashMap<Id, Id>,
}

impl<'a> Builder<'a> {
    pub fn new(root_path: &str, source: &'a dyn Source, options: TreeOptions) -> Builder<'a> {
        let mut tree = Tree::default();
        let name = crate::text::basename(root_path).to_owned();
        let root = tree.add(&name, Json::Object(IndexMap::new()), None, Some(root_path), options.legacy_peer_deps);
        {
            let r = &mut tree.nodes[root];
            r.dev = false;
            r.optional = false;
            r.dev_optional = false;
            r.peer = false;
            r.extraneous = false;
        }
        Builder {
            tree,
            root,
            source,
            options,
            queue: Vec::new(),
            queue_sorted: true,
            seen: HashSet::new(),
            explicit: Vec::new(),
            load_failures: Vec::new(),
            manifests: HashMap::new(),
            peer_set_source: HashMap::new(),
            virtual_roots: HashMap::new(),
        }
    }

    fn push(&mut self, id: Id) {
        if !self.queue.contains(&id) {
            self.queue_sorted = false;
            self.queue.push(id);
        }
    }

    fn pop(&mut self) -> Option<Id> {
        if self.queue.is_empty() {
            return None;
        }
        if !self.queue_sorted {
            let tree = &self.tree;
            self.queue.sort_by(|a, b| {
                tree.depth(*a).cmp(&tree.depth(*b)).then_with(|| locale_compare(&tree.nodes[*a].path, &tree.nodes[*b].path))
            });
            self.queue_sorted = true;
        }
        Some(self.queue.remove(0))
    }

    /// `buildIdealTree({ add: [spec] })` for an empty root: the root depends on `name` at `spec` (`*` for none).
    pub async fn build(&mut self, name: &str, spec: &str) -> Result<(), String> {
        if let Json::Object(map) = &mut self.tree.nodes[self.root].package {
            let mut deps = IndexMap::new();
            deps.insert(name.to_owned(), Json::String(spec.to_owned()));
            map.insert("dependencies".into(), Json::Object(deps));
        }
        self.tree.nodes[self.root].edges.clear();
        self.tree.load_deps(self.root, self.options.legacy_peer_deps);
        self.explicit.push((self.root, name.to_owned()));
        self.push(self.root);
        while let Some(node) = self.pop() {
            self.step(node).await?;
        }
        self.tree.calc_dep_flags(self.root);
        self.prune_failed_optional()?;
        self.check_engine_and_platform()?;
        Ok(())
    }

    fn problem_edges(&self, node: Id) -> Vec<String> {
        let t = &self.tree;
        let bundled: Vec<String> = if t.is_project_root(node) {
            Vec::new()
        } else {
            match get(&t.nodes[node].package, "bundleDependencies") {
                Some(Json::Array(items)) => items.iter().map(pick::js_string).collect(),
                _ => Vec::new(),
            }
        };
        let mut problems = Vec::new();
        for (name, edge) in &t.nodes[node].edges {
            if bundled.contains(name) {
                continue;
            }
            let to = t.resolve(node, name);
            if to.is_some_and(|to| self.load_failures.contains(&to)) {
                continue;
            }
            if to.is_some_and(|to| t.nodes[to].parent.is_some_and(|p| t.nodes[p].has_shrinkwrap)) {
                continue;
            }
            let explicit = self.explicit.contains(&(node, name.clone()));
            match to {
                None => {
                    if edge.kind != EdgeKind::PeerOptional || explicit {
                        problems.push(name.clone());
                    }
                }
                Some(_) if !t.edge_valid(node, name) => problems.push(name.clone()),
                Some(_) if explicit => problems.push(name.clone()),
                Some(_) => {}
            }
        }
        problems
    }

    fn virtual_root(&mut self, node: Id, reuse: bool) -> Id {
        if reuse {
            if let Some(vr) = self.virtual_roots.get(&node) {
                return *vr;
            }
        }
        let path = self.tree.nodes[node].path.clone();
        let package = self.tree.nodes[node].package.clone();
        let name = self.tree.nodes[node].name.clone();
        let vr = self.tree.add(&name, package, None, Some(&path), self.options.legacy_peer_deps);
        self.tree.nodes[vr].source_reference = Some(node);
        self.virtual_roots.insert(node, vr);
        vr
    }

    /// `#fetchManifest(spec)`, memoized by the spec's raw text.
    async fn fetch_manifest(&self, spec: &Spec) -> Result<Json, String> {
        if let Some(manifest) = self.manifests.get(&spec.raw) {
            return Ok(manifest.clone());
        }
        self.source.manifest(spec).await
    }

    async fn node_from_spec(&mut self, name: &str, spec: &Spec, parent: Id) -> Id {
        let fetched = match &spec.kind {
            Kind::Directory => Err("a directory dependency of a registry package".to_owned()),
            _ => self.fetch_manifest(spec).await,
        };
        match fetched {
            Ok(manifest) => {
                self.manifests.insert(spec.raw.clone(), manifest.clone());
                self.tree.add(name, manifest, Some(parent), None, self.options.legacy_peer_deps)
            }
            Err(error) => {
                let id = self.tree.add(name, Json::Object(IndexMap::new()), Some(parent), None, self.options.legacy_peer_deps);
                self.tree.nodes[id].errors.push(error);
                self.load_failures.push(id);
                id
            }
        }
    }

    /// `#nodeFromEdge(edge, parent, secondEdge, required)`.
    fn node_from_edge<'b>(
        &'b mut self,
        edge: (Id, String),
        parent: Option<Id>,
        second: Option<(Id, String)>,
        required: &'b mut Vec<Id>,
    ) -> Pin<Box<dyn Future<Output = Result<Id, String>> + Send + 'b>> {
        Box::pin(async move {
            let parent = match parent {
                Some(p) => p,
                None => self.virtual_root(edge.0, false),
            };
            let spec_text = self.tree.edge(edge.0, &edge.1).map(|e| e.spec.clone()).unwrap_or_default();
            let spec = spec::resolve(&edge.1, &spec_text, &self.tree.nodes[edge.0].path)?;
            let first = self.node_from_spec(&edge.1, &spec, parent).await;
            let second_node = match &second {
                Some(second) if !self.tree.edge_valid(second.0, &second.1) => {
                    let text = self.tree.edge(second.0, &second.1).map(|e| e.spec.clone()).unwrap_or_default();
                    let spec2 = spec::resolve(&edge.1, &text, &self.tree.nodes[second.0].path)?;
                    Some(self.node_from_spec(&edge.1, &spec2, parent).await)
                }
                _ => None,
            };
            let node = match second_node {
                Some(second) if self.tree.edge_valid(edge.0, &edge.1) => second,
                _ => first,
            };
            self.tree.set_parent(node, parent);
            let edge_kind = self.tree.edge(edge.0, &edge.1).map(|e| e.kind);
            let second_kind = second.as_ref().and_then(|s| self.tree.edge(s.0, &s.1).map(|e| e.kind));
            if (required.contains(&edge.0) && edge_kind != Some(EdgeKind::PeerOptional))
                || second.as_ref().is_some_and(|s| required.contains(&s.0) && second_kind != Some(EdgeKind::PeerOptional))
            {
                required.push(node);
            }
            let source = self.tree.nodes[parent].source_reference.unwrap_or(parent);
            self.peer_set_source.insert(node, source);
            self.load_peer_set(node, required).await
        })
    }

    /// `#loadPeerSet(node, required)`.
    async fn load_peer_set(&mut self, node: Id, required: &mut Vec<Id>) -> Result<Id, String> {
        let mut peer_edges: Vec<String> = self.tree.nodes[node]
            .edges
            .iter()
            .filter(|(name, e)| e.kind.peer() && !(self.tree.edge_valid(node, name) && self.tree.resolve(node, name).is_some()))
            .map(|(name, _)| name.clone())
            .collect();
        peer_edges.sort_by(|a, b| locale_compare(a, b));
        for name in peer_edges {
            let Some(parent) = self.tree.nodes[node].parent else { break };
            if self.tree.edge_valid(node, &name) && self.tree.resolve(node, &name).is_some() {
                continue;
            }
            let parent_edge = self.tree.edge(parent, &name).is_some().then(|| (parent, name.clone()));
            let source = self.tree.nodes[parent].source_reference.unwrap_or(parent);
            let is_mine = self.tree.is_project_root(source);
            let conflict_ok = self.options.force || (!is_mine && !self.options.strict_peer_deps);
            if self.tree.resolve(node, &name).is_none() {
                match parent_edge {
                    None => {
                        self.node_from_edge((node, name.clone()), Some(parent), None, required).await?;
                        continue;
                    }
                    Some(parent_edge) => {
                        let dep = self.node_from_edge(parent_edge.clone(), Some(parent), Some((node, name.clone())), required).await?;
                        if self.tree.edge_valid(node, &name) {
                            continue;
                        }
                        if conflict_ok || !required.contains(&dep) {
                            if let Some(e) = self.tree.nodes[node].edges.get_mut(&name) {
                                e.peer_conflicted = true;
                            }
                            continue;
                        }
                        return Err("unable to resolve dependency tree".into());
                    }
                }
            }
            let current = self.tree.resolve(node, &name).unwrap();
            let mut scratch = required.clone();
            let dep = self.node_from_edge((node, name.clone()), None, None, &mut scratch).await?;
            *required = scratch;
            if self.tree.can_replace_with(current, dep, &[]) {
                self.node_from_edge((node, name.clone()), Some(parent), None, required).await?;
                continue;
            }
            if conflict_ok || !required.contains(&node) {
                continue;
            }
            return Err("unable to resolve dependency tree".into());
        }
        Ok(node)
    }

    async fn step(&mut self, node: Id) -> Result<(), String> {
        let t = &self.tree;
        if self.seen.contains(&node) || t.root(node) != self.root || (t.nodes[node].has_shrinkwrap) {
            return Ok(());
        }
        self.seen.insert(node);
        let peer_source = self.peer_set_source.get(&node).copied().unwrap_or(node);
        let mut tasks: Vec<((Id, String), Id)> = Vec::new();
        for name in self.problem_edges(node) {
            let edge = self.tree.edge(node, &name).cloned().unwrap();
            if edge.peer_conflicted {
                continue;
            }
            let source = if edge.kind.peer() { peer_source } else { node };
            let vr = self.virtual_root(source, true);
            let vr_dep = self.tree.edge_to(vr, &name).filter(|_| self.tree.edge_valid(vr, &name));
            let mut required = vec![node];
            let parent = edge.kind.peer().then_some(vr);
            let dep = match vr_dep {
                Some(dep) if self.tree.satisfies(dep, node, &name) => dep,
                _ => self.node_from_edge((node, name.clone()), parent, None, &mut required).await?,
            };
            tasks.push(((node, name), dep));
        }
        tasks.sort_by(|a, b| locale_compare(&a.0.1, &b.0.1));
        let mut prefetch: Vec<Result<Spec, String>> = Vec::new();
        for (edge, dep) in tasks {
            let explicit = self.explicit.contains(&edge);
            let placed = self.place_dep(edge.clone(), dep, explicit, None)?;
            let mut stack = vec![placed];
            // depth-first, the node before its children
            let mut order: Vec<Placed> = Vec::new();
            while let Some(mut p) = stack.pop() {
                let children = std::mem::take(&mut p.children);
                order.push(p);
                for child in children.into_iter().rev() {
                    stack.push(child);
                }
            }
            for p in order {
                let Some(placed) = p.placed else { continue };
                if !self.tree.nodes[placed].errors.is_empty() && !self.load_failures.contains(&placed) {
                    self.load_failures.push(placed);
                }
                match p.can_place_self {
                    Some(Placement::Ok) => {
                        for (from, name) in self.tree.edges_in(placed) {
                            if (from, name.clone()) == p.edge {
                                continue;
                            }
                            let conflicted = self.tree.edge(from, &name).is_some_and(|e| e.peer_conflicted);
                            if !conflicted && !self.tree.edge_valid(from, &name) {
                                if self.seen.contains(&from) {
                                    self.seen.remove(&from);
                                    self.push(from);
                                } else {
                                    self.push(from);
                                }
                            }
                        }
                    }
                    Some(Placement::Replace) => {
                        for (from, name) in self.tree.edges_in(placed) {
                            if (from, name.clone()) == p.edge {
                                continue;
                            }
                            let conflicted = self.tree.edge(from, &name).is_some_and(|e| e.peer_conflicted);
                            if !self.tree.edge_valid(from, &name) && !conflicted {
                                self.seen.remove(&from);
                                self.push(from);
                            }
                        }
                    }
                    _ => {}
                }
                self.push(placed);
                for dep in p.need_evaluation {
                    self.seen.remove(&dep);
                    self.push(dep);
                }
                for name in self.problem_edges(placed) {
                    let spec_text = self.tree.edge(placed, &name).map(|e| e.spec.clone()).unwrap_or_default();
                    prefetch.push(spec::resolve(&name, &spec_text, &self.tree.nodes[placed].path));
                }
            }
        }
        // `promiseCallLimit(promises)`: the prefetches run `availableParallelism - 1` at a time; a manifest already
        // fetched is not asked again, but two asks for one spec in the same wave both go out. A spec that does not
        // parse throws inside its call: the step fails once the calls already started have gone out.
        let limit = std::thread::available_parallelism().map(|n| n.get().saturating_sub(1).max(1)).unwrap_or(1);
        let mut failure = None;
        for wave in prefetch.chunks(limit) {
            let this: &Builder = self;
            let fetches = wave.iter().filter_map(|spec| spec.as_ref().ok()).map(|spec| async move { (spec.raw.clone(), this.fetch_manifest(spec).await) });
            let results = futures_join_all(fetches.collect()).await;
            for (raw, result) in results {
                if let Ok(manifest) = result {
                    self.manifests.insert(raw, manifest);
                }
            }
            if let Some(Err(error)) = wave.iter().find(|spec| spec.is_err()) {
                failure = Some(error.clone());
                break;
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// `new PlaceDep({ edge, dep, ... })`.
    fn place_dep(&mut self, edge: (Id, String), dep: Id, explicit: bool, parent: Option<&ParentPlace>) -> Result<Placed, String> {
        let mut result = Placed { placed: None, can_place_self: None, edge: edge.clone(), need_evaluation: Vec::new(), children: Vec::new() };
        let current = self.tree.edge_to(edge.0, &edge.1);
        if current.is_some() && self.tree.edge_valid(edge.0, &edge.1) && !explicit {
            return Ok(result);
        }
        // getStartNode: as shallow as the peer dependent
        let from = match parent {
            Some(p) => p.start,
            None => edge.0,
        };
        let start = self.tree.deepest_nesting_target(from, &edge.1);
        let parents: Vec<(Id, (Id, String))> = parent.map(|p| p.chain.clone()).unwrap_or_default();
        let mut chosen: Option<CanPlace> = None;
        let mut chosen_self: Option<CanPlace> = None;
        for target in self.tree.ancestry(start) {
            let target_edge = self.tree.edge(target, &edge.1);
            if !self.tree.is_top(target) && target_edge.is_some_and(|e| e.kind.peer()) {
                continue;
            }
            let check = can_place(
                &self.tree,
                CanPlaceInput {
                    dep,
                    target,
                    edge: edge.clone(),
                    prefer_dedupe: self.options.prefer_dedupe,
                    explicit_request: explicit,
                    parents: &parents,
                    peer_path: Vec::new(),
                },
            );
            let conflict = check.can_place == Placement::Conflict;
            let self_ok = check.can_place_self != Placement::Conflict;
            let failed = !self.tree.nodes[dep].errors.is_empty();
            if self_ok {
                chosen_self = Some(clone_check(&check));
            }
            if conflict {
                break;
            }
            chosen = Some(check);
            if failed || self.options.install_strategy == "nested" {
                break;
            }
            if self.options.install_strategy == "shallow" {
                if let Some(rp) = self.tree.nodes[target].parent {
                    if self.tree.is_project_root(rp) {
                        break;
                    }
                }
            }
        }
        let check = match chosen {
            Some(check) => check,
            None => {
                let top_edge = parents.first().map(|(_, e)| e.clone()).unwrap_or(edge.clone());
                let is_mine = self.is_mine(&top_edge);
                if !self.options.force && (is_mine || self.options.strict_peer_deps) {
                    return Err("could not resolve".into());
                }
                match chosen_self {
                    None => {
                        if let Some(e) = self.tree.nodes[edge.0].edges.get_mut(&edge.1) {
                            e.peer_conflicted = true;
                        }
                        return Ok(result);
                    }
                    Some(check) => check,
                }
            }
        };
        let target = check.target;
        let placement = if check.can_place == Placement::Conflict { check.can_place_self } else { check.can_place };
        result.can_place_self = Some(check.can_place_self);
        let edge_peer = self.tree.edge(edge.0, &edge.1).is_some_and(|e| e.kind.peer());
        if placement == Placement::Keep {
            if edge_peer && !self.tree.edge_valid(edge.0, &edge.1) {
                if let Some(e) = self.tree.nodes[edge.0].edges.get_mut(&edge.1) {
                    e.peer_conflicted = true;
                }
            }
            self.prune_dedupable(target, true, explicit);
            return Ok(result);
        }
        // a cycle of conflicting versions: link to the earlier instance
        let mut p = Some(target);
        while let Some(at) = p {
            if self.tree.matches(at, dep) && !self.tree.is_top(at) {
                let name = edge.1.clone();
                let link = self.tree.add(&name, self.tree.nodes[at].package.clone(), None, None, true);
                self.tree.nodes[link].edges.clear();
                self.tree.nodes[link].resolved = None;
                self.tree.nodes[link].integrity = None;
                self.tree.nodes[link].target = Some(at);
                self.tree.set_parent(link, target);
                result.placed = Some(link);
                return Ok(result);
            }
            p = self.tree.nodes[at].parent;
        }
        let virtual_root = self.tree.nodes[dep].parent;
        let placed = self.tree.add(&edge.1, self.tree.nodes[dep].package.clone(), None, None, self.options.legacy_peer_deps);
        self.tree.nodes[placed].resolved = self.tree.nodes[dep].resolved.clone();
        self.tree.nodes[placed].integrity = self.tree.nodes[dep].integrity.clone();
        self.tree.nodes[placed].errors = self.tree.nodes[dep].errors.iter().take(1).cloned().collect();
        let name = edge.1.clone();
        let old = self.tree.nodes[target].children.get(&name).copied();
        match old {
            Some(old) => self.replace_old_dep(placed, old, &mut result),
            None => self.tree.set_parent(placed, target),
        }
        if edge_peer && !self.tree.satisfies(placed, edge.0, &edge.1) {
            if let Some(e) = self.tree.nodes[edge.0].edges.get_mut(&edge.1) {
                e.peer_conflicted = true;
            }
        }
        if self.tree.edge_valid(edge.0, &edge.1) {
            if let Some(to) = self.tree.edge_to(edge.0, &edge.1).filter(|to| *to != placed) {
                self.prune_dedupable(to, false, explicit);
            }
        }
        let root = self.tree.root(target);
        for node in self.tree.inventory(root) {
            if self.tree.root(node) != root || self.tree.nodes[node].name != name {
                continue;
            }
            if self.tree.is_descendant_of(node, target) && !self.tree.is_top(node) {
                self.prune_dedupable(node, false, explicit);
                if self.tree.root(node) == root {
                    let kids: Vec<Id> = self.tree.nodes[node].children.values().copied().collect();
                    for kid in kids {
                        self.prune_dedupable(kid, false, explicit);
                    }
                }
            }
        }
        // the placed node's unmet peers, placed at this location too
        let names: Vec<String> = self.tree.nodes[placed].edges.keys().cloned().collect();
        let mut chain = parents.clone();
        chain.push((dep, edge.clone()));
        for peer_name in names {
            let peer_edge = self.tree.nodes[placed].edges[&peer_name].clone();
            if self.tree.edge_valid(placed, &peer_name) || !peer_edge.kind.peer() || peer_edge.peer_conflicted {
                continue;
            }
            let Some(vr) = virtual_root else { continue };
            let Some(peer) = self.tree.nodes[vr].children.get(&peer_name).copied() else { continue };
            if !self.tree.satisfies(peer, placed, &peer_name) {
                continue;
            }
            let context = ParentPlace { start: self.start_of(parent, &edge), chain: chain.clone() };
            let child = self.place_dep((placed, peer_name.clone()), peer, explicit, Some(&context))?;
            result.children.push(child);
        }
        result.placed = Some(placed);
        Ok(result)
    }

    /// `get isMine`: the top edge comes from the project, or its peer group was entered from it.
    fn is_mine(&self, top_edge: &(Id, String)) -> bool {
        let node = top_edge.0;
        if self.tree.is_project_root(node) {
            return true;
        }
        if !self.tree.edge(node, &top_edge.1).is_some_and(|e| e.kind.peer()) {
            return false;
        }
        let mut has_peer_edges = false;
        for (from, name) in self.tree.edges_in(node) {
            if self.tree.edge(from, &name).is_some_and(|e| e.kind.peer()) {
                has_peer_edges = true;
                continue;
            }
            if self.tree.is_project_root(from) {
                return true;
            }
        }
        if has_peer_edges {
            for (entry, _) in self.tree.peer_entry_sets(node) {
                if self.tree.is_project_root(entry.0) {
                    return true;
                }
            }
        }
        false
    }

    fn start_of(&self, parent: Option<&ParentPlace>, edge: &(Id, String)) -> Id {
        let from = parent.map(|p| p.start).unwrap_or(edge.0);
        self.tree.deepest_nesting_target(from, &edge.1)
    }

    /// `replaceOldDep()`.
    fn replace_old_dep(&mut self, placed: Id, old: Id, result: &mut Placed) {
        let target = self.tree.nodes[old].parent;
        let mut old_deps = Vec::new();
        let names: Vec<String> = self.tree.nodes[old].edges.keys().cloned().collect();
        for name in names {
            if self.tree.edge(placed, &name).is_none() {
                if let Some(to) = self.tree.resolve(old, &name) {
                    let set = self.tree.gather_dep_set(&[to], &|t, from, n| t.resolve(from, n) != Some(to));
                    old_deps.extend(set);
                }
            }
        }
        let mut prune_peer_sets = Vec::new();
        for (from, name) in self.tree.edges_in(old) {
            let edge = self.tree.edge(from, &name).cloned().unwrap();
            if self.tree.satisfied_by(from, &name, placed) || !edge.kind.peer() || self.tree.nodes[from].parent != target || edge.peer_conflicted {
                continue;
            }
            for (entry, _) in self.tree.peer_entry_sets(from) {
                let Some(entry_node) = self.tree.resolve(entry.0, &entry.1) else { continue };
                // `deepestNestingTarget(entryNode)` without a name stops at the entry node itself.
                let deepest = entry_node;
                if Some(deepest) != target && !self.tree.is_project_root(entry.0) {
                    let set = self.tree.gather_dep_set(&[entry_node], &|t, f, n| {
                        t.resolve(f, n) != Some(entry_node) && !t.edge(f, n).is_some_and(|e| e.peer_conflicted)
                    });
                    prune_peer_sets.extend(set);
                } else if let Some(e) = self.tree.nodes[from].edges.get_mut(&name) {
                    e.peer_conflicted = true;
                }
            }
        }
        self.tree.replace(placed, old);
        self.prune_for_replacement(placed, &old_deps);
        for dep in prune_peer_sets {
            for (from, _) in self.tree.edges_in(dep) {
                if !result.need_evaluation.contains(&from) {
                    result.need_evaluation.push(from);
                }
            }
            self.tree.detach(dep);
        }
    }

    fn prune_for_replacement(&mut self, node: Id, old_deps: &[Id]) {
        let mut invalid: Vec<Id> = Vec::new();
        let names: Vec<String> = self.tree.nodes[node].edges.keys().cloned().collect();
        for name in names {
            if let Some(to) = self.tree.resolve(node, &name) {
                if !self.tree.edge_valid(node, &name) && !invalid.contains(&to) {
                    invalid.push(to);
                }
            }
        }
        for dep in old_deps {
            let set = self.tree.gather_dep_set(&[*dep], &|t, f, n| t.resolve(f, n) != Some(*dep) && t.edge_valid(f, n));
            for d in set {
                if !invalid.contains(&d) {
                    invalid.push(d);
                }
            }
        }
        let deps = self.tree.gather_dep_set(&invalid, &|t, f, n| f != node && t.resolve(f, n) != Some(node) && t.edge_valid(f, n));
        for dep in deps {
            self.tree.detach(dep);
        }
    }

    /// `pruneDedupable(node, descend)`.
    fn prune_dedupable(&mut self, node: Id, descend: bool, explicit: bool) {
        if self.tree.can_dedupe(node, self.options.prefer_dedupe, explicit) {
            let deps = self.tree.gather_dep_set(&[node], &|t, f, n| t.resolve(f, n) != Some(node) && t.edge_valid(f, n));
            for dep in deps {
                self.tree.detach(dep);
            }
            return;
        }
        if descend {
            let mut kids: Vec<Id> = self.tree.nodes[node].children.values().copied().collect();
            kids.sort_by(|a, b| locale_compare(&self.tree.location(*a), &self.tree.location(*b)));
            for kid in kids {
                if self.tree.root(kid) == self.tree.root(node) {
                    self.prune_dedupable(kid, true, explicit);
                }
            }
        }
    }

    /// `#pruneFailedOptional()`: a failed required dependency fails the install; a failed optional one takes its
    /// optional set out (inert).
    fn prune_failed_optional(&mut self) -> Result<(), String> {
        for node in self.load_failures.clone() {
            if self.tree.root(node) != self.root {
                continue;
            }
            if !self.tree.nodes[node].optional {
                return Err(self.tree.nodes[node].errors.first().cloned().unwrap_or_default());
            }
            for member in self.tree.optional_set(node) {
                self.tree.nodes[member].inert = true;
            }
        }
        Ok(())
    }

    /// `#checkEngineAndPlatform()`.
    fn check_engine_and_platform(&mut self) -> Result<(), String> {
        for node in self.tree.inventory(self.root) {
            let package = self.tree.nodes[node].package.clone();
            let omitted = self.should_omit(node);
            if !self.tree.nodes[node].optional && !omitted {
                if !pick::engine_ok(&package, None, Some(pick::NODE_VERSION)) && self.options.engine_strict && !self.options.force {
                    return Err("Unsupported engine".into());
                }
                if !self.options.force && !platform_ok(&package, &Environment::default()) {
                    return Err("Unsupported platform".into());
                }
            }
            if self.tree.nodes[node].optional && !self.tree.nodes[node].inert {
                let env = Environment { os: self.options.os.clone(), cpu: self.options.cpu.clone(), libc: self.options.libc.clone() };
                if !pick::engine_ok(&package, None, Some(pick::NODE_VERSION)) || !platform_ok(&package, &env) {
                    for member in self.tree.optional_set(node) {
                        self.tree.nodes[member].inert = true;
                    }
                }
            }
        }
        Ok(())
    }

    /// `shouldOmit(omitSet)` (every node's top is the root here).
    pub fn should_omit(&self, node: Id) -> bool {
        let n = &self.tree.nodes[node];
        let has = |k: &str| self.options.omit.iter().any(|o| o == k);
        if self.options.omit.is_empty() {
            return false;
        }
        (n.peer && has("peer")) || (n.dev && has("dev")) || (n.optional && has("optional")) || (n.dev_optional && has("optional") && has("dev"))
    }
}

struct ParentPlace {
    start: Id,
    chain: Vec<(Id, (Id, String))>,
}

fn clone_check(check: &CanPlace) -> CanPlace {
    CanPlace {
        dep: check.dep,
        target: check.target,
        edge: check.edge.clone(),
        can_place: check.can_place,
        can_place_self: check.can_place_self,
        children: check.children.iter().map(clone_check).collect(),
    }
}

async fn futures_join_all<F: Future>(futures: Vec<F>) -> Vec<F::Output> {
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

/// The platform an install runs on (npm-install-checks `currentEnv`), with the config's overrides.
#[derive(Debug, Clone, Default)]
pub struct Environment {
    pub os: Option<String>,
    pub cpu: Option<String>,
    pub libc: Option<String>,
}

pub fn current_os() -> &'static str {
    if cfg!(target_os = "macos") { "darwin" } else { "linux" }
}

pub fn current_cpu() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// npm-install-checks `libc(os)`: from `/usr/bin/ldd`'s text (the report fallback finds glibc when that fails).
pub fn current_libc(os: &str) -> Option<String> {
    if os != "linux" {
        return None;
    }
    match std::fs::read_to_string("/usr/bin/ldd") {
        Ok(text) if text.contains("musl") => Some("musl".into()),
        Ok(text) if text.contains("GNU C Library") => Some("glibc".into()),
        Ok(_) => None,
        Err(_) => Some("glibc".into()),
    }
}

/// `checkPlatform(pkg, false, environment)`.
pub fn platform_ok(package: &Json, env: &Environment) -> bool {
    let os = env.os.clone().unwrap_or_else(|| current_os().to_owned());
    let cpu = env.cpu.clone().unwrap_or_else(|| current_cpu().to_owned());
    let libc = env.libc.clone().or_else(|| current_libc(&os));
    let check = |key: &str, value: Option<&str>| -> bool {
        let list = get(package, key);
        if !truthy(list) {
            return true;
        }
        let items: Vec<String> = match list.unwrap() {
            Json::String(s) => vec![s.clone()],
            Json::Array(items) => match items.iter().map(|i| match i { Json::String(s) => Some(s.clone()), _ => None }).collect::<Option<Vec<_>>>() {
                Some(items) => items,
                None => return false,
            },
            _ => return false,
        };
        let Some(value) = value else { return false };
        if items.len() == 1 && items[0] == "any" {
            return true;
        }
        let mut negated = 0;
        let mut matched = false;
        for entry in &items {
            match entry.strip_prefix('!') {
                Some(test) => {
                    negated += 1;
                    if value == test {
                        return false;
                    }
                }
                None => matched = matched || value == entry,
            }
        }
        matched || negated == items.len()
    };
    check("os", Some(&os)) && check("cpu", Some(&cpu)) && check("libc", libc.as_deref())
}
