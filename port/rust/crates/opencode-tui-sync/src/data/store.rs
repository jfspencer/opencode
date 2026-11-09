//! The data layer's store (`Store` in solid/data.ts) and the parts of Solid's store semantics that decide its
//! values: `setStore(path, object)` merges into an existing object (keys set to `undefined` are deleted),
//! `reconcile(value)` keeps the previous object's key order and reuses items matched by `id` (`applyState`).
//!
//! Records are shared JSON values (`Rc<Json>`): a reader keeps the value it read, and a change makes a new one when
//! a reader still holds the old (copy on write).

use std::rc::Rc;

use indexmap::IndexMap;
use opencode_protocol::Json;
use opencode_protocol::wire::Object;

use crate::json::JsonExt;
use crate::reactive::LocationField;

pub type Record = Rc<Json>;
pub type List = Rc<Vec<Record>>;

/// `DataSessionStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Idle,
    Running,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
        }
    }
}

#[derive(Default)]
pub struct SessionStore {
    pub info: IndexMap<String, Record>,
    pub missing: IndexMap<String, ()>,
    /// Family index keyed by a family's root (or furthest-known ancestor): every session ID of the family.
    pub family: IndexMap<String, Rc<Vec<String>>>,
    pub active: IndexMap<String, SessionStatus>,
    pub message: IndexMap<String, List>,
    pub message_cursor: IndexMap<String, String>,
    pub message_loading: IndexMap<String, bool>,
    pub pending: IndexMap<String, List>,
    pub permission: IndexMap<String, List>,
    /// Pending forms keyed by owner: a session ID or the temporary "global" elicitation sentinel.
    pub form: IndexMap<String, List>,
}

#[derive(Default)]
pub struct ProjectStore {
    pub info: IndexMap<String, Record>,
    pub permission: IndexMap<String, Record>,
}

/// `LocationData`: the cached catalogs of one location, by field, in the order they were first set.
#[derive(Default, Clone)]
pub struct LocationData(pub IndexMap<LocationField, Record>);

impl LocationData {
    pub fn get(&self, field: LocationField) -> Option<&Record> {
        self.0.get(&field)
    }

    /// `setStore("location", key, { [field]: value })`: `None` deletes the field.
    pub fn set(&mut self, field: LocationField, value: Option<Json>) -> bool {
        match value {
            Some(value) => match self.0.get_mut(&field) {
                Some(existing) if **existing == value => false,
                Some(existing) => {
                    *existing = Rc::new(value);
                    true
                }
                None => {
                    self.0.insert(field, Rc::new(value));
                    true
                }
            },
            None => self.0.shift_remove(&field).is_some(),
        }
    }
}

#[derive(Default)]
pub struct Store {
    pub session: SessionStore,
    pub project: ProjectStore,
    pub location: IndexMap<String, LocationData>,
}

fn list(items: &List) -> Json {
    Json::Array(items.iter().map(|item| (**item).clone()).collect())
}

fn map<V>(entries: &IndexMap<String, V>, value: impl Fn(&V) -> Json) -> Json {
    Json::Object(entries.iter().map(|(k, v)| (k.clone(), value(v))).collect())
}

impl Store {
    /// The store as `JSON.parse(JSON.stringify(store))` of the TS store.
    pub fn snapshot(&self) -> Json {
        let s = &self.session;
        let mut session = Object::new();
        session.insert("info".into(), map(&s.info, |v| (**v).clone()));
        session.insert("missing".into(), map(&s.missing, |_| Json::Bool(true)));
        session.insert(
            "family".into(),
            map(&s.family, |v| Json::Array(v.iter().map(|id| Json::String(id.clone())).collect())),
        );
        session.insert("active".into(), map(&s.active, |v| Json::String(v.as_str().into())));
        session.insert("message".into(), map(&s.message, list));
        session.insert("messageCursor".into(), map(&s.message_cursor, |v| Json::String(v.clone())));
        session.insert("messageLoading".into(), map(&s.message_loading, |v| Json::Bool(*v)));
        session.insert("pending".into(), map(&s.pending, list));
        session.insert("permission".into(), map(&s.permission, list));
        session.insert("form".into(), map(&s.form, list));
        let mut project = Object::new();
        project.insert("info".into(), map(&self.project.info, |v| (**v).clone()));
        project.insert("permission".into(), map(&self.project.permission, |v| (**v).clone()));
        let location = map(&self.location, |data| {
            Json::Object(data.0.iter().map(|(field, value)| (field.name().to_owned(), (**value).clone())).collect())
        });
        let mut root = Object::new();
        root.insert("session".into(), Json::Object(session));
        root.insert("project".into(), Json::Object(project));
        root.insert("location".into(), location);
        Json::Object(root)
    }
}

