//! `createData` (packages/client/src/solid/data.ts): server events applied to, and API reads cached in, one store.
//!
//! The store keeps the TS store's shape and field names ([`store::Store`]; records are the JSON values the server
//! sent, or that events built, as in TS). Every read goes through the same accessors as TS (`data.session.get(id)`,
//! `data.session.message.list(id)`, `data.location.model.list(ref)`, ...), which report what they read to the
//! reactive layer ([`crate::reactive`]); every change reports what it changed.
//!
//! Methods TS writes as `async` return a [`Promise`]: the request starts when the method is called (as the TS
//! promise's synchronous part runs), and the promise can be awaited any number of times.
//!
//! Divergences:
//! - TS `console.error`s a failed event-driven read when no `onError` is given; here it goes to the `log` sink (the
//!   TUI's console is not the terminal).
//! - Records are values here, while the TS store shares objects. Two of those sharings are observable in TS and not
//!   reproduced (TODO(port), both unreachable with the server, which names `previous` and returns the canonical
//!   location the client already uses):
//!   - a `model-switched` row whose `previous` came from the session's own `model` (the event named none) keeps
//!     following that model object, which later `session.model.selected` events merge into;
//!   - a session created without a location holds the default location's object, so a server answer at another
//!     directory (reconciled into it) or a `session.moved` (merged into it) moves the default location too.
//! - A key of a record set to `undefined` keeps its place in a JS object (and JSON drops it); a JSON value cannot hold
//!   it, so such a key comes back at the end when it is set again. Only key order inside records differs.
//! - Responses are trusted to have the shapes the server's schemas give them, as in TS (neither side decodes them),
//!   but a response of another shape is read leniently here (a list that is not an array is empty), where TS fails
//!   the read with a `TypeError` (`snapshot.map is not a function`). The server cannot send one.

mod api;
mod events;
pub mod store;
mod sync;

pub use api::{
    CreateInput, Created, FormApi, InputApi, LoadMoreOptions, LocationApi, McpApi, MessageApi, PendingApi, PermissionApi,
    ProjectApi, ProjectPermissionApi, PromptInput, SessionApi, ShellApi, WebsearchApi,
};

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};
use opencode_protocol::Json;
use opencode_protocol::wire::Object;


use crate::bus::{Emitter, Unsubscribe};
use crate::client::{Client, Error, RequestOptions};
use crate::connection::{ClientConnection, Status};
use crate::event::Event;
use crate::json::JsonExt;
use crate::obj;
use crate::reactive::{self, Key, LocationField, Reactivity};
use crate::runtime::{self, Deferred, Log, LogLevel, Timeout};
use crate::transport::LocalBoxFuture;

pub use store::{SessionStatus, location_key};
use store::{List, Record, Store};
use sync::{Load, Sync};

/// A pending result (a TS promise): settles once, awaited by anyone.
pub type Promise<T> = Deferred<Result<T, Error>>;

/// Where a pending user prompt stands relative to the runner (`DataPendingStatus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingStatus {
    /// Promoted at the next idle boundary.
    Starting,
    /// Waits for the next step boundary of an execution that already delivered input.
    Steering,
    /// Outlived an execution that ended without delivering it.
    Stranded,
    /// Waits for every steer to drain.
    Queued,
}

impl PendingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PendingStatus::Starting => "starting",
            PendingStatus::Steering => "steering",
            PendingStatus::Stranded => "stranded",
            PendingStatus::Queued => "queued",
        }
    }
}

/// What the data layer reads the connection's status from (`connection.status()` and an effect on it).
pub trait ConnectionState {
    fn status(&self) -> Status;
    /// Calls `listener` whenever the status changes.
    fn on_status(&self, listener: Box<dyn Fn(Status)>);
}

impl ConnectionState for ClientConnection {
    fn status(&self) -> Status {
        self.status_untracked()
    }
    fn on_status(&self, listener: Box<dyn Fn(Status)>) {
        ClientConnection::on_status(self, listener)
    }
}

/// `CreateDataInput`.
pub struct CreateDataInput {
    pub api: Rc<dyn Fn() -> Client>,
    pub directory: String,
    /// Raw-message window used for an initial transcript read. Older pages retain their normal size.
    pub initial_message_limit: Option<Rc<dyn Fn() -> f64>>,
    pub event: Emitter,
    pub connection: Option<Rc<dyn ConnectionState>>,
    /// Receives failed event-driven reads. Explicit reads still reject to their caller.
    pub on_error: Option<Rc<dyn Fn(&Error)>>,
    /// Drops the payload of inline `data:` files in tool results before they enter the store.
    pub omit_tool_file_data: bool,
    pub reactivity: Rc<dyn Reactivity>,
    pub log: Option<Log>,
}

const MESSAGE_PAGE_LIMIT: f64 = 20.0;
/// Trailing window for event bursts that each ask for the same refetch.
pub const SETTLE_MS: f64 = 150.0;
const CHILD_PAGE: f64 = 200.0;
const DELETED_SESSION_LIMIT: usize = 512;

/// An update the pending sync reconciles: an item, a delivery change, or a removal.
#[derive(Clone)]
enum PendingUpdate {
    Item(Record),
    Delivery(String),
    Removed,
}

struct Compacting {
    id: String,
    observed: Rc<RefCell<HashSet<String>>>,
    request: Promise<Json>,
}

