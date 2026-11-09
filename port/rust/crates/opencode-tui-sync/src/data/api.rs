//! The accessors and operations of the data layer (`result` in solid/data.ts), by its namespaces.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use opencode_protocol::Json;


use super::store::{self, List, Record};
use super::sync::Load;
use super::{
    Compacting, Error, Inner, PendingStatus, PendingUpdate, Promise, Resource, SessionStatus, form_request_options, id,
    location_key, location_query, omit_message_file_data, optimistic_files, promise, sid,
};
use crate::abort::AbortSignal;
use crate::client::RequestOptions;
use crate::json::JsonExt;
use crate::obj;
use crate::reactive::{Key, LocationField};
use crate::runtime::{self, Deferred};
use crate::transport::LocalBoxFuture;

fn empty() -> List {
    Rc::new(Vec::new())
}

/// `data.session`.
#[derive(Clone)]
pub struct SessionApi {
    pub input: InputApi,
    pub pending: PendingApi,
    pub message: MessageApi,
    pub permission: PermissionApi,
    pub form: FormApi,
    pub(super) inner: Rc<Inner>,
}

/// What `session.create` returns: the minted ID and the request.
#[derive(Clone)]
pub struct Created {
    pub id: String,
    pub request: Promise<Json>,
}

/// `session.create(input)`.
#[derive(Clone, Default)]
pub struct CreateInput {
    pub id: Option<String>,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub model: Option<Json>,
    pub location: Option<Json>,
    pub project_id: Option<String>,
}

/// `session.prompt(input)`: the `SessionPromptInput` fields as JSON, plus the gate and the preparation.
#[derive(Clone, Default)]
pub struct PromptInput {
    /// `{ sessionID, id?, text, files?, agents?, skills?, metadata?, delivery?, resume? }`.
    pub request: Json,
    pub gate: Option<Promise<()>>,
    pub prepare: Option<Rc<dyn Fn() -> Promise<()>>>,
}

/// `message.loadMore(sessionID, options)`.
#[derive(Clone, Default)]
pub struct LoadMoreOptions {
    pub all: bool,
    pub signal: Option<AbortSignal>,
    /// Runs synchronously inside the store-publication batch.
    pub before_publish: Option<Rc<dyn Fn()>>,
}

impl SessionApi {
    fn inner(&self) -> &Rc<Inner> {
        &self.inner
    }