/// `mergeStoreNode(previous, value)` (`setStore(path, object)` on an existing object): each key of `value` is set,
/// `null`-free JSON cannot hold `undefined`, so deletions are expressed by the caller. Returns whether anything
/// changed.
pub fn merge(previous: &mut Json, value: &Object) -> bool {
    let mut changed = false;
    for (key, item) in value {
        if previous.get(key) != Some(item) {
            previous.set(key, item.clone());
            changed = true;
        }
    }
    changed
}

/// `reconcile(target)` applied over `previous` (Solid's `applyState` with `key: "id"`, without merge): the result
/// holds `target`'s values, with the key order of the objects it reused. Returns whether the value changed.
pub fn reconcile(previous: &mut Json, target: Json) -> bool {
    reconcile_keyed(previous, target, "id")
}

/// `reconcile(target, { key })`: items of arrays are matched by `key` instead of `id`.
pub fn reconcile_keyed(previous: &mut Json, target: Json, key: &str) -> bool {
    if *previous == target {
        return false;
    }
    // At the root `applyState` reuses the previous value whatever its key.
    apply(target, previous, true, key);
    true
}

fn apply_state(target: Json, previous: &mut Json, key: &str) {
    apply(target, previous, false, key)
}

fn wrappable(value: &Json) -> bool {
    matches!(value, Json::Object(_) | Json::Array(_))
}

fn id<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    value.get(key)
}

/// The key `applyState` matches items by (`item[key]`), JS-truthy only where the TS tests truthiness.
fn truthy_id<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    id(value, key).filter(|id| crate::json::truthy(id))
}

/// `applyState(target, parent, property)`: `previous` is `parent[property]`.
fn apply(target: Json, previous: &mut Json, root: bool, key: &str) {
    if target == *previous {
        return;
    }
    let target_array = matches!(target, Json::Array(_));
    if !wrappable(&target)
        || !wrappable(previous)
        || target_array != matches!(previous, Json::Array(_))
        || (!root && id(&target, key) != id(previous, key))
    {
        *previous = target;
        return;
    }
    match (target, previous) {
        (Json::Array(target), Json::Array(previous)) => apply_array(target, previous, key),
        (Json::Object(target), previous @ Json::Object(_)) => {
            let keys: Vec<String> = target.keys().cloned().collect();
            let mut target = target;
            for name in &keys {
                let value = target.shift_remove(name).expect("key from target");
                match previous.get_mut(name) {
                    Some(slot) => apply_state(value, slot, key),
                    None => previous.set(name, value),
                }
            }
            if let Json::Object(object) = previous {
                object.retain(|name, _| keys.contains(name));
            }
        }
        _ => unreachable!("both arrays or both objects"),
    }
}

