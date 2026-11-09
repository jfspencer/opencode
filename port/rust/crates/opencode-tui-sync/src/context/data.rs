//! `DataProvider` (context/data.tsx): the data layer over the client context, with what the TUI adds to it:
//! `location.sync` also loads the location's config, and `session.title` regenerates a title.
//!
//! The context reads like the TS one: `data.session.get(id)` and every other path of the data layer go through
//! (`Deref`), `data.location.sync(ref)` is the context's own, `data.session.title.pending(id)` is new.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::Deref;
use std::rc::Rc;

use opencode_protocol::Json;

use super::client::ClientContext;
use crate::client::RequestOptions;
use crate::data::{CreateDataInput, Data, LocationApi, Promise, SessionApi};
use crate::obj;
use crate::reactive::{Id, Key};
use crate::runtime::Deferred;

/// `useData()`.
#[derive(Clone)]
pub struct DataContext {
    pub session: SessionContext,
    pub location: LocationContextApi,
    data: Data,
}

impl Deref for DataContext {
    type Target = Data;
    fn deref(&self) -> &Data {
        &self.data
    }
}

impl DataContext {
    /// The provider's init: `createData` reading stored media as `media:<hash>` refs, with the client's events and
    /// connection.
    pub fn new(client: &ClientContext, directory: &str) -> Self {
        let api_client = client.clone();
        let connection: Rc<dyn crate::data::ConnectionState> = Rc::new(client.connection().clone());
        let data = Data::create(CreateDataInput {
            // Only the transcript store reads stored media as refs; plugins and other callers keep inline bytes.
            api: Rc::new(move || api_client.api().with_media_refs()),
            directory: directory.to_owned(),
            initial_message_limit: None,
            event: client.event().emitter().clone(),
            connection: Some(connection),
            on_error: None,
            omit_tool_file_data: false,
            reactivity: client.reactivity().clone(),
            log: None,
        });
        Self {
            session: SessionContext {
                api: data.session.clone(),
                title: TitleApi { client: client.clone(), data: data.clone(), pending: Rc::default() },
            },
            location: LocationContextApi { api: data.location.clone(), data: data.clone() },
            data,
        }
    }

    /// The data layer itself (what plugins receive).
    pub fn data(&self) -> &Data {
        &self.data
    }
}

/// `data.session`, with `title`.
#[derive(Clone)]
pub struct SessionContext {
    api: SessionApi,
    pub title: TitleApi,
}

impl Deref for SessionContext {
    type Target = SessionApi;
    fn deref(&self) -> &SessionApi {
        &self.api
    }
}

/// `data.session.title`.
#[derive(Clone)]
pub struct TitleApi {
    client: ClientContext,
    data: Data,
    pending: Rc<RefCell<HashSet<String>>>,
}

impl TitleApi {
    /// `title.pending(sessionID)`: a regeneration is in flight.
    pub fn pending(&self, session: &str) -> bool {
        self.data.track(Key::TitlePending(Id::from(session)));
        self.pending.borrow().contains(session)
    }

    fn set_pending(&self, session: &str, pending: bool) {
        let changed = if pending { self.pending.borrow_mut().insert(session.to_owned()) } else { self.pending.borrow_mut().remove(session) };
        if changed {
            self.client.reactivity().trigger(&Key::TitlePending(Id::from(session)));
        }
    }

    /// `title.generate(sessionID)`: asks the server for a new title, pending until it is projected locally (the
    /// response can beat the renamed event).
    pub fn generate(&self, session: &str) -> Promise<()> {
        if self.pending.borrow().contains(session) {
            return Deferred::resolved(Ok(()));
        }
        self.set_pending(session, true);
        let call = self.client.api().api().session().update(&obj! { "sessionID" => session, "title" => "" }, RequestOptions::default());
        let this = self.clone();
        let session = session.to_owned();
        Deferred::spawn(async move {
            let result = match call.await {
                Ok(_) => {
                    this.data.session.invalidate(&session);
                    this.data.session.sync(&session, false).wait().await
                }
                Err(error) => Err(error),
            };
            this.set_pending(&session, false);
            result
        })
    }
}

/// `data.location`, with the context's `sync`.
#[derive(Clone)]
pub struct LocationContextApi {
    api: LocationApi,
    data: Data,
}

impl Deref for LocationContextApi {
    type Target = LocationApi;
    fn deref(&self) -> &LocationApi {
        &self.api
    }
}

impl LocationContextApi {
    /// `location.sync(ref)`: the info, then every catalog and the config.
    pub fn sync(&self, location: Option<&Json>) -> Promise<()> {
        let info = self.data.location.sync_info(location);
        let data = self.data.clone();
        let location = location.cloned();
        Deferred::spawn(async move {
            info.wait().await?;
            let all = data.location.sync(location.as_ref());
            let config = data.location.config.sync(location.as_ref());
            let (all, config) = (all.wait(), config.wait());
            // `Promise.all`: the first rejection wins.
            let mut all = std::pin::pin!(all);
            let mut config = std::pin::pin!(config);
            let (mut left, mut right) = (None, None);
            while left.is_none() || right.is_none() {
                tokio::select! {
                    biased;
                    result = &mut all, if left.is_none() => left = Some(result?),
                    result = &mut config, if right.is_none() => right = Some(result?),
                }
            }
            Ok(())
        })
    }
}
