//! `ClientProvider` (context/client.tsx): the server's client, the event bus the connection publishes to, the
//! connection itself, the media cache (`util/media.ts`) and the persistent-terminal client. A managed service's
//! `reconnect` may replace the client, its URL and headers; `api()` always gives the current one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use opencode_protocol::Json;

use crate::abort::{AbortSignal, Reason};
use crate::bus::{Emitter, Unsubscribe};
use crate::client::{Client, Error, RequestOptions};
use crate::connection::{ClientConnection, ConnectionOptions, Reconnect};
use crate::data::Promise;
use crate::event::Event;
use crate::obj;
use crate::pty::PersistentPtyClient;
use crate::reactive::Reactivity;
use crate::runtime::{Deferred, Log, LogLevel};
use crate::transport::LocalBoxFuture;

/// What a managed service's `reconnect` resolves to.
#[derive(Clone)]
pub struct Reconnected {
    pub api: Client,
    pub url: Option<String>,
    pub headers: Option<Vec<(String, String)>>,
}

/// `ManagedService`: the background service the TUI started, which can move and restart.
#[derive(Clone)]
pub struct ManagedService {
    pub reconnect: Rc<dyn Fn(AbortSignal) -> LocalBoxFuture<'static, Result<Reconnected, Reason>>>,
    pub restart: Rc<dyn Fn() -> LocalBoxFuture<'static, Result<(), Reason>>>,
}

/// The provider's props.
pub struct ClientInput {
    pub api: Client,
    pub url: Option<String>,
    pub headers: Option<Vec<(String, String)>>,
    pub service: Option<ManagedService>,
    /// The TUI's log sink (`useLog({ component: "client" })` adds the component tag).
    pub log: Option<Log>,
    pub reactivity: Rc<dyn Reactivity>,
}

struct State {
    api: RefCell<Client>,
    url: RefCell<Option<String>>,
    headers: RefCell<Option<Vec<(String, String)>>>,
    persistent_pty: RefCell<Option<PersistentPtyClient>>,
    events: Emitter,
    connection: std::cell::OnceCell<ClientConnection>,
    media: MediaCache,
    restart: Option<Rc<dyn Fn() -> LocalBoxFuture<'static, Result<(), Reason>>>>,
    reactivity: Rc<dyn Reactivity>,
}

/// `useClient()`.
#[derive(Clone)]
pub struct ClientContext(Rc<State>);

/// `event: { on, listen }`.
#[derive(Clone)]
pub struct ClientEvents(Emitter);

impl ClientEvents {
    pub fn on(&self, r#type: &str, handler: impl Fn(&Event) + 'static) -> Unsubscribe {
        self.0.on(r#type, handler)
    }

    pub fn listen(&self, handler: impl Fn(&Event) + 'static) -> Unsubscribe {
        self.0.listen(handler)
    }

    /// The bus itself (what `createData`'s `event` input takes).
    pub fn emitter(&self) -> &Emitter {
        &self.0
    }
}

impl ClientContext {
    pub fn new(input: ClientInput) -> Self {
        let api_for_media: Rc<RefCell<Option<std::rc::Weak<State>>>> = Rc::new(RefCell::new(None));
        let media_api = api_for_media.clone();
        let state = Rc::new(State {
            api: RefCell::new(input.api.clone()),
            url: RefCell::new(input.url.clone()),
            headers: RefCell::new(input.headers.clone()),
            persistent_pty: RefCell::new(input.url.clone().map(|url| PersistentPtyClient::new(input.api.clone(), url))),
            events: Emitter::new(),
            connection: std::cell::OnceCell::new(),
            media: MediaCache::new(Rc::new(move || {
                let state = media_api.borrow().as_ref().and_then(std::rc::Weak::upgrade).expect("client context");
                let api = state.api.borrow().clone();
                api
            })),
            restart: input.service.as_ref().map(|service| service.restart.clone()),
            reactivity: input.reactivity.clone(),
        });
        *api_for_media.borrow_mut() = Some(Rc::downgrade(&state));
        let reconnect: Option<Reconnect> = input.service.map(|service| {
            let weak = Rc::downgrade(&state);
            let reconnect: Reconnect = Rc::new(move |signal| {
                let pending = (service.reconnect)(signal);
                let weak = weak.clone();
                Box::pin(async move {
                    let next = pending.await?;
                    let state = weak.upgrade().ok_or_else(|| Reason::error("client context gone"))?;
                    *state.api.borrow_mut() = next.api.clone();
                    if next.url.is_some() {
                        *state.url.borrow_mut() = next.url.clone();
                    }
                    *state.headers.borrow_mut() = next.headers.clone();
                    let url = state.url.borrow().clone();
                    if let Some(url) = url {
                        *state.persistent_pty.borrow_mut() = Some(PersistentPtyClient::new(next.api.clone(), url));
                    }
                    Ok(next.api)
                })
            });
            reconnect
        });
        let log = input.log.map(|sink| {
            let tagged: Log = Rc::new(move |level: LogLevel, message: &str, tags: &Json| {
                let mut merged = obj! { "component" => "client" };
                if let (Json::Object(merged), Json::Object(tags)) = (&mut merged, tags) {
                    for (key, value) in tags {
                        merged.insert(key.clone(), value.clone());
                    }
                }
                sink(level, message, &merged)
            });
            tagged
        });
        let events = state.events.clone();
        let connection = ClientConnection::create(
            input.api,
            ConnectionOptions {
                reconnect,
                on_event: Rc::new(move |event: &Event| events.emit(event)),
                flush_interval: None,
                idle_timeout: None,
                log,
                reactivity: input.reactivity,
            },
        );
        let _ = state.connection.set(connection);
        Self(state)
    }