/// The array branch of `applyState` (no merge).
fn apply_array(target: Vec<Json>, previous: &mut Vec<Json>, key: &str) {
    if target.is_empty() || previous.is_empty() {
        let len = target.len();
        for (i, item) in target.into_iter().enumerate() {
            match previous.get_mut(i) {
                Some(slot) => apply_state(item, slot, key),
                None => previous.push(item),
            }
        }
        previous.truncate(len);
        return;
    }
    let same = |a: &Json, b: &Json| a == b || matches!((truthy_id(a, key), truthy_id(b, key)), (Some(x), Some(y)) if x == y);
    let mut start = 0;
    let common = previous.len().min(target.len());
    while start < common && same(&previous[start], &target[start]) {
        start += 1;
    }
    let mut temp: Vec<Option<Json>> = vec![None; target.len()];
    let (mut end, mut new_end) = (previous.len() as isize - 1, target.len() as isize - 1);
    while end >= start as isize && new_end >= start as isize && same(&previous[end as usize], &target[new_end as usize]) {
        temp[new_end as usize] = Some(previous[end as usize].clone());
        end -= 1;
        new_end -= 1;
    }
    let mut target: Vec<Option<Json>> = target.into_iter().map(Some).collect();
    let mut result: Vec<Json> = Vec::with_capacity(target.len());
    // The matched prefix: applied in place.
    for (i, item) in target.iter_mut().enumerate().take(start) {
        let mut slot = previous[i].clone();
        apply_state(item.take().expect("prefix item"), &mut slot, key);
        result.push(slot);
    }
    if start as isize > new_end || start as isize > end {
        for item in target.iter_mut().take((new_end + 1).max(start as isize) as usize).skip(start) {
            result.push(item.take().expect("new item"));
        }
        let from = (new_end + 1).max(start as isize) as usize;
        for (j, item) in target.iter_mut().enumerate().skip(from) {
            let mut slot = temp[j].take().expect("suffix item");
            apply_state(item.take().expect("suffix target"), &mut slot, key);
            result.push(slot);
        }
        *previous = result;
        return;
    }
    // Match the middle by key: each previous item goes to the first unclaimed target position with its key.
    let key_of = |item: &Json| -> Json { id(item, key).cloned().unwrap_or(Json::Null) };
    let undefined_key = |item: &Json| id(item, key).is_none();
    let mut next: Vec<isize> = vec![-1; (new_end + 1) as usize];
    let mut indices: Vec<(Json, bool, isize)> = Vec::new();
    let lookup = |indices: &Vec<(Json, bool, isize)>, wanted: &Json, undefined: bool| {
        indices.iter().position(|(k, u, _)| *u == undefined && (undefined || k == wanted))
    };
    for j in (start..=new_end as usize).rev() {
        let item = target[j].as_ref().expect("middle target");
        let (value, undefined) = (key_of(item), undefined_key(item));
        match lookup(&indices, &value, undefined) {
            Some(at) => {
                next[j] = indices[at].2;
                indices[at].2 = j as isize;
            }
            None => {
                next[j] = -1;
                indices.push((value, undefined, j as isize));
            }
        }
    }
    for item in previous.iter().take(end as usize + 1).skip(start) {
        let (value, undefined) = (key_of(item), undefined_key(item));
        if let Some(at) = lookup(&indices, &value, undefined) {
            let j = indices[at].2;
            if j != -1 {
                temp[j as usize] = Some(item.clone());
                indices[at].2 = next[j as usize];
            }
        }
    }
    for (j, item) in target.iter_mut().enumerate().skip(start) {
        let item = item.take().expect("target item");
        match temp[j].take() {
            Some(mut slot) => {
                apply_state(item, &mut slot, key);
                result.push(slot);
            }
            None => result.push(item),
        }
    }
    *previous = result;
}

/// `reconcile` of a record (`Object.fromEntries(...)`) over an existing one: kept keys keep their place, new keys
/// are appended in order, missing keys are deleted.
pub fn reconcile_map<V: Clone + PartialEq>(previous: &mut IndexMap<String, V>, target: Vec<(String, V)>) -> Vec<String> {
    let mut changed = Vec::new();
    let keys: Vec<&String> = target.iter().map(|(k, _)| k).collect();
    previous.retain(|key, _| {
        let keep = keys.contains(&key);
        if !keep {
            changed.push(key.clone());
        }
        keep
    });
    for (key, value) in target {
        match previous.get_mut(&key) {
            Some(existing) if *existing == value => {}
            Some(existing) => {
                *existing = value;
                changed.push(key);
            }
            None => {
                previous.insert(key.clone(), value);
                changed.push(key);
            }
        }
    }
    changed
}

/// `JSON.stringify([ref.directory, ref.workspaceID])` (`locationKey`).
pub fn location_key(location: &Json) -> String {
    let field = |key: &str| match location.get(key) {
        Some(value) if !matches!(value, Json::Null) => value.clone(),
        _ => Json::Null,
    };
    crate::client::stringify(&Json::Array(vec![field("directory"), field("workspaceID")]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Json {
        opencode_protocol::wire::parse(text).unwrap()
    }

    #[test]
    fn reconcile_keeps_previous_key_order() {
        let mut previous = parse(r#"{"a":1,"b":{"x":1,"y":2},"c":3}"#);
        reconcile(&mut previous, parse(r#"{"c":4,"b":{"y":2,"x":1,"z":0},"d":5}"#));
        assert_eq!(crate::client::stringify(&previous), r#"{"b":{"x":1,"y":2,"z":0},"c":4,"d":5}"#);
    }

    #[test]
    fn reconcile_matches_items_by_id() {
        let mut previous = parse(r#"[{"id":"a","v":1,"k":0},{"id":"b","k":1,"v":2},{"text":"x","q":1}]"#);
        reconcile(&mut previous, parse(r#"[{"id":"b","v":3,"k":1},{"id":"c"},{"q":2,"text":"y"},{"v":1,"id":"a","k":0}]"#));
        assert_eq!(
            crate::client::stringify(&previous),
            r#"[{"id":"b","k":1,"v":3},{"id":"c"},{"text":"y","q":2},{"id":"a","v":1,"k":0}]"#
        );
    }

    #[test]
    fn location_keys() {
        assert_eq!(location_key(&parse(r#"{"directory":"/p"}"#)), r#"["/p",null]"#);
        assert_eq!(location_key(&parse(r#"{"directory":"/p","workspaceID":"w"}"#)), r#"["/p","w"]"#);
    }
}
