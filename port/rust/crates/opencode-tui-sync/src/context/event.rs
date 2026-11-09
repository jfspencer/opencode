//! `useEvent()` (context/event.ts): the client's events with the directory they happened in.

use crate::bus::Unsubscribe;
use crate::event::Event;
use crate::json::JsonExt;

use super::client::ClientContext;

/// `EventMetadata`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventMetadata {
    /// `event.location?.directory`.
    pub directory: Option<String>,
}

fn metadata(event: &Event) -> EventMetadata {
    EventMetadata { directory: event.location().and_then(|l| l.str("directory")).map(str::to_owned) }
}

#[derive(Clone)]
pub struct EventContext {
    client: ClientContext,
}

impl EventContext {
    pub fn new(client: &ClientContext) -> Self {
        Self { client: client.clone() }
    }

    /// `subscribe(handler)`: every event but `server.connected`.
    pub fn subscribe(&self, handler: impl Fn(&Event, &EventMetadata) + 'static) -> Unsubscribe {
        self.client.event().listen(move |event| {
            if event.r#type() == "server.connected" {
                return;
            }
            handler(event, &metadata(event));
        })
    }

    /// `on(type, handler)`.
    pub fn on(&self, r#type: &str, handler: impl Fn(&Event, &EventMetadata) + 'static) -> Unsubscribe {
        self.client.event().on(r#type, move |event| handler(event, &metadata(event)))
    }
}
