//! A server event (`OpenCodeEvent`, the stream's `EventSubscribeOutput`): the parsed JSON, as the TS client keeps
//! it (it does not validate events). [`Event::decode`] reads one through its protocol schema when a consumer
//! wants the typed form (`opencode_protocol::types::V2Event` or one event's struct).

use std::rc::Rc;

use opencode_protocol::{Json, Wire};

use crate::json::JsonExt;

struct Inner {
    r#type: String,
    json: Json,
}

/// One event; cloning shares it.
#[derive(Clone)]
pub struct Event(Rc<Inner>);

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Event({})", crate::client::stringify(&self.0.json))
    }
}

impl PartialEq for Event {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0) || self.0.json == other.0.json
    }
}

impl Event {
    pub fn new(json: Json) -> Self {
        let r#type = json.str("type").unwrap_or("").to_owned();
        Self(Rc::new(Inner { r#type, json }))
    }

    /// `event.type`.
    pub fn r#type(&self) -> &str {
        &self.0.r#type
    }

    /// The whole event.
    pub fn json(&self) -> &Json {
        &self.0.json
    }

    /// `event.data` (`null` when absent).
    pub fn data(&self) -> &Json {
        self.0.json.get("data").unwrap_or(&Json::Null)
    }

    /// `event.id`.
    pub fn id(&self) -> &str {
        self.0.json.str("id").unwrap_or("")
    }

    /// `event.created`.
    pub fn created(&self) -> Option<f64> {
        self.0.json.num("created")
    }

    /// `event.metadata`.
    pub fn metadata(&self) -> Option<&Json> {
        self.0.json.get("metadata")
    }

    /// `event.location`, when it is an object.
    pub fn location(&self) -> Option<&Json> {
        self.0.json.get("location").filter(|location| matches!(location, Json::Object(_)))
    }

    /// `"durable" in event && event.durable` (truthy).
    pub fn durable(&self) -> Option<&Json> {
        self.0.json.get("durable").filter(|durable| crate::json::truthy(durable))
    }

    /// The event through its schema.
    pub fn decode<T: Wire>(&self) -> Result<T, opencode_protocol::wire::Issue> {
        T::decode(&self.0.json)
    }
}