pub(crate) struct Inner {
    api: Rc<dyn Fn() -> Client>,
    initial_message_limit: Option<Rc<dyn Fn() -> f64>>,
    event: Emitter,
    connection: Option<Rc<dyn ConnectionState>>,
    on_error: Option<Rc<dyn Fn(&Error)>>,
    omit_tool_file_data: bool,
    reactivity: Rc<dyn Reactivity>,
    log: Option<Log>,
    disposed: Cell<bool>,
    settling: RefCell<HashMap<String, Timeout>>,
    store: RefCell<Store>,
    default_location: RefCell<Json>,
    message_index: RefCell<HashMap<String, HashMap<String, usize>>>,
    sync: Rc<Sync>,
    active_updates: RefCell<Option<Rc<RefCell<IndexMap<String, Option<SessionStatus>>>>>>,
    pending_updates: RefCell<HashMap<String, Rc<RefCell<IndexMap<String, PendingUpdate>>>>>,
    /// Inbox IDs of optimistic admissions awaiting acknowledgement.
    outbox: RefCell<IndexSet<String>>,
    /// Session IDs of optimistic creates awaiting acknowledgement.
    session_outbox: RefCell<IndexSet<String>>,
    /// In-flight optimistic creates by session ID.
    creating: RefCell<HashMap<String, Promise<Json>>>,
    /// Per-session send chain: admissions in submission order (never rejects).
    sending: RefCell<HashMap<String, Deferred<()>>>,
    message_loads: RefCell<HashMap<String, Promise<bool>>>,
    compacting: RefCell<HashMap<String, Compacting>>,
    deletion_epoch: Cell<u64>,
    deleted_at: RefCell<IndexMap<String, u64>>,
    listener: RefCell<Option<Unsubscribe>>,
}

/// The data layer: the result of `createData`.
#[derive(Clone)]
pub struct Data {
    pub session: SessionApi,
    pub project: ProjectApi,
    pub shell: ShellApi,
    pub location: LocationApi,
    inner: Rc<Inner>,
}

// ---- the store: reads and changes --------------------------------------------------------------------------

impl Inner {
    fn api(&self) -> Client {
        (self.api)()
    }

    fn connected(&self) -> bool {
        self.connection.as_ref().is_none_or(|c| c.status() == Status::Connected)
    }

    fn track(&self, key: Key) {
        self.reactivity.track(&key);
    }

    fn read<R>(&self, key: Key, f: impl FnOnce(&Store) -> R) -> R {
        self.reactivity.track(&key);
        f(&self.store.borrow())
    }

    /// Changes the store; the keys `f` reports are triggered once the store is released, in one batch.
    fn set<R>(&self, f: impl FnOnce(&mut Store, &mut Vec<Key>) -> R) -> R {
        let mut keys = Vec::new();
        let result = f(&mut self.store.borrow_mut(), &mut keys);
        self.notify(keys);
        result
    }

    fn notify(&self, mut keys: Vec<Key>) {
        if keys.is_empty() {
            return;
        }
        let mut seen = HashSet::new();
        keys.retain(|key| seen.insert(key.clone()));
        let reactivity = self.reactivity.clone();
        reactive::batch(&*reactivity, || keys.iter().for_each(|key| reactivity.trigger(key)));
    }

    fn batch<R>(&self, f: impl FnOnce() -> R) -> R {
        reactive::batch(&*self.reactivity, f)
    }

    fn info(&self, session: &str) -> Option<Record> {
        self.store.borrow().session.info.get(session).cloned()
    }

    fn has_info(&self, session: &str) -> bool {
        self.store.borrow().session.info.contains_key(session)
    }

    fn default_location(&self) -> Json {
        self.default_location.borrow().clone()
    }

    fn log(&self, message: &str, error: &Error) {
        if let Some(log) = &self.log {
            log(LogLevel::Error, message, &obj! { "error" => error.message() });
        }
    }
}

fn id(text: &str) -> reactive::Id {
    Rc::from(text)
}

fn sid(text: &str) -> Json {
    Json::String(text.to_owned())
}

/// `{ directory: ref.directory }` (`locationQuery`).
fn location_query(location: &Json) -> Json {
    obj! { "directory" => location.get("directory") }
}

/// Settles a promise with `future`, run as a local task (any requests in it were started by their `Call`s).
fn promise<T: Clone + 'static>(future: impl std::future::Future<Output = Result<T, Error>> + 'static) -> Promise<T> {
    Deferred::spawn(future)
}

fn is_session_not_found(error: &Error) -> bool {
    error.is("SessionNotFoundError")
}

// ---- message helpers (`message` in data.ts) -----------------------------------------------------------------

/// The parts of a transcript change, for the keys it triggers.
struct MessageChanges<'a> {
    session: &'a str,
    keys: &'a mut Vec<Key>,
}

impl MessageChanges<'_> {
    fn ids(&mut self) {
        self.keys.push(Key::MessageIds(id(self.session)));
    }
    fn message(&mut self, message: &str) {
        self.keys.push(Key::Message(id(self.session), id(message)));
    }
    fn head(&mut self, message: &str) {
        self.keys.push(Key::MessageHead(id(self.session), id(message)));
    }
    fn parts(&mut self, message: &str) {
        self.keys.push(Key::Parts(id(self.session), id(message)));
    }
    fn part(&mut self, message: &str, index: usize) {
        self.keys.push(Key::Part(id(self.session), id(message), index));
    }
    fn all(&mut self) {
        self.keys.push(Key::Messages(id(self.session)));
    }
}

type Index = HashMap<String, usize>;

impl Inner {
    /// `message.update(sessionID, fn)`: the transcript (created empty if absent) and its index.
    fn update_messages<R>(&self, session: &str, f: impl FnOnce(&mut Vec<Record>, &mut Index, &mut MessageChanges) -> R) -> R {
        let mut index_map = self.message_index.borrow_mut();
        let index = index_map.entry(session.to_owned()).or_default();
        self.set(|store, keys| {
            let list = store.session.message.entry(session.to_owned()).or_insert_with(|| Rc::new(Vec::new()));
            let mut changes = MessageChanges { session, keys };
            f(Rc::make_mut(list), index, &mut changes)
        })
    }

