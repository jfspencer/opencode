//! `LocationProvider` (context/location.tsx): the location the TUI is in. Setting one syncs it (the default
//! location as the default); only the server's `LocationNotFoundError` marks it gone, other failures are retried
//! (plugins may still be activating after a restart). Every reconnect syncs it again.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_protocol::Json;

use super::client::ClientContext;
use super::data::DataContext;
use super::retry::{RetryOptions, retry};
use crate::bus::Unsubscribe;
use crate::client::Error;
use crate::connection::Status;
use crate::data::store::Record;
use crate::json::JsonExt;
use crate::reactive::Key;

/// `{ location, cause }`: the location was not found.
#[derive(Clone, Debug)]
pub struct LocationError {
    pub location: Json,
    pub cause: Error,
}

struct State {
    client: ClientContext,
    data: DataContext,
    location: RefCell<Option<Json>>,
    error: RefCell<Option<LocationError>>,
    generation: Cell<u64>,
    connected: RefCell<Option<Unsubscribe>>,
}

/// `useLocation()`.
#[derive(Clone)]
pub struct LocationContext(Rc<State>);

impl LocationContext {
    pub fn new(client: &ClientContext, data: &DataContext) -> Self {
        let state = Rc::new(State {
            client: client.clone(),
            data: data.clone(),
            location: RefCell::new(None),
            error: RefCell::new(None),
            generation: Cell::new(0),
            connected: RefCell::new(None),
        });
        let weak = Rc::downgrade(&state);
        let unsubscribe = client.event().on("server.connected", move |_| {
            if let Some(state) = weak.upgrade() {
                let location = state.location.borrow().clone();
                LocationContext(state).sync(location);
            }
        });
        *state.connected.borrow_mut() = Some(unsubscribe);
        Self(state)
    }

    /// `location.current`: the server-synced info of the location set.
    pub fn current(&self) -> Option<Record> {
        let location = self.r#ref();
        self.0.data.location.info(location.as_ref())
    }

    /// `location.ref`: the location as set, before its synced info arrives.
    pub fn r#ref(&self) -> Option<Json> {
        self.0.client.reactivity().track(&Key::LocationRef);
        self.0.location.borrow().clone()
    }

    /// `location.error`.
    pub fn error(&self) -> Option<LocationError> {
        self.0.client.reactivity().track(&Key::LocationError);
        self.0.error.borrow().clone()
    }

    fn set_error(&self, error: Option<LocationError>) {
        let changed = self.0.error.borrow().is_some() || error.is_some();
        *self.0.error.borrow_mut() = error;
        if changed {
            self.0.client.reactivity().trigger(&Key::LocationError);
        }
    }

    fn sync(&self, location: Option<Json>) {
        let Some(location) = location else { return };
        let state = &self.0;
        state.generation.set(state.generation.get() + 1);
        let attempt = state.generation.get();
        let default = state.data.location.default();
        let same = |key: &str| location.get(key) == default.get(key);
        let target = if same("directory") && same("workspaceID") { None } else { Some(location.clone()) };
        self.set_error(None);
        let this = self.clone();
        let operation_state = self.clone();
        let filter_state = self.clone();
        let syncing = retry(
            move || {
                let current = operation_state.0.generation.get() == attempt;
                let sync = current.then(|| operation_state.0.data.location.sync(target.as_ref()));
                Box::pin(async move {
                    match sync {
                        Some(sync) => sync.wait().await,
                        None => Ok(()),
                    }
                })
            },
            RetryOptions {
                retry_if: Rc::new(move |cause: &Error| filter_state.0.generation.get() == attempt && !cause.is("LocationNotFoundError")),
                ..Default::default()
            },
        );
        crate::runtime::spawn(async move {
            let Err(cause) = syncing.wait().await else { return };
            let current = this.0.location.borrow().clone();
            let matches = current.as_ref().is_some_and(|current| {
                current.get("directory") == location.get("directory") && current.get("workspaceID") == location.get("workspaceID")
            });
            if !cause.is("LocationNotFoundError") || this.0.generation.get() != attempt || !matches {
                return;
            }
            this.set_error(Some(LocationError { location, cause }));
        });
    }

    /// `location.set(location)`: syncs it now when connected.
    pub fn set(&self, location: Option<Json>) {
        *self.0.location.borrow_mut() = location.clone();
        self.0.client.reactivity().trigger(&Key::LocationRef);
        if self.0.client.connection().status() == Status::Connected {
            self.sync(location);
        }
    }

    /// The provider's cleanup.
    pub fn dispose(&self) {
        if let Some(unsubscribe) = self.0.connected.borrow_mut().take() {
            unsubscribe.call();
        }
    }
}