    /// `session.list()`: every session info, most recently updated first.
    pub fn list(&self) -> Vec<Record> {
        self.inner.read(Key::Sessions, |store| {
            let mut sessions: Vec<Record> = store.session.info.values().cloned().collect();
            // `toSorted((a, b) => b.time.updated - a.time.updated)`: stable, NaN as equal.
            sessions.sort_by(|a, b| {
                let updated = |s: &Record| s.get("time").and_then(|t| t.num("updated")).unwrap_or(f64::NAN);
                let order = updated(b) - updated(a);
                if order > 0.0 {
                    std::cmp::Ordering::Greater
                } else if order < 0.0 {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            });
            sessions
        })
    }

    pub fn get(&self, session: &str) -> Option<Record> {
        self.inner.read(Key::Session(id(session)), |store| store.session.info.get(session).cloned())
    }

    pub fn missing(&self, session: &str) -> bool {
        self.inner.read(Key::Missing(id(session)), |store| store.session.missing.contains_key(session))
    }

    /// `session.forget(sessionID)`.
    pub fn forget(&self, session: &str) {
        self.inner.mark_deleted(session);
    }

    /// Whether an optimistic create of the session is in flight.
    pub fn creating(&self, session: &str) -> bool {
        self.inner.creating.borrow().contains_key(session)
    }

    /// `session.remember(info)`.
    pub fn remember(&self, info: Json) {
        let inner = self.inner();
        let session = info.str("id").unwrap_or("").to_owned();
        inner.batch(|| {
            inner.clear_deleted(&session);
            inner.set(|store, keys| {
                let order = |info: &Json| (info.get("parentID").cloned(), info.get("time").and_then(|t| t.get("updated")).cloned());
                // `session.list()` sorts by `time.updated`: a single session is never compared, so its update time
                // is not something the list depends on.
                let compared = store.session.info.len() >= 2;
                match store.session.info.get_mut(&session) {
                    Some(existing) => {
                        let (parent, updated) = order(existing);
                        if store::reconcile(Rc::make_mut(existing), info) {
                            keys.push(Key::Session(id(&session)));
                            let (new_parent, new_updated) = order(existing);
                            if new_updated != updated && compared {
                                keys.push(Key::SessionOrder);
                            }
                            if new_parent != parent {
                                keys.push(Key::SessionParent(id(&session)));
                            }
                        }
                    }
                    None => {
                        store.session.info.insert(session.clone(), Rc::new(info));
                        keys.extend([Key::Session(id(&session)), Key::SessionOrder, Key::SessionParent(id(&session))]);
                    }
                }
            });
            inner.sync.complete(&format!("session:{session}"));
            inner.register_session(&session);
        });
    }

    pub fn set_status(&self, session: &str, status: SessionStatus) {
        self.inner.set_session_active(session, status);
    }

    pub fn root(&self, session: &str) -> String {
        self.inner.resolve_root(session)
    }

    pub fn family(&self, session: &str) -> Rc<Vec<String>> {
        let root = self.inner.resolve_root(session);
        self.inner.read(Key::Family(id(&root)), |store| store.session.family.get(&root).cloned().unwrap_or_default())
    }

    /// Clears heavy cached data for the root and all known descendants.
    pub fn evict(&self, session: &str) {
        let inner = self.inner();
        let root = inner.resolve_root(session);
        inner.batch(|| {
            let mut ids: Vec<String> = vec![root.clone(), session.to_owned()];
            if let Some(family) = inner.store.borrow().session.family.get(&root) {
                ids.extend(family.iter().cloned());
            }
            let mut seen = HashSet::new();
            for id in ids {
                if seen.insert(id.clone()) {
                    inner.evict_session(&id);
                }
            }
        });
    }

    /// `session.cost(sessionID)`: a root's cost counts its whole family.
    pub fn cost(&self, session: &str) -> f64 {
        let inner = self.inner();
        let Some(info) = self.get(session) else { return 0.0 };
        let cost = |info: &Json| info.num("cost").unwrap_or(f64::NAN);
        if info.get("parentID").is_some_and(crate::json::truthy) {
            return info.num("cost").unwrap_or(f64::NAN);
        }
        inner.track(Key::Family(id(session)));
        let family: Vec<String> =
            inner.store.borrow().session.family.get(session).map(|f| (**f).clone()).unwrap_or_else(|| vec![session.to_owned()]);
        family.iter().fold(0.0, |total, member| {
            total
                + inner
                    .read(Key::Session(id(member)), |store| store.session.info.get(member).map(|i| cost(i)))
                    .map(|c| if c.is_nan() { f64::NAN } else { c })
                    .unwrap_or(0.0)
        })
    }

    pub fn status(&self, session: &str) -> SessionStatus {
        self.inner.read(Key::Status(id(session)), |store| store.session.active.get(session).copied().unwrap_or(SessionStatus::Idle))
    }

    /// The running sessions.
    pub fn active(&self) -> Vec<String> {
        self.inner.read(Key::Statuses, |store| {
            store.session.active.iter().filter(|(_, s)| **s == SessionStatus::Running).map(|(id, _)| id.clone()).collect()
        })
    }

    /// `session.sync(sessionID, { children })`.
    pub fn sync(&self, session: &str, children: bool) -> Promise<()> {
        let inner = self.inner.clone();
        let key = if children { format!("session.family:{session}") } else { format!("session:{session}") };
        let session = session.to_owned();
        let load: Load = Box::new(move || {
            let started = inner.deletion_epoch.get();
            let api = inner.api();
            let call = api.api().session().get(&obj! { "sessionID" => session.as_str() }, RequestOptions::default());
            let evict = inner.clone();
            let info: Promise<Json> = promise(async move { call.await.map(|v| v.json()).map_err(|error| evict.evict_missing(error)) });
            let children: Promise<Vec<Json>> = match children {
                true => promise(list_children(&inner, obj! { "parentID" => session.as_str(), "order" => "desc", "limit" => super::CHILD_PAGE })),
                false => Deferred::resolved(Ok(Vec::new())),
            };
            Box::pin(async move {
                let (info, children) = all2(info, children).await?;
                let sessions: Vec<Json> = std::iter::once(info)
                    .chain(children)
                    .filter(|s| inner.deleted_at.borrow().get(s.str("id").unwrap_or("")).copied().unwrap_or(0) <= started)
                    .collect();
                inner.batch(|| {
                    inner.set(|store, keys| {
                        for info in &sessions {
                            let id_text = info.str("id").unwrap_or("").to_owned();
                            // `draft[session.id] = session`: a new value, whatever it holds.
                            keys.extend([Key::Session(id(&id_text)), Key::SessionOrder, Key::SessionParent(id(&id_text))]);
                            store.session.info.insert(id_text, Rc::new(info.clone()));
                        }
                    });
                    for info in &sessions {
                        let id_text = info.str("id").unwrap_or("").to_owned();
                        inner.clear_deleted(&id_text);
                        inner.sync.complete(&format!("session:{id_text}"));
                        inner.register_session(&id_text);
                    }
                });
                Ok(())
            })
        });
        self.inner.sync.run(&key, load)
    }

    pub fn invalidate(&self, session: &str) {
        self.inner.sync.invalidate(Some(&format!("session:{session}")));
    }

    /// `session.create(input)`: admits a local record under a client-minted ID, then creates it on the server.
    pub fn create(&self, input: CreateInput) -> Created {
        let inner = self.inner.clone();
        let session = input.id.clone().unwrap_or_else(runtime::id::session);
        let location = input.location.clone().unwrap_or_else(|| inner.default_location());
        let fresh = !inner.has_info(&session);
        inner.clear_deleted(&session);
        if fresh {
            let now = runtime::now();
            inner.session_outbox.borrow_mut().insert(session.clone());
            let project = input.project_id.clone().map(Json::String).unwrap_or_else(|| {
                let key = location_key(&location);
                inner
                    .store
                    .borrow()
                    .location
                    .get(&key)
                    .and_then(|data| data.get(LocationField::Info))
                    .and_then(|info| info.get("project").and_then(|p| p.get("id")).cloned())
                    .unwrap_or_else(|| Json::String(String::new()))
            });
            self.remember(obj! {
                "id" => session.as_str(),
                "projectID" => project,
                "agent" => input.agent.clone(),
                "model" => input.model.clone(),
                "cost" => 0.0,
                "tokens" => obj! { "input" => 0.0, "output" => 0.0, "reasoning" => 0.0, "cache" => obj! { "read" => 0.0, "write" => 0.0 } },
                "time" => obj! { "created" => now, "updated" => now },
                "title" => input.title.clone(),
                "location" => location.clone(),
            });
            for key in ["session.family", "session.pending", "session.message"] {
                inner.sync.complete(&format!("{key}:{session}"));
            }
        }
        // `{ ...payload, id, location }`; the client sends the body's fields in its own order.
        let body = obj! {
            "id" => session.as_str(),
            "title" => input.title.clone(),
            "agent" => input.agent.clone(),
            "model" => input.model.clone(),
            "location" => location,
        };
        let request = inner.api().api().session().create(&body, RequestOptions::default());
        let this = self.clone();
        let created_id = session.clone();
        let promise: Promise<Json> = promise(async move {
            match request.await {
                Ok(value) => {
                    let info = value.json();
                    this.inner.session_outbox.borrow_mut().shift_remove(&created_id);
                    this.remember(info.clone());
                    Ok(info)
                }
                Err(error) => {
                    if fresh && this.inner.session_outbox.borrow_mut().shift_remove(&created_id) {
                        this.inner.remove_session(&created_id);
                    }
                    Err(error)
                }
            }
        });
        if fresh {
            inner.creating.borrow_mut().insert(session.clone(), promise.clone());
            let watched = promise.clone();
            let weak = Rc::downgrade(&inner);
            let key = session.clone();
            runtime::spawn(async move {
                let _ = watched.wait().await;
                if let Some(inner) = weak.upgrade() {
                    let current = inner.creating.borrow().get(&key).is_some_and(|p| p.ptr_eq(&watched));
                    if current {
                        inner.creating.borrow_mut().remove(&key);
                    }
                }
            });
        }
        Created { id: session, request: promise }
    }

    /// `session.compact({ sessionID, model })`.
    pub fn compact(&self, session: &str, model: Option<Json>) -> Promise<Json> {
        let inner = self.inner.clone();
        if let Some(active) = inner.compacting.borrow().get(session) {
            return active.request.clone();
        }
        let proposed = runtime::id::message();
        let has_compaction = inner
            .store
            .borrow()
            .session
            .pending
            .get(session)
            .is_some_and(|p| p.iter().any(|item| item.str("type") == Some("compaction")));
        if !has_compaction {
            inner.outbox.borrow_mut().insert(proposed.clone());
            inner.admit_local(Rc::new(obj! {
                "id" => proposed.as_str(),
                "sessionID" => session,
                "time" => obj! { "created" => runtime::now() },
                "type" => "compaction",
                "delivery" => "steer",
                "payload" => obj! {},
            }));
        }
        let observed = Rc::new(RefCell::new(HashSet::new()));
        let compacting_id = proposed.clone();
        let send_inner = inner.clone();
        let send_session = session.to_owned();
        let send_id = proposed.clone();
        let admission = inner.send_admission(
            session,
            move || {
                Box::pin(async move {
                    let api = send_inner.api();
                    if let Some(model) = model {
                        api.api()
                            .session()
                            .switch_model(&obj! { "sessionID" => send_session.as_str(), "model" => model }, RequestOptions::default())
                            .await?;
                    }
                    let value = api
                        .api()
                        .session()
                        .compact(&obj! { "sessionID" => send_session.as_str(), "id" => send_id.as_str() }, RequestOptions::default())
                        .await?;
                    Ok(value.json())
                })
            },
            None,
        );
        let this = inner.clone();
        let session_owned = session.to_owned();
        let seen = observed.clone();
        let request_slot: Rc<RefCell<Option<Promise<Json>>>> = Rc::new(RefCell::new(None));
        let slot = request_slot.clone();
        let request: Promise<Json> = promise(async move {
            let result = admission.wait().await;
            let result = match result {
                Ok(item) => {
                    let item_id = item.str("id").unwrap_or("").to_owned();
                    this.batch(|| {
                        this.outbox.borrow_mut().shift_remove(&proposed);
                        if item_id != proposed {
                            this.remove_pending(&session_owned, Some(&proposed));
                        }
                        let in_transcript =
                            this.message_index.borrow().get(&session_owned).is_some_and(|index| index.contains_key(&item_id));
                        if !seen.borrow().contains(&item_id) && !in_transcript {
                            this.admit_local(Rc::new(item.clone()));
                        }
                    });
                    Ok(item)
                }
                Err(error) => {
                    if this.outbox.borrow_mut().shift_remove(&proposed) {
                        this.remove_pending(&session_owned, Some(&proposed));
                    }
                    Err(error)
                }
            };
            let current = slot.borrow().clone();
            let is_current = match (this.compacting.borrow().get(&session_owned), &current) {
                (Some(active), Some(request)) => active.request.ptr_eq(request),
                _ => false,
            };
            if is_current {
                this.compacting.borrow_mut().remove(&session_owned);
            }
            result
        });
        *request_slot.borrow_mut() = Some(request.clone());
        inner.compacting.borrow_mut().insert(
            session.to_owned(),
            Compacting { id: compacting_id, observed, request: request.clone() },
        );
        request
    }

    /// `session.prompt(input)`: renders the prompt under a client-minted ID, sends it, and lets the durable echo
    /// upsert the row.
    pub fn prompt(&self, input: PromptInput) -> Promise<Json> {
        let inner = self.inner.clone();
        let mut request = input.request;
        let session = request.str("sessionID").unwrap_or("").to_owned();
        let prompt_id = request.str("id").map(str::to_owned).unwrap_or_else(runtime::id::message);
        let fresh = !inner.message_index.borrow().get(&session).is_some_and(|index| index.contains_key(&prompt_id))
            && !inner
                .store
                .borrow()
                .session
                .pending
                .get(&session)
                .is_some_and(|p| p.iter().any(|item| item.str("id") == Some(&prompt_id)));
        if fresh {
            let files = optimistic_files(request.get("files"));
            let mut payload = obj! {
                "text" => request.get("text"),
                "agents" => request.get("agents"),
                "metadata" => request.get("metadata"),
            };
            if let Some(files) = files {
                payload.set("files", files);
            }
            inner.outbox.borrow_mut().insert(prompt_id.clone());
            inner.admit_local(Rc::new(obj! {
                "id" => prompt_id.as_str(),
                "sessionID" => session.as_str(),
                "time" => obj! { "created" => runtime::now() },
                "type" => "user",
                "delivery" => request.get("delivery").cloned().unwrap_or_else(|| Json::String("steer".into())),
                "payload" => payload,
            }));
        }
        // `{ ...request, id }`.
        request.set("id", Json::String(prompt_id.clone()));
        let send_inner = inner.clone();
        let prepare = input.prepare.clone();
        let admission = inner.send_admission(
            &session,
            move || {
                Box::pin(async move {
                    if let Some(prepare) = prepare {
                        prepare().wait().await?;
                    }
                    let value = send_inner.api().api().session().prompt(&request, RequestOptions::default()).await?;
                    Ok(value.json())
                })
            },
            input.gate,
        );
        promise(async move {
            match admission.wait().await {
                Ok(value) => Ok(value),
                Err(error) => {
                    if fresh && inner.outbox.borrow_mut().shift_remove(&prompt_id) {
                        inner.retract_local(&session, &prompt_id);
                    }
                    Err(error)
                }
            }
        })
    }
}

/// `Promise.all([a, b])`: the first rejection to happen, else both values.
async fn all2<A: Clone + 'static, B: Clone + 'static>(a: Promise<A>, b: Promise<B>) -> Result<(A, B), Error> {
    let (mut left, mut right) = (None, None);
    let mut wait_a = std::pin::pin!(a.wait());
    let mut wait_b = std::pin::pin!(b.wait());
    while left.is_none() || right.is_none() {
        tokio::select! {
            biased;
            result = &mut wait_a, if left.is_none() => left = Some(result?),
            result = &mut wait_b, if right.is_none() => right = Some(result?),
        }
    }
    Ok((left.expect("settled"), right.expect("settled")))
}

/// `listChildren(input)`: every direct subagent, newest first, page by page.
fn list_children(inner: &Rc<Inner>, input: Json) -> LocalBoxFuture<'static, Result<Vec<Json>, Error>> {
    let call = inner.api().api().session().list(&input, RequestOptions::default());
    let inner = inner.clone();
    Box::pin(async move {
        let page = call.await?.json();
        let data: Vec<Json> = page.arr("data").cloned().unwrap_or_default();
        let next = page.get("cursor").and_then(|c| c.get("next")).filter(|n| crate::json::truthy(n)).cloned();
        if (data.len() as f64) < super::CHILD_PAGE || next.is_none() {
            return Ok(data);
        }
        let rest = list_children(&inner, obj! { "cursor" => next, "limit" => super::CHILD_PAGE }).await?;
        Ok(data.into_iter().chain(rest).collect())
    })
}