    /// `message.append(messages, index, item)`.
    fn append(messages: &mut Vec<Record>, index: &mut Index, item: Json, changes: &mut MessageChanges) {
        let item_id = item.str("id").unwrap_or("").to_owned();
        if index.contains_key(&item_id) {
            return;
        }
        index.insert(item_id.clone(), messages.len());
        messages.push(Rc::new(item));
        changes.ids();
        changes.message(&item_id);
    }

    /// `message.insert(sessionID, item)`.
    fn insert_message(&self, session: &str, item: Json) {
        self.update_messages(session, |messages, index, changes| Self::append(messages, index, item, changes));
    }

    /// `message.reindex(messages, index, start)`.
    fn reindex(messages: &[Record], index: &mut Index, start: usize) {
        for (position, item) in messages.iter().enumerate().skip(start) {
            index.insert(item.str("id").unwrap_or("").to_owned(), position);
        }
    }

    /// `message.editAssistant(sessionID, messageID, fn)`: a missing target (never loaded or evicted) drops the edit.
    fn edit_assistant(&self, session: &str, message: &str, f: impl FnOnce(&mut Json, &mut MessageChanges)) {
        let known = self.message_index.borrow().get(session).is_some_and(|index| index.contains_key(message));
        if !known {
            return;
        }
        self.update_messages(session, |messages, index, changes| {
            let Some(position) = index.get(message).copied() else { return };
            let Some(item) = messages.get_mut(position) else { return };
            if item.str("type") != Some("assistant") {
                return;
            }
            f(Rc::make_mut(item), changes);
        })
    }

    /// The latest part of the assistant's content matching `test`, edited by `f` (`editTool`, `editText`,
    /// `editReasoning`).
    fn edit_part(&self, session: &str, message: &str, test: impl Fn(&Json) -> bool, f: impl FnOnce(&mut Json)) {
        let message_id = message.to_owned();
        self.edit_assistant(session, message, |assistant, changes| {
            let Some(Json::Array(content)) = assistant.get_mut("content") else { return };
            let Some(position) = content.iter().rposition(&test) else { return };
            f(&mut content[position]);
            changes.part(&message_id, position);
        });
    }
}

/// `message.activeAssistant(messages)`: the step in progress, or a failed step whose retry is pending.
fn active_assistant(messages: &[Record]) -> Option<usize> {
    let position = messages.iter().rposition(|item| {
        item.str("type") == Some("assistant")
            && (!item.get("time").and_then(|t| t.get("completed")).is_some_and(crate::json::truthy) || item.get("retry").is_some())
    })?;
    Some(position)
}