    /// `client.api`: the current client.
    pub fn api(&self) -> Client {
        self.0.api.borrow().clone()
    }

    pub fn url(&self) -> Option<String> {
        self.0.url.borrow().clone()
    }

    pub fn headers(&self) -> Option<Vec<(String, String)>> {
        self.0.headers.borrow().clone()
    }

    /// `client.persistentPty`: needs the server's URL.
    pub fn persistent_pty(&self) -> Result<PersistentPtyClient, Error> {
        self.0
            .persistent_pty
            .borrow()
            .clone()
            .ok_or_else(|| Error::Other(Reason::error("Persistent terminal server endpoint is unavailable")))
    }

    pub fn event(&self) -> ClientEvents {
        ClientEvents(self.0.events.clone())
    }

    pub fn connection(&self) -> &ClientConnection {
        self.0.connection.get().expect("connection is set at creation")
    }

    pub fn media(&self) -> &MediaCache {
        &self.0.media
    }

    /// `client.restart` (only with a managed service).
    pub fn restart(&self) -> Option<LocalBoxFuture<'static, Result<(), Reason>>> {
        self.0.restart.as_ref().map(|restart| restart())
    }

    pub fn reactivity(&self) -> &Rc<dyn Reactivity> {
        &self.0.reactivity
    }

    /// The provider's cleanup: the connection stops and the event bus is cleared.
    pub fn dispose(&self) {
        self.0.events.clear();
        self.connection().dispose();
    }
}

/// `mediaHashFromUri(uri)`: the blob hash of a `media:<hash>` ref.
pub fn media_hash_from_uri(uri: &str) -> Option<&str> {
    let hash = uri.strip_prefix("media:")?;
    (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then_some(hash)
}

/// `isMissingMedia(error)`: the server answered that the blob is gone.
pub fn is_missing_media(error: &Error) -> bool {
    error.name() == "FileNotFoundError"
}

/// `createMediaCache(api)`: bytes of `media:<hash>` refs, read once; a hash names its bytes, so entries stay valid
/// across reconnects.
pub struct MediaCache {
    api: Rc<dyn Fn() -> Client>,
    bytes: Rc<RefCell<HashMap<String, Rc<Vec<u8>>>>>,
    inflight: Rc<RefCell<HashMap<String, Promise<Rc<Vec<u8>>>>>>,
}

impl MediaCache {
    pub fn new(api: Rc<dyn Fn() -> Client>) -> Self {
        Self { api, bytes: Rc::default(), inflight: Rc::default() }
    }

    /// `peek(uri)`.
    pub fn peek(&self, uri: &str) -> Option<Rc<Vec<u8>>> {
        self.bytes.borrow().get(media_hash_from_uri(uri)?).cloned()
    }

    /// `read(uri)`.
    pub fn read(&self, uri: &str) -> Promise<Rc<Vec<u8>>> {
        let Some(hash) = media_hash_from_uri(uri).map(str::to_owned) else {
            return Deferred::resolved(Err(Error::Other(Reason::error(format!("Not a media ref: {uri}")))));
        };
        if let Some(cached) = self.bytes.borrow().get(&hash) {
            return Deferred::resolved(Ok(cached.clone()));
        }
        if let Some(pending) = self.inflight.borrow().get(&hash) {
            return pending.clone();
        }
        let call = (self.api)().api().experimental().media().get(&obj! { "hash" => hash.as_str() }, RequestOptions::default());
        let (bytes, inflight, key) = (self.bytes.clone(), self.inflight.clone(), hash.clone());
        let pending: Promise<Rc<Vec<u8>>> = Deferred::spawn(async move {
            let result = call.await.map(|value| Rc::new(value.bytes()));
            if let Ok(result) = &result {
                bytes.borrow_mut().insert(key.clone(), result.clone());
            }
            inflight.borrow_mut().remove(&key);
            result
        });
        self.inflight.borrow_mut().insert(hash, pending.clone());
        pending
    }
}