/// `session.input`: the pending user and synthetic items (compactions are control items).
#[derive(Clone)]
pub struct InputApi(pub(super) Rc<Inner>);

impl InputApi {
    pub fn list(&self, session: &str) -> Vec<String> {
        self.0.read(Key::Pending(id(session)), |store| {
            store
                .session
                .pending
                .get(session)
                .map(|p| {
                    p.iter()
                        .filter(|item| item.str("type") != Some("compaction"))
                        .map(|item| item.str("id").unwrap_or("").to_owned())
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    pub fn has(&self, session: &str, inbox: &str) -> bool {
        self.0.read(Key::Pending(id(session)), |store| {
            store
                .session
                .pending
                .get(session)
                .is_some_and(|p| p.iter().any(|item| item.str("id") == Some(inbox) && item.str("type") != Some("compaction")))
        })
    }
}

/// `session.pending`.
#[derive(Clone)]
pub struct PendingApi(pub(super) Rc<Inner>);

impl PendingApi {
    pub fn list(&self, session: &str) -> List {
        self.0.read(Key::Pending(id(session)), |store| store.session.pending.get(session).cloned().unwrap_or_else(empty))
    }

    /// `pending.status(sessionID, inboxID)`.
    pub fn status(&self, session: &str, inbox: &str) -> Option<PendingStatus> {
        let inner = &self.0;
        let pending = self.list(session);
        let item = pending.iter().find(|entry| entry.str("id") == Some(inbox) && entry.str("type") == Some("user"))?;
        if item.str("delivery") == Some("queue") {
            return Some(PendingStatus::Queued);
        }
        let messages = inner.read(Key::Messages(id(session)), |store| store.session.message.get(session).cloned().unwrap_or_else(empty));
        let boundary = messages.iter().rposition(|entry| entry.str("type") == Some("idle"));
        let created = |m: &Json| m.get("time").and_then(|t| t.num("created")).unwrap_or(f64::NAN);
        let status = inner.read(Key::Status(id(session)), |store| store.session.active.get(session).copied().unwrap_or(SessionStatus::Idle));
        if status == SessionStatus::Idle {
            return Some(match boundary {
                Some(b) if created(item) <= created(&messages[b]) => PendingStatus::Stranded,
                _ => PendingStatus::Starting,
            });
        }
        let start = boundary.map_or(0, |b| b + 1);
        let steering = messages[start.min(messages.len())..].iter().any(|entry| {
            entry.str("type") == Some("user") && !pending.iter().any(|other| other.get("id") == entry.get("id"))
        });
        Some(if steering { PendingStatus::Steering } else { PendingStatus::Starting })
    }

    /// `pending.sync(sessionID)`.
    pub fn sync(&self, session: &str) -> Promise<()> {
        let inner = self.0.clone();
        let session = session.to_owned();
        let key = format!("session.pending:{session}");
        let load: Load = Box::new(move || {
            let started = inner.deletion_epoch.get();
            let updates = Rc::new(RefCell::new(indexmap::IndexMap::new()));
            inner.pending_updates.borrow_mut().insert(session.clone(), updates.clone());
            let call = inner.api().api().session().inbox().list(&obj! { "sessionID" => session.as_str() }, RequestOptions::default());
            Box::pin(async move {
                let result = async {
                    let snapshot = match call.await {
                        Ok(value) => value.json(),
                        Err(error) => return Err(inner.evict_missing(error)),
                    };
                    if inner.deleted_at.borrow().get(&session).copied().unwrap_or(0) > started {
                        return Ok(());
                    }
                    // Events can overtake this response: reconcile them before an older snapshot resurrects input.
                    let mut current: indexmap::IndexMap<String, Record> = match snapshot {
                        Json::Array(items) => {
                            items.into_iter().map(|item| (item.str("id").unwrap_or("").to_owned(), Rc::new(item))).collect()
                        }
                        _ => indexmap::IndexMap::new(),
                    };
                    for (inbox, update) in updates.borrow().iter() {
                        match update {
                            PendingUpdate::Removed => {
                                current.shift_remove(inbox);
                            }
                            PendingUpdate::Delivery(delivery) => {
                                if let Some(existing) = current.get_mut(inbox) {
                                    Rc::make_mut(existing).set("delivery", Json::String(delivery.clone()));
                                }
                            }
                            PendingUpdate::Item(item) => {
                                current.insert(inbox.clone(), item.clone());
                            }
                        }
                    }
                    let pending: Vec<Record> = current.into_values().collect();
                    // A positive read acknowledges admission even when its echo is delayed.
                    {
                        let mut outbox = inner.outbox.borrow_mut();
                        for item in &pending {
                            outbox.shift_remove(item.str("id").unwrap_or(""));
                        }
                    }
                    // Compactions also coalesce by session, not just by the proposed ID.
                    if pending.iter().any(|item| item.str("type") == Some("compaction")) {
                        let local: Vec<String> = inner
                            .store
                            .borrow()
                            .session
                            .pending
                            .get(&session)
                            .map(|p| {
                                p.iter()
                                    .filter(|item| item.str("type") == Some("compaction"))
                                    .map(|item| item.str("id").unwrap_or("").to_owned())
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mut outbox = inner.outbox.borrow_mut();
                        for id in local {
                            outbox.shift_remove(&id);
                        }
                    }
                    // Keep optimistic rows still awaiting their echo.
                    let inflight: Vec<Record> = {
                        let outbox = inner.outbox.borrow();
                        inner
                            .store
                            .borrow()
                            .session
                            .pending
                            .get(&session)
                            .map(|p| p.iter().filter(|item| outbox.contains(item.str("id").unwrap_or(""))).cloned().collect())
                            .unwrap_or_default()
                    };
                    let merged: Vec<Record> = pending.into_iter().chain(inflight).collect();
                    inner.batch(|| {
                        inner.set(|store, keys| {
                            let target = Json::Array(merged.iter().map(|item| (**item).clone()).collect());
                            let changed = match store.session.pending.get_mut(&session) {
                                Some(existing) => {
                                    let mut previous = Json::Array(existing.iter().map(|item| (**item).clone()).collect());
                                    let changed = store::reconcile(&mut previous, target);
                                    if changed {
                                        if let Json::Array(items) = previous {
                                            *existing = Rc::new(items.into_iter().map(Rc::new).collect());
                                        }
                                    }
                                    changed
                                }
                                None => {
                                    store.session.pending.insert(session.clone(), Rc::new(merged.clone()));
                                    true
                                }
                            };
                            if changed {
                                keys.push(Key::Pending(id(&session)));
                            }
                        });
                        for item in &merged {
                            inner.materialize_inbox_message(item);
                        }
                    });
                    Ok(())
                }
                .await;
                let current = inner.pending_updates.borrow().get(&session).is_some_and(|u| Rc::ptr_eq(u, &updates));
                if current {
                    inner.pending_updates.borrow_mut().remove(&session);
                }
                result
            })
        });
        self.0.sync.run(&key, load)
    }

    pub fn invalidate(&self, session: &str) {
        self.0.sync.invalidate(Some(&format!("session.pending:{session}")));
    }
}

/// `session.message`.
#[derive(Clone)]
pub struct MessageApi(pub(super) Rc<Inner>);

impl MessageApi {
    /// The transcript (deep: every change to it notifies).
    pub fn list(&self, session: &str) -> List {
        self.0.read(Key::Messages(id(session)), |store| store.session.message.get(session).cloned().unwrap_or_else(empty))
    }

    /// The transcript's message IDs in order (notifies only when messages come, go or move).
    pub fn ids(&self, session: &str) -> Vec<String> {
        self.0.read(Key::MessageIds(id(session)), |store| {
            store
                .session
                .message
                .get(session)
                .map(|l| l.iter().map(|m| m.str("id").unwrap_or("").to_owned()).collect())
                .unwrap_or_default()
        })
    }

    /// `message.get(sessionID, messageID)` (by the transcript's index).
    pub fn get(&self, session: &str, message: &str) -> Option<Record> {
        self.0.track(Key::MessageIds(id(session)));
        let position = self.0.message_index.borrow().get(session).and_then(|index| index.get(message).copied())?;
        self.0.read(Key::Message(id(session), id(message)), |store| store.session.message.get(session).and_then(|l| l.get(position).cloned()))
    }

    /// One part of an assistant message (notifies only when that part changes).
    pub fn part(&self, session: &str, message: &str, index: usize) -> Option<Json> {
        self.0.track(Key::Part(id(session), id(message), index));
        let position = self.0.message_index.borrow().get(session).and_then(|i| i.get(message).copied())?;
        let store = self.0.store.borrow();
        store.session.message.get(session)?.get(position)?.arr("content")?.get(index).cloned()
    }

    pub fn more(&self, session: &str) -> bool {
        self.0.read(Key::MessageCursor(id(session)), |store| store.session.message_cursor.contains_key(session))
    }

    pub fn loading(&self, session: &str) -> bool {
        self.0.read(Key::MessageLoading(id(session)), |store| store.session.message_loading.get(session).copied().unwrap_or(false))
    }

    /// `message.sync(sessionID)`: the latest page.
    pub fn sync(&self, session: &str) -> Promise<()> {
        let inner = self.0.clone();
        let session = session.to_owned();
        let key = format!("session.message:{session}");
        let load: Load = Box::new(move || {
            let started = inner.deletion_epoch.get();
            let limit = inner.initial_message_limit.as_ref().map(|f| f()).unwrap_or(super::MESSAGE_PAGE_LIMIT);
            let call = inner.api().api().message().list(
                &obj! { "sessionID" => session.as_str(), "limit" => limit, "order" => "desc" },
                RequestOptions::default(),
            );
            Box::pin(async move {
                let response = match call.await {
                    Ok(value) => value.json(),
                    Err(error) => return Err(inner.evict_missing(error)),
                };
                if inner.deleted_at.borrow().get(&session).copied().unwrap_or(0) > started {
                    return Ok(());
                }
                let fetched: Vec<Json> = response
                    .arr("data")
                    .map(|items| items.iter().rev().cloned().map(|m| stored(&inner, m)).collect())
                    .unwrap_or_default();
                // A re-fetch racing an admission must not wipe its local transcript row.
                let ids: HashSet<String> = fetched.iter().map(|m| m.str("id").unwrap_or("").to_owned()).collect();
                let admitted: Vec<String> = inner
                    .store
                    .borrow()
                    .session
                    .pending
                    .get(&session)
                    .map(|p| {
                        p.iter()
                            .filter(|item| matches!(item.str("type"), Some("user") | Some("synthetic")))
                            .map(|item| item.str("id").unwrap_or("").to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                let admitted_set: indexmap::IndexSet<String> = admitted.iter().cloned().collect();
                let local: Vec<Record> = {
                    let outbox = inner.outbox.borrow();
                    inner
                        .store
                        .borrow()
                        .session
                        .message
                        .get(&session)
                        .map(|l| {
                            l.iter()
                                .filter(|item| {
                                    let item_id = item.str("id").unwrap_or("");
                                    !ids.contains(item_id) && (outbox.contains(item_id) || admitted_set.contains(item_id))
                                })
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let messages: Vec<Json> = fetched.into_iter().chain(local.iter().map(|m| (**m).clone())).collect();
                // A pending row the transcript contains missed its inbox.delivered event while the stream was down.
                let delivered: Vec<String> = admitted_set.iter().filter(|id| ids.contains(*id)).cloned().collect();
                let cursor = response.get("cursor").and_then(|c| c.get("next")).filter(|n| !matches!(n, Json::Null)).cloned();
                inner.batch(|| {
                    for inbox in &delivered {
                        inner.outbox.borrow_mut().shift_remove(inbox);
                        inner.remove_pending(&session, Some(inbox));
                    }
                    *inner.message_index.borrow_mut().entry(session.clone()).or_default() =
                        messages.iter().enumerate().map(|(i, m)| (m.str("id").unwrap_or("").to_owned(), i)).collect();
                    inner.set(|store, keys| {
                        publish_messages(store, &session, messages, keys);
                        set_cursor(store, &session, cursor, keys);
                    });
                });
                Ok(())
            })
        });
        self.0.sync.run(&key, load)
    }

    /// `message.loadMore(sessionID, options)`: the next older page (with `all`, every remaining page). With no load in
    /// flight, the request starts at the call, as the TS function's synchronous part sends it.
    pub fn load_more(&self, session: &str, options: LoadMoreOptions) -> Promise<()> {
        let inner = self.0.clone();
        if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Deferred::resolved(Ok(()));
        }
        if !inner.message_loads.borrow().contains_key(session) {
            return load_page(&inner, session, options);
        }
        let session = session.to_owned();
        promise(async move {
            let signal = options.signal.clone();
            let aborted = || signal.as_ref().is_some_and(AbortSignal::aborted);
            loop {
                let pending = inner.message_loads.borrow().get(&session).cloned();
                let Some(pending) = pending else { break };
                let published = match &signal {
                    None => pending.wait().await?,
                    Some(signal) => {
                        tokio::select! {
                            biased;
                            result = pending.wait() => match result {
                                Ok(published) => published,
                                Err(error) => {
                                    if !signal.aborted() {
                                        return Err(error);
                                    }
                                    false
                                }
                            },
                            _ = signal.wait() => false,
                        }
                    }
                };
                if (!options.all && published) || aborted() {
                    return Ok(());
                }
            }
            load_page(&inner, &session, options).wait().await
        })
    }

    pub fn invalidate(&self, session: &str) {
        self.0.sync.invalidate(Some(&format!("session.message:{session}")));
    }
}


/// The part of `loadMore` after any load in flight has settled: reads from the cursor and publishes once.
fn load_page(inner: &Rc<Inner>, session: &str, options: LoadMoreOptions) -> Promise<()> {
    let signal = options.signal.clone();
    let cursor = inner.store.borrow().session.message_cursor.get(session).cloned();
    let Some(cursor) = cursor.filter(|c| !c.is_empty()) else { return Deferred::resolved(Ok(())) };
    if signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Deferred::resolved(Ok(()));
    }
    let session = session.to_owned();
    inner.set(|store, keys| {
        if store.session.message_loading.get(&session) != Some(&true) {
            store.session.message_loading.insert(session.clone(), true);
            keys.push(Key::MessageLoading(id(&session)));
        }
    });
    let limit = if options.all { 200.0 } else { super::MESSAGE_PAGE_LIMIT };
    let page = |inner: &Rc<Inner>, cursor: Option<Json>| {
        inner.api().api().message().list(
            &obj! { "sessionID" => session.as_str(), "limit" => limit, "cursor" => cursor },
            RequestOptions { signal: signal.clone(), ..Default::default() },
        )
    };
    let first = page(inner, Some(Json::String(cursor)));
    let load_inner = inner.clone();
    let load_session = session.clone();
    let request: Promise<bool> = promise(async move {
        let inner = load_inner;
        let session = load_session;
        let aborted = || signal.as_ref().is_some_and(AbortSignal::aborted);
        let result: Result<bool, Error> = async {
            let mut fetched: Vec<Json> = Vec::new();
            let mut call = first;
            let next = loop {
                let response = call.await?.json();
                if aborted() {
                    return Ok(false);
                }
                if let Some(items) = response.arr("data") {
                    fetched.extend(items.iter().cloned().map(|m| stored(&inner, m)));
                }
                let next = response.get("cursor").and_then(|c| c.get("next")).filter(|n| !matches!(n, Json::Null)).cloned();
                if !options.all || next.as_ref().is_none_or(|n| !crate::json::truthy(n)) {
                    break next;
                }
                call = inner.api().api().message().list(
                    &obj! { "sessionID" => session.as_str(), "limit" => limit, "cursor" => next },
                    RequestOptions { signal: signal.clone(), ..Default::default() },
                );
            };
            // A jump through history publishes once, not once per page of offscreen messages.
            let existing: List = inner.store.borrow().session.message.get(&session).cloned().unwrap_or_else(empty);
            let ids: HashSet<String> = existing.iter().map(|m| m.str("id").unwrap_or("").to_owned()).collect();
            let messages: Vec<Json> = fetched
                .into_iter()
                .rev()
                .filter(|m| !ids.contains(m.str("id").unwrap_or("")))
                .chain(existing.iter().map(|m| (**m).clone()))
                .collect();
            inner.batch(|| {
                if let Some(before) = &options.before_publish {
                    before();
                }
                *inner.message_index.borrow_mut().entry(session.clone()).or_default() =
                    messages.iter().enumerate().map(|(i, m)| (m.str("id").unwrap_or("").to_owned(), i)).collect();
                inner.set(|store, keys| {
                    publish_messages(store, &session, messages, keys);
                    set_cursor(store, &session, next.clone(), keys);
                });
            });
            Ok(true)
        }
        .await;
        let result = match result {
            Err(_) if aborted() => Ok(false),
            other => other,
        };
        inner.set(|store, keys| {
            if store.session.message_loading.get(&session) != Some(&false) {
                store.session.message_loading.insert(session.clone(), false);
                keys.push(Key::MessageLoading(id(&session)));
            }
        });
        result
    });
    inner.message_loads.borrow_mut().insert(session.clone(), request.clone());
    let watched = request.clone();
    let weak = Rc::downgrade(inner);
    let key = session.clone();
    runtime::spawn(async move {
        let _ = watched.wait().await;
        if let Some(inner) = weak.upgrade() {
            let current = inner.message_loads.borrow().get(&key).is_some_and(|p| p.ptr_eq(&watched));
            if current {
                inner.message_loads.borrow_mut().remove(&key);
            }
        }
    });
    promise(async move { request.wait().await.map(|_| ()) })
}

/// `setStore(path, key, list)`: `undefined` deletes the entry. (A value that is not a list cannot be held: the
/// server always sends one.)
fn set_list(map: &mut indexmap::IndexMap<String, List>, key: &str, value: crate::client::Value) {
    match value {
        crate::client::Value::Json(Json::Array(items)) => {
            map.insert(key.to_owned(), Rc::new(items.into_iter().map(Rc::new).collect()));
        }
        crate::client::Value::Json(_) => {
            map.insert(key.to_owned(), empty());
        }
        _ => {
            map.shift_remove(key);
        }
    }
}

/// `stored(message)`: drops inline tool-result files when the store omits them.
fn stored(inner: &Inner, message: Json) -> Json {
    if inner.omit_tool_file_data { omit_message_file_data(message) } else { message }
}

/// `setStore("session", "message", sessionID, reconcile(messages))`, with the keys of what changed.
fn publish_messages(store: &mut store::Store, session: &str, messages: Vec<Json>, keys: &mut Vec<Key>) {
    let previous = store.session.message.get(session).cloned();
    let Some(previous) = previous else {
        let list: Vec<Record> = messages.into_iter().map(Rc::new).collect();
        for message in &list {
            keys.push(Key::Message(id(session), id(message.str("id").unwrap_or(""))));
        }
        keys.push(Key::MessageIds(id(session)));
        store.session.message.insert(session.to_owned(), Rc::new(list));
        return;
    };
    let before_ids: Vec<&str> = previous.iter().map(|m| m.str("id").unwrap_or("")).collect();
    let after_ids: Vec<String> = messages.iter().map(|m| m.str("id").unwrap_or("").to_owned()).collect();
    if before_ids.iter().copied().ne(after_ids.iter().map(String::as_str)) {
        keys.push(Key::MessageIds(id(session)));
    }
    let mut target = Json::Array(messages);
    let mut merged = Json::Array(previous.iter().map(|m| (**m).clone()).collect());
    if !store::reconcile(&mut merged, std::mem::take(&mut target)) {
        return;
    }
    let Json::Array(items) = merged else { return };
    let list: Vec<Record> = items
        .into_iter()
        .map(|item| {
            let item_id = item.str("id").unwrap_or("").to_owned();
            match previous.iter().find(|m| m.str("id") == Some(&item_id)) {
                Some(old) if **old == item => old.clone(),
                _ => {
                    keys.push(Key::Message(id(session), id(&item_id)));
                    Rc::new(item)
                }
            }
        })
        .collect();
    store.session.message.insert(session.to_owned(), Rc::new(list));
}

/// `setStore("session", "messageCursor", sessionID, next ?? undefined)`.
fn set_cursor(store: &mut store::Store, session: &str, cursor: Option<Json>, keys: &mut Vec<Key>) {
    let cursor = cursor.map(|c| crate::js::string(&c));
    let changed = match cursor {
        Some(cursor) => store.session.message_cursor.insert(session.to_owned(), cursor.clone()) != Some(cursor),
        None => store.session.message_cursor.shift_remove(session).is_some(),
    };
    if changed {
        keys.push(Key::MessageCursor(id(session)));
    }
}

/// `session.permission`.
#[derive(Clone)]
pub struct PermissionApi(pub(super) Rc<Inner>);

impl PermissionApi {
    pub fn list(&self, session: &str) -> Option<List> {
        self.0.read(Key::Permission(id(session)), |store| store.session.permission.get(session).cloned())
    }

    /// The sessions with pending permission requests.
    pub fn sessions(&self) -> Vec<String> {
        self.0.read(Key::Permissions, |store| {
            store.session.permission.iter().filter(|(_, r)| !r.is_empty()).map(|(id, _)| id.clone()).collect()
        })
    }

    pub fn sync(&self, session: &str) -> Promise<()> {
        let inner = self.0.clone();
        let session = session.to_owned();
        let key = format!("session.permission:{session}");
        let load: Load = Box::new(move || {
            let started = inner.deletion_epoch.get();
            let call = inner.api().api().permission().list(&obj! { "sessionID" => session.as_str() }, RequestOptions::default());
            Box::pin(async move {
                let list = match call.await {
                    Ok(value) => value,
                    Err(error) => return Err(inner.evict_missing(error)),
                };
                if inner.deleted_at.borrow().get(&session).copied().unwrap_or(0) > started {
                    return Ok(());
                }
                inner.set(|store, keys| {
                    set_list(&mut store.session.permission, &session, list);
                    keys.push(Key::Permission(id(&session)));
                });
                Ok(())
            })
        });
        self.0.sync.run(&key, load)
    }

    pub fn invalidate(&self, session: &str) {
        self.0.sync.invalidate(Some(&format!("session.permission:{session}")));
    }

    /// `permission.reply(input)`: `{ sessionID, requestID, reply, message? }`.
    pub fn reply(&self, input: Json) -> Promise<()> {
        let inner = self.0.clone();
        let call = inner.api().api().permission().reply(&input, RequestOptions::default());
        promise(async move {
            if let Err(error) = call.await {
                if !error.is("PermissionNotFoundError") {
                    return Err(error);
                }
            }
            inner.remove_permission(input.str("sessionID").unwrap_or(""), input.str("requestID").unwrap_or(""));
            Ok(())
        })
    }
}

/// `session.form`.
#[derive(Clone)]
pub struct FormApi(pub(super) Rc<Inner>);

impl FormApi {
    /// The pending forms of a session; for `global`, those of a location (none without one).
    pub fn list(&self, session: &str, location: Option<&Json>) -> Option<List> {
        let forms = self.0.read(Key::Form(id(session)), |store| store.session.form.get(session).cloned());
        if session != "global" {
            return forms;
        }
        let location = location?;
        let key = location_key(location);
        forms.map(|forms| {
            Rc::new(forms.iter().filter(|f| f.get("location").is_some_and(|l| location_key(l) == key)).cloned().collect())
        })
    }

    pub fn sessions(&self) -> Vec<String> {
        self.0.read(Key::Forms, |store| {
            store.session.form.iter().filter(|(id, f)| *id != "global" && !f.is_empty()).map(|(id, _)| id.clone()).collect()
        })
    }

    fn key(&self, session: &str, location: Option<&Json>) -> String {
        let suffix = if session == "global" { location_key(location.unwrap_or(&self.0.default_location())) } else { String::new() };
        format!("session.form:{session}:{suffix}")
    }

    pub fn sync(&self, session: &str, location: Option<&Json>) -> Promise<()> {
        let inner = self.0.clone();
        let key = self.key(session, location);
        let session = session.to_owned();
        let location = location.cloned().unwrap_or_else(|| inner.default_location());
        let load: Load = Box::new(move || {
            if session == "global" {
                let call = inner.api().api().form().list(&obj! { "location" => location_query(&location) }, RequestOptions::default());
                return Box::pin(async move {
                    let response = call.await?.json();
                    let location = obj! { "directory" => response.get("location").and_then(|l| l.get("directory")) };
                    let location_id = location_key(&location);
                    inner.set(|store, keys| {
                        let kept: Vec<Record> = store
                            .session
                            .form
                            .get("global")
                            .map(|forms| {
                                forms
                                    .iter()
                                    .filter(|f| f.get("location").is_some_and(|l| location_key(l) != location_id))
                                    .cloned()
                                    .collect()
                            })
                            .unwrap_or_default();
                        let fresh = response.arr("data").into_iter().flatten().filter(|f| f.str("sessionID") == Some("global")).map(|f| {
                            let mut form = f.clone();
                            form.set("location", location.clone());
                            Rc::new(form)
                        });
                        let list: Vec<Record> = kept.into_iter().chain(fresh).collect();
                        store.session.form.insert("global".into(), Rc::new(list));
                        keys.push(Key::Form(id("global")));
                    });
                    Ok(())
                });
            }
            let started = inner.deletion_epoch.get();
            let call = inner.api().api().session().form().list(&obj! { "sessionID" => session.as_str() }, RequestOptions::default());
            Box::pin(async move {
                let list = match call.await {
                    Ok(value) => value,
                    Err(error) => return Err(inner.evict_missing(error)),
                };
                if inner.deleted_at.borrow().get(&session).copied().unwrap_or(0) > started {
                    return Ok(());
                }
                inner.set(|store, keys| {
                    set_list(&mut store.session.form, &session, list);
                    keys.push(Key::Form(id(&session)));
                });
                Ok(())
            })
        });
        self.0.sync.run(&key, load)
    }

    pub fn invalidate(&self, session: &str, location: Option<&Json>) {
        self.0.sync.invalidate(Some(&self.key(session, location)));
    }

    /// `form.reply(input, ref)`: `{ sessionID, formID, answers }`.
    pub fn reply(&self, input: Json, location: Option<&Json>) -> Promise<()> {
        let options = form_request_options(input.str("sessionID").unwrap_or(""), location);
        let call = self.0.api().api().session().form().reply(&input, options);
        self.settle(input, location, call)
    }

    /// `form.cancel(input, ref)`: `{ sessionID, formID }`.
    pub fn cancel(&self, input: Json, location: Option<&Json>) -> Promise<()> {
        let options = form_request_options(input.str("sessionID").unwrap_or(""), location);
        let call = self.0.api().api().session().form().cancel(&input, options);
        self.settle(input, location, call)
    }

    /// `settleForm(input, ref, request)`: a form already gone counts as settled.
    fn settle(&self, input: Json, location: Option<&Json>, call: crate::client::Call) -> Promise<()> {
        let this = self.clone();
        let location = location.cloned();
        promise(async move {
            if let Err(error) = call.await {
                let gone = error.is("FormNotFoundError") || error.is("FormAlreadySettledError");
                if !gone || error.field("id") != input.get("formID") {
                    return Err(error);
                }
            }
            let session = input.str("sessionID").unwrap_or("").to_owned();
            if !this.0.remove_form(&session, input.str("formID").unwrap_or(""), location.as_ref()) {
                return Ok(());
            }
            this.invalidate(&session, location.as_ref());
            let _ = this.sync(&session, location.as_ref());
            Ok(())
        })
    }
}

/// `data.project`.
#[derive(Clone)]
pub struct ProjectApi {
    pub permission: ProjectPermissionApi,
    pub(super) inner: Rc<Inner>,
}

impl ProjectApi {
    /// The projects not archived, most recently active first.
    pub fn list(&self) -> Vec<Record> {
        self.inner.read(Key::Projects, |store| {
            let mut projects: Vec<Record> = store
                .project
                .info
                .values()
                .filter(|p| p.get("time").and_then(|t| t.get("archived")).is_none())
                .cloned()
                .collect();
            projects.sort_by(|a, b| {
                let active = |p: &Record| p.get("time").and_then(|t| t.num("active")).unwrap_or(f64::NAN);
                let order = active(b) - active(a);
                if order > 0.0 {
                    std::cmp::Ordering::Greater
                } else if order < 0.0 {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            });
            projects
        })
    }

    pub fn get(&self, project: &str) -> Option<Record> {
        self.inner.read(Key::Project(id(project)), |store| store.project.info.get(project).cloned())
    }

    pub fn sync(&self) -> Promise<()> {
        let inner = self.inner.clone();
        let load: Load = Box::new(move || {
            let call = inner.api().api().project().list(&Json::Null, RequestOptions::default());
            Box::pin(async move {
                let projects = call.await?.json();
                let entries: Vec<(String, Record)> = projects
                    .as_array_ref()
                    .map(|items| items.iter().map(|p| (p.str("id").unwrap_or("").to_owned(), Rc::new(p.clone()))).collect())
                    .unwrap_or_default();
                inner.set(|store, keys| reconcile_records(&mut store.project.info, entries, keys, |k| Key::Project(id(k))));
                Ok(())
            })
        });
        self.inner.sync.run("project", load)
    }

    pub fn invalidate(&self) {
        self.inner.sync.invalidate(Some("project"));
    }
}

trait ArrayRef {
    fn as_array_ref(&self) -> Option<&Vec<Json>>;
}

impl ArrayRef for Json {
    fn as_array_ref(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }
}

/// `setStore(path, reconcile(Object.fromEntries(entries)))` over a record of records.
pub(super) fn reconcile_records(
    previous: &mut indexmap::IndexMap<String, Record>,
    entries: Vec<(String, Record)>,
    keys: &mut Vec<Key>,
    key: impl Fn(&str) -> Key,
) {
    let wanted: HashSet<String> = entries.iter().map(|(k, _)| k.clone()).collect();
    previous.retain(|k, _| {
        let keep = wanted.contains(k);
        if !keep {
            keys.push(key(k));
        }
        keep
    });
    for (k, value) in entries {
        match previous.get_mut(&k) {
            Some(existing) => {
                if store::reconcile(Rc::make_mut(existing), (*value).clone()) {
                    keys.push(key(&k));
                }
            }
            None => {
                previous.insert(k.clone(), value);
                keys.push(key(&k));
            }
        }
    }
}

/// `project.permission`.
#[derive(Clone)]
pub struct ProjectPermissionApi(pub(super) Rc<Inner>);

impl ProjectPermissionApi {
    pub fn list(&self, project: &str) -> Option<Record> {
        self.0.read(Key::ProjectPermission(id(project)), |store| store.project.permission.get(project).cloned())
    }

    pub fn sync(&self, project: &str) -> Promise<()> {
        let inner = self.0.clone();
        let project = project.to_owned();
        let key = format!("project.permission:{project}");
        let load: Load = Box::new(move || {
            let call = inner.api().api().permission().saved().list(&obj! { "projectID" => project.as_str() }, RequestOptions::default());
            Box::pin(async move {
                let list = call.await?;
                inner.set(|store, keys| {
                    match list {
                        crate::client::Value::Json(list) => {
                            store.project.permission.insert(project.clone(), Rc::new(list));
                        }
                        _ => {
                            store.project.permission.shift_remove(&project);
                        }
                    }
                    keys.push(Key::ProjectPermission(id(&project)));
                });
                Ok(())
            })
        });
        self.0.sync.run(&key, load)
    }

    pub fn invalidate(&self, project: &str) {
        self.0.sync.invalidate(Some(&format!("project.permission:{project}")));
    }
}

/// `data.shell`: the running shell commands, by location.
#[derive(Clone)]
pub struct ShellApi(pub(super) Rc<Inner>);

impl ShellApi {
    fn resource(&self) -> Resource {
        Resource::new(&self.0, LocationField::Shell)
    }

    pub fn list(&self, location: Option<&Json>) -> Vec<Json> {
        match self.resource().list(location).as_deref() {
            Some(Json::Object(shells)) => shells.values().cloned().collect(),
            _ => Vec::new(),
        }
    }

    pub fn list_by_session(&self, session: &str) -> Vec<Json> {
        self.0.read(Key::Locations, |store| {
            store
                .location
                .values()
                .filter_map(|data| data.get(LocationField::Shell))
                .flat_map(|shells| match &**shells {
                    Json::Object(shells) => shells.values().cloned().collect::<Vec<_>>(),
                    _ => Vec::new(),
                })
                .filter(|shell| shell.get("metadata").and_then(|m| m.get("sessionID")) == Some(&sid(session)))
                .collect()
        })
    }

    pub fn get(&self, shell: &str) -> Option<Json> {
        self.0.read(Key::Locations, |store| {
            store.location.values().find_map(|data| data.get(LocationField::Shell).and_then(|shells| shells.get(shell).cloned()))
        })
    }

    pub fn sync(&self, location: Option<&Json>) -> Promise<()> {
        self.resource().sync(location)
    }

    pub fn invalidate(&self, location: Option<&Json>) {
        self.resource().invalidate(location)
    }
}

/// `data.location`.
#[derive(Clone)]
pub struct LocationApi {
    pub vcs: Resource,
    pub usage: Resource,
    pub agent: Resource,
    pub command: Resource,
    pub config: Resource,
    pub integration: Resource,
    pub mcp: McpApi,
    pub model: Resource,
    pub provider: Resource,
    pub reference: Resource,
    pub websearch: WebsearchApi,
    pub skill: Resource,
    pub(super) inner: Rc<Inner>,
}

/// `location.mcp`.
#[derive(Clone)]
pub struct McpApi {
    pub server: Resource,
    pub resource: Resource,
}

impl LocationApi {
    pub(super) fn new(inner: &Rc<Inner>) -> Self {
        let r = |field| Resource::new(inner, field);
        Self {
            vcs: r(LocationField::Vcs),
            usage: r(LocationField::UsageLimits),
            agent: r(LocationField::Agent),
            command: r(LocationField::Command),
            config: r(LocationField::Config),
            integration: r(LocationField::Integration),
            mcp: McpApi { server: r(LocationField::McpServer), resource: r(LocationField::McpResource) },
            model: r(LocationField::Model),
            provider: r(LocationField::Provider),
            reference: r(LocationField::Reference),
            websearch: WebsearchApi(inner.clone()),
            skill: r(LocationField::Skill),
            inner: inner.clone(),
        }
    }

    /// `location.info(ref)`.
    pub fn info(&self, location: Option<&Json>) -> Option<Record> {
        if location.is_none() {
            self.inner.track(Key::DefaultLocation);
        }
        let key = location_key(location.unwrap_or(&self.inner.default_location()));
        self.inner.read(Key::LocationField(id(&key), LocationField::Info), |store| {
            store.location.get(&key).and_then(|data| data.get(LocationField::Info).cloned())
        })
    }

    /// `location.default()`.
    pub fn default(&self) -> Json {
        self.inner.track(Key::DefaultLocation);
        self.inner.default_location()
    }

    /// `location.syncInfo(ref)`.
    pub fn sync_info(&self, location: Option<&Json>) -> Promise<()> {
        let inner = self.inner.clone();
        let given = location.is_some();
        let current = location.cloned().unwrap_or_else(|| inner.default_location());
        let key = format!("location:{}", location_key(&current));
        let load: Load = Box::new(move || {
            let call = inner.api().api().location().get(&obj! { "location" => location_query(&current) }, RequestOptions::default());
            Box::pin(async move {
                let location = call.await?.json();
                let key = location_key(&location);
                inner.set(|store, keys| {
                    if !store.location.contains_key(&key) {
                        store.location.insert(key.clone(), Default::default());
                        keys.push(Key::Location(id(&key)));
                    }
                });
                inner.publish_location(&key, LocationField::Info, Some(location.clone()));
                if !given {
                    let next = obj! { "directory" => location.get("directory") };
                    if *inner.default_location.borrow() != next {
                        *inner.default_location.borrow_mut() = next;
                        inner.notify(vec![Key::DefaultLocation]);
                    }
                }
                Ok(())
            })
        });
        self.inner.sync.run(&key, load)
    }

    /// `location.sync(ref)`: the info, then every catalog.
    pub fn sync(&self, location: Option<&Json>) -> Promise<()> {
        let this = self.clone();
        let location = location.cloned();
        let shell = ShellApi(self.inner.clone());
        let form = FormApi(self.inner.clone());
        let info = self.sync_info(location.as_ref());
        promise(async move {
            info.wait().await?;
            let location = location.unwrap_or_else(|| this.inner.default_location());
            let at = Some(&location);
            let all = [
                this.vcs.sync(at),
                this.agent.sync(at),
                this.command.sync(at),
                this.integration.sync(at),
                this.mcp.server.sync(at),
                this.mcp.resource.sync(at),
                this.model.sync(at),
                this.provider.sync(at),
                this.reference.sync(at),
                this.skill.sync(at),
                shell.sync(at),
                form.sync("global", at),
            ];
            all_settled_first_error(all).await
        })
    }

    /// `location.invalidate(ref)`.
    pub fn invalidate(&self, location: Option<&Json>) {
        let location = location.cloned().unwrap_or_else(|| self.inner.default_location());
        let at = Some(&location);
        self.inner.sync.invalidate(Some(&format!("location:{}", location_key(&location))));
        for resource in [
            &self.vcs,
            &self.agent,
            &self.command,
            &self.config,
            &self.integration,
            &self.mcp.server,
            &self.mcp.resource,
            &self.model,
            &self.provider,
            &self.reference,
            &self.skill,
        ] {
            resource.invalidate(at);
        }
        ShellApi(self.inner.clone()).invalidate(at);
        FormApi(self.inner.clone()).invalidate("global", at);
    }
}

/// `Promise.all(promises)`: rejects with the first rejection to happen, else resolves once all have.
pub(super) async fn all_settled_first_error(promises: impl IntoIterator<Item = Promise<()>>) -> Result<(), Error> {
    let promises: Vec<Promise<()>> = promises.into_iter().collect();
    let first: Deferred<Result<(), Error>> = Deferred::new();
    let remaining = Rc::new(std::cell::Cell::new(promises.len()));
    if promises.is_empty() {
        return Ok(());
    }
    for p in promises {
        let first = first.clone();
        let remaining = remaining.clone();
        runtime::spawn(async move {
            match p.wait().await {
                Err(error) => first.resolve(Err(error)),
                Ok(()) => {
                    remaining.set(remaining.get() - 1);
                    if remaining.get() == 0 {
                        first.resolve(Ok(()));
                    }
                }
            }
        });
    }
    first.wait().await
}

/// `location.websearch`.
#[derive(Clone)]
pub struct WebsearchApi(pub(super) Rc<Inner>);

impl WebsearchApi {
    pub fn list(&self, location: Option<&Json>) -> Option<Record> {
        Resource::new(&self.0, LocationField::Websearch).list(location)
    }

    pub fn refresh(&self, location: Option<&Json>) -> Promise<()> {
        let inner = self.0.clone();
        let location = location.cloned().unwrap_or_else(|| inner.default_location());
        let call =
            inner.api().api().websearch().providers(&obj! { "location" => location_query(&location) }, RequestOptions::default());
        promise(async move {
            let providers = call.await?.json();
            let key = location_key(providers.get("location").unwrap_or(&Json::Null));
            inner.publish_location(&key, LocationField::Websearch, providers.get("data").cloned());
            Ok(())
        })
    }
}