/// `omitFileData(content)`: an empty `data:<mime>;base64,` URI in place of each inline file.
fn omit_file_data(content: &Json) -> Json {
    match content {
        Json::Array(items) => Json::Array(
            items
                .iter()
                .map(|item| {
                    let inline = item.str("type") == Some("file") && item.str("uri").is_some_and(|uri| uri.starts_with("data:"));
                    if !inline {
                        return item.clone();
                    }
                    let mut item = item.clone();
                    let mime = item.get("mime").map(crate::js::string).unwrap_or_else(|| "undefined".into());
                    item.set("uri", Json::String(format!("data:{mime};base64,")));
                    item
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `omitMessageFileData(message)`.
fn omit_message_file_data(message: Json) -> Json {
    if message.str("type") != Some("assistant") {
        return message;
    }
    let mut message = message;
    if let Some(Json::Array(content)) = message.get_mut("content") {
        for part in content.iter_mut() {
            if part.str("type") != Some("tool") {
                continue;
            }
            let has = part.get("state").and_then(|s| s.get("content")).is_some_and(crate::json::truthy);
            if has {
                let state = part.get_mut("state").expect("tool state");
                let omitted = omit_file_data(state.get("content").expect("content"));
                state.set("content", omitted);
            }
        }
    }
    message
}

/// `optimisticFiles(files)`: inline `data:` URLs become files of the optimistic row.
fn optimistic_files(files: Option<&Json>) -> Option<Json> {
    let items: Vec<Json> = files?
        .as_array()?
        .iter()
        .filter_map(|file| {
            let uri = file.str("uri")?;
            let rest = uri.strip_prefix("data:")?;
            let comma = rest.find(',')?;
            let header = &rest[..comma];
            let parts: Vec<&str> = header.split(';').collect();
            if !parts[1..].iter().any(|part| part.to_lowercase() == "base64") {
                return None;
            }
            let mut item = obj! {
                "data" => &rest[comma + 1..],
                "mime" => if parts[0].is_empty() { "application/octet-stream" } else { parts[0] },
                "source" => obj! { "type" => "inline" },
            };
            for key in ["name", "description"] {
                if let Some(value) = file.get(key) {
                    item.set(key, value.clone());
                }
            }
            if let Some(mention) = file.get("mention") {
                item.set("mention", mention.clone());
            }
            Some(item)
        })
        .collect();
    if items.is_empty() { None } else { Some(Json::Array(items)) }
}

trait AsArray {
    fn as_array(&self) -> Option<&Vec<Json>>;
}

impl AsArray for Json {
    fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }
}

/// `formRequestOptions(sessionID, ref)`: global forms name their location in a header.
fn form_request_options(session: &str, location: Option<&Json>) -> RequestOptions {
    match (session, location) {
        ("global", Some(location)) => RequestOptions {
            headers: vec![(
                "x-opencode-directory".into(),
                crate::js::encode_uri_component(&location.get("directory").map(crate::js::string).unwrap_or_default()),
            )],
            ..Default::default()
        },
        _ => RequestOptions::default(),
    }
}

// ---- creation -----------------------------------------------------------------------------------------------

impl Data {
    /// `createData(config)`.
    pub fn create(config: CreateDataInput) -> Data {
        let inner = Rc::new(Inner {
            api: config.api,
            initial_message_limit: config.initial_message_limit,
            event: config.event.clone(),
            connection: config.connection.clone(),
            on_error: config.on_error,
            omit_tool_file_data: config.omit_tool_file_data,
            reactivity: config.reactivity,
            log: config.log,
            disposed: Cell::new(false),
            settling: RefCell::new(HashMap::new()),
            store: RefCell::new(Store::default()),
            default_location: RefCell::new(obj! { "directory" => config.directory.as_str() }),
            message_index: RefCell::new(HashMap::new()),
            sync: Rc::new(Sync::default()),
            active_updates: RefCell::new(None),
            pending_updates: RefCell::new(HashMap::new()),
            outbox: RefCell::new(IndexSet::new()),
            session_outbox: RefCell::new(IndexSet::new()),
            creating: RefCell::new(HashMap::new()),
            sending: RefCell::new(HashMap::new()),
            message_loads: RefCell::new(HashMap::new()),
            compacting: RefCell::new(HashMap::new()),
            deletion_epoch: Cell::new(0),
            deleted_at: RefCell::new(IndexMap::new()),
            listener: RefCell::new(None),
        });
        let data = Data::wrap(inner.clone());
        // `createEffect`: every status other than connected makes cached reads stale. Its first run comes after the
        // synchronous setup that created it, as a Solid effect's does.
        let weak = Rc::downgrade(&inner);
        runtime::spawn(async move {
            if let Some(inner) = weak.upgrade() {
                // `config.connection?.status() === "connected"`: without a connection, never.
                if inner.connection.as_ref().map(|c| c.status()) != Some(Status::Connected) {
                    inner.sync.invalidate(None);
                }
            }
        });
        if let Some(connection) = &config.connection {
            let weak = Rc::downgrade(&inner);
            connection.on_status(Box::new(move |status| {
                if let Some(inner) = weak.upgrade() {
                    if status != Status::Connected {
                        inner.sync.invalidate(None);
                    }
                }
            }));
        }
        let weak = Rc::downgrade(&inner);
        let unsubscribe = config.event.listen(move |event| {
            if let Some(inner) = weak.upgrade() {
                Data::wrap(inner).handle_event(event);
            }
        });
        *inner.listener.borrow_mut() = Some(unsubscribe);
        data
    }

    fn wrap(inner: Rc<Inner>) -> Data {
        Data {
            session: SessionApi {
                input: InputApi(inner.clone()),
                pending: PendingApi(inner.clone()),
                message: MessageApi(inner.clone()),
                permission: PermissionApi(inner.clone()),
                form: FormApi(inner.clone()),
                inner: inner.clone(),
            },
            project: ProjectApi { permission: ProjectPermissionApi(inner.clone()), inner: inner.clone() },
            shell: ShellApi(inner.clone()),
            location: LocationApi::new(&inner),
            inner,
        }
    }

    /// The owner's cleanup.
    pub fn dispose(&self) {
        let inner = &self.inner;
        inner.disposed.set(true);
        for (_, timer) in inner.settling.borrow_mut().drain() {
            timer.clear();
        }
        inner.compacting.borrow_mut().clear();
        if let Some(unsubscribe) = inner.listener.borrow_mut().take() {
            unsubscribe.call();
        }
    }

    /// `data.on(type, handler)` (the client context's event bus).
    pub fn on(&self, r#type: &str, handler: impl Fn(&Event) + 'static) -> Unsubscribe {
        self.inner.event.on(r#type, handler)
    }

    /// `data.listen(handler)`.
    pub fn listen(&self, handler: impl Fn(&Event) + 'static) -> Unsubscribe {
        self.inner.event.listen(handler)
    }

    /// Reports a read of `key` (for readers narrower than an accessor, e.g. [`Key::Part`]).
    pub fn track(&self, key: Key) {
        self.inner.track(key);
    }

    /// Solid's `untrack(fn)`: the reads of `f` subscribe nothing.
    pub fn untrack<R>(&self, f: impl FnOnce() -> R) -> R {
        reactive::untrack(&*self.inner.reactivity, f)
    }

    /// The store as JSON, as the TS store serializes (the harness compares it).
    pub fn snapshot(&self) -> Json {
        self.inner.store.borrow().snapshot()
    }

    /// The internal state the harness compares besides the store: sync keys and the outboxes.
    pub fn internal(&self) -> Json {
        let inner = &self.inner;
        let sync: Object = inner.sync.snapshot().into_iter().map(|(k, v)| (k, Json::String(v.into()))).collect();
        let set = |items: &IndexSet<String>| Json::Array(items.iter().map(|id| Json::String(id.clone())).collect());
        obj! {
            "sync" => Json::Object(sync),
            "outbox" => set(&inner.outbox.borrow()),
            "sessionOutbox" => set(&inner.session_outbox.borrow()),
            "defaultLocation" => inner.default_location(),
        }
    }
}

// ---- refresh, settle, deletion --------------------------------------------------------------------------------

impl Inner {
    /// `refresh(load)`: an event-driven read; failures go to `onError` (a deleted session is forgotten).
    fn refresh(self: &Rc<Self>, load: impl FnOnce() -> LocalBoxFuture<'static, Result<(), Error>>) {
        if self.disposed.get() || !self.connected() {
            return;
        }
        let running = load();
        let this = self.clone();
        runtime::spawn(async move {
            let Err(error) = running.await else { return };
            if this.disposed.get() || !this.connected() {
                return;
            }
            if is_session_not_found(&error) {
                if let Some(session) = error.field_str("sessionID").map(str::to_owned) {
                    this.mark_deleted(&session);
                }
                return;
            }
            if error.is("MessageNotFoundError") {
                return;
            }
            if let Some(on_error) = &this.on_error {
                return on_error(&error);
            }
            this.log("Failed to refresh client data", &error);
        });
    }

    /// `settle(key, load)`: runs `load` once a burst of same-key events goes quiet.
    fn settle(self: &Rc<Self>, key: &str, load: impl FnOnce() -> LocalBoxFuture<'static, Result<(), Error>> + 'static) {
        if let Some(timer) = self.settling.borrow_mut().remove(key) {
            timer.clear();
        }
        let this = Rc::downgrade(self);
        let owned = key.to_owned();
        let timer = runtime::set_timeout(SETTLE_MS, move || {
            let Some(this) = this.upgrade() else { return };
            this.settling.borrow_mut().remove(&owned);
            this.refresh(load);
        });
        self.settling.borrow_mut().insert(key.to_owned(), timer);
    }

    fn set_session_active(&self, session: &str, status: SessionStatus) {
        if let Some(updates) = self.active_updates.borrow().as_ref() {
            updates.borrow_mut().insert(session.to_owned(), Some(status));
        }
        self.set(|store, keys| {
            if store.session.active.get(session) != Some(&status) {
                store.session.active.insert(session.to_owned(), status);
                keys.push(Key::Status(id(session)));
            }
        });
    }

    fn remove_pending(&self, session: &str, inbox: Option<&str>) {
        let Some(inbox) = inbox else { return };
        if let Some(updates) = self.pending_updates.borrow().get(session) {
            updates.borrow_mut().insert(inbox.to_owned(), PendingUpdate::Removed);
        }
        self.set(|store, keys| {
            let Some(pending) = store.session.pending.get_mut(session) else { return };
            if !pending.iter().any(|item| item.str("id") == Some(inbox)) {
                return;
            }
            Rc::make_mut(pending).retain(|item| item.str("id") != Some(inbox));
            keys.push(Key::Pending(id(session)));
        });
    }

    fn remove_permission(&self, session: &str, request: &str) {
        self.set(|store, keys| {
            let Some(requests) = store.session.permission.get_mut(session) else { return };
            if !requests.iter().any(|r| r.str("id") == Some(request)) {
                return;
            }
            Rc::make_mut(requests).retain(|r| r.str("id") != Some(request));
            keys.push(Key::Permission(id(session)));
        });
    }

    fn remove_form(&self, session: &str, form: &str, location: Option<&Json>) -> bool {
        self.set(|store, keys| {
            let Some(forms) = store.session.form.get_mut(session) else { return false };
            let location = location.map(location_key);
            let next: Vec<Record> = forms
                .iter()
                .filter(|item| {
                    if item.str("id") != Some(form) {
                        return true;
                    }
                    let Some(location) = &location else { return false };
                    if session != "global" {
                        return false;
                    }
                    match item.get("location") {
                        Some(own) => location_key(own) != *location,
                        None => true,
                    }
                })
                .cloned()
                .collect();
            if next.len() == forms.len() {
                return false;
            }
            *forms = Rc::new(next);
            keys.push(Key::Form(id(session)));
            true
        })
    }

    fn update_pending(&self, session: &str, inbox: &str, delivery: &str) {
        let (index, item) = {
            let store = self.store.borrow();
            let pending = store.session.pending.get(session);
            let index = pending.and_then(|p| p.iter().position(|item| item.str("id") == Some(inbox)));
            (index, index.and_then(|i| pending.map(|p| p[i].clone())))
        };
        if let Some(updates) = self.pending_updates.borrow().get(session) {
            let update = match &item {
                Some(item) => {
                    let mut next = (**item).clone();
                    next.set("delivery", Json::String(delivery.into()));
                    PendingUpdate::Item(Rc::new(next))
                }
                None => PendingUpdate::Delivery(delivery.into()),
            };
            updates.borrow_mut().insert(inbox.to_owned(), update);
        }
        let (Some(index), Some(item)) = (index, item) else { return };
        if item.str("delivery") == Some(delivery) {
            return;
        }
        self.set(|store, keys| {
            if let Some(pending) = store.session.pending.get_mut(session) {
                let slot = Rc::make_mut(&mut Rc::make_mut(pending)[index]);
                slot.set("delivery", Json::String(delivery.into()));
                keys.push(Key::Pending(id(session)));
            }
        });
    }

    /// `track(map, key, promise)` for the send chain: the entry clears when its own promise settles.
    fn track_sending(self: &Rc<Self>, session: &str, settled: Deferred<()>) {
        self.sending.borrow_mut().insert(session.to_owned(), settled.clone());
        let this = Rc::downgrade(self);
        let session = session.to_owned();
        runtime::spawn(async move {
            settled.wait().await;
            let Some(this) = this.upgrade() else { return };
            let current = this.sending.borrow().get(&session).is_some_and(|p| p.ptr_eq(&settled));
            if current {
                this.sending.borrow_mut().remove(&session);
            }
        });
    }

    /// `sendAdmission(sessionID, send, gate)`: waits for the gate, an in-flight create of the session and the
    /// previous admission, then sends.
    fn send_admission<T: Clone + 'static>(
        self: &Rc<Self>,
        session: &str,
        send: impl FnOnce() -> LocalBoxFuture<'static, Result<T, Error>> + 'static,
        gate: Option<Promise<()>>,
    ) -> Promise<T> {
        let created = self.creating.borrow().get(session).cloned();
        let previous = self.sending.borrow().get(session).cloned();
        let request = promise(async move {
            // `Promise.all([gate, created, previous])`: the first rejection, in settlement order, wins.
            let gate = async move {
                match gate {
                    Some(gate) => gate.wait().await,
                    None => Ok(()),
                }
            };
            let created = async move {
                match created {
                    Some(created) => created.wait().await.map(|_| ()),
                    None => Ok(()),
                }
            };
            let previous = async move {
                if let Some(previous) = previous {
                    previous.wait().await;
                }
            };
            let (gate, created, ()) = tokio::join!(gate, created, previous);
            gate?;
            created?;
            send().await
        });
        let settled: Deferred<()> = Deferred::new();
        let signal = settled.clone();
        let watched = request.clone();
        runtime::spawn(async move {
            let _ = watched.wait().await;
            signal.resolve(());
        });
        self.track_sending(session, settled);
        request
    }

    /// `admitLocal(item)`: upserts an admitted inbox item into pending and (user and synthetic items) the transcript.
    fn admit_local(&self, item: Record) {
        self.batch(|| {
            let session = item.str("sessionID").unwrap_or("").to_owned();
            let item_id = item.str("id").unwrap_or("").to_owned();
            self.set(|store, keys| {
                let pending = store.session.pending.entry(session.clone()).or_insert_with(|| Rc::new(Vec::new()));
                let list = Rc::make_mut(pending);
                match list.iter().position(|entry| entry.str("id") == Some(&item_id)) {
                    Some(at) => list[at] = item.clone(),
                    None => list.push(item.clone()),
                }
                keys.push(Key::Pending(id(&session)));
            });
            if item.str("type") == Some("compaction") {
                return;
            }
            self.materialize_inbox_message(&item);
        });
    }

    /// `materializeInboxMessage(item)`: a user or synthetic inbox item as its transcript row.
    fn materialize_inbox_message(&self, item: &Json) {
        let kind = item.str("type");
        if kind != Some("user") && kind != Some("synthetic") {
            return;
        }
        let mut row = Object::new();
        row.insert("id".into(), item.get("id").cloned().unwrap_or(Json::Null));
        row.insert("type".into(), Json::String(kind.unwrap_or("").into()));
        if let Some(Json::Object(payload)) = item.get("payload") {
            for (key, value) in payload {
                row.insert(key.clone(), value.clone());
            }
        }
        // `{ ...payload, time }`: a payload `time` keeps its place, its value replaced.
        row.insert("time".into(), obj! { "created" => item.get("time").and_then(|t| t.get("created")) });
        let row = Json::Object(row);
        let session = item.str("sessionID").unwrap_or("").to_owned();
        let row_id = item.str("id").unwrap_or("").to_owned();
        self.update_messages(&session, |messages, index, changes| match index.get(&row_id).copied() {
            None => Inner::append(messages, index, row, changes),
            Some(position) => {
                if messages.len() <= position {
                    messages.resize_with(position + 1, || Rc::new(Json::Null));
                }
                messages[position] = Rc::new(row);
                changes.message(&row_id);
            }
        });
    }

    /// `retractLocal(sessionID, inboxID)`: removes an inbox item from pending and the transcript.
    fn retract_local(&self, session: &str, inbox: &str) {
        self.batch(|| {
            self.remove_pending(session, Some(inbox));
            let known = self.message_index.borrow().get(session).is_some_and(|index| index.contains_key(inbox));
            if !known {
                return;
            }
            self.update_messages(session, |messages, index, changes| {
                let Some(position) = index.get(inbox).copied() else { return };
                if position < messages.len() {
                    messages.remove(position);
                }
                index.remove(inbox);
                Inner::reindex(messages, index, position);
                changes.ids();
                changes.message(inbox);
            });
        });
    }

    /// `resolveRoot(sessionID)`: walks `parentID` through loaded infos; a missing parent is the furthest-known
    /// ancestor; a cycle stops at the last non-repeating ancestor.
    fn resolve_root(&self, session: &str) -> String {
        self.track(Key::SessionParent(id(session)));
        let store = self.store.borrow();
        let parent = |id: &str| store.session.info.get(id).and_then(|info| info.str("parentID").map(str::to_owned));
        let mut current = session.to_owned();
        let mut parent_id = parent(session);
        let mut seen = HashSet::from([session.to_owned()]);
        while let Some(next) = parent_id.filter(|p| !p.is_empty()) {
            if !seen.insert(next.clone()) {
                break;
            }
            self.reactivity.track(&Key::SessionParent(id(&next)));
            current = next.clone();
            parent_id = parent(&next);
        }
        current
    }

    /// `registerSession(sessionID)`: adds a session to its family, folding an orphan family keyed by it.
    fn register_session(&self, session: &str) {
        if !self.has_info(session) {
            return;
        }
        let root = self.resolve_root(session);
        self.set(|store, keys| {
            let family = &mut store.session.family;
            if session != root {
                if let Some(orphans) = family.shift_remove(session) {
                    keys.push(Key::Family(id(session)));
                    let members = Rc::make_mut(family.entry(root.clone()).or_insert_with(|| Rc::new(Vec::new())));
                    for member in orphans.iter() {
                        if !members.contains(member) {
                            members.push(member.clone());
                        }
                    }
                    keys.push(Key::Family(id(&root)));
                }
            }
            let members = family.entry(root.clone()).or_insert_with(|| Rc::new(Vec::new()));
            if !members.iter().any(|m| m == session) {
                Rc::make_mut(members).push(session.to_owned());
            }
            keys.push(Key::Family(id(&root)));
        });
    }

    /// `evictSession(sessionID)`: drops the cached transcript and pending input, keeping unacknowledged submissions.
    fn evict_session(&self, session: &str) {
        if self.session_outbox.borrow().contains(session) {
            return;
        }
        self.sync.invalidate(Some(&format!("session.pending:{session}")));
        self.sync.invalidate(Some(&format!("session.message:{session}")));
        self.message_loads.borrow_mut().remove(session);
        let outbox = self.outbox.borrow().clone();
        let (pending, messages): (Vec<Record>, Vec<Record>) = {
            let store = self.store.borrow();
            let keep = |list: Option<&List>| -> Vec<Record> {
                list.map(|l| l.iter().filter(|item| outbox.contains(item.str("id").unwrap_or(""))).cloned().collect())
                    .unwrap_or_default()
            };
            (keep(store.session.pending.get(session)), keep(store.session.message.get(session)))
        };
        {
            let mut index = self.message_index.borrow_mut();
            index.remove(session);
            if !messages.is_empty() {
                index.insert(
                    session.to_owned(),
                    messages.iter().enumerate().map(|(i, m)| (m.str("id").unwrap_or("").to_owned(), i)).collect(),
                );
            }
        }
        self.set(|store, keys| {
            let s = &mut store.session;
            s.message.shift_remove(session);
            s.message_cursor.shift_remove(session);
            s.message_loading.shift_remove(session);
            s.pending.shift_remove(session);
            let k = id(session);
            keys.extend([Key::Messages(k.clone()), Key::MessageCursor(k.clone()), Key::MessageLoading(k.clone()), Key::Pending(k)]);
            if !messages.is_empty() {
                s.message.insert(session.to_owned(), Rc::new(messages));
            }
            if !pending.is_empty() {
                s.pending.insert(session.to_owned(), Rc::new(pending));
            }
        });
    }

    fn clear_deleted(&self, session: &str) {
        self.deleted_at.borrow_mut().shift_remove(session);
        self.set(|store, keys| {
            if store.session.missing.shift_remove(session).is_some() {
                keys.push(Key::Missing(id(session)));
            }
        });
    }

    /// `markDeleted(sessionID)`: forgets a session, remembering it is gone (the last 512).
    fn mark_deleted(&self, session: &str) {
        if self.session_outbox.borrow().contains(session) {
            return;
        }
        self.batch(|| {
            let oldest = {
                let mut deleted = self.deleted_at.borrow_mut();
                deleted.shift_remove(session);
                self.deletion_epoch.set(self.deletion_epoch.get() + 1);
                deleted.insert(session.to_owned(), self.deletion_epoch.get());
                if deleted.len() > DELETED_SESSION_LIMIT {
                    deleted.shift_remove_index(0).map(|(oldest, _)| oldest)
                } else {
                    None
                }
            };
            self.set(|store, keys| {
                if let Some(oldest) = &oldest {
                    if store.session.missing.shift_remove(oldest).is_some() {
                        keys.push(Key::Missing(id(oldest)));
                    }
                }
                if !store.session.missing.contains_key(session) {
                    store.session.missing.insert(session.to_owned(), ());
                    keys.push(Key::Missing(id(session)));
                }
            });
            self.remove_session(session);
        });
    }

    /// `evictMissing(error)`: a read that found its session gone forgets it, then rejects.
    fn evict_missing(&self, error: Error) -> Error {
        if is_session_not_found(&error) {
            if let Some(session) = error.field_str("sessionID") {
                self.mark_deleted(session);
            }
        }
        error
    }

    /// `removeSession(sessionID)`.
    fn remove_session(&self, session: &str) {
        if let Some(updates) = self.active_updates.borrow().as_ref() {
            updates.borrow_mut().insert(session.to_owned(), None);
        }
        let pending_ids: Vec<String> = self
            .store
            .borrow()
            .session
            .pending
            .get(session)
            .map(|p| p.iter().filter_map(|item| item.str("id").map(str::to_owned)).collect())
            .unwrap_or_default();
        {
            let mut outbox = self.outbox.borrow_mut();
            for id in &pending_ids {
                outbox.shift_remove(id);
            }
        }
        self.message_index.borrow_mut().remove(session);
        for key in ["session", "session.family", "session.pending", "session.message", "session.permission"] {
            self.sync.invalidate(Some(&format!("{key}:{session}")));
        }
        self.sync.invalidate(Some(&format!("session.form:{session}:")));
        // `delete draft.x[id]` notifies the property's readers (and key-set readers) even when it held nothing, and
        // every family is assigned anew.
        self.set(|store, keys| {
            let s = &mut store.session;
            let k = id(session);
            s.info.shift_remove(session);
            s.active.shift_remove(session);
            s.message.shift_remove(session);
            s.message_cursor.shift_remove(session);
            s.message_loading.shift_remove(session);
            s.pending.shift_remove(session);
            s.permission.shift_remove(session);
            s.form.shift_remove(session);
            keys.extend([
                Key::Session(k.clone()),
                Key::SessionOrder,
                Key::SessionParent(k.clone()),
                Key::Status(k.clone()),
                Key::Messages(k.clone()),
                Key::MessageCursor(k.clone()),
                Key::MessageLoading(k.clone()),
                Key::Pending(k.clone()),
                Key::Permission(k.clone()),
                Key::Form(k.clone()),
            ]);
            let roots: Vec<String> = s.family.keys().cloned().collect();
            for root in roots {
                let family = s.family.get_mut(&root).expect("root");
                let next: Vec<String> = family.iter().filter(|m| *m != session).cloned().collect();
                if next.is_empty() {
                    s.family.shift_remove(&root);
                } else {
                    *family = Rc::new(next);
                }
                keys.push(Key::Family(id(&root)));
            }
        });
    }

    /// `hasLocation(field, ref)`: whether something has read (or is reading) the field.
    fn has_location(&self, field: LocationField, location: Option<&Json>) -> bool {
        let key = location_key(location.unwrap_or(&self.default_location()));
        self.store.borrow().location.get(&key).is_some_and(|data| data.get(field).is_some())
            || self.sync.has(&format!("location.{}:{key}", field.name()))
    }

    /// `refreshUsageLimits(location)`: reloads usage limits only where something has read them.
    fn refresh_usage_limits(self: &Rc<Self>, location: &Json) {
        let key = location_key(location);
        let loaded = self.store.borrow().location.get(&key).is_some_and(|d| d.get(LocationField::UsageLimits).is_some())
            || self.sync.has(&format!("location.usageLimits:{key}"));
        let usage = Resource::new(self, LocationField::UsageLimits);
        usage.invalidate(Some(location));
        if loaded {
            let location = location.clone();
            self.refresh(move || Box::pin(usage.sync(Some(&location)).wait()));
        }
    }

    /// `refreshLocation(field, resource, location)`.
    fn refresh_location(self: &Rc<Self>, resource: Resource, location: &Json) {
        let active = self.has_location(resource.field, Some(location));
        resource.invalidate(Some(location));
        if active {
            let location = location.clone();
            self.refresh(move || Box::pin(resource.sync(Some(&location)).wait()));
        }
    }

    /// `setStore("location", key, { [field]: value })`.
    fn publish_location(&self, key: &str, field: LocationField, value: Option<Json>) {
        self.set(|store, keys| {
            let created = !store.location.contains_key(key);
            let data = store.location.entry(key.to_owned()).or_default();
            if created {
                keys.push(Key::Location(id(key)));
            }
            if data.set(field, value) {
                keys.push(Key::LocationField(id(key), field));
            }
        });
    }
}

// ---- location resources (`locationResource`) -----------------------------------------------------------------

/// A cached per-location catalog: `sync` loads once per invalidation, keyed by the effective location, and
/// publishes under the server's canonical location (`alias` also under the requested key when they differ).
#[derive(Clone)]
pub struct Resource {
    inner: Rc<Inner>,
    field: LocationField,
}

impl Resource {
    fn new(inner: &Rc<Inner>, field: LocationField) -> Self {
        Self { inner: inner.clone(), field }
    }

    /// `list(ref)`.
    pub fn list(&self, location: Option<&Json>) -> Option<Record> {
        if location.is_none() {
            self.inner.track(Key::DefaultLocation);
        }
        let key = location_key(location.unwrap_or(&self.inner.default_location()));
        self.inner.read(Key::LocationField(id(&key), self.field), |store| {
            store.location.get(&key).and_then(|data| data.get(self.field).cloned())
        })
    }

    /// `sync(ref)`.
    pub fn sync(&self, location: Option<&Json>) -> Promise<()> {
        let inner = self.inner.clone();
        let location = location.cloned().unwrap_or_else(|| inner.default_location());
        let key = location_key(&location);
        let field = self.field;
        let sync_key = format!("location.{}:{key}", field.name());
        let load: Load = Box::new(move || {
            let query = location_query(&location);
            let request = load_location(&inner, field, &query);
            Box::pin(async move {
                let (response_location, data) = request.await?;
                let canonical = location_key(&response_location);
                inner.publish_location(&canonical, field, data.clone());
                if alias(field) && canonical != key {
                    inner.publish_location(&key, field, data);
                }
                Ok(())
            })
        });
        self.inner.sync.run(&sync_key, load)
    }

    /// `invalidate(ref)`.
    pub fn invalidate(&self, location: Option<&Json>) {
        let key = location_key(location.unwrap_or(&self.inner.default_location()));
        self.inner.sync.invalidate(Some(&format!("location.{}:{key}", self.field.name())));
    }

    /// `location.vcs.info(ref)` (the VCS resource's `list`).
    pub fn info(&self, location: Option<&Json>) -> Option<Record> {
        self.list(location)
    }

    /// `location.usage.limits(ref)` (the usage-limits resource's `list`).
    pub fn limits(&self, location: Option<&Json>) -> Option<Record> {
        self.list(location)
    }
}

fn alias(field: LocationField) -> bool {
    matches!(field, LocationField::Model | LocationField::Provider)
}

/// The read behind each location resource: the response's location and the value to publish.
fn load_location(
    inner: &Rc<Inner>,
    field: LocationField,
    query: &Json,
) -> LocalBoxFuture<'static, Result<(Json, Option<Json>), Error>> {
    let api = inner.api();
    let input = obj! { "location" => query.clone() };
    let options = RequestOptions::default;
    let directory = query.get("directory").cloned();
    let call = |name: &str| api.call_named(name, &input, options());
    let standard = |call: crate::client::Call| -> LocalBoxFuture<'static, Result<(Json, Option<Json>), Error>> {
        Box::pin(async move {
            let response = call.await?.json();
            Ok((response.get("location").cloned().unwrap_or(Json::Null), Some(response.get("data").cloned().unwrap_or(Json::Null))))
        })
    };
    match field {
        LocationField::Vcs => standard(call("vcs.get")),
        LocationField::UsageLimits => standard(call("experimental.usage.limits")),
        LocationField::Agent => standard(call("agent.list")),
        LocationField::Command => standard(call("command.list")),
        LocationField::Integration => standard(call("integration.list")),
        LocationField::McpServer => standard(call("mcp.list")),
        LocationField::Model => standard(call("model.list")),
        LocationField::Provider => standard(call("provider.list")),
        LocationField::Reference => standard(call("reference.list")),
        LocationField::Skill => standard(call("skill.list")),
        LocationField::Config => {
            let request = call("config.get");
            Box::pin(async move {
                let data = request.await?.json();
                Ok((obj! { "directory" => directory }, Some(data)))
            })
        }
        LocationField::McpResource => {
            let request = call("mcp.resource.catalog");
            Box::pin(async move {
                let response = request.await?.json();
                let data = response.get("data").and_then(|d| d.get("resources")).cloned();
                Ok((response.get("location").cloned().unwrap_or(Json::Null), data))
            })
        }
        LocationField::Shell => {
            let request = call("shell.list");
            Box::pin(async move {
                let response = request.await?.json();
                let location = response.get("location").cloned().unwrap_or(Json::Null);
                let reference = obj! { "directory" => location.get("directory") };
                let mut shells = Object::new();
                if let Some(Json::Array(items)) = response.get("data") {
                    for info in items {
                        let mut entry = info.clone();
                        entry.set("location", reference.clone());
                        shells.insert(info.get("id").map(crate::js::string).unwrap_or_else(|| "undefined".into()), entry);
                    }
                }
                Ok((location, Some(Json::Object(shells))))
            })
        }
        LocationField::Info | LocationField::Websearch => unreachable!("not a resource"),
    }
}
